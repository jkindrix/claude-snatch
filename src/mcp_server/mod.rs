//! MCP (Model Context Protocol) server implementation.
//!
//! Exposes claude-snatch functionality as MCP tools for AI model integration.
//!
//! # Tools Provided
//!
//! - `list_sessions` - List Claude Code sessions
//! - `get_session_info` - Get detailed session information
//! - `get_stats` - Get usage statistics

#![cfg(feature = "mcp")]

use std::path::PathBuf;

use mcpkit::prelude::*;
use mcpkit::transport::stdio::StdioTransport;

use crate::analytics::SessionAnalytics;
use crate::discovery::ClaudeDirectory;
use crate::reconstruction::Conversation;

// ============================================================================
// Tool Request/Response Types
// ============================================================================

/// Request to list sessions.
#[derive(Debug, Deserialize, ToolInput)]
pub struct ListSessionsRequest {
    /// Filter sessions by project path (substring match).
    pub project: Option<String>,

    /// Maximum number of sessions to return (default: 50).
    pub limit: Option<usize>,

    /// Include subagent sessions in results.
    pub include_subagents: Option<bool>,
}

/// Session summary for list responses.
#[derive(Debug, Serialize)]
pub struct SessionSummary {
    /// Unique session identifier (UUID).
    pub session_id: String,
    /// Path to the project this session belongs to.
    pub project_path: String,
    /// Whether this is a subagent session.
    pub is_subagent: bool,
    /// Last modification time in RFC3339 format.
    pub modified_time: Option<String>,
    /// Whether the session is currently active.
    pub is_active: bool,
}

/// Request to get session info.
#[derive(Debug, Deserialize, ToolInput)]
pub struct GetSessionInfoRequest {
    /// Session ID to get info for (can use prefix).
    pub session_id: String,
}

/// Session info response.
#[derive(Debug, Serialize)]
pub struct SessionInfoResponse {
    /// Unique session identifier (UUID).
    pub session_id: String,
    /// Path to the project this session belongs to.
    pub project_path: String,
    /// Whether this is a subagent session.
    pub is_subagent: bool,
    /// Whether the session is currently active.
    pub is_active: bool,
    /// Last modification time in RFC3339 format.
    pub modified_time: Option<String>,
    /// Session duration in human-readable format.
    pub duration: Option<String>,
    /// Primary model used in the session.
    pub primary_model: Option<String>,
    /// Total tokens used (input + output).
    pub total_tokens: u64,
    /// Input tokens used.
    pub input_tokens: u64,
    /// Output tokens used.
    pub output_tokens: u64,
    /// Total message count.
    pub messages: usize,
    /// User message count.
    pub user_messages: usize,
    /// Assistant message count.
    pub assistant_messages: usize,
    /// Number of tool invocations.
    pub tool_invocations: usize,
    /// Cache hit rate (0.0-1.0).
    pub cache_hit_rate: f64,
    /// Estimated cost in USD.
    pub estimated_cost: Option<f64>,
}

/// Request to get stats.
#[derive(Debug, Deserialize, ToolInput)]
pub struct GetStatsRequest {
    /// Session ID for session-specific stats.
    pub session_id: Option<String>,

    /// Project path filter for project-specific stats.
    pub project: Option<String>,
}

/// Stats response.
#[derive(Debug, Serialize)]
pub struct StatsResponse {
    /// Scope of the stats: "session", "project", or "global".
    pub scope: String,
    /// Number of sessions included in the stats.
    pub sessions: Option<usize>,
    /// Total tokens used (input + output).
    pub total_tokens: u64,
    /// Input tokens used.
    pub input_tokens: u64,
    /// Output tokens used.
    pub output_tokens: u64,
    /// Total message count.
    pub messages: usize,
    /// Number of tool invocations.
    pub tool_invocations: usize,
    /// Estimated cost in USD.
    pub estimated_cost: Option<f64>,
}

// ============================================================================
// MCP Server Implementation
// ============================================================================

/// Claude-snatch MCP server.
#[derive(Debug, Clone)]
pub struct SnatchServer {
    /// Claude directory path.
    claude_dir: Option<PathBuf>,
    /// Maximum file size for parsing.
    max_file_size: Option<u64>,
}

impl SnatchServer {
    /// Create a new MCP server instance.
    pub fn new(claude_dir: Option<PathBuf>, max_file_size: Option<u64>) -> Self {
        Self {
            claude_dir,
            max_file_size,
        }
    }

