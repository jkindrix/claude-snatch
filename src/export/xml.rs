//! XML export for conversation data.
//!
//! Exports conversations to well-formed XML for integration with
//! other systems and tools. Supports both compact and pretty-printed output.

use std::io::Write;

use chrono::{DateTime, Utc};

use crate::analytics::SessionAnalytics;
use crate::error::Result;
use crate::model::{
    content::{ThinkingBlock, ToolResult, ToolResultContent, ToolUse},
    ContentBlock, LogEntry,
};
use crate::reconstruction::Conversation;

use super::{ExportOptions, Exporter};

/// XML exporter for conversation data.
#[derive(Debug, Clone)]
pub struct XmlExporter {
    /// Pretty print with indentation.
    pretty: bool,
    /// Indentation string (when pretty is true).
    indent: String,
    /// Include XML declaration.
    include_declaration: bool,
    /// Include schema reference.
    include_schema: bool,
}

impl Default for XmlExporter {
    fn default() -> Self {
        Self::new()
    }
}

impl XmlExporter {
    /// Create a new XML exporter.
    #[must_use]
    pub fn new() -> Self {
        Self {
            pretty: true,
            indent: "  ".to_string(),
            include_declaration: true,
            include_schema: false,
        }
    }

    /// Enable or disable pretty printing.
    #[must_use]
    pub fn pretty(mut self, enable: bool) -> Self {
        self.pretty = enable;
        self
    }

    /// Set the indentation string.
    #[must_use]
    pub fn with_indent(mut self, indent: &str) -> Self {
        self.indent = indent.to_string();
        self
    }

    /// Include XML declaration.
    #[must_use]
    pub fn with_declaration(mut self, include: bool) -> Self {
        self.include_declaration = include;
        self
    }

    /// Include schema reference.
    #[must_use]
    pub fn with_schema(mut self, include: bool) -> Self {
        self.include_schema = include;
        self
    }

    /// Write indentation at the given depth.
    fn write_indent<W: Write>(&self, writer: &mut W, depth: usize) -> Result<()> {
        if self.pretty {
            for _ in 0..depth {
                write!(writer, "{}", self.indent)?;
            }
        }
        Ok(())
    }

    /// Write a newline if pretty printing.
    fn write_newline<W: Write>(&self, writer: &mut W) -> Result<()> {
        if self.pretty {
            writeln!(writer)?;
        }
        Ok(())
    }

    /// Escape text for XML content.
    fn escape_text(text: &str) -> String {
        text.replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;")
            .replace('"', "&quot;")
            .replace('\'', "&apos;")
    }

    /// Escape text for XML attributes.
    fn escape_attr(text: &str) -> String {
        Self::escape_text(text)
            .replace('\n', "&#10;")
            .replace('\r', "&#13;")
            .replace('\t', "&#9;")
    }

    /// Write an opening tag.
    fn write_open_tag<W: Write>(
        &self,
        writer: &mut W,
        depth: usize,
        name: &str,
        attrs: &[(&str, &str)],
    ) -> Result<()> {
        self.write_indent(writer, depth)?;
        write!(writer, "<{}", name)?;
        for (key, value) in attrs {
            write!(writer, " {}=\"{}\"", key, Self::escape_attr(value))?;
        }
        write!(writer, ">")?;
        self.write_newline(writer)?;
        Ok(())
    }

    /// Write a closing tag.
    fn write_close_tag<W: Write>(&self, writer: &mut W, depth: usize, name: &str) -> Result<()> {
        self.write_indent(writer, depth)?;
        write!(writer, "</{}>", name)?;
        self.write_newline(writer)?;
        Ok(())
    }

    /// Write a self-closing tag.
    fn write_empty_tag<W: Write>(
        &self,
        writer: &mut W,
        depth: usize,
        name: &str,
        attrs: &[(&str, &str)],
    ) -> Result<()> {
        self.write_indent(writer, depth)?;
        write!(writer, "<{}", name)?;
        for (key, value) in attrs {
            write!(writer, " {}=\"{}\"", key, Self::escape_attr(value))?;
        }
        write!(writer, "/>")?;
        self.write_newline(writer)?;
        Ok(())
    }

