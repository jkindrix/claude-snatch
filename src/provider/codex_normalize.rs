//! B3 slice 1: normalize Codex envelope records into the common model.
//!
//! Covers user/assistant content, reasoning summaries, tool calls/results,
//! and usage — the reviewer-set first checkpoint. The mapping is recorded
//! in `docs/multi-provider-design.md` ("B3 slice 1 — normalization
//! mapping") and rests on the 224-session corpus census. Binding
//! constraints (round-21): mapped records keep their B1 deterministic ids
//! `(ordinal, 0)`; `turn_id` rides the semantics sidecar, never message
//! identity; deduplication is by emission identity (the `response_item`
//! stream is authoritative for content; `event_msg` content records are
//! its UI twin), never text equality; usage deltas accumulate onto
//! entries so summing entry usage cannot double-count.
//!
//! Everything outside the slice (session_meta, turn_context, world_state,
//! ghost_snapshot, compacted, task events, ...) remains a preserved
//! `Unknown` entry — consumed as normalization STATE where useful, with
//! its disposition unchanged. Fork-inherited history, compaction, and
//! spawn lineage are later B3 slices.

use std::collections::BTreeMap;

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

/// Session-level state threaded through the linear walk.
struct WalkState {
    version: String,
    cwd: Option<String>,
    model: String,
    turn_id: Option<String>,
    /// Index (into `entries`) of the most recent assistant-authored entry —
    /// the attachment point for `token_count` usage.
    last_assistant: Option<usize>,
    /// Indices of user-role entries not yet claimed by a `user_message`
    /// event (claim marks them human-authored).
    unclaimed_user_entries: Vec<usize>,
    /// `user_message` events seen before their user entry (order-robust).
    pending_human_claims: usize,
    /// Previous mapped entry's synthetic uuid (linear threading).
    last_uuid: Option<String>,
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
    // Emission-identity dedup precondition: the response_item stream is
    // authoritative whenever it exists at all. The corpus has zero
    // event-only sessions; the single-stream path exists for correctness
    // and is fixture-tested.
    let dual_stream = records
        .iter()
        .any(|(_, v)| v.get("type").and_then(Value::as_str) == Some("response_item"));

    let mut state = WalkState {
        version: "unknown".into(),
        cwd: None,
        model: "unknown".into(),
        turn_id: None,
        last_assistant: None,
        unclaimed_user_entries: Vec::new(),
        pending_human_claims: 0,
        last_uuid: None,
    };

    for (record, value) in records {
        let envelope_type = value.get("type").and_then(Value::as_str).unwrap_or("");
        let payload = value.get("payload").unwrap_or(&Value::Null);
        let payload_type = payload.get("type").and_then(Value::as_str).unwrap_or("");
        let timestamp = value
            .get("timestamp")
            .and_then(Value::as_str)
            .and_then(|t| DateTime::parse_from_rfc3339(t).ok())
            .map_or_else(|| DateTime::<Utc>::UNIX_EPOCH, |t| t.with_timezone(&Utc));

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
            _ => {}
        }

        let mapped = match (envelope_type, payload_type) {
            ("response_item", "message") => {
                normalize_message(payload, timestamp, key, record, &mut state, &mut out)
            }
            ("response_item", "reasoning") => Some(push_assistant(
                vec![reasoning_block(payload)],
                timestamp,
                key,
                record,
                &mut state,
                &mut out,
            )),
            ("response_item", "function_call") | ("response_item", "custom_tool_call") => {
                Some(normalize_tool_call(
                    payload,
                    payload_type,
                    timestamp,
                    key,
                    record,
                    &mut state,
                    &mut out,
                ))
            }
            ("response_item", "function_call_output")
            | ("response_item", "custom_tool_call_output") => Some(normalize_tool_output(
                payload, timestamp, key, record, &mut state, &mut out,
            )),
            ("event_msg", "user_message") => {
                if dual_stream {
                    // The response_item stream carries this prompt's durable
                    // content; the event marks it HUMAN-authored.
                    if let Some(idx) = state.unclaimed_user_entries.pop() {
                        let id = out.entries[idx].id.clone();
                        if let Some(sem) = out.semantics.get_mut(&id) {
                            sem.prompt = Some(PromptSemantics {
                                authorship: PromptAuthorship::Human,
                                delivery: PromptDelivery::TurnBoundary,
                            });
                        }
                    } else {
                        state.pending_human_claims += 1;
                    }
                    suppress_duplicate(record, &mut out);
                    None
                } else {
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
                        &mut state,
                        &mut out,
                    );
                    let id = out.entries[idx].id.clone();
                    if let Some(sem) = out.semantics.get_mut(&id) {
                        sem.prompt = Some(PromptSemantics {
                            authorship: PromptAuthorship::Human,
                            delivery: PromptDelivery::TurnBoundary,
                        });
                    }
                    Some(idx)
                }
            }
            ("event_msg", "agent_message") => {
                if dual_stream {
                    suppress_duplicate(record, &mut out);
                    None
                } else {
                    let text = payload
                        .get("message")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string();
                    Some(push_assistant(
                        vec![ContentBlock::Text(TextBlock {
                            text,
                            extra: indexmap::IndexMap::default(),
                        })],
                        timestamp,
                        key,
                        record,
                        &mut state,
                        &mut out,
                    ))
                }
            }
            ("event_msg", "agent_reasoning") | ("event_msg", "agent_reasoning_raw_content") => {
                if dual_stream {
                    suppress_duplicate(record, &mut out);
                    None
                } else {
                    let text = payload
                        .get("text")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string();
                    Some(push_assistant(
                        vec![ContentBlock::Thinking(ThinkingBlock {
                            thinking: text,
                            signature: String::new(),
                            extra: indexmap::IndexMap::default(),
                        })],
                        timestamp,
                        key,
                        record,
                        &mut state,
                        &mut out,
                    ))
                }
            }
            ("event_msg", "token_count") => {
                attach_usage(payload, record, &mut state, &mut out);
                None
            }
            // Everything else: preserved, honestly unmodeled — a later
            // slice's business.
            _ => {
                push_unknown(value.clone(), key, record, &mut out);
                None
            }
        };
        let _ = mapped;
    }
    out
}

