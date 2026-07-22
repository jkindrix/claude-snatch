//! B3 normalization: map Codex envelope records into the common model.
//!
//! Covers user/assistant content, reasoning summaries, tool calls/results
//! (including `web_search_call`), and usage. The mapping is recorded in
//! `docs/multi-provider-design.md` ("B3 slice 1 — normalization mapping",
//! amended by B3.1) and rests on the 224-session corpus census plus the
//! round-22 audit. Binding constraints (round-21):
//!
//! - A mapped record's primary entry keeps its B1 deterministic id
//!   `(ordinal, 0)`; genuine 1:N mappings use deterministic subindices for
//!   additional entries (for example an event-only lifecycle ToolResult).
//! - `turn_id` rides the semantics sidecar (ambient `turn_context` /
//!   `task_started` state, OVERRIDDEN by each item's own
//!   `internal_chat_message_metadata_passthrough` / `metadata` carrier),
//!   never message identity.
//! - Deduplication suppresses only a PROVEN one-to-one twin: matching is
//!   scoped to a turn window, claims each candidate at most once, and
//!   records the twin's ordinal inside the suppression itself. Unmatched
//!   event content (post-compaction notices, reasoning before an aborted
//!   turn) maps directly — it never disappears (round-22 blocker 1).
//! - Canonical usage derives from CUMULATIVE transitions: unchanged totals
//!   contribute zero, decreases open a new epoch, and summed entry usage
//!   telescopes to the sum of epoch finals — never a blind sum of
//!   `last_token_usage` (round-22 blocker 2). Usage events arriving before
//!   their response are held and attached to the NEXT assistant emission;
//!   if none ever arrives the record stays a preserved Unknown entry,
//!   never lost.
//!
//! A `compacted` record maps once as a chronological compact-boundary system
//! entry; its replacement history remains nested reconstruction state and is
//! never expanded into new activity. `world_state` and legacy
//! `ghost_snapshot` records remain verbatim `Unknown` entries with typed state
//! carriers because they are reconstruction checkpoints, not model-visible
//! emissions. Other unmapped vocabulary stays preserved `Unknown`. The
//! provider supplies a proven copied-history interval for old-format forks;
//! this module treats its end as a hard matching/usage window boundary before
//! the provider annotates copied entries as inherited. Spawn lineage is
//! provider-level metadata rather than an entry.

use std::collections::{BTreeMap, BTreeSet};
use std::time::Duration;

use chrono::{DateTime, Utc};
use serde_json::Value;

use crate::model::usage::Usage;
use crate::model::{
    AssistantContent, AssistantMessage, ContentBlock, LogEntry, SystemMessage, SystemSubtype,
    TextBlock, ThinkingBlock, ToolResult, ToolResultContent, ToolUse, UserBlocksContent,
    UserContent, UserMessage,
};

use super::{
    ActivityKind, CompactionKind, CompactionSemantics, CompactionWindow, EntryId, EntrySemantics,
    IdentifiedEntry, IngestionDiagnostics, LogicalSessionKey, PromptAuthorship, PromptDelivery,
    PromptSemantics, RecordDisposition, RecordOutcome, RecordRef, StateCheckpointKind,
    SuppressionReason, ToolExecutionStatus, ToolKind, ToolLifecycleKind, ToolLifecycleObservation,
    ToolSemantics, UsageAggregation, UsageObservation, UsageObservationKind, UsageScope,
};

/// Output of normalizing the parsed record stream.
pub(super) struct NormalizeOutput {
    pub entries: Vec<IdentifiedEntry>,
    pub entry_origins: BTreeMap<EntryId, Vec<RecordRef>>,
    pub record_dispositions: Vec<RecordDisposition>,
    pub semantics: BTreeMap<EntryId, EntrySemantics>,
    pub diagnostics: IngestionDiagnostics,
}

/// A record that starts a new matching window (turn/request boundary).
fn is_window_boundary(envelope_type: &str, payload_type: &str) -> bool {
    matches!(envelope_type, "session_meta" | "turn_context" | "compacted")
        || (envelope_type == "event_msg" && payload_type == "task_started")
}

fn envelope_parts(value: &Value) -> (&str, &Value, &str) {
    let envelope_type = value.get("type").and_then(Value::as_str).unwrap_or("");
    let payload = value.get("payload").unwrap_or(&Value::Null);
    let payload_type = payload.get("type").and_then(Value::as_str).unwrap_or("");
    (envelope_type, payload, payload_type)
}

/// Concatenated text parts of a response_item message content array.
fn joined_text(payload: &Value) -> String {
    payload
        .get("content")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|i| i.get("text").and_then(Value::as_str))
                .collect::<Vec<_>>()
                .join("")
        })
        .unwrap_or_default()
}

/// Pre-computed emission matching (round-22 blocker 1): within each turn
/// window, each content-bearing `event_msg` claims at most one
/// corresponding `response_item` (or one reasoning section) whose text
/// agrees — a positional-and-content-confirmed twin. Events left unclaimed
/// map directly later; response items left unclaimed simply have no twin.
#[derive(Default)]
struct MatchPlan {
    /// event ordinal → target ordinal of its proven twin: the authoritative
    /// response record, or (for a repeated identical native event) the
    /// representative event's own target/record. Targets are always MAPPED
    /// records — the validator enforces it.
    suppressed_events: BTreeMap<u64, u64>,
    /// response_item user-message ordinals claimed by a `user_message`
    /// event → HUMAN-authored prompts.
    human_responses: BTreeSet<u64>,
}

struct Candidate {
    ordinal: u64,
    text: String,
    claimed: bool,
}

/// Native lifecycle families whose end records can either enrich a persisted
/// response-item call or be the only durable evidence of a nested operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum LifecycleFamily {
    Exec,
    Patch,
    Web,
}

#[derive(Debug, Clone)]
struct LifecyclePair {
    target_ordinal: u64,
    call_id: String,
}

#[derive(Default)]
struct LifecyclePlan {
    /// Lifecycle record ordinal -> authoritative response-item call.
    paired: BTreeMap<u64, LifecyclePair>,
    /// Duplicate or contradictory candidates are preserved Unknown rather
    /// than guessed into a call.
    ambiguous: BTreeSet<u64>,
}

fn lifecycle_event_key(payload_type: &str, payload: &Value) -> Option<(LifecycleFamily, String)> {
    let family = match payload_type {
        "exec_command_end" => LifecycleFamily::Exec,
        "patch_apply_end" => LifecycleFamily::Patch,
        "web_search_end" => LifecycleFamily::Web,
        _ => return None,
    };
    let call_id = payload.get("call_id").and_then(Value::as_str)?;
    (!call_id.is_empty()).then(|| (family, call_id.to_string()))
}

fn lifecycle_call_key(payload_type: &str, payload: &Value) -> Option<(LifecycleFamily, String)> {
    let (family, id) = match payload_type {
        "function_call" if payload.get("name").and_then(Value::as_str) == Some("exec_command") => (
            LifecycleFamily::Exec,
            payload.get("call_id").and_then(Value::as_str),
        ),
        "custom_tool_call"
            if payload.get("name").and_then(Value::as_str) == Some("apply_patch") =>
        {
            (
                LifecycleFamily::Patch,
                payload.get("call_id").and_then(Value::as_str),
            )
        }
        "web_search_call" => (
            LifecycleFamily::Web,
            payload
                .get("id")
                .or_else(|| payload.get("call_id"))
                .and_then(Value::as_str),
        ),
        _ => return None,
    };
    let id = id?;
    (!id.is_empty()).then(|| (family, id.to_string()))
}

