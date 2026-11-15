//! Session chain detection and management.
//!
//! When a user resumes a Claude Code session (`claude --resume` or `claude --continue`),
//! Claude Code creates a new JSONL file with a different UUID but sets the internal
//! `sessionId` field to the previous file's UUID. This creates a chain:
//!
//! ```text
//! root.jsonl (file_uuid == sessionId)
//!   → cont1.jsonl (sessionId == root's UUID)
//!     → cont2.jsonl (sessionId == cont1's UUID)
//! ```
//!
//! This module detects these chains by reading the first few lines of each JSONL
//! file and comparing the internal `sessionId` to the filename UUID.
//!
//! Since Claude Code 2.1.319 there is a second kind of link. When a session is
//! handed off to a new session file, the old file gets a `continued-in` record
//! naming its successor, and the successor begins with a copy of the old
//! file's entries (same UUIDs, re-stamped with the successor's own id). The
//! successor carries no pointer back, so that link is read from the old
//! file's tail. Parsing a chain keeps the first copy of each UUID.

use std::collections::HashMap;
use std::io::{BufRead, BufReader};
use std::path::Path;

use chrono::{DateTime, Utc};
use tracing::{debug, trace, warn};

use crate::error::Result;
use crate::model::LogEntry;
use crate::parser::JsonlParser;

/// A chain of session files forming one logical conversation.
#[derive(Debug, Clone)]
pub struct SessionChain {
    /// The root session's file UUID (the original session).
    pub root_id: String,
    /// All file UUIDs in chronological order, including the root.
    pub members: Vec<ChainMember>,
    /// Human-readable slug (stable across the chain).
    pub slug: Option<String>,
}

/// A single file within a session chain.
#[derive(Debug, Clone)]
pub struct ChainMember {
    /// This file's UUID (from the filename).
    pub file_id: String,
    /// The file this one continues: the sessionId field from the JSONL, or
    /// the file whose `continued-in` record names this one.
    /// Equal to file_id for root sessions.
    pub internal_session_id: String,
    /// Slug from the first entry.
    pub slug: Option<String>,
    /// Timestamp of the first timestamped entry.
    pub started: Option<DateTime<Utc>>,
}

/// Extract the internal `sessionId` and `slug` from the first few lines of a JSONL file.
///
/// Reads at most `max_lines` to find the first entry with a `sessionId` field.
/// Returns `(sessionId, slug, timestamp)` if found.
pub(crate) fn extract_session_link(
    path: &Path,
    max_lines: usize,
) -> Option<(String, Option<String>, Option<DateTime<Utc>>)> {
    let file = std::fs::File::open(path).ok()?;
    let reader = BufReader::new(file);

    for line in reader.lines().take(max_lines) {
        let line = line.ok()?;
        let line = line.trim();
        if line.is_empty() {
            continue;
        }

        // Parse just the fields we need without full deserialization
        let value: serde_json::Value = serde_json::from_str(line).ok()?;
        if let Some(sid) = value.get("sessionId").and_then(|v| v.as_str()) {
            let slug = value.get("slug").and_then(|v| v.as_str()).map(String::from);
            let timestamp = value
                .get("timestamp")
                .and_then(|v| v.as_str())
                .and_then(|s| s.parse::<DateTime<Utc>>().ok());
            return Some((sid.to_string(), slug, timestamp));
        }
    }

    None
}

/// How much of a file's tail is searched for a `continued-in` record. The
/// record is written at hand-off, followed only by a few small sidecar
/// records; a session resumed at length afterwards can push it out of reach.
const CONTINUED_IN_TAIL_BYTES: u64 = 64 * 1024;

