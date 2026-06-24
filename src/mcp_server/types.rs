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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub slug: Option<String>,
    pub project_path: String,
    pub is_subagent: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_session_id: Option<String>,
    pub modified_time: Option<String>,
    pub is_active: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration: Option<String>,
    pub compaction_count: usize,
    /// Root session ID if this session is part of a chain.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub chain_id: Option<String>,
    /// Number of files in the chain (only set for chain members).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub chain_length: Option<usize>,
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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub slug: Option<String>,
    /// Root session ID if this session is part of a chain.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub chain_id: Option<String>,
    /// All file UUIDs in the chain, in order.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub chain_members: Option<Vec<String>>,
    pub project_path: String,
    pub is_subagent: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_session_id: Option<String>,
    pub is_active: bool,
    pub modified_time: Option<String>,
    pub duration: Option<String>,
    pub compaction_count: usize,
    pub primary_model: Option<String>,
    pub total_tokens: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_tokens: u64,
    pub cache_creation_tokens: u64,
    pub total_processed_tokens: u64,
    pub messages: usize,
    pub user_messages: usize,
    pub assistant_messages: usize,
    pub tool_invocations: usize,
    pub cache_hit_rate: f64,
    pub estimated_cost: Option<f64>,
    /// Subagents spawned by this session, attached to their spawning Agent/Task
    /// call via `tool_use_id` when available. Empty for subagent sessions or when
    /// none were spawned. The transcripts live in separate `agent-*.jsonl` files
    /// and are not counted in this session's own `messages`.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub subagents: Vec<SubagentSummary>,
}

/// Summary of one subagent spawned by a session, for `get_session_info`.
#[derive(Debug, Serialize)]
pub struct SubagentSummary {
    /// Subagent session id (`agent-<hash>`); query it directly for the transcript.
    pub agent_session_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Spawning Agent/Task tool_use id, when the sidecar records it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_use_id: Option<String>,
    /// User + assistant message count in the subagent transcript; absent if the
    /// transcript could not be read.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message_count: Option<usize>,
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
    pub cache_read_tokens: u64,
    pub cache_creation_tokens: u64,
    pub total_processed_tokens: u64,
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

    /// If true and the session is part of a chain, return messages across all
    /// member files in the chain. Default: false.
    pub chain_aware: Option<bool>,

    /// Only include messages after this timestamp (ISO 8601 or relative like "2h", "30m").
    /// Enables contextual zoom: find an event timestamp from another tool, then
    /// retrieve messages around it.
    pub after_timestamp: Option<String>,

    /// Only include messages before this timestamp (ISO 8601 or relative like "2h", "30m").
    pub before_timestamp: Option<String>,
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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subagent_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prompt: Option<String>,
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

    /// If true and the session is part of a chain, build timeline across all
    /// member files. Default: false.
    pub chain_aware: Option<bool>,
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

/// An error-level system event in the timeline (e.g. an API error).
#[derive(Debug, Serialize)]
pub struct ErrorEvent {
    pub timestamp: String,
    pub message: String,
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
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub error_events: Vec<ErrorEvent>,
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
    pub slug: Option<String>,
    /// Root chain ID if this session represents a chain.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub chain_id: Option<String>,
    /// Number of files in the chain.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub chain_length: Option<usize>,
    pub is_subagent: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_session_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub start_time: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub end_time: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration: Option<String>,
    pub compaction_count: usize,
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
    /// Root chain ID if this session is part of a chain.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub chain_id: Option<String>,
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
    pub recent_prompts: Vec<String>,
    pub total_prompts: usize,
    pub files_touched: Vec<String>,
    pub top_tools: Vec<(String, usize)>,
    pub error_count: usize,
    pub compaction_count: usize,
    pub thinking_keywords: Vec<String>,
    pub formatted: String,
}

// ============================================================================
// New Tool Types: manage_notes
// ============================================================================

/// Request for tactical note management operations.
#[derive(Debug, Deserialize, ToolInput)]
pub struct ManageNotesRequest {
    /// Operation: "list", "add", "remove", "clear".
    pub operation: String,