fn plan_lifecycle(
    records: &[(RecordRef, Value)],
    first_new_after_fork: Option<u64>,
) -> LifecyclePlan {
    let mut plan = LifecyclePlan::default();
    let mut window_start = 0usize;
    let mut i = 0usize;
    loop {
        let at_boundary = i == records.len() || {
            let (et, _, pt) = envelope_parts(&records[i].1);
            is_window_boundary(et, pt) || first_new_after_fork == Some(records[i].0.ordinal)
        };
        if at_boundary && i > window_start {
            plan_lifecycle_window(&records[window_start..i], &mut plan);
            window_start = i;
        }
        if i == records.len() {
            break;
        }
        i += 1;
    }
    plan
}

fn plan_lifecycle_window(window: &[(RecordRef, Value)], plan: &mut LifecyclePlan) {
    let mut calls: BTreeMap<(LifecycleFamily, String), Vec<(u64, &Value)>> = BTreeMap::new();
    let mut events: BTreeMap<(LifecycleFamily, String), Vec<(u64, &Value)>> = BTreeMap::new();
    for (record, value) in window {
        let (envelope_type, payload, payload_type) = envelope_parts(value);
        if envelope_type == "response_item" {
            if let Some(key) = lifecycle_call_key(payload_type, payload) {
                calls
                    .entry(key)
                    .or_default()
                    .push((record.ordinal, payload));
            }
        } else if envelope_type == "event_msg" {
            if let Some(key) = lifecycle_event_key(payload_type, payload) {
                events
                    .entry(key)
                    .or_default()
                    .push((record.ordinal, payload));
            }
        }
    }

    for (key, lifecycle_records) in events {
        let candidates = calls.get(&key).map(Vec::as_slice).unwrap_or_default();
        if lifecycle_records.len() != 1 || candidates.len() > 1 {
            plan.ambiguous
                .extend(lifecycle_records.iter().map(|(ordinal, _)| *ordinal));
            continue;
        }
        let [(event_ordinal, event_payload)] = lifecycle_records.as_slice() else {
            continue;
        };
        let [] = candidates else {
            let [(target_ordinal, target_payload)] = candidates else {
                unreachable!("candidate cardinality checked above")
            };
            // Web end records repeat the exact structured action. The call id
            // alone is insufficient if drift produces contradictory payloads;
            // preserve that record Unknown rather than merging it silently.
            if key.0 == LifecycleFamily::Web
                && event_payload.get("action") != target_payload.get("action")
            {
                plan.ambiguous.insert(*event_ordinal);
                continue;
            }
            plan.paired.insert(
                *event_ordinal,
                LifecyclePair {
                    target_ordinal: *target_ordinal,
                    call_id: key.1,
                },
            );
            continue;
        };
        // No response-item candidate: this is an event-only nested operation,
        // not an ambiguity. It will become its own canonical tool call.
    }
}

fn claim(pool: &mut [Candidate], text: &str) -> Option<u64> {
    pool.iter_mut()
        .find(|c| !c.claimed && c.text == text)
        .map(|c| {
            c.claimed = true;
            c.ordinal
        })
}

fn plan_matches(records: &[(RecordRef, Value)], first_new_after_fork: Option<u64>) -> MatchPlan {
    let mut plan = MatchPlan::default();
    let mut window_start = 0usize;
    let mut i = 0usize;
    loop {
        let at_boundary = i == records.len() || {
            let (et, _, pt) = envelope_parts(&records[i].1);
            is_window_boundary(et, pt) || first_new_after_fork == Some(records[i].0.ordinal)
        };
        if at_boundary && i > window_start {
            plan_window(&records[window_start..i], &mut plan);
            window_start = i;
        }
        if i == records.len() {
            break;
        }
        i += 1;
    }
    plan
}

fn plan_window(window: &[(RecordRef, Value)], plan: &mut MatchPlan) {
    let mut users: Vec<Candidate> = Vec::new();
    let mut agents: Vec<Candidate> = Vec::new();
    let mut sections: Vec<Candidate> = Vec::new();

    // Event-to-event dedup FIRST (round-23 blocker 2): identical native
    // events — same payload type, same payload JSON, same timestamp,
    // same window — are one semantic emission. Later copies map to the
    // representative; repeated text at a DIFFERENT timestamp stays
    // distinct because the timestamp is part of the fingerprint.
    let mut representatives: BTreeMap<(String, String, String), u64> = BTreeMap::new();
    let mut event_duplicates: BTreeMap<u64, u64> = BTreeMap::new();
    for (record, value) in window {
        let (et, payload, pt) = envelope_parts(value);
        if et != "event_msg"
            || !matches!(
                pt,
                "user_message"
                    | "agent_message"
                    | "agent_reasoning"
                    | "agent_reasoning_raw_content"
            )
        {
            continue;
        }
        let fingerprint = (
            pt.to_string(),
            payload.to_string(),
            value
                .get("timestamp")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
        );
        match representatives.get(&fingerprint) {
            Some(rep) => {
                event_duplicates.insert(record.ordinal, *rep);
            }
            None => {
                representatives.insert(fingerprint, record.ordinal);
            }
        }
    }
    for (record, value) in window {
        let (et, payload, pt) = envelope_parts(value);
        if et != "response_item" {
            continue;
        }
        match pt {
            "message" => {
                let role = payload.get("role").and_then(Value::as_str).unwrap_or("");
                let candidate = Candidate {
                    ordinal: record.ordinal,
                    text: joined_text(payload),
                    claimed: false,
                };
                if role == "user" {
                    users.push(candidate);
                } else if role == "assistant" {
                    agents.push(candidate);
                }
            }
            "reasoning" => {
                for list in ["summary", "content"] {
                    if let Some(items) = payload.get(list).and_then(Value::as_array) {
                        for item in items {
                            if let Some(text) = item.get("text").and_then(Value::as_str) {
                                sections.push(Candidate {
                                    ordinal: record.ordinal,
                                    text: text.to_string(),
                                    claimed: false,
                                });
                            }
                        }
                    }
                }
            }
            _ => {}
        }
    }
    for (record, value) in window {
        let (et, payload, pt) = envelope_parts(value);
        if et != "event_msg" || event_duplicates.contains_key(&record.ordinal) {
            continue;
        }
        let twin = match pt {
            "user_message" => {
                let text = payload.get("message").and_then(Value::as_str).unwrap_or("");
                let twin = claim(&mut users, text);
                if let Some(t) = twin {
                    plan.human_responses.insert(t);
                }
                twin
            }
            "agent_message" => {
                let text = payload.get("message").and_then(Value::as_str).unwrap_or("");
                claim(&mut agents, text)
            }
            "agent_reasoning" | "agent_reasoning_raw_content" => {
                let text = payload.get("text").and_then(Value::as_str).unwrap_or("");
                claim(&mut sections, text)
            }
            _ => None,
        };
        if let Some(twin_ordinal) = twin {
            plan.suppressed_events.insert(record.ordinal, twin_ordinal);
        }
    }
    // Duplicates target their representative's twin when it has one, else
    // the representative record itself (which maps, and so satisfies the
    // validator's mapped-twin rule).
    for (dup, rep) in event_duplicates {
        let target = plan.suppressed_events.get(&rep).copied().unwrap_or(rep);
        plan.suppressed_events.insert(dup, target);
    }
}

/// Non-negative counters used only for canonical, summable usage.
#[derive(Clone, Copy, PartialEq, Eq)]
struct RawUsage {
    input: u64,
    cached: u64,
    output: u64,
}

