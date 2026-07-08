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
///
/// Also returns `false` for tool-result turns. A user-role message that
/// carries a `tool_result` block (e.g. a `ToolSearch` result) may be paired
/// with a synthetic harness acknowledgement like `"Tool loaded."`; that text
/// is not human-authored, so such turns are not counted as prompts.
///
/// Also returns `false` for compaction continuation summaries
/// (`isCompactSummary: true`), which are harness-injected "This session is
/// being continued from a previous conversation..." entries, not human input,
/// and for harness meta entries (`isMeta: true`).
pub fn is_human_prompt(entry: &LogEntry) -> bool {
    if let LogEntry::User(user) = entry {
        // Harness-injected meta entries (isMeta: true) are never human prompts.
        if user.is_meta == Some(true) {
            return false;
        }
        if user.is_compact_summary == Some(true) {
            return false;
        }
        if user.message.has_tool_results() {
            return false;
        }
    }
    let text = match extract_user_prompt_text(entry) {
        Some(t) => t,
        None => return false,
    };
    !is_noise_text(&text)
}

/// Check if text content is system-generated noise rather than human input.
pub fn is_noise_text(text: &str) -> bool {
    let trimmed = text.trim();

    // XML-tagged system messages
    if trimmed.starts_with('<') {
        let noise_tags = [
            "<local-command-caveat>",
            "<local-command-stdout>",
            "<local-command-stderr>",
            "<command-name>",
            "<system-reminder>",
            "<task-notification>",
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
                        ContentBlock::Text(t) if !t.text.trim().is_empty() => Some(t.text.trim()),
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

/// Extract the user-typed prompt from a `queued_command` attachment.
///
/// Queued commands with `commandMode: "prompt"` (or an explicit human origin)
/// carry real user input that may never appear as a `user` entry; other modes
/// (e.g. `task-notification`) are machine-generated and stay marker-only.
pub fn queued_human_prompt(entry: &LogEntry) -> Option<&str> {
    let LogEntry::Attachment(att) = entry else {
        return None;
    };
    let payload = att.attachment.as_ref()?;
    if payload.get("type").and_then(|v| v.as_str()) != Some("queued_command") {
        return None;
    }
    let mode = payload.get("commandMode").and_then(|v| v.as_str());
    let human_origin = payload
        .get("origin")
        .and_then(|o| o.get("kind"))
        .and_then(|v| v.as_str())
        == Some("human");
    if mode == Some("prompt") || human_origin {
        payload.get("prompt").and_then(|v| v.as_str())
    } else {
        None
    }
}

/// Check whether an entry opens a prompt-boundary chunk.
///
/// True for human prompts delivered as `user` entries ([`is_human_prompt`])
/// and for mid-turn steering prompts, which exist only as `queued_command`
/// attachments ([`queued_human_prompt`]) — 86% of queued human prompts never
/// appear as a `user` entry. Chunking and `detail=overview` share this
/// predicate so chunk indices and overview prompt listings never drift.
pub fn is_prompt_boundary(entry: &LogEntry) -> bool {
    is_human_prompt(entry) || queued_human_prompt(entry).is_some()
}

/// The prompt text of a boundary entry (typed prompt or queued steering
/// prompt). `None` for entries that are not prompt boundaries.
pub fn boundary_prompt_text(entry: &LogEntry) -> Option<String> {
    if is_human_prompt(entry) {
        return extract_user_prompt_text(entry);
    }
    queued_human_prompt(entry).map(str::to_string)
}

/// Render a human-readable marker for an attachment log entry.
///
/// Every attachment yields a `[attachment: <type>]` marker so a reader knows
/// injected context existed. Content-bearing kinds — injected files (`file`),
/// edited-file snippets (`edited_text_file`), and queued human prompts —
/// additionally surface their payload, truncated to `max_len`. Operational
/// kinds (hook output, skill/tool listings, metadata) are marker-only to
/// avoid flooding the transcript. Returns `None` if the entry is not an
/// attachment.
pub fn render_attachment_content(entry: &LogEntry, max_len: usize) -> Option<String> {
    let LogEntry::Attachment(att) = entry else {
        return None;
    };
    let payload = att.attachment.as_ref();
    let kind = payload
        .and_then(|p| p.get("type"))
        .and_then(|v| v.as_str())
        .unwrap_or("unknown");
    let marker = format!("[attachment: {kind}]");

    let detail = match kind {
        "file" => payload.and_then(|p| {
            let path = p
                .get("displayPath")
                .or_else(|| p.get("filename"))
                .and_then(|v| v.as_str());
            let content = file_attachment_text(p.get("content"));
            render_path_and_body(path, content.as_deref(), max_len)
        }),
        "edited_text_file" => payload.and_then(|p| {
            let path = p.get("filename").and_then(|v| v.as_str());
            let snippet = p.get("snippet").and_then(|v| v.as_str());
            render_path_and_body(path, snippet, max_len)
        }),
        "queued_command" => queued_human_prompt(entry)
            .map(|prompt| format!("(queued user input) {}", truncate_text(prompt, max_len))),
        _ => None,
    };

    Some(match detail {
        Some(d) => format!("{marker} {d}"),
        None => marker,
    })
}

/// Extract the injected file text from a `file` attachment's `content`, which
/// may be stored as a plain string or as a `{file: {content: ...}}` object.
fn file_attachment_text(content: Option<&serde_json::Value>) -> Option<String> {
    let content = content?;
    if let Some(s) = content.as_str() {
        return Some(s.to_string());
    }
    content
        .get("file")
        .and_then(|f| f.get("content"))
        .and_then(|v| v.as_str())
        .map(str::to_string)
}

/// Join an optional path with an optional body (truncated), for attachment rendering.
fn render_path_and_body(path: Option<&str>, body: Option<&str>, max_len: usize) -> Option<String> {
    match (path, body) {
        (Some(path), Some(body)) => Some(format!("{path}\n{}", truncate_text(body, max_len))),
        (Some(path), None) => Some(path.to_string()),
        (None, Some(body)) => Some(truncate_text(body, max_len)),
        (None, None) => None,
    }
}

/// Collect `[image: <media_type>]` placeholders for any top-level image blocks
/// in a user message, so a pasted image is visible rather than silently dropped.
///
/// Returns an empty vector if the entry is not a user message or has no image
/// blocks. Image bytes are never rendered, consistent with the codebase's
/// image-dropping convention.
pub fn extract_image_placeholders(entry: &LogEntry) -> Vec<String> {
    let LogEntry::User(user) = entry else {
        return Vec::new();
    };
    let UserContent::Blocks(blocks) = &user.message else {
        return Vec::new();
    };
    blocks
        .content
        .iter()
        .filter_map(|b| match b {
            ContentBlock::Image(img) => Some(format!(
                "[image: {}]",
                img.source.media_type().unwrap_or("image")
            )),
            _ => None,
        })
        .collect()
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
        "Read" => {
            if let Some(fp) = obj.get("file_path").and_then(|v| v.as_str()) {
                summary.insert("file_path".into(), fp.to_string());
            }
        }
        "Write" => {
            if let Some(fp) = obj.get("file_path").and_then(|v| v.as_str()) {
                summary.insert("file_path".into(), fp.to_string());
            }
            if let Some(content) = obj.get("content").and_then(|v| v.as_str()) {
                summary.insert("content".into(), truncate_text(content, 500));
            }
        }
        "Edit" => {
            if let Some(fp) = obj.get("file_path").and_then(|v| v.as_str()) {
                summary.insert("file_path".into(), fp.to_string());
            }
            if let Some(old) = obj.get("old_string").and_then(|v| v.as_str()) {
                summary.insert("old_string".into(), truncate_text(old, 200));
            }
            if let Some(new) = obj.get("new_string").and_then(|v| v.as_str()) {
                summary.insert("new_string".into(), truncate_text(new, 200));
            }
        }
        "MultiEdit" => {
            if let Some(fp) = obj.get("file_path").and_then(|v| v.as_str()) {
                summary.insert("file_path".into(), fp.to_string());
            }
            if let Some(edits) = obj.get("edits").and_then(|v| v.as_array()) {
                summary.insert("edit_count".into(), edits.len().to_string());
                if let Some(first) = edits.first() {
                    if let Some(old) = first.get("old_string").and_then(|v| v.as_str()) {
                        summary.insert("old_string".into(), truncate_text(old, 200));
                    }
                    if let Some(new) = first.get("new_string").and_then(|v| v.as_str()) {
                        summary.insert("new_string".into(), truncate_text(new, 200));
                    }
                }
            }
        }
        "TodoWrite" => {
            if let Some(todos) = obj.get("todos").and_then(|v| v.as_array()) {
                let rendered: Vec<String> = todos
                    .iter()
                    .filter_map(|t| {
                        let content = t.get("content").and_then(|v| v.as_str())?;
                        let status = t
                            .get("status")
                            .and_then(|v| v.as_str())
                            .unwrap_or("pending");
                        Some(format!("[{status}] {content}"))
                    })
                    .collect();
                if !rendered.is_empty() {
                    summary.insert("todos".into(), truncate_text(&rendered.join("; "), 500));
                }
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
        "Agent" | "Task" => {
            if let Some(desc) = obj.get("description").and_then(|v| v.as_str()) {
                summary.insert("description".into(), truncate_text(desc, 100));
            }
            if let Some(st) = obj.get("subagent_type").and_then(|v| v.as_str()) {
                summary.insert("subagent_type".into(), st.to_string());
            }
            if let Some(prompt) = obj.get("prompt").and_then(|v| v.as_str()) {
                summary.insert("prompt".into(), truncate_text(prompt, 500));
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
                        if let Some(fp) = tool_use.input.get("file_path").and_then(|v| v.as_str()) {
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

/// Detect the thinking-redaction pattern of recent Claude Code versions:
/// thinking blocks present but every one has empty text (only the encrypted
/// signature is persisted to the session log).
///
/// Returns a human-readable note when `entries` contain at least one thinking
/// block and all of them are empty; `None` when there are no thinking blocks
/// or at least one carries text.
pub fn thinking_redaction_note(entries: &[&LogEntry]) -> Option<String> {
    let mut total = 0usize;
    for entry in entries {
        if let LogEntry::Assistant(assistant) = entry {
            for block in assistant.message.thinking_blocks() {
                if !block.thinking.trim().is_empty() {
                    return None;
                }
                total += 1;
            }
        }
    }
    (total > 0).then(|| {
        format!(
            "{total} thinking block(s) present but all empty — this session's Claude Code version does not persist thinking text, so thinking recovery is unavailable"
        )
    })
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

/// Extract a plain-text preview of a tool result's content, for both success
/// and error results.
///
/// Returns `None` when the result carries no content (or only non-text content).
/// Extracts plain text from the content (not Debug format).
pub fn extract_result_preview(
    result: &crate::model::content::ToolResult,
    max_len: usize,
) -> Option<String> {
    let content = result.content.as_ref()?;
    let text = match content {
        crate::model::content::ToolResultContent::String(s) => s.clone(),
        crate::model::content::ToolResultContent::Array(arr) => arr
            .iter()
            .filter_map(|v| v.get("text").and_then(|t| t.as_str()))
            .collect::<Vec<_>>()
            .join("\n"),
    };
    let trimmed = text.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(truncate_text(trimmed, max_len))
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
            if matches!(
                sys.subtype,
                Some(SystemSubtype::CompactBoundary | SystemSubtype::MicrocompactBoundary)
            ) {
                let ts = sys.timestamp.to_rfc3339();
                let summary = sys.content.clone();
                events.push((ts, summary));
            }
        }
    }
    events
}

/// Detect error-level system events (e.g. API errors) in a conversation's
/// main thread entries.
///
/// These sit on the main thread but are not conversation turns, so `turns()`
/// (and timelines built from it) skip them. Surface them separately like
/// compaction events. Returns `(timestamp_rfc3339, message)` pairs.
pub fn find_error_events(entries: &[&LogEntry]) -> Vec<(String, String)> {
    let mut events = Vec::new();
    for entry in entries {
        if let LogEntry::System(sys) = entry {
            let is_error = sys.subtype == Some(SystemSubtype::ApiError)
                || sys.level.as_deref() == Some("error");
            if is_error {
                let ts = sys.timestamp.to_rfc3339();
                let msg = sys
                    .content
                    .clone()
                    .or_else(|| sys.error.as_ref().map(std::string::ToString::to_string))
                    .unwrap_or_else(|| "(error)".to_string());
                events.push((ts, msg));
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
    fn test_find_compaction_events_includes_microcompact() {
        let full = r#"{"type":"system","subtype":"compact_boundary","uuid":"c1","timestamp":"2026-06-21T00:00:00Z","sessionId":"s","content":"full compact"}"#;
        let micro = r#"{"type":"system","subtype":"microcompact_boundary","uuid":"c2","timestamp":"2026-06-21T01:00:00Z","sessionId":"s","content":"micro compact"}"#;
        let full_entry: LogEntry = serde_json::from_str(full).unwrap();
        let micro_entry: LogEntry = serde_json::from_str(micro).unwrap();
        let refs: Vec<&LogEntry> = vec![&full_entry, &micro_entry];
        assert_eq!(find_compaction_events(&refs).len(), 2);
    }

    #[test]
    fn test_find_error_events_surfaces_api_error() {
        let json = r#"{"type":"system","subtype":"api_error","uuid":"e1","timestamp":"2026-06-21T00:00:00Z","sessionId":"s","level":"error","content":"overloaded_error"}"#;
        let entry: LogEntry = serde_json::from_str(json).unwrap();
        let refs: Vec<&LogEntry> = vec![&entry];
        let events = find_error_events(&refs);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].1, "overloaded_error");
    }

    #[test]
    fn test_find_error_events_ignores_non_errors() {
        let json = r#"{"type":"system","subtype":"turn_duration","uuid":"t1","timestamp":"2026-06-21T00:00:00Z","sessionId":"s","durationMs":100}"#;
        let entry: LogEntry = serde_json::from_str(json).unwrap();
        let refs: Vec<&LogEntry> = vec![&entry];
        assert!(find_error_events(&refs).is_empty());
    }

    #[test]
    fn test_queued_human_prompt_rendered() {
        // commandMode "prompt" = real user input → body surfaced
        let human = r#"{"uuid":"1","type":"attachment","timestamp":"2026-01-01T00:00:00Z","sessionId":"s","attachment":{"type":"queued_command","commandMode":"prompt","origin":{"kind":"human"},"prompt":"remember to add an exclude option"}}"#;
        let entry: LogEntry = serde_json::from_str(human).unwrap();
        assert_eq!(
            queued_human_prompt(&entry),
            Some("remember to add an exclude option")
        );
        let rendered = render_attachment_content(&entry, 200).unwrap();
        assert!(rendered.contains("queued user input"));
        assert!(rendered.contains("exclude option"));

        // task-notification = machine-generated → marker only
        let notif = r#"{"uuid":"2","type":"attachment","timestamp":"2026-01-01T00:00:00Z","sessionId":"s","attachment":{"type":"queued_command","commandMode":"task-notification","prompt":"<task-notification>...</task-notification>"}}"#;
        let entry: LogEntry = serde_json::from_str(notif).unwrap();
        assert_eq!(queued_human_prompt(&entry), None);
        assert_eq!(
            render_attachment_content(&entry, 200).unwrap(),
            "[attachment: queued_command]"
        );
    }

    #[test]
    fn test_thinking_redaction_note() {
        let empty = r#"{"type":"assistant","uuid":"a1","timestamp":"2026-07-01T00:00:00Z","sessionId":"s","parentUuid":null,"isSidechain":false,"userType":"external","cwd":"/","version":"2.1.198","gitBranch":"main","message":{"id":"m1","type":"message","role":"assistant","model":"claude-opus-4-8","content":[{"type":"thinking","thinking":"","signature":"sig"}]}}"#;
        let full = r#"{"type":"assistant","uuid":"a2","timestamp":"2026-07-01T00:00:01Z","sessionId":"s","parentUuid":"a1","isSidechain":false,"userType":"external","cwd":"/","version":"2.1.42","gitBranch":"main","message":{"id":"m2","type":"message","role":"assistant","model":"claude-opus-4-8","content":[{"type":"thinking","thinking":"real reasoning","signature":"sig"}]}}"#;
        let empty_entry: LogEntry = serde_json::from_str(empty).unwrap();
        let full_entry: LogEntry = serde_json::from_str(full).unwrap();

        // All-empty thinking → note
        let note = thinking_redaction_note(&[&empty_entry]);
        assert!(note.is_some());
        assert!(note.unwrap().contains("1 thinking block"));

        // Any non-empty thinking → no note
        assert!(thinking_redaction_note(&[&empty_entry, &full_entry]).is_none());

        // No thinking blocks at all → no note
        assert!(thinking_redaction_note(&[]).is_none());
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
        assert_eq!(summary.get("file_path").unwrap(), "/home/user/src/main.rs");
        assert_eq!(summary.get("content").unwrap(), "fn main() {}");
    }

    #[test]
    fn test_extract_tool_input_summary_edit() {
        let input = serde_json::json!({
            "file_path": "/home/user/src/main.rs",
            "old_string": "let x = 1;",
            "new_string": "let x = 2;"
        });
        let summary = extract_tool_input_summary("Edit", &input);
        assert_eq!(summary.get("file_path").unwrap(), "/home/user/src/main.rs");
        assert_eq!(summary.get("old_string").unwrap(), "let x = 1;");
        assert_eq!(summary.get("new_string").unwrap(), "let x = 2;");
    }

    #[test]
    fn test_extract_tool_input_summary_multiedit() {
        let input = serde_json::json!({
            "file_path": "/home/user/src/main.rs",
            "edits": [
                {"old_string": "a", "new_string": "b"},
                {"old_string": "c", "new_string": "d"}
            ]
        });
        let summary = extract_tool_input_summary("MultiEdit", &input);
        assert_eq!(summary.get("file_path").unwrap(), "/home/user/src/main.rs");
        assert_eq!(summary.get("edit_count").unwrap(), "2");
        assert_eq!(summary.get("old_string").unwrap(), "a");
        assert_eq!(summary.get("new_string").unwrap(), "b");
    }

    #[test]
    fn test_extract_tool_input_summary_todowrite() {
        let input = serde_json::json!({
            "todos": [
                {"content": "first task", "status": "completed"},
                {"content": "second task", "status": "in_progress"}
            ]
        });
        let summary = extract_tool_input_summary("TodoWrite", &input);
        let todos = summary.get("todos").unwrap();
        assert!(todos.contains("[completed] first task"));
        assert!(todos.contains("[in_progress] second task"));
    }

    #[test]
    fn test_extract_tool_input_summary_grep() {
        let input = serde_json::json!({"pattern": "fn main", "path": "/src"});
        let summary = extract_tool_input_summary("Grep", &input);
        assert_eq!(summary.get("pattern").unwrap(), "fn main");
        assert_eq!(summary.get("path").unwrap(), "/src");
    }

    #[test]
    fn test_extract_tool_input_summary_agent() {
        let input = serde_json::json!({
            "subagent_type": "Explore",
            "description": "Tests, versioning, release hygiene",
            "prompt": "Inspect the repository at /tmp/rust-mssql-driver."
        });
        let summary = extract_tool_input_summary("Agent", &input);
        assert_eq!(summary.get("subagent_type").unwrap(), "Explore");
        assert_eq!(
            summary.get("description").unwrap(),
            "Tests, versioning, release hygiene"
        );
        assert_eq!(
            summary.get("prompt").unwrap(),
            "Inspect the repository at /tmp/rust-mssql-driver."
        );
        // "Task" is the older alias for the same tool.
        let task_summary = extract_tool_input_summary("Task", &input);
        assert_eq!(task_summary.get("subagent_type").unwrap(), "Explore");
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
        assert!(is_noise_text(
            "<local-command-caveat>Caveat: blah</local-command-caveat>"
        ));
        assert!(is_noise_text(
            "<local-command-stdout>Reconnected to snatch.</local-command-stdout>"
        ));
        assert!(is_noise_text(
            "<local-command-stderr>error output</local-command-stderr>"
        ));
        assert!(is_noise_text("<command-name>/compact</command-name>"));
        assert!(is_noise_text(
            "<system-reminder>Some reminder</system-reminder>"
        ));
    }

    #[test]
    fn test_is_noise_text_interrupt() {
        assert!(is_noise_text("[Request interrupted by user]"));
        assert!(is_noise_text("[Request interrupted by user for tool use]"));
    }

    #[test]
    fn test_task_notification_is_not_human_prompt() {
        // A background-task completion notification is harness-initiated
        // (isMeta is absent on these entries), not human input.
        let line = r#"{"uuid":"1","parentUuid":null,"type":"user","timestamp":"2026-01-01T00:00:00Z","sessionId":"s","version":"2.0","isSidechain":false,"message":{"role":"user","content":"<task-notification>\n<task-id>b4t2cw2zp</task-id>\n<status>completed</status>\n</task-notification>"}}"#;
        let entry: LogEntry = serde_json::from_str(line).unwrap();
        assert!(!is_human_prompt(&entry));
    }

    #[test]
    fn test_tool_result_turn_is_not_human_prompt() {
        // A user-role turn carrying a tool_result plus a synthetic
        // "Tool loaded." acknowledgement is not a human prompt.
        let line = r#"{"uuid":"1","parentUuid":null,"type":"user","timestamp":"2026-01-01T00:00:00Z","sessionId":"s","version":"2.0","isSidechain":false,"message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"t1","content":[{"type":"tool_reference","tool_name":"Read"}]},{"type":"text","text":"Tool loaded."}]}}"#;
        let entry: LogEntry = serde_json::from_str(line).unwrap();
        assert!(!is_human_prompt(&entry));

        // A plain user-typed message is still a human prompt.
        let line2 = r#"{"uuid":"2","parentUuid":null,"type":"user","timestamp":"2026-01-01T00:00:01Z","sessionId":"s","version":"2.0","isSidechain":false,"message":{"role":"user","content":"fix the parser"}}"#;
        let entry2: LogEntry = serde_json::from_str(line2).unwrap();
        assert!(is_human_prompt(&entry2));
    }

    #[test]
    fn test_compact_summary_is_not_human_prompt() {
        // A user-role entry flagged isCompactSummary is a harness-injected
        // continuation summary, not a human prompt.
        let line = r#"{"uuid":"1","parentUuid":null,"type":"user","timestamp":"2026-01-01T00:00:00Z","sessionId":"s","version":"2.0","isSidechain":false,"isCompactSummary":true,"isVisibleInTranscriptOnly":true,"message":{"role":"user","content":"This session is being continued from a previous conversation that ran out of context."}}"#;
        let entry: LogEntry = serde_json::from_str(line).unwrap();
        // The flag deserializes into the typed field.
        if let LogEntry::User(user) = &entry {
            assert_eq!(user.is_compact_summary, Some(true));
        } else {
            panic!("expected user entry");
        }
        assert!(!is_human_prompt(&entry));
    }

    #[test]
    fn test_render_attachment_content_file_surfaces_payload() {
        let line = r#"{"uuid":"1","type":"attachment","timestamp":"2026-01-01T00:00:00Z","sessionId":"s","attachment":{"type":"file","displayPath":"../CLAUDE.md","content":"hello world"}}"#;
        let entry: LogEntry = serde_json::from_str(line).unwrap();
        let rendered = render_attachment_content(&entry, 100).unwrap();
        assert!(rendered.starts_with("[attachment: file]"));
        assert!(rendered.contains("../CLAUDE.md"));
        assert!(rendered.contains("hello world"));
    }

    #[test]
    fn test_render_attachment_content_file_nested_object_payload() {
        // `file` content is sometimes a {file: {content: ...}} object, not a string.
        let line = r#"{"uuid":"1","type":"attachment","timestamp":"2026-01-01T00:00:00Z","sessionId":"s","attachment":{"type":"file","displayPath":"../foo.md","content":{"type":"text","file":{"filePath":"/tmp/foo.md","content":"nested body"}}}}"#;
        let entry: LogEntry = serde_json::from_str(line).unwrap();
        let rendered = render_attachment_content(&entry, 100).unwrap();
        assert!(rendered.starts_with("[attachment: file]"));
        assert!(rendered.contains("../foo.md"));
        assert!(rendered.contains("nested body"));
    }

    #[test]
    fn test_render_attachment_content_hook_is_marker_only() {
        // Operational noise (hook output) gets a marker but no payload dump.
        let line = r#"{"uuid":"1","type":"attachment","timestamp":"2026-01-01T00:00:00Z","sessionId":"s","attachment":{"type":"hook_success","stdout":"lots of noise","content":"lots of noise"}}"#;
        let entry: LogEntry = serde_json::from_str(line).unwrap();
        let rendered = render_attachment_content(&entry, 100).unwrap();
        assert_eq!(rendered, "[attachment: hook_success]");
    }

    #[test]
    fn test_render_attachment_content_non_attachment_is_none() {
        let line = r#"{"uuid":"2","parentUuid":null,"type":"user","timestamp":"2026-01-01T00:00:01Z","sessionId":"s","version":"2.0","isSidechain":false,"message":{"role":"user","content":"hi"}}"#;
        let entry: LogEntry = serde_json::from_str(line).unwrap();
        assert!(render_attachment_content(&entry, 100).is_none());
    }

    #[test]
    fn test_extract_image_placeholders() {
        let line = r#"{"uuid":"1","parentUuid":null,"type":"user","timestamp":"2026-01-01T00:00:00Z","sessionId":"s","version":"2.0","isSidechain":false,"message":{"role":"user","content":[{"type":"image","source":{"type":"base64","media_type":"image/png","data":"abc"}},{"type":"text","text":"look"}]}}"#;
        let entry: LogEntry = serde_json::from_str(line).unwrap();
        assert_eq!(
            extract_image_placeholders(&entry),
            vec!["[image: image/png]"]
        );

        // A text-only message has no image placeholders.
        let line2 = r#"{"uuid":"2","parentUuid":null,"type":"user","timestamp":"2026-01-01T00:00:01Z","sessionId":"s","version":"2.0","isSidechain":false,"message":{"role":"user","content":"plain text"}}"#;
        let entry2: LogEntry = serde_json::from_str(line2).unwrap();
        assert!(extract_image_placeholders(&entry2).is_empty());
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

    #[test]
    fn test_extract_result_preview_string() {
        let r: crate::model::content::ToolResult =
            serde_json::from_str(r#"{"tool_use_id":"t1","content":"hello world"}"#).unwrap();
        assert_eq!(
            extract_result_preview(&r, 100).as_deref(),
            Some("hello world")
        );
    }

    #[test]
    fn test_extract_result_preview_array_joins_text() {
        let r: crate::model::content::ToolResult = serde_json::from_str(
            r#"{"tool_use_id":"t1","content":[{"type":"text","text":"part one"},{"type":"text","text":"part two"}]}"#,
        )
        .unwrap();
        assert_eq!(
            extract_result_preview(&r, 100).as_deref(),
            Some("part one\npart two")
        );
    }

    #[test]
    fn test_extract_result_preview_extracts_success_and_error() {
        // Unlike extract_error_preview, this returns content for both states.
        let err: crate::model::content::ToolResult =
            serde_json::from_str(r#"{"tool_use_id":"t1","content":"boom","is_error":true}"#)
                .unwrap();
        assert_eq!(extract_result_preview(&err, 100).as_deref(), Some("boom"));
        // The error-only helper still bails on success.
        let ok: crate::model::content::ToolResult =
            serde_json::from_str(r#"{"tool_use_id":"t1","content":"ok"}"#).unwrap();
        assert!(extract_error_preview(&ok, 100).is_none());
        assert_eq!(extract_result_preview(&ok, 100).as_deref(), Some("ok"));
    }

    #[test]
    fn test_extract_result_preview_absent_content_is_none() {
        let r: crate::model::content::ToolResult =
            serde_json::from_str(r#"{"tool_use_id":"t1"}"#).unwrap();
        assert!(extract_result_preview(&r, 100).is_none());
    }
}
