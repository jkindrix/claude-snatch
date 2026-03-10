//! File evolution analysis — explains why a file changed over time.
//!
//! Chains file modification history with conversation context and thinking
//! blocks to produce a chronological narrative of changes to a file.
//!
//! Used by both CLI `file-evolution` and MCP `explain_file_evolution` tools.

use chrono::{DateTime, Utc};

use crate::analysis::extraction::{
    extract_assistant_summary, extract_thinking_text, extract_tool_names,
    extract_user_prompt_text, truncate_text,
};
use crate::discovery::Session;
use crate::file_index::FileIndex;
use crate::model::message::LogEntry;

/// Parameters for file evolution analysis.
pub struct FileEvolutionParams {
    /// File path pattern (substring match).
    pub file_pattern: String,
    /// Max changes to return.
    pub limit: usize,
    /// Max chars for text fields.
    pub max_text_len: usize,
    /// Include thinking blocks in output.
    pub include_thinking: bool,
    /// Context window (turns before/after the modification).
    pub context_window: usize,
}

impl Default for FileEvolutionParams {
    fn default() -> Self {
        Self {
            file_pattern: String::new(),
            limit: 50,
            max_text_len: 500,
            include_thinking: true,
            context_window: 1,
        }
    }
}

/// A single change event in the file's evolution.
#[derive(Debug, Clone)]
#[allow(missing_docs)]
pub struct ChangeEvent {
    pub timestamp: DateTime<Utc>,
    pub session_id: String,
    pub message_id: String,
    pub version: u32,
    pub user_prompt: Option<String>,
    pub assistant_response: Option<String>,
    pub thinking: Option<String>,
    pub tools_used: Vec<String>,
    pub had_errors: bool,
}

/// Complete file evolution result.
#[derive(Debug)]
#[allow(missing_docs)]
pub struct FileEvolutionResult {
    pub file_path: String,
    pub total_changes: usize,
    pub sessions_involved: usize,
    pub changes: Vec<ChangeEvent>,
}

/// Analyze the evolution of a file across sessions.
pub fn analyze_file_evolution(
    sessions: &[Session],
    params: &FileEvolutionParams,
    max_file_size: Option<u64>,
) -> Vec<FileEvolutionResult> {
    let file_index = FileIndex::from_sessions(sessions, max_file_size);

    // Find matching files
    let matches = file_index.search(&params.file_pattern);

    if matches.is_empty() {
        return Vec::new();
    }

    // Build a session lookup for quick access
    let session_map: std::collections::HashMap<&str, &Session> = sessions
        .iter()
        .map(|s| (s.session_id(), s))
        .collect();

    let mut results = Vec::new();

    for (file_path, modifications) in matches {
        let total_changes = modifications.len();
        let mut unique_sessions: Vec<&str> = modifications.iter()
            .map(|m| m.session_id.as_str())
            .collect();
        unique_sessions.sort();
        unique_sessions.dedup();
        let sessions_involved = unique_sessions.len();

        let mut changes = Vec::new();

        for modification in modifications.iter().take(params.limit) {
            let session = match session_map.get(modification.session_id.as_str()) {
                Some(s) => s,
                None => continue,
            };

            let entries = match session.parse_with_options(max_file_size) {
                Ok(e) => e,
                Err(_) => continue,
            };

            // Find the entry matching the message_id
            let target_idx = entries.iter().position(|e| {
                e.uuid()
                    .map(|u| u == modification.message_id || u.starts_with(&modification.message_id))
                    .unwrap_or(false)
            });

            let change = if let Some(idx) = target_idx {
                extract_change_context(&entries, idx, modification, params)
            } else {
                // Message not found — still record the change with minimal info
                ChangeEvent {
                    timestamp: modification.timestamp,
                    session_id: modification.session_id.clone(),
                    message_id: modification.message_id.clone(),
                    version: modification.version,
                    user_prompt: None,
                    assistant_response: None,
                    thinking: None,
                    tools_used: Vec::new(),
                    had_errors: false,
                }
            };

            changes.push(change);
        }

        results.push(FileEvolutionResult {
            file_path: file_path.to_string(),
            total_changes,
            sessions_involved,
            changes,
        });
    }

    results
}

/// Extract conversation context around a file modification.
fn extract_change_context(
    entries: &[LogEntry],
    target_idx: usize,
    modification: &crate::file_index::FileModification,
    params: &FileEvolutionParams,
) -> ChangeEvent {
    let window = params.context_window;
    let start = target_idx.saturating_sub(window);
    let end = (target_idx + 1 + window).min(entries.len());

    let mut user_prompt = None;
    let mut assistant_response = None;
    let mut thinking = None;
    let mut tools_used = Vec::new();
    let mut had_errors = false;

    // Scan the window for context
    for i in start..end {
        let entry = &entries[i];

        match entry {
            LogEntry::User(_) => {
                if user_prompt.is_none() {
                    user_prompt = extract_user_prompt_text(entry)
                        .map(|t| truncate_text(&t, params.max_text_len));
                }
                // Check for tool errors in user messages (tool results)
                if let LogEntry::User(u) = entry {
                    for result in u.message.tool_results() {
                        if result.is_error == Some(true) {
                            had_errors = true;
                        }
                    }
                }
            }
            LogEntry::Assistant(_) => {
                if assistant_response.is_none() {
                    assistant_response = extract_assistant_summary(entry, params.max_text_len);
                }
                if params.include_thinking && thinking.is_none() {
                    thinking = extract_thinking_text(entry, params.max_text_len);
                }
                let names = extract_tool_names(entry);
                for name in names {
                    if !tools_used.contains(&name) {
                        tools_used.push(name);
                    }
                }
            }
            _ => {}
        }
    }

    ChangeEvent {
        timestamp: modification.timestamp,
        session_id: modification.session_id.clone(),
        message_id: modification.message_id.clone(),
        version: modification.version,
        user_prompt,
        assistant_response,
        thinking,
        tools_used,
        had_errors,
    }
}
