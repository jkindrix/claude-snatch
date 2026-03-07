//! Decision persistence for Claude Code sessions.
//!
//! Provides a structured decision registry that persists across sessions
//! and compactions. Decisions are stored as JSON in the project's memory directory
//! (`~/.claude/projects/<project>/memory/decisions.json`).

use std::fmt;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::error::{Result, SnatchError};
use crate::util::atomic_write;

/// Decision status.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DecisionStatus {
    /// Decision has been proposed but not confirmed.
    Proposed,
    /// Decision has been confirmed by the user.
    Confirmed,
    /// Decision has been replaced by a newer decision.
    Superseded,
    /// Decision was abandoned or is no longer relevant.
    Abandoned,
}

impl fmt::Display for DecisionStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DecisionStatus::Proposed => write!(f, "proposed"),
            DecisionStatus::Confirmed => write!(f, "confirmed"),
            DecisionStatus::Superseded => write!(f, "superseded"),
            DecisionStatus::Abandoned => write!(f, "abandoned"),
        }
    }
}

impl DecisionStatus {
    /// Parse a status string. Returns None for unrecognized values.
    pub fn parse(s: &str) -> Option<Self> {
        match s.to_lowercase().as_str() {
            "proposed" => Some(DecisionStatus::Proposed),
            "confirmed" => Some(DecisionStatus::Confirmed),
            "superseded" | "replaced" => Some(DecisionStatus::Superseded),
            "abandoned" | "cancelled" | "canceled" => Some(DecisionStatus::Abandoned),
            _ => None,
        }
    }

    /// Whether this decision is "active" (should be injected on compact/startup).
    pub fn is_active(&self) -> bool {
        matches!(self, DecisionStatus::Proposed | DecisionStatus::Confirmed)
    }
}

/// A tracked decision.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Decision {
    /// Unique decision ID (monotonically increasing).
    pub id: u64,
    /// Short decision title.
    pub title: String,
    /// Longer description of the decision.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Current status.
    pub status: DecisionStatus,
    /// Session where this decision was made.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    /// Confidence score (0.0 to 1.0).
    #[serde(default = "default_confidence")]
    pub confidence: f64,
    /// When the decision was created.
    pub created_at: DateTime<Utc>,
    /// When the decision was last updated.
    pub updated_at: DateTime<Utc>,
    /// ID of the decision that supersedes this one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub superseded_by: Option<u64>,
    /// Topic tags for grouping and filtering.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    /// References to related session IDs.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub references: Vec<String>,
}

fn default_confidence() -> f64 {
    1.0
}

/// Persistent decision store.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DecisionStore {
    /// All tracked decisions.
    pub decisions: Vec<Decision>,
    /// Next ID to assign.
    pub next_id: u64,
}

impl Default for DecisionStore {
    fn default() -> Self {
        Self {
            decisions: Vec::new(),
            next_id: 1,
        }
    }
}

impl DecisionStore {
    /// Add a new decision. Returns the assigned ID.
    pub fn add_decision(
        &mut self,
        title: String,
        description: Option<String>,
        session_id: Option<String>,
        confidence: Option<f64>,
        tags: Vec<String>,
    ) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        let now = Utc::now();
        self.decisions.push(Decision {
            id,
            title,
            description,
            status: DecisionStatus::Proposed,
            session_id,
            confidence: confidence.unwrap_or(1.0),
            created_at: now,
            updated_at: now,
            superseded_by: None,
            tags,
            references: Vec::new(),
        });
        id
    }

    /// Update an existing decision. Returns true if found.
    pub fn update_decision(
        &mut self,
        id: u64,
        status: Option<DecisionStatus>,
        description: Option<String>,
        confidence: Option<f64>,
        tags: Option<Vec<String>>,
    ) -> bool {
        if let Some(decision) = self.decisions.iter_mut().find(|d| d.id == id) {
            if let Some(s) = status {
                decision.status = s;
            }
            if let Some(d) = description {
                decision.description = Some(d);
            }
            if let Some(c) = confidence {
                decision.confidence = c;
            }
            if let Some(t) = tags {
                decision.tags = t;
            }
            decision.updated_at = Utc::now();
            true
        } else {
            false
        }
    }

    /// Remove a decision by ID. Returns true if found and removed.
    pub fn remove_decision(&mut self, id: u64) -> bool {
        let len_before = self.decisions.len();
        self.decisions.retain(|d| d.id != id);
        self.decisions.len() < len_before
    }

    /// Supersede a decision with another. Returns true if both found.
    pub fn supersede_decision(&mut self, old_id: u64, new_id: u64) -> bool {
        // Verify new_id exists
        if !self.decisions.iter().any(|d| d.id == new_id) {
            return false;
        }
        if let Some(old) = self.decisions.iter_mut().find(|d| d.id == old_id) {
            old.status = DecisionStatus::Superseded;
            old.superseded_by = Some(new_id);
            old.updated_at = Utc::now();
            true
        } else {
            false
        }
    }

    /// Get all active decisions (proposed or confirmed).
    pub fn active_decisions(&self) -> Vec<&Decision> {
        self.decisions.iter().filter(|d| d.status.is_active()).collect()
    }

    /// Format active decisions for hook injection.
    pub fn format_decisions_for_injection(&self) -> Option<String> {
        let active = self.active_decisions();
        if active.is_empty() {
            return None;
        }

        let mut lines = vec!["### Active Decisions".to_string()];
        for d in &active {
            let conf = if d.confidence < 1.0 {
                format!(" (confidence: {:.0}%)", d.confidence * 100.0)
            } else {
                String::new()
            };
            let tags = if d.tags.is_empty() {
                String::new()
            } else {
                format!(" [{}]", d.tags.join(", "))
            };
            lines.push(format!("- [{}] #{}: {}{}{}", d.status, d.id, d.title, conf, tags));
        }
        Some(lines.join("\n"))
    }
}

