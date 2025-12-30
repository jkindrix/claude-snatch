//! Session tagging and naming.
//!
//! Provides human-friendly labels for sessions, stored in a JSON file
//! separate from the Claude session data.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{Result, SnatchError};
use crate::util::atomic_write;

/// Tag storage filename.
const TAGS_FILENAME: &str = "tags.json";

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
}

impl SessionMeta {
    /// Check if this metadata is empty (no name, tags, or bookmark).
    pub fn is_empty(&self) -> bool {
        self.name.is_none() && self.tags.is_empty() && !self.bookmarked
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
        tags.sort();
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
}
