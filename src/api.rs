//! High-level programmatic API for claude-snatch.
//!
//! This module provides a clean, ergonomic API for common operations
//! without needing to understand the internal module structure.
//!
//! # Quick Start
//!
//! ```rust,no_run
//! use claude_snatch::api::{SnatchClient, ExportFormat};
//!
//! fn main() -> claude_snatch::Result<()> {
//!     // Create a client that auto-discovers Claude Code data
//!     let client = SnatchClient::discover()?;
//!
//!     // List all projects
//!     for project in client.projects()? {
//!         println!("Project: {}", project.path);
//!     }
//!
//!     // List recent sessions and export the first one
//!     let sessions = client.recent_sessions(10)?;
//!     for session in &sessions {
//!         println!("Session: {} ({} messages)", session.id, session.message_count);
//!     }
//!
//!     // Export the first session to markdown (if any exist)
//!     if let Some(session) = sessions.first() {
//!         let markdown = client.export_session(&session.id, ExportFormat::Markdown)?;
//!         println!("{}", markdown);
//!     }
//!
//!     Ok(())
//! }
//! ```
//!
//! # Features
//!
//! - Auto-discovery of Claude Code data directories
//! - Simple session listing and filtering
//! - Export to multiple formats (Markdown, JSON, HTML, etc.)
//! - Analytics and statistics
//! - Session comparison

use std::io::Cursor;
use std::path::Path;

use crate::analytics::{SessionAnalytics, SessionDiff};
use crate::discovery::{ClaudeDirectory, Session, SessionFilter};
use crate::error::{Result, SnatchError};
use crate::export::{
    CsvExporter, Exporter, ExportOptions, HtmlExporter, JsonExporter,
    MarkdownExporter, TextExporter, XmlExporter,
};
use crate::model::LogEntry;
use crate::parser::JsonlParser;
use crate::reconstruction::Conversation;

/// High-level client for claude-snatch operations.
///
/// This is the main entry point for programmatic use of the library.
#[derive(Debug)]
pub struct SnatchClient {
    claude_dir: ClaudeDirectory,
}

/// Available export formats.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExportFormat {
    /// Markdown format (human-readable)
    Markdown,
    /// JSON format (lossless, machine-readable)
    Json,
    /// Pretty-printed JSON
    JsonPretty,
    /// HTML format (self-contained, viewable in browser)
    Html,
    /// Plain text format
    Text,
    /// CSV format (spreadsheet-compatible)
    Csv,
    /// XML format
    Xml,
}

/// Simplified project information.
#[derive(Debug, Clone)]
pub struct ProjectInfo {
    /// Decoded project path.
    pub path: String,
    /// Number of sessions in this project.
    pub session_count: usize,
}

/// Simplified session information.
#[derive(Debug, Clone)]
pub struct SessionInfo {
    /// Session ID.
    pub id: String,
    /// Project path this session belongs to.
    pub project_path: String,
    /// Number of messages in the session.
    pub message_count: usize,
    /// Whether this is a subagent session.
    pub is_subagent: bool,
}

/// Session analytics summary.
#[derive(Debug, Clone)]
pub struct AnalyticsSummary {
    /// Total messages.
    pub total_messages: usize,
    /// User messages.
    pub user_messages: usize,
    /// Assistant messages.
    pub assistant_messages: usize,
    /// Total tokens used.
    pub total_tokens: u64,
    /// Input tokens.
    pub input_tokens: u64,
    /// Output tokens.
    pub output_tokens: u64,
    /// Number of tool invocations.
    pub tool_invocations: usize,
    /// Estimated cost in USD.
    pub estimated_cost: Option<f64>,
    /// Session duration in seconds.
    pub duration_seconds: Option<i64>,
    /// Primary model used.
    pub primary_model: Option<String>,
}

impl SnatchClient {
    /// Create a new client with auto-discovery of Claude Code data.
    ///
    /// This will search for the Claude Code data directory in the default location.
    ///
    /// # Errors
    ///
    /// Returns an error if the Claude Code data directory cannot be found.
    pub fn discover() -> Result<Self> {
        let claude_dir = ClaudeDirectory::discover()?;
        Ok(Self { claude_dir })
    }

    /// Create a client with a specific Claude Code directory path.
    pub fn with_path(path: impl AsRef<Path>) -> Result<Self> {
        let claude_dir = ClaudeDirectory::from_path(path.as_ref())?;
        Ok(Self { claude_dir })
    }

    /// Get the path to the Claude Code data directory.
    #[must_use]
    pub fn data_path(&self) -> &Path {
        self.claude_dir.root()
    }

    /// List all projects.
    pub fn projects(&self) -> Result<Vec<ProjectInfo>> {
        let projects = self.claude_dir.projects()?;
        Ok(projects
            .iter()
            .map(|p| ProjectInfo {
                path: p.decoded_path().to_string(),
                session_count: p.sessions().map(|s| s.len()).unwrap_or(0),
            })
            .collect())
    }