    /// Project path filter (substring match). Required.
    pub project: String,

    /// Note text (required for "add").
    pub text: Option<String>,

    /// Session ID to tag the note with (optional, for "add").
    pub session_id: Option<String>,

    /// Note ID (required for "remove").
    pub id: Option<u64>,
}

/// A note entry in responses.
#[derive(Debug, Serialize)]
pub struct NoteEntry {
    pub id: u64,
    pub text: String,
    pub created_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
}

/// Response for manage_notes.
#[derive(Debug, Serialize)]
pub struct ManageNotesResponse {
    pub operation: String,
    pub project_path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub notes: Option<Vec<NoteEntry>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<NoteEntry>,
}

// ============================================================================
// New Tool Types: manage_decisions
// ============================================================================

/// Request for decision management operations.
#[derive(Debug, Deserialize, ToolInput)]
pub struct ManageDecisionsRequest {
    /// Operation: "list", "add", "update", "remove", "supersede". Auto-scoring via CLI only.
    pub operation: String,

    /// Project path filter (substring match). Required.
    pub project: String,

    /// Decision title (required for "add").
    pub title: Option<String>,

    /// Decision description.
    pub description: Option<String>,

    /// Decision ID (required for "update", "remove", "supersede").
    pub id: Option<u64>,

    /// Status: "proposed", "confirmed", "superseded", "abandoned".
    pub status: Option<String>,

    /// Confidence score (0.0 to 1.0).
    pub confidence: Option<f64>,

    /// Comma-separated tags. Also used as filter for "list".
    pub tags: Option<String>,

    /// ID of decision that supersedes this one (for "supersede" operation).
    pub superseded_by: Option<u64>,

    /// Session ID where the decision was made.
    pub session_id: Option<String>,
}

/// A decision entry in responses.
#[derive(Debug, Serialize)]
pub struct DecisionEntry {
    pub id: u64,
    pub title: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub status: String,
    pub confidence: f64,
    pub created_at: String,
    pub updated_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub superseded_by: Option<u64>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub references: Vec<String>,
}

/// Response for manage_decisions.
#[derive(Debug, Serialize)]
pub struct ManageDecisionsResponse {
    pub operation: String,
    pub project_path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub decisions: Option<Vec<DecisionEntry>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub decision: Option<DecisionEntry>,
}

// ============================================================================
// Message Tagging
// ============================================================================

/// Request to tag or manage message-level tags.
#[derive(Debug, Deserialize, ToolInput)]
pub struct TagMessageRequest {
    /// Operation: "add", "remove", "list", "search".
    pub operation: String,

    /// Project path filter (substring match). Required.
    pub project: String,

    /// Session ID containing the message (required for "add").
    pub session_id: Option<String>,

    /// Message UUID to tag (required for "add" and "remove").
    pub message_uuid: Option<String>,

    /// Tag to add or remove (required for "add" and "remove"). For "search", filters by this tag.
    pub tag: Option<String>,
}

/// A tagged message entry in responses.
#[derive(Debug, Serialize)]
pub struct TaggedMessageEntry {
    pub session_id: String,
    pub message_uuid: String,
    pub tags: Vec<MessageTagEntry>,
}

/// A single tag on a message.
#[derive(Debug, Serialize)]
pub struct MessageTagEntry {
    pub tag: String,
    pub created_at: String,
    pub source: String,
}

// ============================================================================
// File History
// ============================================================================

/// Request for file history lookup.
#[derive(Debug, Deserialize, ToolInput)]
pub struct GetFileHistoryRequest {
    /// File path to look up (substring match).
    pub path: String,

    /// Filter by project path (substring match).
    pub project: Option<String>,

