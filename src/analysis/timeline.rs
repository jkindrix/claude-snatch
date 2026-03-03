//! Timeline building: turn-by-turn narrative with tool-only collapse.
//!
//! Builds a structured timeline from [`ConversationTurn`] slices,
//! collapsing consecutive tool-only turns to reduce noise.
//!
//! # Usage
//!
//! ```rust,no_run
//! use claude_snatch::analysis::timeline::{build_timeline, TimelineOptions};
//! use claude_snatch::reconstruction::Conversation;
//!
//! # fn example(conversation: &Conversation) {
//! let turns = conversation.turns();
//! let opts = TimelineOptions::default();
//! let timeline = build_timeline(&turns, &opts);
//! println!("Timeline has {} entries", timeline.len());
//! # }
//! ```

use std::collections::HashSet;

use crate::model::message::LogEntry;
use crate::reconstruction::ConversationTurn;

use super::extraction::{
    extract_assistant_summary, extract_files_from_tools, extract_user_prompt_text, truncate_text,
};

// ── Result types ────────────────────────────────────────────────────────────

/// A single entry in the session timeline.
#[derive(Debug, Clone)]
pub struct TimelineTurn {
    /// Index of this turn (0-based, from original turn list).
    pub index: usize,
    /// Timestamp of the turn (RFC 3339).
    pub timestamp: Option<String>,
    /// User prompt text (truncated), if present.
    pub user_prompt: Option<String>,
    /// Assistant response summary (truncated), if present.
    pub assistant_summary: Option<String>,
    /// Deduplicated tool names used in this turn.
    pub tools_used: Vec<String>,
    /// File basenames touched in this turn.
    pub files_touched: Vec<String>,
    /// Whether any tool results had errors.
    pub had_errors: bool,
}

// ── Options ─────────────────────────────────────────────────────────────────

/// Options controlling timeline construction.
#[derive(Debug, Clone)]
pub struct TimelineOptions {
    /// Maximum timeline entries to return.
    pub limit: usize,
    /// Max chars for user prompt text.
    pub prompt_max_len: usize,
    /// Max chars for assistant summary text.
    pub summary_max_len: usize,
}

impl Default for TimelineOptions {
    fn default() -> Self {
        Self {
            limit: 30,
            prompt_max_len: 200,
            summary_max_len: 200,
        }
    }
}

// ── Core logic ──────────────────────────────────────────────────────────────

