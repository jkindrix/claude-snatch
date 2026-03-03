//! Request/response types for MCP server tools.
//!
//! These are serialization-oriented structs with self-documenting field names.
//! Doc comments are on the structs and request types; individual fields
//! are clear from their names and serde attributes.
// Serialization structs with self-documenting field names — suppress field-level doc warnings.
#![allow(missing_docs)]

use mcpkit::prelude::*;
use serde::Serialize;

// ============================================================================
// Existing Tool Types
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
    pub session_id: String,
    pub project_path: String,
    pub is_subagent: bool,
    pub modified_time: Option<String>,
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
    pub session_id: String,
    pub project_path: String,
    pub is_subagent: bool,
    pub is_active: bool,
    pub modified_time: Option<String>,
    pub duration: Option<String>,
    pub primary_model: Option<String>,
    pub total_tokens: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub messages: usize,
    pub user_messages: usize,
    pub assistant_messages: usize,
    pub tool_invocations: usize,
    pub cache_hit_rate: f64,
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
    pub scope: String,
    pub sessions: Option<usize>,
    pub total_tokens: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub messages: usize,
    pub tool_invocations: usize,
    pub estimated_cost: Option<f64>,
}

// ============================================================================
// New Tool Types: get_session_messages
// ============================================================================

/// Request to read conversation messages from a session.
#[derive(Debug, Deserialize, ToolInput)]
pub struct GetSessionMessagesRequest {
    /// Session ID (full or prefix).
    pub session_id: String,

    /// Detail level: "overview" (user prompts only, truncated),
    /// "conversation" (user prompts + assistant text, skips tool-only turns),
    /// "standard" (user + assistant text, tool names only),
    /// "full" (includes tool call details).
    /// Default: "standard".
    pub detail: Option<String>,

    /// Message type filter: "user", "assistant", "system", "all".
    /// Default: "all".
    pub message_type: Option<String>,

    /// Maximum number of messages to return. Default: 50.
    pub limit: Option<usize>,

    /// Offset for pagination (skip first N messages). Default: 0.
    pub offset: Option<usize>,

    /// If true, return messages in reverse chronological order.
    /// Default: false.
    pub reverse: Option<bool>,

    /// If true, include thinking/reasoning block content in assistant messages.
    /// Thinking blocks contain decision rationale and evidence chains that
    /// compaction always drops. Default: false.
    pub include_thinking: Option<bool>,
}

/// A message in the session messages response.
#[derive(Debug, Serialize)]
pub struct MessageEntry {
    pub index: usize,
    #[serde(rename = "type")]
    pub msg_type: String,
    pub timestamp: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub git_branch: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_details: Option<Vec<ToolDetail>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub has_thinking: Option<bool>,
    /// Thinking/reasoning block content (only included when include_thinking=true).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thinking_preview: Option<String>,
}

/// Tool call detail for "full" detail level.
#[derive(Debug, Serialize)]
pub struct ToolDetail {
    pub tool_name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pattern: Option<String>,
}

/// Response for get_session_messages.
#[derive(Debug, Serialize)]
pub struct SessionMessagesResponse {
    pub session_id: String,
    pub project_path: String,
    pub total_messages: usize,
    pub returned: usize,
    pub offset: usize,
    pub messages: Vec<MessageEntry>,
}

// ============================================================================
// New Tool Types: get_session_timeline
// ============================================================================

/// Request for session timeline.
#[derive(Debug, Deserialize, ToolInput)]
pub struct GetSessionTimelineRequest {
    /// Session ID (full or prefix).
    pub session_id: String,

    /// Maximum timeline entries. Default: 30.
    pub limit: Option<usize>,
}

/// A turn in the session timeline.
#[derive(Debug, Clone, Serialize)]
pub struct TimelineTurn {
    pub index: usize,
    pub timestamp: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user_prompt: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub assistant_summary: Option<String>,
    pub tools_used: Vec<String>,
    pub files_touched: Vec<String>,
    pub had_errors: bool,
}

/// A compaction event in the timeline.
#[derive(Debug, Serialize)]
pub struct CompactionEvent {
    pub timestamp: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
}

/// Response for get_session_timeline.
#[derive(Debug, Serialize)]
pub struct SessionTimelineResponse {
    pub session_id: String,
    pub project_path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub start_time: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub end_time: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration: Option<String>,
    pub total_turns: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub git_branch: Option<String>,
    pub timeline: Vec<TimelineTurn>,
    pub compaction_events: Vec<CompactionEvent>,
}

