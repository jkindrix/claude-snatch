//! Message types for Claude Code JSONL logs.
//!
//! This module defines all 7 message types:
//! - `assistant`: Claude's responses, tool invocations, thinking
//! - `user`: Human input and tool results
//! - `system`: Notifications, compaction markers, hook summaries
//! - `summary`: Context management summaries
//! - `file-history-snapshot`: File state tracking for undo/redo
//! - `queue-operation`: Input buffering control
//! - `turn_end`: Turn completion markers

use chrono::{DateTime, Utc};
use indexmap::IndexMap;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::content::{AssistantContent, ContentBlock, ImageBlock};
use super::metadata::{CompactMetadata, HookInfo, ThinkingMetadata, Todo};

/// A parsed JSONL line representing any message type.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum LogEntry {
    /// Claude's responses, tool invocations, thinking blocks.
    Assistant(AssistantMessage),

    /// Human input and tool results.
    User(UserMessage),

    /// System notifications, compaction markers, hook summaries.
    System(SystemMessage),

    /// Context management summaries.
    Summary(SummaryMessage),

    /// File state tracking for undo/redo.
    #[serde(rename = "file-history-snapshot")]
    FileHistorySnapshot(FileHistorySnapshot),

    /// Input buffering control.
    #[serde(rename = "queue-operation")]
    QueueOperation(QueueOperation),

    /// Turn completion markers.
    TurnEnd(TurnEnd),
}

impl LogEntry {
    /// Get the UUID of this entry, if present.
    #[must_use]
    pub fn uuid(&self) -> Option<&str> {
        match self {
            Self::Assistant(m) => Some(&m.uuid),
            Self::User(m) => Some(&m.uuid),
            Self::System(m) => Some(&m.uuid),
            Self::Summary(_) => None, // Summary messages lack UUID
            Self::FileHistorySnapshot(_) => None,
            Self::QueueOperation(_) => None,
            Self::TurnEnd(_) => None,
        }
    }

    /// Get the parent UUID of this entry, if present.
    #[must_use]
    pub fn parent_uuid(&self) -> Option<&str> {
        match self {
            Self::Assistant(m) => m.parent_uuid.as_deref(),
            Self::User(m) => m.parent_uuid.as_deref(),
            Self::System(m) => m.parent_uuid.as_deref(),
            _ => None,
        }
    }

    /// Get the logical parent UUID (preserved across compaction).
    #[must_use]
    pub fn logical_parent_uuid(&self) -> Option<&str> {
        match self {
            Self::System(m) => m.logical_parent_uuid.as_deref(),
            _ => None,
        }
    }

    /// Get the session ID of this entry, if present.
    #[must_use]
    pub fn session_id(&self) -> Option<&str> {
        match self {
            Self::Assistant(m) => Some(&m.session_id),
            Self::User(m) => Some(&m.session_id),
            Self::System(m) => m.session_id.as_deref(),
            Self::FileHistorySnapshot(_) => None,
            Self::QueueOperation(m) => Some(&m.session_id),
            Self::TurnEnd(_) => None,
            Self::Summary(_) => None,
        }
    }

    /// Get the timestamp of this entry, if present.
    #[must_use]
    pub fn timestamp(&self) -> Option<DateTime<Utc>> {
        match self {
            Self::Assistant(m) => Some(m.timestamp),
            Self::User(m) => Some(m.timestamp),
            Self::System(m) => Some(m.timestamp),
            Self::FileHistorySnapshot(_) => None,
            Self::QueueOperation(m) => Some(m.timestamp),
            Self::TurnEnd(m) => Some(m.timestamp),
            Self::Summary(_) => None,
        }
    }

    /// Get the Claude Code version, if present.
    #[must_use]
    pub fn version(&self) -> Option<&str> {
        match self {
            Self::Assistant(m) => Some(&m.version),
            Self::User(m) => Some(&m.version),
            Self::System(m) => m.version.as_deref(),
            _ => None,
        }
    }

    /// Check if this is a sidechain (subagent) message.
    #[must_use]
    pub fn is_sidechain(&self) -> bool {
        match self {
            Self::Assistant(m) => m.is_sidechain,
            Self::User(m) => m.is_sidechain,
            Self::System(m) => m.is_sidechain.unwrap_or(false),
            _ => false,
        }
    }

