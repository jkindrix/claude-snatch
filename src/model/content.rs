//! Content block types for Claude Code JSONL logs.
//!
//! This module defines all 5 content block types:
//! - `text`: Natural language responses
//! - `tool_use`: Tool invocation requests
//! - `tool_result`: Tool execution outcomes
//! - `thinking`: Extended reasoning (with signature)
//! - `image`: Visual input (base64/url/file)

use indexmap::IndexMap;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::usage::Usage;

/// Assistant message content structure.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AssistantContent {
    /// API message ID (shared across streaming chunks).
    pub id: String,

    /// Always "message".
    #[serde(rename = "type")]
    pub msg_type: String,

    /// Always "assistant".
    pub role: String,

    /// Model identifier (e.g., "claude-opus-4-5-20251101").
    pub model: String,

    /// Content block array.
    #[serde(default)]
    pub content: Vec<ContentBlock>,

    /// Why generation stopped.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stop_reason: Option<StopReason>,

    /// Stop trigger if applicable.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stop_sequence: Option<String>,

    /// Token statistics.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub usage: Option<Usage>,

    /// Container context for code execution.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub container: Option<Container>,

    /// Context editing info (beta feature).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context_management: Option<ContextManagement>,

    /// Unknown fields for forward compatibility.
    #[serde(flatten)]
    pub extra: IndexMap<String, Value>,
}

impl AssistantContent {
    /// Get all text blocks from this content.
    #[must_use]
    pub fn text_blocks(&self) -> Vec<&TextBlock> {
        self.content
            .iter()
            .filter_map(|c| match c {
                ContentBlock::Text(t) => Some(t),
                _ => None,
            })
            .collect()
    }

    /// Get combined text content from all text blocks.
    #[must_use]
    pub fn combined_text(&self) -> String {
        self.text_blocks()
            .iter()
            .map(|t| t.text.as_str())
            .collect::<Vec<_>>()
            .join("")
    }

    /// Get all tool use blocks from this content.
    #[must_use]
    pub fn tool_uses(&self) -> Vec<&ToolUse> {
        self.content
            .iter()
            .filter_map(|c| match c {
                ContentBlock::ToolUse(t) => Some(t),
                _ => None,
            })
            .collect()
    }

    /// Get all thinking blocks from this content.
    #[must_use]
    pub fn thinking_blocks(&self) -> Vec<&ThinkingBlock> {
        self.content
            .iter()
            .filter_map(|c| match c {
                ContentBlock::Thinking(t) => Some(t),
                _ => None,
            })
            .collect()
    }

    /// Check if this message has any thinking content.
    #[must_use]
    pub fn has_thinking(&self) -> bool {
        self.content.iter().any(|c| matches!(c, ContentBlock::Thinking(_)))
    }

    /// Check if this message has any tool calls.
    #[must_use]
    pub fn has_tool_use(&self) -> bool {
        self.content.iter().any(|c| matches!(c, ContentBlock::ToolUse(_)))
    }
}

/// Stop reason - why generation stopped.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StopReason {
    /// Generation paused for tool invocation (~94% of cases).
    ToolUse,
    /// Response completed naturally (~6% of cases).
    EndTurn,
    /// Output token limit reached (response may be truncated).
    MaxTokens,
    /// Custom stop sequence triggered.
    StopSequence,
}

/// Container context for code execution.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Container {
    /// Container identifier for reuse across requests.
    pub id: String,

    /// ISO 8601 timestamp when container expires.
    pub expires_at: String,

    /// Unknown fields for forward compatibility.
    #[serde(flatten)]
    pub extra: IndexMap<String, Value>,
}

/// Context management info (beta feature).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct ContextManagement {
    /// When to trigger: "input_tokens" or "tool_uses".
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trigger: Option<String>,

    /// Number of tool uses to retain.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub keep: Option<u32>,

    /// Minimum tokens to clear.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub clear_at_least: Option<u32>,

    /// Whether to clear tool input content.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub clear_tool_inputs: Option<bool>,

    /// List of context edits that were applied.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub applied_edits: Vec<ContextEdit>,

    /// Unknown fields for forward compatibility.
    #[serde(flatten)]
    pub extra: IndexMap<String, Value>,
}

