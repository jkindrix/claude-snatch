//! Plain text export for conversations.
//!
//! Generates clean, readable plain text output with configurable
//! line width and ASCII formatting options.

use std::io::Write;

use chrono::{DateTime, Utc};

use super::tool_render::{self, ToolInputView};
use crate::analytics::SessionAnalytics;
use crate::error::Result;
use crate::model::{
    content::{ThinkingBlock, ToolResult, ToolUse},
    AssistantMessage, ContentBlock, LogEntry, SummaryMessage, SystemMessage, UserMessage,
};
use crate::reconstruction::Conversation;

use super::{ExportOptions, Exporter};

/// Plain text exporter for conversations.
#[derive(Debug, Clone)]
pub struct TextExporter {
    /// Maximum line width (0 = no wrapping).
    line_width: usize,
    /// Use ASCII separators.
    use_separators: bool,
    /// Include line numbers.
    line_numbers: bool,
    /// Separator character.
    separator_char: char,
}

impl Default for TextExporter {
    fn default() -> Self {
        Self::new()
    }
}

impl TextExporter {
    /// Create a new text exporter.
    #[must_use]
    pub fn new() -> Self {
        Self {
            line_width: 80,
            use_separators: true,
            line_numbers: false,
            separator_char: '-',
        }
    }

    /// Set the line width (0 = no wrapping).
    #[must_use]
    pub fn with_line_width(mut self, width: usize) -> Self {
        self.line_width = width;
        self
    }

    /// Enable or disable separators.
    #[must_use]
    pub fn with_separators(mut self, use_sep: bool) -> Self {
        self.use_separators = use_sep;
        self
    }

    /// Enable or disable line numbers.
    #[must_use]
    pub fn with_line_numbers(mut self, enable: bool) -> Self {
        self.line_numbers = enable;
        self
    }

    /// Set separator character.
    #[must_use]
    pub fn with_separator_char(mut self, c: char) -> Self {
        self.separator_char = c;
        self
    }

    /// Write a separator line.
    fn write_separator<W: Write>(&self, writer: &mut W) -> Result<()> {
        if self.use_separators {
            let width = if self.line_width > 0 {
                self.line_width
            } else {
                60
            };
            let sep: String = std::iter::repeat(self.separator_char).take(width).collect();
            writeln!(writer, "{sep}")?;
        }
        Ok(())
    }

    /// Wrap text to the configured line width.
    fn wrap_text(&self, text: &str) -> String {
        if self.line_width == 0 {
            return text.to_string();
        }

        let mut result = String::new();
        for line in text.lines() {
            if line.len() <= self.line_width {
                result.push_str(line);
                result.push('\n');
            } else {
                // Simple word wrapping
                let mut current_line = String::new();
                for word in line.split_whitespace() {
                    if current_line.is_empty() {
                        current_line = word.to_string();
                    } else if current_line.len() + 1 + word.len() <= self.line_width {
                        current_line.push(' ');
                        current_line.push_str(word);
                    } else {
                        result.push_str(&current_line);
                        result.push('\n');
                        current_line = word.to_string();
                    }
                }
                if !current_line.is_empty() {
                    result.push_str(&current_line);
                    result.push('\n');
                }
            }
        }
        result
    }

