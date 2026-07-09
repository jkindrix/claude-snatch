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
    pub span: Option<String>,
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
    pub span: Option<String>,
    pub compaction_count: usize,
    pub primary_model: Option<String>,
    pub total_tokens: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_tokens: u64,
    pub cache_creation_tokens: u64,
    pub total_processed_tokens: u64,
    /// Canonical message count: human user prompts plus distinct assistant
    /// turns (deduped by message.id) on the main thread. Matches
    /// get_session_messages.total_messages. Note this is main-thread-scoped, so
    /// it does not equal user_messages + assistant_messages below (those are
    /// whole-session, including sidechains and tool-result carriers).
    pub messages: usize,
    /// Whole-session user-message nodes, including tool-result carriers and
    /// sidechains (broader scope than `messages`).
    pub user_messages: usize,
    /// Whole-session distinct assistant turns, including sidechains (broader
    /// scope than `messages`).
    pub assistant_messages: usize,
    pub tool_invocations: usize,
    pub cache_hit_rate: f64,
    /// Estimated cost in USD. Partial (priced models only) when
    /// `unpriced_models` is non-empty; null when no model could be priced.
    pub estimated_cost: Option<f64>,
    /// Models in this session with no known rate, excluded from
    /// `estimated_cost`. Omitted when empty.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub unpriced_models: Vec<String>,
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

    /// Detail level: "overview" (prompt boundaries only — typed user prompts
    /// plus queued mid-turn steering prompts — truncated),
    /// "conversation" (user prompts + assistant text, skips tool-only turns),
    /// "standard" (user + assistant text, tool names only),
    /// "full" (includes tool call details).
    /// Default: "standard".
    pub detail: Option<String>,

    /// Message type filter: "user", "assistant", "system", "all".
    /// Default: "all".
    pub message_type: Option<String>,

    /// Maximum number of messages to return. Default: 50. 0 = unlimited.
    pub limit: Option<usize>,

    /// Offset for pagination (skip first N messages). Default: 0.
    pub offset: Option<usize>,

    /// If true, return messages in reverse chronological order.
    /// Default: false.
    pub reverse: Option<bool>,

    /// If true, include thinking/reasoning block content in assistant messages.
    /// Thinking text is present only in sessions from old Claude Code
    /// (~2.1.4x and earlier); recent versions persist it empty, and the
    /// response carries a thinking_note when that is the case. Default: false.
    pub include_thinking: Option<bool>,

    /// If the session is part of a resume chain, return messages across all
    /// member files. Default: true (set false to restrict to the single file).
    pub chain_aware: Option<bool>,

    /// Only include messages after this timestamp (ISO 8601 or relative like "2h", "30m").
    /// Enables contextual zoom: find an event timestamp from another tool, then
    /// retrieve messages around it.
    pub after_timestamp: Option<String>,

    /// Only include messages before this timestamp (ISO 8601 or relative like "2h", "30m").
    pub before_timestamp: Option<String>,

    /// If true, inline each spawned subagent's full transcript under its Agent/Task
    /// call (only at detail="full"). Default: false — a pointer (subagent_session_id)
    /// plus a result preview is attached instead, and the transcript is fetched on
    /// demand by querying the subagent id.
    pub include_subagent_transcripts: Option<bool>,

    /// Restrict to prompt-boundary chunk(s): a zero-based index like "4" or an
    /// inclusive range like "2-5". Chunk N is prompt N — a typed user prompt or
    /// a queued mid-turn steering prompt — plus everything it produced (tool
    /// traffic, responses, late async results), up to the next prompt. The
    /// prompts listed by detail="overview" use the same indices, so overview →
    /// pick index → chunk retrieval composes. The response carries chunk_info
    /// describing the selection, including each chunk's prompt_source.
    pub chunk: Option<String>,

    /// If true, only return entries carrying failed tool results. Pairs with
    /// chunk + detail="standard"/"full" to drill into a chunk's errors
    /// (overview/conversation levels filter these entries out).
    pub errors_only: Option<bool>,

    /// Override content truncation length in characters (default is set by
    /// detail level: 200 overview, 500 conversation/standard, 1000 full).
    /// Use small values to skim cheaply, large ones to read in full.
    pub max_text_len: Option<usize>,
}

