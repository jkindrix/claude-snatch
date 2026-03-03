//! Session lesson extraction: error→fix pairs and user corrections.
//!
//! Targets the most expensive compaction failure modes:
//! - **F2 (Negative result amnesia)**: Recovers what failed and how it was fixed
//! - **F4 (Operational gotcha amnesia)**: Recovers user corrections of agent behavior
//!
//! # Usage
//!
//! ```rust,no_run
//! use claude_snatch::analysis::lessons::{extract_lessons, LessonOptions};
//! use claude_snatch::reconstruction::Conversation;
//!
//! # fn example(conversation: &Conversation) {
//! let entries = conversation.chronological_entries();
//! let options = LessonOptions::default();
//! let result = extract_lessons(&entries, &options);
//! println!("Found {} errors, {} corrections",
//!     result.error_fix_pairs.len(),
//!     result.user_corrections.len());
//! # }
//! ```

use std::collections::HashMap;

use crate::model::content::ToolResultContent;
use crate::model::message::LogEntry;

use super::extraction::{
    extract_assistant_summary, extract_tool_input_summary, extract_user_prompt_text, truncate_text,
};

/// Extract plain text from a ToolResultContent value.
fn tool_result_text(content: &ToolResultContent) -> String {
    match content {
        ToolResultContent::String(s) => s.clone(),
        ToolResultContent::Array(arr) => {
            // Extract text from array elements (typically {"type":"text","text":"..."})
            arr.iter()
                .filter_map(|v| v.get("text").and_then(|t| t.as_str()))
                .collect::<Vec<_>>()
                .join("\n")
        }
    }
}

// ── Result types ────────────────────────────────────────────────────────────

/// An error→fix pair: a tool call that failed, and what happened next.
#[derive(Debug, Clone)]
pub struct ErrorFixPair {
    /// When the error occurred (RFC 3339).
    pub timestamp: Option<String>,
    /// The tool that errored.
    pub tool_name: String,
    /// Key input fields for the failing call.
    pub input_summary: HashMap<String, String>,
    /// Preview of the error message.
    pub error_preview: String,
    /// What the assistant did next (text summary of next response).
    pub resolution_summary: Option<String>,
    /// Tools used in the resolution attempt.
    pub resolution_tools: Vec<String>,
}

/// A user correction: where the user corrected the agent's behavior.
#[derive(Debug, Clone)]
pub struct UserCorrectionEntry {
    /// When the correction was made (RFC 3339).
    pub timestamp: Option<String>,
    /// The user's correction text.
    pub user_text: String,
    /// What the assistant was doing before (summary of previous response).
    pub prior_assistant_summary: Option<String>,
}

/// Summary statistics for extracted lessons.
#[derive(Debug, Clone)]
pub struct LessonsSummary {
    /// Total error→fix pairs found.
    pub total_errors: usize,
    /// Total user corrections found.
    pub total_corrections: usize,
    /// Tools ranked by error frequency (most error-prone first).
    pub most_error_prone_tools: Vec<(String, usize)>,
}

/// Complete lesson extraction result.
#[derive(Debug, Clone)]
pub struct LessonResult {
    /// Error→fix pairs found in the session.
    pub error_fix_pairs: Vec<ErrorFixPair>,
    /// User corrections found in the session.
    pub user_corrections: Vec<UserCorrectionEntry>,
    /// Summary statistics.
    pub summary: LessonsSummary,
}

// ── Options ─────────────────────────────────────────────────────────────────

/// Which categories of lessons to extract.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LessonCategory {
    /// Only error→fix pairs.
    Errors,
    /// Only user corrections.
    Corrections,
    /// Both.
    All,
}

impl LessonCategory {
    /// Parse from string (e.g., "errors", "corrections", "all").
    pub fn from_str_loose(s: &str) -> Self {
        match s.trim().to_lowercase().as_str() {
            "errors" => Self::Errors,
            "corrections" => Self::Corrections,
            _ => Self::All,
        }
    }
}

