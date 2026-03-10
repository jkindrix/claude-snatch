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
    is_human_prompt, truncate_text,
};
pub use crate::analysis::filters::{parse_period, period_cutoff};

/// Resolved session with parsed conversation and analytics.
pub struct ResolvedSession {
    /// The full session UUID.
    pub session_id: String,
    /// Decoded project path for this session.
    pub project_path: String,
    /// Reconstructed conversation tree.
    pub conversation: Conversation,
    /// Computed analytics for the session.
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

/// Resolve a session ID to a chain-aware parsed conversation.
///
/// If `chain_aware` is true and the session belongs to a chain, parses all chain
/// members into a unified conversation. The returned `session_id` is the root ID.
/// Otherwise behaves identically to `resolve_session`.
pub fn resolve_session_with_chain(
    server: &SnatchServer,
    session_id: &str,
    chain_aware: bool,
) -> Result<ResolvedSession, ToolOutput> {
    if !chain_aware {
        return resolve_session(server, session_id);
    }

    let claude_dir = server.get_claude_dir().map_err(ToolOutput::error)?;

    let session = claude_dir
        .find_session(session_id)
        .map_err(|e| ToolOutput::error(format!("Failed to find session: {e}")))?
        .ok_or_else(|| ToolOutput::error(format!("Session not found: {session_id}")))?;

    let project_path = session.project_path().to_string();
    let file_session_id = session.session_id().to_string();

    // Find the project so we can detect chains
    let projects = claude_dir
        .projects()
        .map_err(|e| ToolOutput::error(format!("Failed to list projects: {e}")))?;

    let project = projects
        .into_iter()
        .find(|p| p.best_path() == project_path || p.decoded_path() == project_path);

    if let Some(project) = project {
        let chains = project
            .session_chains()
            .map_err(|e| ToolOutput::error(format!("Failed to detect chains: {e}")))?;

        // Check if this session belongs to any chain
        for chain in chains.values() {
            if chain.contains(&file_session_id) {
                // Parse the full chain
                let entries = project
                    .parse_chain(chain)
                    .map_err(|e| ToolOutput::error(format!("Failed to parse chain: {e}")))?;

                let conversation = Conversation::from_entries(entries)
                    .map_err(|e| ToolOutput::error(format!("Failed to reconstruct chain: {e}")))?;

                let analytics = SessionAnalytics::from_conversation(&conversation);

                return Ok(ResolvedSession {
                    session_id: chain.root_id.clone(),
                    project_path,
                    conversation,
                    analytics,
                });
            }
        }
    }

    // Not part of a chain — fall back to single-file resolution
    resolve_session(server, session_id)
}

/// Get the Claude directory from the server config.
pub fn get_claude_dir(server: &SnatchServer) -> Result<ClaudeDirectory, ToolOutput> {
    server.get_claude_dir().map_err(ToolOutput::error)
}

/// Resolved project directory for goal operations.
pub struct ResolvedProject {
    /// The decoded project path (e.g., "/home/user/myproject").
    pub project_path: String,
    /// The project directory under ~/.claude/projects/.
    pub project_dir: std::path::PathBuf,
}

/// Resolve a project filter string to a project directory.
///
/// Uses substring match against decoded project paths. Returns error if
/// no match or ambiguous (multiple matches).
pub fn resolve_project(server: &SnatchServer, project_filter: &str) -> Result<ResolvedProject, ToolOutput> {
    let claude_dir = server.get_claude_dir().map_err(ToolOutput::error)?;
    let projects = claude_dir
        .projects()
        .map_err(|e| ToolOutput::error(format!("Failed to list projects: {e}")))?;

    let matches = crate::cli::helpers::filter_projects(projects, project_filter);

    match matches.len() {
        0 => Err(ToolOutput::error(format!(
            "No project matching '{project_filter}'"
        ))),
        1 => Ok(ResolvedProject {
            project_path: matches[0].decoded_path().to_string(),
            project_dir: matches[0].path().to_path_buf(),
        }),
        n => {
            let names: Vec<_> = matches.iter().map(|p| p.decoded_path()).collect();
            Err(ToolOutput::error(format!(
                "Ambiguous project filter '{project_filter}' matches {n} projects: {}",
                names.join(", ")
            )))
        }
    }
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
