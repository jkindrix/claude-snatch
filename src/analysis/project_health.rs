//! Project health dashboard.
//!
//! Aggregates file modification patterns, error rates, rework indicators,
//! and decision stability metrics across sessions for a project.
//!
//! Used by both CLI and MCP `get_project_health` tools.

use std::collections::{HashMap, HashSet};

use crate::analysis::extraction::extract_tool_names;
use crate::analysis::lessons::{extract_error_fix_pairs, LessonOptions};
use crate::decisions::{DecisionStatus, DecisionStore};
use crate::discovery::Session;
use crate::file_index::FileIndex;

/// Parameters for project health analysis.
pub struct ProjectHealthParams {
    /// Maximum hotspot files to return.
    pub max_hotspots: usize,
}

impl Default for ProjectHealthParams {
    fn default() -> Self {
        Self { max_hotspots: 20 }
    }
}

/// A file that appears frequently in errors or edits.
#[derive(Debug, Clone)]
#[allow(missing_docs)]
pub struct HotspotFile {
    pub path: String,
    pub edit_count: usize,
    pub session_count: usize,
}

/// A file with high rework (many versions across sessions).
#[derive(Debug, Clone)]
#[allow(missing_docs)]
pub struct ReworkFile {
    pub path: String,
    pub version_count: usize,
    pub session_count: usize,
}

/// Decision stability metrics.
#[derive(Debug, Clone)]
#[allow(missing_docs)]
pub struct DecisionChurn {
    pub total_decisions: usize,
    pub confirmed_count: usize,
    pub superseded_count: usize,
    pub abandoned_count: usize,
    pub proposed_count: usize,
}

/// Per-session error/correction stats (for trending).
#[derive(Debug, Clone)]
#[allow(missing_docs)]
pub struct SessionHealthStats {
    pub session_id: String,
    pub timestamp: Option<String>,
    pub error_count: usize,
    pub tool_count: usize,
}

/// Complete project health result.
#[derive(Debug)]
#[allow(missing_docs)]
pub struct ProjectHealthResult {
    pub sessions_analyzed: usize,
    pub hotspot_files: Vec<HotspotFile>,
    pub rework_files: Vec<ReworkFile>,
    pub decision_churn: Option<DecisionChurn>,
    pub session_stats: Vec<SessionHealthStats>,
    pub total_errors: usize,
    pub total_tool_calls: usize,
}

/// Whether `path` is one of the project's own files, for scoping churn metrics.
///
/// Relative paths are resolved against the session's working directory (the
/// project) and kept — except `.tmp/` scratch. Absolute paths are kept only
/// when under one of the project roots; anything else (config under `~/.claude`,
/// unrelated repositories) is cross-project noise.
fn is_project_file(path: &str, project_roots: &HashSet<String>) -> bool {
    if path.starts_with(".tmp/") || path.contains("/.tmp/") {
        return false;
    }
    if !path.starts_with('/') {
        return true;
    }
    project_roots
        .iter()
        .any(|root| path.starts_with(root.as_str()))
}

/// Analyze project health across sessions.
pub fn analyze_project_health(
    sessions: &[Session],
    decision_store: Option<&DecisionStore>,
    params: &ProjectHealthParams,
    max_file_size: Option<u64>,
) -> ProjectHealthResult {
    // Build file index for edit/rework tracking
    let file_index = FileIndex::from_sessions(sessions, max_file_size);

    // File-history snapshots can record files edited outside this project
    // (config under ~/.claude, unrelated repos) and scratch files under .tmp/.
    // Scope churn to the project's own files so hotspots/rework aren't polluted.
    let project_roots: HashSet<String> = sessions
        .iter()
        .map(|s| s.project_path().to_string())
        .collect();

    // Hotspot files: most edits across sessions
    let mut file_edits: HashMap<String, (usize, Vec<String>)> = HashMap::new();
    for (path, mods) in &file_index.entries {
        if !is_project_file(path, &project_roots) {
            continue;
        }
        let mut session_ids: Vec<String> = mods.iter().map(|m| m.session_id.clone()).collect();
        session_ids.sort();
        session_ids.dedup();
        file_edits.insert(path.clone(), (mods.len(), session_ids));
    }

    let mut hotspot_files: Vec<HotspotFile> = file_edits
        .iter()
        .map(|(path, (count, sessions))| HotspotFile {
            path: path.clone(),
            edit_count: *count,
            session_count: sessions.len(),
        })
        .collect();
    hotspot_files.sort_by_key(|b| std::cmp::Reverse(b.edit_count));
    hotspot_files.truncate(params.max_hotspots);

    // Rework files: files edited across multiple sessions
    let mut rework_files: Vec<ReworkFile> = file_edits
        .into_iter()
        .filter(|(_, (_, sessions))| sessions.len() > 1)
        .map(|(path, (count, sessions))| ReworkFile {
            path,
            version_count: count,
            session_count: sessions.len(),
        })
        .collect();
    rework_files.sort_by_key(|b| std::cmp::Reverse(b.session_count));
    rework_files.truncate(params.max_hotspots);

    // Decision churn from registry
    let decision_churn = decision_store.map(|store| {
        let decisions = &store.decisions;
        DecisionChurn {
            total_decisions: decisions.len(),
            confirmed_count: decisions
                .iter()
                .filter(|d| d.status == DecisionStatus::Confirmed)
                .count(),
            superseded_count: decisions
                .iter()
                .filter(|d| d.status == DecisionStatus::Superseded)
                .count(),
            abandoned_count: decisions
                .iter()
                .filter(|d| d.status == DecisionStatus::Abandoned)
                .count(),
            proposed_count: decisions
                .iter()
                .filter(|d| d.status == DecisionStatus::Proposed)
                .count(),
        }
    });

    // Per-session stats
    let lesson_opts = LessonOptions {
        limit: 500,
        ..Default::default()
    };

    let mut session_stats = Vec::new();
    let mut total_errors = 0usize;
    let mut total_tool_calls = 0usize;

    for session in sessions {
        let entries = match session.parse_with_options(max_file_size) {
            Ok(e) => e,
            Err(_) => continue,
        };

        let refs: Vec<&_> = entries.iter().collect();
        let errors = extract_error_fix_pairs(&refs, &lesson_opts);
        let error_count = errors.len();
        total_errors += error_count;

        let tool_count: usize = refs.iter().map(|e| extract_tool_names(e).len()).sum();
        total_tool_calls += tool_count;

        let timestamp = entries
            .first()
            .and_then(|e| e.timestamp())
            .map(|t| t.to_rfc3339());

        session_stats.push(SessionHealthStats {
            session_id: session.session_id().to_string(),
            timestamp,
            error_count,
            tool_count,
        });
    }

    // Sort session stats by timestamp
    session_stats.sort_by(|a, b| a.timestamp.cmp(&b.timestamp));

    ProjectHealthResult {
        sessions_analyzed: sessions.len(),
        hotspot_files,
        rework_files,
        decision_churn,
        session_stats,
        total_errors,
        total_tool_calls,
    }
}
