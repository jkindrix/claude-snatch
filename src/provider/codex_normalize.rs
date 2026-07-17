//! B3 slice 1: normalize Codex envelope records into the common model.
//!
//! Covers user/assistant content, reasoning summaries, tool calls/results
//! (including `web_search_call`), and usage. The mapping is recorded in
//! `docs/multi-provider-design.md` ("B3 slice 1 — normalization mapping",
//! amended by B3.1) and rests on the 224-session corpus census plus the
//! round-22 audit. Binding constraints (round-21):
//!
//! - Mapped records keep their B1 deterministic ids `(ordinal, 0)`.
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
//! Everything outside the slice (session_meta, turn_context, world_state,
//! ghost_snapshot, compacted, task events, ...) remains a preserved
//! `Unknown` entry — consumed as normalization STATE where useful, with
//! its disposition unchanged. Fork-inherited history, compaction, and
//! spawn lineage are later B3 slices.

use std::collections::{BTreeMap, BTreeSet};

use chrono::{DateTime, Utc};
use serde_json::Value;

use crate::model::usage::Usage;
use crate::model::{
    AssistantContent, AssistantMessage, ContentBlock, LogEntry, TextBlock, ThinkingBlock,
    ToolResult, ToolResultContent, ToolUse, UserBlocksContent, UserContent, UserMessage,
};

