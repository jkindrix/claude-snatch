//! Contextual zoom around a specific event in a conversation.
//!
//! Given a message UUID or timestamp + session entries, returns the surrounding
//! conversation context: the user prompt that triggered it, the assistant response,
//! any errors, and the resolution.
//!
//! Used by both CLI `context` and MCP `get_event_context` tools.

use chrono::{DateTime, Utc};
use serde::Serialize;
use std::collections::BTreeSet;

use crate::model::message::LogEntry;
use crate::reconstruction::Conversation;

use super::extraction::{
    extract_assistant_summary, extract_tool_names, extract_user_prompt_text, truncate_text,
};
use super::lessons::{conversation_tool_semantics, count_tool_failures};
use super::timeline::{semantic_turn_ranges, SemanticTurnRange};

/// Parameters for contextual zoom.
pub struct EventContextParams {
    /// Message UUID to find.
    pub message_id: Option<String>,
    /// Timestamp to find (closest match).
    pub timestamp: Option<DateTime<Utc>>,
    /// Number of turns before/after the target. Default: 2.
    pub context_window: usize,
    /// Max chars per text field.
    pub max_text_len: usize,
}

impl Default for EventContextParams {
    fn default() -> Self {
        Self {
            message_id: None,
            timestamp: None,
            context_window: 2,
            max_text_len: 500,
        }
    }
}

/// A single turn in the context window.
#[derive(Debug, Clone)]
#[allow(missing_docs)]
pub struct ContextTurn {
    pub index: usize,
    pub message_type: String,
    pub uuid: String,
    pub timestamp: Option<DateTime<Utc>>,
    pub text: Option<String>,
    pub tools: Vec<String>,
    pub had_errors: bool,
}

/// Result of contextual zoom.
#[derive(Debug)]
#[allow(missing_docs)]
pub struct EventContextResult {
    pub target_index: usize,
    pub target: ContextTurn,
    pub before: Vec<ContextTurn>,
    pub after: Vec<ContextTurn>,
    pub related_files: Vec<String>,
    pub error_count: usize,
    /// Provider-semantic turn window. Absent on the compatibility path, where
    /// `before` and `after` retain their historical adjacent-entry meaning.
    pub semantic_window: Option<SemanticEventWindow>,
    /// Authoritatively classified failures in a semantic window.
    pub confirmed_failure_count: Option<usize>,
    /// Text-inferred failure signals in a semantic window.
    pub inferred_failure_count: Option<usize>,
}

/// Compact context for one provider-semantic turn.
#[derive(Debug, Clone, Serialize)]
#[allow(missing_docs)]
pub struct SemanticContextTurn {
    pub index: usize,
    pub turn_id: Option<String>,
    pub start_entry_index: usize,
    pub end_entry_index: usize,
    pub event_count: usize,
    pub user_prompt: Option<String>,
    pub steering_prompts: Vec<String>,
    pub assistant_response: Option<String>,
    pub tools: Vec<String>,
    /// Kept for text rendering and root-level aggregation. JSON exposes the
    /// deduplicated root `related_files` list once rather than repeating the
    /// same paths inside every turn summary.
    #[serde(skip)]
    pub related_files: Vec<String>,
    pub confirmed_failure_count: usize,
    pub inferred_failure_count: usize,
}

/// The target turn plus a bounded number of semantic turns on either side.
#[derive(Debug, Clone, Serialize)]
#[allow(missing_docs)]
pub struct SemanticEventWindow {
    pub before: Vec<SemanticContextTurn>,
    pub focus: SemanticContextTurn,
    pub after: Vec<SemanticContextTurn>,
}

/// Check if a single entry has tool errors.
fn entry_has_errors(entry: &LogEntry) -> bool {
    if let LogEntry::User(user) = entry {
        for result in user.message.tool_results() {
            if result.is_error == Some(true) {
                return true;
            }
        }
    }
    false
}

/// Build a ContextTurn from a LogEntry.
fn entry_to_turn(entry: &LogEntry, index: usize, max_text_len: usize) -> ContextTurn {
    let message_type = entry.message_type().to_string();
    let uuid = entry.uuid().unwrap_or("").to_string();
    let timestamp = entry.timestamp();

    let text = match entry {
        LogEntry::User(_) => {
            extract_user_prompt_text(entry).map(|t| truncate_text(&t, max_text_len))
        }
        LogEntry::Assistant(_) => extract_assistant_summary(entry, max_text_len),
        LogEntry::System(sys) => sys.content.as_ref().map(|t| truncate_text(t, max_text_len)),
        _ => None,
    };

    let tools = extract_tool_names(entry);
    let had_errors = entry_has_errors(entry);

    ContextTurn {
        index,
        message_type,
        uuid,
        timestamp,
        text,
        tools,
        had_errors,
    }
}