    /// Write a simple text element.
    fn write_text_element<W: Write>(
        &self,
        writer: &mut W,
        depth: usize,
        name: &str,
        text: &str,
    ) -> Result<()> {
        self.write_indent(writer, depth)?;
        write!(writer, "<{}>{}</{}>", name, Self::escape_text(text), name)?;
        self.write_newline(writer)?;
        Ok(())
    }

    /// Write a CDATA section.
    fn write_cdata<W: Write>(
        &self,
        writer: &mut W,
        depth: usize,
        name: &str,
        content: &str,
    ) -> Result<()> {
        self.write_indent(writer, depth)?;
        write!(writer, "<{}>", name)?;
        self.write_newline(writer)?;

        self.write_indent(writer, depth + 1)?;
        // CDATA cannot contain "]]>" so we need to escape it
        let escaped = content.replace("]]>", "]]]]><![CDATA[>");
        write!(writer, "<![CDATA[{}]]>", escaped)?;
        self.write_newline(writer)?;

        self.write_close_tag(writer, depth, name)?;
        Ok(())
    }

    /// Write conversation metadata.
    fn write_metadata<W: Write>(
        &self,
        writer: &mut W,
        conversation: &Conversation,
        options: &ExportOptions,
    ) -> Result<()> {
        let analytics = SessionAnalytics::from_conversation(conversation);

        self.write_open_tag(writer, 1, "metadata", &[])?;

        // Session info
        if options.include_metadata {
            if let Some(first) = conversation.main_thread_entries().first() {
                if let Some(session_id) = first.session_id() {
                    self.write_text_element(writer, 2, "session-id", session_id)?;
                }
                if let Some(version) = first.version() {
                    self.write_text_element(writer, 2, "claude-code-version", version)?;
                }
            }
        }

        // Timestamps
        if options.include_timestamps {
            if let Some(start) = analytics.start_time {
                self.write_text_element(writer, 2, "start-time", &format_timestamp(&start))?;
            }
            if let Some(end) = analytics.end_time {
                self.write_text_element(writer, 2, "end-time", &format_timestamp(&end))?;
            }
            if let Some(duration) = analytics.duration_string() {
                self.write_text_element(writer, 2, "duration", &duration)?;
            }
        }

        // Usage statistics
        if options.include_usage {
            let summary = analytics.summary_report();
            self.write_open_tag(writer, 2, "usage", &[])?;
            self.write_text_element(writer, 3, "total-messages", &summary.total_messages.to_string())?;
            self.write_text_element(writer, 3, "user-messages", &summary.user_messages.to_string())?;
            self.write_text_element(writer, 3, "assistant-messages", &summary.assistant_messages.to_string())?;
            self.write_text_element(writer, 3, "total-tokens", &summary.total_tokens.to_string())?;
            self.write_text_element(writer, 3, "input-tokens", &summary.input_tokens.to_string())?;
            self.write_text_element(writer, 3, "output-tokens", &summary.output_tokens.to_string())?;
            if summary.cache_hit_rate > 0.0 {
                self.write_text_element(writer, 3, "cache-hit-rate", &format!("{:.2}", summary.cache_hit_rate))?;
            }
            self.write_text_element(writer, 3, "tool-invocations", &summary.tool_invocations.to_string())?;
            if summary.thinking_blocks > 0 {
                self.write_text_element(writer, 3, "thinking-blocks", &summary.thinking_blocks.to_string())?;
            }
            if let Some(model) = &summary.primary_model {
                self.write_text_element(writer, 3, "primary-model", model)?;
            }
            if let Some(cost) = summary.estimated_cost {
                self.write_text_element(writer, 3, "estimated-cost-usd", &format!("{:.4}", cost))?;
            }
            self.write_close_tag(writer, 2, "usage")?;
        }

        self.write_close_tag(writer, 1, "metadata")?;
        Ok(())
    }