/// A usage event waiting for its assistant emission (round-22: token
/// events may precede the response records they describe). Carries the
/// window it was born in — pending usage never crosses a turn/window
/// boundary (round-23 blocker 3): at the boundary it flushes as a
/// preserved, unattributed record instead of leaking into a later turn.
struct PendingUsage {
    record: RecordRef,
    value: Value,
    window: u64,
    canonical: Usage,
    observation_kind: UsageObservationKind,
    last_obs: ObservationNumbers,
    total_obs: ObservationNumbers,
    model_context_window: Option<i64>,
    /// The cumulative transition's fresh delta was uninterpretable.
    ambiguous_transition: bool,
}

/// Complete native Codex `TokenUsage` numbers destined for an observation.
#[derive(Clone, Copy)]
struct ObservationNumbers {
    input: i64,
    cached: i64,
    output: i64,
    reasoning_output: i64,
    total: i64,
}

impl ObservationNumbers {
    fn read(value: Option<&Value>) -> Self {
        let get = |key: &str| {
            value
                .and_then(|usage| usage.get(key))
                .and_then(Value::as_i64)
                .unwrap_or(0)
        };
        Self {
            input: get("input_tokens"),
            cached: get("cached_input_tokens"),
            output: get("output_tokens"),
            reasoning_output: get("reasoning_output_tokens"),
            total: get("total_tokens"),
        }
    }

    fn canonical(self) -> RawUsage {
        RawUsage {
            input: u64::try_from(self.input).unwrap_or(0),
            cached: u64::try_from(self.cached).unwrap_or(0),
            output: u64::try_from(self.output).unwrap_or(0),
        }
    }

    fn contradicts_input_basis(self) -> bool {
        self.input < 0 || self.cached < 0 || self.cached > self.input
    }

    fn has_negative_counter(self) -> bool {
        self.input < 0
            || self.cached < 0
            || self.output < 0
            || self.reasoning_output < 0
            || self.total < 0
    }
}

/// Session-level state threaded through the linear walk.
struct WalkState {
    version: String,
    cwd: Option<String>,
    model: String,
    turn_id: Option<String>,
    /// Current window index (increments at every window boundary).
    window: u64,
    /// Most recent assistant-authored entry and the window it was born in.
    last_assistant: Option<(usize, u64)>,
    /// Previous cumulative usage totals (None before the first observation;
    /// re-seeded across epoch resets).
    prev_total: Option<RawUsage>,
    /// Usage events awaiting the next assistant emission.
    pending_usage: Vec<PendingUsage>,
    /// Previous mapped entry's synthetic uuid (linear threading).
    last_uuid: Option<String>,
}

pub(super) fn normalize(
    key: &LogicalSessionKey,
    records: &[(RecordRef, Value)],
    inherited_range: Option<(u64, u64)>,
) -> NormalizeOutput {
    let mut out = NormalizeOutput {
        entries: Vec::new(),
        entry_origins: BTreeMap::new(),
        record_dispositions: Vec::new(),
        semantics: BTreeMap::new(),
        diagnostics: IngestionDiagnostics::default(),
    };
    let first_new_after_fork = inherited_range.map(|(_, last)| last.saturating_add(1));
    let plan = plan_matches(records, first_new_after_fork);
    let lifecycle_plan = plan_lifecycle(records, first_new_after_fork);

    let mut state = WalkState {
        version: "unknown".into(),
        cwd: None,
        model: "unknown".into(),
        turn_id: None,
        window: 0,
        last_assistant: None,
        prev_total: None,
        pending_usage: Vec::new(),
        last_uuid: None,
    };

    for (record, value) in records {
        let (envelope_type, payload, payload_type) = envelope_parts(value);
        let timestamp = value
            .get("timestamp")
            .and_then(Value::as_str)
            .and_then(|t| DateTime::parse_from_rfc3339(t).ok())
            .map_or_else(|| DateTime::<Utc>::UNIX_EPOCH, |t| t.with_timezone(&Utc));

        if is_window_boundary(envelope_type, payload_type)
            || first_new_after_fork == Some(record.ordinal)
        {
            state.window += 1;
            // Pending usage from the closed window flushes as PRESERVED,
            // unattributed records — it must never attach to a later
            // turn's assistant (round-23 blocker 3).
            let stale: Vec<PendingUsage> = std::mem::take(&mut state.pending_usage);
            for pending in stale {
                push_unknown(pending.value, key, &pending.record, &mut out);
            }
        }

        // State updates from records that stay Unknown.
        match envelope_type {
            "session_meta" => {
                if let Some(v) = payload.get("cli_version").and_then(Value::as_str) {
                    state.version = v.to_string();
                }
                if let Some(c) = payload.get("cwd").and_then(Value::as_str) {
                    state.cwd = Some(c.to_string());
                }
            }
            "turn_context" => {
                if let Some(m) = payload.get("model").and_then(Value::as_str) {
                    state.model = m.to_string();
                }
                if let Some(c) = payload.get("cwd").and_then(Value::as_str) {
                    state.cwd = Some(c.to_string());
                }
                if let Some(t) = payload.get("turn_id").and_then(Value::as_str) {
                    state.turn_id = Some(t.to_string());
                }
            }
            "event_msg" if payload_type == "task_started" => {
                if let Some(t) = payload.get("turn_id").and_then(Value::as_str) {
                    state.turn_id = Some(t.to_string());
                }
            }
            "event_msg"
                if matches!(
                    payload_type,
                    "exec_command_end" | "patch_apply_end" | "web_search_end"
                ) =>
            {
                if let Some(t) = payload
                    .get("turn_id")
                    .and_then(Value::as_str)
                    .filter(|t| !t.is_empty())
                {
                    state.turn_id = Some(t.to_string());
                }
            }
            "response_item" => {
                // The per-item carrier OVERRIDES ambient state (round-22:
                // the documented metadata passthrough must be honored, not
                // merely happen to agree with ambient turn context).
                if let Some(t) = item_turn_id(payload) {
                    state.turn_id = Some(t);
                }
            }
            _ => {}
        }

        match (envelope_type, payload_type) {
            ("compacted", _) => {
                normalize_compaction(payload, timestamp, key, record, &mut state, &mut out);
            }
            ("world_state", _) => {
                let checkpoint = match payload.get("full").and_then(Value::as_bool) {
                    Some(true) => Some(StateCheckpointKind::WorldStateFull),
                    Some(false) => Some(StateCheckpointKind::WorldStatePatch),
                    None => None,
                };
                let id = push_unknown(value.clone(), key, record, &mut out);
                if let Some(checkpoint) = checkpoint {
                    out.semantics.entry(id).or_default().state_checkpoint = Some(checkpoint);
                }
            }
            ("response_item", "ghost_snapshot") => {
                let id = push_unknown(value.clone(), key, record, &mut out);
                out.semantics.entry(id).or_default().state_checkpoint =
                    Some(StateCheckpointKind::LegacyGhostSnapshot);
            }
            ("response_item", "message") => {
                normalize_message(payload, timestamp, key, record, &plan, &mut state, &mut out);
            }
            ("response_item", "reasoning") => {
                push_assistant(
                    vec![reasoning_block(payload)],
                    timestamp,
                    key,
                    record,
                    &mut state,
                    &mut out,
                );
            }
            ("response_item", "function_call") | ("response_item", "custom_tool_call") => {
                normalize_tool_call(
                    payload,
                    payload_type,
                    timestamp,
                    key,
                    record,
                    &mut state,
                    &mut out,
                );
            }
            ("response_item", "web_search_call") => {
                normalize_web_search(payload, timestamp, key, record, &mut state, &mut out);
            }
            ("response_item", "function_call_output")
            | ("response_item", "custom_tool_call_output") => {
                normalize_tool_output(payload, timestamp, key, record, &mut state, &mut out);
            }
            ("event_msg", "user_message")
            | ("event_msg", "agent_message")
            | ("event_msg", "agent_reasoning")
            | ("event_msg", "agent_reasoning_raw_content") => {
                if let Some(twin) = plan.suppressed_events.get(&record.ordinal) {
                    out.diagnostics.suppressed += 1;
                    out.record_dispositions.push(RecordDisposition {
                        record: record.clone(),
                        outcome: RecordOutcome::Suppressed {
                            reason: SuppressionReason::DuplicateStream {
                                twin: RecordRef {
                                    artifact: record.artifact.clone(),
                                    ordinal: *twin,
                                },
                            },
                        },
                    });
                } else {
                    // Unmatched event content is unique — map it (round-22:
                    // post-compaction notices, pre-abort reasoning).
                    map_unmatched_event(
                        payload_type,
                        payload,
                        timestamp,
                        key,
                        record,
                        &mut state,
                        &mut out,
                    );
                }
            }
            ("event_msg", "token_count") => {
                handle_token_count(payload, value, record, &mut state, &mut out);
            }
            ("event_msg", "exec_command_end")
            | ("event_msg", "patch_apply_end")
            | ("event_msg", "web_search_end") => {
                if lifecycle_plan.paired.contains_key(&record.ordinal) {
                    // Attached after the walk so web-search end records that
                    // precede their authoritative response item work without
                    // order-dependent guesses.
                } else if lifecycle_plan.ambiguous.contains(&record.ordinal) {
                    push_unknown(value.clone(), key, record, &mut out);
                } else {
                    normalize_lifecycle_event(
                        payload_type,
                        payload,
                        value,
                        timestamp,
                        key,
                        record,
                        &mut state,
                        &mut out,
                    );
                }
            }
            // Everything else: preserved, honestly unmodeled — a later
            // slice's business.
            _ => {
                push_unknown(value.clone(), key, record, &mut out);
            }
        }
    }

    // Usage events whose assistant emission never arrived stay PRESERVED —
    // never lost (round-22).
    let leftovers: Vec<PendingUsage> = std::mem::take(&mut state.pending_usage);
    for pending in leftovers {
        push_unknown(pending.value, key, &pending.record, &mut out);
    }
    attach_paired_lifecycle(records, &lifecycle_plan, key, &mut out);
    // Canonical entry order = record order (late-attached leftovers above
    // would otherwise trail out of place).
    out.entries.sort_by_key(|e| (e.id.ordinal, e.id.subindex));
    out
}