    /// Maximum results to return. Default: 50.
    pub limit: Option<usize>,
}

/// A file modification entry in responses.
#[derive(Debug, Serialize)]
pub struct FileModificationEntry {
    pub file_path: String,
    pub session_id: String,
    pub project_path: String,
    pub message_id: String,
    pub timestamp: String,
    pub version: u32,
}

// ============================================================================
// Topic Threading
// ============================================================================

/// Request for cross-session topic threading.
#[derive(Debug, Deserialize, ToolInput)]
pub struct ThreadTopicRequest {
    /// Search pattern (regex supported).
    pub pattern: String,

    /// Filter by project path (substring match).
    pub project: Option<String>,

    /// Filter by specific session ID.
    pub session_id: Option<String>,

    /// Include thinking/reasoning blocks in search and output. Default: false.
    pub include_thinking: Option<bool>,

    /// Exclude subagent sessions. Default: true.
    pub no_subagents: Option<bool>,

    /// Only sessions modified after this date (e.g., "7d", "2026-03-01").
    pub since: Option<String>,

    /// Only sessions modified before this date.
    pub until: Option<String>,

    /// Only include exchanges that look like decision points. Default: false.
    pub decisions_only: Option<bool>,

    /// Maximum exchanges to return. Default: 30.
    pub limit: Option<usize>,

    /// Maximum characters per context field. Default: 500.
    pub max_context: Option<usize>,
}

/// A threaded exchange entry in responses.
#[derive(Debug, Serialize)]
pub struct ThreadExchangeEntry {
    pub timestamp: String,
    pub session_id: String,
    pub project: String,
    pub entry_uuid: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user_text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub assistant_text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thinking_text: Option<String>,
    pub match_location: String,
    pub match_count: usize,
}

/// Response for thread_topic.
#[derive(Debug, Serialize)]
pub struct ThreadTopicResponse {
    pub pattern: String,
    pub total_exchanges: usize,
    pub session_count: usize,
    pub total_matches: usize,
    pub exchanges: Vec<ThreadExchangeEntry>,
}

// ============================================================================
// Decision Detection
// ============================================================================

/// Request for decision point detection.
#[derive(Debug, Deserialize, ToolInput)]
pub struct DetectDecisionsRequest {
    /// Filter by project path (substring match).
    pub project: Option<String>,

    /// Filter by specific session ID.
    pub session_id: Option<String>,

    /// Minimum confidence threshold (0.0-1.0). Default: 0.5.
    pub min_confidence: Option<f64>,

    /// Exclude subagent sessions. Default: true.
    pub no_subagents: Option<bool>,

    /// Only sessions modified after this date.
    pub since: Option<String>,

    /// Only sessions modified before this date.
    pub until: Option<String>,

    /// Topic filter regex (applied after detection).
    pub topic: Option<String>,

    /// Maximum candidates to return. Default: 50.
    pub limit: Option<usize>,
}

/// A detected decision candidate in responses.
#[derive(Debug, Serialize)]
pub struct DetectedDecisionEntry {
    pub timestamp: String,
    pub session_id: String,
    pub entry_uuid: String,
    pub detection_method: String,
    pub confidence: f64,
    pub question: String,
    pub response: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub confirmation: Option<String>,
}

/// Response for detect_decisions.
#[derive(Debug, Serialize)]
pub struct DetectDecisionsResponse {
    pub total_candidates: usize,
    pub candidates: Vec<DetectedDecisionEntry>,
}

// ============================================================================
// Conflict Detection
// ============================================================================

/// Request for conflict/contradiction detection.
#[derive(Debug, Deserialize, ToolInput)]
pub struct DetectConflictsRequest {
    /// Filter by project path (substring match). Required for registry-based detection.
    pub project: Option<String>,

    /// Topic pattern (regex). Required for search-based detection (opposing language across sessions).
    pub topic: Option<String>,

    /// Minimum confidence threshold (0.0-1.0). Default: 0.3.
    pub min_confidence: Option<f64>,

    /// Exclude subagent sessions. Default: true.
    pub no_subagents: Option<bool>,