/// Build a timeline from conversation turns.
///
/// Each turn is converted to a [`TimelineTurn`] with extracted text, tools,
/// and files. Consecutive tool-only turns (no user prompt, no assistant text)
/// are collapsed into single grouped entries to reduce noise.
pub fn build_timeline(turns: &[ConversationTurn<'_>], opts: &TimelineOptions) -> Vec<TimelineTurn> {
    // Phase 1: Build raw timeline turns
    let raw_turns: Vec<TimelineTurn> = turns
        .iter()
        .enumerate()
        .map(|(i, turn)| {
            let user_prompt = turn.user_message.and_then(|e| {
                extract_user_prompt_text(e).map(|t| truncate_text(&t, opts.prompt_max_len))
            });

            let assistant_summary = turn
                .assistant_message
                .and_then(|e| extract_assistant_summary(e, opts.summary_max_len));

            let mut tools_used: Vec<String> = turn
                .tool_uses
                .iter()
                .map(|t| t.name.clone())
                .collect();
            // Deduplicate tool names while preserving order
            let mut seen: HashSet<String> = HashSet::new();
            tools_used.retain(|t| seen.insert(t.clone()));

            // Extract files from the assistant message's tool calls
            let files_touched = if let Some(entry) = turn.assistant_message {
                let refs = vec![entry];
                extract_files_from_tools(&refs)
            } else {
                vec![]
            };

            // Check for errors in the following user message's tool results
            let had_errors = turn.tool_results.iter().any(|r| r.is_error == Some(true));

            let timestamp = turn
                .user_message
                .or(turn.assistant_message)
                .and_then(|e: &LogEntry| e.timestamp().map(|t| t.to_rfc3339()));

            TimelineTurn {
                index: i,
                timestamp,
                user_prompt,
                assistant_summary,
                tools_used,
                files_touched,
                had_errors,
            }
        })
        .collect();

    // Phase 2: Collapse consecutive tool-only turns
    let mut timeline: Vec<TimelineTurn> = Vec::new();
    let mut i = 0;

    while i < raw_turns.len() {
        let turn = &raw_turns[i];
        let is_tool_only = turn.user_prompt.is_none() && turn.assistant_summary.is_none();

        if is_tool_only {
            // Collect consecutive tool-only turns
            let start = i;
            let mut all_tools = Vec::new();
            let mut all_files = Vec::new();
            let mut any_errors = false;
            let first_timestamp = turn.timestamp.clone();

            while i < raw_turns.len() {
                let t = &raw_turns[i];
                if t.user_prompt.is_some() || t.assistant_summary.is_some() {
                    break;
                }
                all_tools.extend(t.tools_used.iter().cloned());
                all_files.extend(t.files_touched.iter().cloned());
                any_errors = any_errors || t.had_errors;
                i += 1;
            }

            let count = i - start;
            // Deduplicate tools and files
            let mut seen: HashSet<String> = HashSet::new();
            all_tools.retain(|t| seen.insert(t.clone()));
            let mut seen: HashSet<String> = HashSet::new();
            all_files.retain(|f| seen.insert(f.clone()));

            if count > 1 {
                // Collapse into single entry
                timeline.push(TimelineTurn {
                    index: start,
                    timestamp: first_timestamp,
                    user_prompt: None,
                    assistant_summary: Some(format!(
                        "[{} tool-only turns collapsed]",
                        count
                    )),
                    tools_used: all_tools,
                    files_touched: all_files,
                    had_errors: any_errors,
                });
            } else {
                // Single tool-only turn, keep as-is
                timeline.push(raw_turns[start].clone());
            }
        } else {
            timeline.push(raw_turns[i].clone());
            i += 1;
        }
    }

    // Apply limit after collapsing
    timeline.truncate(opts.limit);
    timeline
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::message::LogEntry;

    /// Helper: build a User text entry.
    fn user_text(text: &str) -> LogEntry {
        let json = format!(
            r#"{{
                "type": "user",
                "uuid": "user-1",
                "timestamp": "2025-01-01T00:00:00Z",
                "sessionId": "test-session",
                "version": "2.0.0",
                "message": {{
                    "role": "user",
                    "content": "{text}"
                }}
            }}"#
        );
        serde_json::from_str(&json).expect("failed to parse user_text JSON")
    }

    /// Helper: build an Assistant entry with a tool use.
    fn assistant_with_tool(tool_id: &str, tool_name: &str, text: &str) -> LogEntry {
        let json = format!(
            r#"{{
                "type": "assistant",
                "uuid": "asst-{tool_id}",
                "timestamp": "2025-01-01T00:00:01Z",
                "sessionId": "test-session",
                "version": "2.0.0",
                "message": {{
                    "id": "msg-test",
                    "type": "message",
                    "role": "assistant",
                    "model": "claude-sonnet-4-20250514",
                    "content": [
                        {{
                            "type": "tool_use",
                            "id": "{tool_id}",
                            "name": "{tool_name}",
                            "input": {{"command": "cargo test"}}
                        }},
                        {{
                            "type": "text",
                            "text": "{text}"
                        }}
                    ],
                    "stop_reason": "end_turn"
                }}
            }}"#
        );
        serde_json::from_str(&json).expect("failed to parse assistant_with_tool JSON")
    }

    /// Helper: build an Assistant entry with only a tool use (no text).
    fn assistant_tool_only(tool_id: &str, tool_name: &str) -> LogEntry {
        let json = format!(
            r#"{{
                "type": "assistant",
                "uuid": "asst-{tool_id}",
                "timestamp": "2025-01-01T00:00:01Z",
                "sessionId": "test-session",
                "version": "2.0.0",
                "message": {{
                    "id": "msg-test",
                    "type": "message",
                    "role": "assistant",
                    "model": "claude-sonnet-4-20250514",
                    "content": [
                        {{
                            "type": "tool_use",
                            "id": "{tool_id}",
                            "name": "{tool_name}",
                            "input": {{"command": "ls"}}
                        }}
                    ],
                    "stop_reason": "end_turn"
                }}
            }}"#
        );
        serde_json::from_str(&json).expect("failed to parse assistant_tool_only JSON")
    }

    /// Helper: build a tool result User entry.
    fn user_tool_result(tool_use_id: &str, is_error: bool) -> LogEntry {
        let is_error_str = if is_error { "true" } else { "false" };
        let json = format!(
            r#"{{
                "type": "user",
                "uuid": "user-tr-{tool_use_id}",
                "timestamp": "2025-01-01T00:00:02Z",
                "sessionId": "test-session",
                "version": "2.0.0",
                "message": {{
                    "role": "user",
                    "content": [
                        {{
                            "type": "tool_result",
                            "tool_use_id": "{tool_use_id}",
                            "content": "result text",
                            "is_error": {is_error_str}
                        }}
                    ]
                }}
            }}"#
        );
        serde_json::from_str(&json).expect("failed to parse user_tool_result JSON")
    }

    /// Helper: extract tool_uses from a LogEntry (must be Assistant).
    fn get_tool_uses(entry: &LogEntry) -> Vec<&crate::model::content::ToolUse> {
        if let LogEntry::Assistant(a) = entry {
            a.message.tool_uses()
        } else {
            vec![]
        }
    }

    /// Helper: extract tool_results from a LogEntry (must be User).
    fn get_tool_results(entry: &LogEntry) -> Vec<&crate::model::content::ToolResult> {
        if let LogEntry::User(u) = entry {
            u.message.tool_results()
        } else {
            vec![]
        }
    }

    #[test]
    fn test_build_basic_timeline() {
        let u = user_text("Hello");
        let a = assistant_with_tool("t1", "Bash", "Running command");
        let tr = user_tool_result("t1", false);

        let turn = ConversationTurn {
            user_message: Some(&u),
            assistant_message: Some(&a),
            tool_uses: get_tool_uses(&a),
            tool_results: get_tool_results(&tr),
        };

        let timeline = build_timeline(&[turn], &TimelineOptions::default());
        assert_eq!(timeline.len(), 1);
        assert!(timeline[0].user_prompt.is_some());
        assert!(timeline[0].assistant_summary.is_some());
        assert_eq!(timeline[0].tools_used, vec!["Bash"]);
        assert!(!timeline[0].had_errors);
    }

    #[test]
    fn test_collapse_tool_only_turns() {
        let a1 = assistant_tool_only("t1", "Read");
        let tr1 = user_tool_result("t1", false);
        let a2 = assistant_tool_only("t2", "Grep");
        let tr2 = user_tool_result("t2", false);
        let a3 = assistant_tool_only("t3", "Read");
        let tr3 = user_tool_result("t3", false);

        let turns = vec![
            ConversationTurn {
                user_message: None,
                assistant_message: Some(&a1),
                tool_uses: get_tool_uses(&a1),
                tool_results: get_tool_results(&tr1),
            },
            ConversationTurn {
                user_message: None,
                assistant_message: Some(&a2),
                tool_uses: get_tool_uses(&a2),
                tool_results: get_tool_results(&tr2),
            },
            ConversationTurn {
                user_message: None,
                assistant_message: Some(&a3),
                tool_uses: get_tool_uses(&a3),
                tool_results: get_tool_results(&tr3),
            },
        ];

        let timeline = build_timeline(&turns, &TimelineOptions::default());
        assert_eq!(timeline.len(), 1, "3 tool-only turns should collapse to 1");
        assert!(timeline[0]
            .assistant_summary
            .as_ref()
            .unwrap()
            .contains("3 tool-only turns collapsed"));
        assert!(timeline[0].tools_used.contains(&"Read".to_string()));
        assert!(timeline[0].tools_used.contains(&"Grep".to_string()));
    }

    #[test]
    fn test_error_detection() {
        let u = user_text("test");
        let a = assistant_with_tool("t1", "Bash", "testing");
        let tr = user_tool_result("t1", true);

        let turn = ConversationTurn {
            user_message: Some(&u),
            assistant_message: Some(&a),
            tool_uses: get_tool_uses(&a),
            tool_results: get_tool_results(&tr),
        };

        let timeline = build_timeline(&[turn], &TimelineOptions::default());
        assert!(timeline[0].had_errors);
    }

    #[test]
    fn test_limit_respected() {
        let u = user_text("msg");
        let a = assistant_with_tool("t1", "Bash", "reply");
        let tr = user_tool_result("t1", false);

        let turn = ConversationTurn {
            user_message: Some(&u),
            assistant_message: Some(&a),
            tool_uses: get_tool_uses(&a),
            tool_results: get_tool_results(&tr),
        };

        // Create 5 identical turns
        let turns: Vec<_> = (0..5).map(|_| turn.clone()).collect();
        let opts = TimelineOptions {
            limit: 3,
            ..Default::default()
        };

        let timeline = build_timeline(&turns, &opts);
        assert_eq!(timeline.len(), 3);
    }
}