fn execution_status(payload: &Value) -> Option<ToolExecutionStatus> {
    payload
        .get("status")
        .and_then(Value::as_str)
        .map(|status| match status {
            "completed" => ToolExecutionStatus::Completed,
            "failed" => ToolExecutionStatus::Failed,
            "declined" => ToolExecutionStatus::Declined,
            other => ToolExecutionStatus::Other(other.to_string()),
        })
}

fn native_duration(payload: &Value) -> Option<Duration> {
    let duration = payload.get("duration")?;
    let secs = duration.get("secs")?.as_u64()?;
    let nanos = u32::try_from(duration.get("nanos")?.as_u64()?).ok()?;
    (nanos < 1_000_000_000).then(|| Duration::new(secs, nanos))
}

fn lifecycle_observation(
    payload_type: &str,
    payload: &Value,
    record: &RecordRef,
) -> Option<ToolLifecycleObservation> {
    let kind = match payload_type {
        "exec_command_end" => ToolLifecycleKind::Command,
        "patch_apply_end" => ToolLifecycleKind::Patch,
        "web_search_end" => ToolLifecycleKind::Web,
        _ => return None,
    };
    Some(ToolLifecycleObservation {
        record: record.clone(),
        kind,
        status: execution_status(payload),
        success: payload.get("success").and_then(Value::as_bool),
        exit_code: payload
            .get("exit_code")
            .and_then(Value::as_i64)
            .and_then(|code| i32::try_from(code).ok()),
        duration: native_duration(payload),
        source: payload
            .get("source")
            .and_then(Value::as_str)
            .map(str::to_string),
    })
}

fn lifecycle_failed(observation: &ToolLifecycleObservation) -> bool {
    matches!(
        observation.status,
        Some(ToolExecutionStatus::Failed | ToolExecutionStatus::Declined)
    ) || observation.success == Some(false)
        || observation.exit_code.is_some_and(|code| code != 0)
}

fn lifecycle_tool_name(payload_type: &str) -> &'static str {
    match payload_type {
        "exec_command_end" => "exec_command",
        "patch_apply_end" => "apply_patch",
        "web_search_end" => "web_search",
        _ => "unknown",
    }
}

fn lifecycle_tool_input(payload_type: &str, payload: &Value) -> Value {
    match payload_type {
        "exec_command_end" => serde_json::json!({
            "command": payload.get("command").cloned().unwrap_or(Value::Null),
            "cwd": payload.get("cwd").cloned().unwrap_or(Value::Null),
            "parsed_cmd": payload.get("parsed_cmd").cloned().unwrap_or(Value::Null),
            "source": payload.get("source").cloned().unwrap_or(Value::Null),
        }),
        "patch_apply_end" => serde_json::json!({
            "changes": payload.get("changes").cloned().unwrap_or(Value::Null),
        }),
        "web_search_end" => serde_json::json!({
            "query": payload.get("query").cloned().unwrap_or(Value::Null),
            "action": payload.get("action").cloned().unwrap_or(Value::Null),
        }),
        _ => Value::Null,
    }
}

fn lifecycle_result_text(payload_type: &str, payload: &Value) -> Option<String> {
    match payload_type {
        "exec_command_end" => {
            for field in ["formatted_output", "aggregated_output"] {
                if let Some(text) = payload
                    .get(field)
                    .and_then(Value::as_str)
                    .filter(|text| !text.is_empty())
                {
                    return Some(text.to_string());
                }
            }
            let stdout = payload.get("stdout").and_then(Value::as_str).unwrap_or("");
            let stderr = payload.get("stderr").and_then(Value::as_str).unwrap_or("");
            Some(format!("{stdout}{stderr}"))
        }
        "patch_apply_end" => {
            let stdout = payload.get("stdout").and_then(Value::as_str).unwrap_or("");
            let stderr = payload.get("stderr").and_then(Value::as_str).unwrap_or("");
            Some(format!("{stdout}{stderr}"))
        }
        "web_search_end" => None,
        _ => None,
    }
}

/// Normalize a lifecycle record that has no authoritative response-item call.
/// Command/patch records produce a ToolUse + ToolResult from one native
/// record (true 1:N provenance); web records carry no result/status and
/// therefore produce only a ToolUse without fabricating success.
#[allow(clippy::too_many_arguments)]
fn normalize_lifecycle_event(
    payload_type: &str,
    payload: &Value,
    original: &Value,
    timestamp: DateTime<Utc>,
    key: &LogicalSessionKey,
    record: &RecordRef,
    state: &mut WalkState,
    out: &mut NormalizeOutput,
) {
    let Some(call_id) = payload
        .get("call_id")
        .and_then(Value::as_str)
        .filter(|id| !id.is_empty())
        .map(str::to_string)
    else {
        push_unknown(original.clone(), key, record, out);
        return;
    };
    let Some(observation) = lifecycle_observation(payload_type, payload, record) else {
        push_unknown(original.clone(), key, record, out);
        return;
    };
    let name = lifecycle_tool_name(payload_type).to_string();
    let idx = push_lifecycle_tool_use(
        &call_id,
        &name,
        lifecycle_tool_input(payload_type, payload),
        timestamp,
        key,
        record,
        state,
        out,
    );
    let id = out.entries[idx].id.clone();
    out.semantics
        .get_mut(&id)
        .expect("mapped lifecycle tool call has semantics")
        .tools
        .insert(
            call_id.clone(),
            ToolSemantics {
                kind: classify_tool(&name),
                native_name: name,
                lifecycle: vec![observation.clone()],
            },
        );

    if let Some(output) = lifecycle_result_text(payload_type, payload) {
        push_secondary_tool_result(
            &call_id,
            output,
            lifecycle_failed(&observation),
            timestamp,
            key,
            record,
            state,
            out,
        );
    }
}

