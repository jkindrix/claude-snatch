//! Goal persistence for Claude Code sessions.
//!
//! Provides a simple goal tracking system that persists across sessions
//! and compactions. Goals are stored as JSON in the project's memory directory
//! (`~/.claude/projects/<project>/memory/goals.json`).
//!
//! Claude (the LLM) is the goal recognizer — this module just provides
//! persistence and formatting for hook injection.

use std::fmt;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::error::{Result, SnatchError};
use crate::util::atomic_write;

/// Goal status.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GoalStatus {
    /// Goal has been identified but not started.
    Open,
    /// Goal is actively being worked on.
    InProgress,
    /// Goal has been completed.
    Done,
    /// Goal was abandoned or is no longer relevant.
    Abandoned,
}

impl fmt::Display for GoalStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            GoalStatus::Open => write!(f, "open"),
            GoalStatus::InProgress => write!(f, "in_progress"),
            GoalStatus::Done => write!(f, "done"),
            GoalStatus::Abandoned => write!(f, "abandoned"),
        }
    }
}

impl GoalStatus {
    /// Parse a status string. Returns None for unrecognized values.
    pub fn parse(s: &str) -> Option<Self> {
        match s.to_lowercase().as_str() {
            "open" => Some(GoalStatus::Open),
            "in_progress" | "in-progress" | "inprogress" => Some(GoalStatus::InProgress),
            "done" | "complete" | "completed" => Some(GoalStatus::Done),
            "abandoned" | "cancelled" | "canceled" => Some(GoalStatus::Abandoned),
            _ => None,
        }
    }

    /// Whether this goal is "active" (should be injected on compact/startup).
    pub fn is_active(&self) -> bool {
        matches!(self, GoalStatus::Open | GoalStatus::InProgress)
    }
}

/// A tracked goal.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Goal {
    /// Unique goal ID (monotonically increasing).
    pub id: u64,
    /// Goal description text.
    pub text: String,
    /// Current status.
    pub status: GoalStatus,
    /// When the goal was created.
    pub created_at: DateTime<Utc>,
    /// When the goal was last updated.
    pub updated_at: DateTime<Utc>,
    /// Optional progress notes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub progress: Option<String>,
}

/// Persistent goal store.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GoalStore {
    /// All tracked goals.
    pub goals: Vec<Goal>,
    /// Next ID to assign.
    pub next_id: u64,
}

impl Default for GoalStore {
    fn default() -> Self {
        Self {
            goals: Vec::new(),
            next_id: 1,
        }
    }
}

impl GoalStore {
    /// Add a new goal. Returns the assigned ID.
    pub fn add_goal(&mut self, text: String, progress: Option<String>) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        let now = Utc::now();
        self.goals.push(Goal {
            id,
            text,
            status: GoalStatus::Open,
            created_at: now,
            updated_at: now,
            progress,
        });
        id
    }

    /// Update an existing goal. Returns true if found.
    pub fn update_goal(
        &mut self,
        id: u64,
        status: Option<GoalStatus>,
        progress: Option<String>,
    ) -> bool {
        if let Some(goal) = self.goals.iter_mut().find(|g| g.id == id) {
            if let Some(s) = status {
                goal.status = s;
            }
            if let Some(p) = progress {
                goal.progress = Some(p);
            }
            goal.updated_at = Utc::now();
            true
        } else {
            false
        }
    }

    /// Remove a goal by ID. Returns true if found and removed.
    pub fn remove_goal(&mut self, id: u64) -> bool {
        let len_before = self.goals.len();
        self.goals.retain(|g| g.id != id);
        self.goals.len() < len_before
    }

    /// Get all active goals (open or in_progress).
    pub fn active_goals(&self) -> Vec<&Goal> {
        self.goals.iter().filter(|g| g.status.is_active()).collect()
    }

    /// Format active goals for hook injection.
    ///
    /// Returns compact markdown suitable for injecting into context:
    /// ```text
    /// ### Active Goals
    /// - [in_progress] #2: Close friction gaps (progress: Fixed counts...)
    /// - [open] #3: Add digest tool
    /// ```
    ///
    /// Returns `None` if there are no active goals.
    pub fn format_goals_for_injection(&self) -> Option<String> {
        let active = self.active_goals();
        if active.is_empty() {
            return None;
        }

        let mut lines = vec!["### Active Goals".to_string()];
        for goal in &active {
            let mut line = format!("- [{}] #{}: {}", goal.status, goal.id, goal.text);
            if let Some(ref p) = goal.progress {
                line.push_str(&format!(" (progress: {p})"));
            }
            lines.push(line);
        }
        Some(lines.join("\n"))
    }
}

