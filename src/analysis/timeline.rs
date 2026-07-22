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
use crate::provider::{CompactionKind, PromptAuthorship, PromptDelivery};
use crate::reconstruction::{Conversation, ConversationTurn};

use super::extraction::{
    extract_assistant_summary, extract_files_from_tools, extract_user_prompt_text, is_human_prompt,
    truncate_text,
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
    /// Human prompts delivered after the turn began, in native order.
    pub steering_prompts: Vec<String>,
    /// Assistant response summary (truncated), if present.
    pub assistant_summary: Option<String>,
    /// Deduplicated tool names used in this turn.
    pub tools_used: Vec<String>,
    /// File basenames touched in this turn.
    pub files_touched: Vec<String>,
    /// Whether any tool results had errors.
    pub had_errors: bool,
}

/// Provider-neutral presentation of a chronological compaction boundary.
#[derive(Debug, Clone, serde::Serialize)]
pub struct TimelineCompactionEvent {
    /// Boundary timestamp (RFC 3339).
    pub timestamp: String,
    /// Persisted summary, when the provider retained one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    /// Semantic compaction kind. Absent for classic entries without a
    /// semantic sidecar.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    /// Number of nested replacement-history items. These are reconstruction
    /// state, not additional chronological emissions.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub replacement_history_items: Option<usize>,
    /// Provider context-window identity and chain metadata.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub window: Option<TimelineCompactionWindow>,
}

/// Presentation form of provider context-window identity.
#[derive(Debug, Clone, serde::Serialize)]
pub struct TimelineCompactionWindow {
    /// Monotonic native window position, when recorded.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub number: Option<u64>,
    /// First window in the compaction chain.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub first_id: Option<String>,
    /// Immediately preceding window.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub previous_id: Option<String>,
    /// New/current window identity.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    /// Whether a legacy numeric `window_id` supplied `number`.
    pub legacy_numeric_id: bool,
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

/// Collect compaction boundaries with semantic window metadata when the
/// conversation retains a provider bundle.
#[must_use]
pub fn compaction_events(conversation: &Conversation) -> Vec<TimelineCompactionEvent> {
    conversation
        .main_thread_entries()
        .into_iter()
        .filter_map(|entry| {
            let LogEntry::System(system) = entry else {
                return None;
            };
            if !matches!(
                system.subtype,
                Some(
                    crate::model::SystemSubtype::CompactBoundary
                        | crate::model::SystemSubtype::MicrocompactBoundary
                )
            ) {
                return None;
            }
            let semantic = entry
                .uuid()
                .and_then(|uuid| conversation.semantics_for_uuid(uuid))
                .and_then(|semantics| semantics.compaction.as_ref());
            let kind = semantic.map(|compaction| match &compaction.kind {
                CompactionKind::Full => "full".to_string(),
                CompactionKind::Micro => "micro".to_string(),
                CompactionKind::Other(native) => native.clone(),
            });
            let window = semantic.map(|compaction| TimelineCompactionWindow {
                number: compaction.window.number,
                first_id: compaction.window.first_id.clone(),
                previous_id: compaction.window.previous_id.clone(),
                id: compaction.window.id.clone(),
                legacy_numeric_id: compaction.window.legacy_numeric_id,
            });
            Some(TimelineCompactionEvent {
                timestamp: system.timestamp.to_rfc3339(),
                summary: system.content.clone(),
                kind,
                replacement_history_items: semantic
                    .and_then(|compaction| compaction.replacement_history_items),
                window,
            })
        })
        .collect()
}

// ── Core logic ──────────────────────────────────────────────────────────────

/// A provider-semantic turn with any same-turn human steering retained.
///
/// This is intentionally separate from the long-standing public
/// [`ConversationTurn`]: adding a field to that struct would be a source-
/// breaking API change for downstream Rust callers.
#[derive(Debug, Clone)]
pub struct SemanticTurn<'a> {
    /// Human prompt that opened the turn, if one was persisted.
    pub user_message: Option<&'a LogEntry>,
    /// Human input appended while the turn was active, in native order.
    pub steering_messages: Vec<&'a LogEntry>,
    /// Latest text-bearing assistant response in the turn.
    pub assistant_message: Option<&'a LogEntry>,
    /// Tool calls made during the turn.
    pub tool_uses: Vec<&'a crate::model::content::ToolUse>,
    /// Tool results observed during the turn.
    pub tool_results: Vec<&'a crate::model::content::ToolResult>,
}

/// One canonical provider-semantic turn range over `main_thread_entries`.
/// Internal consumers share this planner so timeline and contextual zoom
/// cannot silently disagree about native turn boundaries.
#[derive(Debug, Clone)]
pub(crate) struct SemanticTurnRange {
    pub(crate) turn_id: Option<String>,
    pub(crate) start: usize,
    pub(crate) end: usize,
}