use super::{
    ActivityKind, EntryId, EntrySemantics, IdentifiedEntry, IngestionDiagnostics,
    LogicalSessionKey, PromptAuthorship, PromptDelivery, PromptSemantics, RecordDisposition,
    RecordOutcome, RecordRef, SuppressionReason, ToolKind, ToolSemantics, UsageAggregation,
    UsageObservation, UsageScope,
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

fn claim(pool: &mut [Candidate], text: &str) -> Option<u64> {
    pool.iter_mut()
        .find(|c| !c.claimed && c.text == text)
        .map(|c| {
            c.claimed = true;
            c.ordinal
        })
}

fn plan_matches(records: &[(RecordRef, Value)]) -> MatchPlan {
    let mut plan = MatchPlan::default();
    let mut window_start = 0usize;
    let mut i = 0usize;
    loop {
        let at_boundary = i == records.len() || {
            let (et, _, pt) = envelope_parts(&records[i].1);
            is_window_boundary(et, pt)
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

/// Raw cumulative usage triple straight from a codex usage object.
#[derive(Clone, Copy, PartialEq, Eq)]
struct RawUsage {
    input: u64,
    cached: u64,
    output: u64,
}

impl RawUsage {
    fn read(v: Option<&Value>) -> Self {
        let get = |k: &str| {
            v.and_then(|u| u.get(k))
                .and_then(Value::as_u64)
                .unwrap_or(0)
        };
        RawUsage {
            input: get("input_tokens"),
            cached: get("cached_input_tokens"),
            output: get("output_tokens"),
        }
    }
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
    last_obs: ObservationNumbers,
    total_obs: ObservationNumbers,
    ambiguous: bool,
}

/// Raw native numbers destined for an observation.
#[derive(Clone, Copy)]
struct ObservationNumbers {
    input: u64,
    cached: u64,
    output: u64,
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
    /// Session usage basis (detected up front from cumulative totals).
    usage_basis: super::UsageBasis,
    /// Usage events awaiting the next assistant emission.
    pending_usage: Vec<PendingUsage>,
    /// Previous mapped entry's synthetic uuid (linear threading).
    last_uuid: Option<String>,
}

/// Detect the session's usage basis from its cumulative totals: an
/// includes-cached basis makes `cached > input` impossible, so observing it
/// anywhere proves the excludes basis. Sessions without usage records get
/// `Unknown` (no canonical derivation happens anyway).
fn detect_usage_basis(records: &[(RecordRef, Value)]) -> super::UsageBasis {
    let mut saw_usage = false;
    for (_, value) in records {
        let (et, payload, pt) = envelope_parts(value);
        if et == "event_msg" && pt == "token_count" {
            if let Some(total) = payload
                .get("info")
                .and_then(|i| i.get("total_token_usage"))
                .filter(|t| t.is_object())
            {
                saw_usage = true;
                let raw = RawUsage::read(Some(total));
                if raw.cached > raw.input {
                    return super::UsageBasis::InputExcludesCached;
                }
            }
        }
    }
    if saw_usage {
        super::UsageBasis::InputIncludesCached
    } else {
        super::UsageBasis::Unknown
    }
}

pub(super) fn normalize(
    key: &LogicalSessionKey,
    records: &[(RecordRef, Value)],
) -> NormalizeOutput {
    let mut out = NormalizeOutput {
        entries: Vec::new(),
        entry_origins: BTreeMap::new(),
        record_dispositions: Vec::new(),
        semantics: BTreeMap::new(),
        diagnostics: IngestionDiagnostics::default(),
    };
    let plan = plan_matches(records);
    let usage_basis = detect_usage_basis(records);

    let mut state = WalkState {
        version: "unknown".into(),
        cwd: None,
        model: "unknown".into(),
        turn_id: None,
        window: 0,
        last_assistant: None,
        prev_total: None,
        usage_basis,
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

        if is_window_boundary(envelope_type, payload_type) {
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
    // Canonical entry order = record order (late-attached leftovers above
    // would otherwise trail out of place).
    out.entries.sort_by_key(|e| (e.id.ordinal, e.id.subindex));
    out
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
            },
        );
    }
}

/// Canonical tool classification from Codex native tool names.
fn classify_tool(name: &str) -> ToolKind {
    match name {
        "shell" | "local_shell" | "exec_command" | "container.exec" => ToolKind::Shell,
        "apply_patch" => ToolKind::FileWrite,
        "read_file" | "view_image" => ToolKind::FileRead,
        "web_search" | "browser.search" | "web.run" => ToolKind::Web,
        "grep" | "find" | "search" => ToolKind::Search,
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
    let total = RawUsage::read(info.get("total_token_usage"));
    let last = RawUsage::read(info.get("last_token_usage"));
    // Cumulative FRESH stream under the DETECTED basis (round-23 blocker 4:
    // canonical derivation is only meaningful when the basis is known):
    // includes-cached → fresh = input − cached; excludes → fresh = input.
    let fresh_of = |r: RawUsage| match state.usage_basis {
        super::UsageBasis::InputIncludesCached => r.input.saturating_sub(r.cached),
        super::UsageBasis::InputExcludesCached | super::UsageBasis::Unknown => r.input,
    };
    // Transition rule: unchanged → zero; input/output decrease → epoch
    // reset (the new cumulative IS the first delta). A fresh decrease
    // WITHOUT a reset is uninterpretable under the detected basis: it is
    // surfaced as an AMBIGUOUS observation and contributes zero fresh —
    // never silently clamped into a wrong number.
    let mut ambiguous = false;
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
                    ambiguous = true;
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
        last_obs: ObservationNumbers {
            input: last.input,
            cached: last.cached,
            output: last.output,
        },
        total_obs: ObservationNumbers {
            input: total.input,
            cached: total.cached,
            output: total.output,
        },
        ambiguous,
    };
    let basis = state.usage_basis;
    match state.last_assistant {
        Some((idx, window)) if window == state.window => attach_usage(pending, basis, idx, out),
        _ => state.pending_usage.push(pending),
    }
}

fn attach_usage(
    pending: PendingUsage,
    basis: super::UsageBasis,
    idx: usize,
    out: &mut NormalizeOutput,
) {
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
        for (scope, aggregation, numbers) in [
            (UsageScope::Call, UsageAggregation::Delta, pending.last_obs),
            (
                UsageScope::Session,
                UsageAggregation::Cumulative,
                pending.total_obs,
            ),
        ] {
            sem.usage.push(UsageObservation {
                scope,
                aggregation,
                record: pending.record.clone(),
                basis,
                ambiguous: pending.ambiguous,
                input_tokens: numbers.input,
                cached_input_tokens: numbers.cached,
                output_tokens: numbers.output,
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
    let basis = state.usage_basis;
    for p in pending {
        debug_assert_eq!(p.window, state.window, "boundary flush must have run");
        attach_usage(p, basis, idx, out);
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
) {
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
        id,
        entry: LogEntry::Unknown(value),
    });
}
