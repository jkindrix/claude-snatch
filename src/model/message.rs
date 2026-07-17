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
use serde_json::{from_value, Value};

use super::content::{AssistantContent, ContentBlock, ImageBlock};
use super::metadata::{CompactMetadata, HookInfo, ThinkingMetadata, Todo};
use super::serde_str::{serde_string_enum, without_top_level_type};
use super::usage::Usage;

/// A parsed JSONL line representing any message type.
///
/// Serialization is implemented manually (see `LogEntryRef`) so that the
/// [`LogEntry::Unknown`] variant re-emits its original raw JSON object verbatim
/// rather than a `{"type":"unknown", ...}` wrapper shape.
#[derive(Debug, Clone)]
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
    FileHistorySnapshot(FileHistorySnapshot),

    /// Input buffering control.
    QueueOperation(QueueOperation),

    /// Turn completion markers.
    TurnEnd(TurnEnd),

    /// Progress notifications (hooks, subagent activity, bash output, etc.).
    Progress(ProgressMessage),

    /// Hook-injected attachment context (e.g. SessionStart hook output).
    ///
    /// These carry a `uuid`/`parentUuid` and are real links in the
    /// conversation tree; dropping them fragments the main thread.
    Attachment(AttachmentMessage),

    /// The most recent user prompt, recorded as session sidecar metadata.
    LastPrompt(LastPromptMessage),

    /// Editor mode sidecar metadata.
    Mode(ModeMessage),

    /// Permission mode sidecar metadata.
    PermissionMode(PermissionModeMessage),

    /// AI-generated session title metadata.
    AiTitle(AiTitleMessage),

    /// Any entry type not modeled above. The full raw JSON object is retained so
    /// the payload — and any `uuid`/`parentUuid`/`timestamp`/etc. — survives
    /// instead of being dropped, and re-serializes byte-for-content identically.
    Unknown(Value),
}