    /// Write the session header.
    fn write_header<W: Write>(
        &self,
        writer: &mut W,
        conversation: &Conversation,
        options: &ExportOptions,
    ) -> Result<()> {
        let analytics = SessionAnalytics::from_conversation(conversation);

        self.write_separator(writer)?;
        writeln!(writer, "CLAUDE CODE CONVERSATION")?;
        self.write_separator(writer)?;
        writeln!(writer)?;

        // Session info
        if options.include_metadata {
            if let Some(first_entry) = conversation.main_thread_entries().first() {
                if let Some(session_id) = first_entry.session_id() {
                    writeln!(writer, "Session ID: {session_id}")?;
                }
                if let Some(version) = first_entry.version() {
                    writeln!(writer, "Claude Code Version: {version}")?;
                }
            }
        }

        // Timestamps
        if options.include_timestamps {
            if let Some(start) = analytics.start_time {
                writeln!(writer, "First Record: {}", format_timestamp(&start))?;
            }
            if let Some(end) = analytics.end_time {
                writeln!(writer, "Last Record: {}", format_timestamp(&end))?;
            }
            if let Some(span) = analytics.duration_string() {
                writeln!(writer, "Transcript Span: {span}")?;
            }
            if let Some(last_turn_end) = analytics.last_turn_end_time {
                writeln!(
                    writer,
                    "Last Turn Ended: {}",
                    format_timestamp(&last_turn_end)
                )?;
            }
            if let Some(reported) = analytics.reported_turn_duration_string() {
                writeln!(writer, "Reported Turn Time: {reported}")?;
            }
        }

        // Usage statistics
        if options.include_usage {
            let summary = analytics.summary_report();
            writeln!(writer)?;
            writeln!(writer, "STATISTICS:")?;
            writeln!(
                writer,
                "  Messages (logical): {} ({} user, {} assistant)",
                summary.total_messages, summary.user_messages, summary.assistant_messages
            )?;
            writeln!(
                writer,
                "  Work Tokens: {} (uncached input + cache creation + output)",
                summary.total_tokens
            )?;
            writeln!(
                writer,
                "  Total Processed Tokens: {} (work + cache reads)",
                summary.total_processed_tokens
            )?;
            writeln!(writer, "  Input (uncached): {}", summary.input_tokens)?;
            writeln!(
                writer,
                "  Cache Creation: {}",
                summary.cache_creation_tokens
            )?;
            writeln!(writer, "  Cache Read: {}", summary.cache_read_tokens)?;
            writeln!(writer, "  Output Tokens: {}", summary.output_tokens)?;
            if summary.cache_hit_rate > 0.0 {
                writeln!(writer, "  Cache Hit Rate: {:.1}%", summary.cache_hit_rate)?;
            }
            writeln!(writer, "  Tool Invocations: {}", summary.tool_invocations)?;
            if summary.thinking_blocks > 0 {
                writeln!(writer, "  Thinking Blocks: {}", summary.thinking_blocks)?;
            }
            if let Some(model) = &summary.primary_model {
                writeln!(writer, "  Primary Model: {model}")?;
            }
            writeln!(
                writer,
                "  Estimated API List Cost: {}",
                summary.cost_string()
            )?;
            if !analytics.usage.pricing_rate_cards.is_empty() {
                writeln!(
                    writer,
                    "  Cost Rate Cards: {}",
                    analytics.usage.pricing_rate_cards.join(", ")
                )?;
                writeln!(
                    writer,
                    "  Cost Source: {}",
                    crate::model::ModelPricing::source_summary()
                )?;
            }
            if !summary.unpriced_models.is_empty() {
                let coverage = if summary.estimated_cost.is_some() {
                    "Partial; excluded models"
                } else {
                    "Unavailable; no verified rate for models"
                };
                writeln!(
                    writer,
                    "  Cost Coverage: {coverage}: {}",
                    summary.unpriced_models.join(", ")
                )?;
            }
            for qualification in &analytics.usage.pricing_qualifications {
                writeln!(writer, "  Cost Qualification: {qualification}")?;
            }
        }

        writeln!(writer)?;
        self.write_separator(writer)?;
        writeln!(writer)?;

        Ok(())
    }

    /// Check if a user message has any renderable content left after filtering.
    ///
    /// The dispatch transform has already pruned excluded blocks, so any block
    /// that remains is content to show (a non-empty text block, or any non-text
    /// block).
    fn has_user_text_content(user: &UserMessage) -> bool {
        match &user.message {
            crate::model::UserContent::Simple(simple) => !simple.content.trim().is_empty(),
            crate::model::UserContent::Blocks(blocks) => {
                blocks.content.iter().any(|block| match block {
                    ContentBlock::Text(t) => !t.text.trim().is_empty(),
                    _ => true,
                })
            }
        }
    }

