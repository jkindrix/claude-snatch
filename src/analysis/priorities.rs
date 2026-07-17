//! Priority suggestion analysis.
//!
//! Combines recurring errors, file hotspots, active goals, and decision
//! stability to surface what deserves attention next. Produces ranked
//! priority items with evidence and rationale.
//!
//! Used by both CLI `priorities` and MCP `suggest_priorities` tools.

use std::collections::HashMap;

use crate::analysis::project_health::{analyze_project_health, ProjectHealthParams};
use crate::analysis::project_lessons::{aggregate_project_lessons, ProjectLessonsParams};
use crate::decisions::{DecisionStatus, DecisionStore};
use crate::discovery::Session;
use crate::goals::{GoalStatus, GoalStore};

/// Parameters for priority suggestion.
pub struct PriorityParams {
    /// Max hotspot files to consider.
    pub max_files: usize,
    /// Max error patterns to consider.
    pub max_errors: usize,
    /// Max priority items to return.
    pub max_priorities: usize,
}

impl Default for PriorityParams {
    fn default() -> Self {
        Self {
            max_files: 20,
            max_errors: 20,
            max_priorities: 10,
        }
    }
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

/// Source of evidence for a priority item.
#[derive(Debug, Clone)]
pub enum PrioritySource {
    /// Recurring error pattern.
    RecurringError {
        /// Tool that produces the error.
        tool: String,
        /// Number of occurrences.
        count: usize,
        /// Number of sessions affected.
        sessions: usize,
        /// Representative error text for the cluster.
        pattern: String,
        /// How the error was last resolved, when a fix followed it.
        last_fix: Option<String>,
    },
    /// High-churn file.
    FileChurn {
        /// File path.
        path: String,
        /// Number of edits.
        edits: usize,
        /// Number of sessions.
        sessions: usize,
    },
    /// Open or in-progress goal.
    OpenGoal {
        /// Goal ID.
        id: usize,
        /// Goal text.
        text: String,
        /// Current status.
        status: String,
    },
    /// Proposed decision needing confirmation.
    ProposedDecision {
        /// Decision ID.
        id: usize,
        /// Decision title.
        title: String,
        /// Confidence score.
        confidence: f64,
    },
}

impl std::fmt::Display for PrioritySource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PrioritySource::RecurringError {
                tool,
                count,
                sessions,
                pattern,
                last_fix,
            } => {
                let fix = last_fix
                    .as_deref()
                    .map(|r| format!("; last fix: {}", clip(r, 100)))
                    .unwrap_or_default();
                write!(
                    f,
                    "tool failure: [{}] {}x across {} sessions; pattern: {}{}",
                    tool,
                    count,
                    sessions,
                    clip(pattern, 120),
                    fix
                )
            }
            PrioritySource::FileChurn {
                path,
                edits,
                sessions,
            } => {
                write!(
                    f,
                    "churn: {} ({} edits, {} sessions)",
                    path, edits, sessions
                )
            }
            PrioritySource::OpenGoal { id, text, status } => {
                write!(f, "goal #{}: {} ({})", id, text, status)
            }
            PrioritySource::ProposedDecision {
                id,
                title,
                confidence,
            } => {
                write!(
                    f,
                    "decision #{}: {} ({:.0}% confidence)",
                    id,
                    title,
                    confidence * 100.0
                )
            }
        }
    }
}

/// A ranked priority item.
#[derive(Debug, Clone)]
#[allow(missing_docs)]
pub struct PriorityItem {
    pub rank: usize,
    pub category: String,
    pub summary: String,
    pub score: f64,
    pub sources: Vec<PrioritySource>,
}

/// Complete priority suggestion result.
#[derive(Debug)]
#[allow(missing_docs)]
pub struct PriorityResult {
    pub sessions_analyzed: usize,
    pub total_errors: usize,
    pub open_goals: usize,
    pub proposed_decisions: usize,
    pub priorities: Vec<PriorityItem>,
}