fn attach_paired_lifecycle(
    records: &[(RecordRef, Value)],
    plan: &LifecyclePlan,
    key: &LogicalSessionKey,
    out: &mut NormalizeOutput,
) {
    for (record, value) in records {
        let Some(pair) = plan.paired.get(&record.ordinal) else {
            continue;
        };
        let (_, payload, payload_type) = envelope_parts(value);
        let target = EntryId::deterministic(key, pair.target_ordinal, 0);
        let Some(observation) = lifecycle_observation(payload_type, payload, record) else {
            push_unknown(value.clone(), key, record, out);
            continue;
        };
        let Some(tool) = out
            .semantics
            .get_mut(&target)
            .and_then(|semantics| semantics.tools.get_mut(&pair.call_id))
        else {
            push_unknown(value.clone(), key, record, out);
            continue;
        };
        tool.lifecycle.push(observation);
        out.entry_origins
            .get_mut(&target)
            .expect("paired tool call has origins")
            .push(record.clone());
        out.diagnostics.mapped += 1;
        out.record_dispositions.push(RecordDisposition {
            record: record.clone(),
            outcome: RecordOutcome::Mapped(vec![target]),
        });
    }
}

/// The per-item turn carrier: `internal_chat_message_metadata_passthrough`
/// (bulk of the corpus) or `metadata` (function_call era).
fn item_turn_id(payload: &Value) -> Option<String> {
    for carrier in ["internal_chat_message_metadata_passthrough", "metadata"] {
        if let Some(t) = payload
            .get(carrier)
            .and_then(|m| m.get("turn_id"))
            .and_then(Value::as_str)
        {
            return Some(t.to_string());
        }
    }
    None
}

/// An unmatched content-bearing event maps directly.
fn map_unmatched_event(
    payload_type: &str,
    payload: &Value,
    timestamp: DateTime<Utc>,
    key: &LogicalSessionKey,
    record: &RecordRef,
    state: &mut WalkState,
    out: &mut NormalizeOutput,
) {
    match payload_type {
        "user_message" => {
            let text = payload
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            let idx = push_user(
                vec![ContentBlock::Text(TextBlock {
                    text,
                    extra: indexmap::IndexMap::default(),
                })],
                "user",
                timestamp,
                key,
                record,
                state,
                out,
            );
            let id = out.entries[idx].id.clone();
            if let Some(sem) = out.semantics.get_mut(&id) {
                // Unmatched (non-duplicate) user events have no response
                // twin: plausibly steering/mid-turn injections — human-
                // authored but NOT a turn boundary (round-23: an unmatched
                // user_message must not automatically open a human turn).
                sem.prompt = Some(PromptSemantics {
                    authorship: PromptAuthorship::Human,
                    delivery: PromptDelivery::MidTurn,
                });
            }
        }
        "agent_message" => {
            let text = payload
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            push_assistant(
                vec![ContentBlock::Text(TextBlock {
                    text,
                    extra: indexmap::IndexMap::default(),
                })],
                timestamp,
                key,
                record,
                state,
                out,
            );
        }
        _ => {
            let text = payload
                .get("text")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            push_assistant(
                vec![ContentBlock::Thinking(ThinkingBlock {
                    thinking: text,
                    signature: String::new(),
                    extra: indexmap::IndexMap::default(),
                })],
                timestamp,
                key,
                record,
                state,
                out,
            );
        }
    }
}

/// response_item `message`: role decides the side; a `user_message` event
/// twin (pre-computed plan) marks the prompt HUMAN.
fn normalize_message(
    payload: &Value,
    timestamp: DateTime<Utc>,
    key: &LogicalSessionKey,
    record: &RecordRef,
    plan: &MatchPlan,
    state: &mut WalkState,
    out: &mut NormalizeOutput,
) {
    let role = payload.get("role").and_then(Value::as_str).unwrap_or("");
    let blocks = content_blocks(payload.get("content"), role == "assistant");
    if role == "assistant" {
        push_assistant(blocks, timestamp, key, record, state, out);
    } else {
        let idx = push_user(blocks, role, timestamp, key, record, state, out);
        let id = out.entries[idx].id.clone();
        let authorship = if plan.human_responses.contains(&record.ordinal) {
            PromptAuthorship::Human
        } else {
            PromptAuthorship::Harness
        };
        if let Some(sem) = out.semantics.get_mut(&id) {
            sem.prompt = Some(PromptSemantics {
                authorship,
                delivery: PromptDelivery::TurnBoundary,
            });
        }
    }
}

/// input_text / output_text → Text; anything else survives as a
/// block-level Unknown.
fn content_blocks(content: Option<&Value>, assistant: bool) -> Vec<ContentBlock> {
    let expected = if assistant {
        "output_text"
    } else {
        "input_text"
    };
    let Some(items) = content.and_then(Value::as_array) else {
        return Vec::new();
    };
    items
        .iter()
        .map(|item| {
            let kind = item.get("type").and_then(Value::as_str).unwrap_or("");
            if kind == expected || kind == "input_text" || kind == "output_text" {
                ContentBlock::Text(TextBlock {
                    text: item
                        .get("text")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string(),
                    extra: indexmap::IndexMap::default(),
                })
            } else {
                ContentBlock::Unknown {
                    kind: kind.to_string(),
                    raw: item.clone(),
                }
            }
        })
        .collect()
}

/// reasoning → one ThinkingBlock: summary texts + content texts, encrypted
/// payload (when a string) as the signature — mirroring Claude's
/// empty-thinking-with-signature reality for encrypted-only eras.
fn reasoning_block(payload: &Value) -> ContentBlock {
    let mut parts: Vec<String> = Vec::new();
    for list in ["summary", "content"] {
        if let Some(items) = payload.get(list).and_then(Value::as_array) {
            for item in items {
                if let Some(text) = item.get("text").and_then(Value::as_str) {
                    if !text.trim().is_empty() {
                        parts.push(text.to_string());
                    }
                }
            }
        }
    }
    ContentBlock::Thinking(ThinkingBlock {
        thinking: parts.join("\n\n"),
        signature: payload
            .get("encrypted_content")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        extra: indexmap::IndexMap::default(),
    })
}

