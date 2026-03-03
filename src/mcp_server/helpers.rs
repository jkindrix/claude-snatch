//! Shared helper functions for MCP server tools.

use std::collections::HashMap;

use chrono::{Duration, Utc};
use mcpkit::prelude::ToolOutput;

use crate::analytics::SessionAnalytics;
use crate::discovery::ClaudeDirectory;
use crate::model::message::{LogEntry, SystemSubtype, UserContent};
use crate::model::content::ContentBlock;
use crate::reconstruction::Conversation;

use super::SnatchServer;

/// Resolved session with parsed conversation and analytics.
pub struct ResolvedSession {
    pub session_id: String,
    pub project_path: String,
    pub conversation: Conversation,
    pub analytics: SessionAnalytics,
}

/// Resolve a session ID to a parsed conversation.
pub fn resolve_session(server: &SnatchServer, session_id: &str) -> Result<ResolvedSession, ToolOutput> {
    let claude_dir = server.get_claude_dir().map_err(ToolOutput::error)?;

    let session = claude_dir
        .find_session(session_id)
        .map_err(|e| ToolOutput::error(format!("Failed to find session: {e}")))?
        .ok_or_else(|| ToolOutput::error(format!("Session not found: {session_id}")))?;

    let entries = session
        .parse_with_options(server.max_file_size)
        .map_err(|e| ToolOutput::error(format!("Failed to parse session: {e}")))?;

    let conversation = Conversation::from_entries(entries)
        .map_err(|e| ToolOutput::error(format!("Failed to reconstruct conversation: {e}")))?;

    let analytics = SessionAnalytics::from_conversation(&conversation);

    Ok(ResolvedSession {
        session_id: session.session_id().to_string(),
        project_path: session.project_path().to_string(),
        conversation,
        analytics,
    })
}

/// Get the Claude directory from the server config.
pub fn get_claude_dir(server: &SnatchServer) -> Result<ClaudeDirectory, ToolOutput> {
    server.get_claude_dir().map_err(ToolOutput::error)
}

/// Truncate text at a word boundary with "..." suffix.
pub fn truncate_text(text: &str, max_len: usize) -> String {
    let text = text.trim();
    if text.len() <= max_len {
        return text.to_string();
    }

    // Find nearest char boundary at or before max_len
    let mut end = max_len;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }

    // Find last space before the boundary for word-break
    let truncated = &text[..end];
    if let Some(last_space) = truncated.rfind(' ') {
        if last_space > end / 2 {
            return format!("{}...", &text[..last_space]);
        }
    }
    format!("{}...", truncated)
}

/// Extract visible user prompt text from a LogEntry.
/// Returns None if the entry is not a User message or has no visible text.
pub fn extract_user_prompt_text(entry: &LogEntry) -> Option<String> {
    match entry {
        LogEntry::User(user) => {
            match &user.message {
                UserContent::Simple(s) => {
                    let text = s.content.trim();
                    if text.is_empty() {
                        None
                    } else {
                        Some(text.to_string())
                    }
                }
                UserContent::Blocks(b) => {
                    // Extract only text blocks, skip tool results
                    let texts: Vec<&str> = b
                        .content
                        .iter()
                        .filter_map(|c| match c {
                            ContentBlock::Text(t) if !t.text.trim().is_empty() => {
                                Some(t.text.trim())
                            }
                            _ => None,
                        })
                        .collect();
                    if texts.is_empty() {
                        None
                    } else {
                        Some(texts.join("\n"))
                    }
                }
            }
        }
        _ => None,
    }
}

/// Extract truncated summary text from an Assistant message.
pub fn extract_assistant_summary(entry: &LogEntry, max_len: usize) -> Option<String> {
    match entry {
        LogEntry::Assistant(assistant) => {
            let text = assistant.message.combined_text();
            let text = text.trim();
            if text.is_empty() {
                None
            } else {
                Some(truncate_text(text, max_len))
            }
        }
        _ => None,
    }
}

/// Extract tool names from an Assistant message's content blocks.
pub fn extract_tool_names(entry: &LogEntry) -> Vec<String> {
    match entry {
        LogEntry::Assistant(assistant) => {
            assistant
                .message
                .tool_uses()
                .iter()
                .map(|t| t.name.clone())
                .collect()
        }
        _ => vec![],
    }
}

/// Check if an Assistant message has thinking blocks.
pub fn has_thinking(entry: &LogEntry) -> bool {
    match entry {
        LogEntry::Assistant(assistant) => assistant.message.has_thinking(),
        _ => false,
    }
}