    /// Get the Claude directory.
    fn get_claude_dir(&self) -> Result<ClaudeDirectory, String> {
        let result = if let Some(ref path) = self.claude_dir {
            ClaudeDirectory::from_path(path.clone())
        } else {
            ClaudeDirectory::discover()
        };
        result.map_err(|e| format!("Failed to access Claude directory: {e}"))
    }
}

#[mcp_server(name = "claude-snatch", version = "0.1.0")]
impl SnatchServer {
    /// List Claude Code sessions with optional filtering.
    #[tool(description = "List Claude Code sessions with optional filtering by project")]
    async fn list_sessions(&self, request: ListSessionsRequest) -> ToolOutput {
        let claude_dir = match self.get_claude_dir() {
            Ok(dir) => dir,
            Err(e) => return ToolOutput::error(e),
        };

        let mut sessions = match claude_dir.all_sessions() {
            Ok(s) => s,
            Err(e) => return ToolOutput::error(format!("Failed to list sessions: {e}")),
        };

        // Apply project filter
        if let Some(ref project) = request.project {
            sessions.retain(|s| s.project_path().contains(project));
        }

        // Filter subagents
        if !request.include_subagents.unwrap_or(false) {
            sessions.retain(|s| !s.is_subagent());
        }

        // Apply limit
        let limit = request.limit.unwrap_or(50);
        sessions.truncate(limit);

        // Convert to summaries
        let summaries: Vec<SessionSummary> = sessions
            .iter()
            .map(|s| SessionSummary {
                session_id: s.session_id().to_string(),
                project_path: s.project_path().to_string(),
                is_subagent: s.is_subagent(),
                modified_time: Some(s.modified_datetime().to_rfc3339()),
                is_active: s.is_active().unwrap_or(false),
            })
            .collect();

        match ToolOutput::json(&summaries) {
            Ok(output) => output,
            Err(e) => ToolOutput::error(format!("JSON serialization error: {e}")),
        }
    }

    /// Get detailed information about a specific Claude Code session.
    #[tool(description = "Get detailed information about a specific Claude Code session")]
    async fn get_session_info(&self, request: GetSessionInfoRequest) -> ToolOutput {
        let claude_dir = match self.get_claude_dir() {
            Ok(dir) => dir,
            Err(e) => return ToolOutput::error(e),
        };

        let session = match claude_dir.find_session(&request.session_id) {
            Ok(Some(s)) => s,
            Ok(None) => {
                return ToolOutput::error(format!("Session not found: {}", request.session_id))
            }
            Err(e) => return ToolOutput::error(format!("Failed to find session: {e}")),
        };

        let entries = match session.parse_with_options(self.max_file_size) {
            Ok(e) => e,
            Err(e) => return ToolOutput::error(format!("Failed to parse session: {e}")),
        };

        let conversation = match Conversation::from_entries(entries) {
            Ok(c) => c,
            Err(e) => {
                return ToolOutput::error(format!("Failed to reconstruct conversation: {e}"))
            }
        };

        let analytics = SessionAnalytics::from_conversation(&conversation);
        let summary = analytics.summary_report();

        let info = SessionInfoResponse {
            session_id: session.session_id().to_string(),
            project_path: session.project_path().to_string(),
            is_subagent: session.is_subagent(),
            is_active: session.is_active().unwrap_or(false),
            modified_time: Some(session.modified_datetime().to_rfc3339()),
            duration: analytics.duration_string(),
            primary_model: analytics.primary_model().map(String::from),
            total_tokens: summary.total_tokens,
            input_tokens: summary.input_tokens,
            output_tokens: summary.output_tokens,
            messages: summary.total_messages,
            user_messages: summary.user_messages,
            assistant_messages: summary.assistant_messages,
            tool_invocations: summary.tool_invocations,
            cache_hit_rate: summary.cache_hit_rate,
            estimated_cost: summary.estimated_cost,
        };

        match ToolOutput::json(&info) {
            Ok(output) => output,
            Err(e) => ToolOutput::error(format!("JSON serialization error: {e}")),
        }
    }