/// Find the session a file was handed off to: the `continuedInSessionId` of
/// the last `continued-in` record in the file's tail.
pub(crate) fn extract_continued_in(path: &Path) -> Option<String> {
    use std::io::{Read, Seek, SeekFrom};

    let mut file = std::fs::File::open(path).ok()?;
    let len = file.metadata().ok()?.len();
    let start = len.saturating_sub(CONTINUED_IN_TAIL_BYTES);
    file.seek(SeekFrom::Start(start)).ok()?;
    let mut tail = Vec::new();
    file.read_to_end(&mut tail).ok()?;
    let tail = String::from_utf8_lossy(&tail);

    // A window that starts mid-file begins with a partial line; it fails to
    // parse and is skipped like any other non-matching line.
    tail.lines()
        .rev()
        .filter(|line| line.contains("continued-in"))
        .find_map(|line| {
            let value: serde_json::Value = serde_json::from_str(line.trim()).ok()?;
            if value.get("type").and_then(|v| v.as_str()) != Some("continued-in") {
                return None;
            }
            value
                .get("continuedInSessionId")
                .and_then(|v| v.as_str())
                .map(String::from)
        })
}

/// Which sessions in a set begin with a copy of another session in the same
/// set (hand-off successors), so totals over the set can count the shared
/// history once.
#[derive(Debug, Default)]
pub struct HandoffIndex {
    /// successor file id -> path of the file it copies
    predecessor: HashMap<String, std::path::PathBuf>,
}

impl HandoffIndex {
    /// Index the hand-offs among `sessions`. A hand-off to or from a session
    /// outside the set is ignored: nothing in the set repeats it.
    pub fn for_sessions<'a>(sessions: impl IntoIterator<Item = &'a super::Session>) -> Self {
        let main: Vec<&super::Session> =
            sessions.into_iter().filter(|s| !s.is_subagent()).collect();
        let ids: std::collections::HashSet<&str> = main.iter().map(|s| s.session_id()).collect();
        let mut predecessor = HashMap::new();
        for session in &main {
            if let Some(successor) = extract_continued_in(session.path()) {
                if successor != session.session_id() && ids.contains(successor.as_str()) {
                    predecessor.insert(successor, session.path().to_path_buf());
                }
            }
        }
        Self { predecessor }
    }

    /// Remove from `entries` (parsed from `session_id`) the entries copied
    /// from its predecessor, matched by UUID. Sessions that are not hand-off
    /// successors are returned untouched. If the predecessor cannot be parsed
    /// under `max_file_size`, nothing is removed: the totals this feeds skip
    /// an unparseable file, so the copy is then the only one counted.
    pub fn drop_inherited(
        &self,
        session_id: &str,
        mut entries: Vec<LogEntry>,
        max_file_size: Option<u64>,
    ) -> Vec<LogEntry> {
        let Some(path) = self.predecessor.get(session_id) else {
            return entries;
        };
        let mut parser = JsonlParser::new().with_lenient(true);
        if let Some(max) = max_file_size {
            parser = parser.with_max_file_size(max);
        }
        let Ok(inherited) = parser.parse_file(path) else {
            return entries;
        };
        let inherited: std::collections::HashSet<&str> =
            inherited.iter().filter_map(LogEntry::uuid).collect();
        entries.retain(|entry| entry.uuid().is_none_or(|uuid| !inherited.contains(uuid)));
        entries
    }
}

