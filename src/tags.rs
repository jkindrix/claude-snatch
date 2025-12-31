//! Session tagging and naming.
//!
//! Provides human-friendly labels for sessions, stored in a JSON file
//! separate from the Claude session data.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::str::FromStr;

use serde::{Deserialize, Serialize};

use crate::error::{Result, SnatchError};
use crate::util::atomic_write;

/// Tag storage filename.
const TAGS_FILENAME: &str = "tags.json";

/// Session outcome classification for analysis.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SessionOutcome {
    /// Session achieved its goal successfully.
    Success,
    /// Session partially achieved its goal.
    Partial,
    /// Session failed to achieve its goal.
    Failed,
    /// Session was abandoned before completion.
    Abandoned,
}

impl std::fmt::Display for SessionOutcome {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Success => write!(f, "success"),
            Self::Partial => write!(f, "partial"),
            Self::Failed => write!(f, "failed"),
            Self::Abandoned => write!(f, "abandoned"),
        }
    }
}

impl FromStr for SessionOutcome {
    type Err = String;

    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "success" | "s" => Ok(Self::Success),
            "partial" | "p" => Ok(Self::Partial),
            "failed" | "fail" | "f" => Ok(Self::Failed),
            "abandoned" | "abandon" | "a" => Ok(Self::Abandoned),
            _ => Err(format!(
                "Invalid outcome '{}'. Valid values: success, partial, failed, abandoned",
                s
            )),
        }
    }
}

/// A timestamped note/annotation for a session.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionNote {
    /// The note content.
    pub text: String,
    /// When the note was created.
    pub created_at: chrono::DateTime<chrono::Utc>,
    /// Optional category/label for the note.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
}

impl SessionNote {
    /// Create a new note with the current timestamp.
    pub fn new(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            created_at: chrono::Utc::now(),
            label: None,
        }
    }

    /// Create a note with a label.
    pub fn with_label(text: impl Into<String>, label: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            created_at: chrono::Utc::now(),
            label: Some(label.into()),
        }
    }
}

/// Statistics for session outcomes.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct OutcomeStats {
    /// Number of successful sessions.
    pub success: usize,
    /// Number of partially successful sessions.
    pub partial: usize,
    /// Number of failed sessions.
    pub failed: usize,
    /// Number of abandoned sessions.
    pub abandoned: usize,
    /// Number of sessions without outcome classification.
    pub unclassified: usize,
}

impl OutcomeStats {
    /// Total number of classified sessions.
    pub fn classified(&self) -> usize {
        self.success + self.partial + self.failed + self.abandoned
    }

    /// Success rate as a percentage (success / classified * 100).
    pub fn success_rate(&self) -> f64 {
        let classified = self.classified();
        if classified == 0 {
            0.0
        } else {
            (self.success as f64 / classified as f64) * 100.0
        }
    }
}

/// Session metadata including tags and optional name.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SessionMeta {
    /// Human-readable name for the session.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Tags associated with the session.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    /// Whether this session is bookmarked/favorited.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub bookmarked: bool,
    /// Session outcome classification.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub outcome: Option<SessionOutcome>,
    /// Notes/annotations for the session.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub notes: Vec<SessionNote>,
}

impl SessionMeta {
    /// Check if this metadata is empty (no name, tags, bookmark, outcome, or notes).
    pub fn is_empty(&self) -> bool {
        self.name.is_none()
            && self.tags.is_empty()
            && !self.bookmarked
            && self.outcome.is_none()
            && self.notes.is_empty()
    }
}

/// Tag store for session metadata.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TagStore {
    /// Version of the tag store format.
    #[serde(default = "default_version")]
    pub version: u32,
    /// Session metadata keyed by session ID.
    #[serde(default)]
    pub sessions: HashMap<String, SessionMeta>,
}

fn default_version() -> u32 {
    1
}

impl TagStore {
    /// Load tag store from default location.
    pub fn load() -> Result<Self> {
        let path = default_tags_path()?;
        if path.exists() {
            Self::load_from(&path)
        } else {
            Ok(Self::default())
        }
    }

