//! Markdown export for conversations.
//!
//! Generates human-readable Markdown output from Claude Code conversations,
//! suitable for documentation, archival, and sharing.

use std::io::Write;

use chrono::{DateTime, Utc};

use crate::analytics::SessionAnalytics;
use crate::error::Result;
use crate::model::{
    content::{ImageSource, StopReason, ThinkingBlock, ToolResult, ToolUse},
    AssistantMessage, ContentBlock, LogEntry, SystemMessage, SummaryMessage, UserMessage,
};
use crate::reconstruction::Conversation;

use super::{ExportOptions, Exporter};

/// Markdown exporter for conversations.
#[derive(Debug, Clone)]
pub struct MarkdownExporter {
    /// Use plain text output (no Markdown formatting).
    plain_text: bool,
    /// Include table of contents.
    include_toc: bool,
    /// Include session summary header.
    include_header: bool,
    /// Code fence language for tool outputs.
    code_fence_lang: String,
    /// Maximum thinking block length before collapsing.
    thinking_collapse_threshold: usize,
    /// Maximum tool result length before collapsing.
    tool_result_collapse_threshold: usize,
}

impl Default for MarkdownExporter {
    fn default() -> Self {
        Self::new()
    }
}

impl MarkdownExporter {
    /// Create a new Markdown exporter.
    #[must_use]
    pub fn new() -> Self {
        Self {
            plain_text: false,
            include_toc: false,
            include_header: true,
            code_fence_lang: String::new(),
            thinking_collapse_threshold: 2000,
            tool_result_collapse_threshold: 5000,
        }
    }

    /// Enable plain text output (no Markdown formatting).
    #[must_use]
    pub fn plain_text(mut self, plain: bool) -> Self {
        self.plain_text = plain;
        self
    }

    /// Include table of contents.
    #[must_use]
    pub fn with_toc(mut self, include: bool) -> Self {
        self.include_toc = include;
        self
    }

    /// Include session header.
    #[must_use]
    pub fn with_header(mut self, include: bool) -> Self {
        self.include_header = include;
        self
    }

    /// Set code fence language.
    #[must_use]
    pub fn code_fence_lang(mut self, lang: impl Into<String>) -> Self {
        self.code_fence_lang = lang.into();
        self
    }

    /// Write the session header.
    fn write_header<W: Write>(
        &self,
        writer: &mut W,
        conversation: &Conversation,
        options: &ExportOptions,
    ) -> Result<()> {
        if !self.include_header {
            return Ok(());
        }

        // Note: conversation.statistics() provides basic stats while
        // SessionAnalytics provides enhanced analytics including cost estimation
        let analytics = SessionAnalytics::from_conversation(conversation);

        if self.plain_text {
            writeln!(writer, "Claude Code Conversation")?;
            writeln!(writer, "========================")?;
            writeln!(writer)?;
        } else {
            writeln!(writer, "# Claude Code Conversation")?;
            writeln!(writer)?;
        }

        // Session info
        if options.include_metadata {
            if let Some(first_entry) = conversation.main_thread_entries().first() {
                if let Some(session_id) = first_entry.session_id() {
                    writeln!(writer, "**Session ID:** `{session_id}`")?;
                }
                if let Some(version) = first_entry.version() {
                    writeln!(writer, "**Claude Code Version:** {version}")?;
                }
            }
        }

        // Timestamps
        if options.include_timestamps {
            if let Some(start) = analytics.start_time {
                writeln!(writer, "**Started:** {}", format_timestamp(&start))?;
            }
            if let Some(end) = analytics.end_time {
                writeln!(writer, "**Ended:** {}", format_timestamp(&end))?;
            }
            if let Some(duration) = analytics.duration_string() {
                writeln!(writer, "**Duration:** {duration}")?;
            }
        }

        // Usage statistics
        if options.include_usage {
            let summary = analytics.summary_report();
            writeln!(writer)?;
            if self.plain_text {
                writeln!(writer, "Statistics")?;
                writeln!(writer, "----------")?;
            } else {
                writeln!(writer, "## Statistics")?;
            }
            writeln!(writer, "- **Messages:** {} ({} user, {} assistant)",
                summary.total_messages,
                summary.user_messages,
                summary.assistant_messages
            )?;
            writeln!(writer, "- **Total Tokens:** {}", summary.total_tokens)?;
            writeln!(writer, "- **Input Tokens:** {}", summary.input_tokens)?;
            writeln!(writer, "- **Output Tokens:** {}", summary.output_tokens)?;
            if summary.cache_hit_rate > 0.0 {
                writeln!(writer, "- **Cache Hit Rate:** {:.1}%", summary.cache_hit_rate)?;
            }
            writeln!(writer, "- **Tool Invocations:** {}", summary.tool_invocations)?;
            if summary.thinking_blocks > 0 {
                writeln!(writer, "- **Thinking Blocks:** {}", summary.thinking_blocks)?;
            }
            if let Some(model) = &summary.primary_model {
                writeln!(writer, "- **Primary Model:** {model}")?;
            }
            writeln!(writer, "- **Estimated Cost:** {}", summary.cost_string())?;
        }

        writeln!(writer)?;
        if !self.plain_text {
            writeln!(writer, "---")?;
            writeln!(writer)?;
        }

        Ok(())
    }