/// Extract combined thinking text from an Assistant message.
pub fn extract_thinking_text(entry: &LogEntry, max_len: usize) -> Option<String> {
    crate::analysis::extraction::extract_thinking_text(entry, max_len)
}

/// Get the model from an Assistant message.
pub fn get_model(entry: &LogEntry) -> Option<String> {
    match entry {
        LogEntry::Assistant(assistant) => {
            let model = &assistant.message.model;
            if model.is_empty() {
                None
            } else {
                Some(model.clone())
            }
        }
        _ => None,
    }
}

/// Extract key fields from a tool use input for summary.
/// Returns a map of field_name -> value for the most important fields.
pub fn extract_tool_input_summary(tool_name: &str, input: &serde_json::Value) -> HashMap<String, String> {
    let mut summary = HashMap::new();
    let obj = match input.as_object() {
        Some(o) => o,
        None => return summary,
    };

    match tool_name {
        "Write" | "Read" => {
            if let Some(fp) = obj.get("file_path").and_then(|v| v.as_str()) {
                summary.insert("file_path".into(), fp.to_string());
            }
        }
        "Edit" => {
            if let Some(fp) = obj.get("file_path").and_then(|v| v.as_str()) {
                summary.insert("file_path".into(), fp.to_string());
            }
        }
        "Bash" => {
            if let Some(cmd) = obj.get("command").and_then(|v| v.as_str()) {
                summary.insert("command".into(), truncate_text(cmd, 200));
            }
        }
        "Grep" => {
            if let Some(p) = obj.get("pattern").and_then(|v| v.as_str()) {
                summary.insert("pattern".into(), p.to_string());
            }
            if let Some(path) = obj.get("path").and_then(|v| v.as_str()) {
                summary.insert("path".into(), path.to_string());
            }
        }
        "Glob" => {
            if let Some(p) = obj.get("pattern").and_then(|v| v.as_str()) {
                summary.insert("pattern".into(), p.to_string());
            }
        }
        "Agent" => {
            if let Some(desc) = obj.get("description").and_then(|v| v.as_str()) {
                summary.insert("description".into(), truncate_text(desc, 100));
            }
        }
        "WebSearch" => {
            if let Some(q) = obj.get("query").and_then(|v| v.as_str()) {
                summary.insert("query".into(), truncate_text(q, 150));
            }
        }
        "WebFetch" => {
            if let Some(url) = obj.get("url").and_then(|v| v.as_str()) {
                summary.insert("url".into(), url.to_string());
            }
        }
        _ => {
            // For unknown tools, grab first string field up to 100 chars
            for (k, v) in obj.iter().take(2) {
                if let Some(s) = v.as_str() {
                    summary.insert(k.clone(), truncate_text(s, 100));
                }
            }
        }
    }

    summary
}

/// Extract unique file paths from tool inputs across entries.
pub fn extract_files_from_tools(entries: &[&LogEntry]) -> Vec<String> {
    crate::analysis::extraction::extract_files_from_tools(entries)
}

/// Check if any tool results in a set of entries had errors.
pub fn has_tool_errors(entries: &[&LogEntry]) -> bool {
    crate::analysis::extraction::has_tool_errors(entries)
}

/// Extract the error preview text from a tool result.
pub fn extract_error_preview(result: &crate::model::content::ToolResult, max_len: usize) -> Option<String> {
    crate::analysis::extraction::extract_error_preview(result, max_len)
}

/// Parse a period string like "24h", "7d", "30d" into a chrono::Duration.
/// Returns None for "all".
pub fn parse_period(period: &str) -> Result<Option<Duration>, String> {
    match period.trim().to_lowercase().as_str() {
        "all" => Ok(None),
        s => {
            if let Some(h) = s.strip_suffix('h') {
                let hours: i64 = h.parse().map_err(|_| format!("Invalid hours: {h}"))?;
                Ok(Some(Duration::hours(hours)))
            } else if let Some(d) = s.strip_suffix('d') {
                let days: i64 = d.parse().map_err(|_| format!("Invalid days: {d}"))?;
                Ok(Some(Duration::days(days)))
            } else if let Some(w) = s.strip_suffix('w') {
                let weeks: i64 = w.parse().map_err(|_| format!("Invalid weeks: {w}"))?;
                Ok(Some(Duration::weeks(weeks)))
            } else {
                Err(format!("Invalid period format: {s}. Use e.g. '24h', '7d', '30d', 'all'"))
            }
        }
    }
}