/// Options controlling lesson extraction behavior.
#[derive(Debug, Clone)]
pub struct LessonOptions {
    /// Which categories to extract.
    pub category: LessonCategory,
    /// Maximum lessons per category.
    pub limit: usize,
    /// Max chars for error preview text.
    pub error_preview_len: usize,
    /// Max chars for resolution summary text.
    pub resolution_summary_len: usize,
    /// Max chars for user correction text.
    pub correction_text_len: usize,
}

impl Default for LessonOptions {
    fn default() -> Self {
        Self {
            category: LessonCategory::All,
            limit: 30,
            error_preview_len: 300,
            resolution_summary_len: 200,
            correction_text_len: 300,
        }
    }
}

// ── Core extraction ─────────────────────────────────────────────────────────

/// Check if text starts with a line-number prefix (e.g., "     1→", "787→").
/// This is the format used by Read tool output.
fn starts_with_line_number(s: &str) -> bool {
    let trimmed = s.trim_start();
    let digit_end = trimmed
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(0);
    digit_end > 0 && trimmed[digit_end..].starts_with('→')
}

/// Check if text starts with grep-style line output (e.g., "21:", "21-").
fn starts_with_grep_line(s: &str) -> bool {
    let trimmed = s.trim_start();
    let digit_end = trimmed
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(0);
    digit_end > 0 && {
        let rest = &trimmed[digit_end..];
        rest.starts_with(':') || rest.starts_with('-')
    }
}

/// Heuristic: detect tool result content that looks like a successful result
/// despite having `is_error=true`. This filters false positives from Claude Code's
/// JSONL logging where certain tool results are spuriously flagged.
fn is_likely_false_positive(tool_name: &str, content: &str) -> bool {
    match tool_name {
        // Read results starting with line-numbered content (N→) are successful file reads
        // Handles both "1→" (start of file) and "787→" (offset reads)
        "Read" => starts_with_line_number(content),
        // Grep results starting with line-numbered output are successful searches
        "Grep" => starts_with_grep_line(content),
        // MCP tool results returning valid JSON objects/arrays are successful calls
        name if name.starts_with("mcp__") => {
            let trimmed = content.trim_start();
            trimmed.starts_with('{') || trimmed.starts_with('[')
        }
        // Agent results with substantial text that don't start with error markers
        "Agent" => content.len() > 200 && !content.starts_with("Error"),
        _ => false,
    }
}

/// Soft error pattern: detect errors in tool result content even when
/// `is_error` is not set (e.g., SIGSEGV, panics, assertion failures).
fn build_soft_error_regex() -> Option<regex::Regex> {
    regex::RegexBuilder::new(
        r"(?:Segmentation fault|SIGSEGV|SIGABRT|panic|stack overflow|assertion failed|fatal error|thread .* panicked|Exit code (?:[1-9]\d*|1\d\d)|error\[E\d+\]|cannot find|unresolved|undefined reference)"
    )
    .case_insensitive(true)
    .build()
    .ok()
}

/// User correction pattern: detect frustration, behavioral correction,
/// or explicit instructions to change approach.
fn build_correction_regex() -> Option<regex::Regex> {
    regex::RegexBuilder::new(
        r"(?:don'?t|(?:^|\W)stop\b|wrong|no[,.\!]|incorrect|that'?s not|instead|should have|why did you|why are you|already|again|what the (?:hell|fuck)|are you ever|sick of|wasting time|same (?:thing|fucking)|over and over|keep (?:doing|searching|looking|trying)|you can'?t|how many times)"
    )
    .case_insensitive(true)
    .build()
    .ok()
}

/// Check if a user message is a compaction/continuation summary (false positive filter).
fn is_summary_text(text: &str) -> bool {
    text.len() > 3000
        || text.contains("All User Messages:")
        || text.contains("conversation that ran out of context")
        || text.contains("Key Technical Concepts:")
}

