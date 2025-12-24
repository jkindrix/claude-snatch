//! CSV export for conversation data.
//!
//! Exports conversation data to CSV format for spreadsheet analysis.
//! Supports exporting messages, usage statistics, and tool invocations.

use std::io::Write;

use chrono::{DateTime, Utc};

use crate::analytics::SessionAnalytics;
use crate::error::Result;
use crate::model::{ContentBlock, LogEntry};
use crate::reconstruction::Conversation;

use super::{ExportOptions, Exporter};

/// CSV export mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CsvMode {
    /// Export messages with basic info.
    Messages,
    /// Export token usage statistics.
    Usage,
    /// Export tool invocations.
    Tools,
    /// Export all data in a single table.
    Full,
}

impl Default for CsvMode {
    fn default() -> Self {
        Self::Messages
    }
}

/// CSV exporter for conversation data.
#[derive(Debug, Clone)]
pub struct CsvExporter {
    /// Export mode.
    mode: CsvMode,
    /// Include header row.
    include_header: bool,
    /// Field delimiter.
    delimiter: char,
    /// Quote character.
    quote_char: char,
}

impl Default for CsvExporter {
    fn default() -> Self {
        Self::new()
    }
}

impl CsvExporter {
    /// Create a new CSV exporter.
    #[must_use]
    pub fn new() -> Self {
        Self {
            mode: CsvMode::Messages,
            include_header: true,
            delimiter: ',',
            quote_char: '"',
        }
    }

    /// Set the export mode.
    #[must_use]
    pub fn with_mode(mut self, mode: CsvMode) -> Self {
        self.mode = mode;
        self
    }

    /// Include or exclude header row.
    #[must_use]
    pub fn with_header(mut self, include: bool) -> Self {
        self.include_header = include;
        self
    }

    /// Set the field delimiter.
    #[must_use]
    pub fn with_delimiter(mut self, delim: char) -> Self {
        self.delimiter = delim;
        self
    }

    /// Escape a field value for CSV.
    fn escape_field(&self, value: &str) -> String {
        let needs_quoting = value.contains(self.delimiter)
            || value.contains(self.quote_char)
            || value.contains('\n')
            || value.contains('\r');

        if needs_quoting {
            let escaped = value.replace(
                self.quote_char,
                &format!("{}{}", self.quote_char, self.quote_char),
            );
            format!("{}{}{}", self.quote_char, escaped, self.quote_char)
        } else {
            value.to_string()
        }
    }

    /// Write a CSV row.
    fn write_row<W: Write>(&self, writer: &mut W, fields: &[&str]) -> Result<()> {
        let line: Vec<String> = fields.iter().map(|f| self.escape_field(f)).collect();
        writeln!(writer, "{}", line.join(&self.delimiter.to_string()))?;
        Ok(())
    }

