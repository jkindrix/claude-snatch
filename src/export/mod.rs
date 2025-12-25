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
    /// Configuration for sensitive data redaction.
    pub redaction: Option<crate::util::RedactionConfig>,
    /// Data minimization settings for privacy-conscious exports.
    pub minimization: Option<DataMinimizationConfig>,
}

/// Configuration for data minimization in shared exports.
///
/// This helps prepare exports for sharing by stripping or anonymizing
/// potentially identifying or sensitive structural information.
#[derive(Debug, Clone, Default)]
pub struct DataMinimizationConfig {
    /// Anonymize file paths (replace with generic paths).
    pub anonymize_paths: bool,
    /// Strip working directory information.
    pub strip_cwd: bool,
    /// Strip git branch/repository information.
    pub strip_git_info: bool,
    /// Anonymize session IDs (replace with sequential numbers).
    pub anonymize_session_ids: bool,
    /// Strip project names/paths.
    pub strip_project_info: bool,
    /// Strip user-identifying information from tool calls.
    pub strip_user_info: bool,
    /// Generalize timestamps (round to hour/day).
    pub generalize_timestamps: bool,
    /// Strip model names.
    pub strip_model_names: bool,
}

impl DataMinimizationConfig {
    /// Create config with no minimization.
    pub fn none() -> Self {
        Self::default()
    }

    /// Create config for maximum minimization (all options enabled).
    pub fn full() -> Self {
        Self {
            anonymize_paths: true,
            strip_cwd: true,
            strip_git_info: true,
            anonymize_session_ids: true,
            strip_project_info: true,
            strip_user_info: true,
            generalize_timestamps: true,
            strip_model_names: false, // Keep model names by default
        }
    }

    /// Create config suitable for public sharing.
    ///
    /// Anonymizes paths and IDs, strips location info, but keeps
    /// timestamps and model info for context.
    pub fn for_sharing() -> Self {
        Self {
            anonymize_paths: true,
            strip_cwd: true,
            strip_git_info: true,
            anonymize_session_ids: true,
            strip_project_info: true,
            strip_user_info: true,
            generalize_timestamps: false,
            strip_model_names: false,
        }
    }

    /// Check if any minimization is enabled.
    pub fn is_enabled(&self) -> bool {
        self.anonymize_paths
            || self.strip_cwd
            || self.strip_git_info
            || self.anonymize_session_ids
            || self.strip_project_info
            || self.strip_user_info
            || self.generalize_timestamps
            || self.strip_model_names
    }

    /// Anonymize a file path.
    pub fn anonymize_path(&self, path: &str) -> String {
        if !self.anonymize_paths {
            return path.to_string();
        }
        // Replace path with just filename and generic prefix
        let file_name = std::path::Path::new(path)
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("file");
        format!("/project/{}", file_name)
    }

    /// Anonymize a session ID.
    pub fn anonymize_session_id(&self, original: &str, counter: usize) -> String {
        if !self.anonymize_session_ids {
            return original.to_string();
        }
        format!("session-{:04}", counter)
    }