/// Build session chains from a list of file UUIDs and their paths.
///
/// Takes an iterator of `(file_uuid, path)` pairs for main sessions (not subagents)
/// and returns a map from root session ID to its chain.
pub fn detect_chains<'a>(
    sessions: impl Iterator<Item = (&'a str, &'a Path)>,
) -> HashMap<String, SessionChain> {
    // Phase 1: Extract link info from each file
    let mut members: Vec<ChainMember> = Vec::new();
    // successor file id -> the file whose `continued-in` record names it
    let mut handed_off_from: HashMap<String, String> = HashMap::new();

    for (file_id, path) in sessions {
        if let Some(successor) = extract_continued_in(path) {
            if successor != file_id {
                handed_off_from.insert(successor, file_id.to_string());
            }
        }
        match extract_session_link(path, 10) {
            Some((internal_sid, slug, started)) => {
                trace!(
                    file_id = file_id,
                    internal_sid = %internal_sid,
                    is_continuation = (file_id != internal_sid),
                    "Extracted session link"
                );
                members.push(ChainMember {
                    file_id: file_id.to_string(),
                    internal_session_id: internal_sid,
                    slug,
                    started,
                });
            }
            None => {
                // No sessionId found — treat as standalone root
                members.push(ChainMember {
                    file_id: file_id.to_string(),
                    internal_session_id: file_id.to_string(),
                    slug: None,
                    started: None,
                });
            }
        }
    }

    // A handed-off successor is stamped with its own id, so it looks like a
    // root; its parent is the file that names it. A successor that was never
    // scanned leaves its predecessor standalone.
    for member in &mut members {
        if member.file_id == member.internal_session_id {
            if let Some(parent) = handed_off_from.get(&member.file_id) {
                member.internal_session_id = parent.clone();
            }
        }
    }

    // Phase 2: Build parent→children map
    // A member is a continuation if file_id != internal_session_id.
    // Its internal_session_id is the file_id of the session it continues from.
    let mut children: HashMap<String, Vec<String>> = HashMap::new();
    let mut member_map: HashMap<String, ChainMember> = HashMap::new();

    for member in members {
        if member.file_id != member.internal_session_id {
            children
                .entry(member.internal_session_id.clone())
                .or_default()
                .push(member.file_id.clone());
        }
        member_map.insert(member.file_id.clone(), member);
    }

    // Phase 3: Find roots and trace chains
    // A root is a file whose file_id is NOT a continuation of anything
    // (i.e., file_id == internal_session_id) AND has at least one child.
    // Standalone sessions (no children, not a continuation) are also roots
    // but we only create chain entries for multi-file chains.
    let mut chains: HashMap<String, SessionChain> = HashMap::new();

    // Also find roots by tracing continuation chains back
    let mut root_for: HashMap<String, String> = HashMap::new();

    for (file_id, member) in &member_map {
        if file_id == &member.internal_session_id {
            // This is a root (file_id == sessionId)
            root_for.insert(file_id.clone(), file_id.clone());
        }
    }

    // For continuations, trace back to find root
    for (file_id, member) in &member_map {
        if file_id != &member.internal_session_id {
            let mut current = member.internal_session_id.clone();
            let mut visited = vec![file_id.clone()];
            let resolved_root;
            loop {
                if let Some(root) = root_for.get(&current) {
                    resolved_root = root.clone();
                    break;
                }
                if visited.contains(&current) {
                    // Link cycle (two files naming each other): no file is a
                    // real root, so pick one deterministically.
                    resolved_root = visited.iter().min().cloned().unwrap_or(current);
                    break;
                }
                visited.push(current.clone());
                if let Some(parent) = member_map.get(&current) {
                    if parent.file_id == parent.internal_session_id {
                        // This is the root
                        resolved_root = current.clone();
                        break;
                    }
                    current = parent.internal_session_id.clone();
                } else {
                    // Parent file not found (might be in a different project or deleted).
                    // Treat the earliest known member as the root.
                    warn!(
                        file_id = %file_id,
                        missing_parent = %current,
                        "Chain parent not found, using earliest member as root"
                    );
                    resolved_root = current.clone();
                    break;
                }
            }
            for v in &visited {
                root_for.insert(v.clone(), resolved_root.clone());
            }
        }
    }

    // Phase 4: Group members by root and build chains.
    // Only include file IDs that exist in our member_map (real files we scanned).
    let mut groups: HashMap<String, Vec<String>> = HashMap::new();
    for (file_id, root_id) in &root_for {
        if member_map.contains_key(file_id.as_str()) {
            groups
                .entry(root_id.clone())
                .or_default()
                .push(file_id.clone());
        }
    }

    for (root_id, mut file_ids) in groups {
        if file_ids.len() < 2 {
            // Single-file session, not a chain
            continue;
        }

        // Only form chains where the root actually exists as a file we can read.
        // If the root is a missing parent UUID, skip this group.
        if !file_ids.contains(&root_id) {
            continue;
        }

        // Sort members by link order: root first, then each file's parent before it.
        // Build order by following the chain from root forward.
        let mut ordered = vec![root_id.clone()];
        let mut remaining: std::collections::HashSet<_> = file_ids.iter().cloned().collect();
        remaining.remove(&root_id);
        loop {
            let last = ordered.last().unwrap().clone();
            // Find the member whose internal_session_id == last (i.e., continues from last)
            let next = remaining
                .iter()
                .find(|id| {
                    member_map
                        .get(id.as_str())
                        .map(|m| m.internal_session_id == last)
                        .unwrap_or(false)
                })
                .cloned();
            match next {
                Some(id) => {
                    remaining.remove(&id);
                    ordered.push(id);
                }
                None => break,
            }
        }
        // Append any remaining members not reachable by link order (shouldn't happen
        // in well-formed data, but handle gracefully)
        for id in remaining {
            ordered.push(id);
        }
        file_ids = ordered;

        let chain_members: Vec<ChainMember> = file_ids
            .iter()
            .filter_map(|id| member_map.remove(id))
            .collect();

        // Slug is stable across the chain — take the first non-None
        let slug = chain_members.iter().find_map(|m| m.slug.clone());

        debug!(
            root_id = %root_id,
            members = chain_members.len(),
            slug = ?slug,
            "Detected session chain"
        );

        chains.insert(
            root_id.clone(),
            SessionChain {
                root_id,
                members: chain_members,
                slug,
            },
        );
    }

    chains
}