/// Find event context within a list of entries.
pub fn find_event_context(
    entries: &[&LogEntry],
    params: &EventContextParams,
) -> Option<EventContextResult> {
    // Find the target entry index
    let target_idx = if let Some(ref msg_id) = params.message_id {
        entries.iter().position(|e| {
            e.uuid()
                .map(|u| u == msg_id || u.starts_with(msg_id))
                .unwrap_or(false)
        })
    } else {
        let target_ts = params.timestamp?;
        // Find closest entry by timestamp
        let mut best_idx = None;
        let mut best_diff = i64::MAX;
        for (i, entry) in entries.iter().enumerate() {
            if let Some(ts) = entry.timestamp() {
                let diff = (ts - target_ts).num_milliseconds().abs();
                if diff < best_diff {
                    best_diff = diff;
                    best_idx = Some(i);
                }
            }
        }
        best_idx
    };

    let target_idx = target_idx?;
    let window = params.context_window;

    let target = entry_to_turn(entries[target_idx], target_idx, params.max_text_len);

    // Collect before window
    let start = target_idx.saturating_sub(window);
    let before: Vec<ContextTurn> = (start..target_idx)
        .map(|i| entry_to_turn(entries[i], i, params.max_text_len))
        .collect();

    // Collect after window
    let end = target_idx
        .saturating_add(1)
        .saturating_add(window)
        .min(entries.len());
    let after: Vec<ContextTurn> = ((target_idx + 1)..end)
        .map(|i| entry_to_turn(entries[i], i, params.max_text_len))
        .collect();

    // Collect related files and errors from the window
    let mut related_files = Vec::new();
    let mut error_count = 0usize;

    #[allow(clippy::needless_range_loop)]
    for i in start..end {
        if let LogEntry::Assistant(a) = entries[i] {
            for tool in a.message.tool_uses() {
                if let Some(path) = tool.input.get("file_path").and_then(|v| v.as_str()) {
                    if !related_files.contains(&path.to_string()) {
                        related_files.push(path.to_string());
                    }
                }
            }
        }
        if entry_has_errors(entries[i]) {
            error_count += 1;
        }
    }

    Some(EventContextResult {
        target_index: target_idx,
        target,
        before,
        after,
        related_files,
        error_count,
        semantic_window: None,
        confirmed_failure_count: None,
        inferred_failure_count: None,
    })
}

fn target_index(entries: &[&LogEntry], params: &EventContextParams) -> Option<usize> {
    if let Some(ref msg_id) = params.message_id {
        entries.iter().position(|entry| {
            entry
                .uuid()
                .is_some_and(|uuid| uuid == msg_id || uuid.starts_with(msg_id))
        })
    } else {
        let target_ts = params.timestamp?;
        entries
            .iter()
            .enumerate()
            .filter_map(|(index, entry)| {
                entry.timestamp().map(|timestamp| {
                    (
                        index,
                        (timestamp - target_ts).num_milliseconds().unsigned_abs(),
                    )
                })
            })
            .min_by_key(|(index, difference)| (*difference, *index))
            .map(|(index, _)| index)
    }
}

fn paths_from_legacy_tools(entries: &[&LogEntry]) -> BTreeSet<String> {
    let mut paths = BTreeSet::new();
    for entry in entries {
        let LogEntry::Assistant(assistant) = entry else {
            continue;
        };
        for tool in assistant.message.tool_uses() {
            for field in ["file_path", "path"] {
                if let Some(path) = tool.input.get(field).and_then(serde_json::Value::as_str) {
                    paths.insert(path.to_string());
                }
            }
        }
    }
    paths
}

