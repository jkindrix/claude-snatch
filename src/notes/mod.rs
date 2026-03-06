//! Tactical session notes for Claude Code sessions.
//!
//! Provides a lightweight scratchpad that persists across compactions.
//! Notes capture tactical work state ("tried X, failed because Y, now doing Z")
//! as opposed to strategic goals ("build feature X").
//!
//! Storage: `~/.claude/projects/<project>/memory/notes.json`

use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::error::{Result, SnatchError};
use crate::util::atomic_write;

/// A tactical note.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Note {
    /// Unique note ID (monotonically increasing).
    pub id: u64,
    /// Note text.
    pub text: String,
    /// Session that created this note (optional).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    /// When the note was created.
    pub created_at: DateTime<Utc>,
}

/// Persistent note store.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NoteStore {
    /// All notes.
    pub notes: Vec<Note>,
    /// Next ID to assign.
    pub next_id: u64,
}

impl Default for NoteStore {
    fn default() -> Self {
        Self {
            notes: Vec::new(),
            next_id: 1,
        }
    }
}

impl NoteStore {
    /// Add a new note. Returns the assigned ID.
    pub fn add_note(&mut self, text: String, session_id: Option<String>) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        self.notes.push(Note {
            id,
            text,
            session_id,
            created_at: Utc::now(),
        });
        id
    }

    /// Clear all notes. Returns the number removed.
    pub fn clear(&mut self) -> usize {
        let count = self.notes.len();
        self.notes.clear();
        self.next_id = 1;
        count
    }

    /// Remove a single note by ID. Returns true if found.
    pub fn remove_note(&mut self, id: u64) -> bool {
        let len_before = self.notes.len();
        self.notes.retain(|n| n.id != id);
        self.notes.len() < len_before
    }

    /// Format notes for hook injection.
    ///
    /// Returns compact markdown suitable for injecting into context.
    /// Returns `None` if there are no notes.
    pub fn format_notes_for_injection(&self) -> Option<String> {
        if self.notes.is_empty() {
            return None;
        }

        let mut lines = vec!["### Tactical Notes".to_string()];
        for note in &self.notes {
            lines.push(format!("- #{}: {}", note.id, note.text));
        }
        Some(lines.join("\n"))
    }
}

/// Resolve the notes.json path for a project directory.
pub fn notes_path(project_dir: &Path) -> PathBuf {
    project_dir.join("memory").join("notes.json")
}

/// Load notes from a project directory.
///
/// Returns a default (empty) store if the file doesn't exist.
pub fn load_notes(project_dir: &Path) -> Result<NoteStore> {
    let path = notes_path(project_dir);
    if !path.exists() {
        return Ok(NoteStore::default());
    }
    let content = std::fs::read_to_string(&path).map_err(|source| SnatchError::IoError {
        context: format!("Failed to read notes file {}", path.display()),
        source,
    })?;
    serde_json::from_str(&content).map_err(|source| SnatchError::SerializationError {
        context: format!("Failed to parse notes file {}", path.display()),
        source,
    })
}

/// Save notes to a project directory (atomic write).
///
/// Creates the `memory/` subdirectory if it doesn't exist.
pub fn save_notes(project_dir: &Path, store: &NoteStore) -> Result<()> {
    let path = notes_path(project_dir);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|source| SnatchError::IoError {
            context: format!("Failed to create memory directory {}", parent.display()),
            source,
        })?;
    }
    let json =
        serde_json::to_string_pretty(store).map_err(|source| SnatchError::SerializationError {
            context: "Failed to serialize notes".to_string(),
            source,
        })?;
    atomic_write(&path, json.as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_add_note() {
        let mut store = NoteStore::default();
        let id1 = store.add_note("First note".into(), None);
        let id2 = store.add_note("Second note".into(), Some("session-123".into()));

        assert_eq!(id1, 1);
        assert_eq!(id2, 2);
        assert_eq!(store.notes.len(), 2);
        assert_eq!(store.next_id, 3);
        assert_eq!(store.notes[0].text, "First note");
        assert!(store.notes[0].session_id.is_none());
        assert_eq!(store.notes[1].session_id.as_deref(), Some("session-123"));
    }

    #[test]
    fn test_clear_notes() {
        let mut store = NoteStore::default();
        store.add_note("Note 1".into(), None);
        store.add_note("Note 2".into(), None);

        let removed = store.clear();
        assert_eq!(removed, 2);
        assert!(store.notes.is_empty());
        assert_eq!(store.next_id, 1);
    }

    #[test]
    fn test_remove_note() {
        let mut store = NoteStore::default();
        store.add_note("Note 1".into(), None);
        store.add_note("Note 2".into(), None);

        assert!(store.remove_note(1));
        assert_eq!(store.notes.len(), 1);
        assert_eq!(store.notes[0].id, 2);

        assert!(!store.remove_note(1)); // already removed
    }

    #[test]
    fn test_format_notes_empty() {
        let store = NoteStore::default();
        assert!(store.format_notes_for_injection().is_none());
    }

    #[test]
    fn test_format_notes_with_entries() {
        let mut store = NoteStore::default();
        store.add_note("Tried redis, failed due to connection pooling".into(), None);
        store.add_note("Using in-memory LRU instead".into(), None);

        let formatted = store.format_notes_for_injection().unwrap();
        assert!(formatted.contains("### Tactical Notes"));
        assert!(formatted.contains("#1: Tried redis, failed due to connection pooling"));
        assert!(formatted.contains("#2: Using in-memory LRU instead"));
    }

    #[test]
    fn test_load_save_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let project_dir = dir.path();

        let store = load_notes(project_dir).unwrap();
        assert!(store.notes.is_empty());

        let mut store = NoteStore::default();
        store.add_note("Test note".into(), Some("sess-abc".into()));

        save_notes(project_dir, &store).unwrap();

        let loaded = load_notes(project_dir).unwrap();
        assert_eq!(loaded.notes.len(), 1);
        assert_eq!(loaded.notes[0].text, "Test note");
        assert_eq!(loaded.notes[0].session_id.as_deref(), Some("sess-abc"));
        assert_eq!(loaded.next_id, 2);
    }

    #[test]
    fn test_notes_path() {
        let path = notes_path(Path::new("/home/user/.claude/projects/-home-user-myproject"));
        assert_eq!(
            path,
            PathBuf::from("/home/user/.claude/projects/-home-user-myproject/memory/notes.json")
        );
    }

    #[test]
    fn test_serde_roundtrip() {
        let mut store = NoteStore::default();
        store.add_note("Note 1".into(), None);
        store.add_note("Note 2".into(), Some("sess-xyz".into()));

        let json = serde_json::to_string_pretty(&store).unwrap();
        let parsed: NoteStore = serde_json::from_str(&json).unwrap();

        assert_eq!(parsed.notes.len(), 2);
        assert_eq!(parsed.next_id, 3);
        assert_eq!(parsed.notes[1].session_id.as_deref(), Some("sess-xyz"));
    }
}
