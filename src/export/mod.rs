//! Export functionality for conversations and sessions.
//!
//! This module provides various export formats:
//! - Markdown: Human-readable conversation transcripts
//! - JSON: Lossless structured data export
//! - HTML: Rich formatted output (future)
//! - Plain text: Simple unformatted output (future)
//!
//! All exporters support streaming output for large conversations
//! and configurable formatting options.

mod html;
mod json;
mod markdown;

pub use html::*;
pub use json::*;
pub use markdown::*;

use std::io::Write;
use std::path::Path;

use crate::error::{Result, SnatchError};
use crate::model::LogEntry;
use crate::reconstruction::Conversation;

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
    /// Plain text.
    Text,
    /// HTML (future).
    Html,
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
            _ => None,
        }
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
pub fn export_to_file(
    conversation: &Conversation,
    path: impl AsRef<Path>,
    format: ExportFormat,
    options: &ExportOptions,
) -> Result<()> {
    let path = path.as_ref();

    let file = std::fs::File::create(path).map_err(|e| {
        SnatchError::io(format!("Failed to create output file: {}", path.display()), e)
    })?;

    let mut writer = std::io::BufWriter::new(file);

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
            // Use markdown exporter with plain output for now
            let exporter = MarkdownExporter::new().plain_text(true);
            exporter.export_conversation(conversation, &mut writer, options)?;
        }
        ExportFormat::Html => {
            let exporter = HtmlExporter::new();
            exporter.export_conversation(conversation, &mut writer, options)?;
        }
    }

    writer.flush().map_err(|e| {
        SnatchError::io(format!("Failed to flush output file: {}", path.display()), e)
    })?;

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
            let exporter = MarkdownExporter::new().plain_text(true);
            exporter.export_conversation(conversation, &mut buffer, options)?;
        }
        ExportFormat::Html => {
            let exporter = HtmlExporter::new();
            exporter.export_conversation(conversation, &mut buffer, options)?;
        }
    }

    String::from_utf8(buffer).map_err(SnatchError::from)
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
}
