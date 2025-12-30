//! Plain text export for conversations.
//!
//! Generates clean, readable plain text output with configurable
//! line width and ASCII formatting options.

use std::io::Write;

use chrono::{DateTime, Utc};

use crate::analytics::SessionAnalytics;
use crate::error::Result;
use crate::model::{
    content::{ThinkingBlock, ToolResult, ToolUse},
    AssistantMessage, ContentBlock, LogEntry, SystemMessage, SummaryMessage, UserMessage,
};
use crate::reconstruction::Conversation;

use super::{ContentType, ExportOptions, Exporter};

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
            let width = if self.line_width > 0 { self.line_width } else { 60 };
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
                writeln!(writer, "Started: {}", format_timestamp(&start))?;
            }
            if let Some(end) = analytics.end_time {
                writeln!(writer, "Ended: {}", format_timestamp(&end))?;
            }
            if let Some(duration) = analytics.duration_string() {
                writeln!(writer, "Duration: {duration}")?;
            }
        }

        // Usage statistics
        if options.include_usage {
            let summary = analytics.summary_report();
            writeln!(writer)?;
            writeln!(writer, "STATISTICS:")?;
            writeln!(writer, "  Messages: {} ({} user, {} assistant)",
                summary.total_messages,
                summary.user_messages,
                summary.assistant_messages
            )?;
            writeln!(writer, "  Total Tokens: {}", summary.total_tokens)?;
            writeln!(writer, "  Input Tokens: {}", summary.input_tokens)?;
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
            writeln!(writer, "  Estimated Cost: {}", summary.cost_string())?;
        }

        writeln!(writer)?;
        self.write_separator(writer)?;
        writeln!(writer)?;

        Ok(())
    }

    /// Write a user message.
    fn write_user_message<W: Write>(
        &self,
        writer: &mut W,
        user: &UserMessage,
        options: &ExportOptions,
    ) -> Result<()> {
        write!(writer, "[USER]")?;
        if options.include_timestamps {
            write!(writer, " ({})", format_timestamp(&user.timestamp))?;
        }
        writeln!(writer)?;
        writeln!(writer)?;

        // Write user content
        match &user.message {
            crate::model::UserContent::Simple(simple) => {
                write!(writer, "{}", self.wrap_text(&simple.content))?;
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
                writeln!(writer, "  [Tokens: {} in, {} out]",
                    usage.total_input_tokens(),
                    usage.output_tokens
                )?;
            }
        }

        writeln!(writer)?;
        Ok(())
    }

    /// Write a content block.
    fn write_content_block<W: Write>(
        &self,
        writer: &mut W,
        content: &ContentBlock,
        options: &ExportOptions,
    ) -> Result<()> {
        match content {
            ContentBlock::Text(text) => {
                // Use should_include() directly for text content to respect exclusive filter
                if options.should_include(ContentType::Assistant) {
                    write!(writer, "{}", self.wrap_text(&text.text))?;
                    writeln!(writer)?;
                }
            }
            ContentBlock::Thinking(thinking) => {
                if options.should_include_thinking() {
                    self.write_thinking(writer, thinking)?;
                }
            }
            ContentBlock::ToolUse(tool_use) => {
                if options.should_include_tool_use() {
                    self.write_tool_use(writer, tool_use)?;
                }
            }
            ContentBlock::ToolResult(result) => {
                if options.should_include_tool_results() {
                    self.write_tool_result(writer, result)?;
                }
            }
            ContentBlock::Image(image) => {
                if options.include_images {
                    let media_type = image.source.media_type().unwrap_or("image");
                    writeln!(writer, "[Image: {media_type}]")?;
                }
            }
        }
        Ok(())
    }

    /// Write a thinking block.
    fn write_thinking<W: Write>(
        &self,
        writer: &mut W,
        thinking: &ThinkingBlock,
    ) -> Result<()> {
        writeln!(writer, "  [THINKING]")?;
        for line in thinking.thinking.lines() {
            writeln!(writer, "  | {}", self.wrap_text(line).trim())?;
        }
        writeln!(writer, "  [/THINKING]")?;
        writeln!(writer)?;
        Ok(())
    }

    /// Write a tool use block.
    fn write_tool_use<W: Write>(
        &self,
        writer: &mut W,
        tool_use: &ToolUse,
    ) -> Result<()> {
        writeln!(writer, "  [TOOL: {}]", tool_use.name)?;
        writeln!(writer, "  ID: {}", tool_use.id)?;
        let input = serde_json::to_string_pretty(&tool_use.input).unwrap_or_default();
        for line in input.lines() {
            writeln!(writer, "  | {line}")?;
        }
        writeln!(writer, "  [/TOOL]")?;
        writeln!(writer)?;
        Ok(())
    }

    /// Write a tool result block.
    fn write_tool_result<W: Write>(
        &self,
        writer: &mut W,
        result: &ToolResult,
    ) -> Result<()> {
        let status = if result.is_explicit_error() { "ERROR" } else { "OK" };
        writeln!(writer, "  [RESULT: {} ({})]", result.tool_use_id, status)?;

        let content_str = match &result.content {
            Some(crate::model::content::ToolResultContent::String(s)) => s.clone(),
            Some(crate::model::content::ToolResultContent::Array(arr)) => {
                serde_json::to_string_pretty(arr).unwrap_or_else(|_| "[Array]".to_string())
            }
            None => String::new(),
        };

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

        // Get entries
        let entries = if options.main_thread_only {
            conversation.main_thread_entries()
        } else {
            conversation.chronological_entries()
        };

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