    /// Write a user message.
    fn write_user_message<W: Write>(
        &self,
        writer: &mut W,
        user: &UserMessage,
        options: &ExportOptions,
    ) -> Result<()> {
        if self.plain_text {
            writeln!(writer, "USER:")?;
        } else {
            write!(writer, "## 👤 User")?;
            if options.include_timestamps {
                write!(writer, " *({})* ", format_timestamp(&user.timestamp))?;
            }
            writeln!(writer)?;
        }
        writeln!(writer)?;

        // Write user content
        match &user.message {
            crate::model::UserContent::Simple(simple) => {
                writeln!(writer, "{}", simple.content)?;
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
        if self.plain_text {
            writeln!(writer, "ASSISTANT:")?;
        } else {
            write!(writer, "## 🤖 Assistant")?;
            if options.include_timestamps {
                write!(writer, " *({})* ", format_timestamp(&assistant.timestamp))?;
            }
            writeln!(writer)?;
        }
        writeln!(writer)?;

        // Write content blocks
        for content in &assistant.message.content {
            self.write_content_block(writer, content, options)?;
        }

        // Stop reason and usage
        if options.include_metadata {
            if let Some(stop_reason) = &assistant.message.stop_reason {
                writeln!(writer)?;
                writeln!(writer, "*Stop reason: {}*", format_stop_reason(stop_reason))?;
            }
        }

        if options.include_usage {
            if let Some(usage) = &assistant.message.usage {
                writeln!(writer)?;
                writeln!(writer, "*Tokens: {} in, {} out*",
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
                writeln!(writer, "{}", text.text)?;
                writeln!(writer)?;
            }
            ContentBlock::Thinking(thinking) => {
                if options.include_thinking {
                    self.write_thinking(writer, thinking)?;
                }
            }
            ContentBlock::ToolUse(tool_use) => {
                if options.include_tool_use {
                    self.write_tool_use(writer, tool_use)?;
                }
            }
            ContentBlock::ToolResult(result) => {
                if options.include_tool_results {
                    self.write_tool_result(writer, result, options)?;
                }
            }
            ContentBlock::Image(image) => {
                if options.include_images {
                    self.write_image(writer, image)?;
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
        if self.plain_text {
            writeln!(writer, "[THINKING]")?;
            writeln!(writer, "{}", thinking.thinking)?;
            writeln!(writer, "[/THINKING]")?;
        } else {
            let should_collapse = thinking.thinking.len() > self.thinking_collapse_threshold;

            if should_collapse {
                writeln!(writer, "<details>")?;
                writeln!(writer, "<summary>💭 Thinking ({} chars)</summary>", thinking.thinking.len())?;
                writeln!(writer)?;
            } else {
                writeln!(writer, "### 💭 Thinking")?;
                writeln!(writer)?;
            }

            writeln!(writer, "```")?;
            writeln!(writer, "{}", thinking.thinking)?;
            writeln!(writer, "```")?;

            if should_collapse {
                writeln!(writer)?;
                writeln!(writer, "</details>")?;
            }
        }
        writeln!(writer)?;
        Ok(())
    }

    /// Write a tool use block.
    fn write_tool_use<W: Write>(
        &self,
        writer: &mut W,
        tool_use: &ToolUse,
    ) -> Result<()> {
        if self.plain_text {
            writeln!(writer, "[TOOL: {}]", tool_use.name)?;
            writeln!(writer, "ID: {}", tool_use.id)?;
            writeln!(writer, "Input: {}", serde_json::to_string_pretty(&tool_use.input).unwrap_or_default())?;
            writeln!(writer, "[/TOOL]")?;
        } else {
            // Determine tool icon
            let icon = if tool_use.is_mcp_tool() {
                "🔌"
            } else if tool_use.is_server_tool() {
                "🖥️"
            } else {
                "🔧"
            };

            writeln!(writer, "### {icon} Tool: `{}`", tool_use.name)?;
            writeln!(writer)?;
            writeln!(writer, "**ID:** `{}`", tool_use.id)?;
            writeln!(writer)?;
            writeln!(writer, "**Input:**")?;
            writeln!(writer, "```json")?;
            writeln!(writer, "{}", serde_json::to_string_pretty(&tool_use.input).unwrap_or_default())?;
            writeln!(writer, "```")?;
        }
        writeln!(writer)?;
        Ok(())
    }

    /// Write a tool result block.
    fn write_tool_result<W: Write>(
        &self,
        writer: &mut W,
        result: &ToolResult,
        _options: &ExportOptions,
    ) -> Result<()> {
        // Get content as string for display
        let content_str = match &result.content {
            Some(crate::model::content::ToolResultContent::String(s)) => s.clone(),
            Some(crate::model::content::ToolResultContent::Array(arr)) => {
                serde_json::to_string_pretty(arr).unwrap_or_else(|_| "[Array]".to_string())
            }
            None => String::new(),
        };

        if self.plain_text {
            writeln!(writer, "[TOOL RESULT: {}]", result.tool_use_id)?;
            if result.is_explicit_error() {
                writeln!(writer, "STATUS: ERROR")?;
            }
            if !content_str.is_empty() {
                writeln!(writer, "{content_str}")?;
            }
            writeln!(writer, "[/TOOL RESULT]")?;
        } else {
            let content_len = content_str.len();
            let should_collapse = content_len > self.tool_result_collapse_threshold;
            let status = if result.is_explicit_error() { "❌ Error" } else { "✅ Result" };

            if should_collapse {
                writeln!(writer, "<details>")?;
                writeln!(writer, "<summary>{} for `{}` ({} chars)</summary>",
                    status, result.tool_use_id, content_len)?;
                writeln!(writer)?;
            } else {
                writeln!(writer, "#### {} for `{}`", status, result.tool_use_id)?;
                writeln!(writer)?;
            }

            if !content_str.is_empty() {
                writeln!(writer, "```")?;
                writeln!(writer, "{content_str}")?;
                writeln!(writer, "```")?;
            }

            if should_collapse {
                writeln!(writer)?;
                writeln!(writer, "</details>")?;
            }
        }
        writeln!(writer)?;
        Ok(())
    }

    /// Write an image block.
    fn write_image<W: Write>(
        &self,
        writer: &mut W,
        image: &crate::model::content::ImageBlock,
    ) -> Result<()> {
        if self.plain_text {
            let media_type = image.source.media_type().unwrap_or("image");
            writeln!(writer, "[Image: {media_type}]")?;
        } else {
            match &image.source {
                ImageSource::Base64 { media_type, data } => {
                    // Write as embedded base64 image
                    writeln!(writer, "![Image](data:{media_type};base64,{}...)",
                        &data[..std::cmp::min(50, data.len())])?;
                    writeln!(writer, "*({} base64 encoded)*", data.len())?;
                }
                ImageSource::Url { url } => {
                    writeln!(writer, "![Image]({url})")?;
                }
                ImageSource::File { file_id } => {
                    writeln!(writer, "*Image file: {file_id}*")?;
                }
            }
        }
        Ok(())
    }

    /// Write a system message.
    fn write_system_message<W: Write>(
        &self,
        writer: &mut W,
        system: &SystemMessage,
        options: &ExportOptions,
    ) -> Result<()> {
        if !options.include_system {
            return Ok(());
        }

        if self.plain_text {
            writeln!(writer, "SYSTEM:")?;
        } else {
            write!(writer, "## ⚙️ System")?;
            if let Some(subtype) = &system.subtype {
                write!(writer, " ({subtype:?})")?;
            }
            if options.include_timestamps {
                write!(writer, " *({})* ", format_timestamp(&system.timestamp))?;
            }
            writeln!(writer)?;
        }
        writeln!(writer)?;

        // Write system content based on subtype
        if let Some(content) = &system.content {
            writeln!(writer, "{content}")?;
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
        if self.plain_text {
            writeln!(writer, "SUMMARY:")?;
        } else {
            writeln!(writer, "## 📋 Summary")?;
        }
        writeln!(writer)?;

        writeln!(writer, "{}", summary.summary)?;

        // Include leaf UUID reference if metadata is requested
        if options.include_metadata {
            if let Some(leaf_uuid) = &summary.leaf_uuid {
                writeln!(writer)?;
                if !self.plain_text {
                    writeln!(writer, "*Leaf UUID: `{leaf_uuid}`*")?;
                } else {
                    writeln!(writer, "(Leaf UUID: {leaf_uuid})")?;
                }
            }
        }

        writeln!(writer)?;
        Ok(())
    }
}

impl Exporter for MarkdownExporter {
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

        // Write conversation header
        if !self.plain_text && !self.include_header {
            writeln!(writer, "# Conversation")?;
            writeln!(writer)?;
        }

        // Write each entry
        for entry in entries {
            self.export_entry(writer, entry, options)?;
        }

        // Write footer with branch info if applicable
        if options.include_branches && conversation.has_branches() {
            writeln!(writer)?;
            if !self.plain_text {
                writeln!(writer, "---")?;
                writeln!(writer)?;
                writeln!(writer, "## Branches")?;
                writeln!(writer)?;
            }

            for bp in conversation.branch_points() {
                if let Some(node) = conversation.get_node(bp) {
                    writeln!(writer, "- Branch point at `{bp}` with {} children",
                        node.children.len())?;
                }
            }
        }

        Ok(())
    }

    fn export_entries<W: Write>(
        &self,
        entries: &[LogEntry],
        writer: &mut W,
        options: &ExportOptions,
    ) -> Result<()> {
        for entry in entries {
            self.export_entry(writer, entry, options)?;
        }
        Ok(())
    }
}

impl MarkdownExporter {
    /// Export a single entry.
    fn export_entry<W: Write>(
        &self,
        writer: &mut W,
        entry: &LogEntry,
        options: &ExportOptions,
    ) -> Result<()> {
        match entry {
            LogEntry::User(user) => {
                self.write_user_message(writer, user, options)?;
            }
            LogEntry::Assistant(assistant) => {
                self.write_assistant_message(writer, assistant, options)?;
            }
            LogEntry::System(system) => {
                self.write_system_message(writer, system, options)?;
            }
            LogEntry::Summary(summary) => {
                self.write_summary_message(writer, summary, options)?;
            }
            LogEntry::FileHistorySnapshot(_) => {
                // Skip file history snapshots in Markdown export
            }
            LogEntry::QueueOperation(_) => {
                // Skip queue operations in Markdown export
            }
            LogEntry::TurnEnd(_) => {
                // Skip turn end markers in Markdown export
            }
        }
        Ok(())
    }
}

/// Format a timestamp for display.
fn format_timestamp(ts: &DateTime<Utc>) -> String {
    ts.format("%Y-%m-%d %H:%M:%S UTC").to_string()
}

/// Format a stop reason for display.
fn format_stop_reason(reason: &StopReason) -> &'static str {
    match reason {
        StopReason::ToolUse => "tool_use",
        StopReason::EndTurn => "end_turn",
        StopReason::MaxTokens => "max_tokens",
        StopReason::StopSequence => "stop_sequence",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_format_timestamp() {
        use chrono::TimeZone;
        let ts = Utc.with_ymd_and_hms(2025, 12, 23, 10, 30, 0).unwrap();
        assert_eq!(format_timestamp(&ts), "2025-12-23 10:30:00 UTC");
    }

    #[test]
    fn test_format_stop_reason() {
        assert_eq!(format_stop_reason(&StopReason::EndTurn), "end_turn");
        assert_eq!(format_stop_reason(&StopReason::ToolUse), "tool_use");
    }

    #[test]
    fn test_exporter_builder() {
        let exporter = MarkdownExporter::new()
            .plain_text(true)
            .with_toc(true)
            .with_header(false);

        assert!(exporter.plain_text);
        assert!(exporter.include_toc);
        assert!(!exporter.include_header);
    }
}