// ============================================================================
// New Tool Types: get_project_history
// ============================================================================

/// Request for project history.
#[derive(Debug, Deserialize, ToolInput)]
pub struct GetProjectHistoryRequest {
    /// Project path filter (substring match).
    pub project: String,

    /// Time period: "24h", "7d", "30d", "all". Default: "7d".
    pub period: Option<String>,

    /// Maximum sessions to include. Default: 20.
    pub limit: Option<usize>,

    /// Include brief summaries of each session. Default: true.
    pub include_summaries: Option<bool>,
}

/// A session entry in project history.
#[derive(Debug, Serialize)]
pub struct ProjectSessionEntry {
    pub session_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub start_time: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub end_time: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub git_branch: Option<String>,
    pub user_prompt_count: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub first_prompt: Option<String>,
    pub key_prompts: Vec<String>,
    pub tools_summary: std::collections::HashMap<String, usize>,
    pub files_touched: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub estimated_cost: Option<f64>,
    pub total_tokens: u64,
}

/// Aggregate stats across project sessions.
#[derive(Debug, Serialize)]
pub struct ProjectAggregate {
    pub total_sessions: usize,
    pub total_tokens: u64,
    pub total_cost: f64,
    pub total_prompts: usize,
    pub active_branches: Vec<String>,
}

/// Response for get_project_history.
#[derive(Debug, Serialize)]
pub struct ProjectHistoryResponse {
    pub project_path: String,
    pub period: String,
    pub sessions_found: usize,
    pub sessions: Vec<ProjectSessionEntry>,
    pub aggregate: ProjectAggregate,
}

// ============================================================================
// New Tool Types: search_sessions
// ============================================================================

/// Request for searching across sessions.
#[derive(Debug, Deserialize, ToolInput)]
pub struct SearchSessionsRequest {
    /// Search pattern (regex supported).
    pub pattern: String,

    /// Filter by project path (substring match).
    pub project: Option<String>,

    /// Filter by specific session ID.
    pub session_id: Option<String>,

    /// Search scope: "text" (user+assistant text), "tools" (tool I/O),
    /// "thinking" (reasoning blocks), "all" (everything). Default: "text".
    pub scope: Option<String>,

    /// Case-insensitive search. Default: true.
    pub ignore_case: Option<bool>,

    /// Maximum results to return. Default: 20.
    pub limit: Option<usize>,
}

/// A search result match.
#[derive(Debug, Serialize)]
pub struct SearchMatch {
    pub session_id: String,
    pub project_path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timestamp: Option<String>,
    pub message_type: String,
    pub matched_text: String,
    pub context: String,
}

/// Response for search_sessions.
#[derive(Debug, Serialize)]
pub struct SearchSessionsResponse {
    pub pattern: String,
    pub total_matches: usize,
    pub returned: usize,
    pub results: Vec<SearchMatch>,
}

// ============================================================================
// New Tool Types: get_tool_calls
// ============================================================================

/// Request for tool call extraction.
#[derive(Debug, Deserialize, ToolInput)]
pub struct GetToolCallsRequest {
    /// Session ID (full or prefix).
    pub session_id: String,

    /// Filter by tool name (comma-separated). If omitted, returns all.
    pub tool_filter: Option<String>,

    /// Only include tool calls that resulted in errors.
    pub errors_only: Option<bool>,

    /// Maximum tool calls to return. Default: 100.
    pub limit: Option<usize>,

    /// Offset for pagination. Default: 0.
    pub offset: Option<usize>,
}

/// A tool call entry.
#[derive(Debug, Serialize)]
pub struct ToolCallEntry {
    pub index: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timestamp: Option<String>,
    pub tool_name: String,
    pub input_summary: std::collections::HashMap<String, String>,
    pub had_error: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_preview: Option<String>,
}

/// Tool call summary stats.
#[derive(Debug, Serialize)]
pub struct ToolCallsSummary {
    pub by_tool: std::collections::HashMap<String, usize>,
    pub files_written: Vec<String>,
    pub files_edited: Vec<String>,
    pub error_count: usize,
}