/// A context edit that was applied.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContextEdit {
    /// Edit type.
    #[serde(rename = "type")]
    pub edit_type: String,

    /// Number of items cleared.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cleared_count: Option<u32>,

    /// Unknown fields for forward compatibility.
    #[serde(flatten)]
    pub extra: IndexMap<String, Value>,
}

/// Content block - one of 5 types.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentBlock {
    /// Natural language text.
    Text(TextBlock),

    /// Tool invocation request.
    ToolUse(ToolUse),

    /// Tool execution outcome.
    ToolResult(ToolResult),

    /// Extended reasoning with signature.
    Thinking(ThinkingBlock),

    /// Visual input (base64/url/file).
    Image(ImageBlock),
}

impl ContentBlock {
    /// Get the type name of this content block.
    #[must_use]
    pub const fn type_name(&self) -> &'static str {
        match self {
            Self::Text(_) => "text",
            Self::ToolUse(_) => "tool_use",
            Self::ToolResult(_) => "tool_result",
            Self::Thinking(_) => "thinking",
            Self::Image(_) => "image",
        }
    }

    /// Check if this is a text block.
    #[must_use]
    pub const fn is_text(&self) -> bool {
        matches!(self, Self::Text(_))
    }

    /// Check if this is a tool use block.
    #[must_use]
    pub const fn is_tool_use(&self) -> bool {
        matches!(self, Self::ToolUse(_))
    }

    /// Check if this is a tool result block.
    #[must_use]
    pub const fn is_tool_result(&self) -> bool {
        matches!(self, Self::ToolResult(_))
    }

    /// Check if this is a thinking block.
    #[must_use]
    pub const fn is_thinking(&self) -> bool {
        matches!(self, Self::Thinking(_))
    }

    /// Check if this is an image block.
    #[must_use]
    pub const fn is_image(&self) -> bool {
        matches!(self, Self::Image(_))
    }
}

/// Text content block.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TextBlock {
    /// The text content.
    pub text: String,

    /// Unknown fields for forward compatibility.
    #[serde(flatten)]
    pub extra: IndexMap<String, Value>,
}

/// Tool use content block - tool invocation request.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolUse {
    /// Tool use ID (toolu_* for client, srvtoolu_* for server).
    pub id: String,

    /// Tool name.
    pub name: String,

    /// Tool input parameters.
    pub input: Value,

    /// Unknown fields for forward compatibility.
    #[serde(flatten)]
    pub extra: IndexMap<String, Value>,
}

impl ToolUse {
    /// Check if this is a server-side tool (web_search, web_fetch, code_execution).
    #[must_use]
    pub fn is_server_tool(&self) -> bool {
        self.id.starts_with("srvtoolu_")
    }

    /// Check if this is an MCP tool.
    #[must_use]
    pub fn is_mcp_tool(&self) -> bool {
        self.name.starts_with("mcp__")
    }

    /// Get the MCP server name if this is an MCP tool.
    #[must_use]
    pub fn mcp_server(&self) -> Option<&str> {
        if self.is_mcp_tool() {
            self.name.strip_prefix("mcp__")?.split("__").next()
        } else {
            None
        }
    }

    /// Get the MCP method name if this is an MCP tool.
    #[must_use]
    pub fn mcp_method(&self) -> Option<&str> {
        if self.is_mcp_tool() {
            let parts: Vec<&str> = self.name.strip_prefix("mcp__")?.split("__").collect();
            if parts.len() >= 2 {
                Some(parts[1])
            } else {
                None
            }
        } else {
            None
        }
    }
}

