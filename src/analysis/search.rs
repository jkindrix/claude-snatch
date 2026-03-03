//! Multi-scope regex search across conversation entries.
//!
//! Searches user text, assistant text, tool inputs/results, and thinking blocks
//! depending on the requested scope.

use crate::model::message::LogEntry;

use super::extraction::extract_user_prompt_text;

/// Search a single entry for a regex pattern match.
///
/// Returns a list of `(matched_text, context_snippet)` pairs. The `scope` parameter
/// controls which parts of the entry are searched:
///
/// - `"text"`: User prompt text and assistant response text
/// - `"tools"`: Tool result content (user entries) and tool use inputs (assistant entries)
/// - `"thinking"`: Assistant thinking/reasoning blocks
/// - `"all"`: All of the above
///
/// `max_context` controls how many characters of surrounding context to include.
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
            // Snap to char boundaries
            let start = snap_char_boundary_left(text, start);
            let end = snap_char_boundary_right(text, end);
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

/// Snap a byte index left to the nearest char boundary.
fn snap_char_boundary_left(s: &str, idx: usize) -> usize {
    let mut i = idx;
    while i > 0 && !s.is_char_boundary(i) {
        i -= 1;
    }
    i
}

/// Snap a byte index right to the nearest char boundary.
fn snap_char_boundary_right(s: &str, idx: usize) -> usize {
    let mut i = idx;
    while i < s.len() && !s.is_char_boundary(i) {
        i += 1;
    }
    i
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a System LogEntry from JSON for testing.
    fn system_entry(content: &str) -> LogEntry {
        serde_json::from_value(serde_json::json!({
            "type": "system",
            "uuid": "test-uuid",
            "timestamp": "2025-01-01T00:00:00Z",
            "content": content,
        }))
        .unwrap()
    }

    #[test]
    fn test_search_no_match() {
        let entry = system_entry("hello world");
        let re = regex::Regex::new("foobar").unwrap();
        let results = search_entry_text(&entry, &re, "text", 20);
        assert!(results.is_empty());
    }

    #[test]
    fn test_search_system_text() {
        let entry = system_entry("the quick brown fox jumps");
        let re = regex::Regex::new("brown").unwrap();
        let results = search_entry_text(&entry, &re, "text", 5);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].0, "brown");
        assert!(results[0].1.contains("brown"));
    }

    #[test]
    fn test_search_scope_filtering() {
        let entry = system_entry("hello world");
        let re = regex::Regex::new("hello").unwrap();

        // "text" scope should find it
        assert_eq!(search_entry_text(&entry, &re, "text", 10).len(), 1);
        // "tools" scope should not
        assert_eq!(search_entry_text(&entry, &re, "tools", 10).len(), 0);
        // "thinking" scope should not
        assert_eq!(search_entry_text(&entry, &re, "thinking", 10).len(), 0);
        // "all" scope should find it
        assert_eq!(search_entry_text(&entry, &re, "all", 10).len(), 1);
    }

    #[test]
    fn test_snap_char_boundary() {
        let s = "hello 😀 world";
        // The emoji is 4 bytes. Make sure we snap correctly.
        let emoji_start = s.find('😀').unwrap();
        assert_eq!(snap_char_boundary_left(s, emoji_start + 1), emoji_start);
        assert_eq!(snap_char_boundary_right(s, emoji_start + 1), emoji_start + 4);
    }
}
