//! Prompt-boundary chunking of a conversation.
//!
//! A *chunk* is everything a prompt boundary produced — a typed user prompt
//! or a queued mid-turn steering prompt, plus all entries up to (not
//! including) the next prompt boundary on the main thread.
//! Chunks are the retrieval unit for "give me turn N of this session" — one
//! prompt, the agentic work it triggered, and the response.
//!
//! Boundary and membership policies:
//! - Harness-initiated turns (task notifications, `isMeta` wakeups) are not
//!   boundaries; their entries are absorbed into the preceding chunk.
//! - Mid-turn steering prompts (queued human messages) are human input and do
//!   start a new chunk.
//! - Abandoned rewind branches attach to the chunk containing their fork
//!   parent, as [`ChunkBranch`] summaries — never inlined into the main flow.
//! - Off-main-thread entries that are not branch material (late async tool
//!   results, progress leaves) attach to the chunk of their nearest
//!   main-thread ancestor, so a background result that lands after the next
//!   prompt still belongs to the chunk that spawned it.
//!
//! Membership covers the conversation tree (uuid-bearing entries). Uuid-less
//! sidecar metadata (`ai-title`, `mode`, `last-prompt`, snapshots) is
//! session-level, not turn content, and is not assigned to chunks.
//!
//! Used by the CLI `chunks` command, the `--chunk` selector on `messages`,
//! and the MCP `get_session_messages` `chunk` parameter.

use std::collections::HashMap;

use chrono::{DateTime, Utc};

use crate::model::message::LogEntry;
use crate::reconstruction::Conversation;

use super::extraction::{
    boundary_prompt_text, extract_user_prompt_text, is_human_prompt, is_prompt_boundary,
    queued_human_prompt,
};

/// How a chunk's opening prompt reached the conversation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PromptSource {
    /// A prompt delivered as a `user` entry (typed at a turn boundary).
    User,
    /// A mid-turn steering prompt, present only as a `queued_command`
    /// attachment (it usually never appears as a `user` entry).
    Queued,
}

impl PromptSource {
    /// Stable string form used by JSON output surfaces.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::User => "user",
            Self::Queued => "queued",
        }
    }
}

/// An abandoned branch (e.g. a rewind fork) attached to the chunk it forked
/// from.
#[derive(Debug, Clone)]
pub struct ChunkBranch {
    /// UUID of the branch root (the first off-main-thread entry of the fork).
    pub root_uuid: String,
    /// The branch root's prompt text, when the branch starts with a human
    /// prompt (the rewind-and-edit signature).
    pub prompt_text: Option<String>,
    /// UUIDs of all entries in the branch subtree, in timestamp order.
    pub uuids: Vec<String>,
}

/// One prompt-boundary chunk of a conversation.
#[derive(Debug, Clone)]
pub struct SessionChunk {
    /// Zero-based chunk index. Matches the ordering of prompt boundaries on
    /// the main thread (the same prompts `detail=overview` lists).
    pub index: usize,
    /// UUID of the human prompt that opens the chunk.
    pub prompt_uuid: String,
    /// Full text of the opening prompt (callers truncate for display).
    pub prompt_text: String,
    /// Earliest timestamp among member entries.
    pub start_ts: Option<DateTime<Utc>>,
    /// Latest timestamp among member entries (attached async results
    /// included, so this can be later than the next chunk's start).
    pub end_ts: Option<DateTime<Utc>>,
    /// Main-thread member UUIDs, in main-thread order (prompt first).
    pub main_uuids: Vec<String>,
    /// Off-main-thread members (async results, progress leaves), in
    /// timestamp order.
    pub attached_uuids: Vec<String>,
    /// Abandoned branches that forked from this chunk.
    pub branches: Vec<ChunkBranch>,
    /// Number of tool_use blocks across the chunk's assistant entries.
    pub tool_call_count: usize,
    /// Number of failed tool results (`is_error: true`) among member entries.
    pub error_count: usize,
    /// How the opening prompt reached the conversation.
    pub prompt_source: PromptSource,
}

impl SessionChunk {
    /// Total member entries (main + attached; branch entries not counted).
    #[must_use]
    pub fn entry_count(&self) -> usize {
        self.main_uuids.len() + self.attached_uuids.len()
    }
}