/// Tool result content block - tool execution outcome.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolResult {
    /// Links to corresponding tool_use.id.
    pub tool_use_id: String,

    /// Result content - string, array, or absent/null.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<ToolResultContent>,

    /// Error state (three-state: true/false/absent).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub is_error: Option<bool>,

    /// Unknown fields for forward compatibility.
    #[serde(flatten)]
    pub extra: IndexMap<String, Value>,
}

impl ToolResult {
    /// Check if this is an error result.
    /// Treats absent `is_error` as success (implicit success).
    #[must_use]
    pub fn is_success(&self) -> bool {
        !self.is_error.unwrap_or(false)
    }

    /// Check if this is an explicit error.
    #[must_use]
    pub fn is_explicit_error(&self) -> bool {
        self.is_error == Some(true)
    }

    /// Check if this is an explicit success.
    #[must_use]
    pub fn is_explicit_success(&self) -> bool {
        self.is_error == Some(false)
    }

    /// Check if success is implicit (is_error absent).
    #[must_use]
    pub fn is_implicit_success(&self) -> bool {
        self.is_error.is_none()
    }

    /// Get the content as a string, if it's a simple string result.
    #[must_use]
    pub fn content_as_string(&self) -> Option<&str> {
        match &self.content {
            Some(ToolResultContent::String(s)) => Some(s),
            _ => None,
        }
    }
}

/// Tool result content - can be string, array, or absent.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ToolResultContent {
    /// String content (most tools).
    String(String),

    /// Array of content blocks (Task tool returns array).
    Array(Vec<Value>),
}

/// Thinking content block - extended reasoning.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ThinkingBlock {
    /// Reasoning text.
    pub thinking: String,

    /// Cryptographic verification hash.
    pub signature: String,

    /// Unknown fields for forward compatibility.
    #[serde(flatten)]
    pub extra: IndexMap<String, Value>,
}

/// Image content block - visual input.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImageBlock {
    /// Image source (base64, url, or file).
    pub source: ImageSource,

    /// Unknown fields for forward compatibility.
    #[serde(flatten)]
    pub extra: IndexMap<String, Value>,
}

/// Image source - base64, url, or file.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ImageSource {
    /// Inline base64-encoded data.
    Base64 {
        /// Media type (e.g., "image/png").
        media_type: String,
        /// Base64-encoded image data.
        data: String,
    },

    /// Public URL reference.
    Url {
        /// The URL.
        url: String,
    },

    /// Files API file ID (beta feature).
    File {
        /// File ID.
        file_id: String,
    },
}

impl ImageSource {
    /// Get the media type if known.
    #[must_use]
    pub fn media_type(&self) -> Option<&str> {
        match self {
            Self::Base64 { media_type, .. } => Some(media_type),
            Self::Url { .. } | Self::File { .. } => None,
        }
    }

    /// Check if this is base64-encoded.
    #[must_use]
    pub const fn is_base64(&self) -> bool {
        matches!(self, Self::Base64 { .. })
    }

    /// Get the approximate size in bytes (for base64 data).
    #[must_use]
    pub fn approximate_size(&self) -> Option<usize> {
        match self {
            Self::Base64 { data, .. } => {
                // Base64 encoding is ~4/3 of original size
                Some(data.len() * 3 / 4)
            }
            Self::Url { .. } | Self::File { .. } => None,
        }
    }
}