    /// Load tag store from a specific path.
    pub fn load_from(path: &Path) -> Result<Self> {
        let content = std::fs::read_to_string(path).map_err(|e| {
            SnatchError::io(format!("Failed to read tags file: {}", path.display()), e)
        })?;

        serde_json::from_str(&content).map_err(|e| {
            SnatchError::InvalidConfig {
                message: format!("Invalid tags file: {e}"),
            }
        })
    }

    /// Save tag store to default location.
    pub fn save(&self) -> Result<()> {
        let path = default_tags_path()?;
        self.save_to(&path)
    }

    /// Save tag store to a specific path.
    pub fn save_to(&self, path: &Path) -> Result<()> {
        let content = serde_json::to_string_pretty(self).map_err(|e| {
            SnatchError::InvalidConfig {
                message: format!("Failed to serialize tags: {e}"),
            }
        })?;

        atomic_write(path, content.as_bytes())?;
        Ok(())
    }

    /// Get metadata for a session.
    pub fn get(&self, session_id: &str) -> Option<&SessionMeta> {
        // Support both full and short IDs
        self.sessions.get(session_id).or_else(|| {
            // Try to find by prefix match
            self.sessions
                .iter()
                .find(|(k, _)| k.starts_with(session_id))
                .map(|(_, v)| v)
        })
    }

    /// Get mutable metadata for a session, creating if needed.
    pub fn get_or_create(&mut self, session_id: &str) -> &mut SessionMeta {
        // First check if exists by prefix
        let full_id = self
            .sessions
            .keys()
            .find(|k| k.starts_with(session_id))
            .cloned();

        let key = full_id.unwrap_or_else(|| session_id.to_string());
        self.sessions.entry(key).or_default()
    }

    /// Set or update the name for a session.
    pub fn set_name(&mut self, session_id: &str, name: Option<String>) {
        let meta = self.get_or_create(session_id);
        meta.name = name;
        self.cleanup_empty(session_id);
    }

    /// Add a tag to a session.
    pub fn add_tag(&mut self, session_id: &str, tag: &str) -> bool {
        let meta = self.get_or_create(session_id);
        let tag = normalize_tag(tag);
        if meta.tags.contains(&tag) {
            false
        } else {
            meta.tags.push(tag);
            meta.tags.sort();
            true
        }
    }

    /// Remove a tag from a session.
    pub fn remove_tag(&mut self, session_id: &str, tag: &str) -> bool {
        if let Some(meta) = self.sessions.get_mut(session_id) {
            let tag = normalize_tag(tag);
            if let Some(pos) = meta.tags.iter().position(|t| t == &tag) {
                meta.tags.remove(pos);
                self.cleanup_empty(session_id);
                return true;
            }
        }
        // Try prefix match
        let full_id = self
            .sessions
            .keys()
            .find(|k| k.starts_with(session_id))
            .cloned();
        if let Some(full_id) = full_id {
            let tag = normalize_tag(tag);
            if let Some(meta) = self.sessions.get_mut(&full_id) {
                if let Some(pos) = meta.tags.iter().position(|t| t == &tag) {
                    meta.tags.remove(pos);
                    self.cleanup_empty(&full_id);
                    return true;
                }
            }
        }
        false
    }

    /// Set bookmark status.
    pub fn set_bookmark(&mut self, session_id: &str, bookmarked: bool) {
        let meta = self.get_or_create(session_id);
        meta.bookmarked = bookmarked;
        self.cleanup_empty(session_id);
    }

    /// Get all bookmarked session IDs.
    pub fn bookmarked_sessions(&self) -> Vec<&str> {
        self.sessions
            .iter()
            .filter(|(_, m)| m.bookmarked)
            .map(|(id, _)| id.as_str())
            .collect()
    }

    /// Set outcome classification for a session.
    pub fn set_outcome(&mut self, session_id: &str, outcome: Option<SessionOutcome>) {
        let meta = self.get_or_create(session_id);
        meta.outcome = outcome;
        self.cleanup_empty(session_id);
    }

