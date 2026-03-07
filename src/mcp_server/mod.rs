//! MCP (Model Context Protocol) server implementation.
//!
//! Exposes claude-snatch functionality as MCP tools for AI model integration.
//!
//! # Tools Provided
//!
//! - `list_sessions` - List Claude Code sessions
//! - `get_session_info` - Get detailed session information
//! - `get_stats` - Get usage statistics
//! - `get_session_messages` - Read conversation messages at different detail levels
//! - `get_session_timeline` - Get turn-by-turn narrative of a session
//! - `get_project_history` - Cross-session overview for a project
//! - `search_sessions` - Regex search across sessions (supports thinking blocks)
//! - `get_tool_calls` - Extract tool invocations with summaries
//! - `get_session_lessons` - Extract error→fix pairs and user corrections
//! - `manage_goals` - Persistent goal tracking across sessions and compactions
//! - `get_session_digest` - Compact session summary for orientation after compaction
//! - `manage_notes` - Tactical session notes that survive compaction
//! - `manage_decisions` - Persistent decision registry across sessions

#![cfg(feature = "mcp")]

pub mod helpers;
pub mod types;

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use mcpkit::prelude::*;
use mcpkit::transport::stdio::StdioTransport;

use crate::analytics::{AnalyticsSummary, SessionAnalytics};
use crate::discovery::ClaudeDirectory;
use crate::model::message::LogEntry;
use crate::reconstruction::Conversation;