    /// Only sessions modified after this date.
    pub since: Option<String>,

    /// Only sessions modified before this date.
    pub until: Option<String>,

    /// Session ID to exclude from search-based detection.
    pub exclude_session: Option<String>,

    /// Maximum conflicts to return. Default: 50.
    pub limit: Option<usize>,
}

/// A conflict pair entry in responses.
#[derive(Debug, Serialize)]
pub struct ConflictPairEntry {
    pub topic: String,
    pub detection_method: String,
    pub confidence: f64,
    pub earlier_timestamp: String,
    pub earlier_session_id: String,
    pub earlier_text: String,
    pub later_timestamp: String,
    pub later_session_id: String,
    pub later_text: String,
}

// ============================================================================
// Project Lessons
// ============================================================================

/// Request for cross-session lesson aggregation.
#[derive(Debug, Deserialize, ToolInput)]
pub struct GetProjectLessonsRequest {
    /// Project path filter (substring match). Required.
    pub project: String,

    /// Time period: "24h", "7d", "30d", "all". Default: "7d".
    pub period: Option<String>,

    /// Category filter: "errors", "corrections", "all". Default: "all".
    pub category: Option<String>,

    /// Maximum recurring patterns per category. Default: 30.
    pub limit: Option<usize>,

    /// Minimum occurrences to include a pattern. Default: 1.
    pub min_occurrences: Option<usize>,

    /// Exclude subagent sessions. Default: true.
    pub no_subagents: Option<bool>,
}

/// A recurring error pattern in responses.
#[derive(Debug, Serialize)]
pub struct RecurringErrorEntry {
    pub tool_name: String,
    pub error_pattern: String,
    pub count: usize,
    pub sessions: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_seen: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub example_resolution: Option<String>,
}

/// A recurring user correction in responses.
#[derive(Debug, Serialize)]
pub struct RecurringCorrectionEntry {
    pub pattern: String,
    pub count: usize,
    pub sessions: Vec<String>,
    pub examples: Vec<String>,
}

/// Response for get_project_lessons.
#[derive(Debug, Serialize)]
pub struct GetProjectLessonsResponse {
    pub project_path: String,
    pub period: String,
    pub sessions_analyzed: usize,
    pub total_errors: usize,
    pub total_corrections: usize,
    pub top_failure_modes: Vec<(String, usize)>,
    pub recurring_errors: Vec<RecurringErrorEntry>,
    pub recurring_corrections: Vec<RecurringCorrectionEntry>,
}

/// Response for detect_conflicts.
#[derive(Debug, Serialize)]
pub struct DetectConflictsResponse {
    pub total_conflicts: usize,
    pub conflicts: Vec<ConflictPairEntry>,
}

// ============================================================================
// Project Health
// ============================================================================

/// Request for project health dashboard.
#[derive(Debug, Deserialize, ToolInput)]
pub struct GetProjectHealthRequest {
    /// Project path filter (substring match). Required.
    pub project: String,

    /// Time period: "24h", "7d", "30d", "all". Default: "7d".
    pub period: Option<String>,

    /// Maximum hotspot/rework files to return. Default: 20.
    pub max_hotspots: Option<usize>,