fn summarize_semantic_group(
    conversation: &Conversation,
    entries: &[&LogEntry],
    group: &SemanticTurnRange,
    group_index: usize,
    max_text_len: usize,
) -> SemanticContextTurn {
    let scoped = &entries[group.start..group.end];
    let mut user_prompt = None;
    let mut steering_prompts = Vec::new();
    let mut assistant_response = None;
    let mut tools = Vec::new();
    let mut related_files = paths_from_legacy_tools(scoped);
    let mut entry_ids = BTreeSet::new();

    for entry in scoped {
        let semantics = entry
            .uuid()
            .and_then(|uuid| conversation.semantics_for_uuid(uuid));
        if let Some(uuid) = entry.uuid() {
            if let Some(id) = conversation.entry_id_for_uuid(uuid) {
                entry_ids.insert(id.clone());
            }
        }
        if let Some(prompt) = semantics.and_then(|semantics| semantics.prompt) {
            if prompt.authorship == crate::provider::PromptAuthorship::Human {
                if let Some(text) = extract_user_prompt_text(entry) {
                    let text = truncate_text(text.trim(), max_text_len);
                    match prompt.delivery {
                        crate::provider::PromptDelivery::TurnBoundary => {
                            if user_prompt.is_none() {
                                user_prompt = Some(text);
                            }
                        }
                        crate::provider::PromptDelivery::MidTurn => steering_prompts.push(text),
                    }
                }
            }
        }
        if matches!(entry, LogEntry::Assistant(_)) {
            if let Some(summary) = extract_assistant_summary(entry, max_text_len) {
                assistant_response = Some(summary);
            }
        }
        for name in extract_tool_names(entry) {
            if !tools.contains(&name) {
                tools.push(name);
            }
        }
    }

    if let Some(bundle) = conversation.provider_bundle() {
        for change in &bundle.file_changes {
            if entry_ids.contains(&change.owner) {
                related_files.insert(change.path.clone());
                if let Some(path) = &change.move_path {
                    related_files.insert(path.clone());
                }
            }
        }
    }

    let failures = count_tool_failures(scoped, &conversation_tool_semantics(conversation), true);
    SemanticContextTurn {
        index: group_index,
        turn_id: group.turn_id.clone(),
        start_entry_index: group.start,
        end_entry_index: group.end.saturating_sub(1),
        event_count: group.end.saturating_sub(group.start),
        user_prompt,
        steering_prompts,
        assistant_response,
        tools,
        related_files: related_files.into_iter().collect(),
        confirmed_failure_count: failures.confirmed,
        inferred_failure_count: failures.inferred,
    }
}

/// Find context using provider turn semantics.
///
/// The exact event remains the target; surrounding context is compacted to one
/// summary per native turn so tool-heavy turns stay bounded.
#[must_use]
pub fn find_semantic_event_context(
    conversation: &Conversation,
    params: &EventContextParams,
) -> Option<EventContextResult> {
    let entries = conversation.main_thread_entries();
    let target_idx = target_index(&entries, params)?;
    let mut groups = semantic_turn_ranges(conversation);
    let target_group = groups
        .iter()
        .position(|group| group.start <= target_idx && target_idx < group.end)
        .unwrap_or_else(|| {
            let insert_at = groups
                .iter()
                .position(|group| group.start > target_idx)
                .unwrap_or(groups.len());
            let turn_id = entries[target_idx]
                .uuid()
                .and_then(|uuid| conversation.semantics_for_uuid(uuid))
                .and_then(|semantics| semantics.turn_id.clone());
            groups.insert(
                insert_at,
                SemanticTurnRange {
                    turn_id,
                    start: target_idx,
                    end: target_idx.saturating_add(1),
                },
            );
            insert_at
        });
    let start = target_group.saturating_sub(params.context_window);
    let end = target_group
        .saturating_add(1)
        .saturating_add(params.context_window)
        .min(groups.len());
    let summarize = |index: usize| {
        summarize_semantic_group(
            conversation,
            &entries,
            &groups[index],
            index,
            params.max_text_len,
        )
    };
    let before: Vec<_> = (start..target_group).map(summarize).collect();
    let focus = summarize(target_group);
    let after: Vec<_> = ((target_group + 1)..end).map(summarize).collect();
    let mut related_files = BTreeSet::new();
    let mut confirmed = 0usize;
    let mut inferred = 0usize;
    for turn in before.iter().chain(std::iter::once(&focus)).chain(&after) {
        related_files.extend(turn.related_files.iter().cloned());
        confirmed = confirmed.saturating_add(turn.confirmed_failure_count);
        inferred = inferred.saturating_add(turn.inferred_failure_count);
    }

    Some(EventContextResult {
        target_index: target_idx,
        target: entry_to_turn(entries[target_idx], target_idx, params.max_text_len),
        before: Vec::new(),
        after: Vec::new(),
        related_files: related_files.into_iter().collect(),
        error_count: confirmed.saturating_add(inferred),
        semantic_window: Some(SemanticEventWindow {
            before,
            focus,
            after,
        }),
        confirmed_failure_count: Some(confirmed),
        inferred_failure_count: Some(inferred),
    })
}
