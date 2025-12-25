//! Export functionality for conversations and sessions.
//!
//! This module provides various export formats:
//! - Markdown: Human-readable conversation transcripts
//! - JSON: Lossless structured data export
//! - HTML: Rich formatted output
//! - Plain text: Simple formatted output with word wrapping
//! - CSV: Spreadsheet-compatible tabular data
//! - XML: Structured markup for integration
//!
//! All exporters support streaming output for large conversations
//! and configurable formatting options.

mod csv;
mod html;
mod json;
mod markdown;
pub mod schema;
mod sqlite;
mod text;
mod xml;

pub use csv::*;
pub use html::*;
pub use json::*;
pub use markdown::*;
pub use schema::{
    entry_schema, entry_schema_string, export_schema, export_schema_string,
    validate_entries, validate_export, SchemaValidator, ValidationResult,
};
pub use sqlite::*;
pub use text::*;
pub use xml::*;

use std::io::Write;
use std::path::Path;

use crate::error::{Result, SnatchError};
use crate::model::LogEntry;
use crate::reconstruction::Conversation;
use crate::util::AtomicFile;

/// Common export options shared across formats.
#[derive(Debug, Clone)]
pub struct ExportOptions {
    /// Include thinking blocks in output.
    pub include_thinking: bool,
    /// Include tool use details.
    pub include_tool_use: bool,
    /// Include tool results.
    pub include_tool_results: bool,
    /// Include system messages.
    pub include_system: bool,
    /// Include timestamps.
    pub include_timestamps: bool,
    /// Use relative timestamps (e.g., "2 hours ago").
    pub relative_timestamps: bool,
    /// Include usage statistics.
    pub include_usage: bool,
    /// Include metadata (UUIDs, session IDs, etc.).
    pub include_metadata: bool,
    /// Include images (base64 or references).
    pub include_images: bool,
    /// Maximum depth for nested content.
    pub max_depth: Option<usize>,
    /// Truncate long content at this length.
    pub truncate_at: Option<usize>,
    /// Include branch/sidechain content.
    pub include_branches: bool,
    /// Only export main thread.
    pub main_thread_only: bool,
}

impl Default for ExportOptions {
    fn default() -> Self {
        Self {
            include_thinking: true,
            include_tool_use: true,
            include_tool_results: true,
            include_system: false,
            include_timestamps: true,
            relative_timestamps: false,
            include_usage: true,
            include_metadata: false,
            include_images: true,
            max_depth: None,
            truncate_at: None,
            include_branches: false,
            main_thread_only: true,
        }
    }
}

impl ExportOptions {
    /// Create options for full export (all content).
    #[must_use]
    pub fn full() -> Self {
        Self {
            include_thinking: true,
            include_tool_use: true,
            include_tool_results: true,
            include_system: true,
            include_timestamps: true,
            relative_timestamps: false,
            include_usage: true,
            include_metadata: true,
            include_images: true,
            max_depth: None,
            truncate_at: None,
            include_branches: true,
            main_thread_only: false,
        }
    }

    /// Create options for minimal export (conversation only).
    #[must_use]
    pub fn minimal() -> Self {
        Self {
            include_thinking: false,
            include_tool_use: false,
            include_tool_results: false,
            include_system: false,
            include_timestamps: false,
            relative_timestamps: false,
            include_usage: false,
            include_metadata: false,
            include_images: false,
            max_depth: None,
            truncate_at: None,
            include_branches: false,
            main_thread_only: true,
        }
    }

    /// Builder: include thinking blocks.
    #[must_use]
    pub fn with_thinking(mut self, include: bool) -> Self {
        self.include_thinking = include;
        self
    }

    /// Builder: include tool use.
    #[must_use]
    pub fn with_tool_use(mut self, include: bool) -> Self {
        self.include_tool_use = include;
        self
    }

    /// Builder: include metadata.
    #[must_use]
    pub fn with_metadata(mut self, include: bool) -> Self {
        self.include_metadata = include;
        self
    }

    /// Builder: include branches.
    #[must_use]
    pub fn with_branches(mut self, include: bool) -> Self {
        self.include_branches = include;
        self.main_thread_only = !include;
        self
    }

    /// Builder: use relative timestamps (e.g., "2 hours ago").
    #[must_use]
    pub fn with_relative_timestamps(mut self, relative: bool) -> Self {
        self.relative_timestamps = relative;
        self
    }
}

/// Export format specification.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExportFormat {
    /// Markdown format.
    Markdown,
    /// JSON format (lossless).
    Json,
    /// Pretty-printed JSON.
    JsonPretty,
    /// Plain text with word wrapping.
    Text,
    /// HTML formatted output.
    Html,
    /// CSV tabular data.
    Csv,
    /// XML structured markup.
    Xml,
    /// SQLite database.
    Sqlite,
}

