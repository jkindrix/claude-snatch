//! Active-monitoring insights (goal #8, design `.tmp/issues/0023`).
//!
//! Goal #8 surfaces cross-session insights on demand. The analysis already
//! exists elsewhere (`project_lessons`); this module is the *delivery* layer's
//! pure core: it maps existing analyzer output into a small ranked [`Insight`]
//! set. The IO (running the analyzers over sessions) and the surfaces
//! (CLI / MCP) live in the command layer — this module stays pure and testable
//! so the ranking and the fingerprints are verifiable in isolation.

use chrono::{DateTime, Utc};

use crate::analysis::project_lessons::{
    aggregate_project_lessons, ProjectLessonsParams, RecurringError,
};
use crate::discovery::Session;

/// The kind of cross-session insight surfaced by the monitor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InsightKind {
    /// An error pattern that recurred across sessions.
    RecurringError,
}

impl InsightKind {
    /// Stable lowercase tag for JSON / machine output.
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::RecurringError => "recurring_error",
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
/// ranking can interleave them. `fingerprint` is a stable identity (used as the
/// deterministic tiebreak in [`rank`]) that must not drift when surface wording
/// changes.
#[derive(Debug, Clone)]
pub struct Insight {
    /// Which kind of insight this is.
    pub kind: InsightKind,
    /// One-line headline.
    pub title: String,
    /// Supporting detail (counts, the error pattern, last fix).
    pub evidence: String,
    /// 0–100 attention score; ranking key.
    pub severity: u32,
    /// Stable identity; deterministic tiebreak when ranking.
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

/// Attention score for a recurring error.
///
/// Log-scaled by occurrence count so frequency actually *discriminates* instead
/// of saturating — the recurrence floor (3×) sits mid-scale and very frequent
/// errors approach, but rarely reach, the ceiling. Examples: 3 → 50, 6 → 60,
/// 12 → 69, 24 → 79, 48 → 89. Heuristic, tuned against real data rather than
/// derived — a flat `count × k` made nearly everything hit 100.
fn error_severity(count: usize) -> u32 {
    let ratio = (count.max(1) as f64) / 3.0;
    (50.0 + 14.0 * ratio.ln()).round().clamp(0.0, 100.0) as u32
}

/// Whether a recurring-error pattern is upstream extraction noise rather than a
/// real project error worth surfacing.
///
/// Map clustered recurring errors into insights.
///
/// `errors` is `ProjectLessonsResult::recurring_errors` (already clustered,
/// occurrence-filtered, and extraction-noise-filtered upstream in
/// `aggregate_project_lessons`). `error_pattern` is the normalized cluster key,
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

/// Compose the existing analyzers over already-resolved inputs into the full
/// insight set (unranked, un-cooled-down).
///
/// Session resolution is the caller's responsibility, so this composes cleanly
/// behind both the CLI and the MCP surfaces.
#[must_use]
pub fn insights_from(
    sessions: &[Session],
    params: &MonitorParams,
    max_file_size: Option<u64>,
) -> Vec<Insight> {
    let lessons_params = ProjectLessonsParams {
        category: "all".to_string(),
        limit: 200,
        min_occurrences: params.min_occurrences,
    };
    let lessons = aggregate_project_lessons(sessions, &lessons_params, max_file_size);
    recurring_error_insights(&lessons.recurring_errors)
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

    #[test]
    fn error_insight_maps_fields_and_severity() {
        let insights = recurring_error_insights(&[err("Bash", "tool:bash:exit 1", 5, 3)]);
        assert_eq!(insights.len(), 1);
        let i = &insights[0];
        assert_eq!(i.kind, InsightKind::RecurringError);
        assert!(
            i.title.contains("Bash") && i.title.contains("5×") && i.title.contains("3 session")
        );
        assert_eq!(i.severity, 57); // log-scaled: 50 + 14*ln(5/3)
        assert_eq!(i.fingerprint, "error:tool:bash:exit 1");
        assert!(i.recency.is_some());
    }

    #[test]
    fn error_severity_spreads_and_caps() {
        // The recurrence floor lands mid-scale, frequency discriminates, and
        // extreme counts saturate at the ceiling (no overflow).
        assert_eq!(error_severity(3), 50);
        assert!(error_severity(6) > error_severity(3));
        assert!(error_severity(48) > error_severity(6));
        assert!(
            error_severity(48) < 100,
            "realistic counts stay under the cap"
        );
        assert_eq!(error_severity(1_000_000), 100);
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
    fn rank_orders_by_severity_then_caps() {
        let all = recurring_error_insights(&[
            err("Bash", "tool:bash:a", 3, 1),  // severity 50
            err("Edit", "tool:edit:b", 48, 1), // higher severity
        ]);
        let ranked = rank(all, 1);
        assert_eq!(ranked.len(), 1, "capped to top_n");
        assert!(
            ranked[0].severity > 50,
            "the higher-severity error ranks first"
        );
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
