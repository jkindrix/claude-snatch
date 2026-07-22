//! Multi-scope regex search across conversation entries.
//!
//! Searches user text, assistant text, tool inputs/results, and thinking blocks
//! depending on the requested scope.

use serde::{Deserialize, Serialize};

use crate::model::{ContentBlock, LogEntry, UserContent};

use super::extraction::extract_user_prompt_text;

/// Provider-neutral kind of one independently searchable text segment.
///
/// Search projections preserve block/emission boundaries instead of joining
/// equal text. This prevents deduplication by value from erasing legitimate
/// repeated messages and lets indexed search retain exact scope semantics.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SearchSegmentKind {
    /// Human, harness, or tool-authored text carried by a user entry. Prompt
    /// authorship/delivery are separate entry-semantic fields in the index.
    UserText,
    /// Visible assistant response text.
    AssistantText,
    /// System-event text.
    SystemText,
    /// Compaction/session summary text.
    SummaryText,
    /// Persisted reasoning/thinking summary text.
    Reasoning,
    /// Serialized tool-call input.
    ToolInput,
    /// Tool execution output.
    ToolResult,
}

impl SearchSegmentKind {
    /// Stable human-readable location label shared by exact search renderers.
    #[must_use]
    pub const fn location(self) -> &'static str {
        match self {
            Self::UserText => "user message",
            Self::AssistantText => "assistant text",
            Self::SystemText => "system",
            Self::SummaryText => "summary",
            Self::Reasoning => "thinking",
            Self::ToolInput => "tool input",
            Self::ToolResult => "tool result",
        }
    }

    /// Whether this segment belongs to the ordinary conversational-text
    /// scope.
    #[must_use]
    pub const fn is_text(self) -> bool {
        matches!(
            self,
            Self::UserText | Self::AssistantText | Self::SystemText | Self::SummaryText
        )
    }

    /// Whether this segment belongs to the tool input/output scope.
    #[must_use]
    pub const fn is_tool(self) -> bool {
        matches!(self, Self::ToolInput | Self::ToolResult)
    }
}

/// One ordered search projection from a normalized entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SearchSegment {
    /// Scope/location classification.
    pub kind: SearchSegmentKind,
    /// Exact searchable text after bounded binary-image omission.
    pub text: String,
    /// Native tool name for tool inputs, when present.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_name: Option<String>,
    /// Native call id for tool inputs/results, when present.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    /// Native explicit tool-result error state, when present.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_is_error: Option<bool>,
}

/// Machine-visible coverage limits for one entry projection.
///
/// Images and unknown blocks/entries remain preserved by the normalized
/// session and fidelity exports; they are not silently presented as indexed
/// searchable text.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SearchProjectionCoverage {
    /// Image content blocks omitted from text indexing.
    pub images_omitted: usize,
    /// Forward-compatible content block kinds without a text contract.
    pub unknown_blocks_omitted: usize,
    /// Forward-compatible top-level entries without a text contract.
    pub unknown_entries_omitted: usize,
}

/// Ordered searchable content plus explicit omission accounting for one
/// normalized entry.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct EntrySearchProjection {
    /// Searchable segments in native normalized block order.
    pub segments: Vec<SearchSegment>,
    /// Content that deliberately remained outside the text-search contract.
    pub coverage: SearchProjectionCoverage,
}

fn text_segment(kind: SearchSegmentKind, text: &str) -> Option<SearchSegment> {
    (!text.is_empty()).then(|| SearchSegment {
        kind,
        text: text.to_string(),
        tool_name: None,
        tool_call_id: None,
        tool_is_error: None,
    })
}

fn push_content_block(
    projection: &mut EntrySearchProjection,
    block: &ContentBlock,
    text_kind: SearchSegmentKind,
) {
    match block {
        ContentBlock::Text(text) => {
            if let Some(segment) = text_segment(text_kind, &text.text) {
                projection.segments.push(segment);
            }
        }
        ContentBlock::Thinking(thinking) => {
            if let Some(segment) = text_segment(SearchSegmentKind::Reasoning, &thinking.thinking) {
                projection.segments.push(segment);
            }
        }
        ContentBlock::ToolUse(tool) => {
            let text = serde_json::to_string(&tool.input).unwrap_or_default();
            if !text.is_empty() {
                projection.segments.push(SearchSegment {
                    kind: SearchSegmentKind::ToolInput,
                    text,
                    tool_name: Some(tool.name.clone()),
                    tool_call_id: Some(tool.id.clone()),
                    tool_is_error: None,
                });
            }
        }
        ContentBlock::ToolResult(result) => {
            if let Some(content) = &result.content {
                let text = content.to_display_string(false);
                if !text.is_empty() {
                    projection.segments.push(SearchSegment {
                        kind: SearchSegmentKind::ToolResult,
                        text,
                        tool_name: None,
                        tool_call_id: Some(result.tool_use_id.clone()),
                        tool_is_error: result.is_error,
                    });
                }
            }
        }
        ContentBlock::Image(_) => {
            projection.coverage.images_omitted =
                projection.coverage.images_omitted.saturating_add(1);
        }
        ContentBlock::Unknown { .. } => {
            projection.coverage.unknown_blocks_omitted =
                projection.coverage.unknown_blocks_omitted.saturating_add(1);
        }
    }
}