impl SessionChain {
    /// Parse all files in this chain into a unified entry list.
    ///
    /// Files are parsed in chain order (root first, continuations after).
    /// The resulting entries can be passed to `Conversation::from_entries()`
    /// to build a single conversation tree spanning all files.
    ///
    /// Requires a function that resolves file IDs to filesystem paths.
    pub fn parse_entries(
        &self,
        resolve_path: impl Fn(&str) -> Option<std::path::PathBuf>,
    ) -> Result<Vec<LogEntry>> {
        Ok(self.parse_entries_counted(None, resolve_path)?.0)
    }

    /// Like [`SessionChain::parse_entries`], but honors a `max_file_size` cap
    /// and also returns the total count of lines the lenient parser skipped
    /// across all members, so callers can surface a drop notice.
    pub fn parse_entries_counted(
        &self,
        max_file_size: Option<u64>,
        resolve_path: impl Fn(&str) -> Option<std::path::PathBuf>,
    ) -> Result<(Vec<LogEntry>, usize)> {
        let mut all_entries = Vec::new();
        let mut unparsed = 0usize;
        // A handed-off successor repeats its predecessor's entries; keep the
        // first copy of each UUID so the shared history is read once.
        let mut seen_uuids: std::collections::HashSet<String> = std::collections::HashSet::new();

        for member in &self.members {
            let path = resolve_path(&member.file_id).ok_or_else(|| {
                crate::error::SnatchError::FileNotFound {
                    path: std::path::PathBuf::from(&member.file_id),
                }
            })?;
            let mut parser = JsonlParser::new().with_lenient(true);
            if let Some(max) = max_file_size {
                parser = parser.with_max_file_size(max);
            }
            let entries = parser.parse_file(&path)?;
            unparsed += parser.stats().lines_skipped;
            debug!(
                file_id = %member.file_id,
                entries = entries.len(),
                "Parsed chain member"
            );
            all_entries.extend(entries.into_iter().filter(|entry| {
                entry
                    .uuid()
                    .is_none_or(|uuid| seen_uuids.insert(uuid.to_string()))
            }));
        }

        debug!(
            chain_root = %self.root_id,
            total_entries = all_entries.len(),
            members = self.members.len(),
            unparsed,
            "Parsed full chain"
        );
        Ok((all_entries, unparsed))
    }

    /// Number of files in this chain.
    pub fn len(&self) -> usize {
        self.members.len()
    }

    /// Whether this chain is empty (should never be true for valid chains).
    pub fn is_empty(&self) -> bool {
        self.members.is_empty()
    }

    /// Get all file UUIDs in order.
    pub fn file_ids(&self) -> Vec<&str> {
        self.members.iter().map(|m| m.file_id.as_str()).collect()
    }

    /// Check if a file UUID is part of this chain.
    pub fn contains(&self, file_id: &str) -> bool {
        self.members.iter().any(|m| m.file_id == file_id)
    }

    /// Get the position of a file in the chain (0-based).
    pub fn position_of(&self, file_id: &str) -> Option<usize> {
        self.members.iter().position(|m| m.file_id == file_id)
    }