pub(crate) fn semantic_turn_ranges(conversation: &Conversation) -> Vec<SemanticTurnRange> {
    struct PendingRange {
        range: SemanticTurnRange,
        meaningful: bool,
    }

    fn flush(current: Option<PendingRange>, ranges: &mut Vec<SemanticTurnRange>) {
        if let Some(current) = current {
            if current.meaningful {
                ranges.push(current.range);
            }
        }
    }

    let entries = conversation.main_thread_entries();
    let mut ranges = Vec::new();
    let mut current: Option<PendingRange> = None;
    let mut current_turn_id: Option<String> = None;
    for (index, entry) in entries.iter().enumerate() {
        let semantics = entry
            .uuid()
            .and_then(|uuid| conversation.semantics_for_uuid(uuid));
        let entry_turn = semantics.and_then(|semantics| semantics.turn_id.clone());
        let prompt = semantics.and_then(|semantics| semantics.prompt);
        let human = matches!(entry, LogEntry::User(_))
            && prompt.is_some_and(|prompt| prompt.authorship == PromptAuthorship::Human);
        let boundary =
            human && prompt.is_some_and(|prompt| prompt.delivery == PromptDelivery::TurnBoundary);
        let turn_changed = match (&entry_turn, &current_turn_id) {
            (Some(new), Some(old)) => new != old,
            (Some(_), None) => current.is_some(),
            _ => false,
        };
        if boundary || turn_changed {
            flush(current.take(), &mut ranges);
        }
        if entry_turn.is_some() {
            current_turn_id = entry_turn;
        }

        let current = current.get_or_insert_with(|| PendingRange {
            range: SemanticTurnRange {
                turn_id: current_turn_id.clone(),
                start: index,
                end: index + 1,
            },
            meaningful: false,
        });
        current.range.end = index + 1;
        if current.range.turn_id.is_none() {
            current.range.turn_id = current_turn_id.clone();
        }
        current.meaningful |= human
            || matches!(entry, LogEntry::Assistant(assistant)
            if !assistant.message.tool_uses().is_empty()
                || assistant.message.content.iter().any(|block| {
                    matches!(block, crate::model::ContentBlock::Text(text)
                        if !text.text.trim().is_empty())
                }));
    }
    flush(current, &mut ranges);
    ranges
}

/// Group a provider conversation into turns using its semantic sidecar.
///
/// A new turn starts when the entry's `turn_id` changes or at a human
/// turn-boundary prompt. Human mid-turn input is retained inside the active
/// turn as steering, in native entry order. Harness context preceding the
/// first human prompt forms no turn unless it produced assistant activity.
#[must_use]
pub fn semantic_turns<'a>(conversation: &'a Conversation) -> Vec<SemanticTurn<'a>> {
    let entries = conversation.main_thread_entries();
    semantic_turn_ranges(conversation)
        .into_iter()
        .map(|range| {
            let mut turn = SemanticTurn {
                user_message: None,
                steering_messages: Vec::new(),
                assistant_message: None,
                tool_uses: Vec::new(),
                tool_results: Vec::new(),
            };
            for entry in &entries[range.start..range.end] {
                let semantics = entry
                    .uuid()
                    .and_then(|uuid| conversation.semantics_for_uuid(uuid));
                let prompt = semantics.and_then(|semantics| semantics.prompt);
                let human = matches!(entry, LogEntry::User(_))
                    && prompt.is_some_and(|prompt| prompt.authorship == PromptAuthorship::Human);
                match entry {
                    LogEntry::User(user) => {
                        if human {
                            if prompt
                                .is_some_and(|prompt| prompt.delivery == PromptDelivery::MidTurn)
                            {
                                turn.steering_messages.push(entry);
                            } else if turn.user_message.is_none() {
                                turn.user_message = Some(entry);
                            }
                        }
                        turn.tool_results.extend(user.message.tool_results());
                    }
                    LogEntry::Assistant(assistant) => {
                        turn.tool_uses.extend(assistant.message.tool_uses());
                        let has_text = assistant.message.content.iter().any(|block| {
                            matches!(block, crate::model::ContentBlock::Text(text)
                                if !text.text.trim().is_empty())
                        });
                        if has_text {
                            turn.assistant_message = Some(entry);
                        }
                    }
                    _ => {}
                }
            }
            turn
        })
        .collect()
}

trait TimelineTurnView<'a> {
    fn user_message(&self) -> Option<&'a LogEntry>;
    fn user_message_is_human(&self) -> bool;
    fn steering_messages(&self) -> &[&'a LogEntry];
    fn assistant_message(&self) -> Option<&'a LogEntry>;
    fn tool_uses(&self) -> &[&'a crate::model::content::ToolUse];
    fn tool_results(&self) -> &[&'a crate::model::content::ToolResult];
}

impl<'a> TimelineTurnView<'a> for ConversationTurn<'a> {
    fn user_message(&self) -> Option<&'a LogEntry> {
        self.user_message
    }

    fn user_message_is_human(&self) -> bool {
        self.user_message.is_some_and(is_human_prompt)
    }

    fn steering_messages(&self) -> &[&'a LogEntry] {
        &[]
    }

    fn assistant_message(&self) -> Option<&'a LogEntry> {
        self.assistant_message
    }

    fn tool_uses(&self) -> &[&'a crate::model::content::ToolUse] {
        &self.tool_uses
    }

    fn tool_results(&self) -> &[&'a crate::model::content::ToolResult] {
        &self.tool_results
    }
}

