//! Matching subagent transcripts to the `Agent`/`Task` call that spawned them.
//!
//! Subagents are written to separate `agent-*.jsonl` files and are invisible from
//! the parent transcript, which records only the spawning tool_use. This module
//! joins each spawn call to its subagent so callers can surface the work attached
//! to the call. The output is rendering-agnostic (used by both the CLI and the MCP
//! server); each surface formats the result and, if desired, the full transcript.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use crate::discovery::Session;
use crate::model::message::LogEntry;
use crate::reconstruction::Conversation;

use super::extraction::extract_assistant_summary;

/// A subagent matched to its spawning `Agent`/`Task` call.
#[derive(Debug, Clone)]
pub struct SubagentMatch {
    /// Subagent session id (`agent-<hash>`); query it for the full transcript.
    pub session_id: String,
    /// Path to the subagent transcript file.
    pub path: PathBuf,
    /// Agent type from the sidecar (e.g. "Explore").
    pub agent_type: Option<String>,
    /// Spawn description from the sidecar.
    pub description: Option<String>,
    /// Spawning tool_use id, when the sidecar records it.
    pub tool_use_id: Option<String>,
    /// Preview of the subagent's final assistant message (its result), truncated.
    pub result_preview: Option<String>,
    /// User + assistant message count in the subagent transcript.
    pub message_count: Option<usize>,
}

/// Match each `Agent`/`Task` call in `ordered_entries` to the subagent it spawned,
/// keyed by the spawning tool_use id.
///
/// Two passes, conservative by design: first the exact sidecar `toolUseId` link
/// (always correct, but only newer Claude Code records it), then a description
/// fallback that attaches only when exactly one unused subagent carries that
/// description. Ambiguous descriptions are left unattached rather than guessed.
#[must_use]
pub fn match_subagents(
    session: &Session,
    ordered_entries: &[&LogEntry],
    max_file_size: Option<u64>,
) -> HashMap<String, SubagentMatch> {
    let mut out = HashMap::new();
    let links = session.subagent_links();
    if links.is_empty() {
        return out;
    }

    // Agent/Task calls in spawn order: (tool_use id, description).
    let mut agent_calls: Vec<(String, Option<String>)> = Vec::new();
    for entry in ordered_entries {
        if let LogEntry::Assistant(a) = entry {
            for tu in a.message.tool_uses() {
                if tu.name == "Agent" || tu.name == "Task" {
                    let desc = tu
                        .input
                        .get("description")
                        .and_then(|v| v.as_str())
                        .map(String::from);
                    agent_calls.push((tu.id.clone(), desc));
                }
            }
        }
    }
    if agent_calls.is_empty() {
        return out;
    }

    let by_id: HashMap<&str, &crate::discovery::SubagentLink> = links
        .iter()
        .filter_map(|l| l.tool_use_id.as_deref().map(|id| (id, l)))
        .collect();
    let mut by_desc: HashMap<&str, Vec<&crate::discovery::SubagentLink>> = HashMap::new();
    for l in &links {
        if let Some(d) = l.description.as_deref() {
            by_desc.entry(d).or_default().push(l);
        }
    }

    let mut used: HashSet<String> = HashSet::new();
    let project = session.project_path();

    // Pass 1: exact tool_use_id matches.
    for (id, _) in &agent_calls {
        if let Some(link) = by_id.get(id.as_str()) {
            if used.insert(link.agent_session_id.clone()) {
                out.insert(id.clone(), build_match(link, project, max_file_size));
            }
        }
    }
    // Pass 2: unique-description fallback for still-unmatched calls.
    for (id, desc) in &agent_calls {
        if out.contains_key(id) {
            continue;
        }
        let Some(d) = desc.as_deref() else { continue };
        let Some(cands) = by_desc.get(d) else {
            continue;
        };
        let avail: Vec<_> = cands
            .iter()
            .filter(|l| !used.contains(&l.agent_session_id))
            .collect();
        if avail.len() == 1 {
            let link = avail[0];
            if used.insert(link.agent_session_id.clone()) {
                out.insert(id.clone(), build_match(link, project, max_file_size));
            }
        }
    }
    out
}

/// Parse a matched subagent transcript to compute its result preview and message
/// count. Falls back to no preview/count if the transcript can't be read.
fn build_match(
    link: &crate::discovery::SubagentLink,
    project_path: &str,
    max_file_size: Option<u64>,
) -> SubagentMatch {
    let session = Session::from_path(&link.path, project_path).ok();

    // Message count from quick metadata so it agrees with `get_session_info` /
    // `snatch info` (which dedup assistant streaming chunks by message id).
    let message_count = session
        .as_ref()
        .and_then(|s| s.quick_metadata_cached().ok())
        .map(|m| m.user_count + m.assistant_count);

    // Result preview: the subagent's final assistant message.
    let entries = session
        .and_then(|s| s.parse_with_options(max_file_size).ok())
        .unwrap_or_default();
    let conversation = Conversation::from_entries(entries).ok();
    let result_preview = conversation
        .as_ref()
        .map(Conversation::main_thread_entries)
        .unwrap_or_default()
        .iter()
        .rev()
        .find(|e| matches!(e, LogEntry::Assistant(_)))
        .and_then(|e| extract_assistant_summary(e, 500));

    SubagentMatch {
        session_id: link.agent_session_id.clone(),
        path: link.path.clone(),
        agent_type: link.agent_type.clone(),
        description: link.description.clone(),
        tool_use_id: link.tool_use_id.clone(),
        result_preview,
        message_count,
    }
}