/// function_call / custom_tool_call → assistant ToolUse + tool semantics.
fn normalize_tool_call(
    payload: &Value,
    payload_type: &str,
    timestamp: DateTime<Utc>,
    key: &LogicalSessionKey,
    record: &RecordRef,
    state: &mut WalkState,
    out: &mut NormalizeOutput,
) {
    let name = payload
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or("unknown")
        .to_string();
    let call_id = payload
        .get("call_id")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let input = if payload_type == "function_call" {
        // `arguments` is a JSON-encoded string; keep the raw string when it
        // does not parse (never lose it).
        match payload.get("arguments").and_then(Value::as_str) {
            Some(args) => {
                serde_json::from_str(args).unwrap_or_else(|_| Value::String(args.to_string()))
            }
            None => Value::Null,
        }
    } else {
        payload.get("input").cloned().unwrap_or(Value::Null)
    };
    let idx = push_assistant(
        vec![ContentBlock::ToolUse(ToolUse {
            id: call_id.clone(),
            name: name.clone(),
            input,
            extra: indexmap::IndexMap::default(),
        })],
        timestamp,
        key,
        record,
        state,
        out,
    );
    let id = out.entries[idx].id.clone();
    if let Some(sem) = out.semantics.get_mut(&id) {
        sem.tools.insert(
            call_id,
            ToolSemantics {
                kind: classify_tool(&name),
                native_name: name,
                lifecycle: Vec::new(),
            },
        );
    }
}

/// web_search_call → assistant ToolUse (round-22: 341 corpus records were
/// left Unknown while the tool-call claim said "complete").
fn normalize_web_search(
    payload: &Value,
    timestamp: DateTime<Utc>,
    key: &LogicalSessionKey,
    record: &RecordRef,
    state: &mut WalkState,
    out: &mut NormalizeOutput,
) {
    // Native id field first (158/341 corpus records carry `ws_...` ids);
    // call_id as a fallback family; a synthesized id only for the id-less
    // era. status AND action are preserved in the input.
    let call_id = payload
        .get("id")
        .or_else(|| payload.get("call_id"))
        .and_then(Value::as_str)
        .map_or_else(|| format!("ws_{}", record.ordinal), str::to_string);
    let input = serde_json::json!({
        "status": payload.get("status").cloned().unwrap_or(Value::Null),
        "action": payload.get("action").cloned().unwrap_or(Value::Null),
    });
    let idx = push_assistant(
        vec![ContentBlock::ToolUse(ToolUse {
            id: call_id.clone(),
            name: "web_search".into(),
            input,
            extra: indexmap::IndexMap::default(),
        })],
        timestamp,
        key,
        record,
        state,
        out,
    );
    let id = out.entries[idx].id.clone();
    if let Some(sem) = out.semantics.get_mut(&id) {
        sem.tools.insert(
            call_id,
            ToolSemantics {
                kind: ToolKind::Web,
                native_name: "web_search".into(),
                lifecycle: Vec::new(),
            },
        );
    }
}

/// Canonical tool classification from Codex native tool names.
fn classify_tool(name: &str) -> ToolKind {
    match name {
        "shell" | "local_shell" | "exec_command" | "write_stdin" | "container.exec" => {
            ToolKind::Shell
        }
        "apply_patch" => ToolKind::FileWrite,
        "read_file" | "view_image" => ToolKind::FileRead,
        "web_search" | "browser.search" | "web.run" => ToolKind::Web,
        "grep" | "find" | "search" => ToolKind::Search,
        "exec" | "wait" | "update_plan" => ToolKind::Orchestration,
        n if n.starts_with("mcp") || n.contains("__") => ToolKind::Mcp,
        other => ToolKind::Other(other.to_string()),
    }
}

/// function_call_output / custom_tool_call_output → user-side ToolResult.
fn normalize_tool_output(
    payload: &Value,
    timestamp: DateTime<Utc>,
    key: &LogicalSessionKey,
    record: &RecordRef,
    state: &mut WalkState,
    out: &mut NormalizeOutput,
) {
    let call_id = payload
        .get("call_id")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let output = match payload.get("output") {
        Some(Value::String(s)) => s.clone(),
        Some(other) => other.to_string(),
        None => String::new(),
    };
    let idx = push_user(
        vec![ContentBlock::ToolResult(ToolResult {
            tool_use_id: call_id,
            content: Some(ToolResultContent::String(output)),
            is_error: None,
            extra: indexmap::IndexMap::default(),
        })],
        "user",
        timestamp,
        key,
        record,
        state,
        out,
    );
    let id = out.entries[idx].id.clone();
    if let Some(sem) = out.semantics.get_mut(&id) {
        sem.prompt = Some(PromptSemantics {
            authorship: PromptAuthorship::Tool,
            delivery: PromptDelivery::MidTurn,
        });
    }
}

/// A `compacted` envelope is one chronological system boundary. Its
/// `replacement_history` is a nested reconstruction snapshot and therefore
/// stays nested in `extra`; it is NEVER expanded into normalized entries or
/// counted as new activity.
fn normalize_compaction(
    payload: &Value,
    timestamp: DateTime<Utc>,
    key: &LogicalSessionKey,
    record: &RecordRef,
    state: &mut WalkState,
    out: &mut NormalizeOutput,
) {
    let previous = state.last_uuid.clone();
    let mut extra = indexmap::IndexMap::new();
    if let Some(object) = payload.as_object() {
        for (name, value) in object {
            if name != "message" {
                extra.insert(name.clone(), value.clone());
            }
        }
    }
    let legacy_number = payload.get("window_id").and_then(Value::as_u64);
    let window = CompactionWindow {
        number: payload
            .get("window_number")
            .and_then(Value::as_u64)
            .or(legacy_number),
        first_id: payload
            .get("first_window_id")
            .and_then(Value::as_str)
            .map(str::to_string),
        previous_id: payload
            .get("previous_window_id")
            .and_then(Value::as_str)
            .map(str::to_string),
        id: payload
            .get("window_id")
            .and_then(Value::as_str)
            .map(str::to_string),
        legacy_numeric_id: legacy_number.is_some(),
    };
    let replacement_history_items = payload
        .get("replacement_history")
        .and_then(Value::as_array)
        .map(Vec::len);
    let entry = LogEntry::System(SystemMessage {
        uuid: synthetic_uuid(key, record.ordinal),
        parent_uuid: previous.clone(),
        logical_parent_uuid: previous,
        subtype: Some(SystemSubtype::CompactBoundary),
        content: payload
            .get("message")
            .and_then(Value::as_str)
            .map(str::to_string),
        level: Some("info".into()),
        is_meta: Some(true),
        timestamp,
        session_id: Some(key.native_id.clone()),
        version: Some(state.version.clone()),
        cwd: state.cwd.clone(),
        git_branch: None,
        is_sidechain: None,
        user_type: None,
        compact_metadata: None,
        error: None,
        retry_in_ms: None,
        retry_attempt: None,
        max_retries: None,
        cause: None,
        hook_count: None,
        hook_infos: Vec::new(),
        has_output: None,
        prevented_continuation: None,
        stop_reason: None,
        tool_use_id: None,
        checkpoint_id: None,
        target_uuid: None,
        rewind_mode: None,
        affected_files: Vec::new(),
        new_name: None,
        old_name: None,
        extra,
    });
    let idx = push_mapped(entry, key, record, state, out);
    let id = out.entries[idx].id.clone();
    out.semantics
        .get_mut(&id)
        .expect("mapped compaction has semantics")
        .compaction = Some(CompactionSemantics {
        kind: CompactionKind::Full,
        replacement_history_items,
        window,
    });
}

