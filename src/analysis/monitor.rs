//! Active-monitoring insights (goal #8, design `.tmp/issues/0023`).
//!
//! Goal #8 surfaces cross-session insights "without being asked". The analysis
//! already exists elsewhere (`project_lessons`, `conflict_detection`, …); this
//! module is the *delivery* layer's pure core: it maps existing analyzer output
//! into a small ranked [`Insight`] set. The IO (running the analyzers over
//! sessions / the decision store), the cooldown state, and the surfaces
//! (CLI / MCP / hook) live in later phases — this module stays pure and
//! testable so the ranking and the fingerprints (which the cooldown depends on)
//! are verifiable in isolation.

use chrono::{DateTime, Utc};

use crate::analysis::conflict_detection::{
    detect_registry_conflicts, ConflictDetection, ConflictPair,
};
use crate::analysis::project_lessons::{
    aggregate_project_lessons, ProjectLessonsParams, RecurringError,
};
use crate::decisions::DecisionStore;
use crate::discovery::Session;

/// The kind of cross-session insight surfaced by the monitor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InsightKind {
    /// An error pattern that recurred across sessions.
    RecurringError,
    /// Two still-active decisions that appear to contradict each other.
    DecisionConflict,
}

impl InsightKind {
    /// Stable lowercase tag for JSON / machine output.
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::RecurringError => "recurring_error",
            Self::DecisionConflict => "decision_conflict",
        }
    }
}

/// Tunables for [`insights_from`].
#[derive(Debug, Clone)]
pub struct MonitorParams {
    /// Minimum occurrences for an error cluster to count as recurring.
    pub min_occurrences: usize,
}

impl Default for MonitorParams {
    fn default() -> Self {
        Self { min_occurrences: 3 }
    }
}

/// A single ranked, surfaceable insight.
///
/// `severity` is a 0–100 "attention score" comparable across kinds so a single
/// ranking can interleave them. `fingerprint` is a stable identity used by the
/// cooldown (Phase 2) to decide whether an insight has already been shown — it
/// must not drift when surface wording changes.
#[derive(Debug, Clone)]
pub struct Insight {
    /// Which kind of insight this is.
    pub kind: InsightKind,
    /// One-line headline.
    pub title: String,
    /// Supporting detail (counts, the pattern, the conflicting texts).
    pub evidence: String,
    /// 0–100 attention score; ranking key.
    pub severity: u32,
    /// Stable identity for cooldown dedup.
    pub fingerprint: String,
    /// Most-recent timestamp associated with the insight (ranking tiebreak).
    pub recency: Option<DateTime<Utc>>,
}

/// Clip a string to at most `n` chars (char-safe), appending `…` when cut.
fn clip(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_string()
    } else {
        let mut out: String = s.chars().take(n).collect();
        out.push('…');
        out
    }
}

/// Attention score for a recurring error: more occurrences → higher, with a
/// ceiling so a runaway count can't dominate forever. 3 → 60, 5 → 100.
fn error_severity(count: usize) -> u32 {
    (count as u32 * 20).min(100)
}

/// Attention score for a conflict: scaled detection confidence (0–1 → 0–100).
fn conflict_severity(confidence: f64) -> u32 {
    (confidence.clamp(0.0, 1.0) * 100.0).round() as u32
}

/// Map clustered recurring errors into insights.
///
/// `errors` is `ProjectLessonsResult::recurring_errors` (already clustered and
/// occurrence-filtered upstream). `error_pattern` is the normalized cluster key,
/// so it makes a stable fingerprint.
#[must_use]
pub fn recurring_error_insights(errors: &[RecurringError]) -> Vec<Insight> {
    errors
        .iter()
        .map(|e| {
            let resolution = e
                .example_resolution
                .as_deref()
                .map(|r| format!("; last fix: {}", clip(r, 100)))
                .unwrap_or_default();
            Insight {
                kind: InsightKind::RecurringError,
                title: format!(
                    "`{}` error recurred {}× across {} session(s)",
                    e.tool_name,
                    e.count,
                    e.sessions.len()
                ),
                evidence: format!("pattern: {}{}", clip(&e.error_pattern, 120), resolution),
                severity: error_severity(e.count),
                fingerprint: format!("error:{}", e.error_pattern),
                recency: e
                    .last_seen
                    .as_deref()
                    .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
                    .map(|dt| dt.with_timezone(&Utc)),
            }
        })
        .collect()
}

/// Map decision conflicts into insights.
///
/// Only *unresolved* conflicts are surfaced: a [`ConflictDetection::SupersedeChain`]
/// is an already-resolved supersede (one decision explicitly replaced the other),
/// so surfacing it would be noise. Registry / opposing-language pairs are the
/// live contradictions worth attention.
#[must_use]
pub fn decision_conflict_insights(conflicts: &[ConflictPair]) -> Vec<Insight> {
    conflicts
        .iter()
        .filter(|c| !matches!(c.detection, ConflictDetection::SupersedeChain))
        .map(|c| {
            let (a, b) = if c.earlier_session <= c.later_session {
                (&c.earlier_session, &c.later_session)
            } else {
                (&c.later_session, &c.earlier_session)
            };
            Insight {
                kind: InsightKind::DecisionConflict,
                title: format!("Possible decision conflict on \"{}\"", clip(&c.topic, 60)),
                evidence: format!(
                    "{} ↔ {}",
                    clip(&c.earlier_text, 90),
                    clip(&c.later_text, 90)
                ),
                severity: conflict_severity(c.confidence),
                fingerprint: format!("conflict:{}:{}|{}", c.topic, a, b),
                recency: Some(c.later_time),
            }
        })
        .collect()
}