/// Extract error→fix pairs from a chronological entry list.
///
/// Detects both hard errors (`is_error=true`) and soft errors (error patterns
/// in tool result content like SIGSEGV, panics, compiler errors).
/// For each error, captures the next assistant response as the resolution.
pub fn extract_error_fix_pairs(
    entries: &[&LogEntry],
    opts: &LessonOptions,
) -> Vec<ErrorFixPair> {
    let soft_error_re = build_soft_error_regex();

    // Build map: tool_use_id → (tool_name, input, timestamp)
    let mut tool_use_map: HashMap<String, (String, serde_json::Value, Option<String>)> =
        HashMap::new();
    for entry in entries {
        if let LogEntry::Assistant(a) = entry {
            let ts = entry.timestamp().map(|t| t.to_rfc3339());
            for tool in a.message.tool_uses() {
                tool_use_map.insert(
                    tool.id.clone(),
                    (tool.name.clone(), tool.input.clone(), ts.clone()),
                );
            }
        }
    }

    let mut pairs = Vec::new();
    let mut i = 0;

    while i < entries.len() && pairs.len() < opts.limit {
        if let LogEntry::User(user) = entries[i] {
            for result in user.message.tool_results() {
                if pairs.len() >= opts.limit {
                    break;
                }

                // Check for hard error (is_error=true)
                let is_hard_error = result.is_error == Some(true);

                // Check for soft error (error patterns in content)
                let content_text = result.content.as_ref().map(tool_result_text);
                let is_soft_error = if !is_hard_error {
                    if let (Some(ref re), Some(ref text)) = (&soft_error_re, &content_text) {
                        re.is_match(text)
                    } else {
                        false
                    }
                } else {
                    false
                };

                if !is_hard_error && !is_soft_error {
                    continue;
                }

                // Look up the tool name for false positive filtering and display
                let (tool_name, input, timestamp) = tool_use_map
                    .get(&result.tool_use_id)
                    .cloned()
                    .unwrap_or_else(|| ("unknown".into(), serde_json::Value::Null, None));

                // Filter false positives: successful results spuriously flagged is_error=true,
                // or soft error patterns matching inside structured response data
                if let Some(ref text) = content_text {
                    if is_likely_false_positive(&tool_name, text) {
                        continue;
                    }
                }

                let error_preview = content_text
                    .as_deref()
                    .map(|t| truncate_text(t, opts.error_preview_len))
                    .unwrap_or_else(|| "(error with no content)".into());

                let input_summary = extract_tool_input_summary(&tool_name, &input);

                // Look ahead for the next assistant message as resolution
                let mut resolution_summary = None;
                let mut resolution_tools = Vec::new();
                for j in (i + 1)..entries.len() {
                    if let LogEntry::Assistant(a) = entries[j] {
                        let text = a.message.combined_text();
                        let trimmed = text.trim();
                        resolution_summary = if trimmed.is_empty() {
                            None
                        } else {
                            Some(truncate_text(trimmed, opts.resolution_summary_len))
                        };
                        resolution_tools = a
                            .message
                            .tool_uses()
                            .iter()
                            .map(|t| t.name.clone())
                            .collect();
                        break;
                    }
                }

                pairs.push(ErrorFixPair {
                    timestamp,
                    tool_name,
                    input_summary,
                    error_preview,
                    resolution_summary,
                    resolution_tools,
                });
            }
        }
        i += 1;
    }

    pairs
}

