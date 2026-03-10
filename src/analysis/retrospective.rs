//! Project retrospective — composite analysis combining health, lessons, and decisions.
//!
//! Chains multiple analysis modules to answer "how is this project going?"
//! Produces a structured summary suitable for both human and AI consumption.
//!
//! Used by both CLI `retrospective` and MCP `project_retrospective` tools.

use crate::analysis::project_health::{
    analyze_project_health, HotspotFile, ProjectHealthParams, ReworkFile, SessionHealthStats,
};
use crate::analysis::project_lessons::{
    aggregate_project_lessons, ProjectLessonsParams, RecurringCorrection, RecurringError,
};
use crate::decisions::{DecisionStatus, DecisionStore};
use crate::discovery::Session;

/// Parameters for retrospective analysis.
pub struct RetrospectiveParams {
    /// Maximum hotspot/rework files to include.
    pub max_files: usize,
    /// Maximum recurring errors to include.
    pub max_errors: usize,
    /// Maximum recurring corrections to include.
    pub max_corrections: usize,
    /// Minimum error occurrences to include.
    pub min_occurrences: usize,
}

impl Default for RetrospectiveParams {
    fn default() -> Self {
        Self {
            max_files: 10,
            max_errors: 10,
            max_corrections: 5,
            min_occurrences: 1,
        }
    }
}

/// A confirmed decision from the registry.
#[derive(Debug, Clone)]
#[allow(missing_docs)]
pub struct ActiveDecision {
    pub id: usize,
    pub title: String,
    pub status: String,
    pub confidence: f64,
    pub tags: Vec<String>,
}

/// Summary statistics for the retrospective.
#[derive(Debug)]
#[allow(missing_docs)]
pub struct RetrospectiveSummary {
    pub sessions_analyzed: usize,
    pub total_errors: usize,
    pub total_tool_calls: usize,
    pub total_corrections: usize,
    pub error_rate: f64,
    pub top_failure_modes: Vec<(String, usize)>,
}

/// Complete retrospective result.
#[derive(Debug)]
#[allow(missing_docs)]
pub struct RetrospectiveResult {
    pub summary: RetrospectiveSummary,
    pub hotspot_files: Vec<HotspotFile>,
    pub rework_files: Vec<ReworkFile>,
    pub recurring_errors: Vec<RecurringError>,
    pub recurring_corrections: Vec<RecurringCorrection>,
    pub decisions: Vec<ActiveDecision>,
    pub session_stats: Vec<SessionHealthStats>,
}

/// Run a composite retrospective analysis across sessions.
pub fn analyze_retrospective(
    sessions: &[Session],
    decision_store: Option<&DecisionStore>,
    params: &RetrospectiveParams,
    max_file_size: Option<u64>,
) -> RetrospectiveResult {
    // Health analysis
    let health_params = ProjectHealthParams {
        max_hotspots: params.max_files,
    };
    let health = analyze_project_health(sessions, decision_store, &health_params, max_file_size);

    // Lessons analysis
    let lessons_params = ProjectLessonsParams {
        category: "all".to_string(),
        limit: params.max_errors.max(params.max_corrections),
        min_occurrences: params.min_occurrences,
    };
    let lessons = aggregate_project_lessons(sessions, &lessons_params, max_file_size);

    // Decisions from registry
    let decisions: Vec<ActiveDecision> = decision_store
        .map(|store| {
            store
                .decisions
                .iter()
                .filter(|d| d.status != DecisionStatus::Abandoned)
                .map(|d| ActiveDecision {
                    id: d.id as usize,
                    title: d.title.clone(),
                    status: format!("{:?}", d.status).to_lowercase(),
                    confidence: d.confidence,
                    tags: d.tags.clone(),
                })
                .collect()
        })
        .unwrap_or_default();

    // Compute error rate
    let error_rate = if health.total_tool_calls > 0 {
        health.total_errors as f64 / health.total_tool_calls as f64
    } else {
        0.0
    };

    let mut recurring_errors = lessons.recurring_errors;
    recurring_errors.truncate(params.max_errors);

    let mut recurring_corrections = lessons.recurring_corrections;
    recurring_corrections.truncate(params.max_corrections);

    RetrospectiveResult {
        summary: RetrospectiveSummary {
            sessions_analyzed: health.sessions_analyzed,
            total_errors: health.total_errors,
            total_tool_calls: health.total_tool_calls,
            total_corrections: lessons.summary.total_corrections,
            error_rate,
            top_failure_modes: lessons.summary.top_failure_modes,
        },
        hotspot_files: health.hotspot_files,
        rework_files: health.rework_files,
        recurring_errors,
        recurring_corrections,
        decisions,
        session_stats: health.session_stats,
    }
}
