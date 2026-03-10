//! Contextual zoom around a specific event in a conversation.
//!
//! Given a message UUID or timestamp + session entries, returns the surrounding
//! conversation context: the user prompt that triggered it, the assistant response,
//! any errors, and the resolution.
//!
//! Used by both CLI `context` and MCP `get_event_context` tools.

use chrono::{DateTime, Utc};

use crate::model::message::LogEntry;

use super::extraction::{
    extract_assistant_summary, extract_tool_names, extract_user_prompt_text,
    truncate_text,
};

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
        LogEntry::User(_) => extract_user_prompt_text(entry)
            .map(|t| truncate_text(&t, max_text_len)),
        LogEntry::Assistant(_) => extract_assistant_summary(entry, max_text_len),
        LogEntry::System(sys) => sys.content.as_ref()
            .map(|t| truncate_text(t, max_text_len)),
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
            e.uuid().map(|u| u == msg_id || u.starts_with(msg_id)).unwrap_or(false)
        })
    } else if let Some(target_ts) = params.timestamp {
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
    } else {
        return None;
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
    let end = (target_idx + 1 + window).min(entries.len());
    let after: Vec<ContextTurn> = ((target_idx + 1)..end)
        .map(|i| entry_to_turn(entries[i], i, params.max_text_len))
        .collect();

    // Collect related files and errors from the window
    let mut related_files = Vec::new();
    let mut error_count = 0usize;

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
    })
}