/// Extract user corrections from a chronological entry list.
///
/// Detects user messages containing frustration, behavioral correction,
/// or explicit instructions to change approach. Filters out compaction
/// summaries and session continuation text to avoid false positives.
pub fn extract_user_corrections(
    entries: &[&LogEntry],
    opts: &LessonOptions,
) -> Vec<UserCorrectionEntry> {
    let correction_re = match build_correction_regex() {
        Some(re) => re,
        None => return Vec::new(),
    };

    let mut corrections = Vec::new();
    let mut prev_assistant_summary: Option<String> = None;

    for entry in entries {
        if corrections.len() >= opts.limit {
            break;
        }

        match entry {
            LogEntry::Assistant(_) => {
                prev_assistant_summary =
                    extract_assistant_summary(entry, opts.resolution_summary_len);
            }
            LogEntry::User(_) => {
                if let Some(text) = extract_user_prompt_text(entry) {
                    if correction_re.is_match(&text) && text.len() > 10 && !is_summary_text(&text)
                    {
                        corrections.push(UserCorrectionEntry {
                            timestamp: entry.timestamp().map(|t| t.to_rfc3339()),
                            user_text: truncate_text(&text, opts.correction_text_len),
                            prior_assistant_summary: prev_assistant_summary.clone(),
                        });
                    }
                }
            }
            _ => {}
        }
    }

    corrections
}

/// Rank tools by error frequency.
///
/// Returns a sorted list of `(tool_name, error_count)` pairs, most error-prone first.
pub fn rank_error_prone_tools(pairs: &[ErrorFixPair]) -> Vec<(String, usize)> {
    let mut counts: HashMap<String, usize> = HashMap::new();
    for pair in pairs {
        *counts.entry(pair.tool_name.clone()).or_default() += 1;
    }
    let mut ranked: Vec<(String, usize)> = counts.into_iter().collect();
    ranked.sort_by(|a, b| b.1.cmp(&a.1));
    ranked
}