/// A message in the session messages response.
#[derive(Debug, Clone, Serialize)]
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
#[derive(Debug, Clone, Serialize)]
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
    /// Remaining tool-input fields not surfaced as a named field above (e.g.
    /// Edit `old_string`/`new_string`, Write `content`, TodoWrite `todos`),
    /// each truncated. Lets a reader see what the call did, not just that it ran.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input_summary: Option<std::collections::BTreeMap<String, String>>,
    /// For Agent/Task calls: the spawned subagent's session id (`agent-<hash>`),
    /// when it could be matched to this call. Query it for the full transcript.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subagent_session_id: Option<String>,
    /// Preview of the subagent's final assistant message (its result), truncated.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subagent_result_preview: Option<String>,
    /// Full subagent transcript, present only when include_subagent_transcripts=true.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subagent_transcript: Option<Vec<MessageEntry>>,
    /// Whether the matched tool result was an error (absent when success).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub had_error: Option<bool>,
    /// Truncated preview of the tool result's output (file contents, stdout,
    /// matches). For Agent/Task calls a richer subagent_result_preview is used
    /// instead, so this is absent when that is present.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result_preview: Option<String>,
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
    /// Subagents present on disk but not confidently joined to a spawning
    /// Agent/Task call (e.g. several key-less subagents in one turn). Surfaced
    /// so they are never silently dropped; the matched ones are attached inline
    /// to their spawning message instead. Only populated at detail="full".
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub unmatched_subagents: Vec<UnmatchedSubagent>,
    /// Set when duplicate-UUID entries with *differing* content were dropped
    /// during reconstruction (a real collision, not benign chain overlap), so
    /// the loss is never silent on this path.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duplicate_notice: Option<String>,
    /// Set when include_thinking was requested but every thinking block in the
    /// session is empty (recent Claude Code versions persist only the encrypted
    /// signature), so the absence of thinking output is never silent.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thinking_note: Option<String>,
    /// Describes the chunk selection when the chunk parameter was used.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub chunk_info: Option<ChunkInfo>,
}

/// Chunk-selection metadata for get_session_messages.
#[derive(Debug, Serialize)]
pub struct ChunkInfo {
    /// Total prompt-boundary chunks in the session.
    pub total_chunks: usize,
    /// First selected chunk index (zero-based, inclusive).
    pub start: usize,
    /// Last selected chunk index (zero-based, inclusive).
    pub end: usize,
    /// The selected chunks.
    pub chunks: Vec<ChunkSummary>,
}

/// Summary of one selected chunk.
#[derive(Debug, Serialize)]
pub struct ChunkSummary {
    pub index: usize,
    /// Opening human prompt, truncated.
    pub prompt: String,
    /// How the prompt reached the conversation: "user" (typed at a turn
    /// boundary) or "queued" (mid-turn steering via a queued_command
    /// attachment).
    pub prompt_source: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub start_ts: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub end_ts: Option<String>,
    /// Member entries (main-thread + attached; branch entries excluded).
    pub entries: usize,
    /// Off-main-thread members (late async results, progress leaves).
    pub attached: usize,
    pub tool_calls: usize,
    /// Failed tool results (is_error) among member entries — where to aim
    /// detail="full" drill-downs when auditing.
    pub errors: usize,
    /// Abandoned branches (e.g. rewind forks) that forked from this chunk.
    /// Their entries are not in the message list; fetch by uuid if needed.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub branches: Vec<ChunkBranchSummary>,
}

/// An abandoned branch attached to a chunk.
#[derive(Debug, Serialize)]
pub struct ChunkBranchSummary {
    pub root_uuid: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prompt: Option<String>,
    pub entries: usize,
}

/// A subagent present on disk but not joinable to a specific spawn call.
#[derive(Debug, Serialize)]
pub struct UnmatchedSubagent {
    pub session_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message_count: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result_preview: Option<String>,
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