impl ExportFormat {
    /// Get the file extension for this format.
    #[must_use]
    pub const fn extension(&self) -> &'static str {
        match self {
            Self::Markdown => "md",
            Self::Json | Self::JsonPretty => "json",
            Self::Text => "txt",
            Self::Html => "html",
            Self::Csv => "csv",
            Self::Xml => "xml",
            Self::Sqlite => "db",
        }
    }

    /// Parse format from string.
    #[must_use]
    pub fn from_str(s: &str) -> Option<Self> {
        match s.to_lowercase().as_str() {
            "markdown" | "md" => Some(Self::Markdown),
            "json" => Some(Self::Json),
            "json-pretty" | "jsonpretty" => Some(Self::JsonPretty),
            "text" | "txt" => Some(Self::Text),
            "html" => Some(Self::Html),
            "csv" => Some(Self::Csv),
            "xml" => Some(Self::Xml),
            "sqlite" | "db" | "sql" => Some(Self::Sqlite),
            _ => None,
        }
    }

    /// Check if this format requires a file (cannot write to stdout).
    #[must_use]
    pub const fn requires_file(&self) -> bool {
        matches!(self, Self::Sqlite)
    }
}

/// Trait for exporters.
pub trait Exporter {
    /// Export a conversation to the writer.
    fn export_conversation<W: Write>(
        &self,
        conversation: &Conversation,
        writer: &mut W,
        options: &ExportOptions,
    ) -> Result<()>;

    /// Export raw entries to the writer.
    fn export_entries<W: Write>(
        &self,
        entries: &[LogEntry],
        writer: &mut W,
        options: &ExportOptions,
    ) -> Result<()>;
}

/// Export a conversation to a file.
///
/// This function uses atomic file writes to ensure data integrity.
/// Content is written to a temporary file first, then atomically
/// renamed to the target path.
pub fn export_to_file(
    conversation: &Conversation,
    path: impl AsRef<Path>,
    format: ExportFormat,
    options: &ExportOptions,
) -> Result<()> {
    let path = path.as_ref();

    // SQLite handles its own file creation
    if matches!(format, ExportFormat::Sqlite) {
        let exporter = SqliteExporter::new();
        return exporter.export_to_file(conversation, path, options);
    }

    // Use atomic file writing for all other formats
    let mut atomic = AtomicFile::create(path)?;
    let mut writer = std::io::BufWriter::new(atomic.writer());

    match format {
        ExportFormat::Markdown => {
            let exporter = MarkdownExporter::new();
            exporter.export_conversation(conversation, &mut writer, options)?;
        }
        ExportFormat::Json => {
            let exporter = JsonExporter::new();
            exporter.export_conversation(conversation, &mut writer, options)?;
        }
        ExportFormat::JsonPretty => {
            let exporter = JsonExporter::new().pretty(true);
            exporter.export_conversation(conversation, &mut writer, options)?;
        }
        ExportFormat::Text => {
            let exporter = TextExporter::new();
            exporter.export_conversation(conversation, &mut writer, options)?;
        }
        ExportFormat::Html => {
            let exporter = HtmlExporter::new();
            exporter.export_conversation(conversation, &mut writer, options)?;
        }
        ExportFormat::Csv => {
            let exporter = CsvExporter::new();
            exporter.export_conversation(conversation, &mut writer, options)?;
        }
        ExportFormat::Xml => {
            let exporter = XmlExporter::new();
            exporter.export_conversation(conversation, &mut writer, options)?;
        }
        ExportFormat::Sqlite => {
            unreachable!("SQLite handled above");
        }
    }

    // Flush the BufWriter before finishing atomic write
    writer.flush().map_err(|e| {
        SnatchError::io(format!("Failed to flush output file: {}", path.display()), e)
    })?;

    // Drop the BufWriter to release the borrow on atomic.writer()
    drop(writer);

    // Complete the atomic write
    atomic.finish()?;

    Ok(())
}

/// Export a conversation to a string.
pub fn export_to_string(
    conversation: &Conversation,
    format: ExportFormat,
    options: &ExportOptions,
) -> Result<String> {
    let mut buffer = Vec::new();

    match format {
        ExportFormat::Markdown => {
            let exporter = MarkdownExporter::new();
            exporter.export_conversation(conversation, &mut buffer, options)?;
        }
        ExportFormat::Json => {
            let exporter = JsonExporter::new();
            exporter.export_conversation(conversation, &mut buffer, options)?;
        }
        ExportFormat::JsonPretty => {
            let exporter = JsonExporter::new().pretty(true);
            exporter.export_conversation(conversation, &mut buffer, options)?;
        }
        ExportFormat::Text => {
            let exporter = TextExporter::new();
            exporter.export_conversation(conversation, &mut buffer, options)?;
        }
        ExportFormat::Html => {
            let exporter = HtmlExporter::new();
            exporter.export_conversation(conversation, &mut buffer, options)?;
        }
        ExportFormat::Csv => {
            let exporter = CsvExporter::new();
            exporter.export_conversation(conversation, &mut buffer, options)?;
        }
        ExportFormat::Xml => {
            let exporter = XmlExporter::new();
            exporter.export_conversation(conversation, &mut buffer, options)?;
        }
        ExportFormat::Sqlite => {
            return Err(SnatchError::export(
                "SQLite export requires a file path, not a string buffer",
            ));
        }
    }

    String::from_utf8(buffer).map_err(SnatchError::from)
}

