//! Metadata structures for Claude Code JSONL logs.
//!
//! This module defines metadata structures including:
//! - ThinkingMetadata: Extended thinking configuration
//! - Todo: Workflow task tracking
//! - HookInfo: Hook execution details
//! - CompactMetadata: Context compaction information

use indexmap::IndexMap;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Extended thinking configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ThinkingMetadata {
    /// Thinking budget level: "high", "medium", "low".
    #[serde(skip_serializing_if = "Option::is_none")]
    pub level: Option<ThinkingLevel>,

    /// Whether extended thinking is disabled.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub disabled: Option<bool>,

    /// Array of trigger conditions (typically empty).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub triggers: Vec<String>,

    /// Unknown fields for forward compatibility.
    #[serde(flatten)]
    pub extra: IndexMap<String, Value>,
}

/// Thinking budget level.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ThinkingLevel {
    /// High thinking budget.
    High,
    /// Medium thinking budget.
    Medium,
    /// Low thinking budget.
    Low,
}

impl Default for ThinkingLevel {
    fn default() -> Self {
        Self::Medium
    }
}

/// Workflow task item.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Todo {
    /// Task description in imperative form.
    pub content: String,

    /// Task state: "pending", "in_progress", "completed".
    pub status: TodoStatus,

    /// Task description in present continuous form.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub active_form: Option<String>,

    /// Task priority (if specified).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub priority: Option<String>,

    /// Unknown fields for forward compatibility.
    #[serde(flatten)]
    pub extra: IndexMap<String, Value>,
}

/// Todo status.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TodoStatus {
    /// Task not yet started.
    Pending,
    /// Currently working on task.
    InProgress,
    /// Task finished successfully.
    Completed,
}

impl Todo {
    /// Check if the task is completed.
    #[must_use]
    pub fn is_completed(&self) -> bool {
        self.status == TodoStatus::Completed
    }

    /// Check if the task is in progress.
    #[must_use]
    pub fn is_in_progress(&self) -> bool {
        self.status == TodoStatus::InProgress
    }

    /// Check if the task is pending.
    #[must_use]
    pub fn is_pending(&self) -> bool {
        self.status == TodoStatus::Pending
    }
}

/// Hook execution details.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HookInfo {
    /// Hook identifier from settings.
    pub hook_name: String,

    /// Shell command that was executed.
    pub command: String,

    /// Command exit code (0 = success).
    pub exit_code: i32,

    /// Combined stdout/stderr output.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output: Option<String>,

    /// Execution time in milliseconds.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,

    /// Unknown fields for forward compatibility.
    #[serde(flatten)]
    pub extra: IndexMap<String, Value>,
}

impl HookInfo {
    /// Check if the hook execution succeeded (exit code 0).
    #[must_use]
    pub fn succeeded(&self) -> bool {
        self.exit_code == 0
    }

    /// Check if the hook execution failed (non-zero exit code).
    #[must_use]
    pub fn failed(&self) -> bool {
        self.exit_code != 0
    }
}

/// Context compaction metadata.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CompactMetadata {
    /// Compaction trigger: "manual" or "auto".
    pub trigger: CompactTrigger,

    /// Token count before compaction.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pre_tokens: Option<u64>,

    /// Unknown fields for forward compatibility.
    #[serde(flatten)]
    pub extra: IndexMap<String, Value>,
}

/// Compaction trigger type.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CompactTrigger {
    /// User-initiated compaction.
    Manual,
    /// Automatic compaction due to context limits.
    Auto,
}

/// API error details from system messages.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApiErrorDetails {
    /// HTTP status code (e.g., 529 for overloaded).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<u16>,

    /// Response headers.
    #[serde(default, skip_serializing_if = "IndexMap::is_empty")]
    pub headers: IndexMap<String, String>,

    /// API request identifier.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,

    /// Nested error object.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<ApiErrorInner>,

    /// Unknown fields for forward compatibility.
    #[serde(flatten)]
    pub extra: IndexMap<String, Value>,
}

/// Inner API error structure.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApiErrorInner {
    /// Error category (e.g., "error").
    #[serde(rename = "type")]
    pub error_type: String,

    /// Nested error details.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<ApiErrorType>,

    /// Request ID from response.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,

    /// Unknown fields for forward compatibility.
    #[serde(flatten)]
    pub extra: IndexMap<String, Value>,
}

/// Specific API error type.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApiErrorType {
    /// Specific error type (e.g., "overloaded_error").
    #[serde(rename = "type")]
    pub error_type: String,

    /// Human-readable error message.
    pub message: String,

    /// Unknown fields for forward compatibility.
    #[serde(flatten)]
    pub extra: IndexMap<String, Value>,
}

/// Local command content parsed from XML.
#[derive(Debug, Clone)]
pub struct LocalCommandContent {
    /// The slash command invoked.
    pub command_name: String,
    /// Command display text.
    pub command_message: String,
    /// Arguments passed to the command.
    pub command_args: Option<String>,
    /// Command output (in response messages).
    pub stdout: Option<String>,
}