    /// If the session is part of a resume chain, build the timeline across all
    /// member files. Default: true (set false to restrict to the single file).
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
    pub span: Option<String>,
    pub total_turns: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub git_branch: Option<String>,
    pub timeline: Vec<TimelineTurn>,
    pub compaction_events: Vec<CompactionEvent>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub error_events: Vec<ErrorEvent>,
    /// Set when duplicate-UUID entries with *differing* content were dropped
    /// during reconstruction (a real collision, not benign chain overlap), so
    /// the loss is never silent on this path.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duplicate_notice: Option<String>,
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
    pub span: Option<String>,
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

    /// When `session_id` is set and that session is part of a resume chain,
    /// search the whole chain. Default: true; set false to restrict to the
    /// single file.
    pub chain_aware: Option<bool>,
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
    /// Set when scope="thinking" scanned only empty thinking blocks (recent
    /// Claude Code versions persist only the encrypted signature), so a
    /// zero-match result is explained rather than silent.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
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

    /// Restrict to prompt-boundary chunk(s): "4" or "2-5" (same indices as
    /// get_session_messages). Ground-truth view of what actually ran in a
    /// chunk, without the narrative.
    pub chunk: Option<String>,

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
    /// Truncated preview of a successful tool result's output (absent on error).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result_preview: Option<String>,
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
    /// Set when thinking blocks exist but are all empty (recent Claude Code
    /// versions persist only the encrypted signature), explaining why
    /// `thinking_keywords` is empty.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thinking_note: Option<String>,
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

    /// Concrete future moment this note must precede (optional, for "add").
    /// Structured join key for debrief sweeps and resurfacing.
    pub resurface_when: Option<String>,

    /// Condition/version after which this note is stale (optional, for "add").
    pub expires_when: Option<String>,
}

/// A note entry in responses.
#[derive(Debug, Serialize)]
pub struct NoteEntry {
    pub id: u64,
    pub text: String,
    pub created_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resurface_when: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expires_when: Option<String>,
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

    /// Concrete future moment this decision must precede (for "add"/"update").
    /// Structured join key for debrief sweeps and resurfacing.
    pub resurface_when: Option<String>,

    /// Condition/version after which this decision is stale (for "add"/"update").
    pub expires_when: Option<String>,
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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resurface_when: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expires_when: Option<String>,
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

    /// Include thinking/reasoning blocks in search and output (text present
    /// only in old-Claude-Code sessions, ~2.1.4x and earlier). Default: false.
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
// Active Monitor
// ============================================================================

/// Request for proactive cross-session insights.
#[derive(Debug, Deserialize, ToolInput)]
pub struct MonitorProjectRequest {
    /// Project path filter (substring match). Required.
    pub project: String,

    /// Time period: "24h", "7d", "30d", "all". Default: "7d".
    pub period: Option<String>,

    /// Minimum occurrences for an error to count as recurring. Default: 3.
    pub min_occurrences: Option<usize>,

    /// Exclude subagent sessions. Default: true.
    pub no_subagents: Option<bool>,

    /// Maximum insights to return. Default: 10.
    pub limit: Option<usize>,
}

/// A single ranked insight.
#[derive(Debug, Serialize)]
pub struct MonitorInsightEntry {
    pub kind: String,
    pub title: String,
    pub evidence: String,
    pub severity: u32,
    pub fingerprint: String,
}

/// Response for monitor_project.
#[derive(Debug, Serialize)]
pub struct MonitorProjectResponse {
    pub project_path: String,
    pub period: String,
    pub count: usize,
    pub insights: Vec<MonitorInsightEntry>,
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
    pub tool_failure_count: usize,
    pub tool_count: usize,
}

/// Response for get_project_health.
#[derive(Debug, Serialize)]
pub struct GetProjectHealthResponse {
    pub project_path: String,
    pub period: String,
    pub sessions_analyzed: usize,
    pub total_tool_failures: usize,
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

    /// If the session is part of a resume chain, resolve context across all
    /// member files. Default: true (set false to restrict to the single file).
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
    pub total_tool_failures: usize,
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

    /// Include thinking blocks (decision rationale; text present only in
    /// old-Claude-Code sessions, ~2.1.4x and earlier). Default: true.
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