use chrono::{DateTime, Utc};

/// Format a timestamp, optionally as relative time.
///
/// If `relative` is true, returns human-readable relative time like "2 hours ago".
/// Otherwise returns ISO-8601 formatted timestamp.
pub fn format_timestamp(ts: &DateTime<Utc>, relative: bool) -> String {
    if relative {
        format_relative_time(ts)
    } else {
        ts.format("%Y-%m-%d %H:%M:%S UTC").to_string()
    }
}

/// Format a timestamp as relative time (e.g., "2 hours ago", "yesterday").
pub fn format_relative_time(ts: &DateTime<Utc>) -> String {
    let now = Utc::now();
    let duration = now.signed_duration_since(*ts);

    if duration.num_seconds() < 0 {
        // Future time
        let abs = -duration.num_seconds();
        if abs < 60 {
            return "in a moment".to_string();
        } else if abs < 3600 {
            let mins = abs / 60;
            return format!("in {} minute{}", mins, if mins == 1 { "" } else { "s" });
        } else if abs < 86400 {
            let hours = abs / 3600;
            return format!("in {} hour{}", hours, if hours == 1 { "" } else { "s" });
        } else {
            let days = abs / 86400;
            return format!("in {} day{}", days, if days == 1 { "" } else { "s" });
        }
    }

    let secs = duration.num_seconds();

    if secs < 60 {
        "just now".to_string()
    } else if secs < 3600 {
        let mins = secs / 60;
        format!("{} minute{} ago", mins, if mins == 1 { "" } else { "s" })
    } else if secs < 86400 {
        let hours = secs / 3600;
        format!("{} hour{} ago", hours, if hours == 1 { "" } else { "s" })
    } else if secs < 172800 {
        "yesterday".to_string()
    } else if secs < 604800 {
        let days = secs / 86400;
        format!("{} days ago", days)
    } else if secs < 2592000 {
        let weeks = secs / 604800;
        format!("{} week{} ago", weeks, if weeks == 1 { "" } else { "s" })
    } else if secs < 31536000 {
        let months = secs / 2592000;
        format!("{} month{} ago", months, if months == 1 { "" } else { "s" })
    } else {
        let years = secs / 31536000;
        format!("{} year{} ago", years, if years == 1 { "" } else { "s" })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_export_format_extension() {
        assert_eq!(ExportFormat::Markdown.extension(), "md");
        assert_eq!(ExportFormat::Json.extension(), "json");
        assert_eq!(ExportFormat::Text.extension(), "txt");
    }

    #[test]
    fn test_export_format_from_str() {
        assert_eq!(ExportFormat::from_str("markdown"), Some(ExportFormat::Markdown));
        assert_eq!(ExportFormat::from_str("MD"), Some(ExportFormat::Markdown));
        assert_eq!(ExportFormat::from_str("json"), Some(ExportFormat::Json));
        assert_eq!(ExportFormat::from_str("invalid"), None);
    }

    #[test]
    fn test_export_options_builders() {
        let opts = ExportOptions::default()
            .with_thinking(false)
            .with_metadata(true);

        assert!(!opts.include_thinking);
        assert!(opts.include_metadata);
    }

    #[test]
    fn test_relative_timestamps_builder() {
        let opts = ExportOptions::default().with_relative_timestamps(true);
        assert!(opts.relative_timestamps);
    }

    #[test]
    fn test_format_timestamp_absolute() {
        use chrono::TimeZone;
        let ts = Utc.with_ymd_and_hms(2025, 12, 24, 10, 30, 0).unwrap();
        let result = format_timestamp(&ts, false);
        assert_eq!(result, "2025-12-24 10:30:00 UTC");
    }

    #[test]
    fn test_format_relative_time_just_now() {
        let ts = Utc::now();
        let result = format_relative_time(&ts);
        assert_eq!(result, "just now");
    }

    #[test]
    fn test_format_relative_time_minutes() {
        use chrono::Duration;
        let ts = Utc::now() - Duration::minutes(5);
        let result = format_relative_time(&ts);
        assert_eq!(result, "5 minutes ago");
    }

    #[test]
    fn test_format_relative_time_hours() {
        use chrono::Duration;
        let ts = Utc::now() - Duration::hours(3);
        let result = format_relative_time(&ts);
        assert_eq!(result, "3 hours ago");
    }

    #[test]
    fn test_format_relative_time_yesterday() {
        use chrono::Duration;
        let ts = Utc::now() - Duration::hours(30);
        let result = format_relative_time(&ts);
        assert_eq!(result, "yesterday");
    }

    #[test]
    fn test_format_relative_time_days() {
        use chrono::Duration;
        let ts = Utc::now() - Duration::days(4);
        let result = format_relative_time(&ts);
        assert_eq!(result, "4 days ago");
    }

    #[test]
    fn test_format_relative_time_weeks() {
        use chrono::Duration;
        let ts = Utc::now() - Duration::weeks(2);
        let result = format_relative_time(&ts);
        assert_eq!(result, "2 weeks ago");
    }
}