    /// Exclude subagent sessions. Default: true.
    pub no_subagents: Option<bool>,
}

/// A hotspot file entry.
#[derive(Debug, Serialize)]
pub struct HotspotFileEntry {
    pub path: String,
    pub edit_count: usize,
    pub session_count: usize,
}

/// A rework file entry.
#[derive(Debug, Serialize)]
pub struct ReworkFileEntry {
    pub path: String,
    pub version_count: usize,
    pub session_count: usize,
}

/// Decision stability metrics.
#[derive(Debug, Serialize)]
pub struct DecisionChurnEntry {
    pub total_decisions: usize,
    pub confirmed_count: usize,
    pub superseded_count: usize,
    pub abandoned_count: usize,
    pub proposed_count: usize,
}

/// Per-session health stats.
#[derive(Debug, Serialize)]
pub struct SessionHealthEntry {
    pub session_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timestamp: Option<String>,
    pub error_count: usize,
    pub tool_count: usize,
}

/// Response for get_project_health.
#[derive(Debug, Serialize)]
pub struct GetProjectHealthResponse {
    pub project_path: String,
    pub period: String,
    pub sessions_analyzed: usize,
    pub total_errors: usize,
    pub total_tool_calls: usize,
    pub hotspot_files: Vec<HotspotFileEntry>,
    pub rework_files: Vec<ReworkFileEntry>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub decision_churn: Option<DecisionChurnEntry>,
    pub session_stats: Vec<SessionHealthEntry>,
}

// ============================================================================
// Event Context
// ============================================================================

/// Request for contextual zoom around a specific event.
#[derive(Debug, Deserialize, ToolInput)]
pub struct GetEventContextRequest {
    /// Session ID (full or prefix).
    pub session_id: String,

    /// Message UUID to find.
    pub message_id: Option<String>,

    /// Timestamp to find (ISO 8601 or relative). Finds closest match.
    pub timestamp: Option<String>,

    /// Number of turns before/after the target. Default: 2.
    pub context_window: Option<usize>,

    /// If true and the session is part of a chain, search across all chain members.
    pub chain_aware: Option<bool>,
}

/// A turn in the context window response.
#[derive(Debug, Serialize)]
pub struct ContextTurnEntry {
    pub index: usize,
    #[serde(rename = "type")]
    pub msg_type: String,
    pub uuid: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timestamp: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub tools: Vec<String>,
    pub had_errors: bool,
}

/// Response for get_event_context.
#[derive(Debug, Serialize)]
pub struct GetEventContextResponse {
    pub session_id: String,
    pub target_index: usize,
    pub target: ContextTurnEntry,
    pub before: Vec<ContextTurnEntry>,
    pub after: Vec<ContextTurnEntry>,
    pub related_files: Vec<String>,
    pub error_count: usize,
}

// ============================================================================
// Project Retrospective
// ============================================================================

/// Request for composite project retrospective.
#[derive(Debug, Deserialize, ToolInput)]
pub struct ProjectRetrospectiveRequest {
    /// Project path filter (substring match). Required.
    pub project: String,

    /// Time period: "24h", "7d", "30d", "all". Default: "7d".
    pub period: Option<String>,

    /// Maximum hotspot/rework files to include. Default: 10.
    pub max_files: Option<usize>,

    /// Maximum recurring errors to include. Default: 10.
    pub max_errors: Option<usize>,

    /// Maximum recurring corrections to include. Default: 5.
    pub max_corrections: Option<usize>,

    /// Minimum error occurrences to include a pattern. Default: 1.
    pub min_occurrences: Option<usize>,

    /// Exclude subagent sessions. Default: true.
    pub no_subagents: Option<bool>,
}

/// Summary statistics in retrospective response.
#[derive(Debug, Serialize)]
pub struct RetrospectiveSummaryEntry {
    pub sessions_analyzed: usize,
    pub total_errors: usize,
    pub total_tool_calls: usize,
    pub total_corrections: usize,
    pub error_rate: f64,
    pub top_failure_modes: Vec<FailureModeEntry>,
}

/// A failure mode (tool + count).
#[derive(Debug, Serialize)]
pub struct FailureModeEntry {
    pub tool: String,
    pub count: usize,
}

/// A decision entry in retrospective response.
#[derive(Debug, Serialize)]
pub struct ActiveDecisionEntry {
    pub id: usize,
    pub title: String,
    pub status: String,
    pub confidence: f64,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
}

/// Response for project_retrospective.
#[derive(Debug, Serialize)]
pub struct ProjectRetrospectiveResponse {
    pub project_path: String,
    pub period: String,
    pub summary: RetrospectiveSummaryEntry,
    pub hotspot_files: Vec<HotspotFileEntry>,
    pub rework_files: Vec<ReworkFileEntry>,
    pub recurring_errors: Vec<RecurringErrorEntry>,
    pub recurring_corrections: Vec<RecurringCorrectionEntry>,
    pub decisions: Vec<ActiveDecisionEntry>,
    pub session_stats: Vec<SessionHealthEntry>,
}

// ============================================================================
// Suggest Priorities
// ============================================================================

/// Request for priority suggestions.
#[derive(Debug, Deserialize, ToolInput)]
pub struct SuggestPrioritiesRequest {
    /// Project path filter (substring match). Required.
    pub project: String,