    /// List all sessions.
    pub fn all_sessions(&self) -> Result<Vec<SessionInfo>> {
        let sessions = self.claude_dir.all_sessions()?;
        Ok(sessions.iter().map(|s| self.session_to_info(s)).collect())
    }

    /// List recent sessions (sorted by modification time, newest first).
    pub fn recent_sessions(&self, limit: usize) -> Result<Vec<SessionInfo>> {
        let mut sessions = self.claude_dir.all_sessions()?;
        sessions.sort_by_key(|s| std::cmp::Reverse(s.modified_time()));
        sessions.truncate(limit);
        Ok(sessions.iter().map(|s| self.session_to_info(s)).collect())
    }

    /// List sessions matching a filter.
    pub fn filtered_sessions(&self, filter: &SessionFilter) -> Result<Vec<SessionInfo>> {
        let sessions = self.claude_dir.all_sessions()?;
        let filtered: Vec<_> = sessions
            .iter()
            .filter(|s| filter.matches(s).unwrap_or(false))
            .collect();
        Ok(filtered.iter().map(|s| self.session_to_info(s)).collect())
    }

    /// Get a session by ID.
    pub fn get_session(&self, session_id: &str) -> Result<Option<SessionInfo>> {
        if let Some(session) = self.claude_dir.find_session(session_id)? {
            Ok(Some(self.session_to_info(&session)))
        } else {
            Ok(None)
        }
    }

    /// Parse a session and return the log entries.
    pub fn parse_session(&self, session_id: &str) -> Result<Vec<LogEntry>> {
        let session = self
            .claude_dir
            .find_session(session_id)?
            .ok_or_else(|| SnatchError::SessionNotFound {
                session_id: session_id.to_string(),
            })?;
        session.parse()
    }

    /// Build a conversation tree from a session.
    pub fn build_conversation(&self, session_id: &str) -> Result<Conversation> {
        let entries = self.parse_session(session_id)?;
        Conversation::from_entries(entries)
    }

    /// Get analytics for a session.
    pub fn session_analytics(&self, session_id: &str) -> Result<AnalyticsSummary> {
        let conversation = self.build_conversation(session_id)?;
        let analytics = SessionAnalytics::from_conversation(&conversation);
        let summary = analytics.summary_report();

        Ok(AnalyticsSummary {
            total_messages: summary.total_messages,
            user_messages: summary.user_messages,
            assistant_messages: summary.assistant_messages,
            total_tokens: summary.total_tokens,
            input_tokens: summary.input_tokens,
            output_tokens: summary.output_tokens,
            tool_invocations: summary.tool_invocations,
            estimated_cost: summary.estimated_cost,
            duration_seconds: analytics.duration().map(|d| d.num_seconds()),
            primary_model: summary.primary_model,
        })
    }

    /// Compare two sessions and return the differences.
    pub fn compare_sessions(&self, session_a_id: &str, session_b_id: &str) -> Result<String> {
        let conv_a = self.build_conversation(session_a_id)?;
        let conv_b = self.build_conversation(session_b_id)?;
        let diff = SessionDiff::from_conversations(&conv_a, &conv_b);
        Ok(diff.report())
    }

    /// Export a session to a string in the specified format.
    pub fn export_session(&self, session_id: &str, format: ExportFormat) -> Result<String> {
        self.export_session_with_options(session_id, format, &ExportOptions::default())
    }

    /// Export a session with custom options.
    pub fn export_session_with_options(
        &self,
        session_id: &str,
        format: ExportFormat,
        options: &ExportOptions,
    ) -> Result<String> {
        let conversation = self.build_conversation(session_id)?;
        let mut buffer = Cursor::new(Vec::new());

        match format {
            ExportFormat::Markdown => {
                let exporter = MarkdownExporter::new();
                exporter.export_conversation(&conversation, &mut buffer, options)?;
            }
            ExportFormat::Json => {
                let exporter = JsonExporter::new();
                exporter.export_conversation(&conversation, &mut buffer, options)?;
            }
            ExportFormat::JsonPretty => {
                let exporter = JsonExporter::new().pretty(true);
                exporter.export_conversation(&conversation, &mut buffer, options)?;
            }
            ExportFormat::Html => {
                let exporter = HtmlExporter::new();
                exporter.export_conversation(&conversation, &mut buffer, options)?;
            }
            ExportFormat::Text => {
                let exporter = TextExporter::new();
                exporter.export_conversation(&conversation, &mut buffer, options)?;
            }
            ExportFormat::Csv => {
                let exporter = CsvExporter::new();
                exporter.export_conversation(&conversation, &mut buffer, options)?;
            }
            ExportFormat::Xml => {
                let exporter = XmlExporter::new();
                exporter.export_conversation(&conversation, &mut buffer, options)?;
            }
        }

        String::from_utf8(buffer.into_inner()).map_err(|e| {
            SnatchError::export(format!("Failed to convert export to string: {}", e))
        })
    }