/// Response for get_tool_calls.
#[derive(Debug, Serialize)]
pub struct ToolCallsResponse {
    pub session_id: String,
    pub total_tool_calls: usize,
    pub returned: usize,
    pub tool_calls: Vec<ToolCallEntry>,
    pub summary: ToolCallsSummary,
}

// ============================================================================
// New Tool Types: get_session_lessons
// ============================================================================

/// Request for session lessons extraction.
#[derive(Debug, Deserialize, ToolInput)]
pub struct GetSessionLessonsRequest {
    /// Session ID (full or prefix).
    pub session_id: String,

    /// Lesson category filter: "errors" (error→fix pairs), "corrections"
    /// (user corrections of agent behavior), "all". Default: "all".
    pub category: Option<String>,

    /// Maximum lessons to return. Default: 30.
    pub limit: Option<usize>,
}

/// An error→fix lesson: a tool call that failed, and what happened next.
#[derive(Debug, Clone, Serialize)]
pub struct ErrorFixLesson {
    /// When the error occurred.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timestamp: Option<String>,
    /// The tool that errored.
    pub tool_name: String,
    /// Key input fields for the failing call.
    pub input_summary: std::collections::HashMap<String, String>,
    /// Preview of the error message.
    pub error_preview: String,
    /// What the assistant did next (text summary of next response).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resolution_summary: Option<String>,
    /// Tools used in the resolution attempt.
    pub resolution_tools: Vec<String>,
}

/// A user correction: where the user corrected the agent's behavior.
#[derive(Debug, Clone, Serialize)]
pub struct UserCorrection {
    /// When the correction was made.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timestamp: Option<String>,
    /// The user's correction text.
    pub user_text: String,
    /// What the assistant was doing before (summary of previous response).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prior_assistant_summary: Option<String>,
}

/// Response for get_session_lessons.
#[derive(Debug, Serialize)]
pub struct SessionLessonsResponse {
    pub session_id: String,
    pub project_path: String,
    pub error_fix_pairs: Vec<ErrorFixLesson>,
    pub user_corrections: Vec<UserCorrection>,
    pub summary: LessonsSummary,
}

/// Summary of lessons found.
#[derive(Debug, Serialize)]
pub struct LessonsSummary {
    pub total_errors: usize,
    pub total_corrections: usize,
    pub most_error_prone_tools: Vec<(String, usize)>,
}

// ============================================================================
// New Tool Types: manage_goals
// ============================================================================

/// Request for goal management operations.
#[derive(Debug, Deserialize, ToolInput)]
pub struct ManageGoalsRequest {
    /// Operation: "list", "add", "update", "remove".
    pub operation: String,

    /// Project path filter (substring match). Required.
    pub project: String,

    /// Goal text (required for "add").
    pub text: Option<String>,

    /// Goal ID (required for "update" and "remove").
    pub id: Option<u64>,

    /// New status for "update": "open", "in_progress", "done", "abandoned".
    pub status: Option<String>,

    /// Progress notes for "add" or "update".
    pub progress: Option<String>,
}

/// A goal entry in responses.
#[derive(Debug, Serialize)]
pub struct GoalEntry {
    pub id: u64,
    pub text: String,
    pub status: String,
    pub created_at: String,
    pub updated_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub progress: Option<String>,
}

/// Response for manage_goals.
#[derive(Debug, Serialize)]
pub struct ManageGoalsResponse {
    pub operation: String,
    pub project_path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub goals: Option<Vec<GoalEntry>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub goal: Option<GoalEntry>,
}

// ============================================================================
// New Tool Types: get_session_digest
// ============================================================================

/// Request for session digest.
#[derive(Debug, Deserialize, ToolInput)]
pub struct GetSessionDigestRequest {
    /// Session ID (full or prefix).
    pub session_id: String,

    /// Maximum key prompts to include. Default: 3.
    pub max_prompts: Option<usize>,

    /// Maximum files to include. Default: 10.
    pub max_files: Option<usize>,
}

/// Response for get_session_digest.
#[derive(Debug, Serialize)]
pub struct SessionDigestResponse {
    pub session_id: String,
    pub project_path: String,
    pub key_prompts: Vec<String>,
    pub files_touched: Vec<String>,
    pub top_tools: Vec<(String, usize)>,
    pub error_count: usize,
    pub compaction_count: usize,
    pub thinking_keywords: Vec<String>,
    pub formatted: String,
}
