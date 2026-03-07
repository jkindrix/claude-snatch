//! Message-level tagging for Claude Code sessions.
//!
//! Provides sidecar storage for tagging individual messages (by UUID) within sessions.
//! Tags are stored per-project in `~/.claude/projects/<project>/memory/message-tags.json`.
//! Original JSONL files are never modified.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::error::{Result, SnatchError};
use crate::util::atomic_write;

/// A tag applied to a specific message.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MessageTag {
    /// The tag string (e.g., "decision", "reversal", "decision:drop-trait").
    pub tag: String,
    /// When the tag was applied.
    pub created_at: DateTime<Utc>,
    /// How the tag was created.
    #[serde(default = "default_source")]
    pub source: TagSource,
}

fn default_source() -> TagSource {
    TagSource::Manual
}

/// How a tag was created.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TagSource {
    /// Manually added by user or AI.
    Manual,
    /// Auto-detected by the detection heuristic.
    AutoDetected,
}

/// Tags for a single message.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaggedMessage {
    /// Session ID containing this message.
    pub session_id: String,
    /// UUID of the tagged message.
    pub message_uuid: String,
    /// Tags applied to this message.
    pub tags: Vec<MessageTag>,
}

/// Store of all message tags for a project.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct MessageTagStore {
    /// Tagged messages keyed by message UUID.
    #[serde(default)]
    pub messages: HashMap<String, TaggedMessage>,
}

impl MessageTagStore {
    /// Add a tag to a message. Returns true if the tag was new.
    pub fn add_tag(
        &mut self,
        session_id: &str,
        message_uuid: &str,
        tag: &str,
        source: TagSource,
    ) -> bool {
        let entry = self.messages
            .entry(message_uuid.to_string())
            .or_insert_with(|| TaggedMessage {
                session_id: session_id.to_string(),
                message_uuid: message_uuid.to_string(),
                tags: Vec::new(),
            });

        // Check if tag already exists
        if entry.tags.iter().any(|t| t.tag == tag) {
            return false;
        }

        entry.tags.push(MessageTag {
            tag: tag.to_string(),
            created_at: Utc::now(),
            source,
        });
        true
    }

    /// Remove a tag from a message. Returns true if the tag was found and removed.
    pub fn remove_tag(&mut self, message_uuid: &str, tag: &str) -> bool {
        if let Some(entry) = self.messages.get_mut(message_uuid) {
            let len_before = entry.tags.len();
            entry.tags.retain(|t| t.tag != tag);
            let removed = entry.tags.len() < len_before;

            // Clean up empty entries
            if entry.tags.is_empty() {
                self.messages.remove(message_uuid);
            }

            removed
        } else {
            false
        }
    }

    /// Get all tags for a message.
    pub fn get_tags(&self, message_uuid: &str) -> Vec<&MessageTag> {
        self.messages
            .get(message_uuid)
            .map(|entry| entry.tags.iter().collect())
            .unwrap_or_default()
    }

    /// Find all message UUIDs with a given tag (exact or prefix match).
    pub fn messages_with_tag(&self, tag: &str) -> Vec<&TaggedMessage> {
        self.messages
            .values()
            .filter(|entry| {
                entry.tags.iter().any(|t| {
                    t.tag == tag || t.tag.starts_with(&format!("{}:", tag))
                })
            })
            .collect()
    }

    /// Find all message UUIDs in a given session.
    pub fn messages_in_session(&self, session_id: &str) -> Vec<&TaggedMessage> {
        self.messages
            .values()
            .filter(|entry| entry.session_id == session_id)
            .collect()
    }

    /// Get all unique tags across all messages.
    pub fn all_tags(&self) -> Vec<String> {
        let mut tags: Vec<String> = self.messages
            .values()
            .flat_map(|entry| entry.tags.iter().map(|t| t.tag.clone()))
            .collect();
        tags.sort();
        tags.dedup();
        tags
    }

    /// Total number of tagged messages.
    pub fn count(&self) -> usize {
        self.messages.len()
    }

    /// Clear all tags for a session.
    pub fn clear_session(&mut self, session_id: &str) {
        self.messages.retain(|_, entry| entry.session_id != session_id);
    }
}

/// Resolve the message-tags.json path for a project directory.
pub fn message_tags_path(project_dir: &Path) -> PathBuf {
    project_dir.join("memory").join("message-tags.json")
}

/// Load message tags from a project directory.
pub fn load_message_tags(project_dir: &Path) -> Result<MessageTagStore> {
    let path = message_tags_path(project_dir);
    if !path.exists() {
        return Ok(MessageTagStore::default());
    }
    let content = std::fs::read_to_string(&path).map_err(|source| SnatchError::IoError {
        context: format!("Failed to read message tags file {}", path.display()),
        source,
    })?;
    serde_json::from_str(&content).map_err(|source| SnatchError::SerializationError {
        context: format!("Failed to parse message tags file {}", path.display()),
        source,
    })
}