    /// Get the message type as a string.
    #[must_use]
    pub const fn message_type(&self) -> &'static str {
        match self {
            Self::Assistant(_) => "assistant",
            Self::User(_) => "user",
            Self::System(_) => "system",
            Self::Summary(_) => "summary",
            Self::FileHistorySnapshot(_) => "file-history-snapshot",
            Self::QueueOperation(_) => "queue-operation",
            Self::TurnEnd(_) => "turn_end",
        }
    }
}

/// Common fields present in most message types.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommonFields {
    /// Unique identifier for this event.
    pub uuid: String,

    /// Parent event reference (null at conversation start).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_uuid: Option<String>,

    /// ISO 8601 UTC timestamp.
    pub timestamp: DateTime<Utc>,

    /// Conversation session identifier.
    pub session_id: String,

    /// Claude Code version (e.g., "2.0.74").
    pub version: String,

    /// Working directory path.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,

    /// Current git branch name.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub git_branch: Option<String>,

    /// Interaction source (e.g., "external").
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user_type: Option<String>,

    /// Subagent/branched conversation indicator.
    #[serde(default)]
    pub is_sidechain: bool,

    /// Teammate mode flag.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub is_teammate: Option<bool>,

    /// Short agent identifier (e.g., "agent-3e533ee").
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<String>,

    /// Human-readable session identifier (adjective-adjective-noun pattern).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub slug: Option<String>,

    /// API request identifier.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
}

/// Assistant message - Claude's responses, tool invocations, thinking blocks.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AssistantMessage {
    /// Unique identifier for this event.
    pub uuid: String,

    /// Parent event reference.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_uuid: Option<String>,

    /// ISO 8601 UTC timestamp.
    pub timestamp: DateTime<Utc>,

    /// Conversation session identifier.
    pub session_id: String,

    /// Claude Code version.
    pub version: String,

    /// Working directory path.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,

    /// Current git branch name.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub git_branch: Option<String>,

    /// Interaction source.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user_type: Option<String>,

    /// Subagent/branched conversation indicator.
    #[serde(default)]
    pub is_sidechain: bool,

    /// Teammate mode flag.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub is_teammate: Option<bool>,

    /// Agent identifier.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<String>,

    /// Human-readable session identifier.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub slug: Option<String>,

    /// API request identifier.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,

    /// API error flag.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub is_api_error_message: Option<bool>,

    /// The actual message content.
    pub message: AssistantContent,

    /// Unknown fields for forward compatibility.
    #[serde(flatten)]
    pub extra: IndexMap<String, Value>,
}

/// User message - Human input and tool results.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UserMessage {
    /// Unique identifier for this event.
    pub uuid: String,

    /// Parent event reference.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_uuid: Option<String>,

    /// ISO 8601 UTC timestamp.
    pub timestamp: DateTime<Utc>,

    /// Conversation session identifier.
    pub session_id: String,

    /// Claude Code version.
    pub version: String,

    /// Working directory path.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,

    /// Current git branch name.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub git_branch: Option<String>,

    /// Interaction source.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user_type: Option<String>,

    /// Subagent/branched conversation indicator.
    #[serde(default)]
    pub is_sidechain: bool,

    /// Teammate mode flag.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub is_teammate: Option<bool>,

    /// Agent identifier.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<String>,

    /// Human-readable session identifier.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub slug: Option<String>,

    /// System-injected event marker.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub is_meta: Option<bool>,

    /// Visible only in transcript view.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub is_visible_in_transcript_only: Option<bool>,

    /// Extended thinking configuration.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thinking_metadata: Option<ThinkingMetadata>,

    /// Workflow task tracking.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub todos: Vec<Todo>,

    /// Tool-specific execution metadata.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_use_result: Option<Value>,

    /// The message content - either a string or array of content blocks.
    pub message: UserContent,

    /// Unknown fields for forward compatibility.
    #[serde(flatten)]
    pub extra: IndexMap<String, Value>,
}