    /// Get usage statistics for sessions, projects, or globally.
    #[tool(description = "Get usage statistics for sessions, projects, or globally")]
    async fn get_stats(&self, request: GetStatsRequest) -> ToolOutput {
        let claude_dir = match self.get_claude_dir() {
            Ok(dir) => dir,
            Err(e) => return ToolOutput::error(e),
        };

        let response = if let Some(session_id) = request.session_id {
            // Session-specific stats
            let session = match claude_dir.find_session(&session_id) {
                Ok(Some(s)) => s,
                Ok(None) => return ToolOutput::error(format!("Session not found: {session_id}")),
                Err(e) => return ToolOutput::error(format!("Failed to find session: {e}")),
            };

            let entries = match session.parse_with_options(self.max_file_size) {
                Ok(e) => e,
                Err(e) => return ToolOutput::error(format!("Failed to parse session: {e}")),
            };

            let conversation = match Conversation::from_entries(entries) {
                Ok(c) => c,
                Err(e) => {
                    return ToolOutput::error(format!("Failed to reconstruct conversation: {e}"))
                }
            };

            let analytics = SessionAnalytics::from_conversation(&conversation);
            let summary = analytics.summary_report();

            StatsResponse {
                scope: "session".to_string(),
                sessions: Some(1),
                total_tokens: summary.total_tokens,
                input_tokens: summary.input_tokens,
                output_tokens: summary.output_tokens,
                messages: summary.total_messages,
                tool_invocations: summary.tool_invocations,
                estimated_cost: summary.estimated_cost,
            }
        } else if let Some(project) = request.project {
            // Project-specific stats
            let sessions = match claude_dir.all_sessions() {
                Ok(s) => s,
                Err(e) => return ToolOutput::error(format!("Failed to list sessions: {e}")),
            };

            let project_sessions: Vec<_> = sessions
                .iter()
                .filter(|s| s.project_path().contains(&project))
                .collect();

            let mut total_tokens = 0u64;
            let mut input_tokens = 0u64;
            let mut output_tokens = 0u64;
            let mut messages = 0usize;
            let mut tool_invocations = 0usize;
            let mut cost = 0.0f64;

            for session in &project_sessions {
                if let Ok(entries) = session.parse_with_options(self.max_file_size) {
                    if let Ok(conversation) = Conversation::from_entries(entries) {
                        let analytics = SessionAnalytics::from_conversation(&conversation);
                        let summary = analytics.summary_report();
                        total_tokens += summary.total_tokens;
                        input_tokens += summary.input_tokens;
                        output_tokens += summary.output_tokens;
                        messages += summary.total_messages;
                        tool_invocations += summary.tool_invocations;
                        cost += summary.estimated_cost.unwrap_or(0.0);
                    }
                }
            }

            StatsResponse {
                scope: project,
                sessions: Some(project_sessions.len()),
                total_tokens,
                input_tokens,
                output_tokens,
                messages,
                tool_invocations,
                estimated_cost: if cost > 0.0 { Some(cost) } else { None },
            }
        } else {
            // Global stats
            let sessions = match claude_dir.all_sessions() {
                Ok(s) => s,
                Err(e) => return ToolOutput::error(format!("Failed to list sessions: {e}")),
            };

            let mut total_tokens = 0u64;
            let mut input_tokens = 0u64;
            let mut output_tokens = 0u64;
            let mut messages = 0usize;
            let mut tool_invocations = 0usize;
            let mut cost = 0.0f64;

            for session in &sessions {
                if let Ok(entries) = session.parse_with_options(self.max_file_size) {
                    if let Ok(conversation) = Conversation::from_entries(entries) {
                        let analytics = SessionAnalytics::from_conversation(&conversation);
                        let summary = analytics.summary_report();
                        total_tokens += summary.total_tokens;
                        input_tokens += summary.input_tokens;
                        output_tokens += summary.output_tokens;
                        messages += summary.total_messages;
                        tool_invocations += summary.tool_invocations;
                        cost += summary.estimated_cost.unwrap_or(0.0);
                    }
                }
            }

            StatsResponse {
                scope: "global".to_string(),
                sessions: Some(sessions.len()),
                total_tokens,
                input_tokens,
                output_tokens,
                messages,
                tool_invocations,
                estimated_cost: if cost > 0.0 { Some(cost) } else { None },
            }
        };

        match ToolOutput::json(&response) {
            Ok(output) => output,
            Err(e) => ToolOutput::error(format!("JSON serialization error: {e}")),
        }
    }
}

/// Run the MCP server.
pub async fn run_server(
    claude_dir: Option<PathBuf>,
    max_file_size: Option<u64>,
) -> crate::error::Result<()> {
    let server = SnatchServer::new(claude_dir, max_file_size);
    let transport = StdioTransport::new();

    server
        .into_server()
        .serve(transport)
        .await
        .map_err(|e| crate::error::SnatchError::ExportError {
            message: format!("MCP server error: {e}"),
            source: None,
        })
}