    /// Write a user message.
    fn write_user_message<W: Write>(
        &self,
        writer: &mut W,
        entry: &LogEntry,
        options: &ExportOptions,
    ) -> Result<()> {
        if let LogEntry::User(user) = entry {
            let mut attrs = vec![("type", "user")];
            let uuid = entry.uuid().unwrap_or("");
            let parent = entry.parent_uuid().unwrap_or("");
            let timestamp = user.timestamp.format("%Y-%m-%dT%H:%M:%SZ").to_string();

            if options.include_metadata && !uuid.is_empty() {
                attrs.push(("uuid", uuid));
            }
            if options.include_metadata && !parent.is_empty() {
                attrs.push(("parent-uuid", parent));
            }

            let attrs_owned: Vec<(&str, String)> = attrs
                .into_iter()
                .map(|(k, v)| (k, v.to_string()))
                .collect();
            let attrs_ref: Vec<(&str, &str)> = attrs_owned
                .iter()
                .map(|(k, v)| (*k, v.as_str()))
                .collect();

            self.write_open_tag(writer, 2, "message", &attrs_ref)?;

            if options.include_timestamps {
                self.write_text_element(writer, 3, "timestamp", &timestamp)?;
            }

            // Write content
            self.write_open_tag(writer, 3, "content", &[])?;
            match &user.message {
                crate::model::UserContent::Simple(s) => {
                    self.write_cdata(writer, 4, "text", &s.content)?;
                }
                crate::model::UserContent::Blocks(b) => {
                    for block in &b.content {
                        self.write_content_block(writer, 4, block, options)?;
                    }
                }
            }
            self.write_close_tag(writer, 3, "content")?;

            self.write_close_tag(writer, 2, "message")?;
        }
        Ok(())
    }

    /// Write an assistant message.
    fn write_assistant_message<W: Write>(
        &self,
        writer: &mut W,
        entry: &LogEntry,
        options: &ExportOptions,
    ) -> Result<()> {
        if let LogEntry::Assistant(assistant) = entry {
            let uuid = entry.uuid().unwrap_or("");
            let parent = entry.parent_uuid().unwrap_or("");
            let timestamp = assistant.timestamp.format("%Y-%m-%dT%H:%M:%SZ").to_string();
            let model = &assistant.message.model;

            let mut attrs = vec![("type", "assistant"), ("model", model.as_str())];

            if options.include_metadata && !uuid.is_empty() {
                attrs.push(("uuid", uuid));
            }
            if options.include_metadata && !parent.is_empty() {
                attrs.push(("parent-uuid", parent));
            }

            self.write_open_tag(writer, 2, "message", &attrs)?;

            if options.include_timestamps {
                self.write_text_element(writer, 3, "timestamp", &timestamp)?;
            }

            // Usage
            if options.include_usage {
                if let Some(usage) = &assistant.message.usage {
                    self.write_open_tag(writer, 3, "usage", &[])?;
                    self.write_text_element(writer, 4, "input-tokens", &usage.total_input_tokens().to_string())?;
                    self.write_text_element(writer, 4, "output-tokens", &usage.output_tokens.to_string())?;
                    if let Some(cache_read) = usage.cache_read_input_tokens {
                        self.write_text_element(writer, 4, "cache-read-tokens", &cache_read.to_string())?;
                    }
                    if let Some(cache_creation) = usage.cache_creation_input_tokens {
                        self.write_text_element(writer, 4, "cache-creation-tokens", &cache_creation.to_string())?;
                    }
                    self.write_close_tag(writer, 3, "usage")?;
                }
            }

            // Content
            self.write_open_tag(writer, 3, "content", &[])?;
            for block in &assistant.message.content {
                self.write_content_block(writer, 4, block, options)?;
            }
            self.write_close_tag(writer, 3, "content")?;

            self.write_close_tag(writer, 2, "message")?;
        }
        Ok(())
    }

    /// Write a content block.
    fn write_content_block<W: Write>(
        &self,
        writer: &mut W,
        depth: usize,
        block: &ContentBlock,
        options: &ExportOptions,
    ) -> Result<()> {
        match block {
            ContentBlock::Text(text) => {
                self.write_cdata(writer, depth, "text", &text.text)?;
            }
            ContentBlock::Thinking(thinking) => {
                if options.include_thinking {
                    self.write_thinking(writer, depth, thinking)?;
                }
            }
            ContentBlock::ToolUse(tool_use) => {
                if options.include_tool_use {
                    self.write_tool_use(writer, depth, tool_use)?;
                }
            }
            ContentBlock::ToolResult(result) => {
                if options.include_tool_results {
                    self.write_tool_result(writer, depth, result)?;
                }
            }
            ContentBlock::Image(image) => {
                if options.include_images {
                    let media_type = image.source.media_type().unwrap_or("image/unknown");
                    self.write_empty_tag(writer, depth, "image", &[("media-type", media_type)])?;
                }
            }
        }
        Ok(())
    }

