//! Cross-session lesson aggregation.
//!
//! Aggregates error→fix pairs, user corrections, and failure modes across
//! all sessions for a project. Deduplicates similar errors, ranks by frequency,
//! and identifies recurring patterns.
//!
//! Internal aggregator: the `project-lessons` command and `get_project_lessons`
//! MCP tool were removed; `recurring_errors` is now consumed by `priorities`
//! and `monitor`.

use std::collections::HashMap;

use crate::analysis::lessons::{extract_lessons, ErrorFixPair, LessonOptions};
use crate::discovery::Session;

/// Parameters for project-level lesson aggregation.
pub struct ProjectLessonsParams {
    /// Category filter: "errors", "corrections", "all".
    pub category: String,
    /// Maximum recurring patterns per category.
    pub limit: usize,
    /// Minimum occurrences to include a pattern.
    pub min_occurrences: usize,
}

impl Default for ProjectLessonsParams {
    fn default() -> Self {
        Self {
            category: "all".to_string(),
            limit: 30,
            min_occurrences: 1,
        }
    }
}

/// A recurring error pattern across sessions.
#[derive(Debug, Clone)]
#[allow(missing_docs)]
pub struct RecurringError {
    pub tool_name: String,
    pub error_pattern: String,
    pub count: usize,
    pub sessions: Vec<String>,
    pub last_seen: Option<String>,
    pub example_resolution: Option<String>,
}

/// Complete result of project-level lesson aggregation.
///
/// Only `recurring_errors` is retained: the corrections/summary aggregation was
/// dropped when the `project-lessons` command and `get_project_lessons` MCP tool
/// were removed. `priorities` and `monitor` consume only recurring errors.
#[derive(Debug)]
#[allow(missing_docs)]
pub struct ProjectLessonsResult {
    pub recurring_errors: Vec<RecurringError>,
}

/// Normalize an error message for clustering.
///
/// Strips variable parts (paths, line numbers, identifiers) to group
/// similar errors together.
fn normalize_error(tool_name: &str, error: &str) -> String {
    let mut s = error.to_string();

    // Strip ANSI color codes
    if let Ok(re) = regex::Regex::new(r"\x1b\[[0-9;]*m") {
        s = re.replace_all(&s, "").to_string();
    }

    // Normalize file paths
    if let Ok(re) = regex::Regex::new(r"(?:/[\w.-]+)+(?:\.\w+)?") {
        s = re.replace_all(&s, "<PATH>").to_string();
    }

    // Normalize line/column numbers
    if let Ok(re) = regex::Regex::new(r":\d+(?::\d+)?") {
        s = re.replace_all(&s, ":<N>").to_string();
    }

    // Truncate to first meaningful line for clustering
    if let Some(first_line) = s.lines().next() {
        let trimmed = first_line.trim();
        if trimmed.len() > 10 {
            return format!(
                "{}:{}",
                tool_name,
                trimmed.chars().take(120).collect::<String>()
            );
        }
    }

    format!("{}:{}", tool_name, s.chars().take(120).collect::<String>())
}

/// Aggregate lessons across all sessions for a project.
pub fn aggregate_project_lessons(
    sessions: &[Session],
    params: &ProjectLessonsParams,
    max_file_size: Option<u64>,
) -> ProjectLessonsResult {
    let opts = LessonOptions {
        category: crate::analysis::lessons::LessonCategory::from_str_loose(&params.category),
        limit: 200, // Higher per-session limit for aggregation
        ..Default::default()
    };

    let mut all_errors: Vec<(ErrorFixPair, String)> = Vec::new(); // (pair, session_id)

    for session in sessions {
        let entries = match session.parse_with_options(max_file_size) {
            Ok(e) => e,
            Err(_) => continue,
        };

        let refs: Vec<&_> = entries.iter().collect();
        let result = extract_lessons(&refs, &opts);
        let sid = session.session_id().to_string();

        for pair in result.error_fix_pairs {
            all_errors.push((pair, sid.clone()));
        }
    }

    // Cluster errors by normalized pattern
    let mut error_clusters: HashMap<String, Vec<(ErrorFixPair, String)>> = HashMap::new();
    for (pair, sid) in all_errors {
        let key = normalize_error(&pair.tool_name, &pair.error_preview);
        error_clusters.entry(key).or_default().push((pair, sid));
    }

    let mut recurring_errors: Vec<RecurringError> = error_clusters
        .into_iter()
        .filter(|(_, v)| v.len() >= params.min_occurrences)
        .map(|(pattern, mut entries)| {
            entries.sort_by(|a, b| b.0.timestamp.cmp(&a.0.timestamp));
            let count = entries.len();
            let mut sessions: Vec<String> = entries.iter().map(|(_, sid)| sid.clone()).collect();
            sessions.sort();
            sessions.dedup();
            let last_seen = entries.first().and_then(|(p, _)| p.timestamp.clone());
            let example_resolution = entries
                .first()
                .and_then(|(p, _)| p.resolution_summary.clone());
            let tool_name = entries
                .first()
                .map(|(p, _)| p.tool_name.clone())
                .unwrap_or_default();

            // Use first entry's error_preview as the display pattern
            let error_pattern = entries
                .first()
                .map(|(p, _)| p.error_preview.clone())
                .unwrap_or(pattern);

            RecurringError {
                tool_name,
                error_pattern,
                count,
                sessions,
                last_seen,
                example_resolution,
            }
        })
        .collect();

    recurring_errors.sort_by_key(|b| std::cmp::Reverse(b.count));
    recurring_errors.truncate(params.limit);

    ProjectLessonsResult { recurring_errors }
}