    /// Export messages to CSV.
    fn export_messages<W: Write>(
        &self,
        writer: &mut W,
        conversation: &Conversation,
        options: &ExportOptions,
    ) -> Result<()> {
        // Header
        if self.include_header {
            self.write_row(
                writer,
                &[
                    "uuid",
                    "parent_uuid",
                    "type",
                    "timestamp",
                    "model",
                    "content_preview",
                    "input_tokens",
                    "output_tokens",
                    "tool_count",
                ],
            )?;
        }

        let entries = if options.main_thread_only {
            conversation.main_thread_entries()
        } else {
            conversation.chronological_entries()
        };

        for entry in entries {
            let uuid = entry.uuid().unwrap_or("");
            let parent_uuid = entry.parent_uuid().unwrap_or("");
            let timestamp = entry
                .timestamp()
                .map(|t| format_timestamp(&t))
                .unwrap_or_default();

            match entry {
                LogEntry::User(user) => {
                    let content = match &user.message {
                        crate::model::UserContent::Simple(s) => &s.content,
                        crate::model::UserContent::Blocks(b) => {
                            b.content.first().map(|c| content_preview(c)).unwrap_or("")
                        }
                    };
                    self.write_row(
                        writer,
                        &[
                            uuid,
                            parent_uuid,
                            "user",
                            &timestamp,
                            "",
                            &truncate(content, 100),
                            "",
                            "",
                            "",
                        ],
                    )?;
                }
                LogEntry::Assistant(assistant) => {
                    let content = assistant
                        .message
                        .content
                        .first()
                        .map(content_preview)
                        .unwrap_or("");
                    let model = &assistant.message.model;
                    let (input, output) = assistant
                        .message
                        .usage
                        .as_ref()
                        .map(|u| (u.total_input_tokens().to_string(), u.output_tokens.to_string()))
                        .unwrap_or_default();
                    let tool_count = assistant
                        .message
                        .content
                        .iter()
                        .filter(|c| matches!(c, ContentBlock::ToolUse(_)))
                        .count();

                    self.write_row(
                        writer,
                        &[
                            uuid,
                            parent_uuid,
                            "assistant",
                            &timestamp,
                            model,
                            &truncate(content, 100),
                            &input,
                            &output,
                            &tool_count.to_string(),
                        ],
                    )?;
                }
                LogEntry::System(_) => {
                    if options.include_system {
                        self.write_row(
                            writer,
                            &[uuid, parent_uuid, "system", &timestamp, "", "", "", "", ""],
                        )?;
                    }
                }
                LogEntry::Summary(summary) => {
                    self.write_row(
                        writer,
                        &[
                            uuid,
                            parent_uuid,
                            "summary",
                            &timestamp,
                            "",
                            &truncate(&summary.summary, 100),
                            "",
                            "",
                            "",
                        ],
                    )?;
                }
                _ => {}
            }
        }

        Ok(())
    }

    /// Export usage statistics to CSV.
    fn export_usage<W: Write>(
        &self,
        writer: &mut W,
        conversation: &Conversation,
    ) -> Result<()> {
        let analytics = SessionAnalytics::from_conversation(conversation);
        let summary = analytics.summary_report();

        // Header
        if self.include_header {
            self.write_row(
                writer,
                &[
                    "metric",
                    "value",
                ],
            )?;
        }

        self.write_row(writer, &["total_messages", &summary.total_messages.to_string()])?;
        self.write_row(writer, &["user_messages", &summary.user_messages.to_string()])?;
        self.write_row(writer, &["assistant_messages", &summary.assistant_messages.to_string()])?;
        self.write_row(writer, &["total_tokens", &summary.total_tokens.to_string()])?;
        self.write_row(writer, &["input_tokens", &summary.input_tokens.to_string()])?;
        self.write_row(writer, &["output_tokens", &summary.output_tokens.to_string()])?;
        self.write_row(writer, &["cache_hit_rate", &format!("{:.2}", summary.cache_hit_rate)])?;
        self.write_row(writer, &["tool_invocations", &summary.tool_invocations.to_string()])?;
        self.write_row(writer, &["thinking_blocks", &summary.thinking_blocks.to_string()])?;

        if let Some(cost) = summary.estimated_cost {
            self.write_row(writer, &["estimated_cost_usd", &format!("{:.4}", cost)])?;
        }

        if let Some(model) = &summary.primary_model {
            self.write_row(writer, &["primary_model", model])?;
        }

        Ok(())
    }

    /// Export tool invocations to CSV.
    fn export_tools<W: Write>(
        &self,
        writer: &mut W,
        conversation: &Conversation,
        options: &ExportOptions,
    ) -> Result<()> {
        // Header
        if self.include_header {
            self.write_row(
                writer,
                &[
                    "message_uuid",
                    "tool_id",
                    "tool_name",
                    "input_preview",
                    "is_mcp",
                    "is_server_tool",
                ],
            )?;
        }

        let entries = if options.main_thread_only {
            conversation.main_thread_entries()
        } else {
            conversation.chronological_entries()
        };

        for entry in entries {
            if let LogEntry::Assistant(assistant) = entry {
                let uuid = entry.uuid().unwrap_or("");
                for content in &assistant.message.content {
                    if let ContentBlock::ToolUse(tool_use) = content {
                        let input = serde_json::to_string(&tool_use.input).unwrap_or_default();
                        self.write_row(
                            writer,
                            &[
                                uuid,
                                &tool_use.id,
                                &tool_use.name,
                                &truncate(&input, 200),
                                &tool_use.is_mcp_tool().to_string(),
                                &tool_use.is_server_tool().to_string(),
                            ],
                        )?;
                    }
                }
            }
        }

        Ok(())
    }
}