impl<'a> TimelineTurnView<'a> for SemanticTurn<'a> {
    fn user_message(&self) -> Option<&'a LogEntry> {
        self.user_message
    }

    fn user_message_is_human(&self) -> bool {
        self.user_message.is_some()
    }

    fn steering_messages(&self) -> &[&'a LogEntry] {
        &self.steering_messages
    }

    fn assistant_message(&self) -> Option<&'a LogEntry> {
        self.assistant_message
    }

    fn tool_uses(&self) -> &[&'a crate::model::content::ToolUse] {
        &self.tool_uses
    }

    fn tool_results(&self) -> &[&'a crate::model::content::ToolResult] {
        &self.tool_results
    }
}

/// Build a timeline from conversation turns.
///
/// Each turn is converted to a [`TimelineTurn`] with extracted text, tools,
/// and files. Consecutive tool-only turns (no user prompt, no assistant text)
/// are collapsed into single grouped entries to reduce noise.
pub fn build_timeline(turns: &[ConversationTurn<'_>], opts: &TimelineOptions) -> Vec<TimelineTurn> {
    build_timeline_from_views(turns, opts)
}

/// Build a timeline from provider-semantic turns, retaining same-turn input.
#[must_use]
pub fn build_semantic_timeline(
    turns: &[SemanticTurn<'_>],
    opts: &TimelineOptions,
) -> Vec<TimelineTurn> {
    build_timeline_from_views(turns, opts)
}

fn build_timeline_from_views<'a, T>(turns: &[T], opts: &TimelineOptions) -> Vec<TimelineTurn>
where
    T: TimelineTurnView<'a>,
{
    // Phase 1: Build raw timeline turns
    let raw_turns: Vec<TimelineTurn> = turns
        .iter()
        .enumerate()
        .map(|(i, turn)| {
            let user_prompt = turn
                .user_message()
                .filter(|_| turn.user_message_is_human())
                .and_then(extract_user_prompt_text)
                .map(|t| truncate_text(&t, opts.prompt_max_len));
            let steering_prompts = turn
                .steering_messages()
                .iter()
                .filter_map(|e| extract_user_prompt_text(e))
                .map(|t| truncate_text(&t, opts.prompt_max_len))
                .collect();

            let assistant_summary = turn
                .assistant_message()
                .and_then(|e| extract_assistant_summary(e, opts.summary_max_len));

            let mut tools_used: Vec<String> =
                turn.tool_uses().iter().map(|t| t.name.clone()).collect();
            // Deduplicate tool names while preserving order
            let mut seen: HashSet<String> = HashSet::new();
            tools_used.retain(|t| seen.insert(t.clone()));

            // Extract files from the assistant message's tool calls
            let files_touched = if let Some(entry) = turn.assistant_message() {
                let refs = vec![entry];
                extract_files_from_tools(&refs)
            } else {
                vec![]
            };

            // Check for errors in the following user message's tool results
            let had_errors = turn.tool_results().iter().any(|r| r.is_error == Some(true));

            let timestamp = turn
                .user_message()
                .or_else(|| turn.steering_messages().first().copied())
                .or(turn.assistant_message())
                .and_then(|e: &LogEntry| e.timestamp().map(|t| t.to_rfc3339()));

            TimelineTurn {
                index: i,
                timestamp,
                user_prompt,
                steering_prompts,
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
        let is_tool_only = turn.user_prompt.is_none()
            && turn.steering_prompts.is_empty()
            && turn.assistant_summary.is_none();

        if is_tool_only {
            // Collect consecutive tool-only turns
            let start = i;
            let mut all_tools = Vec::new();
            let mut all_files = Vec::new();
            let mut any_errors = false;
            let first_timestamp = turn.timestamp.clone();

            while i < raw_turns.len() {
                let t = &raw_turns[i];
                if t.user_prompt.is_some()
                    || !t.steering_prompts.is_empty()
                    || t.assistant_summary.is_some()
                {
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
                    steering_prompts: Vec::new(),
                    assistant_summary: Some(format!("[{} tool-only turns collapsed]", count)),
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
    fn test_midturn_steering_is_retained_in_native_order() {
        let boundary = user_text("start the task");
        let steering = user_text("also check the docs");
        let assistant = assistant_with_tool("t1", "Read", "done, docs checked");
        let turn = SemanticTurn {
            user_message: Some(&boundary),
            steering_messages: vec![&steering],
            assistant_message: Some(&assistant),
            tool_uses: get_tool_uses(&assistant),
            tool_results: vec![],
        };

        let timeline = build_semantic_timeline(&[turn], &TimelineOptions::default());
        assert_eq!(timeline.len(), 1);
        assert_eq!(timeline[0].user_prompt.as_deref(), Some("start the task"));
        assert_eq!(timeline[0].steering_prompts, ["also check the docs"]);
        assert_eq!(
            timeline[0].assistant_summary.as_deref(),
            Some("done, docs checked")
        );
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
