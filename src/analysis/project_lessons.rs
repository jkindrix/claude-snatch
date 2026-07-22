//! Cross-session lesson aggregation.
//!
//! Aggregates error→fix pairs, user corrections, and failure modes across
//! all sessions for a project. Deduplicates similar errors, ranks by frequency,
//! and identifies recurring patterns.
//!
//! Internal aggregator: the `project-lessons` command and `get_project_lessons`
//! MCP tool were removed; `recurring_errors` is now consumed by `priorities`.

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
    pub example_resolution: Option<String>,
}

/// Complete result of project-level lesson aggregation.
///
/// Only `recurring_errors` is retained: the corrections/summary aggregation was
/// dropped when the `project-lessons` command and `get_project_lessons` MCP tool
/// were removed. `priorities` consumes only recurring errors.
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
    let mut s = without_transport_headers(error);

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

    // Truncate to first meaningful line for clustering.
    if let Some(first_line) = s.lines().find(|line| !line.trim().is_empty()) {
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

/// Remove only leading provider transport metadata, retaining the complete
/// native failure body for representative evidence.
fn without_transport_headers(error: &str) -> String {
    let mut found_body = false;
    let lines: Vec<_> = error
        .lines()
        .filter(|line| {
            if found_body {
                return true;
            }
            let line = line.trim();
            let header = line.is_empty()
                || [
                    "Chunk ID:",
                    "Wall time:",
                    "Process exited with code ",
                    "Original token count:",
                    "Final output:",
                    "Output:",
                ]
                .iter()
                .any(|prefix| line.starts_with(prefix));
            found_body = !header;
            found_body
        })
        .collect();
    if lines.is_empty() {
        error.to_string()
    } else {
        lines.join("\n")
    }
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

    ProjectLessonsResult {
        recurring_errors: aggregate_failure_pairs_with_order(all_errors, params, false),
    }
}

/// Cluster already-classified failure evidence across logical sessions.
///
/// Provider-routed project analyses use this entry point so their failure
/// taxonomy is derived once from complete parsed bundles and reused by both
/// health and priority ranking.
pub fn aggregate_failure_pairs(
    all_errors: Vec<(ErrorFixPair, String)>,
    params: &ProjectLessonsParams,
) -> Vec<RecurringError> {
    aggregate_failure_pairs_with_order(all_errors, params, true)
}

fn aggregate_failure_pairs_with_order(
    all_errors: Vec<(ErrorFixPair, String)>,
    params: &ProjectLessonsParams,
    deterministic_ties: bool,
) -> Vec<RecurringError> {
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
                .map(|(pair, _)| {
                    if deterministic_ties {
                        without_transport_headers(&pair.error_preview)
                    } else {
                        pair.error_preview.clone()
                    }
                })
                .unwrap_or(pattern);

            RecurringError {
                tool_name,
                error_pattern,
                count,
                sessions,
                example_resolution,
            }
        })
        .collect();

    // Drop extraction noise (successful Read/build/test output the extractor
    // mis-flags as errors) at the source, so priorities sees a cleaned
    // recurring-error set.
    recurring_errors.retain(|e| !is_extraction_noise(&e.tool_name, &e.error_pattern));
    if deterministic_ties {
        recurring_errors.sort_by(|a, b| {
            b.count
                .cmp(&a.count)
                .then_with(|| a.tool_name.cmp(&b.tool_name))
                .then_with(|| a.error_pattern.cmp(&b.error_pattern))
        });
    } else {
        recurring_errors.sort_by_key(|error| std::cmp::Reverse(error.count));
    }
    recurring_errors.truncate(params.limit);
    recurring_errors
}

/// Whether a recurring "error" is actually mis-flagged successful tool output.
///
/// `aggregate_project_lessons` occasionally flags success as failure: a `Read`
/// whose file content literally contains words like "error"/"panic", or a
/// `Bash` run whose salient output is a cargo build/test success (often paired
/// with a trailing command that exits nonzero). This drops only the unambiguous
/// success cases — never anything that also looks like a real failure.
fn is_extraction_noise(tool_name: &str, pattern: &str) -> bool {
    match tool_name {
        "Read" => starts_with_line_marker(pattern),
        "Bash" => looks_like_build_or_test_success(pattern),
        _ => false,
    }
}

/// A leading `<number><tab|→|pipe>` marks line-numbered file/search output — a
/// successful read, not an error.
fn starts_with_line_marker(s: &str) -> bool {
    let t = s.trim_start();
    let digits = t.chars().take_while(char::is_ascii_digit).count();
    if digits == 0 {
        return false;
    }
    let rest = &t[digits..];
    rest.starts_with('\t') || rest.starts_with('→') || rest.starts_with('|')
}

/// Cargo build/test success — but never when the same text also shows a failure.
fn looks_like_build_or_test_success(p: &str) -> bool {
    if p.contains("error[E") || p.contains("FAILED") || p.contains("panicked") {
        return false;
    }
    (p.contains("Finished `") && p.contains("profile")) || p.contains("test result: ok")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::analysis::lessons::{FailureBasis, FailureKind};

    fn failure(preview: &str) -> ErrorFixPair {
        ErrorFixPair {
            timestamp: None,
            tool_name: "exec_command".to_string(),
            input_summary: HashMap::new(),
            error_preview: preview.to_string(),
            failure_kind: FailureKind::Confirmed,
            failure_basis: FailureBasis::ProcessExit,
            resolution_summary: None,
            resolution_tools: Vec::new(),
        }
    }

    #[test]
    fn extraction_noise_drops_success_keeps_failures() {
        // Successful Read (line-numbered content) mis-flagged as an error.
        assert!(is_extraction_noise(
            "Read",
            "1\t//! Message types for Claude Code"
        ));
        // Cargo build success (even paired with a trailing nonzero exit upstream).
        assert!(is_extraction_noise(
            "Bash",
            "Compiling claude-snatch v0.1.0\n    Finished `test` profile [optimized]"
        ));
        // A genuine compile error must survive (not noise).
        assert!(!is_extraction_noise(
            "Bash",
            "Exit code 101\nerror[E0061]: wrong arg count"
        ));
        // Build output that ALSO shows a failure is a real error.
        assert!(!is_extraction_noise(
            "Bash",
            "Compiling ...\ntest result: FAILED"
        ));
        // Other tools are never treated as extraction noise.
        assert!(!is_extraction_noise("Edit", "String to replace not found"));
    }

    #[test]
    fn provider_transport_headers_do_not_fragment_recurring_failures() {
        let failures = vec![
            (
                failure(
                    "Chunk ID: abc123\nWall time: 0.1 seconds\nProcess exited with code 2\nFinal output:\nerror: unexpected argument '--bad'",
                ),
                "codex:first".to_string(),
            ),
            (
                failure(
                    "Chunk ID: def456\nWall time: 8.9 seconds\nProcess exited with code 2\nFinal output:\nerror: unexpected argument '--bad'",
                ),
                "codex:second".to_string(),
            ),
        ];
        let result = aggregate_failure_pairs(
            failures,
            &ProjectLessonsParams {
                category: "errors".to_string(),
                limit: 10,
                min_occurrences: 2,
            },
        );
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].count, 2);
        assert_eq!(result[0].sessions.len(), 2);
        assert_eq!(
            result[0].error_pattern,
            "error: unexpected argument '--bad'"
        );
    }
}