    /// Write a user message.
    fn write_user_message<W: Write>(
        &self,
        writer: &mut W,
        user: &UserMessage,
        options: &ExportOptions,
    ) -> Result<()> {
        // Skip empty user messages (e.g., tool result placeholders with no text)
        if !Self::has_user_text_content(user) {
            return Ok(());
        }

        write!(writer, "[USER]")?;
        if options.include_timestamps {
            write!(writer, " ({})", format_timestamp(&user.timestamp))?;
        }
        writeln!(writer)?;
        writeln!(writer)?;

        // Write user content
        match &user.message {
            crate::model::UserContent::Simple(simple) => {
                if !simple.content.is_empty() {
                    write!(writer, "{}", self.wrap_text(&simple.content))?;
                }
            }
            crate::model::UserContent::Blocks(blocks) => {
                for content in &blocks.content {
                    self.write_content_block(writer, content, options)?;
                }
            }
        }

        writeln!(writer)?;
        Ok(())
    }

    /// Write an assistant message.
    fn write_assistant_message<W: Write>(
        &self,
        writer: &mut W,
        assistant: &AssistantMessage,
        options: &ExportOptions,
    ) -> Result<()> {
        write!(writer, "[ASSISTANT]")?;
        if options.include_timestamps {
            write!(writer, " ({})", format_timestamp(&assistant.timestamp))?;
        }
        writeln!(writer)?;
        writeln!(writer)?;

        // Write content blocks
        for content in &assistant.message.content {
            self.write_content_block(writer, content, options)?;
        }

        // Usage
        if options.include_usage {
            if let Some(usage) = &assistant.message.usage {
                writeln!(writer)?;
                writeln!(
                    writer,
                    "  [Tokens: {} in (incl. cache reads), {} out]",
                    usage.total_input_tokens(),
                    usage.output_tokens
                )?;
            }
        }

        writeln!(writer)?;
        Ok(())
    }

    /// Write a content block.
    ///
    /// Block-level filtering happens upstream in the dispatch transform, so this
    /// renders whatever blocks it receives.
    fn write_content_block<W: Write>(
        &self,
        writer: &mut W,
        content: &ContentBlock,
        _options: &ExportOptions,
    ) -> Result<()> {
        match content {
            ContentBlock::Unknown { kind, raw } => {
                let label = if kind.is_empty() {
                    "unknown"
                } else {
                    kind.as_str()
                };
                writeln!(writer, "[Unknown content block: {label}]")?;
                writeln!(
                    writer,
                    "{}",
                    serde_json::to_string(raw).unwrap_or_else(|_| raw.to_string())
                )?;
                writeln!(writer)?;
            }
            ContentBlock::Text(text) => {
                write!(writer, "{}", self.wrap_text(&text.text))?;
                writeln!(writer)?;
            }
            ContentBlock::Thinking(thinking) => self.write_thinking(writer, thinking)?,
            ContentBlock::ToolUse(tool_use) => self.write_tool_use(writer, tool_use)?,
            ContentBlock::ToolResult(result) => self.write_tool_result(writer, result)?,
            ContentBlock::Image(image) => {
                let media_type = image.source.media_type().unwrap_or("image");
                writeln!(writer, "[Image: {media_type}]")?;
            }
        }
        Ok(())
    }

    /// Write a thinking block.
    ///
    /// Recent Claude Code versions persist thinking blocks with empty text
    /// (only the encrypted signature) — skip those instead of rendering an
    /// empty section.
    fn write_thinking<W: Write>(&self, writer: &mut W, thinking: &ThinkingBlock) -> Result<()> {
        if thinking.thinking.is_empty() {
            return Ok(());
        }
        writeln!(writer, "  [THINKING]")?;
        for line in thinking.thinking.lines() {
            writeln!(writer, "  | {}", self.wrap_text(line).trim())?;
        }
        writeln!(writer, "  [/THINKING]")?;
        writeln!(writer)?;
        Ok(())
    }

    /// Write a tool use block.
    fn write_tool_use<W: Write>(&self, writer: &mut W, tool_use: &ToolUse) -> Result<()> {
        writeln!(writer, "  [TOOL: {}]", tool_use.name)?;
        writeln!(writer, "  ID: {}", tool_use.id)?;
        self.write_tool_input(writer, tool_use)?;
        writeln!(writer, "  [/TOOL]")?;
        writeln!(writer)?;
        Ok(())
    }