/// Resolve the decisions.json path for a project directory.
pub fn decisions_path(project_dir: &Path) -> PathBuf {
    project_dir.join("memory").join("decisions.json")
}

/// Load decisions from a project directory.
///
/// Returns a default (empty) store if the file doesn't exist.
pub fn load_decisions(project_dir: &Path) -> Result<DecisionStore> {
    let path = decisions_path(project_dir);
    if !path.exists() {
        return Ok(DecisionStore::default());
    }
    let content = std::fs::read_to_string(&path).map_err(|source| SnatchError::IoError {
        context: format!("Failed to read decisions file {}", path.display()),
        source,
    })?;
    serde_json::from_str(&content).map_err(|source| SnatchError::SerializationError {
        context: format!("Failed to parse decisions file {}", path.display()),
        source,
    })
}

/// Save decisions to a project directory (atomic write).
///
/// Creates the `memory/` subdirectory if it doesn't exist.
pub fn save_decisions(project_dir: &Path, store: &DecisionStore) -> Result<()> {
    let path = decisions_path(project_dir);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|source| SnatchError::IoError {
            context: format!("Failed to create memory directory {}", parent.display()),
            source,
        })?;
    }
    let json = serde_json::to_string_pretty(store).map_err(|source| SnatchError::SerializationError {
        context: "Failed to serialize decisions".to_string(),
        source,
    })?;
    atomic_write(&path, json.as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_decision_status_parse() {
        assert_eq!(DecisionStatus::parse("proposed"), Some(DecisionStatus::Proposed));
        assert_eq!(DecisionStatus::parse("confirmed"), Some(DecisionStatus::Confirmed));
        assert_eq!(DecisionStatus::parse("superseded"), Some(DecisionStatus::Superseded));
        assert_eq!(DecisionStatus::parse("replaced"), Some(DecisionStatus::Superseded));
        assert_eq!(DecisionStatus::parse("abandoned"), Some(DecisionStatus::Abandoned));
        assert_eq!(DecisionStatus::parse("CONFIRMED"), Some(DecisionStatus::Confirmed));
        assert_eq!(DecisionStatus::parse("bogus"), None);
    }

    #[test]
    fn test_decision_status_is_active() {
        assert!(DecisionStatus::Proposed.is_active());
        assert!(DecisionStatus::Confirmed.is_active());
        assert!(!DecisionStatus::Superseded.is_active());
        assert!(!DecisionStatus::Abandoned.is_active());
    }

    #[test]
    fn test_add_decision() {
        let mut store = DecisionStore::default();
        let id1 = store.add_decision("No Drop trait".into(), Some("Manual resource management".into()), None, None, vec!["memory".into()]);
        let id2 = store.add_decision("Use MVS".into(), None, Some("abc123".into()), Some(0.9), vec![]);

        assert_eq!(id1, 1);
        assert_eq!(id2, 2);
        assert_eq!(store.decisions.len(), 2);
        assert_eq!(store.decisions[0].title, "No Drop trait");
        assert_eq!(store.decisions[0].status, DecisionStatus::Proposed);
        assert_eq!(store.decisions[0].confidence, 1.0);
        assert_eq!(store.decisions[0].tags, vec!["memory"]);
        assert_eq!(store.decisions[1].confidence, 0.9);
        assert_eq!(store.decisions[1].session_id.as_deref(), Some("abc123"));
    }

    #[test]
    fn test_update_decision() {
        let mut store = DecisionStore::default();
        store.add_decision("Test".into(), None, None, None, vec![]);

        assert!(store.update_decision(1, Some(DecisionStatus::Confirmed), None, None, None));
        assert_eq!(store.decisions[0].status, DecisionStatus::Confirmed);

        assert!(store.update_decision(1, None, None, Some(0.8), Some(vec!["tag1".into()])));
        assert_eq!(store.decisions[0].confidence, 0.8);
        assert_eq!(store.decisions[0].tags, vec!["tag1"]);

        assert!(!store.update_decision(99, Some(DecisionStatus::Confirmed), None, None, None));
    }

    #[test]
    fn test_supersede_decision() {
        let mut store = DecisionStore::default();
        store.add_decision("Old approach".into(), None, None, None, vec![]);
        store.add_decision("New approach".into(), None, None, None, vec![]);

        assert!(store.supersede_decision(1, 2));
        assert_eq!(store.decisions[0].status, DecisionStatus::Superseded);
        assert_eq!(store.decisions[0].superseded_by, Some(2));

        // Can't supersede with non-existent ID
        assert!(!store.supersede_decision(2, 99));
    }

    #[test]
    fn test_remove_decision() {
        let mut store = DecisionStore::default();
        store.add_decision("D1".into(), None, None, None, vec![]);
        store.add_decision("D2".into(), None, None, None, vec![]);

        assert!(store.remove_decision(1));
        assert_eq!(store.decisions.len(), 1);
        assert!(!store.remove_decision(1));
    }

    #[test]
    fn test_active_decisions() {
        let mut store = DecisionStore::default();
        store.add_decision("Proposed".into(), None, None, None, vec![]);
        store.add_decision("Confirmed".into(), None, None, None, vec![]);
        store.update_decision(2, Some(DecisionStatus::Confirmed), None, None, None);
        store.add_decision("Superseded".into(), None, None, None, vec![]);
        store.update_decision(3, Some(DecisionStatus::Superseded), None, None, None);

        let active = store.active_decisions();
        assert_eq!(active.len(), 2);
        assert_eq!(active[0].id, 1);
        assert_eq!(active[1].id, 2);
    }

    #[test]
    fn test_format_decisions_for_injection() {
        let mut store = DecisionStore::default();
        store.add_decision("No Drop trait".into(), None, None, Some(0.9), vec!["memory".into()]);
        store.update_decision(1, Some(DecisionStatus::Confirmed), None, None, None);
        store.add_decision("Use MVS".into(), None, None, None, vec![]);

        let formatted = store.format_decisions_for_injection().unwrap();
        assert!(formatted.contains("### Active Decisions"));
        assert!(formatted.contains("[confirmed] #1: No Drop trait (confidence: 90%) [memory]"));
        assert!(formatted.contains("[proposed] #2: Use MVS"));
    }

    #[test]
    fn test_format_decisions_empty() {
        let store = DecisionStore::default();
        assert!(store.format_decisions_for_injection().is_none());
    }

    #[test]
    fn test_load_save_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let project_dir = dir.path();

        let store = load_decisions(project_dir).unwrap();
        assert!(store.decisions.is_empty());

        let mut store = DecisionStore::default();
        store.add_decision("Test decision".into(), Some("Details".into()), Some("sess1".into()), Some(0.85), vec!["tag1".into()]);
        store.update_decision(1, Some(DecisionStatus::Confirmed), None, None, None);

        save_decisions(project_dir, &store).unwrap();

        let loaded = load_decisions(project_dir).unwrap();
        assert_eq!(loaded.decisions.len(), 1);
        assert_eq!(loaded.decisions[0].title, "Test decision");
        assert_eq!(loaded.decisions[0].status, DecisionStatus::Confirmed);
        assert_eq!(loaded.decisions[0].confidence, 0.85);
        assert_eq!(loaded.decisions[0].session_id.as_deref(), Some("sess1"));
        assert_eq!(loaded.next_id, 2);
    }

    #[test]
    fn test_serde_roundtrip() {
        let mut store = DecisionStore::default();
        store.add_decision("D1".into(), None, None, None, vec![]);
        store.add_decision("D2".into(), Some("desc".into()), None, Some(0.5), vec!["a".into(), "b".into()]);
        store.supersede_decision(1, 2);

        let json = serde_json::to_string_pretty(&store).unwrap();
        let parsed: DecisionStore = serde_json::from_str(&json).unwrap();

        assert_eq!(parsed.decisions.len(), 2);
        assert_eq!(parsed.next_id, 3);
        assert_eq!(parsed.decisions[0].status, DecisionStatus::Superseded);
        assert_eq!(parsed.decisions[0].superseded_by, Some(2));
    }
}