    /// Write a thinking block.
    fn write_thinking<W: Write>(
        &self,
        writer: &mut W,
        depth: usize,
        thinking: &ThinkingBlock,
    ) -> Result<()> {
        // Use signature as a truncated identifier
        let id = if thinking.signature.len() > 16 {
            &thinking.signature[..16]
        } else {
            &thinking.signature
        };
        self.write_open_tag(writer, depth, "thinking", &[("signature", id)])?;
        self.write_cdata(writer, depth + 1, "content", &thinking.thinking)?;
        self.write_close_tag(writer, depth, "thinking")?;
        Ok(())
    }

    /// Write a tool use block.
    fn write_tool_use<W: Write>(
        &self,
        writer: &mut W,
        depth: usize,
        tool_use: &ToolUse,
    ) -> Result<()> {
        self.write_open_tag(
            writer,
            depth,
            "tool-use",
            &[("id", &tool_use.id), ("name", &tool_use.name)],
        )?;

        // Serialize input as JSON within CDATA
        let input_json = serde_json::to_string_pretty(&tool_use.input).unwrap_or_default();
        self.write_cdata(writer, depth + 1, "input", &input_json)?;

        self.write_close_tag(writer, depth, "tool-use")?;
        Ok(())
    }

    /// Write a tool result block.
    fn write_tool_result<W: Write>(
        &self,
        writer: &mut W,
        depth: usize,
        result: &ToolResult,
    ) -> Result<()> {
        let status = if result.is_explicit_error() {
            "error"
        } else {
            "success"
        };

        self.write_open_tag(
            writer,
            depth,
            "tool-result",
            &[("tool-use-id", &result.tool_use_id), ("status", status)],
        )?;

        if let Some(content) = &result.content {
            match content {
                ToolResultContent::String(s) => {
                    self.write_cdata(writer, depth + 1, "output", s)?;
                }
                ToolResultContent::Array(arr) => {
                    let json = serde_json::to_string_pretty(arr).unwrap_or_default();
                    self.write_cdata(writer, depth + 1, "output", &json)?;
                }
            }
        }

        self.write_close_tag(writer, depth, "tool-result")?;
        Ok(())
    }

    /// Write a system message.
    fn write_system_message<W: Write>(
        &self,
        writer: &mut W,
        entry: &LogEntry,
        options: &ExportOptions,
    ) -> Result<()> {
        if !options.include_system {
            return Ok(());
        }

        if let LogEntry::System(system) = entry {
            let uuid = entry.uuid().unwrap_or("");
            let timestamp = system.timestamp.format("%Y-%m-%dT%H:%M:%SZ").to_string();

            let subtype = system
                .subtype
                .as_ref()
                .map(|s| format!("{:?}", s))
                .unwrap_or_default();

            let mut attrs = vec![("type", "system")];
            if !subtype.is_empty() {
                attrs.push(("subtype", &subtype));
            }
            if options.include_metadata && !uuid.is_empty() {
                attrs.push(("uuid", uuid));
            }

            self.write_open_tag(writer, 2, "message", &attrs)?;

            if options.include_timestamps {
                self.write_text_element(writer, 3, "timestamp", &timestamp)?;
            }

            if let Some(content) = &system.content {
                self.write_cdata(writer, 3, "content", content)?;
            }

            self.write_close_tag(writer, 2, "message")?;
        }
        Ok(())
    }

    /// Write a summary message.
    fn write_summary_message<W: Write>(
        &self,
        writer: &mut W,
        entry: &LogEntry,
        options: &ExportOptions,
    ) -> Result<()> {
        if let LogEntry::Summary(summary) = entry {
            let uuid = entry.uuid().unwrap_or("");

            let mut attrs = vec![("type", "summary")];
            if options.include_metadata && !uuid.is_empty() {
                attrs.push(("uuid", uuid));
            }

            self.write_open_tag(writer, 2, "message", &attrs)?;

            self.write_cdata(writer, 3, "content", &summary.summary)?;

            if options.include_metadata {
                if let Some(leaf) = &summary.leaf_uuid {
                    self.write_text_element(writer, 3, "leaf-uuid", leaf)?;
                }
            }

            self.write_close_tag(writer, 2, "message")?;
        }
        Ok(())
    }
}