impl Exporter for CsvExporter {
    fn export_conversation<W: Write>(
        &self,
        conversation: &Conversation,
        writer: &mut W,
        options: &ExportOptions,
    ) -> Result<()> {
        match self.mode {
            CsvMode::Messages => self.export_messages(writer, conversation, options),
            CsvMode::Usage => self.export_usage(writer, conversation),
            CsvMode::Tools => self.export_tools(writer, conversation, options),
            CsvMode::Full => {
                // Export all modes with separators
                writeln!(writer, "# Messages")?;
                self.export_messages(writer, conversation, options)?;
                writeln!(writer)?;
                writeln!(writer, "# Usage")?;
                self.export_usage(writer, conversation)?;
                writeln!(writer)?;
                writeln!(writer, "# Tools")?;
                self.export_tools(writer, conversation, options)?;
                Ok(())
            }
        }
    }

    fn export_entries<W: Write>(
        &self,
        entries: &[LogEntry],
        writer: &mut W,
        options: &ExportOptions,
    ) -> Result<()> {
        // For raw entries, use messages mode
        if self.include_header {
            self.write_row(
                writer,
                &[
                    "uuid",
                    "parent_uuid",
                    "type",
                    "timestamp",
                    "content_preview",
                ],
            )?;
        }

        for entry in entries {
            let uuid = entry.uuid().unwrap_or("");
            let parent_uuid = entry.parent_uuid().unwrap_or("");
            let timestamp = entry
                .timestamp()
                .map(|t| format_timestamp(&t))
                .unwrap_or_default();
            let entry_type = match entry {
                LogEntry::User(_) => "user",
                LogEntry::Assistant(_) => "assistant",
                LogEntry::System(_) => "system",
                LogEntry::Summary(_) => "summary",
                LogEntry::FileHistorySnapshot(_) => "file_history",
                LogEntry::QueueOperation(_) => "queue",
                LogEntry::TurnEnd(_) => "turn_end",
            };

            if !options.include_system && matches!(entry, LogEntry::System(_)) {
                continue;
            }

            self.write_row(writer, &[uuid, parent_uuid, entry_type, &timestamp, ""])?;
        }

        Ok(())
    }
}

/// Format a timestamp for CSV.
fn format_timestamp(ts: &DateTime<Utc>) -> String {
    ts.format("%Y-%m-%dT%H:%M:%SZ").to_string()
}

/// Get a preview of content block.
fn content_preview(block: &ContentBlock) -> &str {
    match block {
        ContentBlock::Text(t) => &t.text,
        ContentBlock::Thinking(_) => "[thinking]",
        ContentBlock::ToolUse(t) => &t.name,
        ContentBlock::ToolResult(_) => "[result]",
        ContentBlock::Image(_) => "[image]",
    }
}

/// Truncate a string to max length.
fn truncate(s: &str, max_len: usize) -> String {
    if s.len() <= max_len {
        s.replace('\n', " ").replace('\r', "")
    } else {
        format!("{}...", s[..max_len].replace('\n', " ").replace('\r', ""))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_csv_exporter_builder() {
        let exporter = CsvExporter::new()
            .with_mode(CsvMode::Tools)
            .with_header(false)
            .with_delimiter('\t');

        assert_eq!(exporter.mode, CsvMode::Tools);
        assert!(!exporter.include_header);
        assert_eq!(exporter.delimiter, '\t');
    }

    #[test]
    fn test_escape_field() {
        let exporter = CsvExporter::new();

        assert_eq!(exporter.escape_field("simple"), "simple");
        assert_eq!(exporter.escape_field("with,comma"), "\"with,comma\"");
        assert_eq!(exporter.escape_field("with\"quote"), "\"with\"\"quote\"");
        assert_eq!(exporter.escape_field("with\nnewline"), "\"with\nnewline\"");
    }

    #[test]
    fn test_truncate() {
        assert_eq!(truncate("short", 10), "short");
        assert_eq!(truncate("this is longer", 10), "this is lo...");
        assert_eq!(truncate("with\nnewline", 20), "with newline");
    }
}