/// Extract all lessons from a chronological entry list.
///
/// This is the main entry point. It combines error→fix pair extraction,
/// user correction detection, and summary statistics.
pub fn extract_lessons(entries: &[&LogEntry], opts: &LessonOptions) -> LessonResult {
    let error_fix_pairs = if opts.category == LessonCategory::Corrections {
        Vec::new()
    } else {
        extract_error_fix_pairs(entries, opts)
    };

    let user_corrections = if opts.category == LessonCategory::Errors {
        Vec::new()
    } else {
        extract_user_corrections(entries, opts)
    };

    let most_error_prone_tools = rank_error_prone_tools(&error_fix_pairs);

    LessonResult {
        summary: LessonsSummary {
            total_errors: error_fix_pairs.len(),
            total_corrections: user_corrections.len(),
            most_error_prone_tools,
        },
        error_fix_pairs,
        user_corrections,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Helper: build a User entry that contains a tool result.
    fn user_tool_result(tool_use_id: &str, is_error: bool, content: &str) -> LogEntry {
        let is_error_str = if is_error { "true" } else { "false" };
        let json = format!(
            r#"{{
                "type": "user",
                "uuid": "user-{tool_use_id}",
                "timestamp": "2025-01-01T00:00:00Z",
                "sessionId": "test-session",
                "version": "2.0.0",
                "message": {{
                    "role": "user",
                    "content": [
                        {{
                            "type": "tool_result",
                            "tool_use_id": "{tool_use_id}",
                            "content": "{content}",
                            "is_error": {is_error_str}
                        }}
                    ]
                }}
            }}"#
        );
        serde_json::from_str(&json).expect("failed to parse user_tool_result JSON")
    }

    /// Helper: build an Assistant entry with a tool use and text.
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

    /// Helper: build a simple assistant text entry.
    fn assistant_text(text: &str) -> LogEntry {
        let json = format!(
            r#"{{
                "type": "assistant",
                "uuid": "asst-text",
                "timestamp": "2025-01-01T00:00:02Z",
                "sessionId": "test-session",
                "version": "2.0.0",
                "message": {{
                    "id": "msg-test",
                    "type": "message",
                    "role": "assistant",
                    "model": "claude-sonnet-4-20250514",
                    "content": [
                        {{
                            "type": "text",
                            "text": "{text}"
                        }}
                    ],
                    "stop_reason": "end_turn"
                }}
            }}"#
        );
        serde_json::from_str(&json).expect("failed to parse assistant_text JSON")
    }

    /// Helper: build a simple user text entry.
    fn user_text(text: &str) -> LogEntry {
        let json = format!(
            r#"{{
                "type": "user",
                "uuid": "user-text",
                "timestamp": "2025-01-01T00:00:03Z",
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

    #[test]
    fn test_extract_hard_error_fix_pair() {
        let entries = vec![
            assistant_with_tool("t1", "Bash", "Running tests"),
            user_tool_result("t1", true, "error: test failed"),
            assistant_text("I see the test failed. Let me fix the issue."),
        ];
        let refs: Vec<&LogEntry> = entries.iter().collect();
        let opts = LessonOptions::default();

        let pairs = extract_error_fix_pairs(&refs, &opts);
        assert_eq!(pairs.len(), 1);
        assert_eq!(pairs[0].tool_name, "Bash");
        assert!(pairs[0].error_preview.contains("test failed"));
        assert!(pairs[0].resolution_summary.is_some());
        assert!(pairs[0]
            .resolution_summary
            .as_ref()
            .unwrap()
            .contains("fix the issue"));
    }

    #[test]
    fn test_extract_soft_error() {
        let entries = vec![
            assistant_with_tool("t1", "Bash", "Building"),
            user_tool_result("t1", false, "error[E0308]: mismatched types"),
            assistant_text("The compiler error indicates a type mismatch."),
        ];
        let refs: Vec<&LogEntry> = entries.iter().collect();
        let opts = LessonOptions::default();

        let pairs = extract_error_fix_pairs(&refs, &opts);
        assert_eq!(pairs.len(), 1);
        assert!(pairs[0].error_preview.contains("E0308"));
    }

    #[test]
    fn test_no_error_no_pair() {
        let entries = vec![
            assistant_with_tool("t1", "Bash", "Running"),
            user_tool_result("t1", false, "All 5 tests passed"),
            assistant_text("Tests passed!"),
        ];
        let refs: Vec<&LogEntry> = entries.iter().collect();
        let opts = LessonOptions::default();

        let pairs = extract_error_fix_pairs(&refs, &opts);
        assert!(pairs.is_empty());
    }

    #[test]
    fn test_error_limit_respected() {
        let mut entries = Vec::new();
        for i in 0..10 {
            let id = format!("t{i}");
            entries.push(assistant_with_tool(&id, "Bash", ""));
            entries.push(user_tool_result(&id, true, "error"));
            entries.push(assistant_text("fixed"));
        }
        let refs: Vec<&LogEntry> = entries.iter().collect();
        let opts = LessonOptions {
            limit: 3,
            ..Default::default()
        };

        let pairs = extract_error_fix_pairs(&refs, &opts);
        assert_eq!(pairs.len(), 3);
    }

    #[test]
    fn test_extract_user_correction() {
        let entries = vec![
            assistant_text("I'll delete the file now."),
            user_text("No, don't delete that file!"),
        ];
        let refs: Vec<&LogEntry> = entries.iter().collect();
        let opts = LessonOptions::default();

        let corrections = extract_user_corrections(&refs, &opts);
        assert_eq!(corrections.len(), 1);
        assert!(corrections[0].user_text.contains("don't delete"));
        assert!(corrections[0].prior_assistant_summary.is_some());
    }

    #[test]
    fn test_correction_skips_short_text() {
        let entries = vec![
            assistant_text("working on it"),
            user_text("no"), // too short (<=10 chars)
        ];
        let refs: Vec<&LogEntry> = entries.iter().collect();
        let opts = LessonOptions::default();

        let corrections = extract_user_corrections(&refs, &opts);
        assert!(corrections.is_empty());
    }

    #[test]
    fn test_correction_skips_summaries() {
        let long_summary = "This session is being continued from a previous conversation that ran out of context. ".repeat(50);
        let entries = vec![
            assistant_text("working"),
            user_text(&long_summary),
        ];
        let refs: Vec<&LogEntry> = entries.iter().collect();
        let opts = LessonOptions::default();

        let corrections = extract_user_corrections(&refs, &opts);
        assert!(corrections.is_empty());
    }

    #[test]
    fn test_category_filter_errors_only() {
        let entries = vec![
            assistant_with_tool("t1", "Bash", ""),
            user_tool_result("t1", true, "error"),
            assistant_text("fixed"),
            user_text("Why did you do that wrong thing again?"),
        ];
        let refs: Vec<&LogEntry> = entries.iter().collect();
        let opts = LessonOptions {
            category: LessonCategory::Errors,
            ..Default::default()
        };

        let result = extract_lessons(&refs, &opts);
        assert_eq!(result.error_fix_pairs.len(), 1);
        assert!(result.user_corrections.is_empty());
    }

    #[test]
    fn test_category_filter_corrections_only() {
        let entries = vec![
            assistant_with_tool("t1", "Bash", ""),
            user_tool_result("t1", true, "error"),
            assistant_text("I tried something wrong."),
            user_text("That's not what I asked for, stop doing that."),
        ];
        let refs: Vec<&LogEntry> = entries.iter().collect();
        let opts = LessonOptions {
            category: LessonCategory::Corrections,
            ..Default::default()
        };

        let result = extract_lessons(&refs, &opts);
        assert!(result.error_fix_pairs.is_empty());
        assert_eq!(result.user_corrections.len(), 1);
    }

    #[test]
    fn test_rank_error_prone_tools() {
        let pairs = vec![
            ErrorFixPair {
                timestamp: None,
                tool_name: "Bash".into(),
                input_summary: HashMap::new(),
                error_preview: "err".into(),
                resolution_summary: None,
                resolution_tools: vec![],
            },
            ErrorFixPair {
                timestamp: None,
                tool_name: "Edit".into(),
                input_summary: HashMap::new(),
                error_preview: "err".into(),
                resolution_summary: None,
                resolution_tools: vec![],
            },
            ErrorFixPair {
                timestamp: None,
                tool_name: "Bash".into(),
                input_summary: HashMap::new(),
                error_preview: "err".into(),
                resolution_summary: None,
                resolution_tools: vec![],
            },
        ];

        let ranked = rank_error_prone_tools(&pairs);
        assert_eq!(ranked[0], ("Bash".to_string(), 2));
        assert_eq!(ranked[1], ("Edit".to_string(), 1));
    }

    #[test]
    fn test_extract_lessons_full() {
        let entries = vec![
            assistant_with_tool("t1", "Bash", "Running cargo test"),
            user_tool_result("t1", true, "error: compilation failed"),
            assistant_text("I see the compilation error. Let me fix it."),
            user_text("Why did you run tests before fixing the import?"),
        ];
        let refs: Vec<&LogEntry> = entries.iter().collect();
        let opts = LessonOptions::default();

        let result = extract_lessons(&refs, &opts);
        assert_eq!(result.error_fix_pairs.len(), 1);
        assert_eq!(result.user_corrections.len(), 1);
        assert_eq!(result.summary.total_errors, 1);
        assert_eq!(result.summary.total_corrections, 1);
        assert!(!result.summary.most_error_prone_tools.is_empty());
    }

    #[test]
    fn test_false_positive_read_file_content() {
        // Read tool result with is_error=true but content is valid file output
        let entries = vec![
            assistant_with_tool("t1", "Read", "Reading file"),
            user_tool_result("t1", true, "     1\\u2192fn main() {}"),
            assistant_text("Got the file."),
        ];
        let refs: Vec<&LogEntry> = entries.iter().collect();
        let opts = LessonOptions::default();

        let pairs = extract_error_fix_pairs(&refs, &opts);
        assert!(pairs.is_empty(), "Read with line-numbered content should be filtered as false positive");
    }

    #[test]
    fn test_false_positive_mcp_json_result() {
        // MCP tool result with is_error=true but content is valid JSON
        let entries = vec![
            assistant_with_tool("t1", "mcp__snatch__get_session_lessons", "Fetching lessons"),
            user_tool_result("t1", true, "{session_id: abc}"),
            assistant_text("Got the lessons."),
        ];
        let refs: Vec<&LogEntry> = entries.iter().collect();
        let opts = LessonOptions::default();

        let pairs = extract_error_fix_pairs(&refs, &opts);
        assert!(pairs.is_empty(), "MCP tool returning JSON should be filtered as false positive");
    }

    #[test]
    fn test_false_positive_read_offset_content() {
        // Read tool result with is_error=true but content is file output at offset
        let entries = vec![
            assistant_with_tool("t1", "Read", "Reading file"),
            user_tool_result("t1", true, "787\\u2192#[cfg(test)]"),
            assistant_text("Got the file."),
        ];
        let refs: Vec<&LogEntry> = entries.iter().collect();
        let opts = LessonOptions::default();

        let pairs = extract_error_fix_pairs(&refs, &opts);
        assert!(pairs.is_empty(), "Read with offset line-numbered content should be filtered");
    }

    #[test]
    fn test_false_positive_grep_output() {
        // Grep tool result with is_error=true but content is grep output
        let entries = vec![
            assistant_with_tool("t1", "Grep", "Searching"),
            user_tool_result("t1", true, "21:[Omitted long matching line]"),
            assistant_text("Found matches."),
        ];
        let refs: Vec<&LogEntry> = entries.iter().collect();
        let opts = LessonOptions::default();

        let pairs = extract_error_fix_pairs(&refs, &opts);
        assert!(pairs.is_empty(), "Grep with line-numbered output should be filtered");
    }

    #[test]
    fn test_real_read_error_not_filtered() {
        // Read tool result with is_error=true and actual error content
        let entries = vec![
            assistant_with_tool("t1", "Read", "Reading file"),
            user_tool_result("t1", true, "File does not exist."),
            assistant_text("The file is missing."),
        ];
        let refs: Vec<&LogEntry> = entries.iter().collect();
        let opts = LessonOptions::default();

        let pairs = extract_error_fix_pairs(&refs, &opts);
        assert_eq!(pairs.len(), 1, "Real Read error should NOT be filtered");
    }

    #[test]
    fn test_is_likely_false_positive() {
        // Read: line-numbered content at any offset
        assert!(is_likely_false_positive("Read", "     1→fn main() {}"));
        assert!(is_likely_false_positive("Read", "1→hello"));
        assert!(is_likely_false_positive("Read", "787→#[cfg(test)]"));
        assert!(is_likely_false_positive("Read", "   42→some line"));
        assert!(!is_likely_false_positive("Read", "File does not exist."));
        assert!(!is_likely_false_positive("Read", "Sibling tool call errored"));

        // Grep: grep-style line output
        assert!(is_likely_false_positive("Grep", "21:[Omitted long matching line]"));
        assert!(is_likely_false_positive("Grep", "5-context line"));
        assert!(!is_likely_false_positive("Grep", "InputValidationError: Grep failed"));

        // MCP: JSON responses
        assert!(is_likely_false_positive("mcp__snatch__list_sessions", r#"{"sessions": []}"#));
        assert!(!is_likely_false_positive("mcp__snatch__list_sessions", "MCP error -32000: Connection closed"));

        // Agent: substantial non-error text
        assert!(is_likely_false_positive("Agent", &"x".repeat(300)));
        assert!(!is_likely_false_positive("Agent", "Error: something went wrong"));

        // Other tools: no filtering
        assert!(!is_likely_false_positive("Bash", "error: compilation failed"));
    }
}