/// token_count: canonical usage from CUMULATIVE transitions (round-22
/// blocker 2), attached to the current window's assistant emission or held
/// for the next one.
fn handle_token_count(
    payload: &Value,
    value: &Value,
    record: &RecordRef,
    state: &mut WalkState,
    out: &mut NormalizeOutput,
) {
    let info = payload
        .get("info")
        .filter(|i| i.is_object() && i.get("total_token_usage").is_some());
    let Some(info) = info else {
        // Heartbeat without usage numbers: nothing to account for.
        out.diagnostics.suppressed += 1;
        out.record_dispositions.push(RecordDisposition {
            record: record.clone(),
            outcome: RecordOutcome::Suppressed {
                reason: SuppressionReason::Other("token_count heartbeat without usage info".into()),
            },
        });
        return;
    };
    let total_obs = ObservationNumbers::read(info.get("total_token_usage"));
    let last_obs = ObservationNumbers::read(info.get("last_token_usage"));
    let total = total_obs.canonical();
    let observation_kind = if total_obs.total > 0
        && total_obs.input == 0
        && total_obs.cached == 0
        && total_obs.output == 0
        && total_obs.reasoning_output == 0
    {
        UsageObservationKind::ContextWindow
    } else {
        UsageObservationKind::ModelTokens
    };
    // The basis is SOURCE-BACKED, not detected (round-24): Codex's own
    // TokenUsage defines non-cached input as input_tokens −
    // cached_input_tokens across every audited tag (0.31 … 0.144.6), and
    // the corpus census found zero cumulative observations contradicting
    // it. It is validated PER OBSERVATION: an observation whose own
    // numbers contradict the basis (cached > input — four Call
    // observations in one January session) is marked Unknown/ambiguous
    // with its raw values preserved, and never reinterprets the session.
    let fresh_of = |r: RawUsage| r.input.saturating_sub(r.cached);
    // Transition rule on the cumulative FRESH stream: unchanged → zero;
    // input/output decrease → epoch reset (the new cumulative IS the first
    // delta). A fresh decrease WITHOUT a reset is uninterpretable: the
    // FRESH delta is zeroed and the Cumulative observation is flagged
    // ambiguous; the cached and output deltas remain well-defined and
    // still contribute (field-specific ambiguity).
    let mut ambiguous_transition = false;
    let canonical = match state.prev_total {
        None => Usage {
            input_tokens: fresh_of(total),
            output_tokens: total.output,
            cache_read_input_tokens: Some(total.cached),
            ..Default::default()
        },
        Some(prev) => {
            if total.input < prev.input || total.output < prev.output {
                Usage {
                    input_tokens: fresh_of(total),
                    output_tokens: total.output,
                    cache_read_input_tokens: Some(total.cached),
                    ..Default::default()
                }
            } else {
                let fresh_now = fresh_of(total);
                let fresh_prev = fresh_of(prev);
                if fresh_now < fresh_prev {
                    ambiguous_transition = true;
                }
                Usage {
                    input_tokens: fresh_now.saturating_sub(fresh_prev),
                    output_tokens: total.output - prev.output,
                    cache_read_input_tokens: Some(total.cached.saturating_sub(prev.cached)),
                    ..Default::default()
                }
            }
        }
    };
    state.prev_total = Some(total);

    let pending = PendingUsage {
        record: record.clone(),
        value: value.clone(),
        window: state.window,
        canonical,
        observation_kind,
        last_obs,
        total_obs,
        model_context_window: info.get("model_context_window").and_then(Value::as_i64),
        ambiguous_transition,
    };
    match state.last_assistant {
        Some((idx, window)) if window == state.window => attach_usage(pending, idx, out),
        _ => state.pending_usage.push(pending),
    }
}

fn attach_usage(pending: PendingUsage, idx: usize, out: &mut NormalizeOutput) {
    let id = out.entries[idx].id.clone();
    out.entry_origins
        .get_mut(&id)
        .expect("assistant entry has origins")
        .push(pending.record.clone());
    out.diagnostics.mapped += 1;
    out.record_dispositions.push(RecordDisposition {
        record: pending.record.clone(),
        outcome: RecordOutcome::Mapped(vec![id.clone()]),
    });
    if let LogEntry::Assistant(msg) = &mut out.entries[idx].entry {
        let usage = msg.message.usage.get_or_insert_with(Usage::default);
        usage.input_tokens += pending.canonical.input_tokens;
        usage.output_tokens += pending.canonical.output_tokens;
        if let Some(c) = pending.canonical.cache_read_input_tokens {
            *usage.cache_read_input_tokens.get_or_insert(0) += c;
        }
    }
    if let Some(sem) = out.semantics.get_mut(&id) {
        // Basis and ambiguity are PER OBSERVATION (round-24): an
        // observation whose own numbers contradict the includes-cached
        // basis is Unknown/ambiguous; the Cumulative observation is also
        // ambiguous when its transition's fresh delta was uninterpretable.
        let obs_basis = |kind: UsageObservationKind, n: ObservationNumbers| {
            if kind == UsageObservationKind::ContextWindow || n.contradicts_input_basis() {
                super::UsageBasis::Unknown
            } else {
                super::UsageBasis::InputIncludesCached
            }
        };
        let last_contradicts =
            pending.last_obs.contradicts_input_basis() || pending.last_obs.has_negative_counter();
        let total_contradicts =
            pending.total_obs.contradicts_input_basis() || pending.total_obs.has_negative_counter();
        for (scope, aggregation, numbers, ambiguous) in [
            (
                UsageScope::Call,
                UsageAggregation::Delta,
                pending.last_obs,
                last_contradicts,
            ),
            (
                UsageScope::Session,
                UsageAggregation::Cumulative,
                pending.total_obs,
                total_contradicts || pending.ambiguous_transition,
            ),
        ] {
            sem.usage.push(UsageObservation {
                kind: pending.observation_kind,
                scope,
                aggregation,
                record: pending.record.clone(),
                basis: obs_basis(pending.observation_kind, numbers),
                ambiguous,
                input_tokens: numbers.input,
                cached_input_tokens: numbers.cached,
                output_tokens: numbers.output,
                reasoning_output_tokens: numbers.reasoning_output,
                total_tokens: numbers.total,
                model_context_window: pending.model_context_window,
            });
        }
    }
}

/// Shared mapped-entry plumbing: id at `(ordinal, 0)` (constraint 1),
/// synthetic uuid = the INJECTIVE EntryId encoding (round-22: never a bare
/// `native:ordinal` that omits provider and namespace), origins,
/// disposition, base semantics with the current turn id.
fn push_mapped(
    entry: LogEntry,
    key: &LogicalSessionKey,
    record: &RecordRef,
    state: &mut WalkState,
    out: &mut NormalizeOutput,
) -> usize {
    let id = EntryId::deterministic(key, record.ordinal, 0);
    out.entry_origins.insert(id.clone(), vec![record.clone()]);
    out.diagnostics.mapped += 1;
    out.record_dispositions.push(RecordDisposition {
        record: record.clone(),
        outcome: RecordOutcome::Mapped(vec![id.clone()]),
    });
    out.semantics.insert(
        id.clone(),
        EntrySemantics {
            activity: ActivityKind::New,
            turn_id: state.turn_id.clone(),
            ..Default::default()
        },
    );
    state.last_uuid = Some(id.to_string());
    out.entries.push(IdentifiedEntry { id, entry });
    out.entries.len() - 1
}

fn synthetic_uuid(key: &LogicalSessionKey, ordinal: u64) -> String {
    EntryId::deterministic(key, ordinal, 0).to_string()
}

fn synthetic_uuid_at(key: &LogicalSessionKey, ordinal: u64, subindex: u32) -> String {
    EntryId::deterministic(key, ordinal, subindex).to_string()
}

