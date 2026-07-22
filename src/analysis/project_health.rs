//! Project health dashboard.
//!
//! Aggregates file modification patterns, error rates, rework indicators,
//! and decision stability metrics across sessions for a project.
//!
//! Used by both CLI and MCP `get_project_health` tools.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::sync::Arc;

use crate::analysis::extraction::extract_tool_names;
use crate::analysis::lessons::{
    extract_error_fix_pairs, extract_lessons_from_conversation, FailureKind, LessonOptions,
};
use crate::decisions::{DecisionStatus, DecisionStore};
use crate::discovery::Session;
use crate::file_index::{FileIndex, ProviderFileIndexBuilder};
use crate::provider::{
    ActivityKind, FileChangeOutcome, FileChangeProjection, LogicalSessionKey, ParsedSession,
};
use crate::reconstruction::Conversation;

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

/// Provider-routed health result plus failure evidence.
///
/// Transport adapters render `health`; the evidence remains
/// internal so the two project analyses cannot disagree about failure counts.
#[derive(Debug)]
pub struct ProviderProjectHealthResult {
    /// Provider-neutral health metrics.
    pub health: ProjectHealthResult,
    /// Successfully parsed source-session descriptors.
    pub session_descriptors_analyzed: usize,
    /// Authoritatively classified failures.
    pub confirmed_failures: usize,
    /// Failures inferred from unstructured output text.
    pub inferred_failures: usize,
    /// Failure/fix evidence paired with its logical continuation root.
    pub failures: Vec<(crate::analysis::lessons::ErrorFixPair, LogicalSessionKey)>,
}

#[derive(Default)]
struct ProviderSessionAggregate {
    timestamp: Option<String>,
    error_count: usize,
    tool_count: usize,
}

/// Streaming provider-project health accumulator.
///
/// Each source-session descriptor is analyzed once, while continuation members are
/// accumulated under their typed logical root. Forks retain distinct roots,
/// spawned transcripts are filtered by the registry visitor, and inherited
/// fork history is excluded here by entry/file-change activity annotations.
#[derive(Default)]
pub struct ProviderProjectHealthAccumulator {
    sessions: BTreeMap<LogicalSessionKey, ProviderSessionAggregate>,
    files: ProviderFileIndexBuilder,
    project_roots: BTreeSet<String>,
    failures: Vec<(crate::analysis::lessons::ErrorFixPair, LogicalSessionKey)>,
    session_descriptors_analyzed: usize,
    confirmed_failures: usize,
    inferred_failures: usize,
}

impl ProviderProjectHealthAccumulator {
    /// Add one successfully parsed source-session descriptor.
    pub fn add_session(
        &mut self,
        project_roots: &[String],
        logical_root: &LogicalSessionKey,
        parsed: Arc<ParsedSession>,
        semantic_annotations: bool,
    ) -> crate::error::Result<()> {
        self.project_roots.extend(project_roots.iter().cloned());
        let conversation = Conversation::from_parsed_session(Arc::clone(&parsed))?;
        let lesson_opts = LessonOptions {
            limit: usize::MAX,
            ..Default::default()
        };
        let lessons =
            extract_lessons_from_conversation(&conversation, &lesson_opts, semantic_annotations);
        let error_count = lessons.error_fix_pairs.len();
        let confirmed = lessons
            .error_fix_pairs
            .iter()
            .filter(|pair| pair.failure_kind == FailureKind::Confirmed)
            .count();
        self.confirmed_failures = self.confirmed_failures.saturating_add(confirmed);
        self.inferred_failures = self
            .inferred_failures
            .saturating_add(error_count.saturating_sub(confirmed));
        self.failures.extend(
            lessons
                .error_fix_pairs
                .into_iter()
                .map(|pair| (pair, logical_root.clone())),
        );

        let active_entries: Vec<_> = parsed
            .entries
            .iter()
            .filter(|entry| {
                parsed
                    .semantics
                    .get(&entry.id)
                    .map_or(true, |semantics| semantics.activity == ActivityKind::New)
            })
            .collect();
        let tool_count = active_entries
            .iter()
            .map(|entry| match &entry.entry {
                crate::model::LogEntry::Assistant(message) => message.message.tool_uses().len(),
                _ => 0,
            })
            .sum::<usize>();
        let timestamp = active_entries
            .iter()
            .filter_map(|entry| entry.entry.timestamp())
            .min()
            .map(|value| value.to_rfc3339());

        let aggregate = self.sessions.entry(logical_root.clone()).or_default();
        aggregate.error_count = aggregate.error_count.saturating_add(error_count);
        aggregate.tool_count = aggregate.tool_count.saturating_add(tool_count);
        if let Some(timestamp) = timestamp {
            if aggregate
                .timestamp
                .as_ref()
                .map_or(true, |current| timestamp < *current)
            {
                aggregate.timestamp = Some(timestamp);
            }
        }

        let projection = FileChangeProjection {
            changes: parsed.file_changes.clone(),
            inherited_owners: parsed
                .semantics
                .iter()
                .filter(|(_, semantics)| semantics.activity == ActivityKind::InheritedHistory)
                .map(|(entry, _)| entry.clone())
                .collect(),
            owner_timestamps: parsed
                .entries
                .iter()
                .filter_map(|entry| {
                    entry
                        .entry
                        .timestamp()
                        .map(|timestamp| (entry.id.clone(), timestamp))
                })
                .collect(),
        };
        self.files.add_projection_for_logical_session(
            project_roots.first().map_or("", String::as_str),
            &parsed.descriptor.key,
            logical_root,
            &projection,
        );
        self.session_descriptors_analyzed = self.session_descriptors_analyzed.saturating_add(1);
        Ok(())
    }

