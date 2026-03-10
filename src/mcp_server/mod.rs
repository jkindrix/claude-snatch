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
//! - `get_file_history` - Reverse index: file path → sessions that modified it
//! - `thread_topic` - Cross-session topic threading with conversation context
//! - `detect_decisions` - Decision point detection with confidence scoring
//! - `detect_conflicts` - Contradiction detection across sessions and decision registry
//! - `get_project_lessons` - Cross-session lesson aggregation: recurring errors and corrections
//! - `get_project_health` - Project health dashboard: hotspots, rework, error trends
//! - `get_event_context` - Contextual zoom around a specific event by message_id or timestamp
//! - `project_retrospective` - Composite analysis: health + lessons + decisions in one call
//! - `explain_file_evolution` - Why a file changed: modification history with conversation context
//! - `suggest_priorities` - What to work on next: errors, churn, goals, decisions ranked by score

#![cfg(feature = "mcp")]

pub mod helpers;
pub mod types;

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use mcpkit::prelude::*;
use mcpkit::transport::stdio::StdioTransport;

use crate::analytics::{AnalyticsSummary, SessionAnalytics};
use crate::discovery::{chain::detect_chains, ClaudeDirectory};
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

        // Detect chains for the sessions we're listing
        let main_sessions: Vec<_> = sessions.iter()
            .filter(|s| !s.is_subagent())
            .collect();
        let chains = detect_chains(
            main_sessions.iter().map(|s| (s.session_id(), s.path()))
        );
        // Build reverse lookup: file_id -> (chain_root, chain_len)
        let mut chain_lookup: HashMap<String, (String, usize)> = HashMap::new();
        for (root_id, chain) in &chains {
            for member in &chain.members {
                chain_lookup.insert(
                    member.file_id.clone(),
                    (root_id.clone(), chain.len()),
                );
            }
        }

        let summaries: Vec<SessionSummary> = sessions
            .iter()
            .map(|s| {
                let (duration, compaction_count, slug) = s.quick_metadata_cached()
                    .map(|m| (m.duration_human(), m.compaction_count, m.slug.clone()))
                    .unwrap_or((None, 0, None));
                let chain_info = chain_lookup.get(s.session_id());
                SessionSummary {
                    session_id: s.session_id().to_string(),
                    slug,
                    project_path: s.project_path().to_string(),
                    is_subagent: s.is_subagent(),
                    parent_session_id: s.parent_session_id().map(String::from),
                    modified_time: Some(s.modified_datetime().to_rfc3339()),
                    is_active: s.is_active().unwrap_or(false),
                    duration,
                    compaction_count,
                    chain_id: chain_info.map(|(root, _)| root.clone()),
                    chain_length: chain_info.map(|(_, len)| *len),
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

        let (compaction_count, slug) = session.quick_metadata_cached()
            .map(|m| (m.compaction_count, m.slug.clone()))
            .unwrap_or((0, None));

        // Detect chain membership for this session
        let (chain_id, chain_members) = if !session.is_subagent() {
            if let Ok(Some(project)) = claude_dir.find_project(session.project_path()) {
                if let Ok(chains) = project.session_chains() {
                    chains.values()
                        .find(|c| c.contains(session.session_id()))
                        .map(|c| (
                            Some(c.root_id.clone()),
                            Some(c.file_ids().into_iter().map(String::from).collect()),
                        ))
                        .unwrap_or((None, None))
                } else {
                    (None, None)
                }
            } else {
                (None, None)
            }
        } else {
            (None, None)
        };

        let info = SessionInfoResponse {
            session_id: session.session_id().to_string(),
            slug,
            chain_id,
            chain_members,
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
        let chain_aware = request.chain_aware.unwrap_or(false);
        let resolved = match resolve_session_with_chain(self, &request.session_id, chain_aware) {
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

        // Filter by timestamp window
        if request.after_timestamp.is_some() || request.before_timestamp.is_some() {
            let after = if let Some(ref ts) = request.after_timestamp {
                match parse_timestamp_param(ts) {
                    Ok(dt) => Some(dt),
                    Err(e) => return ToolOutput::error(format!("Invalid after_timestamp: {e}")),
                }
            } else {
                None
            };
            let before = if let Some(ref ts) = request.before_timestamp {
                match parse_timestamp_param(ts) {
                    Ok(dt) => Some(dt),
                    Err(e) => return ToolOutput::error(format!("Invalid before_timestamp: {e}")),
                }
            } else {
                None
            };
            entries.retain(|e| {
                if let Some(ts) = e.timestamp() {
                    if let Some(ref a) = after {
                        if ts < *a { return false; }
                    }
                    if let Some(ref b) = before {
                        if ts > *b { return false; }
                    }
                    true
                } else {
                    // Keep entries without timestamps (conservative)
                    true
                }
            });
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
        let chain_aware = request.chain_aware.unwrap_or(false);
        let resolved = match resolve_session_with_chain(self, &request.session_id, chain_aware) {
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

        // Iterate per-project so we can detect chains
        let projects = match claude_dir.projects() {
            Ok(p) => p,
            Err(e) => return ToolOutput::error(format!("Failed to list projects: {e}")),
        };

        let filtered_projects: Vec<_> = projects
            .into_iter()
            .filter(|p| p.best_path().contains(&request.project))
            .collect();

        let mut project_path = String::new();
        let mut agg_tokens = 0u64;
        let mut agg_cost = 0.0f64;
        let mut agg_prompts = 0usize;
        let mut agg_branches = HashSet::new();
        let mut session_entries = Vec::new();

        for project in &filtered_projects {
            if project_path.is_empty() {
                project_path = project.best_path().to_string();
            }

            let mut sessions = match project.main_sessions() {
                Ok(s) => s,
                Err(_) => continue,
            };

            // Filter by time
            if let Some(cutoff_time) = cutoff {
                sessions.retain(|s| s.modified_datetime() >= cutoff_time);
            }

            // Detect chains for this project
            let chains = project.session_chains().unwrap_or_default();

            // Build lookup: session_id → chain info
            let mut chain_lookup: HashMap<String, (&str, usize, Option<&str>)> = HashMap::new();
            let mut skip_set: HashSet<String> = HashSet::new();
            for chain in chains.values() {
                for member in &chain.members {
                    chain_lookup.insert(
                        member.file_id.clone(),
                        (&chain.root_id, chain.len(), chain.slug.as_deref()),
                    );
                    if member.file_id != chain.root_id {
                        skip_set.insert(member.file_id.clone());
                    }
                }
            }

            for session in &sessions {
                let sid = session.session_id().to_string();

                // Skip non-root chain members (they'll be included in the root entry)
                if skip_set.contains(&sid) {
                    continue;
                }

                // If this is a chain root, parse the full chain; otherwise single file
                let (entries, chain_info) = if let Some(chain) = chains.get(&sid) {
                    match project.parse_chain(chain) {
                        Ok(e) => (e, Some((chain.root_id.clone(), chain.len(), chain.slug.clone()))),
                        Err(_) => continue,
                    }
                } else {
                    match session.parse_with_options(self.max_file_size) {
                        Ok(e) => {
                            let slug = chain_lookup.get(&sid).and_then(|(_, _, s)| s.map(String::from));
                            (e, slug.map(|s| (sid.clone(), 1, Some(s))))
                        }
                        Err(_) => continue,
                    }
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

                // Extract chain metadata
                let (chain_id, chain_length, slug) = match chain_info {
                    Some((root, len, s)) if len > 1 => (Some(root), Some(len), s),
                    Some((_, _, s)) => (None, None, s),
                    None => (None, None, None),
                };

                session_entries.push(ProjectSessionEntry {
                    session_id: session.session_id().to_string(),
                    slug,
                    chain_id,
                    chain_length,
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
        }

        // Filter out empty sessions (no prompts and no tokens)
        session_entries.retain(|s| s.user_prompt_count > 0 || s.total_tokens > 0);

        // Sort by start time (newest first) and truncate
        session_entries.sort_by(|a, b| b.start_time.cmp(&a.start_time));
        session_entries.truncate(limit);

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

        // Build chain lookup: session_id → chain root_id
        let mut chain_lookup: HashMap<String, String> = HashMap::new();
        if let Ok(projects) = claude_dir.projects() {
            for project in &projects {
                if let Some(ref proj_filter) = request.project {
                    if !project.best_path().contains(proj_filter) {
                        continue;
                    }
                }
                if let Ok(chains) = project.session_chains() {
                    for chain in chains.values() {
                        for member in &chain.members {
                            chain_lookup.insert(member.file_id.clone(), chain.root_id.clone());
                        }
                    }
                }
            }
        }

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

            let sid = session.session_id().to_string();
            let chain_id = chain_lookup.get(&sid).cloned();

            // Search ALL entries (not just main thread) so branches,
            // sidechains, and agent sub-conversations are included.
            for entry in conversation.chronological_entries() {
                let matches = search_entry_text(entry, &regex, scope, 100);
                for (matched, context) in matches {
                    results.push(SearchMatch {
                        session_id: sid.clone(),
                        project_path: session.project_path().to_string(),
                        chain_id: chain_id.clone(),
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

    /// Look up which sessions modified a file. Returns file modification history
    /// from file-history-snapshot entries — the reverse index from file path to sessions.
    #[tool(description = "Look up which sessions modified a file path. Uses file-history-snapshot entries to build a reverse index. Returns session IDs, timestamps, and version numbers for each modification. Use to answer 'when was this file changed?' or 'which session introduced this code?'")]
    async fn get_file_history(&self, request: GetFileHistoryRequest) -> ToolOutput {
        use crate::file_index::FileIndex;

        let claude_dir = match self.get_claude_dir() {
            Ok(dir) => dir,
            Err(e) => return ToolOutput::error(e),
        };

        let projects = match claude_dir.projects() {
            Ok(p) => p,
            Err(e) => return ToolOutput::error(format!("Failed to list projects: {e}")),
        };

        let mut sessions = Vec::new();
        for project in &projects {
            if let Some(ref filter) = request.project {
                if !project.best_path().contains(filter) {
                    continue;
                }
            }
            if let Ok(s) = project.sessions() {
                sessions.extend(s);
            }
        }

        let index = FileIndex::from_sessions(&sessions, self.max_file_size);
        let mut matches = index.search(&request.path);
        matches.sort_by_key(|(path, _)| path.to_string());

        let limit = request.limit.unwrap_or(50);
        let total_files = matches.len();
        let total_modifications: usize = matches.iter().map(|(_, m)| m.len()).sum();

        let mut modifications = Vec::new();
        for (path, mods) in &matches {
            for m in *mods {
                if modifications.len() >= limit {
                    break;
                }
                modifications.push(FileModificationEntry {
                    file_path: path.to_string(),
                    session_id: m.session_id.clone(),
                    project_path: m.project_path.clone(),
                    message_id: m.message_id.clone(),
                    timestamp: m.timestamp.to_rfc3339(),
                    version: m.version,
                });
            }
            if modifications.len() >= limit {
                break;
            }
        }

        let returned = modifications.len();
        let response = GetFileHistoryResponse {
            path_query: request.path,
            total_files,
            total_modifications,
            returned,
            modifications,
        };

        match ToolOutput::json(&response) {
            Ok(output) => output,
            Err(e) => ToolOutput::error(format!("JSON error: {e}")),
        }
    }

    /// Cross-session topic threading: search for a pattern across sessions and
    /// return chronologically-ordered exchanges with conversation context.
    #[tool(description = "Cross-session topic threading. Searches all sessions for a regex pattern and returns chronologically-ordered exchanges with surrounding user/assistant context. Use to trace how a topic evolved across sessions — 'show me every time we discussed X'. Set decisions_only=true to filter to decision points. Set include_thinking=true to search reasoning blocks.")]
    async fn thread_topic(&self, request: ThreadTopicRequest) -> ToolOutput {
        use crate::analysis::threading::{thread_topic, ThreadParams};
        use crate::cli::helpers::truncate;

        let claude_dir = match self.get_claude_dir() {
            Ok(dir) => dir,
            Err(e) => return ToolOutput::error(e),
        };

        let pattern = &request.pattern;
        let ignore_case = true;
        let regex = match regex::RegexBuilder::new(pattern)
            .case_insensitive(ignore_case)
            .build()
        {
            Ok(r) => r,
            Err(e) => return ToolOutput::error(format!("Invalid regex pattern: {e}")),
        };

        // Collect sessions with filters
        let mut sessions = if let Some(ref session_id) = request.session_id {
            match claude_dir.find_session(session_id) {
                Ok(Some(s)) => vec![s],
                Ok(None) => return ToolOutput::error(format!("Session not found: {session_id}")),
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
            all
        };

        if request.no_subagents.unwrap_or(true) {
            sessions.retain(|s| !s.is_subagent());
        }

        // Apply date filters
        if request.since.is_some() || request.until.is_some() {
            use crate::cli::helpers::filter_sessions_by_date;
            if let Err(e) = filter_sessions_by_date(
                &mut sessions,
                request.since.as_deref(),
                request.until.as_deref(),
            ) {
                return ToolOutput::error(format!("Date filter error: {e}"));
            }
        }

        let max_context = request.max_context.unwrap_or(500);
        let params = ThreadParams {
            include_thinking: request.include_thinking.unwrap_or(false),
            limit: request.limit.unwrap_or(30),
            max_user_context: max_context,
            max_assistant_context: max_context,
            max_thinking_context: max_context,
            role_filter: None,
            decisions_only: request.decisions_only.unwrap_or(false),
        };

        let result = thread_topic(&sessions, &regex, &params, self.max_file_size);

        let exchanges: Vec<ThreadExchangeEntry> = result
            .exchanges
            .into_iter()
            .map(|e| ThreadExchangeEntry {
                timestamp: e.timestamp.to_rfc3339(),
                session_id: e.session_id,
                project: e.project,
                entry_uuid: e.entry_uuid,
                user_text: e.user_text.map(|t| truncate(&t, max_context)),
                assistant_text: e.assistant_text.map(|t| truncate(&t, max_context)),
                thinking_text: e.thinking_text.map(|t| truncate(&t, max_context)),
                match_location: e.match_location,
                match_count: e.match_count,
            })
            .collect();

        let response = ThreadTopicResponse {
            pattern: request.pattern,
            total_exchanges: exchanges.len(),
            session_count: result.session_count,
            total_matches: result.total_matches,
            exchanges,
        };

        match ToolOutput::json(&response) {
            Ok(output) => output,
            Err(e) => ToolOutput::error(format!("JSON error: {e}")),
        }
    }

    /// Detect candidate decision points across sessions using structural patterns,
    /// explicit markers, and reversal detection.
    #[tool(description = "Detect candidate decision points across sessions. Uses three detection methods: (1) structural pattern matching (question→options→confirmation), (2) explicit markers ('DEF-001', 'we decided', 'design decision'), (3) reversal patterns ('changed my mind', 'scratch that'). Returns candidates with confidence scores. Use to find decisions that should be tracked.")]
    async fn detect_decisions(&self, request: DetectDecisionsRequest) -> ToolOutput {
        use crate::analysis::decision_detection::{detect_decisions, DetectParams};
        use crate::cli::helpers::{filter_sessions_by_date, truncate};

        let claude_dir = match self.get_claude_dir() {
            Ok(dir) => dir,
            Err(e) => return ToolOutput::error(e),
        };

        let mut sessions = if let Some(ref session_id) = request.session_id {
            match claude_dir.find_session(session_id) {
                Ok(Some(s)) => vec![s],
                Ok(None) => return ToolOutput::error(format!("Session not found: {session_id}")),
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
            all
        };

        if request.no_subagents.unwrap_or(true) {
            sessions.retain(|s| !s.is_subagent());
        }

        if request.since.is_some() || request.until.is_some() {
            if let Err(e) = filter_sessions_by_date(
                &mut sessions,
                request.since.as_deref(),
                request.until.as_deref(),
            ) {
                return ToolOutput::error(format!("Date filter error: {e}"));
            }
        }

        let topic_filter = if let Some(ref topic) = request.topic {
            match regex::Regex::new(topic) {
                Ok(r) => Some(r),
                Err(e) => return ToolOutput::error(format!("Invalid topic regex: {e}")),
            }
        } else {
            None
        };

        let params = DetectParams {
            min_confidence: request.min_confidence.unwrap_or(0.5),
            limit: request.limit.unwrap_or(50),
            topic_filter,
        };

        let result = detect_decisions(&sessions, &params, self.max_file_size);

        let candidates: Vec<DetectedDecisionEntry> = result
            .candidates
            .into_iter()
            .map(|c| DetectedDecisionEntry {
                timestamp: c.timestamp.to_rfc3339(),
                session_id: c.session_id,
                entry_uuid: c.entry_uuid,
                detection_method: format!("{}", c.detection_method),
                confidence: c.confidence,
                question: truncate(&c.question, 300),
                response: truncate(&c.response, 500),
                confirmation: c.confirmation.map(|t| truncate(&t, 200)),
            })
            .collect();

        let response = DetectDecisionsResponse {
            total_candidates: candidates.len(),
            candidates,
        };

        match ToolOutput::json(&response) {
            Ok(output) => output,
            Err(e) => ToolOutput::error(format!("JSON error: {e}")),
        }
    }

    // ========================================================================
    // New Tool: get_project_lessons
    // ========================================================================

    /// Aggregate lessons across all sessions for a project.
    #[tool(description = "Aggregate error->fix pairs and user corrections across ALL sessions for a project. Deduplicates similar errors, ranks by frequency, identifies recurring failure patterns. Answers 'what keeps going wrong?' across the project lifetime. More useful than per-session lessons after many sessions.")]
    async fn get_project_lessons(&self, request: GetProjectLessonsRequest) -> ToolOutput {
        use crate::analysis::project_lessons::{aggregate_project_lessons, ProjectLessonsParams};

        let claude_dir = match self.get_claude_dir() {
            Ok(dir) => dir,
            Err(e) => return ToolOutput::error(e),
        };

        let period = request.period.as_deref().unwrap_or("7d");
        let cutoff = match period_cutoff(period) {
            Ok(c) => c,
            Err(e) => return ToolOutput::error(format!("Invalid period: {e}")),
        };

        let mut sessions = match claude_dir.all_sessions() {
            Ok(s) => s,
            Err(e) => return ToolOutput::error(format!("Failed to list sessions: {e}")),
        };

        sessions.retain(|s| s.project_path().contains(request.project.as_str()));

        if request.no_subagents.unwrap_or(true) {
            sessions.retain(|s| !s.is_subagent());
        }

        // Apply period filter
        if let Some(cutoff_dt) = cutoff {
            let cutoff_systime = std::time::SystemTime::from(cutoff_dt);
            sessions.retain(|s| s.modified_time() >= cutoff_systime);
        }

        let params = ProjectLessonsParams {
            category: request.category.unwrap_or_else(|| "all".to_string()),
            limit: request.limit.unwrap_or(30),
            min_occurrences: request.min_occurrences.unwrap_or(1),
        };

        let result = aggregate_project_lessons(&sessions, &params, self.max_file_size);

        let recurring_errors: Vec<RecurringErrorEntry> = result.recurring_errors
            .into_iter()
            .map(|e| RecurringErrorEntry {
                tool_name: e.tool_name,
                error_pattern: e.error_pattern,
                count: e.count,
                sessions: e.sessions,
                last_seen: e.last_seen,
                example_resolution: e.example_resolution,
            })
            .collect();

        let recurring_corrections: Vec<RecurringCorrectionEntry> = result.recurring_corrections
            .into_iter()
            .map(|c| RecurringCorrectionEntry {
                pattern: c.pattern,
                count: c.count,
                sessions: c.sessions,
                examples: c.examples,
            })
            .collect();

        let response = GetProjectLessonsResponse {
            project_path: request.project,
            period: period.to_string(),
            sessions_analyzed: result.summary.sessions_analyzed,
            total_errors: result.summary.total_errors,
            total_corrections: result.summary.total_corrections,
            top_failure_modes: result.summary.top_failure_modes,
            recurring_errors,
            recurring_corrections,
        };

        match ToolOutput::json(&response) {
            Ok(output) => output,
            Err(e) => ToolOutput::error(format!("JSON error: {e}")),
        }
    }

    // ========================================================================
    // New Tool: get_project_health
    // ========================================================================

    /// Project health dashboard: hotspot files, rework, error trends, decision stability.
    #[tool(description = "Project health dashboard. Shows hotspot files (most edits), rework files (edited across multiple sessions), decision stability metrics, and per-session error/tool counts. Answers 'which parts of the codebase cause the most trouble?' and 'are we improving?'")]
    async fn get_project_health(&self, request: GetProjectHealthRequest) -> ToolOutput {
        use crate::analysis::project_health::{analyze_project_health, ProjectHealthParams};

        let claude_dir = match self.get_claude_dir() {
            Ok(dir) => dir,
            Err(e) => return ToolOutput::error(e),
        };

        let period = request.period.as_deref().unwrap_or("7d");
        let cutoff = match period_cutoff(period) {
            Ok(c) => c,
            Err(e) => return ToolOutput::error(format!("Invalid period: {e}")),
        };

        let mut sessions = match claude_dir.all_sessions() {
            Ok(s) => s,
            Err(e) => return ToolOutput::error(format!("Failed to list sessions: {e}")),
        };

        sessions.retain(|s| s.project_path().contains(request.project.as_str()));

        if request.no_subagents.unwrap_or(true) {
            sessions.retain(|s| !s.is_subagent());
        }

        if let Some(cutoff_dt) = cutoff {
            let cutoff_systime = std::time::SystemTime::from(cutoff_dt);
            sessions.retain(|s| s.modified_time() >= cutoff_systime);
        }

        // Try to load decision store for this project
        let projects = claude_dir.projects().unwrap_or_default();
        let decision_store = projects.iter()
            .find(|p| p.path().to_string_lossy().contains(request.project.as_str()))
            .and_then(|proj| crate::decisions::load_decisions(proj.path()).ok());

        let params = ProjectHealthParams {
            max_hotspots: request.max_hotspots.unwrap_or(20),
        };

        let result = analyze_project_health(
            &sessions,
            decision_store.as_ref(),
            &params,
            self.max_file_size,
        );

        let response = GetProjectHealthResponse {
            project_path: request.project,
            period: period.to_string(),
            sessions_analyzed: result.sessions_analyzed,
            total_errors: result.total_errors,
            total_tool_calls: result.total_tool_calls,
            hotspot_files: result.hotspot_files.into_iter().map(|f| HotspotFileEntry {
                path: f.path,
                edit_count: f.edit_count,
                session_count: f.session_count,
            }).collect(),
            rework_files: result.rework_files.into_iter().map(|f| ReworkFileEntry {
                path: f.path,
                version_count: f.version_count,
                session_count: f.session_count,
            }).collect(),
            decision_churn: result.decision_churn.map(|dc| DecisionChurnEntry {
                total_decisions: dc.total_decisions,
                confirmed_count: dc.confirmed_count,
                superseded_count: dc.superseded_count,
                abandoned_count: dc.abandoned_count,
                proposed_count: dc.proposed_count,
            }),
            session_stats: result.session_stats.into_iter().map(|s| SessionHealthEntry {
                session_id: s.session_id,
                timestamp: s.timestamp,
                error_count: s.error_count,
                tool_count: s.tool_count,
            }).collect(),
        };

        match ToolOutput::json(&response) {
            Ok(output) => output,
            Err(e) => ToolOutput::error(format!("JSON error: {e}")),
        }
    }

    // ========================================================================
    // New Tool: project_retrospective
    // ========================================================================

    /// Composite project retrospective combining health, lessons, and decisions.
    #[tool(description = "Composite project analysis: combines health metrics (hotspots, rework), recurring errors/corrections, decision stability, and per-session stats into a single retrospective. Answers 'how is this project going?' in one call instead of chaining get_project_health + get_project_lessons + manage_decisions. Use for periodic project reviews or when starting a new session on a project.")]
    async fn project_retrospective(&self, request: ProjectRetrospectiveRequest) -> ToolOutput {
        use crate::analysis::retrospective::{analyze_retrospective, RetrospectiveParams};
        use crate::cli::helpers::truncate;

        let claude_dir = match self.get_claude_dir() {
            Ok(dir) => dir,
            Err(e) => return ToolOutput::error(e),
        };

        let period = request.period.as_deref().unwrap_or("7d");
        let cutoff = match period_cutoff(period) {
            Ok(c) => c,
            Err(e) => return ToolOutput::error(format!("Invalid period: {e}")),
        };

        let mut sessions = match claude_dir.all_sessions() {
            Ok(s) => s,
            Err(e) => return ToolOutput::error(format!("Failed to list sessions: {e}")),
        };

        sessions.retain(|s| s.project_path().contains(request.project.as_str()));

        if request.no_subagents.unwrap_or(true) {
            sessions.retain(|s| !s.is_subagent());
        }

        if let Some(cutoff_dt) = cutoff {
            let cutoff_systime = std::time::SystemTime::from(cutoff_dt);
            sessions.retain(|s| s.modified_time() >= cutoff_systime);
        }

        // Load decision store
        let projects = claude_dir.projects().unwrap_or_default();
        let decision_store = projects.iter()
            .find(|p| p.path().to_string_lossy().contains(request.project.as_str()))
            .and_then(|proj| crate::decisions::load_decisions(proj.path()).ok());

        let params = RetrospectiveParams {
            max_files: request.max_files.unwrap_or(10),
            max_errors: request.max_errors.unwrap_or(10),
            max_corrections: request.max_corrections.unwrap_or(5),
            min_occurrences: request.min_occurrences.unwrap_or(1),
        };

        let result = analyze_retrospective(
            &sessions,
            decision_store.as_ref(),
            &params,
            self.max_file_size,
        );

        let response = ProjectRetrospectiveResponse {
            project_path: request.project,
            period: period.to_string(),
            summary: RetrospectiveSummaryEntry {
                sessions_analyzed: result.summary.sessions_analyzed,
                total_errors: result.summary.total_errors,
                total_tool_calls: result.summary.total_tool_calls,
                total_corrections: result.summary.total_corrections,
                error_rate: result.summary.error_rate,
                top_failure_modes: result.summary.top_failure_modes.into_iter()
                    .map(|(tool, count)| FailureModeEntry { tool, count })
                    .collect(),
            },
            hotspot_files: result.hotspot_files.into_iter().map(|f| HotspotFileEntry {
                path: f.path,
                edit_count: f.edit_count,
                session_count: f.session_count,
            }).collect(),
            rework_files: result.rework_files.into_iter().map(|f| ReworkFileEntry {
                path: f.path,
                version_count: f.version_count,
                session_count: f.session_count,
            }).collect(),
            recurring_errors: result.recurring_errors.into_iter().map(|e| RecurringErrorEntry {
                tool_name: e.tool_name,
                error_pattern: truncate(&e.error_pattern, 300),
                count: e.count,
                sessions: e.sessions.into_iter().take(5).collect(),
                last_seen: e.last_seen,
                example_resolution: e.example_resolution.map(|r| truncate(&r, 200)),
            }).collect(),
            recurring_corrections: result.recurring_corrections.into_iter().map(|c| RecurringCorrectionEntry {
                pattern: truncate(&c.pattern, 300),
                count: c.count,
                sessions: c.sessions.into_iter().take(5).collect(),
                examples: c.examples.into_iter().take(3).map(|e| truncate(&e, 200)).collect(),
            }).collect(),
            decisions: result.decisions.into_iter().map(|d| ActiveDecisionEntry {
                id: d.id,
                title: d.title,
                status: d.status,
                confidence: d.confidence,
                tags: d.tags,
            }).collect(),
            session_stats: result.session_stats.into_iter().map(|s| SessionHealthEntry {
                session_id: s.session_id,
                timestamp: s.timestamp,
                error_count: s.error_count,
                tool_count: s.tool_count,
            }).collect(),
        };

        match ToolOutput::json(&response) {
            Ok(output) => output,
            Err(e) => ToolOutput::error(format!("JSON error: {e}")),
        }
    }

    // ========================================================================
    // New Tool: suggest_priorities
    // ========================================================================

    /// Suggest what to work on next based on project data.
    #[tool(description = "Suggest priorities based on project data: recurring errors (reliability issues), high-churn files (stability concerns), open goals (committed work), and proposed decisions (unresolved uncertainty). Returns ranked items with evidence. Use at session start or when deciding what to tackle next.")]
    async fn suggest_priorities(&self, request: SuggestPrioritiesRequest) -> ToolOutput {
        use crate::analysis::priorities::{suggest_priorities as analyze_priorities, PriorityParams};

        let claude_dir = match self.get_claude_dir() {
            Ok(dir) => dir,
            Err(e) => return ToolOutput::error(e),
        };

        let period = request.period.as_deref().unwrap_or("7d");
        let cutoff = match period_cutoff(period) {
            Ok(c) => c,
            Err(e) => return ToolOutput::error(format!("Invalid period: {e}")),
        };

        let mut sessions = match claude_dir.all_sessions() {
            Ok(s) => s,
            Err(e) => return ToolOutput::error(format!("Failed to list sessions: {e}")),
        };

        sessions.retain(|s| s.project_path().contains(request.project.as_str()));

        if request.no_subagents.unwrap_or(true) {
            sessions.retain(|s| !s.is_subagent());
        }

        if let Some(cutoff_dt) = cutoff {
            let cutoff_systime = std::time::SystemTime::from(cutoff_dt);
            sessions.retain(|s| s.modified_time() >= cutoff_systime);
        }

        // Load decision and goal stores
        let projects = claude_dir.projects().unwrap_or_default();
        let project_dir = projects.iter()
            .find(|p| p.path().to_string_lossy().contains(request.project.as_str()));

        let decision_store = project_dir
            .and_then(|proj| crate::decisions::load_decisions(proj.path()).ok());
        let goal_store = project_dir
            .and_then(|proj| crate::goals::load_goals(proj.path()).ok());

        let params = PriorityParams {
            max_priorities: request.max_priorities.unwrap_or(10),
            ..Default::default()
        };

        let result = analyze_priorities(
            &sessions,
            decision_store.as_ref(),
            goal_store.as_ref(),
            &params,
            self.max_file_size,
        );

        let response = SuggestPrioritiesResponse {
            project_path: request.project,
            period: period.to_string(),
            sessions_analyzed: result.sessions_analyzed,
            total_errors: result.total_errors,
            open_goals: result.open_goals,
            proposed_decisions: result.proposed_decisions,
            priorities: result.priorities.into_iter().map(|p| PriorityItemEntry {
                rank: p.rank,
                category: p.category,
                summary: p.summary,
                score: p.score,
                sources: p.sources.into_iter().map(|s| {
                    let (source_type, detail) = match &s {
                        crate::analysis::priorities::PrioritySource::RecurringError { .. } => ("error", s.to_string()),
                        crate::analysis::priorities::PrioritySource::FileChurn { .. } => ("churn", s.to_string()),
                        crate::analysis::priorities::PrioritySource::OpenGoal { .. } => ("goal", s.to_string()),
                        crate::analysis::priorities::PrioritySource::ProposedDecision { .. } => ("decision", s.to_string()),
                    };
                    PrioritySourceEntry {
                        source_type: source_type.to_string(),
                        detail,
                    }
                }).collect(),
            }).collect(),
        };

        match ToolOutput::json(&response) {
            Ok(output) => output,
            Err(e) => ToolOutput::error(format!("JSON error: {e}")),
        }
    }

    // ========================================================================
    // New Tool: explain_file_evolution
    // ========================================================================

    /// Explain why a file changed over time.
    #[tool(description = "Explain how and why a file evolved across sessions. For each modification, shows the user prompt that triggered it, the assistant's response, thinking/rationale (if available), and tools used. Answers 'why did this file end up this way?' by combining file history with conversation context. Returns chronologically ordered change events.")]
    async fn explain_file_evolution(&self, request: ExplainFileEvolutionRequest) -> ToolOutput {
        use crate::analysis::file_evolution::{analyze_file_evolution, FileEvolutionParams};

        let claude_dir = match self.get_claude_dir() {
            Ok(dir) => dir,
            Err(e) => return ToolOutput::error(e),
        };

        let period = request.period.as_deref().unwrap_or("30d");
        let cutoff = match period_cutoff(period) {
            Ok(c) => c,
            Err(e) => return ToolOutput::error(format!("Invalid period: {e}")),
        };

        let mut sessions = match claude_dir.all_sessions() {
            Ok(s) => s,
            Err(e) => return ToolOutput::error(format!("Failed to list sessions: {e}")),
        };

        sessions.retain(|s| s.project_path().contains(request.project.as_str()));

        if request.no_subagents.unwrap_or(true) {
            sessions.retain(|s| !s.is_subagent());
        }

        if let Some(cutoff_dt) = cutoff {
            let cutoff_systime = std::time::SystemTime::from(cutoff_dt);
            sessions.retain(|s| s.modified_time() >= cutoff_systime);
        }

        let params = FileEvolutionParams {
            file_pattern: request.file_pattern.clone(),
            limit: request.limit.unwrap_or(30),
            max_text_len: 500,
            include_thinking: request.include_thinking.unwrap_or(true),
            context_window: request.context_window.unwrap_or(1),
        };

        let results = analyze_file_evolution(&sessions, &params, self.max_file_size);

        let response = ExplainFileEvolutionResponse {
            project_path: request.project,
            file_pattern: request.file_pattern,
            period: period.to_string(),
            files: results.into_iter().map(|r| FileEvolutionEntry {
                file_path: r.file_path,
                total_changes: r.total_changes,
                sessions_involved: r.sessions_involved,
                changes: r.changes.into_iter().map(|c| ChangeEventEntry {
                    timestamp: c.timestamp.to_rfc3339(),
                    session_id: c.session_id,
                    message_id: c.message_id,
                    version: c.version,
                    user_prompt: c.user_prompt,
                    assistant_response: c.assistant_response,
                    thinking: c.thinking,
                    tools_used: c.tools_used,
                    had_errors: c.had_errors,
                }).collect(),
            }).collect(),
        };

        match ToolOutput::json(&response) {
            Ok(output) => output,
            Err(e) => ToolOutput::error(format!("JSON error: {e}")),
        }
    }

    // ========================================================================
    // New Tool: get_event_context
    // ========================================================================

    /// Get contextual zoom around a specific event in a session.
    #[tool(description = "Get conversation context around a specific message or timestamp in a session. Returns the target event plus surrounding turns (user prompts, assistant responses, tools, errors). Use to understand 'what was happening around this event?' after finding events via other tools. Provide either message_id or timestamp.")]
    async fn get_event_context(&self, request: GetEventContextRequest) -> ToolOutput {
        use crate::analysis::event_context::{find_event_context, EventContextParams};

        if request.message_id.is_none() && request.timestamp.is_none() {
            return ToolOutput::error("Either message_id or timestamp is required");
        }

        let chain_aware = request.chain_aware.unwrap_or(false);
        let resolved = match resolve_session_with_chain(self, &request.session_id, chain_aware) {
            Ok(r) => r,
            Err(e) => return e,
        };

        let entries = resolved.conversation.main_thread_entries();
        let entry_refs: Vec<&LogEntry> = entries.iter().copied().collect();

        let timestamp = if let Some(ref ts) = request.timestamp {
            match parse_timestamp_param(ts) {
                Ok(dt) => Some(dt),
                Err(e) => return ToolOutput::error(format!("Invalid timestamp: {e}")),
            }
        } else {
            None
        };

        let params = EventContextParams {
            message_id: request.message_id,
            timestamp,
            context_window: request.context_window.unwrap_or(2),
            max_text_len: 500,
        };

        let result = match find_event_context(&entry_refs, &params) {
            Some(r) => r,
            None => return ToolOutput::error("Event not found in session"),
        };

        let to_entry = |t: crate::analysis::event_context::ContextTurn| -> ContextTurnEntry {
            ContextTurnEntry {
                index: t.index,
                msg_type: t.message_type,
                uuid: t.uuid,
                timestamp: t.timestamp.map(|ts| ts.to_rfc3339()),
                text: t.text,
                tools: t.tools,
                had_errors: t.had_errors,
            }
        };

        let response = GetEventContextResponse {
            session_id: resolved.session_id,
            target_index: result.target_index,
            target: to_entry(result.target),
            before: result.before.into_iter().map(to_entry).collect(),
            after: result.after.into_iter().map(to_entry).collect(),
            related_files: result.related_files,
            error_count: result.error_count,
        };

        match ToolOutput::json(&response) {
            Ok(output) => output,
            Err(e) => ToolOutput::error(format!("JSON error: {e}")),
        }
    }

    // ========================================================================
    // New Tool: detect_conflicts
    // ========================================================================

    /// Detect contradictions and conflicts across sessions and the decision registry.
    #[tool(description = "Detect contradictions across sessions. Two methods: (1) Registry-based: finds decisions sharing tags with opposing language, and supersede chains. Requires 'project'. (2) Search-based: finds opposing language about a topic across session conclusions. Requires 'topic'. Use to find where positions have changed or conflict.")]
    async fn detect_conflicts(&self, request: DetectConflictsRequest) -> ToolOutput {
        use crate::analysis::conflict_detection::{
            detect_registry_conflicts, detect_search_conflicts, ConflictPair,
        };
        use crate::cli::helpers::{filter_sessions_by_date, truncate};

        let claude_dir = match self.get_claude_dir() {
            Ok(dir) => dir,
            Err(e) => return ToolOutput::error(e),
        };

        let mut conflicts: Vec<ConflictPair> = Vec::new();

        // Registry-based detection
        if let Some(ref project) = request.project {
            let projects = match claude_dir.projects() {
                Ok(p) => p,
                Err(e) => return ToolOutput::error(format!("Failed to list projects: {e}")),
            };

            if let Some(proj) = projects.iter().find(|p| p.path().to_string_lossy().contains(project.as_str())) {
                let project_dir = proj.path();
                match crate::decisions::load_decisions(project_dir) {
                    Ok(store) => {
                        detect_registry_conflicts(&store, &request.topic, &mut conflicts);
                    }
                    Err(_) => {} // No decision store — skip registry detection
                }
            }
        }

        // Search-based detection
        if let Some(ref topic) = request.topic {
            let mut sessions = if let Some(ref project) = request.project {
                let mut all = match claude_dir.all_sessions() {
                    Ok(s) => s,
                    Err(e) => return ToolOutput::error(format!("Failed to list sessions: {e}")),
                };
                all.retain(|s| s.project_path().contains(project.as_str()));
                all
            } else {
                match claude_dir.all_sessions() {
                    Ok(s) => s,
                    Err(e) => return ToolOutput::error(format!("Failed to list sessions: {e}")),
                }
            };

            if request.no_subagents.unwrap_or(true) {
                sessions.retain(|s| !s.is_subagent());
            }

            if request.since.is_some() || request.until.is_some() {
                if let Err(e) = filter_sessions_by_date(
                    &mut sessions,
                    request.since.as_deref(),
                    request.until.as_deref(),
                ) {
                    return ToolOutput::error(format!("Date filter error: {e}"));
                }
            }

            match detect_search_conflicts(
                &sessions,
                topic,
                request.exclude_session.as_deref(),
                self.max_file_size,
            ) {
                Ok(search_conflicts) => conflicts.extend(search_conflicts),
                Err(e) => return ToolOutput::error(format!("Search conflict detection error: {e}")),
            }
        }

        let min_confidence = request.min_confidence.unwrap_or(0.3);
        conflicts.retain(|c| c.confidence >= min_confidence);

        conflicts.sort_by(|a, b| {
            b.confidence
                .partial_cmp(&a.confidence)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.earlier_time.cmp(&b.earlier_time))
        });

        let limit = request.limit.unwrap_or(50);
        conflicts.truncate(limit);

        let entries: Vec<ConflictPairEntry> = conflicts
            .into_iter()
            .map(|c| ConflictPairEntry {
                topic: c.topic,
                detection_method: format!("{}", c.detection),
                confidence: c.confidence,
                earlier_timestamp: c.earlier_time.to_rfc3339(),
                earlier_session_id: c.earlier_session,
                earlier_text: truncate(&c.earlier_text, 500),
                later_timestamp: c.later_time.to_rfc3339(),
                later_session_id: c.later_session,
                later_text: truncate(&c.later_text, 500),
            })
            .collect();

        let response = DetectConflictsResponse {
            total_conflicts: entries.len(),
            conflicts: entries,
        };

        match ToolOutput::json(&response) {
            Ok(output) => output,
            Err(e) => ToolOutput::error(format!("JSON error: {e}")),
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::discovery::encode_project_path;
    use tempfile::TempDir;

    const PROJECT_PATH: &str = "/home/user/test-project";

    fn setup_claude_dir(session_id: &str, project_path: &str, jsonl: &str) -> TempDir {
        let tmp = TempDir::new().expect("failed to create temp dir");
        let encoded = encode_project_path(project_path);
        let project_dir = tmp.path().join("projects").join(&encoded);
        std::fs::create_dir_all(&project_dir).unwrap();
        std::fs::write(project_dir.join(format!("{session_id}.jsonl")), jsonl).unwrap();
        tmp
    }

    fn minimal_session_jsonl(session_id: &str) -> String {
        let user_line = format!(
            r#"{{"type":"user","uuid":"11111111-1111-1111-1111-111111111111","parentUuid":null,"timestamp":"2025-01-15T10:00:00.000Z","sessionId":"{session_id}","version":"2.0.74","message":{{"role":"user","content":"Hello, Claude!"}}}}"#
        );
        let assistant_line = format!(
            r#"{{"type":"assistant","uuid":"22222222-2222-2222-2222-222222222222","parentUuid":"11111111-1111-1111-1111-111111111111","timestamp":"2025-01-15T10:00:01.000Z","sessionId":"{session_id}","version":"2.0.74","message":{{"id":"msg_001","type":"message","role":"assistant","content":[{{"type":"text","text":"Hello! How can I help you today?"}}],"model":"claude-sonnet-4-20250514","stop_reason":"end_turn","usage":{{"input_tokens":10,"output_tokens":15,"cache_creation_input_tokens":0,"cache_read_input_tokens":0}}}}}}"#
        );
        format!("{user_line}\n{assistant_line}\n")
    }

    fn unwrap_output(output: ToolOutput) -> String {
        match output {
            ToolOutput::Success(result) => {
                result.content.iter().filter_map(|c| c.as_text()).collect::<Vec<_>>().join("\n")
            }
            ToolOutput::RecoverableError { message, .. } => {
                panic!("Expected success but got error: {message}");
            }
        }
    }

    fn assert_error(output: ToolOutput) {
        assert!(
            matches!(output, ToolOutput::RecoverableError { .. }),
            "Expected error but got success"
        );
    }

    fn make_server(tmp: &TempDir) -> SnatchServer {
        SnatchServer::new(Some(tmp.path().to_path_buf()), None)
    }

    #[tokio::test]
    async fn test_list_sessions_returns_fixture() {
        let sid = "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee";
        let tmp = setup_claude_dir(sid, PROJECT_PATH, &minimal_session_jsonl(sid));
        let server = make_server(&tmp);
        let text = unwrap_output(server.list_sessions(ListSessionsRequest {
            project: None, limit: None, include_subagents: None,
        }).await);
        assert!(text.contains(sid));
    }

    #[tokio::test]
    async fn test_list_sessions_project_filter() {
        let sid = "bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb";
        let tmp = setup_claude_dir(sid, PROJECT_PATH, &minimal_session_jsonl(sid));
        let server = make_server(&tmp);
        let text = unwrap_output(server.list_sessions(ListSessionsRequest {
            project: Some("test-project".to_string()), limit: None, include_subagents: None,
        }).await);
        assert!(text.contains(sid));
    }

    #[tokio::test]
    async fn test_get_session_info_valid() {
        let sid = "eeeeeeee-eeee-eeee-eeee-eeeeeeeeeeee";
        let tmp = setup_claude_dir(sid, PROJECT_PATH, &minimal_session_jsonl(sid));
        let server = make_server(&tmp);
        let text = unwrap_output(server.get_session_info(GetSessionInfoRequest {
            session_id: sid.to_string(),
        }).await);
        assert!(text.contains(sid));
    }

    #[tokio::test]
    async fn test_get_session_info_nonexistent() {
        let tmp = TempDir::new().unwrap();
        std::fs::create_dir_all(tmp.path().join("projects")).unwrap();
        let server = make_server(&tmp);
        assert_error(server.get_session_info(GetSessionInfoRequest {
            session_id: "ffffffff-ffff-ffff-ffff-ffffffffffff".to_string(),
        }).await);
    }

    #[tokio::test]
    async fn test_search_sessions_match() {
        let sid = "11111111-aaaa-bbbb-cccc-dddddddddddd";
        let tmp = setup_claude_dir(sid, PROJECT_PATH, &minimal_session_jsonl(sid));
        let server = make_server(&tmp);
        let text = unwrap_output(server.search_sessions(SearchSessionsRequest {
            pattern: "Hello, Claude!".to_string(),
            project: None, session_id: None, scope: None,
            ignore_case: None, limit: None,
        }).await);
        assert!(text.contains(sid));
    }

    #[tokio::test]
    async fn test_search_sessions_no_match() {
        let sid = "22222222-aaaa-bbbb-cccc-dddddddddddd";
        let tmp = setup_claude_dir(sid, PROJECT_PATH, &minimal_session_jsonl(sid));
        let server = make_server(&tmp);
        let text = unwrap_output(server.search_sessions(SearchSessionsRequest {
            pattern: "xyzzy_nonexistent".to_string(),
            project: None, session_id: None, scope: None,
            ignore_case: None, limit: None,
        }).await);
        assert!(!text.contains(sid));
    }

    #[tokio::test]
    async fn test_get_session_timeline() {
        let sid = "33333333-aaaa-bbbb-cccc-dddddddddddd";
        let tmp = setup_claude_dir(sid, PROJECT_PATH, &minimal_session_jsonl(sid));
        let server = make_server(&tmp);
        let text = unwrap_output(server.get_session_timeline(GetSessionTimelineRequest {
            session_id: sid.to_string(), limit: None, chain_aware: None,
        }).await);
        assert!(!text.is_empty());
    }

    #[tokio::test]
    async fn test_get_session_digest() {
        let sid = "44444444-aaaa-bbbb-cccc-dddddddddddd";
        let tmp = setup_claude_dir(sid, PROJECT_PATH, &minimal_session_jsonl(sid));
        let server = make_server(&tmp);
        let text = unwrap_output(server.get_session_digest(GetSessionDigestRequest {
            session_id: sid.to_string(), max_prompts: None, max_files: None,
        }).await);
        assert!(!text.is_empty());
    }

    #[tokio::test]
    async fn test_get_session_lessons_no_errors() {
        let sid = "55555555-aaaa-bbbb-cccc-dddddddddddd";
        let tmp = setup_claude_dir(sid, PROJECT_PATH, &minimal_session_jsonl(sid));
        let server = make_server(&tmp);
        let _text = unwrap_output(server.get_session_lessons(GetSessionLessonsRequest {
            session_id: sid.to_string(), category: None, limit: None,
        }).await);
    }

    #[tokio::test]
    async fn test_get_stats() {
        let sid = "66666666-aaaa-bbbb-cccc-dddddddddddd";
        let tmp = setup_claude_dir(sid, PROJECT_PATH, &minimal_session_jsonl(sid));
        let server = make_server(&tmp);
        let text = unwrap_output(server.get_stats(GetStatsRequest {
            session_id: Some(sid.to_string()), project: None,
        }).await);
        assert!(!text.is_empty());
    }

    #[tokio::test]
    async fn test_get_session_messages() {
        let sid = "77777777-aaaa-bbbb-cccc-dddddddddddd";
        let tmp = setup_claude_dir(sid, PROJECT_PATH, &minimal_session_jsonl(sid));
        let server = make_server(&tmp);
        let text = unwrap_output(server.get_session_messages(GetSessionMessagesRequest {
            session_id: sid.to_string(), detail: None, message_type: None,
            limit: None, offset: None, reverse: None, include_thinking: None, chain_aware: None,
            after_timestamp: None, before_timestamp: None,
        }).await);
        assert!(text.contains("Hello"));
    }

    #[tokio::test]
    async fn test_get_project_history() {
        let sid = "88888888-aaaa-bbbb-cccc-dddddddddddd";
        let tmp = setup_claude_dir(sid, PROJECT_PATH, &minimal_session_jsonl(sid));
        let server = make_server(&tmp);
        let text = unwrap_output(server.get_project_history(GetProjectHistoryRequest {
            project: "test-project".to_string(), period: Some("all".to_string()),
            limit: None, include_summaries: None,
        }).await);
        assert!(!text.is_empty());
    }

    #[tokio::test]
    async fn test_get_tool_calls() {
        let sid = "99999999-aaaa-bbbb-cccc-dddddddddddd";
        let tmp = setup_claude_dir(sid, PROJECT_PATH, &minimal_session_jsonl(sid));
        let server = make_server(&tmp);
        let _text = unwrap_output(server.get_tool_calls(GetToolCallsRequest {
            session_id: sid.to_string(), tool_filter: None, errors_only: None,
            limit: None, offset: None,
        }).await);
    }
}