/// Result of chunking a conversation.
#[derive(Debug, Clone, Default)]
pub struct ChunkingResult {
    /// The chunks, in main-thread order.
    pub chunks: Vec<SessionChunk>,
    /// Tree entries before the first human prompt (hook injections, session
    /// preamble) plus anything unassignable. Empty-prompt sessions put every
    /// entry here.
    pub preamble_uuids: Vec<String>,
}

impl ChunkingResult {
    /// Number of chunks.
    #[must_use]
    pub fn len(&self) -> usize {
        self.chunks.len()
    }

    /// Whether the conversation produced no chunks (no human prompts).
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.chunks.is_empty()
    }
}

/// Split a conversation into prompt-boundary chunks.
///
/// Walks the main thread, starting a chunk at every [`is_human_prompt`]
/// entry, then assigns each off-main-thread node to the chunk of its nearest
/// main-thread ancestor (as a branch when the fork root is itself a human
/// prompt, as an attached entry otherwise). Off-main-thread roots with no
/// path to the main thread fall back to timestamp-window assignment.
#[must_use]
pub fn chunk_conversation(conversation: &Conversation) -> ChunkingResult {
    // Pass 1: chunk index for every main-thread node. None = preamble.
    let main_thread = conversation.main_thread();
    let mut main_chunk: HashMap<&str, Option<usize>> = HashMap::new();
    let mut chunks: Vec<SessionChunk> = Vec::new();
    let mut preamble_uuids: Vec<String> = Vec::new();

    for uuid in main_thread {
        let Some(node) = conversation.get_node(uuid) else {
            continue;
        };
        if is_prompt_boundary(&node.entry) {
            let prompt_source = if queued_human_prompt(&node.entry).is_some() {
                PromptSource::Queued
            } else {
                PromptSource::User
            };
            chunks.push(SessionChunk {
                index: chunks.len(),
                prompt_uuid: uuid.clone(),
                prompt_text: boundary_prompt_text(&node.entry).unwrap_or_default(),
                start_ts: None,
                end_ts: None,
                main_uuids: Vec::new(),
                attached_uuids: Vec::new(),
                branches: Vec::new(),
                tool_call_count: 0,
                error_count: 0,
                prompt_source,
            });
        }
        let idx = chunks.len().checked_sub(1);
        main_chunk.insert(uuid.as_str(), idx);
        match idx {
            Some(i) => chunks[i].main_uuids.push(uuid.clone()),
            None => preamble_uuids.push(uuid.clone()),
        }
    }

    // Prompt timestamps bound the fallback windows for unreachable nodes.
    let prompt_ts: Vec<Option<DateTime<Utc>>> = chunks
        .iter()
        .map(|c| {
            conversation
                .get_node(&c.prompt_uuid)
                .and_then(|n| n.entry.timestamp())
        })
        .collect();

    // Pass 2: place every off-main-thread node. Walk up the parent chain to
    // the nearest main-thread ancestor; the topmost off-main-thread node on
    // that path is the fork root and decides branch-vs-attached for the
    // whole subtree.
    let mut branch_members: HashMap<String, Vec<String>> = HashMap::new();
    let mut branch_chunk: HashMap<String, Option<usize>> = HashMap::new();
    for (uuid, node) in conversation.nodes() {
        if main_chunk.contains_key(uuid.as_str()) {
            continue;
        }
        let mut fork_root = uuid.clone();
        let mut cur = node;
        let placement: Option<Option<usize>> = loop {
            match cur.parent_uuid.as_deref() {
                Some(p) => {
                    if let Some(idx) = main_chunk.get(p) {
                        break Some(*idx);
                    }
                    match conversation.get_node(p) {
                        Some(parent) => {
                            fork_root = p.to_string();
                            cur = parent;
                        }
                        // Dangling parent: no path to the main thread.
                        None => break None,
                    }
                }
                // Off-main-thread root.
                None => break None,
            }
        };
        let chunk_idx = match placement {
            Some(idx) => idx,
            // Fallback: the last chunk whose prompt timestamp is <= this
            // entry's timestamp (harness writes forward in time).
            None => node
                .entry
                .timestamp()
                .and_then(|ts| prompt_ts.iter().rposition(|p| p.is_some_and(|p| p <= ts))),
        };
        let root_is_prompt = conversation
            .get_node(&fork_root)
            .is_some_and(|n| is_human_prompt(&n.entry));
        match chunk_idx {
            Some(i) if root_is_prompt => {
                branch_chunk.insert(fork_root.clone(), Some(i));
                branch_members
                    .entry(fork_root)
                    .or_default()
                    .push(uuid.clone());
            }
            Some(i) => chunks[i].attached_uuids.push(uuid.clone()),
            None if root_is_prompt => {
                branch_chunk.insert(fork_root.clone(), None);
                branch_members
                    .entry(fork_root)
                    .or_default()
                    .push(uuid.clone());
            }
            None => preamble_uuids.push(uuid.clone()),
        }
    }

    // Materialize branches on their chunks (preamble branches flatten into
    // the preamble list — there is no chunk to hang them on).
    let ts_of = |u: &String| conversation.get_node(u).and_then(|n| n.entry.timestamp());
    for (root, mut members) in branch_members {
        members.sort_by_key(ts_of);
        let target = branch_chunk.get(&root).copied().flatten();
        match target {
            Some(i) => {
                let prompt_text = conversation
                    .get_node(&root)
                    .and_then(|n| extract_user_prompt_text(&n.entry));
                chunks[i].branches.push(ChunkBranch {
                    root_uuid: root,
                    prompt_text,
                    uuids: members,
                });
            }
            None => preamble_uuids.extend(members),
        }
    }

    // Finalize per-chunk ordering, time range, and tool counts.
    for chunk in &mut chunks {
        chunk.attached_uuids.sort_by_key(ts_of);
        chunk.branches.sort_by_key(|b| ts_of(&b.root_uuid));
        let mut tool_calls = 0;
        let mut errors = 0;
        let mut start: Option<DateTime<Utc>> = None;
        let mut end: Option<DateTime<Utc>> = None;
        for uuid in chunk.main_uuids.iter().chain(&chunk.attached_uuids) {
            let Some(node) = conversation.get_node(uuid) else {
                continue;
            };
            if let Some(ts) = node.entry.timestamp() {
                start = Some(start.map_or(ts, |s| s.min(ts)));
                end = Some(end.map_or(ts, |e| e.max(ts)));
            }
            match &node.entry {
                LogEntry::Assistant(a) => {
                    tool_calls += a.message.tool_uses().len();
                }
                LogEntry::User(u) => {
                    errors += u
                        .message
                        .tool_results()
                        .iter()
                        .filter(|r| r.is_error == Some(true))
                        .count();
                }
                _ => {}
            }
        }
        chunk.start_ts = start;
        chunk.end_ts = end;
        chunk.tool_call_count = tool_calls;
        chunk.error_count = errors;
    }

    ChunkingResult {
        chunks,
        preamble_uuids,
    }
}

