//! Shared helper functions for MCP server tools.
//!
//! Most analytical functions are delegated to [`crate::analysis`].
//! This module provides MCP-specific wiring (session resolution, ToolOutput errors)
//! and re-exports analysis functions for backward compatibility.

use mcpkit::prelude::ToolOutput;

use crate::analytics::SessionAnalytics;
use crate::discovery::ClaudeDirectory;
use crate::model::message::LogEntry;
use crate::reconstruction::Conversation;

use super::SnatchServer;

// Re-export analysis functions so `use helpers::*` in mod.rs continues to work.
pub use crate::analysis::extraction::{
    extract_assistant_summary, extract_error_preview, extract_files_from_tools,
    extract_thinking_text, extract_tool_input_summary, extract_tool_names,
    extract_user_prompt_text, find_compaction_events, get_model, has_thinking, has_tool_errors,
    truncate_text,
};
pub use crate::analysis::filters::{parse_period, period_cutoff};

/// Resolved session with parsed conversation and analytics.
pub struct ResolvedSession {
    pub session_id: String,
    pub project_path: String,
    pub conversation: Conversation,
    pub analytics: SessionAnalytics,
}

/// Resolve a session ID to a parsed conversation.
pub fn resolve_session(server: &SnatchServer, session_id: &str) -> Result<ResolvedSession, ToolOutput> {
    let claude_dir = server.get_claude_dir().map_err(ToolOutput::error)?;

    let session = claude_dir
        .find_session(session_id)
        .map_err(|e| ToolOutput::error(format!("Failed to find session: {e}")))?
        .ok_or_else(|| ToolOutput::error(format!("Session not found: {session_id}")))?;

    let entries = session
        .parse_with_options(server.max_file_size)
        .map_err(|e| ToolOutput::error(format!("Failed to parse session: {e}")))?;

    let conversation = Conversation::from_entries(entries)
        .map_err(|e| ToolOutput::error(format!("Failed to reconstruct conversation: {e}")))?;

    let analytics = SessionAnalytics::from_conversation(&conversation);

    Ok(ResolvedSession {
        session_id: session.session_id().to_string(),
        project_path: session.project_path().to_string(),
        conversation,
        analytics,
    })
}

/// Get the Claude directory from the server config.
pub fn get_claude_dir(server: &SnatchServer) -> Result<ClaudeDirectory, ToolOutput> {
    server.get_claude_dir().map_err(ToolOutput::error)
}

/// Search a single entry for a regex pattern match.
/// Returns the matched text and surrounding context if found.
pub fn search_entry_text(
    entry: &LogEntry,
    regex: &regex::Regex,
    scope: &str,
    max_context: usize,
) -> Vec<(String, String)> {
    crate::analysis::search::search_entry_text(entry, regex, scope, max_context)
}