impl Exporter for XmlExporter {
    fn export_conversation<W: Write>(
        &self,
        conversation: &Conversation,
        writer: &mut W,
        options: &ExportOptions,
    ) -> Result<()> {
        // XML declaration
        if self.include_declaration {
            writeln!(writer, "<?xml version=\"1.0\" encoding=\"UTF-8\"?>")?;
        }

        // Root element
        let mut root_attrs = vec![];
        if self.include_schema {
            root_attrs.push((
                "xmlns:xsi",
                "http://www.w3.org/2001/XMLSchema-instance",
            ));
        }
        self.write_open_tag(writer, 0, "conversation", &root_attrs)?;

        // Metadata
        self.write_metadata(writer, conversation, options)?;

        // Messages
        self.write_open_tag(writer, 1, "messages", &[])?;

        let entries = if options.main_thread_only {
            conversation.main_thread_entries()
        } else {
            conversation.chronological_entries()
        };

        for entry in entries {
            match entry {
                LogEntry::User(_) => {
                    self.write_user_message(writer, entry, options)?;
                }
                LogEntry::Assistant(_) => {
                    self.write_assistant_message(writer, entry, options)?;
                }
                LogEntry::System(_) => {
                    self.write_system_message(writer, entry, options)?;
                }
                LogEntry::Summary(_) => {
                    self.write_summary_message(writer, entry, options)?;
                }
                LogEntry::FileHistorySnapshot(_)
                | LogEntry::QueueOperation(_)
                | LogEntry::TurnEnd(_) => {
                    // Skip these in XML export
                }
            }
        }

        self.write_close_tag(writer, 1, "messages")?;
        self.write_close_tag(writer, 0, "conversation")?;

        Ok(())
    }

    fn export_entries<W: Write>(
        &self,
        entries: &[LogEntry],
        writer: &mut W,
        options: &ExportOptions,
    ) -> Result<()> {
        // XML declaration
        if self.include_declaration {
            writeln!(writer, "<?xml version=\"1.0\" encoding=\"UTF-8\"?>")?;
        }

        self.write_open_tag(writer, 0, "entries", &[])?;

        for entry in entries {
            match entry {
                LogEntry::User(_) => {
                    self.write_user_message(writer, entry, options)?;
                }
                LogEntry::Assistant(_) => {
                    self.write_assistant_message(writer, entry, options)?;
                }
                LogEntry::System(_) => {
                    self.write_system_message(writer, entry, options)?;
                }
                LogEntry::Summary(_) => {
                    self.write_summary_message(writer, entry, options)?;
                }
                _ => {}
            }
        }

        self.write_close_tag(writer, 0, "entries")?;
        Ok(())
    }
}

/// Format a timestamp for XML.
fn format_timestamp(ts: &DateTime<Utc>) -> String {
    ts.format("%Y-%m-%dT%H:%M:%SZ").to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_xml_exporter_builder() {
        let exporter = XmlExporter::new()
            .pretty(false)
            .with_indent("\t")
            .with_declaration(false);

        assert!(!exporter.pretty);
        assert_eq!(exporter.indent, "\t");
        assert!(!exporter.include_declaration);
    }

    #[test]
    fn test_escape_text() {
        assert_eq!(XmlExporter::escape_text("hello"), "hello");
        assert_eq!(XmlExporter::escape_text("<test>"), "&lt;test&gt;");
        assert_eq!(XmlExporter::escape_text("a & b"), "a &amp; b");
        assert_eq!(XmlExporter::escape_text("\"quote\""), "&quot;quote&quot;");
    }

    #[test]
    fn test_escape_attr() {
        assert_eq!(XmlExporter::escape_attr("hello\nworld"), "hello&#10;world");
        assert_eq!(XmlExporter::escape_attr("a\tb"), "a&#9;b");
    }
}