/// Parse a chunk selector: a single index (`"4"`) or an inclusive range
/// (`"2-5"`). Indices are zero-based.
///
/// # Errors
/// Returns a human-readable message when the spec is malformed or out of
/// bounds for `total` chunks.
pub fn parse_chunk_spec(spec: &str, total: usize) -> Result<(usize, usize), String> {
    if total == 0 {
        return Err("session has no chunks (no human prompts found)".to_string());
    }
    let spec = spec.trim();
    let parse_idx = |s: &str| {
        s.trim().parse::<usize>().map_err(|_| {
            format!("invalid chunk index '{s}' (expected a number like 4 or a range like 2-5)")
        })
    };
    let (start, end) = match spec.split_once('-') {
        Some((a, b)) => (parse_idx(a)?, parse_idx(b)?),
        None => {
            let i = parse_idx(spec)?;
            (i, i)
        }
    };
    if start > end {
        return Err(format!("chunk range {start}-{end} is reversed"));
    }
    if end >= total {
        return Err(format!(
            "chunk {end} out of range: session has {total} chunk(s), valid indices are 0-{}",
            total - 1
        ));
    }
    Ok((start, end))
}

/// Collect the entries of chunks `start..=end` for rendering.
///
/// Each chunk contributes its main-thread members in main-thread order, then
/// its attached entries in timestamp order. Branch entries are excluded
/// (surfaced as metadata only).
#[must_use]
pub fn entries_for_chunk_range<'a>(
    conversation: &'a Conversation,
    result: &ChunkingResult,
    start: usize,
    end: usize,
) -> Vec<&'a LogEntry> {
    let mut out = Vec::new();
    for chunk in result
        .chunks
        .iter()
        .skip(start)
        .take(end.saturating_sub(start) + 1)
    {
        for uuid in chunk.main_uuids.iter().chain(&chunk.attached_uuids) {
            if let Some(node) = conversation.get_node(uuid) {
                out.push(&node.entry);
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn user(uuid: &str, parent: Option<&str>, ts: &str, text: &str) -> String {
        let parent = parent.map_or("null".to_string(), |p| format!("\"{p}\""));
        format!(
            r#"{{"uuid":"{uuid}","parentUuid":{parent},"type":"user","timestamp":"{ts}","sessionId":"s","version":"2.0","isSidechain":false,"message":{{"role":"user","content":{}}}}}"#,
            serde_json::to_string(text).unwrap()
        )
    }

    fn tool_result(uuid: &str, parent: &str, ts: &str) -> String {
        format!(
            r#"{{"uuid":"{uuid}","parentUuid":"{parent}","type":"user","timestamp":"{ts}","sessionId":"s","version":"2.0","isSidechain":false,"message":{{"role":"user","content":[{{"type":"tool_result","tool_use_id":"t1","content":"done"}}]}}}}"#
        )
    }

    fn error_tool_result(uuid: &str, parent: &str, ts: &str) -> String {
        format!(
            r#"{{"uuid":"{uuid}","parentUuid":"{parent}","type":"user","timestamp":"{ts}","sessionId":"s","version":"2.0","isSidechain":false,"message":{{"role":"user","content":[{{"type":"tool_result","tool_use_id":"t1","is_error":true,"content":"command failed"}}]}}}}"#
        )
    }

    fn assistant(uuid: &str, parent: &str, ts: &str, text: &str) -> String {
        format!(
            r#"{{"uuid":"{uuid}","parentUuid":"{parent}","type":"assistant","timestamp":"{ts}","sessionId":"s","version":"2.0","isSidechain":false,"message":{{"id":"m-{uuid}","type":"message","role":"assistant","model":"claude","content":[{{"type":"text","text":"{text}"}}]}}}}"#
        )
    }

    fn queued_attachment(uuid: &str, parent: &str, ts: &str, prompt: &str, mode: &str) -> String {
        format!(
            r#"{{"uuid":"{uuid}","parentUuid":"{parent}","type":"attachment","timestamp":"{ts}","sessionId":"s","isSidechain":false,"attachment":{{"type":"queued_command","commandMode":"{mode}","origin":{{"kind":"{}"}},"prompt":{}}}}}"#,
            if mode == "prompt" { "human" } else { "harness" },
            serde_json::to_string(prompt).unwrap()
        )
    }

    fn conv(lines: &[String]) -> Conversation {
        let entries = lines
            .iter()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        Conversation::from_entries(entries).unwrap()
    }

    #[test]
    fn test_two_prompts_two_chunks() {
        let c = conv(&[
            user("u1", None, "2026-01-01T00:00:00Z", "first task"),
            assistant("a1", "u1", "2026-01-01T00:00:01Z", "done first"),
            user("u2", Some("a1"), "2026-01-01T00:00:02Z", "second task"),
            assistant("a2", "u2", "2026-01-01T00:00:03Z", "done second"),
        ]);
        let r = chunk_conversation(&c);
        assert_eq!(r.len(), 2);
        assert_eq!(r.chunks[0].prompt_text, "first task");
        assert_eq!(r.chunks[0].main_uuids, vec!["u1", "a1"]);
        assert_eq!(r.chunks[1].main_uuids, vec!["u2", "a2"]);
        assert!(r.preamble_uuids.is_empty());
    }

    #[test]
    fn test_task_notification_absorbed() {
        // A harness task notification mid-chain must not open a chunk.
        let c = conv(&[
            user("u1", None, "2026-01-01T00:00:00Z", "start the scan"),
            assistant("a1", "u1", "2026-01-01T00:00:01Z", "launched"),
            user(
                "n1",
                Some("a1"),
                "2026-01-01T00:05:00Z",
                "<task-notification>\n<task-id>x</task-id>\n</task-notification>",
            ),
            assistant("a2", "n1", "2026-01-01T00:05:01Z", "scan finished"),
        ]);
        let r = chunk_conversation(&c);
        assert_eq!(r.len(), 1);
        assert_eq!(r.chunks[0].main_uuids, vec!["u1", "a1", "n1", "a2"]);
    }

    #[test]
    fn test_rewind_branch_attaches_to_fork_chunk() {
        // u2a is an abandoned rewind (leaf, earlier activity); u2b is the
        // live branch (later activity wins the main thread).
        let c = conv(&[
            user("u1", None, "2026-01-01T00:00:00Z", "task"),
            assistant("a1", "u1", "2026-01-01T00:00:01Z", "reply"),
            user("u2a", Some("a1"), "2026-01-01T00:01:00Z", "typo promt"),
            assistant("a2a", "u2a", "2026-01-01T00:01:01Z", "answer to typo"),
            user("u2b", Some("a1"), "2026-01-01T00:02:00Z", "fixed prompt"),
            assistant("a2b", "u2b", "2026-01-01T00:02:01Z", "answer to fixed"),
        ]);
        let r = chunk_conversation(&c);
        assert_eq!(r.len(), 2, "abandoned branch prompt must not open a chunk");
        assert_eq!(r.chunks[1].prompt_text, "fixed prompt");
        assert_eq!(r.chunks[0].branches.len(), 1);
        let b = &r.chunks[0].branches[0];
        assert_eq!(b.root_uuid, "u2a");
        assert_eq!(b.prompt_text.as_deref(), Some("typo promt"));
        assert_eq!(b.uuids, vec!["u2a", "a2a"]);
    }

    #[test]
    fn test_async_result_attaches_to_spawning_chunk() {
        // A background tool result lands after the next prompt but forks off
        // chunk 0's assistant — it must belong to chunk 0.
        let c = conv(&[
            user("u1", None, "2026-01-01T00:00:00Z", "run it in background"),
            assistant("a1", "u1", "2026-01-01T00:00:01Z", "running"),
            assistant("a2", "a1", "2026-01-01T00:00:02Z", "moving on"),
            user("u2", Some("a2"), "2026-01-01T00:01:00Z", "next task"),
            assistant("a3", "u2", "2026-01-01T00:01:01Z", "ok"),
            // The async result is the session's LAST event — the hardest
            // shape: reconstruction must not let it steal the main-thread
            // tail, and chunking must attach it to the spawning chunk.
            tool_result("tr1", "a1", "2026-01-01T00:06:00Z"),
        ]);
        let r = chunk_conversation(&c);
        assert_eq!(r.len(), 2);
        assert_eq!(r.chunks[0].attached_uuids, vec!["tr1"]);
        assert!(r.chunks[1].attached_uuids.is_empty());
        // The late result stretches the chunk's end past the next chunk's start.
        assert!(r.chunks[0].end_ts.unwrap() > r.chunks[1].start_ts.unwrap());
    }

    #[test]
    fn test_queued_steering_prompt_starts_chunk() {
        // A mid-turn steering prompt exists only as a queued_command
        // attachment (corpus scans found queued prompts that never recur
        // as user entries); it must open a chunk, marked as Queued.
        let c = conv(&[
            user("u1", None, "2026-01-01T00:00:00Z", "start the work"),
            assistant("a1", "u1", "2026-01-01T00:00:01Z", "working"),
            queued_attachment(
                "q1",
                "a1",
                "2026-01-01T00:00:30Z",
                "wait, use the other approach",
                "prompt",
            ),
            assistant("a2", "q1", "2026-01-01T00:00:31Z", "switching approach"),
        ]);
        let r = chunk_conversation(&c);
        assert_eq!(r.len(), 2);
        assert_eq!(r.chunks[0].prompt_source, PromptSource::User);
        assert_eq!(r.chunks[1].prompt_source, PromptSource::Queued);
        assert_eq!(r.chunks[1].prompt_text, "wait, use the other approach");
        assert_eq!(r.chunks[1].main_uuids, vec!["q1", "a2"]);
    }

    #[test]
    fn test_machine_queued_command_is_not_a_boundary() {
        // task-notification queued_commands are machine traffic — absorbed.
        let c = conv(&[
            user("u1", None, "2026-01-01T00:00:00Z", "start the work"),
            assistant("a1", "u1", "2026-01-01T00:00:01Z", "working"),
            queued_attachment(
                "q1",
                "a1",
                "2026-01-01T00:00:30Z",
                "<task-notification>done</task-notification>",
                "task-notification",
            ),
            assistant("a2", "q1", "2026-01-01T00:00:31Z", "noted"),
        ]);
        let r = chunk_conversation(&c);
        assert_eq!(r.len(), 1);
        assert_eq!(r.chunks[0].main_uuids, vec!["u1", "a1", "q1", "a2"]);
    }

    #[test]
    fn test_preamble_before_first_prompt() {
        let c = conv(&[
            user(
                "h1",
                None,
                "2026-01-01T00:00:00Z",
                "<system-reminder>hook output</system-reminder>",
            ),
            user("u1", Some("h1"), "2026-01-01T00:00:01Z", "real task"),
            assistant("a1", "u1", "2026-01-01T00:00:02Z", "reply"),
        ]);
        let r = chunk_conversation(&c);
        assert_eq!(r.len(), 1);
        assert_eq!(r.preamble_uuids, vec!["h1"]);
        assert_eq!(r.chunks[0].main_uuids, vec!["u1", "a1"]);
    }

    #[test]
    fn test_no_prompts_no_chunks() {
        let c = conv(&[assistant(
            "a1",
            "x-missing",
            "2026-01-01T00:00:00Z",
            "orphan reply",
        )]);
        let r = chunk_conversation(&c);
        assert!(r.is_empty());
        assert_eq!(r.preamble_uuids, vec!["a1"]);
    }

    #[test]
    fn test_entries_for_chunk_range() {
        let c = conv(&[
            user("u1", None, "2026-01-01T00:00:00Z", "one"),
            assistant("a1", "u1", "2026-01-01T00:00:01Z", "r1"),
            user("u2", Some("a1"), "2026-01-01T00:00:02Z", "two"),
            assistant("a2", "u2", "2026-01-01T00:00:03Z", "r2"),
            user("u3", Some("a2"), "2026-01-01T00:00:04Z", "three"),
        ]);
        let r = chunk_conversation(&c);
        assert_eq!(r.len(), 3);
        let uuids: Vec<_> = entries_for_chunk_range(&c, &r, 1, 2)
            .iter()
            .filter_map(|e| e.uuid())
            .collect();
        assert_eq!(uuids, vec!["u2", "a2", "u3"]);
    }

    #[test]
    fn test_error_count_per_chunk() {
        // Failed tool results are counted per chunk (attached members too),
        // so audits know where to aim full-detail drill-downs.
        let c = conv(&[
            user("u1", None, "2026-01-01T00:00:00Z", "task one"),
            assistant("a1", "u1", "2026-01-01T00:00:01Z", "running"),
            error_tool_result("e1", "a1", "2026-01-01T00:00:02Z"),
            assistant("a2", "e1", "2026-01-01T00:00:03Z", "retrying"),
            tool_result("ok1", "a2", "2026-01-01T00:00:04Z"),
            assistant("a3", "ok1", "2026-01-01T00:00:05Z", "done"),
            user("u2", Some("a3"), "2026-01-01T00:01:00Z", "task two"),
            assistant("a4", "u2", "2026-01-01T00:01:01Z", "clean"),
        ]);
        let r = chunk_conversation(&c);
        assert_eq!(r.len(), 2);
        assert_eq!(r.chunks[0].error_count, 1);
        assert_eq!(r.chunks[1].error_count, 0);
    }

    #[test]
    fn test_overview_predicate_matches_chunk_boundaries() {
        // detail=overview retains main-thread entries via is_prompt_boundary;
        // chunk indices must always line up with that listing.
        let c = conv(&[
            user("u1", None, "2026-01-01T00:00:00Z", "one"),
            assistant("a1", "u1", "2026-01-01T00:00:01Z", "r1"),
            queued_attachment("q1", "a1", "2026-01-01T00:00:30Z", "steer!", "prompt"),
            assistant("a2", "q1", "2026-01-01T00:00:31Z", "r2"),
            user("u2", Some("a2"), "2026-01-01T00:01:00Z", "two"),
        ]);
        let r = chunk_conversation(&c);
        let overview_uuids: Vec<&str> = c
            .main_thread_entries()
            .iter()
            .filter(|e| is_prompt_boundary(e))
            .filter_map(|e| e.uuid())
            .collect();
        let chunk_uuids: Vec<&str> = r.chunks.iter().map(|c| c.prompt_uuid.as_str()).collect();
        assert_eq!(overview_uuids, chunk_uuids);
        assert_eq!(chunk_uuids, vec!["u1", "q1", "u2"]);
    }

    #[test]
    fn test_parse_chunk_spec() {
        assert_eq!(parse_chunk_spec("4", 10), Ok((4, 4)));
        assert_eq!(parse_chunk_spec("2-5", 10), Ok((2, 5)));
        assert_eq!(parse_chunk_spec(" 0 - 9 ", 10), Ok((0, 9)));
        assert!(parse_chunk_spec("5-2", 10).is_err());
        assert!(parse_chunk_spec("10", 10).is_err());
        assert!(parse_chunk_spec("x", 10).is_err());
        assert!(parse_chunk_spec("0", 0).is_err());
    }
}