/// Suggest priorities based on project data.
pub fn suggest_priorities(
    sessions: &[Session],
    decision_store: Option<&DecisionStore>,
    goal_store: Option<&GoalStore>,
    params: &PriorityParams,
    max_file_size: Option<u64>,
) -> PriorityResult {
    let mut items: Vec<PriorityItem> = Vec::new();

    // 1. Recurring errors → "fix reliability" priorities
    let lessons_params = ProjectLessonsParams {
        category: "errors".to_string(),
        limit: params.max_errors,
        min_occurrences: 2,
    };
    let lessons = aggregate_project_lessons(sessions, &lessons_params, max_file_size);

    for error in &lessons.recurring_errors {
        let session_count = error.sessions.len();
        let score = (error.count as f64).ln() * (1.0 + session_count as f64 * 0.5);
        items.push(PriorityItem {
            rank: 0,
            category: "reliability".to_string(),
            summary: format!(
                "[{}] tool failure occurring {}x across {} sessions",
                error.tool_name, error.count, session_count,
            ),
            score,
            sources: vec![PrioritySource::RecurringError {
                tool: error.tool_name.clone(),
                count: error.count,
                sessions: session_count,
                pattern: error.error_pattern.clone(),
                last_fix: error.example_resolution.clone(),
            }],
        });
    }

    // 2. High-churn files → "stabilize" priorities
    let health_params = ProjectHealthParams {
        max_hotspots: params.max_files,
    };
    let health = analyze_project_health(sessions, decision_store, &health_params, max_file_size);

    // Files with high rework across many sessions
    for file in &health.rework_files {
        if file.session_count >= 3 {
            let score = (file.session_count as f64) * 1.5 + (file.version_count as f64).ln();
            items.push(PriorityItem {
                rank: 0,
                category: "stability".to_string(),
                summary: format!(
                    "{} reworked across {} sessions ({} versions)",
                    file.path, file.session_count, file.version_count,
                ),
                score,
                sources: vec![PrioritySource::FileChurn {
                    path: file.path.clone(),
                    edits: file.version_count,
                    sessions: file.session_count,
                }],
            });
        }
    }

    // 3. Open goals → "committed work" priorities
    let mut open_goals = 0usize;
    if let Some(store) = goal_store {
        for goal in &store.goals {
            if goal.status.is_active() {
                open_goals += 1;
                let is_in_progress = matches!(goal.status, GoalStatus::InProgress);
                let score = if is_in_progress { 8.0 } else { 5.0 };
                let status_str = if is_in_progress {
                    "in_progress"
                } else {
                    "open"
                };
                items.push(PriorityItem {
                    rank: 0,
                    category: "goal".to_string(),
                    summary: goal.text.clone(),
                    score,
                    sources: vec![PrioritySource::OpenGoal {
                        id: goal.id as usize,
                        text: goal.text.clone(),
                        status: status_str.to_string(),
                    }],
                });
            }
        }
    }

    // 4. Proposed decisions → "resolve uncertainty" priorities
    let mut proposed_decisions = 0usize;
    if let Some(store) = decision_store {
        for decision in &store.decisions {
            if decision.status == DecisionStatus::Proposed {
                proposed_decisions += 1;
                let score = 4.0 + (1.0 - decision.confidence) * 3.0;
                items.push(PriorityItem {
                    rank: 0,
                    category: "decision".to_string(),
                    summary: format!("Resolve: {}", decision.title),
                    score,
                    sources: vec![PrioritySource::ProposedDecision {
                        id: decision.id as usize,
                        title: decision.title.clone(),
                        confidence: decision.confidence,
                    }],
                });
            }
        }
    }

    // Deduplicate: merge items with overlapping file paths
    let mut merged = deduplicate_by_file(&mut items);

    // Sort by score descending
    merged.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    merged.truncate(params.max_priorities);

    // Assign ranks
    for (i, item) in merged.iter_mut().enumerate() {
        item.rank = i + 1;
    }

    PriorityResult {
        sessions_analyzed: health.sessions_analyzed,
        total_errors: health.total_errors,
        open_goals,
        proposed_decisions,
        priorities: merged,
    }
}

/// Merge priority items that reference the same file path.
fn deduplicate_by_file(items: &mut Vec<PriorityItem>) -> Vec<PriorityItem> {
    let mut file_items: HashMap<String, usize> = HashMap::new();
    let mut result: Vec<PriorityItem> = Vec::new();

    for item in items.drain(..) {
        // Check if any source references a file path
        let file_path = item.sources.iter().find_map(|s| {
            if let PrioritySource::FileChurn { ref path, .. } = s {
                Some(path.clone())
            } else {
                None
            }
        });

        if let Some(ref path) = file_path {
            if let Some(&existing_idx) = file_items.get(path) {
                // Merge into existing
                result[existing_idx].score += item.score * 0.5;
                result[existing_idx].sources.extend(item.sources);
                continue;
            }
            file_items.insert(path.clone(), result.len());
        }

        result.push(item);
    }

    result
}