use helpers::*;
use types::*;

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
    pub(crate) fn get_claude_dir(&self) -> Result<ClaudeDirectory, String> {
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
    // ========================================================================
    // Existing Tools
    // ========================================================================

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

        if let Some(ref project) = request.project {
            sessions.retain(|s| s.project_path().contains(project));
        }

        if !request.include_subagents.unwrap_or(false) {
            sessions.retain(|s| !s.is_subagent());
        }

        let limit = request.limit.unwrap_or(50);
        sessions.truncate(limit);

        let summaries: Vec<SessionSummary> = sessions
            .iter()
            .map(|s| {
                let (duration, compaction_count) = s.quick_metadata_cached()
                    .map(|m| (m.duration_human(), m.compaction_count))
                    .unwrap_or((None, 0));
                SessionSummary {
                    session_id: s.session_id().to_string(),
                    project_path: s.project_path().to_string(),
                    is_subagent: s.is_subagent(),
                    parent_session_id: s.parent_session_id().map(String::from),
                    modified_time: Some(s.modified_datetime().to_rfc3339()),
                    is_active: s.is_active().unwrap_or(false),
                    duration,
                    compaction_count,
                }
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

        let compaction_count = session.quick_metadata_cached()
            .map(|m| m.compaction_count)
            .unwrap_or(0);

        let info = SessionInfoResponse {
            session_id: session.session_id().to_string(),
            project_path: session.project_path().to_string(),
            is_subagent: session.is_subagent(),
            parent_session_id: session.parent_session_id().map(String::from),
            is_active: session.is_active().unwrap_or(false),
            modified_time: Some(session.modified_datetime().to_rfc3339()),
            duration: analytics.duration_string(),
            compaction_count,
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
        } else {
            let sessions = match claude_dir.all_sessions() {
                Ok(s) => s,
                Err(e) => return ToolOutput::error(format!("Failed to list sessions: {e}")),
            };

            let (scope, target_sessions): (String, Vec<_>) =
                if let Some(project) = request.project {
                    let filtered: Vec<_> = sessions
                        .iter()
                        .filter(|s| s.project_path().contains(&project))
                        .collect();
                    (project, filtered)
                } else {
                    ("global".to_string(), sessions.iter().collect())
                };

            let summaries: Vec<_> = target_sessions
                .iter()
                .filter_map(|session| {
                    let entries = session.parse_with_options(self.max_file_size).ok()?;
                    let conversation = Conversation::from_entries(entries).ok()?;
                    let analytics = SessionAnalytics::from_conversation(&conversation);
                    Some(analytics.summary_report())
                })
                .collect();

            let agg = AnalyticsSummary::aggregate(&summaries);

            StatsResponse {
                scope,
                sessions: Some(target_sessions.len()),
                total_tokens: agg.total_tokens,
                input_tokens: agg.input_tokens,
                output_tokens: agg.output_tokens,
                messages: agg.total_messages,
                tool_invocations: agg.tool_invocations,
                estimated_cost: agg.estimated_cost,
            }
        };

        match ToolOutput::json(&response) {
            Ok(output) => output,
            Err(e) => ToolOutput::error(format!("JSON serialization error: {e}")),
        }
    }

    // ========================================================================
    // New Tool: get_session_messages
    // ========================================================================

    /// Read conversation messages from a session at different detail levels.
    /// Use detail="overview" for user prompts only, "standard" for user+assistant
    /// text with tool names, or "full" for tool call details.
    #[tool(description = "Read conversation messages from a session. Use detail='overview' for prompts only, 'conversation' for user+assistant text (skipping tool-only turns), 'standard' for user+assistant text, 'full' for tool details. Set include_thinking=true to recover reasoning/decision rationale (always lost in compaction). Supports pagination with offset/limit.")]
    async fn get_session_messages(&self, request: GetSessionMessagesRequest) -> ToolOutput {
        let resolved = match resolve_session(self, &request.session_id) {
            Ok(r) => r,
            Err(e) => return e,
        };

        let detail = request.detail.as_deref().unwrap_or("standard");
        let msg_type_filter = request.message_type.as_deref().unwrap_or("all");
        let limit = request.limit.unwrap_or(50);
        let offset = request.offset.unwrap_or(0);
        let reverse = request.reverse.unwrap_or(false);
        let include_thinking = request.include_thinking.unwrap_or(false);
        let thinking_max_len = match detail {
            "overview" => 500,
            "conversation" | "standard" => 1000,
            _ => 2000,
        };

        let mut entries: Vec<&LogEntry> = resolved.conversation.main_thread_entries();

        // Filter by message type
        match msg_type_filter {
            "user" => entries.retain(|e| is_human_prompt(e)),
            "assistant" => entries.retain(|e| matches!(e, LogEntry::Assistant(_))),
            "system" => entries.retain(|e| matches!(e, LogEntry::System(_))),
            _ => {} // "all" — keep everything
        }

        // Pre-filter entries based on detail level
        match detail {
            "overview" => {
                // Only human-authored prompts (excludes system noise)
                entries.retain(|e| is_human_prompt(e));
            }
            "conversation" => {
                // Human prompts + assistant messages with text content
                // Skips tool-only assistant turns, system messages, and noise
                entries.retain(|e| match e {
                    LogEntry::User(_) => is_human_prompt(e),
                    LogEntry::Assistant(_) => extract_assistant_summary(e, 1).is_some(),
                    _ => false,
                });
            }
            _ => {} // standard/full: keep everything
        }

        let total_messages = entries.len();

        // Build (original_index, entry) pairs so indices survive reordering
        let mut indexed: Vec<(usize, &LogEntry)> =
            entries.into_iter().enumerate().collect();

        if reverse {
            indexed.reverse();
        }

        // Apply pagination
        let paginated: Vec<(usize, &LogEntry)> =
            indexed.into_iter().skip(offset).take(limit).collect();

        let truncate_len = match detail {
            "overview" => 200,
            "conversation" => 500,
            "standard" => 500,
            _ => 1000,
        };

        let messages: Vec<MessageEntry> = paginated
            .iter()
            .filter_map(|(orig_idx, entry)| {
                let msg_type = entry.message_type().to_string();
                let timestamp = entry.timestamp().map(|t| t.to_rfc3339());
                let git_branch = entry.git_branch().map(String::from);

                match detail {
                    "overview" => {
                        let content = extract_user_prompt_text(entry)
                            .map(|t| truncate_text(&t, truncate_len));
                        Some(MessageEntry {
                            index: *orig_idx,
                            msg_type,
                            timestamp,
                            content,
                            git_branch,
                            model: None,
                            tool_calls: None,
                            tool_details: None,
                            has_thinking: None,
                            thinking_preview: None,
                        })
                    }
                    "conversation" => {
                        // User prompts + assistant text, no tool details
                        let content = match entry {
                            LogEntry::User(_) => extract_user_prompt_text(entry)
                                .map(|t| truncate_text(&t, truncate_len)),
                            LogEntry::Assistant(_) => extract_assistant_summary(entry, truncate_len),
                            _ => None,
                        };
                        let thinking = if include_thinking {
                            extract_thinking_text(entry, thinking_max_len)
                        } else {
                            None
                        };
                        Some(MessageEntry {
                            index: *orig_idx,
                            msg_type,
                            timestamp,
                            content,
                            git_branch,
                            model: get_model(entry),
                            tool_calls: None,
                            tool_details: None,
                            has_thinking: if has_thinking(entry) { Some(true) } else { None },
                            thinking_preview: thinking,
                        })
                    }
                    "standard" => {
                        let content = match entry {
                            LogEntry::User(_) => extract_user_prompt_text(entry)
                                .map(|t| truncate_text(&t, truncate_len)),
                            LogEntry::Assistant(_) => extract_assistant_summary(entry, truncate_len),
                            LogEntry::System(sys) => sys.content.clone(),
                            _ => None,
                        };
                        let tool_names = extract_tool_names(entry);
                        let thinking = if include_thinking {
                            extract_thinking_text(entry, thinking_max_len)
                        } else {
                            None
                        };
                        Some(MessageEntry {
                            index: *orig_idx,
                            msg_type,
                            timestamp,
                            content,
                            git_branch,
                            model: get_model(entry),
                            tool_calls: if tool_names.is_empty() {
                                None
                            } else {
                                Some(tool_names)
                            },
                            tool_details: None,
                            has_thinking: if has_thinking(entry) {
                                Some(true)
                            } else {
                                None
                            },
                            thinking_preview: thinking,
                        })
                    }
                    "full" | _ => {
                        let content = match entry {
                            LogEntry::User(_) => extract_user_prompt_text(entry)
                                .map(|t| truncate_text(&t, truncate_len)),
                            LogEntry::Assistant(_) => extract_assistant_summary(entry, truncate_len),
                            LogEntry::System(sys) => sys.content.clone(),
                            _ => None,
                        };
                        let tool_names = extract_tool_names(entry);
                        let tool_details: Vec<ToolDetail> = if let LogEntry::Assistant(a) = entry {
                            a.message
                                .tool_uses()
                                .iter()
                                .map(|t| {
                                    let summary = extract_tool_input_summary(&t.name, &t.input);
                                    ToolDetail {
                                        tool_name: t.name.clone(),
                                        file_path: summary.get("file_path").cloned(),
                                        command: summary.get("command").cloned(),
                                        pattern: summary.get("pattern").cloned(),
                                    }
                                })
                                .collect()
                        } else {
                            vec![]
                        };
                        let thinking = if include_thinking {
                            extract_thinking_text(entry, thinking_max_len)
                        } else {
                            None
                        };
                        Some(MessageEntry {
                            index: *orig_idx,
                            msg_type,
                            timestamp,
                            content,
                            git_branch,
                            model: get_model(entry),
                            tool_calls: if tool_names.is_empty() {
                                None
                            } else {
                                Some(tool_names)
                            },
                            tool_details: if tool_details.is_empty() {
                                None
                            } else {
                                Some(tool_details)
                            },
                            has_thinking: if has_thinking(entry) {
                                Some(true)
                            } else {
                                None
                            },
                            thinking_preview: thinking,
                        })
                    }
                }
            })
            .collect();

        let returned = messages.len();
        let response = SessionMessagesResponse {
            session_id: resolved.session_id,
            project_path: resolved.project_path,
            total_messages,
            returned,
            offset,
            messages,
        };

        match ToolOutput::json(&response) {
            Ok(output) => output,
            Err(e) => ToolOutput::error(format!("JSON serialization error: {e}")),
        }
    }

    // ========================================================================
    // New Tool: get_session_timeline
    // ========================================================================

    /// Get a turn-by-turn narrative timeline of a session showing what was asked,
    /// what Claude did, and what files were touched.
    #[tool(description = "Get a turn-by-turn narrative timeline of a session. Each turn shows the user prompt, assistant summary, tools used, and files touched. Also surfaces compaction events.")]
    async fn get_session_timeline(&self, request: GetSessionTimelineRequest) -> ToolOutput {
        let resolved = match resolve_session(self, &request.session_id) {
            Ok(r) => r,
            Err(e) => return e,
        };

        let limit = request.limit.unwrap_or(30);
        let turns = resolved.conversation.turns();
        let total_turns = turns.len();

        // Detect compaction events from main thread
        let main_entries = resolved.conversation.main_thread_entries();
        let main_refs: Vec<&LogEntry> = main_entries.iter().copied().collect();
        let compaction_events: Vec<CompactionEvent> = find_compaction_events(&main_refs)
            .into_iter()
            .map(|(ts, summary)| CompactionEvent {
                timestamp: ts,
                summary,
            })
            .collect();

        // Get session time bounds and git branch
        let start_time = resolved.analytics.start_time.map(|t| t.to_rfc3339());
        let end_time = resolved.analytics.end_time.map(|t| t.to_rfc3339());
        let duration = resolved.analytics.duration_string();
        let git_branch = main_entries
            .iter()
            .find_map(|e| e.git_branch().map(String::from));

        // Build timeline using shared analysis module
        let timeline_opts = crate::analysis::timeline::TimelineOptions {
            limit,
            prompt_max_len: 200,
            summary_max_len: 200,
        };
        let analysis_timeline = crate::analysis::timeline::build_timeline(&turns, &timeline_opts);

        // Map analysis types to MCP response types
        let timeline: Vec<TimelineTurn> = analysis_timeline
            .into_iter()
            .map(|t| TimelineTurn {
                index: t.index,
                timestamp: t.timestamp,
                user_prompt: t.user_prompt,
                assistant_summary: t.assistant_summary,
                tools_used: t.tools_used,
                files_touched: t.files_touched,
                had_errors: t.had_errors,
            })
            .collect();

        let response = SessionTimelineResponse {
            session_id: resolved.session_id,
            project_path: resolved.project_path,
            start_time,
            end_time,
            duration,
            total_turns,
            git_branch,
            timeline,
            compaction_events,
        };

        match ToolOutput::json(&response) {
            Ok(output) => output,
            Err(e) => ToolOutput::error(format!("JSON serialization error: {e}")),
        }
    }

    // ========================================================================
    // New Tool: get_project_history
    // ========================================================================

    /// Get a cross-session overview for a project, showing what was worked on
    /// across sessions with key prompts, tools used, and files touched.
    #[tool(description = "Get cross-session history for a project. Shows sessions with key prompts, tools, files, and costs. Filter by period (24h/7d/30d/all). Use to understand what has been worked on across sessions.")]
    async fn get_project_history(&self, request: GetProjectHistoryRequest) -> ToolOutput {
        let claude_dir = match get_claude_dir(self) {
            Ok(dir) => dir,
            Err(e) => return e,
        };

        let period = request.period.as_deref().unwrap_or("7d");
        let limit = request.limit.unwrap_or(20);
        let include_summaries = request.include_summaries.unwrap_or(true);

        let cutoff = match period_cutoff(period) {
            Ok(c) => c,
            Err(e) => return ToolOutput::error(e),
        };

        let mut sessions = match claude_dir.all_sessions() {
            Ok(s) => s,
            Err(e) => return ToolOutput::error(format!("Failed to list sessions: {e}")),
        };

        // Filter by project
        sessions.retain(|s| s.project_path().contains(&request.project));

        // Filter subagents
        sessions.retain(|s| !s.is_subagent());

        // Filter by time
        if let Some(cutoff_time) = cutoff {
            sessions.retain(|s| s.modified_datetime() >= cutoff_time);
        }

        // Limit
        sessions.truncate(limit);

        let mut project_path = String::new();
        let mut agg_tokens = 0u64;
        let mut agg_cost = 0.0f64;
        let mut agg_prompts = 0usize;
        let mut agg_branches = HashSet::new();

        let mut session_entries = Vec::new();

        for session in &sessions {
            if project_path.is_empty() {
                project_path = session.project_path().to_string();
            }

            let entries = match session.parse_with_options(self.max_file_size) {
                Ok(e) => e,
                Err(_) => continue,
            };

            let conversation = match Conversation::from_entries(entries) {
                Ok(c) => c,
                Err(_) => continue,
            };

            let analytics = SessionAnalytics::from_conversation(&conversation);
            let summary_report = analytics.summary_report();

            let main_entries = conversation.main_thread_entries();
            let main_refs: Vec<&LogEntry> = main_entries.iter().copied().collect();

            // Extract user prompts (excluding system noise)
            let mut prompts: Vec<String> = Vec::new();
            let mut prompt_count = 0usize;
            for entry in &main_refs {
                if is_human_prompt(entry) {
                    prompt_count += 1;
                    if include_summaries && prompts.len() < 3 {
                        if let Some(text) = extract_user_prompt_text(entry) {
                            if text.len() > 20 {
                                prompts.push(truncate_text(&text, 150));
                            }
                        }
                    }
                }
            }

            // Extract git branch
            let branch = main_refs
                .iter()
                .find_map(|e| e.git_branch().map(String::from));
            if let Some(ref b) = branch {
                agg_branches.insert(b.clone());
            }

            // Extract files
            let files = extract_files_from_tools(&main_refs);

            // Tool counts
            let mut tool_counts: HashMap<String, usize> = HashMap::new();
            for entry in &main_refs {
                for name in extract_tool_names(entry) {
                    *tool_counts.entry(name).or_default() += 1;
                }
            }

            let first_prompt = prompts.first().cloned();
            let start_time = analytics.start_time.map(|t| t.to_rfc3339());
            let end_time = analytics.end_time.map(|t| t.to_rfc3339());
            let duration = analytics.duration_string();
            let tokens = summary_report.total_tokens;
            let cost = summary_report.estimated_cost;

            agg_tokens += tokens;
            agg_cost += cost.unwrap_or(0.0);
            agg_prompts += prompt_count;

            let compaction_count = session.quick_metadata_cached()
                .map(|m| m.compaction_count)
                .unwrap_or(0);

            session_entries.push(ProjectSessionEntry {
                session_id: session.session_id().to_string(),
                is_subagent: session.is_subagent(),
                parent_session_id: session.parent_session_id().map(String::from),
                start_time,
                end_time,
                duration,
                compaction_count,
                git_branch: branch,
                user_prompt_count: prompt_count,
                first_prompt,
                key_prompts: prompts,
                tools_summary: tool_counts,
                files_touched: files.into_iter().take(10).collect(),
                estimated_cost: cost,
                total_tokens: tokens,
            });
        }

        // Filter out empty sessions (no prompts and no tokens)
        session_entries.retain(|s| s.user_prompt_count > 0 || s.total_tokens > 0);
        let sessions_found = session_entries.len();

        let mut branches: Vec<String> = agg_branches.into_iter().collect();
        branches.sort();

        let response = ProjectHistoryResponse {
            project_path,
            period: period.to_string(),
            sessions_found,
            sessions: session_entries,
            aggregate: ProjectAggregate {
                total_sessions: sessions_found,
                total_tokens: agg_tokens,
                total_cost: agg_cost,
                total_prompts: agg_prompts,
                active_branches: branches,
            },
        };

        match ToolOutput::json(&response) {
            Ok(output) => output,
            Err(e) => ToolOutput::error(format!("JSON serialization error: {e}")),
        }
    }

    // ========================================================================
    // New Tool: search_sessions
    // ========================================================================

    /// Search across sessions for text patterns using regex.
    #[tool(description = "Search across sessions for text patterns (regex). Filter by project, session, scope (text/tools/thinking/all). Use scope='thinking' to search reasoning blocks (decision rationale, evidence chains). Returns matching text with context.")]
    async fn search_sessions(&self, request: SearchSessionsRequest) -> ToolOutput {
        let claude_dir = match get_claude_dir(self) {
            Ok(dir) => dir,
            Err(e) => return e,
        };

        let scope = request.scope.as_deref().unwrap_or("text");
        let ignore_case = request.ignore_case.unwrap_or(true);
        let limit = request.limit.unwrap_or(20);

        let regex = match regex::RegexBuilder::new(&request.pattern)
            .case_insensitive(ignore_case)
            .build()
        {
            Ok(r) => r,
            Err(e) => return ToolOutput::error(format!("Invalid regex pattern: {e}")),
        };

        // Determine which sessions to search
        let sessions = if let Some(ref session_id) = request.session_id {
            match claude_dir.find_session(session_id) {
                Ok(Some(s)) => vec![s],
                Ok(None) => {
                    return ToolOutput::error(format!("Session not found: {session_id}"))
                }
                Err(e) => return ToolOutput::error(format!("Failed to find session: {e}")),
            }
        } else {
            let mut all = match claude_dir.all_sessions() {
                Ok(s) => s,
                Err(e) => return ToolOutput::error(format!("Failed to list sessions: {e}")),
            };
            if let Some(ref project) = request.project {
                all.retain(|s| s.project_path().contains(project));
            }
            all.retain(|s| !s.is_subagent());
            all
        };

        let mut results = Vec::new();

        for session in &sessions {
            let entries = match session.parse_with_options(self.max_file_size) {
                Ok(e) => e,
                Err(_) => continue,
            };

            let conversation = match Conversation::from_entries(entries) {
                Ok(c) => c,
                Err(_) => continue,
            };

            // Search ALL entries (not just main thread) so branches,
            // sidechains, and agent sub-conversations are included.
            for entry in conversation.chronological_entries() {
                let matches = search_entry_text(entry, &regex, scope, 100);
                for (matched, context) in matches {
                    results.push(SearchMatch {
                        session_id: session.session_id().to_string(),
                        project_path: session.project_path().to_string(),
                        timestamp: entry.timestamp().map(|t| t.to_rfc3339()),
                        message_type: entry.message_type().to_string(),
                        matched_text: truncate_text(&matched, 200),
                        context: truncate_text(&context, 300),
                    });
                }
            }
        }

        let total = results.len();
        results.truncate(limit);
        let returned = results.len();
        let response = SearchSessionsResponse {
            pattern: request.pattern,
            total_matches: total,
            returned,
            results,
        };

        match ToolOutput::json(&response) {
            Ok(output) => output,
            Err(e) => ToolOutput::error(format!("JSON serialization error: {e}")),
        }
    }

    // ========================================================================
    // New Tool: get_tool_calls
    // ========================================================================

    /// Extract tool invocations from a session with input summaries and error states.
    #[tool(description = "Extract tool invocations from a session. Filter by tool name or errors. Returns tool names, input summaries (file paths, commands), and error states. Use to understand what was built or changed.")]
    async fn get_tool_calls(&self, request: GetToolCallsRequest) -> ToolOutput {
        let resolved = match resolve_session(self, &request.session_id) {
            Ok(r) => r,
            Err(e) => return e,
        };

        let limit = request.limit.unwrap_or(100);
        let offset = request.offset.unwrap_or(0);
        let errors_only = request.errors_only.unwrap_or(false);

        let tool_filter: Option<HashSet<String>> = request.tool_filter.map(|f| {
            f.split(',').map(|s| s.trim().to_string()).collect()
        });

        let main_entries = resolved.conversation.main_thread_entries();

        // Build list of tool calls with their results
        struct ToolCallWithResult {
            timestamp: Option<String>,
            tool_name: String,
            input: serde_json::Value,
            had_error: bool,
            error_text: Option<String>,
        }

        let mut all_calls: Vec<ToolCallWithResult> = Vec::new();
        let mut tool_result_map: HashMap<String, (bool, Option<String>)> = HashMap::new();

        // First pass: collect tool results from user messages
        for entry in &main_entries {
            if let LogEntry::User(user) = entry {
                for result in user.message.tool_results() {
                    let is_err = result.is_error == Some(true);
                    let err_text = if is_err {
                        extract_error_preview(result, 300)
                    } else {
                        None
                    };
                    tool_result_map.insert(result.tool_use_id.clone(), (is_err, err_text));
                }
            }
        }

        // Second pass: collect tool uses from assistant messages
        for entry in &main_entries {
            if let LogEntry::Assistant(assistant) = entry {
                let timestamp = entry.timestamp().map(|t| t.to_rfc3339());
                for tool_use in assistant.message.tool_uses() {
                    let (had_error, error_text) = tool_result_map
                        .get(&tool_use.id)
                        .cloned()
                        .unwrap_or((false, None));

                    all_calls.push(ToolCallWithResult {
                        timestamp: timestamp.clone(),
                        tool_name: tool_use.name.clone(),
                        input: tool_use.input.clone(),
                        had_error,
                        error_text,
                    });
                }
            }
        }

        // Apply filters
        if let Some(ref filter) = tool_filter {
            all_calls.retain(|c| filter.contains(&c.tool_name));
        }
        if errors_only {
            all_calls.retain(|c| c.had_error);
        }

        let total_tool_calls = all_calls.len();

        // Build summary before pagination
        let mut by_tool: HashMap<String, usize> = HashMap::new();
        let mut files_written = HashSet::new();
        let mut files_edited = HashSet::new();
        let mut error_count = 0usize;

        for call in &all_calls {
            *by_tool.entry(call.tool_name.clone()).or_default() += 1;
            if call.had_error {
                error_count += 1;
            }
            if let Some(fp) = call.input.get("file_path").and_then(|v| v.as_str()) {
                let basename = std::path::Path::new(fp)
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or(fp);
                match call.tool_name.as_str() {
                    "Write" => { files_written.insert(basename.to_string()); }
                    "Edit" => { files_edited.insert(basename.to_string()); }
                    _ => {}
                }
            }
        }

        // Paginate
        let paginated: Vec<ToolCallEntry> = all_calls
            .into_iter()
            .skip(offset)
            .take(limit)
            .enumerate()
            .map(|(i, call)| {
                let input_summary = extract_tool_input_summary(&call.tool_name, &call.input);
                ToolCallEntry {
                    index: offset + i,
                    timestamp: call.timestamp,
                    tool_name: call.tool_name,
                    input_summary,
                    had_error: call.had_error,
                    error_preview: call.error_text,
                }
            })
            .collect();

        let returned = paginated.len();

        let mut written: Vec<String> = files_written.into_iter().collect();
        written.sort();
        let mut edited: Vec<String> = files_edited.into_iter().collect();
        edited.sort();

        let response = ToolCallsResponse {
            session_id: resolved.session_id,
            total_tool_calls,
            returned,
            tool_calls: paginated,
            summary: ToolCallsSummary {
                by_tool,
                files_written: written,
                files_edited: edited,
                error_count,
            },
        };

        match ToolOutput::json(&response) {
            Ok(output) => output,
            Err(e) => ToolOutput::error(format!("JSON serialization error: {e}")),
        }
    }

    // ========================================================================
    // New Tool: get_session_lessons
    // ========================================================================

    /// Extract operational lessons from a session: error→fix pairs and user corrections.
    /// Targets the most expensive compaction failure mode (negative result amnesia).
    #[tool(description = "Extract lessons from a session: error->fix pairs (what failed and how it was resolved) and user corrections (where the user corrected agent behavior). Use after compaction to recover operational gotchas and avoid retrying failed approaches.")]
    async fn get_session_lessons(&self, request: GetSessionLessonsRequest) -> ToolOutput {
        use crate::analysis::lessons::{extract_lessons, LessonCategory, LessonOptions};

        let resolved = match resolve_session(self, &request.session_id) {
            Ok(r) => r,
            Err(e) => return e,
        };

        let category = request.category.as_deref().unwrap_or("all");
        let limit = request.limit.unwrap_or(30);

        // Use all entries (not just main thread) so lessons on branches
        // and across compaction boundaries are visible.
        let all_entries = resolved.conversation.chronological_entries();
        let entry_refs: Vec<&LogEntry> = all_entries.iter().map(|e| *e).collect();

        let opts = LessonOptions {
            category: LessonCategory::from_str_loose(category),
            limit,
            ..LessonOptions::default()
        };

        let result = extract_lessons(&entry_refs, &opts);

        // Convert from analysis types to MCP wire types
        let response = SessionLessonsResponse {
            session_id: resolved.session_id,
            project_path: resolved.project_path,
            error_fix_pairs: result
                .error_fix_pairs
                .into_iter()
                .map(|p| ErrorFixLesson {
                    timestamp: p.timestamp,
                    tool_name: p.tool_name,
                    input_summary: p.input_summary,
                    error_preview: p.error_preview,
                    resolution_summary: p.resolution_summary,
                    resolution_tools: p.resolution_tools,
                })
                .collect(),
            user_corrections: result
                .user_corrections
                .into_iter()
                .map(|c| UserCorrection {
                    timestamp: c.timestamp,
                    user_text: c.user_text,
                    prior_assistant_summary: c.prior_assistant_summary,
                })
                .collect(),
            summary: LessonsSummary {
                total_errors: result.summary.total_errors,
                total_corrections: result.summary.total_corrections,
                most_error_prone_tools: result.summary.most_error_prone_tools,
            },
        };

        match ToolOutput::json(&response) {
            Ok(output) => output,
            Err(e) => ToolOutput::error(format!("JSON serialization error: {e}")),
        }
    }

    // ========================================================================
    // Goal Management
    // ========================================================================

    /// Manage persistent goals for a project. Goals survive compaction and sessions.
    #[tool(description = "Manage persistent goals for a project. Operations: list, add, update, remove. Goals survive compaction and sessions.")]
    async fn manage_goals(&self, request: ManageGoalsRequest) -> ToolOutput {
        use crate::goals::{load_goals, save_goals, GoalStatus};

        let resolved = match resolve_project(self, &request.project) {
            Ok(r) => r,
            Err(e) => return e,
        };

        let mut store = match load_goals(&resolved.project_dir) {
            Ok(s) => s,
            Err(e) => return ToolOutput::error(format!("Failed to load goals: {e}")),
        };

        match request.operation.as_str() {
            "list" => {
                let goals: Vec<GoalEntry> = store
                    .goals
                    .iter()
                    .map(|g| GoalEntry {
                        id: g.id,
                        text: g.text.clone(),
                        status: g.status.to_string(),
                        created_at: g.created_at.to_rfc3339(),
                        updated_at: g.updated_at.to_rfc3339(),
                        progress: g.progress.clone(),
                    })
                    .collect();

                let response = ManageGoalsResponse {
                    operation: "list".into(),
                    project_path: resolved.project_path,
                    message: Some(format!("{} goal(s)", goals.len())),
                    goals: Some(goals),
                    goal: None,
                };

                match ToolOutput::json(&response) {
                    Ok(output) => output,
                    Err(e) => ToolOutput::error(format!("JSON error: {e}")),
                }
            }

            "add" => {
                let text = match request.text {
                    Some(t) if !t.trim().is_empty() => t,
                    _ => return ToolOutput::error("'text' is required for add operation"),
                };

                let id = store.add_goal(text.clone(), request.progress);

                if let Err(e) = save_goals(&resolved.project_dir, &store) {
                    return ToolOutput::error(format!("Failed to save goals: {e}"));
                }

                let goal = &store.goals.iter().find(|g| g.id == id).unwrap();
                let response = ManageGoalsResponse {
                    operation: "add".into(),
                    project_path: resolved.project_path,
                    message: Some(format!("Added goal #{id}")),
                    goals: None,
                    goal: Some(GoalEntry {
                        id: goal.id,
                        text: goal.text.clone(),
                        status: goal.status.to_string(),
                        created_at: goal.created_at.to_rfc3339(),
                        updated_at: goal.updated_at.to_rfc3339(),
                        progress: goal.progress.clone(),
                    }),
                };

                match ToolOutput::json(&response) {
                    Ok(output) => output,
                    Err(e) => ToolOutput::error(format!("JSON error: {e}")),
                }
            }

            "update" => {
                let id = match request.id {
                    Some(id) => id,
                    None => return ToolOutput::error("'id' is required for update operation"),
                };

                let status = match request.status.as_deref() {
                    Some(s) => match GoalStatus::parse(s) {
                        Some(status) => Some(status),
                        None => return ToolOutput::error(format!(
                            "Invalid status '{s}'. Use: open, in_progress, done, abandoned"
                        )),
                    },
                    None => None,
                };

                if status.is_none() && request.progress.is_none() {
                    return ToolOutput::error(
                        "At least one of 'status' or 'progress' is required for update",
                    );
                }

                if !store.update_goal(id, status, request.progress) {
                    return ToolOutput::error(format!("Goal #{id} not found"));
                }

                if let Err(e) = save_goals(&resolved.project_dir, &store) {
                    return ToolOutput::error(format!("Failed to save goals: {e}"));
                }

                let goal = store.goals.iter().find(|g| g.id == id).unwrap();
                let response = ManageGoalsResponse {
                    operation: "update".into(),
                    project_path: resolved.project_path,
                    message: Some(format!("Updated goal #{id}")),
                    goals: None,
                    goal: Some(GoalEntry {
                        id: goal.id,
                        text: goal.text.clone(),
                        status: goal.status.to_string(),
                        created_at: goal.created_at.to_rfc3339(),
                        updated_at: goal.updated_at.to_rfc3339(),
                        progress: goal.progress.clone(),
                    }),
                };

                match ToolOutput::json(&response) {
                    Ok(output) => output,
                    Err(e) => ToolOutput::error(format!("JSON error: {e}")),
                }
            }

            "remove" => {
                let id = match request.id {
                    Some(id) => id,
                    None => return ToolOutput::error("'id' is required for remove operation"),
                };

                if !store.remove_goal(id) {
                    return ToolOutput::error(format!("Goal #{id} not found"));
                }

                if let Err(e) = save_goals(&resolved.project_dir, &store) {
                    return ToolOutput::error(format!("Failed to save goals: {e}"));
                }

                let response = ManageGoalsResponse {
                    operation: "remove".into(),
                    project_path: resolved.project_path,
                    message: Some(format!("Removed goal #{id}")),
                    goals: None,
                    goal: None,
                };

                match ToolOutput::json(&response) {
                    Ok(output) => output,
                    Err(e) => ToolOutput::error(format!("JSON error: {e}")),
                }
            }

            other => ToolOutput::error(format!(
                "Unknown operation '{other}'. Use: list, add, update, remove"
            )),
        }
    }

    // ========================================================================
    // Session Digest
    // ========================================================================

    /// Get a compact summary of a session's key topics, files, tools, and decisions.
    #[tool(description = "Get a compact digest of a session: key prompts, files touched, top tools, errors, compaction events, and decision keywords from thinking blocks.")]
    async fn get_session_digest(&self, request: GetSessionDigestRequest) -> ToolOutput {
        use crate::analysis::digest::{build_digest, format_digest, DigestOptions};

        let resolved = match resolve_session(self, &request.session_id) {
            Ok(r) => r,
            Err(e) => return e,
        };

        let all_entries = resolved.conversation.chronological_entries();
        let entry_refs: Vec<&LogEntry> = all_entries.iter().map(|e| *e).collect();

        let opts = DigestOptions {
            max_prompts: request.max_prompts.unwrap_or(3),
            max_files: request.max_files.unwrap_or(10),
            ..DigestOptions::default()
        };

        let digest = build_digest(&entry_refs, &opts);
        let formatted = format_digest(&digest, opts.max_chars);

        let response = SessionDigestResponse {
            session_id: resolved.session_id,
            project_path: resolved.project_path,
            key_prompts: digest.key_prompts,
            recent_prompts: digest.recent_prompts,
            total_prompts: digest.total_prompts,
            files_touched: digest.files_touched,
            top_tools: digest.top_tools,
            error_count: digest.error_count,
            compaction_count: digest.compaction_count,
            thinking_keywords: digest.thinking_keywords,
            formatted,
        };

        match ToolOutput::json(&response) {
            Ok(output) => output,
            Err(e) => ToolOutput::error(format!("JSON serialization error: {e}")),
        }
    }

    // ========================================================================
    // Tactical Notes
    // ========================================================================

    /// Manage tactical session notes for a project. Notes capture work state that survives compaction.
    #[tool(description = "Manage tactical session notes for a project. Notes capture mid-work state (\"tried X, failed because Y\") that survives compaction. Operations: list, add, remove, clear.")]
    async fn manage_notes(&self, request: ManageNotesRequest) -> ToolOutput {
        use crate::notes::{load_notes, save_notes};

        let resolved = match resolve_project(self, &request.project) {
            Ok(r) => r,
            Err(e) => return e,
        };

        let mut store = match load_notes(&resolved.project_dir) {
            Ok(s) => s,
            Err(e) => return ToolOutput::error(format!("Failed to load notes: {e}")),
        };

        match request.operation.as_str() {
            "list" => {
                let notes: Vec<NoteEntry> = store
                    .notes
                    .iter()
                    .map(|n| NoteEntry {
                        id: n.id,
                        text: n.text.clone(),
                        created_at: n.created_at.to_rfc3339(),
                        session_id: n.session_id.clone(),
                    })
                    .collect();

                let response = ManageNotesResponse {
                    operation: "list".into(),
                    project_path: resolved.project_path,
                    message: Some(format!("{} note(s)", notes.len())),
                    notes: Some(notes),
                    note: None,
                };

                match ToolOutput::json(&response) {
                    Ok(output) => output,
                    Err(e) => ToolOutput::error(format!("JSON error: {e}")),
                }
            }

            "add" => {
                let text = match request.text {
                    Some(t) if !t.trim().is_empty() => t,
                    _ => return ToolOutput::error("'text' is required for add operation"),
                };

                let id = store.add_note(text.clone(), request.session_id);

                if let Err(e) = save_notes(&resolved.project_dir, &store) {
                    return ToolOutput::error(format!("Failed to save notes: {e}"));
                }

                let note = store.notes.iter().find(|n| n.id == id).unwrap();
                let response = ManageNotesResponse {
                    operation: "add".into(),
                    project_path: resolved.project_path,
                    message: Some(format!("Added note #{id}")),
                    notes: None,
                    note: Some(NoteEntry {
                        id: note.id,
                        text: note.text.clone(),
                        created_at: note.created_at.to_rfc3339(),
                        session_id: note.session_id.clone(),
                    }),
                };

                match ToolOutput::json(&response) {
                    Ok(output) => output,
                    Err(e) => ToolOutput::error(format!("JSON error: {e}")),
                }
            }

            "remove" => {
                let id = match request.id {
                    Some(id) => id,
                    None => return ToolOutput::error("'id' is required for remove operation"),
                };

                if !store.remove_note(id) {
                    return ToolOutput::error(format!("Note #{id} not found"));
                }

                if let Err(e) = save_notes(&resolved.project_dir, &store) {
                    return ToolOutput::error(format!("Failed to save notes: {e}"));
                }

                let response = ManageNotesResponse {
                    operation: "remove".into(),
                    project_path: resolved.project_path,
                    message: Some(format!("Removed note #{id}")),
                    notes: None,
                    note: None,
                };

                match ToolOutput::json(&response) {
                    Ok(output) => output,
                    Err(e) => ToolOutput::error(format!("JSON error: {e}")),
                }
            }

            "clear" => {
                let removed = store.clear();

                if let Err(e) = save_notes(&resolved.project_dir, &store) {
                    return ToolOutput::error(format!("Failed to save notes: {e}"));
                }

                let response = ManageNotesResponse {
                    operation: "clear".into(),
                    project_path: resolved.project_path,
                    message: Some(format!("Cleared {removed} note(s)")),
                    notes: None,
                    note: None,
                };

                match ToolOutput::json(&response) {
                    Ok(output) => output,
                    Err(e) => ToolOutput::error(format!("JSON error: {e}")),
                }
            }

            other => ToolOutput::error(format!(
                "Unknown operation '{other}'. Use: list, add, remove, clear"
            )),
        }
    }

    #[tool(description = "Manage a persistent decision registry for a project. Decisions track design choices with status, confidence, tags, and session provenance. Operations: list, add, update, remove, supersede. For confidence auto-scoring use CLI: snatch decisions score -p <project>.")]
    async fn manage_decisions(&self, request: ManageDecisionsRequest) -> ToolOutput {
        use crate::decisions::{load_decisions, save_decisions, DecisionStatus};

        let resolved = match resolve_project(self, &request.project) {
            Ok(r) => r,
            Err(e) => return e,
        };

        let mut store = match load_decisions(&resolved.project_dir) {
            Ok(s) => s,
            Err(e) => return ToolOutput::error(format!("Failed to load decisions: {e}")),
        };

        fn to_entry(d: &crate::decisions::Decision) -> DecisionEntry {
            DecisionEntry {
                id: d.id,
                title: d.title.clone(),
                description: d.description.clone(),
                status: d.status.to_string(),
                confidence: d.confidence,
                created_at: d.created_at.to_rfc3339(),
                updated_at: d.updated_at.to_rfc3339(),
                session_id: d.session_id.clone(),
                superseded_by: d.superseded_by,
                tags: d.tags.clone(),
                references: d.references.clone(),
            }
        }

        match request.operation.as_str() {
            "list" => {
                let mut decisions: Vec<&crate::decisions::Decision> = store.decisions.iter().collect();

                // Filter by status if specified
                if let Some(ref status_str) = request.status {
                    if let Some(status) = DecisionStatus::parse(status_str) {
                        decisions.retain(|d| d.status == status);
                    }
                }

                // Filter by tag if specified
                if let Some(ref tags_str) = request.tags {
                    let tag = tags_str.trim();
                    decisions.retain(|d| d.tags.iter().any(|t| t.contains(tag)));
                }

                let entries: Vec<DecisionEntry> = decisions.iter().map(|d| to_entry(d)).collect();

                let response = ManageDecisionsResponse {
                    operation: "list".into(),
                    project_path: resolved.project_path,
                    message: Some(format!("{} decision(s)", entries.len())),
                    decisions: Some(entries),
                    decision: None,
                };

                match ToolOutput::json(&response) {
                    Ok(output) => output,
                    Err(e) => ToolOutput::error(format!("JSON error: {e}")),
                }
            }

            "add" => {
                let title = match request.title {
                    Some(t) if !t.trim().is_empty() => t,
                    _ => return ToolOutput::error("'title' is required for add operation"),
                };

                let tags: Vec<String> = request
                    .tags
                    .as_deref()
                    .map(|t| t.split(',').map(|s| s.trim().to_string()).collect())
                    .unwrap_or_default();

                let id = store.add_decision(
                    title,
                    request.description,
                    request.session_id,
                    request.confidence,
                    tags,
                );

                // Apply status if specified
                if let Some(ref status_str) = request.status {
                    if let Some(status) = DecisionStatus::parse(status_str) {
                        store.update_decision(id, Some(status), None, None, None);
                    }
                }

                if let Err(e) = save_decisions(&resolved.project_dir, &store) {
                    return ToolOutput::error(format!("Failed to save decisions: {e}"));
                }

                let decision = store.decisions.iter().find(|d| d.id == id).unwrap();
                let response = ManageDecisionsResponse {
                    operation: "add".into(),
                    project_path: resolved.project_path,
                    message: Some(format!("Added decision #{id}")),
                    decisions: None,
                    decision: Some(to_entry(decision)),
                };

                match ToolOutput::json(&response) {
                    Ok(output) => output,
                    Err(e) => ToolOutput::error(format!("JSON error: {e}")),
                }
            }

            "update" => {
                let id = match request.id {
                    Some(id) => id,
                    None => return ToolOutput::error("'id' is required for update operation"),
                };

                let status = request
                    .status
                    .as_deref()
                    .and_then(DecisionStatus::parse);

                let tags: Option<Vec<String>> = request
                    .tags
                    .as_deref()
                    .map(|t| t.split(',').map(|s| s.trim().to_string()).collect());

                if !store.update_decision(id, status, request.description, request.confidence, tags) {
                    return ToolOutput::error(format!("Decision #{id} not found"));
                }

                if let Err(e) = save_decisions(&resolved.project_dir, &store) {
                    return ToolOutput::error(format!("Failed to save decisions: {e}"));
                }

                let decision = store.decisions.iter().find(|d| d.id == id).unwrap();
                let response = ManageDecisionsResponse {
                    operation: "update".into(),
                    project_path: resolved.project_path,
                    message: Some(format!("Updated decision #{id}")),
                    decisions: None,
                    decision: Some(to_entry(decision)),
                };

                match ToolOutput::json(&response) {
                    Ok(output) => output,
                    Err(e) => ToolOutput::error(format!("JSON error: {e}")),
                }
            }

            "remove" => {
                let id = match request.id {
                    Some(id) => id,
                    None => return ToolOutput::error("'id' is required for remove operation"),
                };

                if !store.remove_decision(id) {
                    return ToolOutput::error(format!("Decision #{id} not found"));
                }

                if let Err(e) = save_decisions(&resolved.project_dir, &store) {
                    return ToolOutput::error(format!("Failed to save decisions: {e}"));
                }

                let response = ManageDecisionsResponse {
                    operation: "remove".into(),
                    project_path: resolved.project_path,
                    message: Some(format!("Removed decision #{id}")),
                    decisions: None,
                    decision: None,
                };

                match ToolOutput::json(&response) {
                    Ok(output) => output,
                    Err(e) => ToolOutput::error(format!("JSON error: {e}")),
                }
            }

            "supersede" => {
                let id = match request.id {
                    Some(id) => id,
                    None => return ToolOutput::error("'id' is required for supersede operation"),
                };
                let by = match request.superseded_by {
                    Some(by) => by,
                    None => {
                        return ToolOutput::error(
                            "'superseded_by' is required for supersede operation",
                        )
                    }
                };

                if !store.supersede_decision(id, by) {
                    return ToolOutput::error(format!("Decision #{id} or #{by} not found"));
                }

                if let Err(e) = save_decisions(&resolved.project_dir, &store) {
                    return ToolOutput::error(format!("Failed to save decisions: {e}"));
                }

                let decision = store.decisions.iter().find(|d| d.id == id).unwrap();
                let response = ManageDecisionsResponse {
                    operation: "supersede".into(),
                    project_path: resolved.project_path,
                    message: Some(format!("Decision #{id} superseded by #{by}")),
                    decisions: None,
                    decision: Some(to_entry(decision)),
                };

                match ToolOutput::json(&response) {
                    Ok(output) => output,
                    Err(e) => ToolOutput::error(format!("JSON error: {e}")),
                }
            }

            other => ToolOutput::error(format!(
                "Unknown operation '{other}'. Use: list, add, update, remove, supersede. For auto-scoring use CLI: snatch decisions score -p <project>"
            )),
        }
    }

    #[tool(description = "Tag individual messages within a session for retrieval. Tags like 'decision', 'reversal', 'correction', 'bug', 'milestone' mark key moments. Use 'decision:topic' for topic-specific decision tags. Operations: add (tag a message), remove (untag), list (show tags for a session), search (find messages by tag).")]
    async fn tag_message(&self, request: TagMessageRequest) -> ToolOutput {
        use crate::message_tags::{load_message_tags, save_message_tags, TagSource};

        let resolved = match resolve_project(self, &request.project) {
            Ok(r) => r,
            Err(e) => return e,
        };

        fn to_entry(msg: &crate::message_tags::TaggedMessage) -> TaggedMessageEntry {
            TaggedMessageEntry {
                session_id: msg.session_id.clone(),
                message_uuid: msg.message_uuid.clone(),
                tags: msg.tags.iter().map(|t| MessageTagEntry {
                    tag: t.tag.clone(),
                    created_at: t.created_at.to_rfc3339(),
                    source: format!("{:?}", t.source).to_lowercase(),
                }).collect(),
            }
        }

        match request.operation.as_str() {
            "add" => {
                let session_id = match &request.session_id {
                    Some(s) => s.clone(),
                    None => return ToolOutput::error("'session_id' is required for add operation"),
                };
                let message_uuid = match &request.message_uuid {
                    Some(u) => u.clone(),
                    None => return ToolOutput::error("'message_uuid' is required for add operation"),
                };
                let tag = match &request.tag {
                    Some(t) => t.clone(),
                    None => return ToolOutput::error("'tag' is required for add operation"),
                };

                let mut store = match load_message_tags(&resolved.project_dir) {
                    Ok(s) => s,
                    Err(e) => return ToolOutput::error(format!("Failed to load message tags: {e}")),
                };

                let added = store.add_tag(&session_id, &message_uuid, &tag, TagSource::Manual);

                if let Err(e) = save_message_tags(&resolved.project_dir, &store) {
                    return ToolOutput::error(format!("Failed to save message tags: {e}"));
                }

                let response = TagMessageResponse {
                    operation: "add".into(),
                    project_path: resolved.project_path,
                    message: Some(if added {
                        format!("Tagged message {message_uuid} with '{tag}'")
                    } else {
                        format!("Message {message_uuid} already has tag '{tag}'")
                    }),
                    messages: None,
                    tags: None,
                };

                match ToolOutput::json(&response) {
                    Ok(output) => output,
                    Err(e) => ToolOutput::error(format!("JSON error: {e}")),
                }
            }

            "remove" => {
                let message_uuid = match &request.message_uuid {
                    Some(u) => u.clone(),
                    None => return ToolOutput::error("'message_uuid' is required for remove operation"),
                };
                let tag = match &request.tag {
                    Some(t) => t.clone(),
                    None => return ToolOutput::error("'tag' is required for remove operation"),
                };

                let mut store = match load_message_tags(&resolved.project_dir) {
                    Ok(s) => s,
                    Err(e) => return ToolOutput::error(format!("Failed to load message tags: {e}")),
                };

                let removed = store.remove_tag(&message_uuid, &tag);

                if let Err(e) = save_message_tags(&resolved.project_dir, &store) {
                    return ToolOutput::error(format!("Failed to save message tags: {e}"));
                }

                let response = TagMessageResponse {
                    operation: "remove".into(),
                    project_path: resolved.project_path,
                    message: Some(if removed {
                        format!("Removed tag '{tag}' from message {message_uuid}")
                    } else {
                        format!("Message {message_uuid} does not have tag '{tag}'")
                    }),
                    messages: None,
                    tags: None,
                };

                match ToolOutput::json(&response) {
                    Ok(output) => output,
                    Err(e) => ToolOutput::error(format!("JSON error: {e}")),
                }
            }

            "list" => {
                let store = match load_message_tags(&resolved.project_dir) {
                    Ok(s) => s,
                    Err(e) => return ToolOutput::error(format!("Failed to load message tags: {e}")),
                };

                let messages = if let Some(ref session_id) = request.session_id {
                    store.messages_in_session(session_id)
                        .into_iter()
                        .map(to_entry)
                        .collect()
                } else {
                    store.messages.values()
                        .map(to_entry)
                        .collect()
                };

                let response = TagMessageResponse {
                    operation: "list".into(),
                    project_path: resolved.project_path,
                    message: None,
                    messages: Some(messages),
                    tags: Some(store.all_tags()),
                };

                match ToolOutput::json(&response) {
                    Ok(output) => output,
                    Err(e) => ToolOutput::error(format!("JSON error: {e}")),
                }
            }

            "search" => {
                let tag = match &request.tag {
                    Some(t) => t.clone(),
                    None => return ToolOutput::error("'tag' is required for search operation"),
                };

                let store = match load_message_tags(&resolved.project_dir) {
                    Ok(s) => s,
                    Err(e) => return ToolOutput::error(format!("Failed to load message tags: {e}")),
                };

                let messages: Vec<TaggedMessageEntry> = store.messages_with_tag(&tag)
                    .into_iter()
                    .map(to_entry)
                    .collect();

                let response = TagMessageResponse {
                    operation: "search".into(),
                    project_path: resolved.project_path,
                    message: Some(format!("Found {} messages with tag '{tag}'", messages.len())),
                    messages: Some(messages),
                    tags: None,
                };

                match ToolOutput::json(&response) {
                    Ok(output) => output,
                    Err(e) => ToolOutput::error(format!("JSON error: {e}")),
                }
            }

            other => ToolOutput::error(format!(
                "Unknown operation '{other}'. Use: add, remove, list, search"
            )),
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