    /// Finish deterministic rankings and attach optional Claude-registry
    /// decision coverage.
    #[must_use]
    pub fn finish(
        self,
        decision_store: Option<&DecisionStore>,
        params: &ProjectHealthParams,
    ) -> ProviderProjectHealthResult {
        let file_index = self.files.build();
        let mut file_aggregates: BTreeMap<String, (usize, BTreeSet<LogicalSessionKey>)> =
            BTreeMap::new();
        for (path, changes) in file_index.entries {
            if !is_provider_project_file(&path, &self.project_roots) {
                continue;
            }
            let aggregate = file_aggregates.entry(path).or_default();
            for change in changes {
                if change.outcome == FileChangeOutcome::Applied {
                    aggregate.0 = aggregate.0.saturating_add(1);
                    aggregate.1.insert(change.session);
                }
            }
        }
        file_aggregates.retain(|_, (edits, _)| *edits > 0);
        let mut hotspot_files: Vec<_> = file_aggregates
            .iter()
            .map(|(path, (edits, sessions))| HotspotFile {
                path: path.clone(),
                edit_count: *edits,
                session_count: sessions.len(),
            })
            .collect();
        hotspot_files.sort_by(|a, b| {
            b.edit_count
                .cmp(&a.edit_count)
                .then_with(|| a.path.cmp(&b.path))
        });
        hotspot_files.truncate(params.max_hotspots);

        let mut rework_files: Vec<_> = file_aggregates
            .into_iter()
            .filter(|(_, (_, sessions))| sessions.len() > 1)
            .map(|(path, (edits, sessions))| ReworkFile {
                path,
                version_count: edits,
                session_count: sessions.len(),
            })
            .collect();
        rework_files.sort_by(|a, b| {
            b.session_count
                .cmp(&a.session_count)
                .then_with(|| b.version_count.cmp(&a.version_count))
                .then_with(|| a.path.cmp(&b.path))
        });
        rework_files.truncate(params.max_hotspots);

        let mut session_stats: Vec<_> = self
            .sessions
            .iter()
            .map(|(key, aggregate)| SessionHealthStats {
                session_id: key.to_string(),
                timestamp: aggregate.timestamp.clone(),
                error_count: aggregate.error_count,
                tool_count: aggregate.tool_count,
            })
            .collect();
        session_stats.sort_by(|a, b| {
            a.timestamp
                .cmp(&b.timestamp)
                .then_with(|| a.session_id.cmp(&b.session_id))
        });

        let total_errors = self
            .confirmed_failures
            .saturating_add(self.inferred_failures);
        let total_tool_calls = session_stats.iter().fold(0_usize, |sum, session| {
            sum.saturating_add(session.tool_count)
        });
        ProviderProjectHealthResult {
            health: ProjectHealthResult {
                sessions_analyzed: self.sessions.len(),
                hotspot_files,
                rework_files,
                decision_churn: decision_store.map(decision_churn),
                session_stats,
                total_errors,
                total_tool_calls,
            },
            session_descriptors_analyzed: self.session_descriptors_analyzed,
            confirmed_failures: self.confirmed_failures,
            inferred_failures: self.inferred_failures,
            failures: self.failures,
        }
    }
}