/// An event-only lifecycle call is a tool emission, but not a model response
/// and therefore must never become the owner of pending model-token usage.
#[allow(clippy::too_many_arguments)]
fn push_lifecycle_tool_use(
    call_id: &str,
    name: &str,
    input: Value,
    timestamp: DateTime<Utc>,
    key: &LogicalSessionKey,
    record: &RecordRef,
    state: &mut WalkState,
    out: &mut NormalizeOutput,
) -> usize {
    let parent = state.last_uuid.clone();
    let entry = LogEntry::Assistant(AssistantMessage {
        uuid: synthetic_uuid(key, record.ordinal),
        parent_uuid: parent,
        timestamp,
        session_id: key.native_id.clone(),
        version: state.version.clone(),
        cwd: state.cwd.clone(),
        git_branch: None,
        user_type: None,
        is_sidechain: false,
        is_teammate: None,
        agent_id: None,
        slug: None,
        request_id: None,
        is_api_error_message: None,
        message: AssistantContent {
            id: synthetic_uuid(key, record.ordinal),
            msg_type: "message".into(),
            role: "assistant".into(),
            model: state.model.clone(),
            content: vec![ContentBlock::ToolUse(ToolUse {
                id: call_id.to_string(),
                name: name.to_string(),
                input,
                extra: indexmap::IndexMap::default(),
            })],
            stop_reason: None,
            stop_sequence: None,
            usage: None,
            container: None,
            context_management: None,
            extra: indexmap::IndexMap::default(),
        },
        extra: indexmap::IndexMap::default(),
    });
    push_mapped(entry, key, record, state, out)
}

/// Add the ToolResult half of a one-record -> two-entry lifecycle mapping.
/// The first ToolUse already created the record disposition and diagnostics;
/// this helper extends that same mapping rather than double-counting it.
#[allow(clippy::too_many_arguments)]
fn push_secondary_tool_result(
    call_id: &str,
    output: String,
    is_error: bool,
    timestamp: DateTime<Utc>,
    key: &LogicalSessionKey,
    record: &RecordRef,
    state: &mut WalkState,
    out: &mut NormalizeOutput,
) -> usize {
    let id = EntryId::deterministic(key, record.ordinal, 1);
    let parent = state.last_uuid.clone();
    let entry = LogEntry::User(UserMessage {
        uuid: synthetic_uuid_at(key, record.ordinal, 1),
        parent_uuid: parent,
        timestamp,
        session_id: key.native_id.clone(),
        version: state.version.clone(),
        cwd: state.cwd.clone(),
        git_branch: None,
        user_type: None,
        is_sidechain: false,
        is_teammate: None,
        agent_id: None,
        slug: None,
        is_meta: None,
        is_compact_summary: None,
        is_visible_in_transcript_only: None,
        thinking_metadata: None,
        todos: Vec::new(),
        tool_use_result: None,
        message: UserContent::Blocks(UserBlocksContent {
            role: "user".into(),
            content: vec![ContentBlock::ToolResult(ToolResult {
                tool_use_id: call_id.to_string(),
                content: Some(ToolResultContent::String(output)),
                is_error: Some(is_error),
                extra: indexmap::IndexMap::default(),
            })],
            extra: indexmap::IndexMap::default(),
        }),
        extra: indexmap::IndexMap::default(),
    });
    out.entry_origins.insert(id.clone(), vec![record.clone()]);
    out.semantics.insert(
        id.clone(),
        EntrySemantics {
            activity: ActivityKind::New,
            prompt: Some(PromptSemantics {
                authorship: PromptAuthorship::Tool,
                delivery: PromptDelivery::MidTurn,
            }),
            turn_id: state.turn_id.clone(),
            ..Default::default()
        },
    );
    let disposition = out
        .record_dispositions
        .iter_mut()
        .find(|disposition| disposition.record == *record)
        .expect("primary lifecycle entry created a record disposition");
    let RecordOutcome::Mapped(entries) = &mut disposition.outcome else {
        unreachable!("primary lifecycle entry is mapped")
    };
    entries.push(id.clone());
    state.last_uuid = Some(id.to_string());
    out.entries.push(IdentifiedEntry { id, entry });
    out.entries.len() - 1
}

fn push_assistant(
    blocks: Vec<ContentBlock>,
    timestamp: DateTime<Utc>,
    key: &LogicalSessionKey,
    record: &RecordRef,
    state: &mut WalkState,
    out: &mut NormalizeOutput,
) -> usize {
    let parent = state.last_uuid.clone();
    let entry = LogEntry::Assistant(AssistantMessage {
        uuid: synthetic_uuid(key, record.ordinal),
        parent_uuid: parent,
        timestamp,
        session_id: key.native_id.clone(),
        version: state.version.clone(),
        cwd: state.cwd.clone(),
        git_branch: None,
        user_type: None,
        is_sidechain: false,
        is_teammate: None,
        agent_id: None,
        slug: None,
        request_id: None,
        is_api_error_message: None,
        message: AssistantContent {
            id: synthetic_uuid(key, record.ordinal),
            msg_type: "message".into(),
            role: "assistant".into(),
            model: state.model.clone(),
            content: blocks,
            stop_reason: None,
            stop_sequence: None,
            usage: None,
            container: None,
            context_management: None,
            extra: indexmap::IndexMap::default(),
        },
        extra: indexmap::IndexMap::default(),
    });
    let idx = push_mapped(entry, key, record, state, out);
    state.last_assistant = Some((idx, state.window));
    // Usage events that arrived earlier IN THIS WINDOW attach now (a
    // boundary already flushed anything older as preserved).
    let pending: Vec<PendingUsage> = std::mem::take(&mut state.pending_usage);
    for p in pending {
        debug_assert_eq!(p.window, state.window, "boundary flush must have run");
        attach_usage(p, idx, out);
    }
    idx
}

fn push_user(
    blocks: Vec<ContentBlock>,
    role: &str,
    timestamp: DateTime<Utc>,
    key: &LogicalSessionKey,
    record: &RecordRef,
    state: &mut WalkState,
    out: &mut NormalizeOutput,
) -> usize {
    let parent = state.last_uuid.clone();
    let entry = LogEntry::User(UserMessage {
        uuid: synthetic_uuid(key, record.ordinal),
        parent_uuid: parent,
        timestamp,
        session_id: key.native_id.clone(),
        version: state.version.clone(),
        cwd: state.cwd.clone(),
        git_branch: None,
        user_type: None,
        is_sidechain: false,
        is_teammate: None,
        agent_id: None,
        slug: None,
        is_meta: None,
        is_compact_summary: None,
        is_visible_in_transcript_only: None,
        thinking_metadata: None,
        todos: Vec::new(),
        tool_use_result: None,
        message: UserContent::Blocks(UserBlocksContent {
            role: role.to_string(),
            content: blocks,
            extra: indexmap::IndexMap::default(),
        }),
        extra: indexmap::IndexMap::default(),
    });
    push_mapped(entry, key, record, state, out)
}

fn push_unknown(
    value: Value,
    key: &LogicalSessionKey,
    record: &RecordRef,
    out: &mut NormalizeOutput,
) -> EntryId {
    let id = EntryId::deterministic(key, record.ordinal, 0);
    out.entry_origins.insert(id.clone(), vec![record.clone()]);
    out.diagnostics.unknown += 1;
    out.record_dispositions.push(RecordDisposition {
        record: record.clone(),
        outcome: RecordOutcome::Unknown {
            entries: vec![id.clone()],
        },
    });
    out.entries.push(IdentifiedEntry {
        id: id.clone(),
        entry: LogEntry::Unknown(value),
    });
    id
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn current_host_control_tools_are_not_shell_commands() {
        assert_eq!(classify_tool("exec"), ToolKind::Orchestration);
        assert_eq!(classify_tool("wait"), ToolKind::Orchestration);
        assert_eq!(classify_tool("update_plan"), ToolKind::Orchestration);
        assert_eq!(classify_tool("exec_command"), ToolKind::Shell);
    }
}