/// Borrowed mirror of [`LogEntry`]'s known variants, used solely to drive
/// serialization with the original internally-tagged `type` shape. The
/// [`LogEntry::Unknown`] variant is handled separately (it serializes its raw
/// value directly), so this enum intentionally has no `Unknown` arm and never
/// recurses back into `LogEntry::serialize`.
#[derive(Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum LogEntryRef<'a> {
    Assistant(&'a AssistantMessage),
    User(&'a UserMessage),
    System(&'a SystemMessage),
    Summary(&'a SummaryMessage),
    #[serde(rename = "file-history-snapshot")]
    FileHistorySnapshot(&'a FileHistorySnapshot),
    #[serde(rename = "queue-operation")]
    QueueOperation(&'a QueueOperation),
    TurnEnd(&'a TurnEnd),
    Progress(&'a ProgressMessage),
    Attachment(&'a AttachmentMessage),
    #[serde(rename = "last-prompt")]
    LastPrompt(&'a LastPromptMessage),
    Mode(&'a ModeMessage),
    #[serde(rename = "permission-mode")]
    PermissionMode(&'a PermissionModeMessage),
    #[serde(rename = "ai-title")]
    AiTitle(&'a AiTitleMessage),
}

impl Serialize for LogEntry {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        match self {
            Self::Assistant(m) => LogEntryRef::Assistant(m).serialize(serializer),
            Self::User(m) => LogEntryRef::User(m).serialize(serializer),
            Self::System(m) => LogEntryRef::System(m).serialize(serializer),
            Self::Summary(m) => LogEntryRef::Summary(m).serialize(serializer),
            Self::FileHistorySnapshot(m) => {
                LogEntryRef::FileHistorySnapshot(m).serialize(serializer)
            }
            Self::QueueOperation(m) => LogEntryRef::QueueOperation(m).serialize(serializer),
            Self::TurnEnd(m) => LogEntryRef::TurnEnd(m).serialize(serializer),
            Self::Progress(m) => LogEntryRef::Progress(m).serialize(serializer),
            Self::Attachment(m) => LogEntryRef::Attachment(m).serialize(serializer),
            Self::LastPrompt(m) => LogEntryRef::LastPrompt(m).serialize(serializer),
            Self::Mode(m) => LogEntryRef::Mode(m).serialize(serializer),
            Self::PermissionMode(m) => LogEntryRef::PermissionMode(m).serialize(serializer),
            Self::AiTitle(m) => LogEntryRef::AiTitle(m).serialize(serializer),
            Self::Unknown(raw) => raw.serialize(serializer),
        }
    }
}

impl<'de> Deserialize<'de> for LogEntry {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        use serde::de::Error as _;
        let value = Value::deserialize(deserializer)?;
        let tag = value.get("type").and_then(Value::as_str).map(str::to_owned);
        // Strip the discriminator for known variants so it is not re-captured by
        // their flattened `extra` map and then duplicated on serialize. The
        // Unknown arm keeps the full object (including `type`) verbatim.
        let known = |v: Value| without_top_level_type(v);
        Ok(match tag.as_deref() {
            Some("assistant") => {
                Self::Assistant(from_value(known(value)).map_err(D::Error::custom)?)
            }
            Some("user") => Self::User(from_value(known(value)).map_err(D::Error::custom)?),
            Some("system") => Self::System(from_value(known(value)).map_err(D::Error::custom)?),
            Some("summary") => Self::Summary(from_value(known(value)).map_err(D::Error::custom)?),
            Some("file-history-snapshot") => {
                Self::FileHistorySnapshot(from_value(known(value)).map_err(D::Error::custom)?)
            }
            Some("queue-operation") => {
                Self::QueueOperation(from_value(known(value)).map_err(D::Error::custom)?)
            }
            Some("turn_end") => Self::TurnEnd(from_value(known(value)).map_err(D::Error::custom)?),
            Some("progress") => Self::Progress(from_value(known(value)).map_err(D::Error::custom)?),
            Some("attachment") => {
                Self::Attachment(from_value(known(value)).map_err(D::Error::custom)?)
            }
            Some("last-prompt") => {
                Self::LastPrompt(from_value(known(value)).map_err(D::Error::custom)?)
            }
            Some("mode") => Self::Mode(from_value(known(value)).map_err(D::Error::custom)?),
            Some("permission-mode") => {
                Self::PermissionMode(from_value(known(value)).map_err(D::Error::custom)?)
            }
            Some("ai-title") => Self::AiTitle(from_value(known(value)).map_err(D::Error::custom)?),
            // Unknown or absent `type`: retain the whole object verbatim, but
            // only if it is a JSON object — a bare scalar/array is not a valid
            // log entry and must still be rejected (strict) / skipped (lenient).
            _ if value.is_object() => Self::Unknown(value),
            _ => return Err(D::Error::custom("log entry must be a JSON object")),
        })
    }
}

/// Read a string field from an unknown entry's raw JSON object.
fn unknown_field<'a>(raw: &'a Value, key: &str) -> Option<&'a str> {
    raw.get(key).and_then(Value::as_str)
}

impl LogEntry {
    /// Get the UUID of this entry, if present.
    #[must_use]
    pub fn uuid(&self) -> Option<&str> {
        match self {
            Self::Assistant(m) => Some(&m.uuid),
            Self::User(m) => Some(&m.uuid),
            Self::System(m) => Some(&m.uuid),
            Self::Progress(m) => Some(&m.uuid),
            Self::Attachment(m) => Some(&m.uuid),
            Self::Summary(_) => None, // Summary messages lack UUID
            Self::FileHistorySnapshot(_) => None,
            Self::QueueOperation(_) => None,
            Self::TurnEnd(_) => None,
            // Sidecar metadata entries carry no UUID.
            Self::LastPrompt(_) | Self::Mode(_) | Self::PermissionMode(_) | Self::AiTitle(_) => {
                None
            }
            Self::Unknown(raw) => unknown_field(raw, "uuid"),
        }
    }

    /// Get the parent UUID of this entry, if present.
    #[must_use]
    pub fn parent_uuid(&self) -> Option<&str> {
        match self {
            Self::Assistant(m) => m.parent_uuid.as_deref(),
            Self::User(m) => m.parent_uuid.as_deref(),
            Self::System(m) => m.parent_uuid.as_deref(),
            Self::Progress(m) => m.parent_uuid.as_deref(),
            Self::Attachment(m) => m.parent_uuid.as_deref(),
            Self::Unknown(raw) => unknown_field(raw, "parentUuid"),
            _ => None,
        }
    }

    /// Get the logical parent UUID (preserved across compaction).
    #[must_use]
    pub fn logical_parent_uuid(&self) -> Option<&str> {
        match self {
            Self::System(m) => m.logical_parent_uuid.as_deref(),
            Self::Unknown(raw) => unknown_field(raw, "logicalParentUuid"),
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
            Self::Progress(m) => Some(&m.session_id),
            Self::Attachment(m) => Some(&m.session_id),
            Self::LastPrompt(m) => Some(&m.session_id),
            Self::Mode(m) => Some(&m.session_id),
            Self::PermissionMode(m) => Some(&m.session_id),
            Self::AiTitle(m) => Some(&m.session_id),
            Self::FileHistorySnapshot(_) => None,
            Self::QueueOperation(m) => Some(&m.session_id),
            Self::TurnEnd(_) => None,
            Self::Summary(_) => None,
            Self::Unknown(raw) => unknown_field(raw, "sessionId"),
        }
    }

    /// Get the timestamp of this entry, if present.
    #[must_use]
    pub fn timestamp(&self) -> Option<DateTime<Utc>> {
        match self {
            Self::Assistant(m) => Some(m.timestamp),
            Self::User(m) => Some(m.timestamp),
            Self::System(m) => Some(m.timestamp),
            Self::Progress(m) => Some(m.timestamp),
            Self::Attachment(m) => Some(m.timestamp),
            Self::FileHistorySnapshot(_) => None,
            Self::QueueOperation(m) => Some(m.timestamp),
            Self::TurnEnd(m) => Some(m.timestamp),
            Self::Summary(_) => None,
            // Sidecar metadata entries carry no timestamp.
            Self::LastPrompt(_) | Self::Mode(_) | Self::PermissionMode(_) | Self::AiTitle(_) => {
                None
            }
            Self::Unknown(raw) => unknown_field(raw, "timestamp")
                .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
                .map(|dt| dt.with_timezone(&Utc)),
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
            Self::Progress(m) => m.is_sidechain,
            Self::Attachment(m) => m.is_sidechain,
            Self::Unknown(raw) => raw
                .get("isSidechain")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            _ => false,
        }
    }

    /// Get the slug (human-readable session name) from this entry, if present.
    #[must_use]
    pub fn slug(&self) -> Option<&str> {
        match self {
            Self::Assistant(m) => m.slug.as_deref(),
            Self::User(m) => m.slug.as_deref(),
            Self::Progress(m) => m.slug.as_deref(),
            _ => None,
        }
    }

    /// Get the token usage for this entry, if available.
    /// Usage data is only present on assistant messages.
    #[must_use]
    pub fn usage(&self) -> Option<&Usage> {
        match self {
            Self::Assistant(m) => m.message.usage.as_ref(),
            _ => None,
        }
    }

    /// Get the message type as a string.
    ///
    /// For an unknown entry this returns its actual raw `type` (falling back to
    /// `"unknown"` only when the discriminator is absent), so preserved types
    /// like `pr-link` are visible in CSV/analytics surfaces rather than
    /// collapsed to a generic placeholder.
    #[must_use]
    pub fn message_type(&self) -> &str {
        match self {
            Self::Assistant(_) => "assistant",
            Self::User(_) => "user",
            Self::System(_) => "system",
            Self::Summary(_) => "summary",
            Self::Progress(_) => "progress",
            Self::FileHistorySnapshot(_) => "file-history-snapshot",
            Self::QueueOperation(_) => "queue-operation",
            Self::TurnEnd(_) => "turn_end",
            Self::Attachment(_) => "attachment",
            Self::LastPrompt(_) => "last-prompt",
            Self::Mode(_) => "mode",
            Self::PermissionMode(_) => "permission-mode",
            Self::AiTitle(_) => "ai-title",
            Self::Unknown(raw) => unknown_field(raw, "type").unwrap_or("unknown"),
        }
    }

    /// Get the working directory (cwd) from this entry, if present.
    ///
    /// The `cwd` field contains the actual project working directory path
    /// as it was when the session was created. This is the authoritative
    /// source for the project path, more reliable than decoding the
    /// encoded directory name.
    #[must_use]
    pub fn cwd(&self) -> Option<&str> {
        match self {
            Self::Assistant(m) => m.cwd.as_deref(),
            Self::User(m) => m.cwd.as_deref(),
            Self::System(m) => m.cwd.as_deref(),
            Self::Attachment(m) => m.cwd.as_deref(),
            Self::Unknown(raw) => unknown_field(raw, "cwd"),
            _ => None,
        }
    }

    /// Get the git branch from this entry, if present.
    #[must_use]
    pub fn git_branch(&self) -> Option<&str> {
        match self {
            Self::Assistant(m) => m.git_branch.as_deref(),
            Self::User(m) => m.git_branch.as_deref(),
            Self::System(m) => m.git_branch.as_deref(),
            Self::Attachment(m) => m.git_branch.as_deref(),
            Self::Unknown(raw) => unknown_field(raw, "gitBranch"),
            _ => None,
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

    /// Compaction continuation summary marker. Harness-injected "This session
    /// is being continued from a previous conversation..." entries carry this,
    /// so they can be distinguished from genuine human prompts.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub is_compact_summary: Option<bool>,

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
    /// Unknown fields for forward compatibility.
    #[serde(flatten)]
    pub extra: IndexMap<String, Value>,
}

/// User content with array of blocks.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserBlocksContent {
    /// User role.
    pub role: String,
    /// Array of content blocks.
    pub content: Vec<ContentBlock>,
    /// Unknown fields for forward compatibility.
    #[serde(flatten)]
    pub extra: IndexMap<String, Value>,
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

    /// Check if this content has any visible text (not just tool results).
    ///
    /// Returns true if:
    /// - This is a Simple content with non-empty text, OR
    /// - This is Blocks content with at least one Text block that has non-empty content
    ///
    /// Returns false if:
    /// - This is a Simple content with empty text
    /// - This is Blocks content with only tool results or images (no text)
    #[must_use]
    pub fn has_visible_text(&self) -> bool {
        match self {
            Self::Simple(s) => !s.content.trim().is_empty(),
            Self::Blocks(b) => b
                .content
                .iter()
                .any(|c| matches!(c, ContentBlock::Text(t) if !t.text.trim().is_empty())),
        }
    }

    /// Check if this contains tool results.
    #[must_use]
    pub fn has_tool_results(&self) -> bool {
        match self {
            Self::Simple(_) => false,
            Self::Blocks(b) => b
                .content
                .iter()
                .any(|c| matches!(c, ContentBlock::ToolResult(_))),
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

    // Checkpoint/Rewind fields
    /// Checkpoint identifier for rewind operations.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub checkpoint_id: Option<String>,

    /// Target UUID for rewind operations.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target_uuid: Option<String>,

    /// Rewind mode: "conversation", "code", or "both".
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rewind_mode: Option<String>,

    /// Files affected by checkpoint/rewind.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub affected_files: Vec<String>,

    // Session rename fields
    /// New session name after rename.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub new_name: Option<String>,

    /// Old session name before rename.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub old_name: Option<String>,

    /// Unknown fields for forward compatibility.
    #[serde(flatten)]
    pub extra: IndexMap<String, Value>,
}

/// System message subtypes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SystemSubtype {
    /// Compaction boundary marker.
    CompactBoundary,
    /// Microcompaction boundary marker (partial compaction; still a
    /// compaction event for counting/filtering purposes).
    MicrocompactBoundary,
    /// Hook execution summary.
    StopHookSummary,
    /// API error with retry info.
    ApiError,
    /// CLI slash command execution.
    LocalCommand,
    /// Checkpoint creation event.
    Checkpoint,
    /// Rewind/restore event.
    Rewind,
    /// Session rename event.
    Rename,
    /// Init event at session start.
    Init,
    /// Session resume event.
    Resume,
    /// Permission request/grant event.
    Permission,
    /// Tool execution event.
    Tool,
    /// Any subtype not modeled above, captured verbatim (the original string is
    /// retained rather than collapsed to a nameless placeholder).
    Other(String),
}

serde_string_enum!(SystemSubtype {
    CompactBoundary => "compact_boundary",
    MicrocompactBoundary => "microcompact_boundary",
    StopHookSummary => "stop_hook_summary",
    ApiError => "api_error",
    LocalCommand => "local_command",
    Checkpoint => "checkpoint",
    Rewind => "rewind",
    Rename => "rename",
    Init => "init",
    Resume => "resume",
    Permission => "permission",
    Tool => "tool",
} other Other);

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
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum QueueOperationType {
    /// Add input to queue while processing.
    Enqueue,
    /// Remove and process queued input.
    Dequeue,
    /// Cancel/remove queued input.
    Remove,
    /// Clear and process all queued inputs.
    PopAll,
    /// Any operation not modeled above, captured verbatim.
    Other(String),
}

serde_string_enum!(QueueOperationType {
    Enqueue => "enqueue",
    Dequeue => "dequeue",
    Remove => "remove",
    PopAll => "popAll",
} other Other);

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

/// Progress notification from hooks, subagents, bash output, etc.
///
/// These entries contain provenance information linking subagent activity
/// to the specific tool call that spawned them.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProgressMessage {
    /// Unique identifier for this progress entry.
    pub uuid: String,

    /// Parent UUID in the conversation tree.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_uuid: Option<String>,

    /// ISO 8601 UTC timestamp.
    pub timestamp: DateTime<Utc>,

    /// The logical session this belongs to.
    #[serde(default)]
    pub session_id: String,

    /// The tool use ID that this progress relates to.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_use_id: Option<String>,

    /// The parent tool use ID that spawned this activity.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[serde(rename = "parentToolUseID")]
    pub parent_tool_use_id: Option<String>,

    /// Agent ID for subagent progress.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<String>,

    /// Whether this is a sidechain (subagent) message.
    #[serde(default)]
    pub is_sidechain: bool,

    /// Human-readable session slug.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub slug: Option<String>,

    /// Progress data containing the subtype and payload.
    pub data: ProgressData,

    /// Unknown fields for forward compatibility.
    #[serde(flatten)]
    pub extra: IndexMap<String, Value>,
}

/// Progress data payload.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProgressData {
    /// Progress subtype: hook_progress, agent_progress, bash_progress, etc.
    #[serde(rename = "type")]
    pub progress_type: String,

    /// Agent ID (present in agent_progress).
    #[serde(skip_serializing_if = "Option::is_none")]
    #[serde(rename = "agentId")]
    pub agent_id: Option<String>,

    /// The prompt sent to a subagent (present in agent_progress).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prompt: Option<String>,

    /// Unknown fields for forward compatibility.
    #[serde(flatten)]
    pub extra: IndexMap<String, Value>,
}

impl ProgressMessage {
    /// Whether this is an agent progress entry (subagent spawned).
    #[must_use]
    pub fn is_agent_progress(&self) -> bool {
        self.data.progress_type == "agent_progress"
    }

    /// Get the effective agent ID from the data payload or the entry-level field.
    #[must_use]
    pub fn effective_agent_id(&self) -> Option<&str> {
        self.data.agent_id.as_deref().or(self.agent_id.as_deref())
    }
}

/// Hook-injected attachment context.
///
/// Carries `uuid`/`parentUuid` and is a structural link in the conversation
/// tree, so it must be preserved during reconstruction to avoid fragmenting
/// the main thread.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AttachmentMessage {
    /// Unique identifier for this entry.
    pub uuid: String,

    /// Parent event reference in the conversation tree.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_uuid: Option<String>,

    /// ISO 8601 UTC timestamp.
    pub timestamp: DateTime<Utc>,

    /// Conversation session identifier.
    #[serde(default)]
    pub session_id: String,

    /// Whether this is a sidechain (subagent) message.
    #[serde(default)]
    pub is_sidechain: bool,

    /// Working directory when the entry was recorded.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,

    /// Git branch when the entry was recorded.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub git_branch: Option<String>,

    /// Claude Code version.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,

    /// The attachment payload (hook output, injected context, etc.).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub attachment: Option<Value>,

    /// Unknown fields for forward compatibility.
    #[serde(flatten)]
    pub extra: IndexMap<String, Value>,
}

/// Sidecar metadata recording the most recent user prompt.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LastPromptMessage {
    /// The most recent user prompt text. Absent in newer pointer-only
    /// last-prompt entries that carry only `leafUuid`/`sessionId`.
    #[serde(default)]
    pub last_prompt: Option<String>,

    /// Conversation session identifier.
    #[serde(default)]
    pub session_id: String,

    /// Unknown fields for forward compatibility.
    #[serde(flatten)]
    pub extra: IndexMap<String, Value>,
}

/// Sidecar metadata recording the editor mode.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModeMessage {
    /// Editor mode (e.g. "normal").
    pub mode: String,

    /// Conversation session identifier.
    #[serde(default)]
    pub session_id: String,

    /// Unknown fields for forward compatibility.
    #[serde(flatten)]
    pub extra: IndexMap<String, Value>,
}

/// Sidecar metadata recording the permission mode.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PermissionModeMessage {
    /// Permission mode (e.g. "bypassPermissions").
    pub permission_mode: String,

    /// Conversation session identifier.
    #[serde(default)]
    pub session_id: String,

    /// Unknown fields for forward compatibility.
    #[serde(flatten)]
    pub extra: IndexMap<String, Value>,
}

/// Sidecar metadata recording the AI-generated session title.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AiTitleMessage {
    /// AI-generated session title.
    pub ai_title: String,

    /// Conversation session identifier.
    #[serde(default)]
    pub session_id: String,

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

    #[test]
    fn test_system_turn_duration_parses() {
        let json = r#"{"parentUuid":"2662d654-12a7","isSidechain":false,"userType":"external","cwd":"/tmp","sessionId":"test","version":"2.1.63","gitBranch":"main","slug":"test","type":"system","subtype":"turn_duration","durationMs":31226,"timestamp":"2026-02-28T12:29:40.051Z","uuid":"3c6c72db-test","isMeta":false}"#;
        let result: Result<LogEntry, _> = serde_json::from_str(json);
        match &result {
            Ok(entry) => {
                assert_eq!(entry.uuid(), Some("3c6c72db-test"));
                assert_eq!(entry.message_type(), "system");
            }
            Err(e) => panic!("Failed to parse turn_duration system entry: {e}"),
        }
    }

    #[test]
    fn test_progress_entry_parsing() {
        let json = r#"{"parentUuid":"59648a0e-2e62","isSidechain":true,"userType":"external","cwd":"/tmp","sessionId":"29907bd0-test","version":"2.1.63","gitBranch":"main","agentId":"aef951e7b587a5f44","slug":"kind-honking-gray","type":"progress","data":{"type":"hook_progress","hookEvent":"PostToolUse","hookName":"PostToolUse:Read","command":"callback"},"parentToolUseID":"toolu_01Gv1AwMjEQfjPJbCEN8AMJm","toolUseID":"toolu_01Gv1AwMjEQfjPJbCEN8AMJm","timestamp":"2026-02-28T14:59:00.866Z","uuid":"a9aa02c6-cf82"}"#;
        let entry: LogEntry = serde_json::from_str(json).unwrap();
        assert_eq!(entry.message_type(), "progress");
        assert_eq!(entry.uuid(), Some("a9aa02c6-cf82"));
        assert_eq!(entry.session_id(), Some("29907bd0-test"));
        assert!(entry.is_sidechain());
        assert_eq!(entry.slug(), Some("kind-honking-gray"));

        if let LogEntry::Progress(p) = &entry {
            assert_eq!(p.data.progress_type, "hook_progress");
            assert_eq!(p.agent_id.as_deref(), Some("aef951e7b587a5f44"));
            assert_eq!(
                p.parent_tool_use_id.as_deref(),
                Some("toolu_01Gv1AwMjEQfjPJbCEN8AMJm")
            );
            assert!(!p.is_agent_progress());
        } else {
            panic!("Expected Progress variant");
        }
    }

    #[test]
    fn test_agent_progress_entry() {
        let json = r#"{"parentUuid":"e08af7a4-f18b","isSidechain":false,"userType":"external","cwd":"/tmp","sessionId":"29907bd0-test","version":"2.1.63","gitBranch":"main","type":"progress","data":{"type":"agent_progress","prompt":"analyze this file","agentId":"a6f6f2bba8a080f95","message":{},"normalizedMessages":[]},"toolUseID":"agent_msg_01QunCZaUkmWtCoYzBpXAuKD","parentToolUseID":"toolu_01RWqMEXFBWfKHHpDArnDTvv","timestamp":"2026-02-28T14:20:19.665Z","uuid":"test-uuid-1234"}"#;
        let entry: LogEntry = serde_json::from_str(json).unwrap();

        if let LogEntry::Progress(p) = &entry {
            assert!(p.is_agent_progress());
            assert_eq!(p.effective_agent_id(), Some("a6f6f2bba8a080f95"));
            assert_eq!(p.data.prompt.as_deref(), Some("analyze this file"));
            assert_eq!(
                p.parent_tool_use_id.as_deref(),
                Some("toolu_01RWqMEXFBWfKHHpDArnDTvv")
            );
        } else {
            panic!("Expected Progress variant");
        }
    }

    #[test]
    fn test_unknown_entry_type_degrades() {
        // An unmodeled entry type parses as Unknown, retaining its full payload.
        let json = r#"{"type":"future-entry-type","uuid":"x1","sessionId":"s","timestamp":"2026-06-21T00:00:00Z","weird":true}"#;
        let entry: LogEntry = serde_json::from_str(json).unwrap();
        // message_type() exposes the real wire type, not a generic placeholder.
        assert_eq!(entry.message_type(), "future-entry-type");
        assert!(matches!(entry, LogEntry::Unknown(_)));
        // Payload survives: re-serialization equals the original object verbatim
        // (no `{"type":"unknown", ...}` wrapper shape).
        let original: Value = serde_json::from_str(json).unwrap();
        assert_eq!(serde_json::to_value(&entry).unwrap(), original);
    }

    #[test]
    fn test_unknown_entry_real_shapes_roundtrip() {
        // Real unmodeled top-level types observed in the archive must round-trip
        // by content with their `type` and payload preserved.
        for json in [
            r#"{"type":"pr-link","sessionId":"s1","prNumber":136,"prUrl":"https://example/pr/136","prRepository":"o/r","timestamp":"2026-06-10T06:02:47.027Z"}"#,
            r#"{"type":"custom-title","customTitle":"streaming-primitives","sessionId":"s2"}"#,
            r#"{"type":"agent-name","sessionId":"s3","name":"explorer"}"#,
        ] {
            let entry: LogEntry = serde_json::from_str(json).unwrap();
            assert!(
                matches!(entry, LogEntry::Unknown(_)),
                "expected Unknown for {json}"
            );
            let original: Value = serde_json::from_str(json).unwrap();
            assert_eq!(
                serde_json::to_value(&entry).unwrap(),
                original,
                "round-trip mismatch for {json}"
            );
        }
    }

    #[test]
    fn test_user_content_preserves_unknown_fields() {
        // Unknown keys inside the inner user `message` object (alongside
        // role/content) must survive via the flattened `extra`.
        let json = r#"{"role":"user","content":"hi","providerMetadata":{"k":1}}"#;
        let content: UserContent = serde_json::from_str(json).unwrap();
        match &content {
            UserContent::Simple(s) => {
                assert!(s.extra.contains_key("providerMetadata"));
            }
            UserContent::Blocks(_) => panic!("expected simple content"),
        }
        let original: Value = serde_json::from_str(json).unwrap();
        assert_eq!(serde_json::to_value(&content).unwrap(), original);
    }

    #[test]
    fn test_unknown_entry_accessors_extract_from_raw() {
        // Tree-linkage fields are recovered from an unknown entry's raw JSON so
        // it can still thread and be attributed.
        let json = r#"{"type":"future-msg","uuid":"u9","parentUuid":"p9","logicalParentUuid":"lp9","sessionId":"sess9","timestamp":"2026-06-21T00:00:05Z","isSidechain":true,"cwd":"/work","gitBranch":"feat"}"#;
        let entry: LogEntry = serde_json::from_str(json).unwrap();
        assert_eq!(entry.uuid(), Some("u9"));
        assert_eq!(entry.parent_uuid(), Some("p9"));
        assert_eq!(entry.logical_parent_uuid(), Some("lp9"));
        assert_eq!(entry.session_id(), Some("sess9"));
        assert_eq!(entry.cwd(), Some("/work"));
        assert_eq!(entry.git_branch(), Some("feat"));
        assert!(entry.is_sidechain());
        assert!(entry.timestamp().is_some());
    }

    #[test]
    fn test_non_object_entry_still_rejected() {
        // A bare JSON scalar is not a valid log entry and must error (so strict
        // mode rejects and lenient mode skips it).
        assert!(serde_json::from_str::<LogEntry>("123").is_err());
        assert!(serde_json::from_str::<LogEntry>("true").is_err());
        assert!(serde_json::from_str::<LogEntry>("null").is_err());
    }

    #[test]
    fn test_unknown_content_block_preserves_message() {
        // An unmodeled content block must not drop the whole message; the
        // sibling text block still parses and the unknown block is retained.
        let json = r#"{"type":"assistant","uuid":"a1","timestamp":"2026-06-21T00:00:01Z","sessionId":"s","version":"2.1.0","isSidechain":false,"message":{"id":"m1","type":"message","role":"assistant","model":"claude","content":[{"type":"redacted_thinking","data":"opaque"},{"type":"text","text":"survives"}]}}"#;
        let entry: LogEntry = serde_json::from_str(json).unwrap();
        if let LogEntry::Assistant(a) = &entry {
            assert_eq!(a.message.combined_text(), "survives");
            // The redacted_thinking block is retained verbatim, not dropped.
            let unknown = a
                .message
                .content
                .iter()
                .find(|b| matches!(b, ContentBlock::Unknown { .. }))
                .expect("unknown block retained");
            if let ContentBlock::Unknown { kind, raw } = unknown {
                assert_eq!(kind, "redacted_thinking");
                assert_eq!(raw.get("data").and_then(Value::as_str), Some("opaque"));
            }
        } else {
            panic!("Expected Assistant variant");
        }
    }

    #[test]
    fn test_unknown_content_block_fallback_roundtrip() {
        // The real `fallback` model-switch block must round-trip verbatim.
        let block_json = r#"{"type":"fallback","from":{"model":"claude-fable-5"},"to":{"model":"claude-opus-4-8"}}"#;
        let block: ContentBlock = serde_json::from_str(block_json).unwrap();
        match &block {
            ContentBlock::Unknown { kind, .. } => assert_eq!(kind, "fallback"),
            other => panic!("expected Unknown fallback, got {other:?}"),
        }
        let original: Value = serde_json::from_str(block_json).unwrap();
        assert_eq!(serde_json::to_value(&block).unwrap(), original);
    }

    #[test]
    fn test_system_subtype_unknown_preserved_verbatim() {
        // An unmodeled system subtype is retained as Other(<string>), not
        // collapsed to a nameless placeholder, and serializes back to the
        // original string.
        let json = r#"{"type":"system","subtype":"turn_duration","uuid":"sy9","timestamp":"2026-06-21T00:00:06Z","sessionId":"s","durationMs":31226}"#;
        let entry: LogEntry = serde_json::from_str(json).unwrap();
        if let LogEntry::System(s) = &entry {
            assert_eq!(
                s.subtype,
                Some(SystemSubtype::Other("turn_duration".to_string()))
            );
            let reser = serde_json::to_value(&entry).unwrap();
            assert_eq!(
                reser.get("subtype").and_then(Value::as_str),
                Some("turn_duration")
            );
        } else {
            panic!("Expected System variant");
        }
    }

    #[test]
    fn test_stop_hook_summary_without_hook_name() {
        // hookInfos entries omitting hookName must still parse.
        let json = r#"{"type":"system","subtype":"stop_hook_summary","uuid":"sy1","timestamp":"2026-06-21T00:00:02Z","sessionId":"s","hookInfos":[{"command":"x","exitCode":0}]}"#;
        let entry: LogEntry = serde_json::from_str(json).unwrap();
        assert_eq!(entry.message_type(), "system");
    }

    #[test]
    fn test_known_entry_no_duplicate_top_level_type_key() {
        // The discriminator must be stripped before deserializing into a struct
        // with a flattened `extra` map, else it is re-captured and emitted twice
        // (a duplicate top-level "type" key). serde_json::Value dedups keys, so
        // this must be asserted at the string level.
        let json = r#"{"type":"user","uuid":"u1","timestamp":"2026-06-21T00:00:00Z","sessionId":"s","version":"2.1.0","isSidechain":false,"message":{"role":"user","content":"hi"}}"#;
        let entry: LogEntry = serde_json::from_str(json).unwrap();
        let out = serde_json::to_string(&entry).unwrap();
        // Exactly one top-level "type": the nested message has no `type` here.
        assert_eq!(
            out.matches(r#""type":"#).count(),
            1,
            "duplicate type key in: {out}"
        );
    }
}