/// Compose the existing analyzers over already-resolved inputs into the full
/// insight set (unranked, un-cooled-down).
///
/// Session resolution and the decision store are the caller's responsibility, so
/// this composes cleanly behind both the CLI and the MCP surfaces.
#[must_use]
pub fn insights_from(
    sessions: &[Session],
    store: &DecisionStore,
    params: &MonitorParams,
    max_file_size: Option<u64>,
) -> Vec<Insight> {
    let lessons_params = ProjectLessonsParams {
        category: "all".to_string(),
        limit: 200,
        min_occurrences: params.min_occurrences,
    };
    let lessons = aggregate_project_lessons(sessions, &lessons_params, max_file_size);
    let mut insights = recurring_error_insights(&lessons.recurring_errors);

    let mut conflicts = Vec::new();
    detect_registry_conflicts(store, &None, &mut conflicts);
    insights.extend(decision_conflict_insights(&conflicts));

    insights
}

/// Rank insights by attention score (then recency, then fingerprint for a
/// deterministic order) and keep the top `top_n`.
#[must_use]
pub fn rank(mut insights: Vec<Insight>, top_n: usize) -> Vec<Insight> {
    insights.sort_by(|a, b| {
        b.severity
            .cmp(&a.severity)
            .then(b.recency.cmp(&a.recency))
            .then(a.fingerprint.cmp(&b.fingerprint))
    });
    insights.truncate(top_n);
    insights
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ts(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
    }

    fn err(tool: &str, pattern: &str, count: usize, sessions: usize) -> RecurringError {
        RecurringError {
            tool_name: tool.to_string(),
            error_pattern: pattern.to_string(),
            count,
            sessions: (0..sessions).map(|i| format!("s{i}")).collect(),
            last_seen: Some("2026-06-01T10:00:00Z".to_string()),
            example_resolution: None,
        }
    }

    fn conflict(detection: ConflictDetection, confidence: f64) -> ConflictPair {
        ConflictPair {
            earlier_time: ts("2026-06-01T10:00:00Z"),
            earlier_session: "sB".to_string(),
            earlier_text: "we will use trait Drop".to_string(),
            later_time: ts("2026-06-02T10:00:00Z"),
            later_session: "sA".to_string(),
            later_text: "we will NOT use trait Drop".to_string(),
            detection,
            confidence,
            topic: "drop-trait".to_string(),
        }
    }

    #[test]
    fn error_insight_maps_fields_and_severity() {
        let insights = recurring_error_insights(&[err("Bash", "tool:bash:exit 1", 5, 3)]);
        assert_eq!(insights.len(), 1);
        let i = &insights[0];
        assert_eq!(i.kind, InsightKind::RecurringError);
        assert!(
            i.title.contains("Bash") && i.title.contains("5×") && i.title.contains("3 session")
        );
        assert_eq!(i.severity, 100); // 5 * 20, capped
        assert_eq!(i.fingerprint, "error:tool:bash:exit 1");
        assert!(i.recency.is_some());
    }

    #[test]
    fn error_severity_scales_and_caps() {
        assert_eq!(error_severity(3), 60);
        assert_eq!(error_severity(5), 100);
        assert_eq!(error_severity(50), 100);
    }

    #[test]
    fn error_insight_includes_resolution_when_present() {
        let mut e = err("Edit", "tool:edit:no match", 3, 2);
        e.example_resolution = Some("re-read the file first".to_string());
        let insights = recurring_error_insights(&[e]);
        assert!(insights[0]
            .evidence
            .contains("last fix: re-read the file first"));
    }

    #[test]
    fn supersede_chain_conflicts_are_filtered_out() {
        let insights =
            decision_conflict_insights(&[conflict(ConflictDetection::SupersedeChain, 0.9)]);
        assert!(
            insights.is_empty(),
            "resolved supersede chains must not surface"
        );
    }

    #[test]
    fn unresolved_conflict_maps_with_sorted_fingerprint() {
        let insights = decision_conflict_insights(&[conflict(ConflictDetection::Registry, 0.7)]);
        assert_eq!(insights.len(), 1);
        let i = &insights[0];
        assert_eq!(i.kind, InsightKind::DecisionConflict);
        assert_eq!(i.severity, 70);
        // sessions sorted (sA < sB) regardless of earlier/later
        assert_eq!(i.fingerprint, "conflict:drop-trait:sA|sB");
        assert!(i.evidence.contains("↔"));
    }

    #[test]
    fn rank_orders_by_severity_then_caps() {
        let mut all = recurring_error_insights(&[
            err("Bash", "tool:bash:a", 3, 1), // severity 60
            err("Edit", "tool:edit:b", 5, 1), // severity 100
        ]);
        all.extend(decision_conflict_insights(&[conflict(
            ConflictDetection::Registry,
            0.8,
        )])); // severity 80
        let ranked = rank(all, 2);
        assert_eq!(ranked.len(), 2);
        assert_eq!(ranked[0].severity, 100); // Edit error
        assert_eq!(ranked[1].severity, 80); // conflict beats the 60 error
    }

    #[test]
    fn rank_is_deterministic_on_severity_ties() {
        // Two equal-severity errors must order stably by fingerprint.
        let all = recurring_error_insights(&[
            err("Bash", "tool:bash:zzz", 3, 1),
            err("Edit", "tool:edit:aaa", 3, 1),
        ]);
        let ranked = rank(all, 2);
        assert_eq!(ranked[0].fingerprint, "error:tool:bash:zzz");
        assert_eq!(ranked[1].fingerprint, "error:tool:edit:aaa");
    }
}