    /// Start time of the chain (from the first member).
    pub fn started(&self) -> Option<DateTime<Utc>> {
        self.members.first().and_then(|m| m.started)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::TempDir;

    fn write_jsonl(
        dir: &Path,
        name: &str,
        session_id: &str,
        slug: Option<&str>,
    ) -> std::path::PathBuf {
        let path = dir.join(format!("{name}.jsonl"));
        let mut file = std::fs::File::create(&path).unwrap();
        let slug_field = slug
            .map(|s| format!(r#", "slug": "{s}""#))
            .unwrap_or_default();
        writeln!(
            file,
            r#"{{"type": "user", "uuid": "{name}", "sessionId": "{session_id}", "timestamp": "2026-01-01T00:00:00Z", "version": "2.1.0", "isSidechain": false{slug_field}, "message": {{"role": "user", "content": "hello"}}}}"#
        )
        .unwrap();
        path
    }

    #[test]
    fn test_standalone_session_no_chain() {
        let dir = TempDir::new().unwrap();
        let id = "aaa-111";
        let path = write_jsonl(dir.path(), id, id, None);

        let sessions: Vec<(&str, &Path)> = vec![(id, &path)];
        let chains = detect_chains(sessions.into_iter());
        assert!(chains.is_empty(), "Single file should not create a chain");
    }

    #[test]
    fn test_two_file_chain() {
        let dir = TempDir::new().unwrap();
        let root_id = "aaa-111";
        let cont_id = "bbb-222";
        let root_path = write_jsonl(dir.path(), root_id, root_id, Some("cool-session"));
        let cont_path = write_jsonl(dir.path(), cont_id, root_id, Some("cool-session"));

        let sessions: Vec<(&str, &Path)> = vec![(root_id, &root_path), (cont_id, &cont_path)];
        let chains = detect_chains(sessions.into_iter());

        assert_eq!(chains.len(), 1);
        let chain = chains.get(root_id).unwrap();
        assert_eq!(chain.len(), 2);
        assert_eq!(chain.root_id, root_id);
        assert_eq!(chain.slug.as_deref(), Some("cool-session"));
        assert!(chain.contains(root_id));
        assert!(chain.contains(cont_id));
        assert_eq!(chain.position_of(root_id), Some(0));
        assert_eq!(chain.position_of(cont_id), Some(1));
    }

    #[test]
    fn test_three_file_chain() {
        let dir = TempDir::new().unwrap();
        // A -> B -> C (each points to previous)
        let a_path = write_jsonl(dir.path(), "aaa", "aaa", Some("slug"));
        let b_path = write_jsonl(dir.path(), "bbb", "aaa", Some("slug"));
        let c_path = write_jsonl(dir.path(), "ccc", "bbb", Some("slug"));

        let sessions: Vec<(&str, &Path)> =
            vec![("aaa", &a_path), ("bbb", &b_path), ("ccc", &c_path)];
        let chains = detect_chains(sessions.into_iter());

        assert_eq!(chains.len(), 1);
        let chain = chains.get("aaa").unwrap();
        assert_eq!(chain.len(), 3);
        assert_eq!(chain.file_ids(), vec!["aaa", "bbb", "ccc"]);
    }

    #[test]
    fn test_two_independent_chains() {
        let dir = TempDir::new().unwrap();
        let a1 = write_jsonl(dir.path(), "a1", "a1", Some("chain-a"));
        let a2 = write_jsonl(dir.path(), "a2", "a1", Some("chain-a"));
        let b1 = write_jsonl(dir.path(), "b1", "b1", Some("chain-b"));
        let b2 = write_jsonl(dir.path(), "b2", "b1", Some("chain-b"));

        let sessions: Vec<(&str, &Path)> = vec![("a1", &a1), ("a2", &a2), ("b1", &b1), ("b2", &b2)];
        let chains = detect_chains(sessions.into_iter());

        assert_eq!(chains.len(), 2);
        assert!(chains.contains_key("a1"));
        assert!(chains.contains_key("b1"));
    }

    #[test]
    fn test_missing_parent_handled() {
        let dir = TempDir::new().unwrap();
        // cont points to a parent that doesn't exist in our session list
        let cont_path = write_jsonl(dir.path(), "cont", "missing-parent", None);

        let sessions: Vec<(&str, &Path)> = vec![("cont", &cont_path)];
        let chains = detect_chains(sessions.into_iter());

        // Should not panic, and single orphan doesn't form a chain
        assert!(chains.is_empty());
    }

    #[test]
    fn test_chain_parse_entries() {
        let dir = TempDir::new().unwrap();
        let root_path = write_jsonl(dir.path(), "root", "root", Some("slug"));
        let cont_path = write_jsonl(dir.path(), "cont", "root", Some("slug"));

        let sessions: Vec<(&str, &Path)> = vec![("root", &root_path), ("cont", &cont_path)];
        let chains = detect_chains(sessions.into_iter());
        let chain = chains.get("root").unwrap();

        let entries = chain
            .parse_entries(|file_id| Some(dir.path().join(format!("{file_id}.jsonl"))))
            .unwrap();

        // Each file has 1 user entry, so chain should have 2
        assert_eq!(entries.len(), 2);
        assert!(entries.iter().all(|e| e.message_type() == "user"));
    }

    /// Write a session file stamped with its own id: one entry per uuid, then
    /// an optional `continued-in` record and the sidecar records Claude Code
    /// appends after a hand-off.
    fn write_handoff_file(dir: &Path, id: &str, uuids: &[&str], continued_in: Option<&str>) {
        let mut file = std::fs::File::create(dir.join(format!("{id}.jsonl"))).unwrap();
        for uuid in uuids {
            writeln!(
                file,
                r#"{{"type": "user", "uuid": "{uuid}", "sessionId": "{id}", "timestamp": "2026-11-08T00:00:00Z", "version": "2.1.319", "isSidechain": false, "message": {{"role": "user", "content": "hello"}}}}"#
            )
            .unwrap();
        }
        if let Some(next) = continued_in {
            writeln!(
                file,
                r#"{{"type":"continued-in","timestamp":"2026-11-08T20:55:01.599Z","sessionId":"{id}","continuedInSessionId":"{next}"}}"#
            )
            .unwrap();
            writeln!(
                file,
                r#"{{"type":"mode","mode":"normal","sessionId":"{id}"}}"#
            )
            .unwrap();
        }
    }

    fn chains_in(dir: &Path, ids: &[&str]) -> HashMap<String, SessionChain> {
        let paths: Vec<_> = ids
            .iter()
            .map(|id| dir.join(format!("{id}.jsonl")))
            .collect();
        detect_chains(ids.iter().copied().zip(paths.iter().map(|p| p.as_path())))
    }

    #[test]
    fn test_handoff_links_files_and_reads_shared_history_once() {
        let dir = TempDir::new().unwrap();
        // "new" is stamped with its own id and repeats "old"'s two entries.
        write_handoff_file(dir.path(), "old", &["u1", "u2"], Some("new"));
        write_handoff_file(dir.path(), "new", &["u1", "u2", "u3"], None);

        let chains = chains_in(dir.path(), &["new", "old"]);
        assert_eq!(chains.len(), 1);
        let chain = chains.get("old").expect("the handed-off file is the root");
        assert_eq!(chain.file_ids(), vec!["old", "new"]);

        let entries = chain
            .parse_entries(|id| Some(dir.path().join(format!("{id}.jsonl"))))
            .unwrap();
        let uuids: Vec<_> = entries.iter().filter_map(|e| e.uuid()).collect();
        assert_eq!(uuids, vec!["u1", "u2", "u3"]);
    }

    fn session_at(dir: &Path, id: &str) -> crate::discovery::Session {
        crate::discovery::Session::from_path(dir.join(format!("{id}.jsonl")), "-proj").unwrap()
    }

    #[test]
    fn test_handoff_index_drops_only_the_copied_entries() {
        // Session discovery only accepts UUID file names.
        const OLD: &str = "11111111-1111-4111-8111-111111111111";
        const NEW: &str = "22222222-2222-4222-8222-222222222222";
        let dir = TempDir::new().unwrap();
        write_handoff_file(dir.path(), OLD, &["u1", "u2"], Some(NEW));
        write_handoff_file(dir.path(), NEW, &["u1", "u2", "u3"], None);
        let (old, new) = (session_at(dir.path(), OLD), session_at(dir.path(), NEW));
        let uuids = |index: &HandoffIndex, s: &crate::discovery::Session| -> Vec<String> {
            index
                .drop_inherited(s.session_id(), s.parse().unwrap(), None)
                .iter()
                .filter_map(|e| e.uuid().map(String::from))
                .collect()
        };

        let both = HandoffIndex::for_sessions(&[new.clone(), old.clone()]);
        assert_eq!(uuids(&both, &old), vec!["u1", "u2"]);
        assert_eq!(uuids(&both, &new), vec!["u3"]);

        // Without the predecessor in the set, the successor's copy is the
        // only one, so it is kept.
        let alone = HandoffIndex::for_sessions(std::slice::from_ref(&new));
        assert_eq!(uuids(&alone, &new), vec!["u1", "u2", "u3"]);
    }

    #[test]
    fn test_handoff_to_a_missing_file_is_not_a_chain() {
        let dir = TempDir::new().unwrap();
        write_handoff_file(dir.path(), "old", &["u1"], Some("gone"));
        assert!(chains_in(dir.path(), &["old"]).is_empty());
    }

    #[test]
    fn test_handoff_extends_a_resume_chain() {
        let dir = TempDir::new().unwrap();
        // root -> cont by resume (cont carries root's id), cont -> next by hand-off.
        write_jsonl(dir.path(), "root", "root", None);
        let mut cont = std::fs::File::create(dir.path().join("cont.jsonl")).unwrap();
        writeln!(cont, r#"{{"type": "user", "uuid": "c1", "sessionId": "root", "timestamp": "2026-11-08T00:00:00Z", "version": "2.1.319", "isSidechain": false, "message": {{"role": "user", "content": "hi"}}}}"#).unwrap();
        writeln!(
            cont,
            r#"{{"type":"continued-in","sessionId":"cont","continuedInSessionId":"next"}}"#
        )
        .unwrap();
        drop(cont);
        write_handoff_file(dir.path(), "next", &["c1", "n1"], None);

        let chains = chains_in(dir.path(), &["next", "cont", "root"]);
        assert_eq!(chains.len(), 1);
        assert_eq!(chains["root"].file_ids(), vec!["root", "cont", "next"]);
    }

    #[test]
    fn test_handoff_cycle_terminates() {
        let dir = TempDir::new().unwrap();
        write_handoff_file(dir.path(), "a", &["u1"], Some("b"));
        write_handoff_file(dir.path(), "b", &["u1"], Some("a"));
        // Must return; which file is called the root is not asserted.
        let chains = chains_in(dir.path(), &["a", "b"]);
        assert!(chains.len() <= 1);
    }

    #[test]
    fn test_extract_continued_in_ignores_mentions_in_content() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("s.jsonl");
        let mut file = std::fs::File::create(&path).unwrap();
        writeln!(file, r#"{{"type": "user", "uuid": "u1", "sessionId": "s", "message": {{"role": "user", "content": "{{\"type\":\"continued-in\",\"continuedInSessionId\":\"x\"}}"}}}}"#).unwrap();
        drop(file);
        assert_eq!(extract_continued_in(&path), None);
    }

    #[test]
    fn test_extract_session_link() {
        let dir = TempDir::new().unwrap();
        let path = write_jsonl(dir.path(), "test", "parent-id", Some("my-slug"));

        let (sid, slug, _ts) = extract_session_link(&path, 10).unwrap();
        assert_eq!(sid, "parent-id");
        assert_eq!(slug.as_deref(), Some("my-slug"));
    }

    #[test]
    fn test_file_starting_with_snapshot() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("test.jsonl");
        let mut file = std::fs::File::create(&path).unwrap();
        // First line is file-history-snapshot (no sessionId)
        writeln!(
            file,
            r#"{{"type": "file-history-snapshot", "messageId": "x", "snapshot": {{}}}}"#
        )
        .unwrap();
        // Second line has sessionId
        writeln!(file, r#"{{"type": "user", "sessionId": "the-id", "slug": "the-slug", "timestamp": "2026-01-01T00:00:00Z", "message": {{"role": "user", "content": "hi"}}}}"#).unwrap();

        let (sid, slug, _ts) = extract_session_link(&path, 10).unwrap();
        assert_eq!(sid, "the-id");
        assert_eq!(slug.as_deref(), Some("the-slug"));
    }
}