/// Save message tags to a project directory (atomic write).
pub fn save_message_tags(project_dir: &Path, store: &MessageTagStore) -> Result<()> {
    let path = message_tags_path(project_dir);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|source| SnatchError::IoError {
            context: format!("Failed to create memory directory {}", parent.display()),
            source,
        })?;
    }
    let json = serde_json::to_string_pretty(store).map_err(|source| SnatchError::SerializationError {
        context: "Failed to serialize message tags".to_string(),
        source,
    })?;
    atomic_write(&path, json.as_bytes())
}

/// Well-known tag categories.
pub mod well_known {
    /// Auto-suggestible tags (can be detected from conversation structure).
    pub const DECISION: &str = "decision";
    /// A previous decision was reversed.
    pub const REVERSAL: &str = "reversal";
    /// User corrected the AI.
    pub const CORRECTION: &str = "correction";
    /// Bug discovered or fixed.
    pub const BUG: &str = "bug";
    /// Significant deliverable completed.
    pub const MILESTONE: &str = "milestone";
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_add_tag() {
        let mut store = MessageTagStore::default();
        assert!(store.add_tag("sess1", "uuid1", "decision", TagSource::Manual));
        assert!(!store.add_tag("sess1", "uuid1", "decision", TagSource::Manual)); // duplicate
        assert!(store.add_tag("sess1", "uuid1", "reversal", TagSource::AutoDetected));

        assert_eq!(store.count(), 1); // 1 message, 2 tags
        assert_eq!(store.get_tags("uuid1").len(), 2);
    }

    #[test]
    fn test_remove_tag() {
        let mut store = MessageTagStore::default();
        store.add_tag("sess1", "uuid1", "decision", TagSource::Manual);
        store.add_tag("sess1", "uuid1", "reversal", TagSource::Manual);

        assert!(store.remove_tag("uuid1", "decision"));
        assert_eq!(store.get_tags("uuid1").len(), 1);

        assert!(store.remove_tag("uuid1", "reversal"));
        assert_eq!(store.count(), 0); // entry removed when empty

        assert!(!store.remove_tag("uuid1", "nonexistent"));
    }

    #[test]
    fn test_messages_with_tag() {
        let mut store = MessageTagStore::default();
        store.add_tag("sess1", "uuid1", "decision:drop-trait", TagSource::Manual);
        store.add_tag("sess1", "uuid2", "decision:memory-model", TagSource::Manual);
        store.add_tag("sess2", "uuid3", "bug", TagSource::AutoDetected);

        // Exact match
        let bugs = store.messages_with_tag("bug");
        assert_eq!(bugs.len(), 1);

        // Prefix match: "decision" matches "decision:*"
        let decisions = store.messages_with_tag("decision");
        assert_eq!(decisions.len(), 2);
    }

    #[test]
    fn test_messages_in_session() {
        let mut store = MessageTagStore::default();
        store.add_tag("sess1", "uuid1", "decision", TagSource::Manual);
        store.add_tag("sess1", "uuid2", "bug", TagSource::Manual);
        store.add_tag("sess2", "uuid3", "decision", TagSource::Manual);

        let sess1_msgs = store.messages_in_session("sess1");
        assert_eq!(sess1_msgs.len(), 2);
    }

    #[test]
    fn test_all_tags() {
        let mut store = MessageTagStore::default();
        store.add_tag("sess1", "uuid1", "decision", TagSource::Manual);
        store.add_tag("sess1", "uuid2", "decision", TagSource::Manual);
        store.add_tag("sess1", "uuid3", "bug", TagSource::Manual);

        let tags = store.all_tags();
        assert_eq!(tags, vec!["bug", "decision"]);
    }

    #[test]
    fn test_clear_session() {
        let mut store = MessageTagStore::default();
        store.add_tag("sess1", "uuid1", "decision", TagSource::Manual);
        store.add_tag("sess1", "uuid2", "bug", TagSource::Manual);
        store.add_tag("sess2", "uuid3", "decision", TagSource::Manual);

        store.clear_session("sess1");
        assert_eq!(store.count(), 1);
        assert_eq!(store.messages_in_session("sess1").len(), 0);
        assert_eq!(store.messages_in_session("sess2").len(), 1);
    }

    #[test]
    fn test_load_save_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let project_dir = dir.path();

        let store = load_message_tags(project_dir).unwrap();
        assert_eq!(store.count(), 0);

        let mut store = MessageTagStore::default();
        store.add_tag("sess1", "uuid1", "decision:drop-trait", TagSource::Manual);
        store.add_tag("sess1", "uuid2", "reversal", TagSource::AutoDetected);

        save_message_tags(project_dir, &store).unwrap();

        let loaded = load_message_tags(project_dir).unwrap();
        assert_eq!(loaded.count(), 2);
        assert_eq!(loaded.all_tags(), vec!["decision:drop-trait", "reversal"]);
    }
}