    /// Get all sessions with a specific outcome.
    pub fn sessions_with_outcome(&self, outcome: SessionOutcome) -> Vec<&str> {
        self.sessions
            .iter()
            .filter(|(_, m)| m.outcome == Some(outcome))
            .map(|(id, _)| id.as_str())
            .collect()
    }

    /// Get outcome statistics across all sessions.
    pub fn outcome_stats(&self) -> OutcomeStats {
        let mut stats = OutcomeStats::default();
        for meta in self.sessions.values() {
            match meta.outcome {
                Some(SessionOutcome::Success) => stats.success += 1,
                Some(SessionOutcome::Partial) => stats.partial += 1,
                Some(SessionOutcome::Failed) => stats.failed += 1,
                Some(SessionOutcome::Abandoned) => stats.abandoned += 1,
                None => stats.unclassified += 1,
            }
        }
        stats
    }

    /// Add a note to a session.
    pub fn add_note(&mut self, session_id: &str, text: &str, label: Option<&str>) {
        let meta = self.get_or_create(session_id);
        let note = if let Some(label) = label {
            SessionNote::with_label(text, label)
        } else {
            SessionNote::new(text)
        };
        meta.notes.push(note);
    }

    /// Remove a note from a session by index.
    pub fn remove_note(&mut self, session_id: &str, index: usize) -> bool {
        // Try exact match first
        if let Some(meta) = self.sessions.get_mut(session_id) {
            if index < meta.notes.len() {
                meta.notes.remove(index);
                self.cleanup_empty(session_id);
                return true;
            }
            return false;
        }

        // Try prefix match
        let full_id = self
            .sessions
            .keys()
            .find(|k| k.starts_with(session_id))
            .cloned();
        if let Some(full_id) = full_id {
            if let Some(meta) = self.sessions.get_mut(&full_id) {
                if index < meta.notes.len() {
                    meta.notes.remove(index);
                    self.cleanup_empty(&full_id);
                    return true;
                }
            }
        }
        false
    }

    /// Clear all notes for a session.
    pub fn clear_notes(&mut self, session_id: &str) {
        if let Some(meta) = self.sessions.get_mut(session_id) {
            meta.notes.clear();
            self.cleanup_empty(session_id);
            return;
        }

        // Try prefix match
        let full_id = self
            .sessions
            .keys()
            .find(|k| k.starts_with(session_id))
            .cloned();
        if let Some(full_id) = full_id {
            if let Some(meta) = self.sessions.get_mut(&full_id) {
                meta.notes.clear();
                self.cleanup_empty(&full_id);
            }
        }
    }

    /// Get notes for a session.
    pub fn get_notes(&self, session_id: &str) -> Option<&[SessionNote]> {
        self.get(session_id).map(|m| m.notes.as_slice())
    }

    /// Get all sessions with notes.
    pub fn sessions_with_notes(&self) -> Vec<&str> {
        self.sessions
            .iter()
            .filter(|(_, m)| !m.notes.is_empty())
            .map(|(id, _)| id.as_str())
            .collect()
    }

    /// Count total notes across all sessions.
    pub fn note_count(&self) -> usize {
        self.sessions.values().map(|m| m.notes.len()).sum()
    }

    /// Get all sessions with a specific tag.
    pub fn sessions_with_tag(&self, tag: &str) -> Vec<&str> {
        let tag = normalize_tag(tag);
        self.sessions
            .iter()
            .filter(|(_, m)| m.tags.contains(&tag))
            .map(|(id, _)| id.as_str())
            .collect()
    }

    /// Get all unique tags.
    pub fn all_tags(&self) -> Vec<&str> {
        let mut tags: Vec<_> = self
            .sessions
            .values()
            .flat_map(|m| m.tags.iter().map(|s| s.as_str()))
            .collect();
        tags.sort_unstable();
        tags.dedup();
        tags
    }

    /// Remove entry if it has no useful metadata.
    fn cleanup_empty(&mut self, session_id: &str) {
        if let Some(meta) = self.sessions.get(session_id) {
            if meta.is_empty() {
                self.sessions.remove(session_id);
            }
        }
    }