    /// Write a tool call's input, rendering common tools readably (Edit → diff,
    /// Bash → shell, Write → code, TodoWrite → checklist) and falling back to
    /// pretty-JSON for everything else. Body lines keep the `  | ` prefix.
    fn write_tool_input<W: Write>(&self, writer: &mut W, tool_use: &ToolUse) -> Result<()> {
        let prefixed = |writer: &mut W, body: &str| -> Result<()> {
            for line in body.lines() {
                writeln!(writer, "  | {line}")?;
            }
            Ok(())
        };
        match tool_render::classify(tool_use) {
            ToolInputView::Edit { file_path, edits } => {
                if edits.len() > 1 {
                    writeln!(writer, "  Edit: {} ({} changes)", file_path, edits.len())?;
                } else {
                    writeln!(writer, "  Edit: {file_path}")?;
                }
                for edit in &edits {
                    prefixed(
                        writer,
                        &tool_render::unified_diff(edit.old_string, edit.new_string),
                    )?;
                }
            }
            ToolInputView::Bash {
                command,
                description,
            } => {
                if let Some(desc) = description {
                    writeln!(writer, "  Command: {desc}")?;
                } else {
                    writeln!(writer, "  Command:")?;
                }
                prefixed(writer, command)?;
            }
            ToolInputView::Write { file_path, content } => {
                writeln!(writer, "  Write: {file_path}")?;
                prefixed(writer, content)?;
            }
            ToolInputView::Todos(items) => {
                writeln!(writer, "  Todos:")?;
                for item in &items {
                    writeln!(writer, "  | {} {}", item.checkbox(), item.content)?;
                }
            }
            ToolInputView::Json => {
                let input = serde_json::to_string_pretty(&tool_use.input).unwrap_or_default();
                prefixed(writer, &input)?;
            }
        }
        Ok(())
    }

    /// Write a tool result block.
    fn write_tool_result<W: Write>(&self, writer: &mut W, result: &ToolResult) -> Result<()> {
        let status = if result.is_explicit_error() {
            "ERROR"
        } else {
            "OK"
        };
        writeln!(writer, "  [RESULT: {} ({})]", result.tool_use_id, status)?;

        let content_str = result
            .content
            .as_ref()
            .map(|c| c.to_display_string(true))
            .unwrap_or_default();

        if !content_str.is_empty() {
            // Limit output for very long results
            let max_lines = 50;
            let lines: Vec<&str> = content_str.lines().collect();
            for (i, line) in lines.iter().enumerate() {
                if i >= max_lines {
                    writeln!(writer, "  | ... ({} more lines)", lines.len() - max_lines)?;
                    break;
                }
                writeln!(writer, "  | {line}")?;
            }
        }
        writeln!(writer, "  [/RESULT]")?;
        writeln!(writer)?;
        Ok(())
    }

    /// Write a system message.
    fn write_system_message<W: Write>(
        &self,
        writer: &mut W,
        system: &SystemMessage,
        options: &ExportOptions,
    ) -> Result<()> {
        if !options.should_include_system() {
            return Ok(());
        }

        write!(writer, "[SYSTEM")?;
        if let Some(subtype) = &system.subtype {
            write!(writer, ": {subtype:?}")?;
        }
        write!(writer, "]")?;
        if options.include_timestamps {
            write!(writer, " ({})", format_timestamp(&system.timestamp))?;
        }
        writeln!(writer)?;

        if let Some(content) = &system.content {
            write!(writer, "{}", self.wrap_text(content))?;
        }

        writeln!(writer)?;
        Ok(())
    }