impl LocalCommandContent {
    /// Parse local command content from XML string.
    #[must_use]
    pub fn parse(content: &str) -> Option<Self> {
        // Simple XML parsing for local command content
        let command_name = extract_xml_tag(content, "command-name")?;
        let command_message = extract_xml_tag(content, "command-message").unwrap_or_default();
        let command_args = extract_xml_tag(content, "command-args");
        let stdout = extract_xml_tag(content, "local-command-stdout");

        Some(Self {
            command_name,
            command_message,
            command_args,
            stdout,
        })
    }
}

/// Simple XML tag extraction helper.
fn extract_xml_tag(content: &str, tag: &str) -> Option<String> {
    let start_tag = format!("<{tag}>");
    let end_tag = format!("</{tag}>");

    let start_idx = content.find(&start_tag)?;
    let end_idx = content.find(&end_tag)?;

    if start_idx >= end_idx {
        return None;
    }

    let value_start = start_idx + start_tag.len();
    Some(content[value_start..end_idx].trim().to_string())
}

/// Session metadata extracted from log entries.
#[derive(Debug, Clone, Default)]
pub struct SessionMetadata {
    /// Session UUID.
    pub session_id: String,

    /// Human-readable session identifier.
    pub slug: Option<String>,

    /// Working directory.
    pub cwd: Option<String>,

    /// Git branch.
    pub git_branch: Option<String>,

    /// First timestamp in session.
    pub start_time: Option<chrono::DateTime<chrono::Utc>>,

    /// Last timestamp in session.
    pub end_time: Option<chrono::DateTime<chrono::Utc>>,

    /// Claude Code version used.
    pub version: Option<String>,

    /// Total message count.
    pub message_count: usize,

    /// User message count.
    pub user_message_count: usize,

    /// Assistant message count.
    pub assistant_message_count: usize,

    /// Whether this is a sidechain session.
    pub is_sidechain: bool,

    /// Agent ID if sidechain.
    pub agent_id: Option<String>,

    /// Root UUID of conversation tree.
    pub root_uuid: Option<String>,
}

impl SessionMetadata {
    /// Calculate session duration.
    #[must_use]
    pub fn duration(&self) -> Option<chrono::Duration> {
        match (&self.start_time, &self.end_time) {
            (Some(start), Some(end)) => Some(*end - *start),
            _ => None,
        }
    }

    /// Get duration as human-readable string.
    #[must_use]
    pub fn duration_string(&self) -> Option<String> {
        self.duration().map(|d| {
            let total_secs = d.num_seconds();
            if total_secs < 60 {
                format!("{total_secs}s")
            } else if total_secs < 3600 {
                format!("{}m {}s", total_secs / 60, total_secs % 60)
            } else {
                format!(
                    "{}h {}m {}s",
                    total_secs / 3600,
                    (total_secs % 3600) / 60,
                    total_secs % 60
                )
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_thinking_metadata_parsing() {
        let json = r#"{"level":"high","disabled":false,"triggers":[]}"#;
        let metadata: ThinkingMetadata = serde_json::from_str(json).unwrap();
        assert_eq!(metadata.level, Some(ThinkingLevel::High));
        assert_eq!(metadata.disabled, Some(false));
        assert!(metadata.triggers.is_empty());
    }

    #[test]
    fn test_todo_status() {
        let todo = Todo {
            content: "Test task".to_string(),
            status: TodoStatus::InProgress,
            active_form: Some("Testing task".to_string()),
            priority: None,
            extra: IndexMap::new(),
        };

        assert!(todo.is_in_progress());
        assert!(!todo.is_completed());
        assert!(!todo.is_pending());
    }

    #[test]
    fn test_local_command_parsing() {
        let content = r"<command-name>/mcp</command-name>
            <command-message>mcp</command-message>
            <command-args></command-args>";

        let parsed = LocalCommandContent::parse(content).unwrap();
        assert_eq!(parsed.command_name, "/mcp");
        assert_eq!(parsed.command_message, "mcp");
    }

    #[test]
    fn test_hook_info() {
        let hook = HookInfo {
            hook_name: "lint-check".to_string(),
            command: "npm run lint".to_string(),
            exit_code: 0,
            output: Some("All checks passed".to_string()),
            duration_ms: Some(1523),
            extra: IndexMap::new(),
        };

        assert!(hook.succeeded());
        assert!(!hook.failed());
    }

    #[test]
    fn test_session_metadata_duration() {
        use chrono::{TimeZone, Utc};

        let mut metadata = SessionMetadata::default();
        metadata.start_time = Some(Utc.with_ymd_and_hms(2025, 12, 23, 10, 0, 0).unwrap());
        metadata.end_time = Some(Utc.with_ymd_and_hms(2025, 12, 23, 11, 30, 45).unwrap());

        let duration = metadata.duration().unwrap();
        assert_eq!(duration.num_seconds(), 5445); // 1h 30m 45s

        let duration_str = metadata.duration_string().unwrap();
        assert_eq!(duration_str, "1h 30m 45s");
    }
}