/// Resolve the goals.json path for a project directory.
pub fn goals_path(project_dir: &Path) -> PathBuf {
    project_dir.join("memory").join("goals.json")
}

/// Load goals from a project directory.
///
/// Returns a default (empty) store if the file doesn't exist.
pub fn load_goals(project_dir: &Path) -> Result<GoalStore> {
    let path = goals_path(project_dir);
    if !path.exists() {
        return Ok(GoalStore::default());
    }
    let content = std::fs::read_to_string(&path).map_err(|source| SnatchError::IoError {
        context: format!("Failed to read goals file {}", path.display()),
        source,
    })?;
    serde_json::from_str(&content).map_err(|source| SnatchError::SerializationError {
        context: format!("Failed to parse goals file {}", path.display()),
        source,
    })
}

/// Save goals to a project directory (atomic write).
///
/// Creates the `memory/` subdirectory if it doesn't exist.
pub fn save_goals(project_dir: &Path, store: &GoalStore) -> Result<()> {
    let path = goals_path(project_dir);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|source| SnatchError::IoError {
            context: format!("Failed to create memory directory {}", parent.display()),
            source,
        })?;
    }
    let json = serde_json::to_string_pretty(store).map_err(|source| SnatchError::SerializationError {
        context: "Failed to serialize goals".to_string(),
        source,
    })?;
    atomic_write(&path, json.as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_goal_status_parse() {
        assert_eq!(GoalStatus::parse("open"), Some(GoalStatus::Open));
        assert_eq!(GoalStatus::parse("in_progress"), Some(GoalStatus::InProgress));
        assert_eq!(GoalStatus::parse("in-progress"), Some(GoalStatus::InProgress));
        assert_eq!(GoalStatus::parse("done"), Some(GoalStatus::Done));
        assert_eq!(GoalStatus::parse("complete"), Some(GoalStatus::Done));
        assert_eq!(GoalStatus::parse("abandoned"), Some(GoalStatus::Abandoned));
        assert_eq!(GoalStatus::parse("cancelled"), Some(GoalStatus::Abandoned));
        assert_eq!(GoalStatus::parse("OPEN"), Some(GoalStatus::Open));
        assert_eq!(GoalStatus::parse("bogus"), None);
    }

    #[test]
    fn test_goal_status_is_active() {
        assert!(GoalStatus::Open.is_active());
        assert!(GoalStatus::InProgress.is_active());
        assert!(!GoalStatus::Done.is_active());
        assert!(!GoalStatus::Abandoned.is_active());
    }

    #[test]
    fn test_add_goal() {
        let mut store = GoalStore::default();
        let id1 = store.add_goal("First goal".into(), None);
        let id2 = store.add_goal("Second goal".into(), Some("started".into()));

        assert_eq!(id1, 1);
        assert_eq!(id2, 2);
        assert_eq!(store.goals.len(), 2);
        assert_eq!(store.next_id, 3);
        assert_eq!(store.goals[0].text, "First goal");
        assert_eq!(store.goals[0].status, GoalStatus::Open);
        assert!(store.goals[0].progress.is_none());
        assert_eq!(store.goals[1].progress.as_deref(), Some("started"));
    }

    #[test]
    fn test_update_goal() {
        let mut store = GoalStore::default();
        store.add_goal("Goal".into(), None);

        assert!(store.update_goal(1, Some(GoalStatus::InProgress), Some("working on it".into())));
        assert_eq!(store.goals[0].status, GoalStatus::InProgress);
        assert_eq!(store.goals[0].progress.as_deref(), Some("working on it"));

        // Update only status
        assert!(store.update_goal(1, Some(GoalStatus::Done), None));
        assert_eq!(store.goals[0].status, GoalStatus::Done);
        assert_eq!(store.goals[0].progress.as_deref(), Some("working on it")); // unchanged

        // Non-existent ID
        assert!(!store.update_goal(99, Some(GoalStatus::Done), None));
    }

    #[test]
    fn test_remove_goal() {
        let mut store = GoalStore::default();
        store.add_goal("Goal 1".into(), None);
        store.add_goal("Goal 2".into(), None);

        assert!(store.remove_goal(1));
        assert_eq!(store.goals.len(), 1);
        assert_eq!(store.goals[0].id, 2);

        assert!(!store.remove_goal(1)); // already removed
    }

    #[test]
    fn test_active_goals() {
        let mut store = GoalStore::default();
        store.add_goal("Open goal".into(), None);
        store.add_goal("In progress goal".into(), None);
        store.update_goal(2, Some(GoalStatus::InProgress), None);
        store.add_goal("Done goal".into(), None);
        store.update_goal(3, Some(GoalStatus::Done), None);
        store.add_goal("Abandoned goal".into(), None);
        store.update_goal(4, Some(GoalStatus::Abandoned), None);

        let active = store.active_goals();
        assert_eq!(active.len(), 2);
        assert_eq!(active[0].id, 1);
        assert_eq!(active[1].id, 2);
    }

    #[test]
    fn test_format_goals_for_injection_empty() {
        let store = GoalStore::default();
        assert!(store.format_goals_for_injection().is_none());
    }

    #[test]
    fn test_format_goals_for_injection_with_goals() {
        let mut store = GoalStore::default();
        store.add_goal("Build MCP server".into(), Some("9 tools shipped".into()));
        store.update_goal(1, Some(GoalStatus::InProgress), None);
        store.add_goal("Write tests".into(), None);

        let formatted = store.format_goals_for_injection().unwrap();
        assert!(formatted.contains("### Active Goals"));
        assert!(formatted.contains("[in_progress] #1: Build MCP server (progress: 9 tools shipped)"));
        assert!(formatted.contains("[open] #2: Write tests"));
    }

    #[test]
    fn test_format_goals_excludes_done() {
        let mut store = GoalStore::default();
        store.add_goal("Done goal".into(), None);
        store.update_goal(1, Some(GoalStatus::Done), None);

        assert!(store.format_goals_for_injection().is_none());
    }

    #[test]
    fn test_load_save_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let project_dir = dir.path();

        // Load from non-existent returns default
        let store = load_goals(project_dir).unwrap();
        assert!(store.goals.is_empty());
        assert_eq!(store.next_id, 1);

        // Save and reload
        let mut store = GoalStore::default();
        store.add_goal("Test goal".into(), Some("in progress".into()));
        store.update_goal(1, Some(GoalStatus::InProgress), None);

        save_goals(project_dir, &store).unwrap();

        let loaded = load_goals(project_dir).unwrap();
        assert_eq!(loaded.goals.len(), 1);
        assert_eq!(loaded.goals[0].text, "Test goal");
        assert_eq!(loaded.goals[0].status, GoalStatus::InProgress);
        assert_eq!(loaded.goals[0].progress.as_deref(), Some("in progress"));
        assert_eq!(loaded.next_id, 2);
    }

    #[test]
    fn test_goals_path() {
        let path = goals_path(Path::new("/home/user/.claude/projects/-home-user-myproject"));
        assert_eq!(
            path,
            PathBuf::from("/home/user/.claude/projects/-home-user-myproject/memory/goals.json")
        );
    }

    #[test]
    fn test_serde_roundtrip() {
        let mut store = GoalStore::default();
        store.add_goal("Goal 1".into(), None);
        store.add_goal("Goal 2".into(), Some("notes".into()));
        store.update_goal(2, Some(GoalStatus::Done), Some("all done".into()));

        let json = serde_json::to_string_pretty(&store).unwrap();
        let parsed: GoalStore = serde_json::from_str(&json).unwrap();

        assert_eq!(parsed.goals.len(), 2);
        assert_eq!(parsed.next_id, 3);
        assert_eq!(parsed.goals[1].status, GoalStatus::Done);
        assert_eq!(parsed.goals[1].progress.as_deref(), Some("all done"));
    }
}
