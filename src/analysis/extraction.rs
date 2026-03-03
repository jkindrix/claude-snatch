//! Conversation text extraction helpers.
//!
//! Pure functions that extract text, tool names, and metadata from [`LogEntry`] values.
//! These are the building blocks used by lessons, timeline, and search operations.

use std::collections::{HashMap, HashSet};

use crate::model::content::ContentBlock;
use crate::model::message::{LogEntry, SystemSubtype, UserContent};

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
    format!("{truncated}...")
}

/// Check if a user entry is an actual human-authored prompt.
///
/// Returns `false` for system-generated user entries like `/compact` commands,
/// `/mcp` reconnect messages, `local-command-caveat` wrappers,
/// `local-command-stdout` outputs, and `[Request interrupted by user]`.
pub fn is_human_prompt(entry: &LogEntry) -> bool {
    let text = match extract_user_prompt_text(entry) {
        Some(t) => t,
        None => return false,
    };
    !is_noise_text(&text)
}

/// Check if text content is system-generated noise rather than human input.
fn is_noise_text(text: &str) -> bool {
    let trimmed = text.trim();

    // XML-tagged system messages
    if trimmed.starts_with('<') {
        let noise_tags = [
            "<local-command-caveat>",
            "<local-command-stdout>",
            "<local-command-stderr>",
            "<command-name>",
            "<system-reminder>",
        ];
        if noise_tags.iter().any(|tag| trimmed.starts_with(tag)) {
            return true;
        }
    }

    // Interrupt markers
    if trimmed.starts_with("[Request interrupted") {
        return true;
    }

    false
}

/// Extract visible user prompt text from a [`LogEntry`].
///
/// Returns `None` if the entry is not a User message or has no visible text
/// (i.e., contains only tool results).
pub fn extract_user_prompt_text(entry: &LogEntry) -> Option<String> {
    match entry {
        LogEntry::User(user) => match &user.message {
            UserContent::Simple(s) => {
                let text = s.content.trim();
                if text.is_empty() {
                    None
                } else {
                    Some(text.to_string())
                }
            }
            UserContent::Blocks(b) => {
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
        },
        _ => None,
    }
}

/// Extract text summary from an Assistant message.
///
/// Returns the assistant's combined text content, truncated to `max_len`.
/// Returns `None` if the entry is not an Assistant message or has no text.
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
        LogEntry::Assistant(assistant) => assistant
            .message
            .tool_uses()
            .iter()
            .map(|t| t.name.clone())
            .collect(),
        _ => vec![],
    }
}