    /// Write a summary message.
    fn write_summary_message<W: Write>(
        &self,
        writer: &mut W,
        summary: &SummaryMessage,
        options: &ExportOptions,
    ) -> Result<()> {
        writeln!(writer, "[SUMMARY]")?;
        writeln!(writer)?;
        write!(writer, "{}", self.wrap_text(&summary.summary))?;

        if options.include_metadata {
            if let Some(leaf_uuid) = &summary.leaf_uuid {
                writeln!(writer)?;
                writeln!(writer, "(Leaf UUID: {leaf_uuid})")?;
            }
        }

        writeln!(writer)?;
        Ok(())
    }
}

impl Exporter for TextExporter {
    fn export_conversation<W: Write>(
        &self,
        conversation: &Conversation,
        writer: &mut W,
        options: &ExportOptions,
    ) -> Result<()> {
        // Write header
        self.write_header(writer, conversation, options)?;

        // Get entries (compaction summaries first, then the rendered thread)
        let entries = conversation.entries_for_export(options.main_thread_only);

        // Write each entry
        for entry in entries {
            match entry {
                LogEntry::User(user) if options.should_include_user() => {
                    self.write_user_message(writer, user, options)?;
                }
                LogEntry::Assistant(assistant) if options.should_include_assistant() => {
                    self.write_assistant_message(writer, assistant, options)?;
                }
                LogEntry::System(system) if options.should_include_system() => {
                    self.write_system_message(writer, system, options)?;
                }
                LogEntry::Summary(summary) if options.should_include_summary() => {
                    self.write_summary_message(writer, summary, options)?;
                }
                LogEntry::FileHistorySnapshot(_)
                | LogEntry::QueueOperation(_)
                | LogEntry::TurnEnd(_) => {
                    // Skip these in text export
                }
                _ => {
                    // Filtered out by options
                }
            }
        }

        self.write_separator(writer)?;
        writeln!(writer, "END OF CONVERSATION")?;
        self.write_separator(writer)?;

        Ok(())
    }

    fn export_entries<W: Write>(
        &self,
        entries: &[LogEntry],
        writer: &mut W,
        options: &ExportOptions,
    ) -> Result<()> {
        for entry in entries {
            match entry {
                LogEntry::User(user) if options.should_include_user() => {
                    self.write_user_message(writer, user, options)?;
                }
                LogEntry::Assistant(assistant) if options.should_include_assistant() => {
                    self.write_assistant_message(writer, assistant, options)?;
                }
                LogEntry::System(system) if options.should_include_system() => {
                    self.write_system_message(writer, system, options)?;
                }
                LogEntry::Summary(summary) if options.should_include_summary() => {
                    self.write_summary_message(writer, summary, options)?;
                }
                LogEntry::Unknown(raw) if options.should_include_system() => {
                    // Preserved unknown events render as compact JSON rather
                    // than vanishing, when system-style output is requested.
                    let label = raw
                        .get("type")
                        .and_then(|v| v.as_str())
                        .unwrap_or("unknown");
                    writeln!(writer, "[{label} event]")?;
                    writeln!(
                        writer,
                        "{}",
                        serde_json::to_string(raw).unwrap_or_else(|_| raw.to_string())
                    )?;
                    writeln!(writer)?;
                }
                _ => {}
            }
        }
        Ok(())
    }
}

/// Format a timestamp for display.
fn format_timestamp(ts: &DateTime<Utc>) -> String {
    ts.format("%Y-%m-%d %H:%M:%S UTC").to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_text_exporter_builder() {
        let exporter = TextExporter::new()
            .with_line_width(120)
            .with_separators(false)
            .with_line_numbers(true);

        assert_eq!(exporter.line_width, 120);
        assert!(!exporter.use_separators);
        assert!(exporter.line_numbers);
    }

    #[test]
    fn test_wrap_text() {
        let exporter = TextExporter::new().with_line_width(20);
        let text = "This is a long line that should be wrapped";
        let wrapped = exporter.wrap_text(text);

        for line in wrapped.lines() {
            assert!(line.len() <= 20 || !line.contains(' '));
        }
    }

    #[test]
    fn test_wrap_text_no_wrap() {
        let exporter = TextExporter::new().with_line_width(0);
        let text = "This line should not be wrapped at all";
        let wrapped = exporter.wrap_text(text);
        assert_eq!(wrapped.trim(), text);
    }
}