    /// Generalize a timestamp (round to nearest hour).
    pub fn generalize_timestamp(&self, ts: chrono::DateTime<chrono::Utc>) -> chrono::DateTime<chrono::Utc> {
        if !self.generalize_timestamps {
            return ts;
        }
        use chrono::Timelike;
        ts.with_minute(0).unwrap_or(ts)
            .with_second(0).unwrap_or(ts)
            .with_nanosecond(0).unwrap_or(ts)
    }
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
            redaction: None,
            minimization: None,
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
            redaction: None,
            minimization: None,
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
            redaction: None,
            minimization: None,
        }
    }

    /// Create options suitable for public sharing.
    ///
    /// This applies both data minimization and security redaction
    /// to prepare the export for safe sharing.
    #[must_use]
    pub fn for_sharing() -> Self {
        Self {
            include_thinking: false,
            include_tool_use: true,
            include_tool_results: false,
            include_system: false,
            include_timestamps: true,
            relative_timestamps: true,
            include_usage: false,
            include_metadata: false,
            include_images: false,
            max_depth: None,
            truncate_at: None,
            include_branches: false,
            main_thread_only: true,
            redaction: Some(crate::util::RedactionConfig::security()),
            minimization: Some(DataMinimizationConfig::for_sharing()),
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

    /// Builder: set redaction configuration.
    ///
    /// When set, sensitive data (API keys, passwords, emails, etc.) will be
    /// redacted from the exported content based on the configuration.
    #[must_use]
    pub fn with_redaction(mut self, config: crate::util::RedactionConfig) -> Self {
        self.redaction = Some(config);
        self
    }

    /// Builder: enable security-focused redaction.
    ///
    /// This enables redaction of API keys, passwords, credit cards, SSN,
    /// AWS keys, and URL credentials. Emails, IP addresses, and phone
    /// numbers are not redacted by default.
    #[must_use]
    pub fn with_security_redaction(mut self) -> Self {
        self.redaction = Some(crate::util::RedactionConfig::security());
        self
    }

    /// Builder: enable full redaction of all sensitive data types.
    #[must_use]
    pub fn with_full_redaction(mut self) -> Self {
        self.redaction = Some(crate::util::RedactionConfig::all());
        self
    }

    /// Apply redaction to text if configured.
    ///
    /// Returns the original text if no redaction is configured, otherwise
    /// returns the redacted text.
    #[must_use]
    pub fn redact<'a>(&self, text: &'a str) -> std::borrow::Cow<'a, str> {
        match &self.redaction {
            Some(config) if config.is_enabled() => crate::util::redact_sensitive(text, config),
            _ => std::borrow::Cow::Borrowed(text),
        }
    }

    /// Builder: set data minimization configuration.
    #[must_use]
    pub fn with_minimization(mut self, config: DataMinimizationConfig) -> Self {
        self.minimization = Some(config);
        self
    }

    /// Builder: enable minimization for public sharing.
    #[must_use]
    pub fn with_sharing_minimization(mut self) -> Self {
        self.minimization = Some(DataMinimizationConfig::for_sharing());
        self
    }

    /// Builder: enable full minimization.
    #[must_use]
    pub fn with_full_minimization(mut self) -> Self {
        self.minimization = Some(DataMinimizationConfig::full());
        self
    }

    /// Check if minimization is enabled.
    #[must_use]
    pub fn has_minimization(&self) -> bool {
        self.minimization.as_ref().map_or(false, |m| m.is_enabled())
    }

    /// Apply minimization to a file path if configured.
    #[must_use]
    pub fn minimize_path(&self, path: &str) -> String {
        match &self.minimization {
            Some(config) if config.anonymize_paths => config.anonymize_path(path),
            _ => path.to_string(),
        }
    }

    /// Apply minimization to a session ID if configured.
    #[must_use]
    pub fn minimize_session_id(&self, session_id: &str, counter: usize) -> String {
        match &self.minimization {
            Some(config) if config.anonymize_session_ids => config.anonymize_session_id(session_id, counter),
            _ => session_id.to_string(),
        }
    }

    /// Check if git info should be stripped.
    #[must_use]
    pub fn should_strip_git_info(&self) -> bool {
        self.minimization.as_ref().map_or(false, |m| m.strip_git_info)
    }

    /// Check if project info should be stripped.
    #[must_use]
    pub fn should_strip_project_info(&self) -> bool {
        self.minimization.as_ref().map_or(false, |m| m.strip_project_info)
    }

    /// Check if CWD should be stripped.
    #[must_use]
    pub fn should_strip_cwd(&self) -> bool {
        self.minimization.as_ref().map_or(false, |m| m.strip_cwd)
    }

    /// Check if model names should be stripped.
    #[must_use]
    pub fn should_strip_model(&self) -> bool {
        self.minimization.as_ref().map_or(false, |m| m.strip_model_names)
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

    #[test]
    fn test_data_minimization_config_none() {
        let config = DataMinimizationConfig::none();
        assert!(!config.is_enabled());
    }

    #[test]
    fn test_data_minimization_config_full() {
        let config = DataMinimizationConfig::full();
        assert!(config.is_enabled());
        assert!(config.anonymize_paths);
        assert!(config.strip_cwd);
        assert!(config.strip_git_info);
        assert!(config.anonymize_session_ids);
    }

    #[test]
    fn test_data_minimization_config_for_sharing() {
        let config = DataMinimizationConfig::for_sharing();
        assert!(config.is_enabled());
        assert!(config.anonymize_paths);
        assert!(!config.generalize_timestamps); // Keeps timestamps for context
        assert!(!config.strip_model_names); // Keeps model names
    }

    #[test]
    fn test_data_minimization_anonymize_path() {
        let config = DataMinimizationConfig::full();
        let result = config.anonymize_path("/home/user/projects/secret-project/src/main.rs");
        assert_eq!(result, "/project/main.rs");
        assert!(!result.contains("user"));
        assert!(!result.contains("secret-project"));
    }

    #[test]
    fn test_data_minimization_anonymize_session_id() {
        let config = DataMinimizationConfig::full();
        let result = config.anonymize_session_id("01930a2e-3f4b-7c8d-9e0f-123456789abc", 5);
        assert_eq!(result, "session-0005");
        assert!(!result.contains("01930a2e"));
    }

    #[test]
    fn test_data_minimization_generalize_timestamp() {
        use chrono::{TimeZone, Timelike};
        let config = DataMinimizationConfig::full();
        let ts = Utc.with_ymd_and_hms(2025, 12, 24, 10, 45, 32).unwrap();
        let result = config.generalize_timestamp(ts);
        assert_eq!(result.minute(), 0);
        assert_eq!(result.second(), 0);
        assert_eq!(result.hour(), 10); // Hour preserved
    }

    #[test]
    fn test_export_options_for_sharing() {
        let opts = ExportOptions::for_sharing();
        assert!(opts.redaction.is_some());
        assert!(opts.minimization.is_some());
        assert!(!opts.include_thinking);
        assert!(opts.relative_timestamps);
    }

    #[test]
    fn test_export_options_minimize_path() {
        let opts = ExportOptions::default().with_sharing_minimization();
        let result = opts.minimize_path("/home/user/code/app.py");
        assert_eq!(result, "/project/app.py");
    }

    #[test]
    fn test_export_options_minimize_session_id() {
        let opts = ExportOptions::default().with_sharing_minimization();
        let result = opts.minimize_session_id("abc123-def456", 7);
        assert_eq!(result, "session-0007");
    }

    #[test]
    fn test_export_options_no_minimization() {
        let opts = ExportOptions::default();
        assert!(!opts.has_minimization());
        // Without minimization, paths are unchanged
        let result = opts.minimize_path("/home/user/code/app.py");
        assert_eq!(result, "/home/user/code/app.py");
    }
}