/// Extract key fields from a tool use input for summary display.
///
/// Returns a map of field names to values for the most important fields,
/// depending on the tool type (e.g., `file_path` for Write/Edit, `command` for Bash).
pub fn extract_tool_input_summary(
    tool_name: &str,
    input: &serde_json::Value,
) -> HashMap<String, String> {
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
///
/// Looks at Write, Edit, and Read tool calls for `file_path` fields.
/// Returns sorted basenames (parent/filename format would need custom logic).
pub fn extract_files_from_tools(entries: &[&LogEntry]) -> Vec<String> {
    let mut files = HashSet::new();

    for entry in entries {
        if let LogEntry::Assistant(assistant) = entry {
            for tool_use in assistant.message.tool_uses() {
                match tool_use.name.as_str() {
                    "Write" | "Edit" | "Read" => {
                        if let Some(fp) = tool_use
                            .input
                            .get("file_path")
                            .and_then(|v| v.as_str())
                        {
                            let basename = std::path::Path::new(fp)
                                .file_name()
                                .and_then(|n| n.to_str())
                                .unwrap_or(fp);
                            files.insert(basename.to_string());
                        }
                    }
                    _ => {}
                }
            }
        }
    }

    let mut sorted: Vec<String> = files.into_iter().collect();
    sorted.sort();
    sorted
}

/// Extract combined thinking text from an Assistant message.
///
/// Returns the concatenated thinking block text, separated by `---`,
/// truncated to `max_len`. Returns `None` if the entry has no thinking blocks.
pub fn extract_thinking_text(entry: &LogEntry, max_len: usize) -> Option<String> {
    match entry {
        LogEntry::Assistant(assistant) => {
            let blocks = assistant.message.thinking_blocks();
            if blocks.is_empty() {
                return None;
            }
            let combined: String = blocks
                .iter()
                .map(|b| b.thinking.as_str())
                .collect::<Vec<_>>()
                .join("\n---\n");
            let trimmed = combined.trim();
            if trimmed.is_empty() {
                None
            } else {
                Some(truncate_text(trimmed, max_len))
            }
        }
        _ => None,
    }
}

/// Check if any tool results in a set of entries had errors.
pub fn has_tool_errors(entries: &[&LogEntry]) -> bool {
    for entry in entries {
        if let LogEntry::User(user) = entry {
            for result in user.message.tool_results() {
                if result.is_error == Some(true) {
                    return true;
                }
            }
        }
    }
    false
}

/// Extract error preview text from a tool result.
///
/// Returns `None` if the result is not an error.
/// Extracts plain text from the content (not Debug format).
pub fn extract_error_preview(
    result: &crate::model::content::ToolResult,
    max_len: usize,
) -> Option<String> {
    if result.is_error != Some(true) {
        return None;
    }
    match &result.content {
        Some(content) => {
            let text = match content {
                crate::model::content::ToolResultContent::String(s) => s.clone(),
                crate::model::content::ToolResultContent::Array(arr) => arr
                    .iter()
                    .filter_map(|v| v.get("text").and_then(|t| t.as_str()))
                    .collect::<Vec<_>>()
                    .join("\n"),
            };
            Some(truncate_text(&text, max_len))
        }
        None => Some("(error with no content)".into()),
    }
}

/// Check if an Assistant message has thinking blocks.
pub fn has_thinking(entry: &LogEntry) -> bool {
    match entry {
        LogEntry::Assistant(assistant) => assistant.message.has_thinking(),
        _ => false,
    }
}

/// Get the model from an Assistant message.
///
/// Returns `None` if the entry is not an Assistant message or has an empty model string.
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

/// Detect compaction events in a conversation's main thread entries.
///
/// Returns a list of `(timestamp_rfc3339, optional_summary)` pairs.
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
        assert!(result.len() <= 18);
    }

    #[test]
    fn test_truncate_text_word_boundary() {
        let result = truncate_text("hello world foo bar", 12);
        assert_eq!(result, "hello world...");
    }

    #[test]
    fn test_truncate_text_multibyte() {
        // Should not panic on multi-byte characters
        let result = truncate_text("hello \u{1F600} world", 8);
        assert!(result.ends_with("..."));
    }

    #[test]
    fn test_extract_tool_input_summary_bash() {
        let input = serde_json::json!({"command": "cargo test --all"});
        let summary = extract_tool_input_summary("Bash", &input);
        assert_eq!(summary.get("command").unwrap(), "cargo test --all");
    }

    #[test]
    fn test_extract_tool_input_summary_write() {
        let input =
            serde_json::json!({"file_path": "/home/user/src/main.rs", "content": "fn main() {}"});
        let summary = extract_tool_input_summary("Write", &input);
        assert_eq!(
            summary.get("file_path").unwrap(),
            "/home/user/src/main.rs"
        );
        assert!(!summary.contains_key("content"));
    }

    #[test]
    fn test_extract_tool_input_summary_grep() {
        let input = serde_json::json!({"pattern": "fn main", "path": "/src"});
        let summary = extract_tool_input_summary("Grep", &input);
        assert_eq!(summary.get("pattern").unwrap(), "fn main");
        assert_eq!(summary.get("path").unwrap(), "/src");
    }

    #[test]
    fn test_extract_tool_input_summary_unknown() {
        let input = serde_json::json!({"foo": "bar", "baz": "qux", "extra": "ignored"});
        let summary = extract_tool_input_summary("CustomTool", &input);
        // Should grab first 2 string fields
        assert!(summary.len() <= 2);
    }

    #[test]
    fn test_is_noise_text_local_command() {
        assert!(is_noise_text("<local-command-caveat>Caveat: blah</local-command-caveat>"));
        assert!(is_noise_text("<local-command-stdout>Reconnected to snatch.</local-command-stdout>"));
        assert!(is_noise_text("<local-command-stderr>error output</local-command-stderr>"));
        assert!(is_noise_text("<command-name>/compact</command-name>"));
        assert!(is_noise_text("<system-reminder>Some reminder</system-reminder>"));
    }

    #[test]
    fn test_is_noise_text_interrupt() {
        assert!(is_noise_text("[Request interrupted by user]"));
        assert!(is_noise_text("[Request interrupted by user for tool use]"));
    }

    #[test]
    fn test_is_noise_text_real_prompt() {
        assert!(!is_noise_text("Fix the bug in auth.rs"));
        assert!(!is_noise_text("commit and push"));
        assert!(!is_noise_text("What files handle routing?"));
    }

    #[test]
    fn test_is_noise_text_edge_cases() {
        assert!(!is_noise_text("")); // empty is not noise (it's nothing)
        assert!(!is_noise_text("<p>HTML paragraph</p>")); // random XML is not noise
        assert!(!is_noise_text("[some bracketed text]")); // only interrupt markers
    }
}
