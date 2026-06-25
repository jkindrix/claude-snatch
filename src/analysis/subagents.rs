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

/// Result of joining subagent transcripts to their spawning `Agent`/`Task` calls.
///
/// `matched` keys each joined subagent by the spawning tool_use id. `unmatched`
/// holds subagents that are present on disk but could not be confidently joined
/// to a single call — callers MUST still surface these (as an "unlinked" marker)
/// so a present subagent never silently vanishes from the rendered output.
#[derive(Debug, Clone, Default)]
pub struct SubagentMatches {
    /// Subagents joined to a spawning call, keyed by the spawning tool_use id.
    pub matched: HashMap<String, SubagentMatch>,
    /// Subagents present on disk but not confidently joined to one call.
    pub unmatched: Vec<SubagentMatch>,
}

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

/// Match each `Agent`/`Task` call in `ordered_entries` to the subagent it spawned.
///
/// Returns both the confident joins (keyed by the spawning tool_use id) and any
/// subagents that are present on disk but could not be confidently joined.
///
/// Joins run conservatively, in order of decreasing confidence:
/// 1. exact sidecar `toolUseId` link (always correct, but only newer Claude Code
///    records it);
/// 2. a description fallback that attaches only when exactly one unused subagent
///    carries that description;
/// 3. a single-spawn fallback that attaches when exactly one call and exactly one
///    subagent remain unmatched — the common case where the sidecar carries no
///    join keys at all (no `meta.json`).
///
/// Anything still unmatched (ambiguous descriptions, or several key-less subagents
/// in one turn) is returned in `unmatched` rather than guessed, so callers can
/// always emit a marker and indices never jump unexplained.
#[must_use]
pub fn match_subagents(
    session: &Session,
    ordered_entries: &[&LogEntry],
    max_file_size: Option<u64>,
) -> SubagentMatches {
    let mut result = SubagentMatches::default();
    let links = session.subagent_links();
    if links.is_empty() {
        return result;
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

    let project = session.project_path();
    let mut out: HashMap<String, SubagentMatch> = HashMap::new();
    let mut used: HashSet<String> = HashSet::new();

    if !agent_calls.is_empty() {
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
        // Pass 3: single-spawn fallback. When exactly one call and exactly one
        // subagent are still unmatched, they must be each other (the common
        // key-less single-spawn case). More than one of either side stays
        // ambiguous and falls through to the unlinked-marker path below.
        let unmatched_calls: Vec<&String> = agent_calls
            .iter()
            .filter(|(id, _)| !out.contains_key(id))
            .map(|(id, _)| id)
            .collect();
        let unused_links: Vec<&crate::discovery::SubagentLink> = links
            .iter()
            .filter(|l| !used.contains(&l.agent_session_id))
            .collect();
        if unmatched_calls.len() == 1 && unused_links.len() == 1 {
            let id = unmatched_calls[0];
            let link = unused_links[0];
            used.insert(link.agent_session_id.clone());
            out.insert(id.clone(), build_match(link, project, max_file_size));
        }
    }

    // Any subagent present on disk but not joined to a call: surface it as
    // unmatched so a present subagent is never silently dropped.
    for link in &links {
        if !used.contains(&link.agent_session_id) {
            result
                .unmatched
                .push(build_match(link, project, max_file_size));
        }
    }

    result.matched = out;
    result
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    const TS: &str = "2026-01-01T00:00:00Z";

    /// One Agent/Task tool_use as a JSONL assistant line.
    fn agent_call_line(uuid: &str, parent: Option<&str>, tool_id: &str, desc: &str) -> String {
        let parent_field = match parent {
            Some(p) => format!("\"{p}\""),
            None => "null".to_string(),
        };
        format!(
            "{{\"type\":\"assistant\",\"uuid\":\"{uuid}\",\"parentUuid\":{parent_field},\
             \"timestamp\":\"{TS}\",\"sessionId\":\"sess\",\"version\":\"1.0.0\",\
             \"message\":{{\"id\":\"msg_{uuid}\",\"type\":\"message\",\"role\":\"assistant\",\
             \"model\":\"claude-test\",\"content\":[{{\"type\":\"tool_use\",\
             \"id\":\"{tool_id}\",\"name\":\"Task\",\"input\":{{\"description\":\"{desc}\"}}}}]}}}}"
        )
    }

    /// A subagent transcript with one assistant text message (so a result preview
    /// and message count can be computed).
    fn write_subagent(dir: &Path, stem: &str, text: &str, meta: Option<&str>) {
        let line = format!(
            "{{\"type\":\"assistant\",\"uuid\":\"{stem}-u\",\"parentUuid\":null,\
             \"timestamp\":\"{TS}\",\"sessionId\":\"{stem}\",\"version\":\"1.0.0\",\
             \"message\":{{\"id\":\"msg_{stem}\",\"type\":\"message\",\"role\":\"assistant\",\
             \"model\":\"claude-test\",\"content\":[{{\"type\":\"text\",\
             \"text\":\"{text}\"}}]}}}}\n"
        );
        std::fs::write(dir.join(format!("{stem}.jsonl")), line).unwrap();
        if let Some(m) = meta {
            std::fs::write(dir.join(format!("{stem}.meta.json")), m).unwrap();
        }
    }

    /// Build a parent session from JSONL lines plus an optional set of subagent
    /// transcripts, returning the parsed session and its entries.
    fn fixture(
        lines: &[String],
        subagents: &[(&str, &str, Option<&str>)],
    ) -> (tempfile::TempDir, Session, Vec<LogEntry>) {
        let tmp = tempfile::tempdir().unwrap();
        let project = tmp.path();
        let uuid = "11111111-2222-3333-4444-555555555555";
        let main_path = project.join(format!("{uuid}.jsonl"));
        std::fs::write(&main_path, format!("{}\n", lines.join("\n"))).unwrap();

        let dir = project.join(uuid).join("subagents");
        std::fs::create_dir_all(&dir).unwrap();
        for (stem, text, meta) in subagents {
            write_subagent(&dir, stem, text, *meta);
        }

        let session = Session::from_path(&main_path, "/tmp/project").unwrap();
        let entries = session.parse_with_options(None).unwrap();
        (tmp, session, entries)
    }

    #[test]
    fn meta_tool_use_id_match_still_works() {
        let line = agent_call_line("a", None, "toolu_exact", "Do a thing");
        let meta =
            r#"{"agentType":"Explore","description":"Do a thing","toolUseId":"toolu_exact"}"#;
        let (_tmp, session, entries) = fixture(&[line], &[("agent-aaa", "all done", Some(meta))]);
        let refs: Vec<&LogEntry> = entries.iter().collect();

        let res = match_subagents(&session, &refs, None);
        assert_eq!(res.matched.len(), 1);
        assert!(res.unmatched.is_empty());
        let m = res
            .matched
            .get("toolu_exact")
            .expect("matched by tool_use_id");
        assert_eq!(m.session_id, "agent-aaa");
    }

    #[test]
    fn meta_description_match_still_works() {
        let line = agent_call_line("a", None, "toolu_x", "Review code");
        // Sidecar carries description but no toolUseId (the common partial case).
        let meta = r#"{"agentType":"general-purpose","description":"Review code"}"#;
        let (_tmp, session, entries) = fixture(&[line], &[("agent-bbb", "reviewed", Some(meta))]);
        let refs: Vec<&LogEntry> = entries.iter().collect();

        let res = match_subagents(&session, &refs, None);
        assert_eq!(res.matched.len(), 1);
        assert!(res.unmatched.is_empty());
        assert_eq!(res.matched.get("toolu_x").unwrap().session_id, "agent-bbb");
    }

    #[test]
    fn single_pair_fallback_links_meta_less_subagent() {
        // One Agent/Task call, one subagent, NO meta.json: must still link.
        let line = agent_call_line("a", None, "toolu_only", "Explore codebase");
        let (_tmp, session, entries) =
            fixture(&[line], &[("agent-ccc", "comprehensive report", None)]);
        let refs: Vec<&LogEntry> = entries.iter().collect();

        let res = match_subagents(&session, &refs, None);
        assert_eq!(res.matched.len(), 1, "single-spawn must link via fallback");
        assert!(res.unmatched.is_empty());
        let m = res.matched.get("toolu_only").unwrap();
        assert_eq!(m.session_id, "agent-ccc");
        assert!(m.result_preview.is_some());
    }

    #[test]
    fn multiple_meta_less_subagents_fall_to_unlinked_not_guessed() {
        // Two calls, two meta-less subagents: ambiguous -> no guess, both unlinked.
        let l1 = agent_call_line("a", None, "toolu_1", "First");
        let l2 = agent_call_line("b", Some("a"), "toolu_2", "Second");
        let (_tmp, session, entries) = fixture(
            &[l1, l2],
            &[("agent-ddd", "one", None), ("agent-eee", "two", None)],
        );
        let refs: Vec<&LogEntry> = entries.iter().collect();

        let res = match_subagents(&session, &refs, None);
        assert!(
            res.matched.is_empty(),
            "ambiguous pairs must not be guessed"
        );
        assert_eq!(res.unmatched.len(), 2, "both surface as unlinked markers");
        let mut ids: Vec<&str> = res
            .unmatched
            .iter()
            .map(|m| m.session_id.as_str())
            .collect();
        ids.sort_unstable();
        assert_eq!(ids, ["agent-ddd", "agent-eee"]);
    }

    #[test]
    fn present_subagent_with_no_spawn_call_is_unmatched_not_dropped() {
        // A subagent on disk but no Agent/Task call at all: must be surfaced.
        let (_tmp, session, entries) = fixture(&[], &[("agent-fff", "orphan work", None)]);
        let refs: Vec<&LogEntry> = entries.iter().collect();

        let res = match_subagents(&session, &refs, None);
        assert!(res.matched.is_empty());
        assert_eq!(res.unmatched.len(), 1);
        assert_eq!(res.unmatched[0].session_id, "agent-fff");
        assert_eq!(res.unmatched[0].message_count, Some(1));
    }
}