/// Filter sessions by time period. Returns cutoff time if period is bounded.
pub fn period_cutoff(period: &str) -> Result<Option<chrono::DateTime<Utc>>, String> {
    match parse_period(period)? {
        Some(duration) => Ok(Some(Utc::now() - duration)),
        None => Ok(None),
    }
}

/// Detect compaction events in a conversation's main thread entries.
pub fn find_compaction_events(entries: &[&LogEntry]) -> Vec<(String, Option<String>)> {
    let mut events = Vec::new();
    for entry in entries {
        if let LogEntry::System(sys) = entry {
            if sys.subtype == Some(SystemSubtype::CompactBoundary) {
                let ts = sys.timestamp.to_rfc3339();
                let summary = sys.content.clone();
                events.push((ts, summary));
            }
        }
    }
    events
}

/// Search a single entry for a regex pattern match.
/// Returns the matched text and surrounding context if found.
pub fn search_entry_text(
    entry: &LogEntry,
    regex: &regex::Regex,
    scope: &str,
    max_context: usize,
) -> Vec<(String, String)> {
    let mut matches = Vec::new();

    let mut search_text = |text: &str| {
        for mat in regex.find_iter(text) {
            let start = mat.start().saturating_sub(max_context);
            let end = (mat.end() + max_context).min(text.len());
            let context = &text[start..end];
            matches.push((mat.as_str().to_string(), context.to_string()));
        }
    };

    match entry {
        LogEntry::User(user) => {
            if scope == "text" || scope == "all" {
                if let Some(text) = extract_user_prompt_text(&LogEntry::User(user.clone())) {
                    search_text(&text);
                }
            }
            if scope == "tools" || scope == "all" {
                for result in user.message.tool_results() {
                    if let Some(ref content) = result.content {
                        let text = format!("{content:?}");
                        search_text(&text);
                    }
                }
            }
        }
        LogEntry::Assistant(assistant) => {
            if scope == "text" || scope == "all" {
                let text = assistant.message.combined_text();
                if !text.is_empty() {
                    search_text(&text);
                }
            }
            if scope == "tools" || scope == "all" {
                for tool in assistant.message.tool_uses() {
                    let input_str = tool.input.to_string();
                    search_text(&input_str);
                }
            }
            if scope == "thinking" || scope == "all" {
                for block in assistant.message.thinking_blocks() {
                    if !block.thinking.is_empty() {
                        search_text(&block.thinking);
                    }
                }
            }
        }
        LogEntry::System(sys) => {
            if scope == "text" || scope == "all" {
                if let Some(ref content) = sys.content {
                    search_text(content);
                }
            }
        }
        _ => {}
    }

    matches
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_truncate_text_short() {
        assert_eq!(truncate_text("hello", 10), "hello");
    }

    #[test]
    fn test_truncate_text_long() {
        let result = truncate_text("hello world this is a long text", 15);
        assert!(result.ends_with("..."));
        assert!(result.len() <= 18); // 15 + "..."
    }

    #[test]
    fn test_truncate_text_word_boundary() {
        let result = truncate_text("hello world foo bar", 12);
        assert_eq!(result, "hello world...");
    }

    #[test]
    fn test_parse_period_hours() {
        let d = parse_period("24h").unwrap().unwrap();
        assert_eq!(d.num_hours(), 24);
    }

    #[test]
    fn test_parse_period_days() {
        let d = parse_period("7d").unwrap().unwrap();
        assert_eq!(d.num_days(), 7);
    }

    #[test]
    fn test_parse_period_all() {
        assert!(parse_period("all").unwrap().is_none());
    }

    #[test]
    fn test_parse_period_invalid() {
        assert!(parse_period("xyz").is_err());
    }

    #[test]
    fn test_extract_tool_input_summary_bash() {
        let input = serde_json::json!({"command": "cargo test --all"});
        let summary = extract_tool_input_summary("Bash", &input);
        assert_eq!(summary.get("command").unwrap(), "cargo test --all");
    }

    #[test]
    fn test_extract_tool_input_summary_write() {
        let input = serde_json::json!({"file_path": "/home/user/src/main.rs", "content": "fn main() {}"});
        let summary = extract_tool_input_summary("Write", &input);
        assert_eq!(summary.get("file_path").unwrap(), "/home/user/src/main.rs");
        assert!(!summary.contains_key("content"));
    }
}