/// response_item `message`: role decides the side of the conversation.
fn normalize_message(
    payload: &Value,
    timestamp: DateTime<Utc>,
    key: &LogicalSessionKey,
    record: &RecordRef,
    state: &mut WalkState,
    out: &mut NormalizeOutput,
) -> Option<usize> {
    let role = payload.get("role").and_then(Value::as_str).unwrap_or("");
    let blocks = content_blocks(payload.get("content"), role == "assistant");
    if role == "assistant" {
        Some(push_assistant(blocks, timestamp, key, record, state, out))
    } else {
        let idx = push_user(blocks, role, timestamp, key, record, state, out);
        let id = out.entries[idx].id.clone();
        let authorship = if role == "user" {
            if state.pending_human_claims > 0 {
                state.pending_human_claims -= 1;
                PromptAuthorship::Human
            } else {
                state.unclaimed_user_entries.push(idx);
                PromptAuthorship::Harness
            }
        } else {
            PromptAuthorship::Harness
        };
        if let Some(sem) = out.semantics.get_mut(&id) {
            sem.prompt = Some(PromptSemantics {
                authorship,
                delivery: PromptDelivery::TurnBoundary,
            });
        }
        Some(idx)
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
) -> usize {
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
    idx
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
) -> usize {
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
    idx
}

/// token_count → Mapped INTO the most recent assistant entry (N:1
/// provenance). The entry's Usage accumulates the DELTA so summing entry
/// usage never double-counts; both observations attach with their axes.
fn attach_usage(
    payload: &Value,
    record: &RecordRef,
    state: &mut WalkState,
    out: &mut NormalizeOutput,
) {
    let info = payload.get("info");
    let (Some(info), Some(idx)) = (info.filter(|i| i.is_object()), state.last_assistant) else {
        let reason = if state.last_assistant.is_none() {
            "token_count with no attributable assistant emission"
        } else {
            "token_count heartbeat without usage info"
        };
        out.diagnostics.suppressed += 1;
        out.record_dispositions.push(RecordDisposition {
            record: record.clone(),
            outcome: RecordOutcome::Suppressed {
                reason: SuppressionReason::Other(reason.into()),
            },
        });
        return;
    };
    let delta = usage_from(info.get("last_token_usage"));
    let total = usage_from(info.get("total_token_usage"));
    let id = out.entries[idx].id.clone();

    // N:1 provenance: this record maps into the existing assistant entry.
    out.entry_origins
        .get_mut(&id)
        .expect("assistant entry has origins")
        .push(record.clone());
    out.diagnostics.mapped += 1;
    out.record_dispositions.push(RecordDisposition {
        record: record.clone(),
        outcome: RecordOutcome::Mapped(vec![id.clone()]),
    });

    if let LogEntry::Assistant(msg) = &mut out.entries[idx].entry {
        let usage = msg.message.usage.get_or_insert_with(Usage::default);
        usage.input_tokens += delta.input_tokens;
        usage.output_tokens += delta.output_tokens;
        if let Some(c) = delta.cache_read_input_tokens {
            *usage.cache_read_input_tokens.get_or_insert(0) += c;
        }
    }
    if let Some(sem) = out.semantics.get_mut(&id) {
        sem.usage.push(UsageObservation {
            scope: UsageScope::Call,
            aggregation: UsageAggregation::Delta,
            usage: delta,
        });
        sem.usage.push(UsageObservation {
            scope: UsageScope::Session,
            aggregation: UsageAggregation::Cumulative,
            usage: total,
        });
    }
}

/// Map Codex usage numbers into the model's [`Usage`]: Codex
/// `input_tokens` INCLUDES cached tokens, the model's `input_tokens` is
/// fresh input — so fresh = input − cached, cached → cache_read.
fn usage_from(v: Option<&Value>) -> Usage {
    let get = |k: &str| {
        v.and_then(|u| u.get(k))
            .and_then(Value::as_u64)
            .unwrap_or(0)
    };
    let input = get("input_tokens");
    let cached = get("cached_input_tokens");
    Usage {
        input_tokens: input.saturating_sub(cached),
        output_tokens: get("output_tokens"),
        cache_read_input_tokens: Some(cached),
        ..Default::default()
    }
}

fn suppress_duplicate(record: &RecordRef, out: &mut NormalizeOutput) {
    out.diagnostics.suppressed += 1;
    out.record_dispositions.push(RecordDisposition {
        record: record.clone(),
        outcome: RecordOutcome::Suppressed {
            reason: SuppressionReason::DuplicateStream,
        },
    });
}

/// Shared mapped-entry plumbing: id at `(ordinal, 0)` (constraint 1),
/// synthetic linear-thread uuid, origins, disposition, base semantics with
/// the current turn id.
#[allow(clippy::too_many_arguments)]
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
    out.entries.push(IdentifiedEntry { id, entry });
    state.last_uuid = Some(synthetic_uuid(key, record.ordinal));
    out.entries.len() - 1
}

fn synthetic_uuid(key: &LogicalSessionKey, ordinal: u64) -> String {
    format!("{}:{ordinal}", key.native_id)
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
    state.last_assistant = Some(idx);
    idx
}

#[allow(clippy::too_many_arguments)]
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