/// Known tool names in Claude Code.
pub mod tool_names {
    /// File reading tool.
    pub const READ: &str = "Read";
    /// File writing tool.
    pub const WRITE: &str = "Write";
    /// File editing tool.
    pub const EDIT: &str = "Edit";
    /// Multi-file editing tool.
    pub const MULTI_EDIT: &str = "MultiEdit";
    /// Bash command execution.
    pub const BASH: &str = "Bash";
    /// File globbing.
    pub const GLOB: &str = "Glob";
    /// Content search with regex.
    pub const GREP: &str = "Grep";
    /// Directory listing.
    pub const LS: &str = "LS";
    /// Web fetching.
    pub const WEB_FETCH: &str = "WebFetch";
    /// Web search.
    pub const WEB_SEARCH: &str = "WebSearch";
    /// Task/subagent spawning.
    pub const TASK: &str = "Task";
    /// Task output retrieval.
    pub const TASK_OUTPUT: &str = "TaskOutput";
    /// User question asking.
    pub const ASK_USER_QUESTION: &str = "AskUserQuestion";
    /// Todo reading.
    pub const TODO_READ: &str = "TodoRead";
    /// Todo writing.
    pub const TODO_WRITE: &str = "TodoWrite";
    /// Notebook reading.
    pub const NOTEBOOK_READ: &str = "NotebookRead";
    /// Notebook editing.
    pub const NOTEBOOK_EDIT: &str = "NotebookEdit";
    /// Shell killing.
    pub const KILL_SHELL: &str = "KillShell";
    /// LSP operations (v2.0.74+).
    pub const LSP: &str = "LSP";
    /// MCP resource listing.
    pub const LIST_MCP_RESOURCES: &str = "ListMcpResourcesTool";
    /// MCP resource reading.
    pub const READ_MCP_RESOURCE: &str = "ReadMcpResourceTool";
    /// Enter plan mode.
    pub const ENTER_PLAN_MODE: &str = "EnterPlanMode";
    /// Exit plan mode.
    pub const EXIT_PLAN_MODE: &str = "ExitPlanMode";
    /// Skill invocation.
    pub const SKILL: &str = "Skill";
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_tool_use_server_detection() {
        let client_tool = ToolUse {
            id: "toolu_013Xeg9XPXus1or3pHKNB6Lq".to_string(),
            name: "Read".to_string(),
            input: serde_json::json!({"file_path": "/test"}),
            extra: IndexMap::new(),
        };
        assert!(!client_tool.is_server_tool());

        let server_tool = ToolUse {
            id: "srvtoolu_018es16J4ZnSyvS3LSGnjFp9".to_string(),
            name: "web_search".to_string(),
            input: serde_json::json!({"query": "test"}),
            extra: IndexMap::new(),
        };
        assert!(server_tool.is_server_tool());
    }

    #[test]
    fn test_mcp_tool_detection() {
        let mcp_tool = ToolUse {
            id: "toolu_test".to_string(),
            name: "mcp__chrome__navigate".to_string(),
            input: serde_json::json!({}),
            extra: IndexMap::new(),
        };
        assert!(mcp_tool.is_mcp_tool());
        assert_eq!(mcp_tool.mcp_server(), Some("chrome"));
        assert_eq!(mcp_tool.mcp_method(), Some("navigate"));
    }

    #[test]
    fn test_tool_result_three_state() {
        // Explicit success
        let explicit_success = ToolResult {
            tool_use_id: "test".to_string(),
            content: None,
            is_error: Some(false),
            extra: IndexMap::new(),
        };
        assert!(explicit_success.is_success());
        assert!(explicit_success.is_explicit_success());
        assert!(!explicit_success.is_implicit_success());

        // Implicit success (absent)
        let implicit_success = ToolResult {
            tool_use_id: "test".to_string(),
            content: None,
            is_error: None,
            extra: IndexMap::new(),
        };
        assert!(implicit_success.is_success());
        assert!(!implicit_success.is_explicit_success());
        assert!(implicit_success.is_implicit_success());

        // Explicit error
        let error = ToolResult {
            tool_use_id: "test".to_string(),
            content: None,
            is_error: Some(true),
            extra: IndexMap::new(),
        };
        assert!(!error.is_success());
        assert!(error.is_explicit_error());
    }

    #[test]
    fn test_image_source_parsing() {
        let base64_json = r#"{"type":"base64","media_type":"image/png","data":"iVBORw0KGgo="}"#;
        let source: ImageSource = serde_json::from_str(base64_json).unwrap();
        assert!(source.is_base64());
        assert_eq!(source.media_type(), Some("image/png"));

        let url_json = r#"{"type":"url","url":"https://example.com/image.png"}"#;
        let source: ImageSource = serde_json::from_str(url_json).unwrap();
        assert!(!source.is_base64());
    }
}
