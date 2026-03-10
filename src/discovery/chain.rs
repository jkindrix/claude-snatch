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
    /// The sessionId field from the JSONL (points to parent file).
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
fn extract_session_link(path: &Path, max_lines: usize) -> Option<(String, Option<String>, Option<DateTime<Utc>>)> {
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

/// Build session chains from a list of file UUIDs and their paths.
///
/// Takes an iterator of `(file_uuid, path)` pairs for main sessions (not subagents)
/// and returns a map from root session ID to its chain.
pub fn detect_chains<'a>(
    sessions: impl Iterator<Item = (&'a str, &'a Path)>,
) -> HashMap<String, SessionChain> {
    // Phase 1: Extract link info from each file
    let mut members: Vec<ChainMember> = Vec::new();

    for (file_id, path) in sessions {
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
            groups.entry(root_id.clone()).or_default().push(file_id.clone());
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
            let next = remaining.iter().find(|id| {
                member_map
                    .get(id.as_str())
                    .map(|m| m.internal_session_id == last)
                    .unwrap_or(false)
            }).cloned();
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
        let mut all_entries = Vec::new();
        let mut parser = JsonlParser::new().with_lenient(true);

        for member in &self.members {
            let path = resolve_path(&member.file_id).ok_or_else(|| {
                crate::error::SnatchError::FileNotFound {
                    path: std::path::PathBuf::from(&member.file_id),
                }
            })?;
            let entries = parser.parse_file(&path)?;
            debug!(
                file_id = %member.file_id,
                entries = entries.len(),
                "Parsed chain member"
            );
            all_entries.extend(entries);
        }

        debug!(
            chain_root = %self.root_id,
            total_entries = all_entries.len(),
            members = self.members.len(),
            "Parsed full chain"
        );
        Ok(all_entries)
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

    fn write_jsonl(dir: &Path, name: &str, session_id: &str, slug: Option<&str>) -> std::path::PathBuf {
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

        let sessions: Vec<(&str, &Path)> = vec![
            (root_id, &root_path),
            (cont_id, &cont_path),
        ];
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

        let sessions: Vec<(&str, &Path)> = vec![
            ("aaa", &a_path),
            ("bbb", &b_path),
            ("ccc", &c_path),
        ];
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

        let sessions: Vec<(&str, &Path)> = vec![
            ("a1", &a1), ("a2", &a2),
            ("b1", &b1), ("b2", &b2),
        ];
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

        let sessions: Vec<(&str, &Path)> = vec![
            ("root", &root_path),
            ("cont", &cont_path),
        ];
        let chains = detect_chains(sessions.into_iter());
        let chain = chains.get("root").unwrap();

        let entries = chain.parse_entries(|file_id| {
            Some(dir.path().join(format!("{file_id}.jsonl")))
        }).unwrap();

        // Each file has 1 user entry, so chain should have 2
        assert_eq!(entries.len(), 2);
        assert!(entries.iter().all(|e| e.message_type() == "user"));
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
        writeln!(file, r#"{{"type": "file-history-snapshot", "messageId": "x", "snapshot": {{}}}}"#).unwrap();
        // Second line has sessionId
        writeln!(file, r#"{{"type": "user", "sessionId": "the-id", "slug": "the-slug", "timestamp": "2026-01-01T00:00:00Z", "message": {{"role": "user", "content": "hi"}}}}"#).unwrap();

        let (sid, slug, _ts) = extract_session_link(&path, 10).unwrap();
        assert_eq!(sid, "the-id");
        assert_eq!(slug.as_deref(), Some("the-slug"));
    }
}