fn decision_churn(store: &DecisionStore) -> DecisionChurn {
    let decisions = &store.decisions;
    DecisionChurn {
        total_decisions: decisions.len(),
        confirmed_count: decisions
            .iter()
            .filter(|decision| decision.status == DecisionStatus::Confirmed)
            .count(),
        superseded_count: decisions
            .iter()
            .filter(|decision| decision.status == DecisionStatus::Superseded)
            .count(),
        abandoned_count: decisions
            .iter()
            .filter(|decision| decision.status == DecisionStatus::Abandoned)
            .count(),
        proposed_count: decisions
            .iter()
            .filter(|decision| decision.status == DecisionStatus::Proposed)
            .count(),
    }
}

pub(crate) fn is_provider_project_file(path: &str, project_roots: &BTreeSet<String>) -> bool {
    let windows_drive = path.as_bytes().get(1) == Some(&b':');
    let windows_absolute =
        windows_drive && matches!(path.as_bytes().get(2), Some(b'/') | Some(b'\\'));
    if windows_drive && !windows_absolute {
        return false;
    }
    if !path.starts_with('/') && !windows_absolute {
        let Some(relative) = crate::provider::project::normalize_cwd(path) else {
            return false;
        };
        return relative != ".."
            && !relative.starts_with("../")
            && !relative.split('/').any(|segment| segment == ".tmp");
    }
    let Some(path) = crate::provider::project::normalize_cwd(path) else {
        return false;
    };
    if path.split('/').any(|segment| segment == ".tmp") {
        return false;
    }
    project_roots.iter().any(|root| {
        crate::provider::project::normalize_cwd(root).is_some_and(|root| {
            path == root
                || path
                    .strip_prefix(&root)
                    .is_some_and(|tail| tail.starts_with('/'))
        })
    })
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::fake::{multi_artifact_key, FakeProvider};
    use crate::provider::{
        FileChangeDetail, FileChangeEvidence, FileChangeKind, FileChangeObservation, SourceProvider,
    };

    #[test]
    fn provider_health_scopes_snapshot_dedup_to_continuation_members() {
        let provider = FakeProvider;
        let mut parsed = provider.parse(&multi_artifact_key()).unwrap();
        let active_owner = parsed.entries[2].id.clone();
        let inherited_owner = parsed.entries.last().unwrap().id.clone();
        let record = parsed.entry_origins[&active_owner][0].clone();
        let change = |owner, path: &str, outcome| FileChangeObservation {
            owner,
            operation_id: format!("snapshot-{path}"),
            change_index: 0,
            record: record.clone(),
            outcome_record: None,
            path: path.to_string(),
            move_path: None,
            kind: FileChangeKind::Update,
            detail: FileChangeDetail::FullContent("new".to_string()),
            evidence: FileChangeEvidence::FileHistorySnapshot,
            outcome,
            observed_at: None,
            native_version: Some(1),
        };
        parsed.file_changes = vec![
            change(
                active_owner.clone(),
                "/work/src/lib.rs",
                FileChangeOutcome::Applied,
            ),
            change(
                active_owner.clone(),
                "/work/src/lib.rs",
                FileChangeOutcome::Applied,
            ),
            change(
                active_owner.clone(),
                "/work/src/fail.rs",
                FileChangeOutcome::Failed,
            ),
            change(
                inherited_owner,
                "/work/src/inherited.rs",
                FileChangeOutcome::Applied,
            ),
            change(
                active_owner,
                "/workbench/src/outside.rs",
                FileChangeOutcome::Applied,
            ),
            change(
                parsed.entries[2].id.clone(),
                "../outside.rs",
                FileChangeOutcome::Applied,
            ),
            change(
                parsed.entries[2].id.clone(),
                "src/../.tmp/scratch.rs",
                FileChangeOutcome::Applied,
            ),
            change(
                parsed.entries[2].id.clone(),
                "C:drive-relative.rs",
                FileChangeOutcome::Applied,
            ),
        ];
        let first = Arc::new(parsed);
        let mut second = (*first).clone();
        second.descriptor.key.native_id = "continuation-member".to_string();
        let second = Arc::new(second);
        let mut aggregate = ProviderProjectHealthAccumulator::default();
        for parsed in [first, second] {
            aggregate
                .add_session(&["/work".to_string()], &multi_artifact_key(), parsed, true)
                .unwrap();
        }
        let result = aggregate.finish(None, &ProjectHealthParams::default());
        assert_eq!(result.session_descriptors_analyzed, 2);
        assert_eq!(result.health.sessions_analyzed, 1);
        assert_eq!(result.health.hotspot_files.len(), 1);
        assert_eq!(result.health.hotspot_files[0].path, "/work/src/lib.rs");
        assert_eq!(result.health.hotspot_files[0].edit_count, 2);
        assert_eq!(result.health.hotspot_files[0].session_count, 1);
        assert!(result.health.rework_files.is_empty());
    }
}