/// User message content - can be a simple string or array of content blocks.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum UserContent {
    /// Simple string content (direct human input).
    Simple(UserSimpleContent),
    /// Array of content blocks (tool results, images).
    Blocks(UserBlocksContent),
}

/// Simple user content (string message).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserSimpleContent {
    /// User role.
    pub role: String,
    /// The text content.
    pub content: String,
}

/// User content with array of blocks.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserBlocksContent {
    /// User role.
    pub role: String,
    /// Array of content blocks.
    pub content: Vec<ContentBlock>,
}

/// User content block - individual pieces of user content.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum UserContentBlock {
    /// Text content.
    Text(String),
    /// Tool result.
    ToolResult(super::content::ToolResult),
    /// Image.
    Image(ImageBlock),
}

impl UserContent {
    /// Get the text content if this is a simple message.
    #[must_use]
    pub fn as_text(&self) -> Option<&str> {
        match self {
            Self::Simple(s) => Some(&s.content),
            Self::Blocks(_) => None,
        }
    }

    /// Check if this contains tool results.
    #[must_use]
    pub fn has_tool_results(&self) -> bool {
        match self {
            Self::Simple(_) => false,
            Self::Blocks(b) => b.content.iter().any(|c| matches!(c, ContentBlock::ToolResult(_))),
        }
    }

    /// Get all tool results from this content.
    #[must_use]
    pub fn tool_results(&self) -> Vec<&super::content::ToolResult> {
        match self {
            Self::Simple(_) => vec![],
            Self::Blocks(b) => b
                .content
                .iter()
                .filter_map(|c| match c {
                    ContentBlock::ToolResult(tr) => Some(tr),
                    _ => None,
                })
                .collect(),
        }
    }

    /// Get all images from this content.
    #[must_use]
    pub fn images(&self) -> Vec<&ImageBlock> {
        match self {
            Self::Simple(_) => vec![],
            Self::Blocks(b) => b
                .content
                .iter()
                .filter_map(|c| match c {
                    ContentBlock::Image(img) => Some(img),
                    _ => None,
                })
                .collect(),
        }
    }
}

/// System message - Notifications, compaction markers, hook summaries.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SystemMessage {
    /// Unique identifier for this event.
    pub uuid: String,

    /// Parent event reference.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_uuid: Option<String>,

    /// Parent preserved across compaction.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub logical_parent_uuid: Option<String>,

    /// Event subtype classification.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subtype: Option<SystemSubtype>,

    /// Event description.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,

    /// Severity indicator ("info", "error", etc.).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub level: Option<String>,

    /// System-injected event marker.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub is_meta: Option<bool>,

    /// ISO 8601 UTC timestamp.
    pub timestamp: DateTime<Utc>,

    /// Conversation session identifier.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,

    /// Claude Code version.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,

    /// Working directory path.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,

    /// Current git branch name.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub git_branch: Option<String>,

    /// Subagent/branched conversation indicator.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub is_sidechain: Option<bool>,

    /// Interaction source.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user_type: Option<String>,

    /// Compaction metadata.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub compact_metadata: Option<CompactMetadata>,

    // API Error fields
    /// Error object for api_error subtype.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<Value>,

    /// Milliseconds until retry attempt.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub retry_in_ms: Option<f64>,

    /// Current retry number (1-indexed).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub retry_attempt: Option<u32>,

    /// Maximum retry attempts allowed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_retries: Option<u32>,

    /// Error cause chain.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cause: Option<Value>,

    // Stop hook summary fields
    /// Number of Stop hooks that executed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hook_count: Option<u32>,

    /// Details for each executed hook.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub hook_infos: Vec<HookInfo>,

    /// Whether any hook produced output.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub has_output: Option<bool>,

    /// Whether hook blocked Claude from continuing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prevented_continuation: Option<bool>,

    /// Custom stop message if prevented_continuation is true.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stop_reason: Option<String>,

    /// Associated tool_use ID if hook triggered on tool completion.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_use_id: Option<String>,

    /// Unknown fields for forward compatibility.
    #[serde(flatten)]
    pub extra: IndexMap<String, Value>,
}