    /// Export a session directly to a file.
    pub fn export_session_to_file(
        &self,
        session_id: &str,
        format: ExportFormat,
        path: impl AsRef<Path>,
    ) -> Result<()> {
        let content = self.export_session(session_id, format)?;
        std::fs::write(path, content)
            .map_err(|e| SnatchError::io("Failed to write export file", e))
    }

    /// Parse a JSONL file directly.
    pub fn parse_jsonl_file(&self, path: impl AsRef<Path>) -> Result<Vec<LogEntry>> {
        let mut parser = JsonlParser::new();
        parser.parse_file(path.as_ref())
    }

    /// Convert a session to a conversation (internal helper).
    fn session_to_info(&self, session: &Session) -> SessionInfo {
        SessionInfo {
            id: session.session_id().to_string(),
            project_path: session.project_path().to_string(),
            message_count: session.parse().map(|e| e.len()).unwrap_or(0),
            is_subagent: session.is_subagent(),
        }
    }
}

/// Builder for creating export options.
#[derive(Debug, Clone)]
pub struct ExportOptionsBuilder {
    options: ExportOptions,
}

impl Default for ExportOptionsBuilder {
    fn default() -> Self {
        Self::new()
    }
}

impl ExportOptionsBuilder {
    /// Create a new builder with default options.
    #[must_use]
    pub fn new() -> Self {
        Self {
            options: ExportOptions::default(),
        }
    }

    /// Include thinking blocks in output.
    #[must_use]
    pub fn with_thinking(mut self, include: bool) -> Self {
        self.options.include_thinking = include;
        self
    }

    /// Include tool use in output.
    #[must_use]
    pub fn with_tool_use(mut self, include: bool) -> Self {
        self.options.include_tool_use = include;
        self
    }

    /// Include tool results in output.
    #[must_use]
    pub fn with_tool_results(mut self, include: bool) -> Self {
        self.options.include_tool_results = include;
        self
    }

    /// Include system messages in output.
    #[must_use]
    pub fn with_system(mut self, include: bool) -> Self {
        self.options.include_system = include;
        self
    }

    /// Include timestamps in output.
    #[must_use]
    pub fn with_timestamps(mut self, include: bool) -> Self {
        self.options.include_timestamps = include;
        self
    }

    /// Only export main thread (no branches).
    #[must_use]
    pub fn main_thread_only(mut self, only: bool) -> Self {
        self.options.main_thread_only = only;
        self
    }

    /// Truncate content at specified length.
    #[must_use]
    pub fn truncate_at(mut self, length: usize) -> Self {
        self.options.truncate_at = Some(length);
        self
    }

    /// Build the options.
    #[must_use]
    pub fn build(self) -> ExportOptions {
        self.options
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_export_format_variants() {
        let formats = [
            ExportFormat::Markdown,
            ExportFormat::Json,
            ExportFormat::JsonPretty,
            ExportFormat::Html,
            ExportFormat::Text,
            ExportFormat::Csv,
            ExportFormat::Xml,
        ];
        assert_eq!(formats.len(), 7);
    }

    #[test]
    fn test_export_options_builder() {
        let options = ExportOptionsBuilder::new()
            .with_thinking(true)
            .with_tool_use(false)
            .with_timestamps(true)
            .main_thread_only(true)
            .truncate_at(1000)
            .build();

        assert!(options.include_thinking);
        assert!(!options.include_tool_use);
        assert!(options.include_timestamps);
        assert!(options.main_thread_only);
        assert_eq!(options.truncate_at, Some(1000));
    }

    #[test]
    fn test_export_options_builder_default() {
        let builder = ExportOptionsBuilder::default();
        let options = builder.build();
        assert!(options.include_thinking); // Default is true
    }

    #[test]
    fn test_project_info() {
        let info = ProjectInfo {
            path: "/home/user/project".to_string(),
            session_count: 5,
        };
        assert_eq!(info.path, "/home/user/project");
        assert_eq!(info.session_count, 5);
    }

    #[test]
    fn test_session_info() {
        let info = SessionInfo {
            id: "abc123".to_string(),
            project_path: "/project".to_string(),
            message_count: 10,
            is_subagent: false,
        };
        assert_eq!(info.id, "abc123");
        assert!(!info.is_subagent);
    }

    #[test]
    fn test_analytics_summary() {
        let summary = AnalyticsSummary {
            total_messages: 20,
            user_messages: 10,
            assistant_messages: 10,
            total_tokens: 5000,
            input_tokens: 2000,
            output_tokens: 3000,
            tool_invocations: 5,
            estimated_cost: Some(0.05),
            duration_seconds: Some(300),
            primary_model: Some("claude-sonnet-4-20250514".to_string()),
        };

        assert_eq!(summary.total_messages, 20);
        assert_eq!(summary.total_tokens, 5000);
        assert_eq!(summary.estimated_cost, Some(0.05));
    }
}
