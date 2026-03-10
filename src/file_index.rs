//! File-session relationship index.
//!
//! Builds a reverse index from file paths to the sessions and messages
//! that modified them, using `file-history-snapshot` entries.

use std::collections::HashMap;

use chrono::{DateTime, Utc};

use crate::discovery::{ClaudeDirectory, Session};
use crate::error::Result;
use crate::model::message::LogEntry;

/// A record of a file being modified in a session.
#[derive(Debug, Clone)]
pub struct FileModification {
    /// The session that modified the file.
    pub session_id: String,
    /// The project path for the session.
    pub project_path: String,
    /// The message ID associated with the modification.
    pub message_id: String,
    /// When the modification was recorded.
    pub timestamp: DateTime<Utc>,
    /// Backup version number.
    pub version: u32,
}

/// Reverse index: file path → list of modifications across sessions.
#[derive(Debug, Default)]
pub struct FileIndex {
    /// Map from file path to modifications, sorted by timestamp.
    pub entries: HashMap<String, Vec<FileModification>>,
}

impl FileIndex {
    /// Build a file index from a set of sessions.
    pub fn from_sessions(sessions: &[Session], max_file_size: Option<u64>) -> Self {
        let mut index = FileIndex::default();

        for session in sessions {
            let entries = match session.parse_with_options(max_file_size) {
                Ok(e) => e,
                Err(_) => continue,
            };

            let sid = session.session_id().to_string();
            let project_path = session.project_path().to_string();

            for entry in &entries {
                if let LogEntry::FileHistorySnapshot(snapshot) = entry {
                    for (file_path, backup) in &snapshot.snapshot.tracked_file_backups {
                        index
                            .entries
                            .entry(file_path.clone())
                            .or_default()
                            .push(FileModification {
                                session_id: sid.clone(),
                                project_path: project_path.clone(),
                                message_id: snapshot.message_id.clone(),
                                timestamp: backup.backup_time,
                                version: backup.version,
                            });
                    }
                }
            }
        }

        // Sort each file's modifications by timestamp
        for mods in index.entries.values_mut() {
            mods.sort_by_key(|m| m.timestamp);
        }

        index
    }

    /// Build a file index for all sessions matching a project filter.
    pub fn for_project(
        claude_dir: &ClaudeDirectory,
        project_filter: &str,
        max_file_size: Option<u64>,
    ) -> Result<Self> {
        let mut all_sessions: Vec<Session> = Vec::new();

        for project in claude_dir.projects()? {
            if project.best_path().contains(project_filter) {
                all_sessions.extend(project.sessions()?);
            }
        }

        Ok(Self::from_sessions(&all_sessions, max_file_size))
    }

    /// Look up which sessions modified a specific file.
    pub fn get(&self, file_path: &str) -> Option<&[FileModification]> {
        self.entries.get(file_path).map(|v| v.as_slice())
    }

    /// Find files matching a substring pattern.
    pub fn search(&self, pattern: &str) -> Vec<(&str, &[FileModification])> {
        self.entries
            .iter()
            .filter(|(path, _)| path.contains(pattern))
            .map(|(path, mods)| (path.as_str(), mods.as_slice()))
            .collect()
    }

    /// Total number of unique files tracked.
    pub fn file_count(&self) -> usize {
        self.entries.len()
    }

    /// Total number of modification records.
    pub fn modification_count(&self) -> usize {
        self.entries.values().map(|v| v.len()).sum()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_file_index_default() {
        let index = FileIndex::default();
        assert_eq!(index.file_count(), 0);
        assert_eq!(index.modification_count(), 0);
        assert!(index.get("/some/path").is_none());
    }

    #[test]
    fn test_file_index_search_empty() {
        let index = FileIndex::default();
        assert!(index.search("anything").is_empty());
    }
}