/// System message subtypes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SystemSubtype {
    /// Compaction boundary marker.
    CompactBoundary,
    /// Hook execution summary.
    StopHookSummary,
    /// API error with retry info.
    ApiError,
    /// CLI slash command execution.
    LocalCommand,
    /// Unknown subtype for forward compatibility.
    #[serde(other)]
    Unknown,
}

/// Summary message - Context management summaries.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SummaryMessage {
    /// Human-readable conversation summary.
    pub summary: String,

    /// UUID of the latest message included in summary.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub leaf_uuid: Option<String>,

    /// Present when generated by compaction.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub is_compact_summary: Option<bool>,

    /// Unknown fields for forward compatibility.
    #[serde(flatten)]
    pub extra: IndexMap<String, Value>,
}

/// File history snapshot - File state tracking for undo/redo.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileHistorySnapshot {
    /// Message ID reference.
    pub message_id: String,

    /// Whether this is an update to existing snapshot.
    #[serde(default)]
    pub is_snapshot_update: bool,

    /// Snapshot data.
    pub snapshot: SnapshotData,

    /// Unknown fields for forward compatibility.
    #[serde(flatten)]
    pub extra: IndexMap<String, Value>,
}

/// Snapshot data containing tracked file backups.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SnapshotData {
    /// Message ID reference.
    pub message_id: String,

    /// Snapshot timestamp.
    pub timestamp: DateTime<Utc>,

    /// Map of file paths to backup metadata.
    pub tracked_file_backups: IndexMap<String, FileBackup>,

    /// Unknown fields for forward compatibility.
    #[serde(flatten)]
    pub extra: IndexMap<String, Value>,
}

/// Backup metadata for a tracked file.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileBackup {
    /// Reference to backup file (null for newly created files).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub backup_file_name: Option<String>,

    /// Incremental version counter.
    pub version: u32,

    /// Backup creation timestamp.
    pub backup_time: DateTime<Utc>,

    /// Unknown fields for forward compatibility.
    #[serde(flatten)]
    pub extra: IndexMap<String, Value>,
}

/// Queue operation - Input buffering control.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QueueOperation {
    /// Operation type.
    pub operation: QueueOperationType,

    /// ISO 8601 UTC timestamp.
    pub timestamp: DateTime<Utc>,

    /// Conversation session identifier.
    pub session_id: String,

    /// Buffered content (present for enqueue/popAll).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,

    /// Unknown fields for forward compatibility.
    #[serde(flatten)]
    pub extra: IndexMap<String, Value>,
}

/// Queue operation types.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum QueueOperationType {
    /// Add input to queue while processing.
    Enqueue,
    /// Remove and process queued input.
    Dequeue,
    /// Cancel/remove queued input.
    Remove,
    /// Clear and process all queued inputs.
    PopAll,
}

/// Turn end marker.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TurnEnd {
    /// ISO 8601 UTC timestamp.
    pub timestamp: DateTime<Utc>,

    /// Agent identifier.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<String>,

    /// Unknown fields for forward compatibility.
    #[serde(flatten)]
    pub extra: IndexMap<String, Value>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_log_entry_message_type() {
        let assistant = LogEntry::Assistant(AssistantMessage {
            uuid: "test".to_string(),
            parent_uuid: None,
            timestamp: Utc::now(),
            session_id: "session".to_string(),
            version: "2.0.74".to_string(),
            cwd: None,
            git_branch: None,
            user_type: None,
            is_sidechain: false,
            is_teammate: None,
            agent_id: None,
            slug: None,
            request_id: None,
            is_api_error_message: None,
            message: AssistantContent::default(),
            extra: IndexMap::new(),
        });

        assert_eq!(assistant.message_type(), "assistant");
    }

    #[test]
    fn test_queue_operation_types() {
        let enqueue_json = r#"{"type":"queue-operation","operation":"enqueue","timestamp":"2025-12-23T00:00:00Z","sessionId":"test","content":"hello"}"#;
        let parsed: LogEntry = serde_json::from_str(enqueue_json).unwrap();

        if let LogEntry::QueueOperation(op) = parsed {
            assert_eq!(op.operation, QueueOperationType::Enqueue);
            assert_eq!(op.content, Some("hello".to_string()));
        } else {
            panic!("Expected QueueOperation");
        }
    }
}
