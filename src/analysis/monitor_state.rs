//! Cooldown state for the active monitor (goal #8, design `.tmp/issues/0023`).
//!
//! The monitor's value depends on *not nagging*: an insight seen and not acted
//! on must not reappear unchanged every session (the project's F6 "summary bloat
//! spiral"). This module records which insights have been surfaced and decides,
//! per insight, whether enough has changed to surface it again.
//!
//! Rule: an insight surfaces if it is new, OR its severity has *worsened* since
//! it was last shown, OR the cooldown window has elapsed. Otherwise it is
//! suppressed. The decision rule is pure (and unit-tested); only `load`/`save`
//! touch disk, and `load` tolerates a missing or corrupt file by resetting.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};

use crate::analysis::monitor::Insight;
use crate::error::Result;
use crate::util::atomic_write;

/// Record of when an insight fingerprint was last surfaced.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShownRecord {
    /// When the insight was last surfaced.
    pub last_shown: DateTime<Utc>,
    /// The severity at which it was last surfaced (re-surface if it worsens).
    pub last_severity: u32,
}

/// Persistent cooldown state: fingerprint → last-shown record.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MonitorState {
    /// Surfaced-insight records keyed by fingerprint.
    #[serde(default)]
    pub shown: BTreeMap<String, ShownRecord>,
}

/// Path to a project's monitor-state file.
#[must_use]
pub fn monitor_state_path(project_dir: &Path) -> PathBuf {
    project_dir.join("memory").join("monitor-state.json")
}

impl MonitorState {
    /// Load state for a project, returning an empty state if the file is
    /// missing or unreadable/corrupt (this state is cosmetic — never fail a
    /// caller, just reset).
    #[must_use]
    pub fn load(project_dir: &Path) -> Self {
        let path = monitor_state_path(project_dir);
        std::fs::read_to_string(&path)
            .ok()
            .and_then(|c| serde_json::from_str(&c).ok())
            .unwrap_or_default()
    }

    /// Persist state for a project (atomic write; creates `memory/`).
    pub fn save(&self, project_dir: &Path) -> Result<()> {
        let path = monitor_state_path(project_dir);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|source| {
                crate::error::SnatchError::IoError {
                    context: format!("Failed to create memory directory {}", parent.display()),
                    source,
                }
            })?;
        }
        let json = serde_json::to_string_pretty(self).map_err(|source| {
            crate::error::SnatchError::SerializationError {
                context: "Failed to serialize monitor state".to_string(),
                source,
            }
        })?;
        atomic_write(&path, json.as_bytes())
    }

    /// Whether an insight should surface now given the cooldown.
    ///
    /// New insight → yes. Previously shown → only if its severity has worsened
    /// or the cooldown window has elapsed.
    #[must_use]
    pub fn should_surface(
        &self,
        insight: &Insight,
        now: DateTime<Utc>,
        cooldown_days: i64,
    ) -> bool {
        match self.shown.get(&insight.fingerprint) {
            None => true,
            Some(rec) => {
                insight.severity > rec.last_severity
                    || now.signed_duration_since(rec.last_shown) >= Duration::days(cooldown_days)
            }
        }
    }

    /// Record that an insight was surfaced now.
    pub fn mark_shown(&mut self, insight: &Insight, now: DateTime<Utc>) {
        self.shown.insert(
            insight.fingerprint.clone(),
            ShownRecord {
                last_shown: now,
                last_severity: insight.severity,
            },
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::analysis::monitor::{Insight, InsightKind};

    fn insight(fingerprint: &str, severity: u32) -> Insight {
        Insight {
            kind: InsightKind::RecurringError,
            title: "t".to_string(),
            evidence: "e".to_string(),
            severity,
            fingerprint: fingerprint.to_string(),
            recency: None,
        }
    }

    fn ts(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
    }

    #[test]
    fn new_insight_surfaces() {
        let state = MonitorState::default();
        assert!(state.should_surface(&insight("error:a", 60), ts("2026-06-10T00:00:00Z"), 7));
    }

    #[test]
    fn recently_shown_same_severity_is_suppressed() {
        let mut state = MonitorState::default();
        state.mark_shown(&insight("error:a", 60), ts("2026-06-10T00:00:00Z"));
        // 2 days later, same severity → suppressed (cooldown 7).
        assert!(!state.should_surface(&insight("error:a", 60), ts("2026-06-12T00:00:00Z"), 7));
    }

    #[test]
    fn worsened_severity_re_surfaces_within_cooldown() {
        let mut state = MonitorState::default();
        state.mark_shown(&insight("error:a", 60), ts("2026-06-10T00:00:00Z"));
        // 2 days later but severity climbed 60 → 80 → re-surface.
        assert!(state.should_surface(&insight("error:a", 80), ts("2026-06-12T00:00:00Z"), 7));
    }

    #[test]
    fn elapsed_cooldown_re_surfaces() {
        let mut state = MonitorState::default();
        state.mark_shown(&insight("error:a", 60), ts("2026-06-10T00:00:00Z"));
        // 8 days later, same severity → cooldown elapsed → re-surface.
        assert!(state.should_surface(&insight("error:a", 60), ts("2026-06-18T00:00:00Z"), 7));
    }

    #[test]
    fn load_missing_dir_returns_default() {
        let state = MonitorState::load(Path::new("/nonexistent/project/dir"));
        assert!(state.shown.is_empty());
    }

    #[test]
    fn save_then_load_round_trips() {
        let tmp = tempfile::tempdir().unwrap();
        let mut state = MonitorState::default();
        state.mark_shown(&insight("error:a", 60), ts("2026-06-10T00:00:00Z"));
        state.save(tmp.path()).unwrap();
        let loaded = MonitorState::load(tmp.path());
        assert_eq!(loaded.shown.len(), 1);
        assert_eq!(loaded.shown["error:a"].last_severity, 60);
    }

    #[test]
    fn load_corrupt_file_resets() {
        let tmp = tempfile::tempdir().unwrap();
        let path = monitor_state_path(tmp.path());
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "{ not valid json").unwrap();
        let state = MonitorState::load(tmp.path());
        assert!(
            state.shown.is_empty(),
            "corrupt state must reset, not crash"
        );
    }
}