    /// Time period: "24h", "7d", "30d", "all". Default: "7d".
    pub period: Option<String>,

    /// Maximum priority items to return. Default: 10.
    pub max_priorities: Option<usize>,

    /// Exclude subagent sessions. Default: true.
    pub no_subagents: Option<bool>,
}

/// A source of evidence for a priority item.
#[derive(Debug, Serialize)]
pub struct PrioritySourceEntry {
    #[serde(rename = "type")]
    pub source_type: String,
    pub detail: String,
}

/// A ranked priority item.
#[derive(Debug, Serialize)]
pub struct PriorityItemEntry {
    pub rank: usize,
    pub category: String,
    pub summary: String,
    pub score: f64,
    pub sources: Vec<PrioritySourceEntry>,
}

/// Response for suggest_priorities.
#[derive(Debug, Serialize)]
pub struct SuggestPrioritiesResponse {
    pub project_path: String,
    pub period: String,
    pub sessions_analyzed: usize,
    pub total_errors: usize,
    pub open_goals: usize,
    pub proposed_decisions: usize,
    pub priorities: Vec<PriorityItemEntry>,
}

// ============================================================================
// File Evolution
// ============================================================================

/// Request for file evolution analysis.
#[derive(Debug, Deserialize, ToolInput)]
pub struct ExplainFileEvolutionRequest {
    /// File path pattern (substring match). Required.
    pub file_pattern: String,

    /// Project path filter (substring match). Required.
    pub project: String,

    /// Time period: "24h", "7d", "30d", "all". Default: "30d".
    pub period: Option<String>,

    /// Maximum change events to return per file. Default: 30.
    pub limit: Option<usize>,

    /// Include thinking blocks (decision rationale). Default: true.
    pub include_thinking: Option<bool>,

    /// Context window (turns before/after each modification). Default: 1.
    pub context_window: Option<usize>,

    /// Exclude subagent sessions. Default: true.
    pub no_subagents: Option<bool>,
}

/// A change event in file evolution response.
#[derive(Debug, Serialize)]
pub struct ChangeEventEntry {
    pub timestamp: String,
    pub session_id: String,
    pub message_id: String,
    pub version: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user_prompt: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub assistant_response: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thinking: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub tools_used: Vec<String>,
    pub had_errors: bool,
}

/// A single file's evolution in the response.
#[derive(Debug, Serialize)]
pub struct FileEvolutionEntry {
    pub file_path: String,
    pub total_changes: usize,
    pub sessions_involved: usize,
    pub changes: Vec<ChangeEventEntry>,
}

/// Response for explain_file_evolution.
#[derive(Debug, Serialize)]
pub struct ExplainFileEvolutionResponse {
    pub project_path: String,
    pub file_pattern: String,
    pub period: String,
    pub files: Vec<FileEvolutionEntry>,
}

/// Response for get_file_history.
#[derive(Debug, Serialize)]
pub struct GetFileHistoryResponse {
    pub path_query: String,
    pub total_files: usize,
    pub total_modifications: usize,
    pub returned: usize,
    pub modifications: Vec<FileModificationEntry>,
}

/// Response for tag_message.
#[derive(Debug, Serialize)]
pub struct TagMessageResponse {
    pub operation: String,
    pub project_path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub messages: Option<Vec<TaggedMessageEntry>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tags: Option<Vec<String>>,
}
