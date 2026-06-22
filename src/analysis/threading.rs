//! Cross-session topic threading.
//!
//! Searches sessions for a regex pattern and returns chronologically-ordered
//! exchanges with surrounding user/assistant conversation context.
//! Used by both CLI `thread` and MCP `thread_topic` tools.

use std::collections::HashSet;

use chrono::{DateTime, Utc};
use regex::Regex;
use serde::Serialize;

use crate::cli::helpers::{
    extract_text, extract_thinking_text, looks_like_decision, main_thread_entries, short_id,
};
use crate::discovery::Session;

/// Parameters for topic threading.
pub struct ThreadParams {
    /// Include thinking/reasoning blocks in search and output.
    pub include_thinking: bool,
    /// Maximum results to return.
    pub limit: usize,
    /// Maximum characters for user context text.
    pub max_user_context: usize,
    /// Maximum characters for assistant context text.
    pub max_assistant_context: usize,
    /// Maximum characters for thinking context text.
    pub max_thinking_context: usize,
    /// Filter to specific message role ("user" or "assistant").
    pub role_filter: Option<String>,
    /// Only include exchanges that look like decision points.
    pub decisions_only: bool,
}

impl Default for ThreadParams {
    fn default() -> Self {
        Self {
            include_thinking: false,
            limit: 30,
            max_user_context: 500,
            max_assistant_context: 500,
            max_thinking_context: 500,
            role_filter: None,
            decisions_only: false,
        }
    }
}

/// A threaded exchange: a match with surrounding conversation context.
#[derive(Debug, Serialize)]
#[allow(missing_docs)]
pub struct ThreadedExchange {
    pub timestamp: DateTime<Utc>,
    pub session_id: String,
    pub short_id: String,
    pub project: String,
    pub entry_uuid: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user_text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub assistant_text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thinking_text: Option<String>,
    pub match_location: String,
    pub match_count: usize,
}

/// Result of a threading operation.
pub struct ThreadResult {
    /// Chronologically-ordered exchanges matching the pattern.
    pub exchanges: Vec<ThreadedExchange>,
    /// Number of unique sessions with matches.
    pub session_count: usize,
    /// Total match count across all exchanges.
    pub total_matches: usize,
}

/// Run topic threading across a set of sessions.
///
/// Searches each session's main thread for the regex pattern, collects
/// matching exchanges with their surrounding user/assistant context,
/// and returns them in chronological order.
pub fn thread_topic(
    sessions: &[Session],
    regex: &Regex,
    params: &ThreadParams,
    max_file_size: Option<u64>,
) -> ThreadResult {
    let mut exchanges: Vec<ThreadedExchange> = Vec::new();

    for session in sessions {
        let entries = match session.parse_with_options(max_file_size) {
            Ok(e) => e,
            Err(_) => continue,
        };

        let main = main_thread_entries(&entries);
        let mut seen_uuids: HashSet<String> = HashSet::new();

        for (idx, entry) in main.iter().enumerate() {
            let mut match_location = String::new();

            let entry_text = extract_text(entry);
            let thinking_text = if params.include_thinking {
                extract_thinking_text(entry)
            } else {
                None
            };

            let mut match_count = 0;

            if let Some(ref text) = entry_text {
                let count = regex.find_iter(text).count();
                if count > 0 {
                    match_count += count;
                    match_location = entry.message_type().to_string();
                }
            }

            if let Some(ref text) = thinking_text {
                let count = regex.find_iter(text).count();
                if count > 0 {
                    match_count += count;
                    if match_location.is_empty() {
                        match_location = "thinking".to_string();
                    }
                }
            }

            if match_count == 0 {
                continue;
            }

            // Filter by role
            if let Some(ref role) = params.role_filter {
                if entry.message_type() != role.as_str() {
                    continue;
                }
            }

            // Filter to decision-point exchanges only
            if params.decisions_only {
                let is_decision = entry_text.as_ref().is_some_and(|t| looks_like_decision(t));
                if !is_decision {
                    let paired_assistant = if entry.message_type() == "user" {
                        ((idx + 1)..main.len())
                            .find(|&i| main[i].message_type() == "assistant")
                            .and_then(|i| extract_text(main[i]))
                    } else {
                        None
                    };
                    let paired_is_decision = paired_assistant
                        .as_ref()
                        .is_some_and(|t| looks_like_decision(t));
                    if !paired_is_decision {
                        continue;
                    }
                }
            }

            let uuid = entry.uuid().unwrap_or("").to_string();
            if !uuid.is_empty() && !seen_uuids.insert(uuid.clone()) {
                continue;
            }

            let timestamp = entry.timestamp().unwrap_or_else(Utc::now);

            let user_text = if entry.message_type() == "user" {
                extract_text(entry)
            } else {
                (0..idx)
                    .rev()
                    .find(|&i| main[i].message_type() == "user")
                    .and_then(|i| extract_text(main[i]))
            };

            let assistant_text = if entry.message_type() == "assistant" {
                extract_text(entry)
            } else {
                ((idx + 1)..main.len())
                    .find(|&i| main[i].message_type() == "assistant")
                    .and_then(|i| extract_text(main[i]))
            };

            let thinking_text = if params.include_thinking {
                if entry.message_type() == "assistant" {
                    extract_thinking_text(entry)
                } else {
                    ((idx + 1)..main.len())
                        .find(|&i| main[i].message_type() == "assistant")
                        .and_then(|i| extract_thinking_text(main[i]))
                }
            } else {
                None
            };

            exchanges.push(ThreadedExchange {
                timestamp,
                session_id: session.session_id().to_string(),
                short_id: short_id(session.session_id()).to_string(),
                project: session.project_path().to_string(),
                entry_uuid: uuid,
                user_text,
                assistant_text,
                thinking_text,
                match_location,
                match_count,
            });
        }
    }

    exchanges.sort_by_key(|a| a.timestamp);
    exchanges.truncate(params.limit);

    let session_ids: HashSet<&str> = exchanges.iter().map(|e| e.session_id.as_str()).collect();
    let total_matches: usize = exchanges.iter().map(|e| e.match_count).sum();

    ThreadResult {
        session_count: session_ids.len(),
        total_matches,
        exchanges,
    }
}