    /// Resolve a short session ID to a full ID if it exists in the store.
    pub fn resolve_id<'a>(&'a self, short_id: &str) -> Option<&'a str> {
        if self.sessions.contains_key(short_id) {
            // Return the stored key, not the input
            return self.sessions.keys().find(|k| *k == short_id).map(|s| s.as_str());
        }
        self.sessions
            .keys()
            .find(|k| k.starts_with(short_id))
            .map(|s| s.as_str())
    }
}

/// Normalize a tag to lowercase with hyphens.
fn normalize_tag(tag: &str) -> String {
    tag.trim()
        .to_lowercase()
        .replace(' ', "-")
        .chars()
        .filter(|c| c.is_alphanumeric() || *c == '-' || *c == '_')
        .collect()
}

/// Get the default tags storage path.
pub fn default_tags_path() -> Result<PathBuf> {
    let config_dir = dirs::config_dir().ok_or_else(|| SnatchError::Unsupported {
        feature: "config directory discovery".to_string(),
    })?;

    Ok(config_dir.join("claude-snatch").join(TAGS_FILENAME))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_normalize_tag() {
        assert_eq!(normalize_tag("API Refactor"), "api-refactor");
        assert_eq!(normalize_tag("bug_fix"), "bug_fix");
        assert_eq!(normalize_tag("  TEST  "), "test");
        assert_eq!(normalize_tag("special!@#chars"), "specialchars");
    }

    #[test]
    fn test_session_meta_is_empty() {
        let meta = SessionMeta::default();
        assert!(meta.is_empty());

        let meta = SessionMeta {
            name: Some("test".to_string()),
            ..Default::default()
        };
        assert!(!meta.is_empty());

        let meta = SessionMeta {
            tags: vec!["tag".to_string()],
            ..Default::default()
        };
        assert!(!meta.is_empty());

        let meta = SessionMeta {
            bookmarked: true,
            ..Default::default()
        };
        assert!(!meta.is_empty());
    }

    #[test]
    fn test_tag_store_add_remove() {
        let mut store = TagStore::default();
        let session_id = "test-session-id";

        // Add tag
        assert!(store.add_tag(session_id, "feature"));
        assert!(!store.add_tag(session_id, "feature")); // Duplicate

        // Check tag exists
        let meta = store.get(session_id).unwrap();
        assert!(meta.tags.contains(&"feature".to_string()));

        // Remove tag
        assert!(store.remove_tag(session_id, "feature"));
        assert!(!store.remove_tag(session_id, "nonexistent"));

        // Entry should be cleaned up
        assert!(store.get(session_id).is_none());
    }

    #[test]
    fn test_tag_store_name() {
        let mut store = TagStore::default();
        let session_id = "test-session";

        store.set_name(session_id, Some("My Session".to_string()));
        assert_eq!(
            store.get(session_id).unwrap().name,
            Some("My Session".to_string())
        );

        store.set_name(session_id, None);
        assert!(store.get(session_id).is_none());
    }

    #[test]
    fn test_tag_store_bookmark() {
        let mut store = TagStore::default();
        let session_id = "test-session";

        store.set_bookmark(session_id, true);
        assert!(store.get(session_id).unwrap().bookmarked);

        let bookmarked = store.bookmarked_sessions();
        assert!(bookmarked.contains(&session_id));

        store.set_bookmark(session_id, false);
        assert!(store.get(session_id).is_none());
    }

    #[test]
    fn test_all_tags() {
        let mut store = TagStore::default();

        store.add_tag("session1", "feature");
        store.add_tag("session1", "urgent");
        store.add_tag("session2", "feature");
        store.add_tag("session2", "bug");

        let tags = store.all_tags();
        assert_eq!(tags, vec!["bug", "feature", "urgent"]);
    }

    #[test]
    fn test_sessions_with_tag() {
        let mut store = TagStore::default();

        store.add_tag("session1", "feature");
        store.add_tag("session2", "feature");
        store.add_tag("session3", "bug");

        let sessions = store.sessions_with_tag("feature");
        assert_eq!(sessions.len(), 2);
        assert!(sessions.contains(&"session1"));
        assert!(sessions.contains(&"session2"));
    }

    #[test]
    fn test_short_id_resolution() {
        let mut store = TagStore::default();
        let full_id = "40afc8a7-3fcb-4d29-b1ee-100b81b8c6c0";

        store.add_tag(full_id, "test");

        // Full ID lookup
        assert!(store.get(full_id).is_some());

        // Short ID lookup
        assert!(store.get("40afc8a7").is_some());
        assert!(store.get("40afc").is_some());
    }

    #[test]
    fn test_serialization() {
        let mut store = TagStore::default();
        store.add_tag("session1", "feature");
        store.set_name("session1", Some("My Session".to_string()));
        store.set_bookmark("session2", true);

        let json = serde_json::to_string(&store).unwrap();
        let loaded: TagStore = serde_json::from_str(&json).unwrap();

        assert_eq!(loaded.sessions.len(), 2);
        assert!(loaded.get("session1").unwrap().tags.contains(&"feature".to_string()));
        assert!(loaded.get("session2").unwrap().bookmarked);
    }

    #[test]
    fn test_session_outcome() {
        let mut store = TagStore::default();
        let session_id = "test-session";

        // Set outcome
        store.set_outcome(session_id, Some(SessionOutcome::Success));
        assert_eq!(store.get(session_id).unwrap().outcome, Some(SessionOutcome::Success));

        // Change outcome
        store.set_outcome(session_id, Some(SessionOutcome::Failed));
        assert_eq!(store.get(session_id).unwrap().outcome, Some(SessionOutcome::Failed));

        // Clear outcome
        store.set_outcome(session_id, None);
        assert!(store.get(session_id).is_none()); // Entry should be cleaned up
    }

    #[test]
    fn test_sessions_with_outcome() {
        let mut store = TagStore::default();

        store.set_outcome("session1", Some(SessionOutcome::Success));
        store.set_outcome("session2", Some(SessionOutcome::Success));
        store.set_outcome("session3", Some(SessionOutcome::Failed));
        store.set_outcome("session4", Some(SessionOutcome::Partial));

        let successful = store.sessions_with_outcome(SessionOutcome::Success);
        assert_eq!(successful.len(), 2);
        assert!(successful.contains(&"session1"));
        assert!(successful.contains(&"session2"));

        let failed = store.sessions_with_outcome(SessionOutcome::Failed);
        assert_eq!(failed.len(), 1);
        assert!(failed.contains(&"session3"));
    }

    #[test]
    fn test_outcome_stats() {
        let mut store = TagStore::default();

        store.set_outcome("s1", Some(SessionOutcome::Success));
        store.set_outcome("s2", Some(SessionOutcome::Success));
        store.set_outcome("s3", Some(SessionOutcome::Success));
        store.set_outcome("s4", Some(SessionOutcome::Partial));
        store.set_outcome("s5", Some(SessionOutcome::Failed));
        store.add_tag("s6", "unclassified"); // No outcome

        let stats = store.outcome_stats();
        assert_eq!(stats.success, 3);
        assert_eq!(stats.partial, 1);
        assert_eq!(stats.failed, 1);
        assert_eq!(stats.abandoned, 0);
        assert_eq!(stats.unclassified, 1);
        assert_eq!(stats.classified(), 5);
        assert!((stats.success_rate() - 60.0).abs() < 0.01);
    }

    #[test]
    fn test_outcome_parse() {
        assert_eq!("success".parse::<SessionOutcome>().unwrap(), SessionOutcome::Success);
        assert_eq!("s".parse::<SessionOutcome>().unwrap(), SessionOutcome::Success);
        assert_eq!("partial".parse::<SessionOutcome>().unwrap(), SessionOutcome::Partial);
        assert_eq!("p".parse::<SessionOutcome>().unwrap(), SessionOutcome::Partial);
        assert_eq!("failed".parse::<SessionOutcome>().unwrap(), SessionOutcome::Failed);
        assert_eq!("fail".parse::<SessionOutcome>().unwrap(), SessionOutcome::Failed);
        assert_eq!("f".parse::<SessionOutcome>().unwrap(), SessionOutcome::Failed);
        assert_eq!("abandoned".parse::<SessionOutcome>().unwrap(), SessionOutcome::Abandoned);
        assert_eq!("a".parse::<SessionOutcome>().unwrap(), SessionOutcome::Abandoned);
        assert!("invalid".parse::<SessionOutcome>().is_err());
    }

    #[test]
    fn test_outcome_serialization() {
        let mut store = TagStore::default();
        store.set_outcome("session1", Some(SessionOutcome::Success));
        store.set_name("session1", Some("Test".to_string()));

        let json = serde_json::to_string(&store).unwrap();
        let loaded: TagStore = serde_json::from_str(&json).unwrap();

        assert_eq!(loaded.get("session1").unwrap().outcome, Some(SessionOutcome::Success));
    }

    #[test]
    fn test_session_note_new() {
        let note = SessionNote::new("Test note");
        assert_eq!(note.text, "Test note");
        assert!(note.label.is_none());
    }

    #[test]
    fn test_session_note_with_label() {
        let note = SessionNote::with_label("Test note", "todo");
        assert_eq!(note.text, "Test note");
        assert_eq!(note.label, Some("todo".to_string()));
    }

    #[test]
    fn test_add_note() {
        let mut store = TagStore::default();
        let session_id = "test-session";

        store.add_note(session_id, "First note", None);
        store.add_note(session_id, "Second note", Some("important"));

        let notes = store.get_notes(session_id).unwrap();
        assert_eq!(notes.len(), 2);
        assert_eq!(notes[0].text, "First note");
        assert!(notes[0].label.is_none());
        assert_eq!(notes[1].text, "Second note");
        assert_eq!(notes[1].label, Some("important".to_string()));
    }

    #[test]
    fn test_remove_note() {
        let mut store = TagStore::default();
        let session_id = "test-session";

        store.add_note(session_id, "First", None);
        store.add_note(session_id, "Second", None);
        store.add_note(session_id, "Third", None);

        // Remove middle note
        assert!(store.remove_note(session_id, 1));
        let notes = store.get_notes(session_id).unwrap();
        assert_eq!(notes.len(), 2);
        assert_eq!(notes[0].text, "First");
        assert_eq!(notes[1].text, "Third");

        // Invalid index
        assert!(!store.remove_note(session_id, 10));
    }

    #[test]
    fn test_clear_notes() {
        let mut store = TagStore::default();
        let session_id = "test-session";

        store.add_note(session_id, "Note 1", None);
        store.add_note(session_id, "Note 2", None);

        store.clear_notes(session_id);
        assert!(store.get(session_id).is_none()); // Entry should be cleaned up
    }

    #[test]
    fn test_sessions_with_notes() {
        let mut store = TagStore::default();

        store.add_note("session1", "Note", None);
        store.add_note("session2", "Note", None);
        store.add_tag("session3", "no-notes");

        let sessions = store.sessions_with_notes();
        assert_eq!(sessions.len(), 2);
        assert!(sessions.contains(&"session1"));
        assert!(sessions.contains(&"session2"));
    }

    #[test]
    fn test_note_count() {
        let mut store = TagStore::default();

        store.add_note("session1", "Note 1", None);
        store.add_note("session1", "Note 2", None);
        store.add_note("session2", "Note 3", None);

        assert_eq!(store.note_count(), 3);
    }

    #[test]
    fn test_notes_serialization() {
        let mut store = TagStore::default();
        store.add_note("session1", "Test note", Some("todo"));

        let json = serde_json::to_string(&store).unwrap();
        let loaded: TagStore = serde_json::from_str(&json).unwrap();

        let notes = loaded.get_notes("session1").unwrap();
        assert_eq!(notes.len(), 1);
        assert_eq!(notes[0].text, "Test note");
        assert_eq!(notes[0].label, Some("todo".to_string()));
    }

    #[test]
    fn test_session_meta_is_empty_with_notes() {
        let meta = SessionMeta {
            notes: vec![SessionNote::new("test")],
            ..Default::default()
        };
        assert!(!meta.is_empty());
    }
}