/// Project one normalized entry into ordered provider-neutral search text.
///
/// This function deliberately performs no provider inference. Prompt
/// authorship/delivery, activity, canonical tool kind, lineage, and source
/// identity come from the surrounding [`crate::provider::ParsedSession`] and
/// are attached by the index builder. Native raw records remain at the source.
#[must_use]
pub fn project_entry_for_search(entry: &LogEntry) -> EntrySearchProjection {
    let mut projection = EntrySearchProjection::default();
    match entry {
        LogEntry::User(user) => match &user.message {
            UserContent::Simple(content) => {
                if let Some(segment) = text_segment(SearchSegmentKind::UserText, &content.content) {
                    projection.segments.push(segment);
                }
            }
            UserContent::Blocks(content) => {
                for block in &content.content {
                    push_content_block(&mut projection, block, SearchSegmentKind::UserText);
                }
            }
        },
        LogEntry::Assistant(assistant) => {
            for block in &assistant.message.content {
                push_content_block(&mut projection, block, SearchSegmentKind::AssistantText);
            }
        }
        LogEntry::System(system) => {
            if let Some(content) = system
                .content
                .as_deref()
                .and_then(|text| text_segment(SearchSegmentKind::SystemText, text))
            {
                projection.segments.push(content);
            }
        }
        LogEntry::Summary(summary) => {
            if let Some(segment) = text_segment(SearchSegmentKind::SummaryText, &summary.summary) {
                projection.segments.push(segment);
            }
        }
        LogEntry::Unknown(_) => {
            projection.coverage.unknown_entries_omitted = 1;
        }
        _ => {}
    }
    projection
}

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
    #![allow(clippy::trivial_regex)]
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
        assert_eq!(
            snap_char_boundary_right(s, emoji_start + 1),
            emoji_start + 4
        );
    }

    #[test]
    fn search_projection_preserves_order_identity_and_tool_metadata() {
        let entry: LogEntry = serde_json::from_value(serde_json::json!({
            "type": "assistant",
            "uuid": "assistant-1",
            "timestamp": "2026-07-22T00:00:00Z",
            "sessionId": "session-1",
            "version": "2.1.193",
            "message": {
                "id": "message-1",
                "type": "message",
                "role": "assistant",
                "model": "test-model",
                "content": [
                    {"type": "text", "text": "repeat"},
                    {"type": "text", "text": "repeat"},
                    {"type": "thinking", "thinking": "reason", "signature": "sig"},
                    {"type": "tool_use", "id": "call-1", "name": "Read", "input": {"file_path": "src/lib.rs"}},
                    {"type": "tool_result", "tool_use_id": "call-1", "content": "result", "is_error": false}
                ]
            }
        }))
        .unwrap();

        let projection = project_entry_for_search(&entry);
        assert_eq!(projection.segments.len(), 5);
        assert_eq!(
            projection.segments[0].kind,
            SearchSegmentKind::AssistantText
        );
        assert_eq!(
            projection.segments[1].kind,
            SearchSegmentKind::AssistantText
        );
        assert_eq!(projection.segments[0].text, "repeat");
        assert_eq!(projection.segments[1].text, "repeat");
        assert_eq!(projection.segments[2].kind, SearchSegmentKind::Reasoning);
        assert_eq!(projection.segments[3].kind, SearchSegmentKind::ToolInput);
        assert_eq!(projection.segments[3].tool_name.as_deref(), Some("Read"));
        assert_eq!(
            projection.segments[3].tool_call_id.as_deref(),
            Some("call-1")
        );
        assert_eq!(projection.segments[4].kind, SearchSegmentKind::ToolResult);
        assert_eq!(projection.segments[4].tool_is_error, Some(false));
        assert_eq!(projection.coverage, SearchProjectionCoverage::default());
    }

    #[test]
    fn search_projection_includes_user_tool_results_without_indexing_image_payloads() {
        let secret_base64 = "A".repeat(512);
        let entry: LogEntry = serde_json::from_value(serde_json::json!({
            "type": "user",
            "uuid": "user-1",
            "timestamp": "2026-07-22T00:00:00Z",
            "sessionId": "session-1",
            "version": "2.1.193",
            "message": {
                "role": "user",
                "content": [
                    {"type": "text", "text": "prompt"},
                    {
                        "type": "tool_result",
                        "tool_use_id": "call-1",
                        "is_error": true,
                        "content": [{
                            "type": "image",
                            "source": {"type": "base64", "media_type": "image/png", "data": secret_base64}
                        }]
                    },
                    {
                        "type": "image",
                        "source": {"type": "url", "url": "https://example.invalid/image.png"}
                    },
                    {"type": "future_block", "payload": "preserved outside search"}
                ]
            }
        }))
        .unwrap();

        let projection = project_entry_for_search(&entry);
        assert_eq!(projection.segments.len(), 2);
        assert_eq!(projection.segments[0].kind, SearchSegmentKind::UserText);
        assert_eq!(projection.segments[1].kind, SearchSegmentKind::ToolResult);
        assert_eq!(
            projection.segments[1].tool_call_id.as_deref(),
            Some("call-1")
        );
        assert_eq!(projection.segments[1].tool_is_error, Some(true));
        assert!(projection.segments[1].text.contains("base64 image omitted"));
        assert!(!projection.segments[1].text.contains(&"A".repeat(128)));
        assert_eq!(projection.coverage.images_omitted, 1);
        assert_eq!(projection.coverage.unknown_blocks_omitted, 1);
        assert_eq!(projection.coverage.unknown_entries_omitted, 0);
    }

    #[test]
    fn search_projection_reports_unknown_entry_coverage() {
        let entry: LogEntry = serde_json::from_value(serde_json::json!({
            "type": "future-entry",
            "uuid": "unknown-1",
            "content": "not silently claimed as searchable"
        }))
        .unwrap();
        let projection = project_entry_for_search(&entry);
        assert!(projection.segments.is_empty());
        assert_eq!(projection.coverage.unknown_entries_omitted, 1);
    }
}
