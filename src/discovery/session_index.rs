//! Session index reader for Claude Code's `sessions-index.json`.
//!
//! Claude Code maintains a per-project index file with session metadata
//! including creation time, first prompt, summary, and sidechain status.
//! This module reads that index and makes it available during discovery.

use std::collections::HashMap;
use std::path::Path;

use chrono::{DateTime, Utc};
use serde::Deserialize;
use tracing::{debug, warn};

use crate::error::Result;

/// The per-project session index maintained by Claude Code.
#[derive(Debug, Clone)]
pub struct SessionIndex {
    entries: HashMap<String, SessionIndexEntry>,
}

/// A single entry in the session index.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionIndexEntry {
    /// Session ID (matches filename UUID for root sessions, or the logical session ID).
    pub session_id: String,
    /// Full path to the JSONL file.
    pub full_path: String,
    /// File modification time in milliseconds since epoch.
    pub file_mtime: u64,
    /// First user prompt in the session.
    #[serde(default)]
    pub first_prompt: Option<String>,
    /// Summary of the session.
    #[serde(default)]
    pub summary: Option<String>,
    /// Number of messages in the session.
    #[serde(default)]
    pub message_count: Option<usize>,
    /// Session creation time.
    #[serde(default)]
    pub created: Option<DateTime<Utc>>,
    /// Session last modification time.
    #[serde(default)]
    pub modified: Option<DateTime<Utc>>,
    /// Git branch at session time.
    #[serde(default)]
    pub git_branch: Option<String>,
    /// Project working directory path.
    #[serde(default)]
    pub project_path: Option<String>,
    /// Whether this is a sidechain/subagent session.
    #[serde(default)]
    pub is_sidechain: bool,
}

/// Raw JSON structure of the index file.
#[derive(Debug, Deserialize)]
struct RawSessionIndex {
    #[allow(dead_code)]
    version: u32,
    #[serde(default)]
    entries: Vec<SessionIndexEntry>,
}

impl SessionIndex {
    /// Load the session index from a project directory.
    ///
    /// Returns an empty index if the file doesn't exist or can't be parsed.
    pub fn load(project_dir: &Path) -> Self {
        let index_path = project_dir.join("sessions-index.json");

        if !index_path.exists() {
            return Self::empty();
        }

        match Self::load_from_file(&index_path) {
            Ok(index) => {
                debug!(entries = index.entries.len(), "Loaded session index");
                index
            }
            Err(e) => {
                warn!(?e, "Failed to load sessions-index.json, using empty index");
                Self::empty()
            }
        }
    }

    /// Load from a specific file path.
    fn load_from_file(path: &Path) -> Result<Self> {
        let content = std::fs::read_to_string(path).map_err(|e| {
            crate::error::SnatchError::io(
                format!("Failed to read {}", path.display()),
                e,
            )
        })?;

        let raw: RawSessionIndex = serde_json::from_str(&content).map_err(|e| {
            crate::error::SnatchError::parse(
                0,
                format!("Failed to parse {}: {}", path.display(), e),
            )
        })?;

        let mut entries = HashMap::with_capacity(raw.entries.len());
        for entry in raw.entries {
            // Key by sessionId for lookup
            entries.insert(entry.session_id.clone(), entry);
        }

        Ok(Self { entries })
    }

    /// Create an empty index.
    fn empty() -> Self {
        Self {
            entries: HashMap::new(),
        }
    }

    /// Look up a session by its ID.
    pub fn get(&self, session_id: &str) -> Option<&SessionIndexEntry> {
        self.entries.get(session_id)
    }

    /// Number of entries in the index.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether the index is empty (no entries or file not found).
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Iterate over all entries.
    pub fn iter(&self) -> impl Iterator<Item = (&String, &SessionIndexEntry)> {
        self.entries.iter()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn test_empty_index_on_missing_file() {
        let index = SessionIndex::load(&PathBuf::from("/nonexistent/path"));
        assert!(index.is_empty());
    }

    #[test]
    fn test_deserialize_entry() {
        let json = r#"{
            "sessionId": "abc-123",
            "fullPath": "/path/to/abc-123.jsonl",
            "fileMtime": 1700000000000,
            "firstPrompt": "Hello world",
            "summary": "Test session",
            "messageCount": 10,
            "created": "2026-01-01T00:00:00.000Z",
            "modified": "2026-01-01T01:00:00.000Z",
            "gitBranch": "main",
            "projectPath": "/home/user/project",
            "isSidechain": false
        }"#;

        let entry: SessionIndexEntry = serde_json::from_str(json).unwrap();
        assert_eq!(entry.session_id, "abc-123");
        assert_eq!(entry.first_prompt.as_deref(), Some("Hello world"));
        assert_eq!(entry.summary.as_deref(), Some("Test session"));
        assert_eq!(entry.message_count, Some(10));
        assert!(entry.created.is_some());
        assert!(!entry.is_sidechain);
    }

    #[test]
    fn test_deserialize_minimal_entry() {
        let json = r#"{
            "sessionId": "abc-123",
            "fullPath": "/path/to/abc-123.jsonl",
            "fileMtime": 1700000000000
        }"#;

        let entry: SessionIndexEntry = serde_json::from_str(json).unwrap();
        assert_eq!(entry.session_id, "abc-123");
        assert!(entry.first_prompt.is_none());
        assert!(entry.summary.is_none());
        assert!(!entry.is_sidechain);
    }

    #[test]
    fn test_load_full_index() {
        let json = r#"{
            "version": 1,
            "entries": [
                {
                    "sessionId": "aaa-111",
                    "fullPath": "/path/aaa-111.jsonl",
                    "fileMtime": 1700000000000,
                    "firstPrompt": "First"
                },
                {
                    "sessionId": "bbb-222",
                    "fullPath": "/path/bbb-222.jsonl",
                    "fileMtime": 1700000001000,
                    "firstPrompt": "Second"
                }
            ]
        }"#;

        let raw: super::RawSessionIndex = serde_json::from_str(json).unwrap();
        assert_eq!(raw.version, 1);
        assert_eq!(raw.entries.len(), 2);
    }
}
