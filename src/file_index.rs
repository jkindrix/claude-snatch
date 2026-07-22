//! File-session relationship index.
//!
//! Builds a reverse index from file paths to the sessions and messages
//! that modified them, using `file-history-snapshot` entries.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use chrono::{DateTime, Utc};

use crate::discovery::{ClaudeDirectory, Session};
use crate::error::Result;
use crate::model::message::LogEntry;
use crate::provider::{
    ActivityKind, EntryId, FileChangeEvidence, FileChangeKind, FileChangeOutcome,
    FileChangeProjection, LogicalSessionKey, ParsedSession, RecordRef,
};

/// A record of a file being modified in a session.
#[derive(Debug, Clone)]
pub struct FileModification {
    /// The session that modified the file.
    pub session_id: String,
    /// The project path for the session.
    pub project_path: String,
    /// The message ID associated with the modification.
    pub message_id: String,
    /// When the modification was recorded.
    pub timestamp: DateTime<Utc>,
    /// Backup version number.
    pub version: u32,
}

/// Reverse index: file path → list of modifications across sessions.
#[derive(Debug, Default)]
pub struct FileIndex {
    /// Map from file path to modifications, sorted by timestamp.
    pub entries: HashMap<String, Vec<FileModification>>,
}

/// Provider-neutral evidence about one file-change operation.
#[derive(Debug, Clone)]
pub struct ProviderFileModification {
    /// Provider-qualified logical session identity.
    pub session: LogicalSessionKey,
    /// Best available project path/cwd for the session.
    pub project_path: String,
    /// Normalized entry owning the native evidence.
    pub entry_id: EntryId,
    /// Provider-native operation identity.
    pub operation_id: String,
    /// Native observation time, when persisted.
    pub timestamp: Option<DateTime<Utc>>,
    /// Provider-native version, when available.
    pub version: Option<u32>,
    /// Native source path.
    pub path: String,
    /// Native destination path for a move/update.
    pub move_path: Option<String>,
    /// Add/delete/update operation.
    pub kind: FileChangeKind,
    /// Content coverage retained by the evidence.
    pub coverage: String,
    /// Evidence source/strength.
    pub evidence: FileChangeEvidence,
    /// Source-backed application outcome.
    pub outcome: FileChangeOutcome,
    /// Native record carrying the change itself.
    pub record: RecordRef,
    /// Native record proving the outcome, when present.
    pub outcome_record: Option<RecordRef>,
}

/// Deterministic reverse index over provider-normalized file-change evidence.
#[derive(Debug, Default)]
pub struct ProviderFileIndex {
    /// Native source path → observations, sorted chronologically.
    pub entries: BTreeMap<String, Vec<ProviderFileModification>>,
    /// One best row per cumulative snapshot state. Snapshot records repeat
    /// every previously tracked file, so retaining all copies until `finish`
    /// makes corpus-wide history proportional to the Cartesian repetition
    /// rather than the number of actual file versions.
    snapshot_states: BTreeMap<(LogicalSessionKey, String, u32), ProviderFileModification>,
}

/// Streaming builder for [`ProviderFileIndex`].
#[derive(Debug, Default)]
pub struct ProviderFileIndexBuilder {
    index: ProviderFileIndex,
}

/// Counts plus one deterministic, globally limited view of matching evidence.
///
/// `selected` follows source-path order and then the index's native-time order.
/// Applied and non-applied observations share that one sequence, so a small
/// limit cannot hide attempts merely because successful changes exist.
pub struct ProviderFileSearch<'a> {
    /// Number of source-path groups containing at least one matching row.
    pub total_files: usize,
    /// Matching applied observations after snapshot-state deduplication.
    pub total_modifications: usize,
    /// Matching failed, declined, or unknown-outcome observations.
    pub total_attempts: usize,
    /// First `limit` rows in deterministic index order.
    pub selected: Vec<(&'a str, &'a ProviderFileModification)>,
}

/// Earliest and latest native or normalized timestamps in one parsed bundle.
#[must_use]
pub fn parsed_session_time_range(parsed: &ParsedSession) -> Option<(DateTime<Utc>, DateTime<Utc>)> {
    let mut timestamps = parsed
        .entries
        .iter()
        .filter_map(|entry| entry.entry.timestamp())
        .chain(
            parsed
                .file_changes
                .iter()
                .filter_map(|change| change.observed_at),
        );
    let first = timestamps.next()?;
    let mut start = first;
    let mut end = first;
    for timestamp in timestamps {
        start = start.min(timestamp);
        end = end.max(timestamp);
    }
    Some((start, end))
}

impl ProviderFileIndexBuilder {
    /// Add one complete provider bundle, retaining only compact index rows.
    pub fn add_session(&mut self, project_path: &str, parsed: &ParsedSession) {
        self.index.add_session(project_path, parsed);
    }

    /// Add one provider-owned compact projection without retaining its
    /// conversation entries.
    pub fn add_projection(
        &mut self,
        project_path: &str,
        session: &LogicalSessionKey,
        projection: &FileChangeProjection,
    ) {
        self.index.add_projection(project_path, session, projection);
    }

    /// Sort and deduplicate snapshot states, returning the completed index.
    #[must_use]
    pub fn build(mut self) -> ProviderFileIndex {
        self.index.finish();
        self.index
    }
}

impl ProviderFileIndex {
    /// Build an index from complete provider bundles plus their project labels.
    ///
    /// Fork-inherited entries are excluded from this cross-session new-work
    /// projection. Snapshot states are deduplicated by qualified session,
    /// path, and native version, preserving the earliest observation exactly
    /// like the established Claude index. Distinct native emissions are never
    /// merged by text or path equality.
    pub fn from_parsed_sessions<'a, I>(sessions: I) -> Self
    where
        I: IntoIterator<Item = (&'a str, &'a ParsedSession)>,
    {
        let mut builder = ProviderFileIndexBuilder::default();
        for (project_path, parsed) in sessions {
            builder.add_session(project_path, parsed);
        }
        builder.build()
    }

    fn add_session(&mut self, project_path: &str, parsed: &ParsedSession) {
        let owner_timestamps = parsed
            .entries
            .iter()
            .filter_map(|entry| {
                entry
                    .entry
                    .timestamp()
                    .map(|timestamp| (entry.id.clone(), timestamp))
            })
            .collect();
        let inherited_owners = parsed
            .semantics
            .iter()
            .filter(|(_, semantics)| semantics.activity == ActivityKind::InheritedHistory)
            .map(|(entry, _)| entry.clone())
            .collect();
        self.add_projection(
            project_path,
            &parsed.descriptor.key,
            &FileChangeProjection {
                changes: parsed.file_changes.clone(),
                inherited_owners,
                owner_timestamps,
            },
        );
    }

    fn add_projection(
        &mut self,
        project_path: &str,
        session: &LogicalSessionKey,
        projection: &FileChangeProjection,
    ) {
        for change in &projection.changes {
            if projection.inherited_owners.contains(&change.owner) {
                continue;
            }
            let timestamp = change
                .observed_at
                .or_else(|| projection.owner_timestamps.get(&change.owner).copied());
            let modification = ProviderFileModification {
                session: session.clone(),
                project_path: project_path.to_string(),
                entry_id: change.owner.clone(),
                operation_id: change.operation_id.clone(),
                timestamp,
                version: change.native_version,
                path: change.path.clone(),
                move_path: change.move_path.clone(),
                kind: change.kind,
                coverage: change.detail.coverage().to_string(),
                evidence: change.evidence,
                outcome: change.outcome,
                record: change.record.clone(),
                outcome_record: change.outcome_record.clone(),
            };
            if modification.evidence == FileChangeEvidence::FileHistorySnapshot {
                if let Some(version) = modification.version {
                    let key = (session.clone(), modification.path.clone(), version);
                    match self.snapshot_states.entry(key) {
                        std::collections::btree_map::Entry::Vacant(entry) => {
                            entry.insert(modification);
                        }
                        std::collections::btree_map::Entry::Occupied(mut entry) => {
                            if modification_order(&modification, entry.get()).is_lt() {
                                entry.insert(modification);
                            }
                        }
                    }
                    continue;
                }
            }
            self.entries
                .entry(change.path.clone())
                .or_default()
                .push(modification);
        }
    }

    fn finish(&mut self) {
        for modification in std::mem::take(&mut self.snapshot_states).into_values() {
            self.entries
                .entry(modification.path.clone())
                .or_default()
                .push(modification);
        }
        for modifications in self.entries.values_mut() {
            modifications.sort_by(modification_order);
            let mut snapshot_states = BTreeSet::new();
            modifications.retain(|change| {
                if change.evidence != FileChangeEvidence::FileHistorySnapshot {
                    return true;
                }
                let Some(version) = change.version else {
                    return true;
                };
                snapshot_states.insert((change.session.clone(), change.path.clone(), version))
            });
        }
    }

    /// Find path groups whose source path or move destination contains a
    /// substring. Each native observation appears once even when both match.
    pub fn search(&self, pattern: &str) -> Vec<(&str, &[ProviderFileModification])> {
        self.entries
            .iter()
            .filter(|(path, changes)| {
                path.contains(pattern)
                    || changes.iter().any(|change| {
                        change
                            .move_path
                            .as_deref()
                            .is_some_and(|path| path.contains(pattern))
                    })
            })
            .map(|(path, changes)| (path.as_str(), changes.as_slice()))
            .collect()
    }

    /// Search individual evidence rows and apply one limit across outcomes.
    ///
    /// A move-destination match selects only the operation carrying that
    /// destination, not every unrelated operation grouped under its source
    /// path. Totals still cover the complete matching corpus.
    pub fn search_limited(&self, pattern: &str, limit: usize) -> ProviderFileSearch<'_> {
        let mut result = ProviderFileSearch {
            total_files: 0,
            total_modifications: 0,
            total_attempts: 0,
            selected: Vec::with_capacity(limit.min(self.observation_count())),
        };
        for (path, changes) in &self.entries {
            let source_matches = path.contains(pattern);
            let mut file_matches = false;
            for change in changes {
                let matches = source_matches
                    || change
                        .move_path
                        .as_deref()
                        .is_some_and(|destination| destination.contains(pattern));
                if !matches {
                    continue;
                }
                file_matches = true;
                if change.outcome == FileChangeOutcome::Applied {
                    result.total_modifications += 1;
                } else {
                    result.total_attempts += 1;
                }
                if result.selected.len() < limit {
                    result.selected.push((path.as_str(), change));
                }
            }
            result.total_files += usize::from(file_matches);
        }
        result
    }

    /// Total source paths tracked.
    pub fn file_count(&self) -> usize {
        self.entries.len()
    }

    /// Total evidence observations after snapshot-state deduplication.
    pub fn observation_count(&self) -> usize {
        self.entries.values().map(Vec::len).sum()
    }
}

fn modification_order(
    left: &ProviderFileModification,
    right: &ProviderFileModification,
) -> std::cmp::Ordering {
    left.timestamp
        .cmp(&right.timestamp)
        .then_with(|| left.session.cmp(&right.session))
        .then_with(|| left.entry_id.cmp(&right.entry_id))
        .then_with(|| left.operation_id.cmp(&right.operation_id))
}

impl FileIndex {
    /// Build a file index from a set of sessions.
    pub fn from_sessions(sessions: &[Session], max_file_size: Option<u64>) -> Self {
        let mut index = FileIndex::default();

        for session in sessions {
            let entries = match session.parse_with_options(max_file_size) {
                Ok(e) => e,
                Err(_) => continue,
            };

            let sid = session.session_id().to_string();
            let project_path = session.project_path().to_string();

            for entry in &entries {
                if let LogEntry::FileHistorySnapshot(snapshot) = entry {
                    for (file_path, backup) in &snapshot.snapshot.tracked_file_backups {
                        index.entries.entry(file_path.clone()).or_default().push(
                            FileModification {
                                session_id: sid.clone(),
                                project_path: project_path.clone(),
                                message_id: snapshot.message_id.clone(),
                                timestamp: backup.backup_time,
                                version: backup.version,
                            },
                        );
                    }
                }
            }
        }

        // Sort each file's modifications by timestamp, then collapse duplicate
        // snapshot records. A file-history-snapshot re-lists every tracked file
        // (with its current version) each time any file changes, so the same
        // (session, version) state appears in many snapshots. Keep one record
        // per (session, version) — the earliest — so counts reflect actual
        // modifications rather than snapshot appearances.
        for mods in index.entries.values_mut() {
            mods.sort_by_key(|m| m.timestamp);
            let mut seen = HashSet::new();
            mods.retain(|m| seen.insert((m.session_id.clone(), m.version)));
        }

        index
    }

    /// Build a file index for all sessions matching a project filter.
    pub fn for_project(
        claude_dir: &ClaudeDirectory,
        project_filter: &str,
        max_file_size: Option<u64>,
    ) -> Result<Self> {
        let mut all_sessions: Vec<Session> = Vec::new();

        for project in claude_dir.projects()? {
            if project.best_path().contains(project_filter) {
                all_sessions.extend(project.sessions()?);
            }
        }

        Ok(Self::from_sessions(&all_sessions, max_file_size))
    }

    /// Look up which sessions modified a specific file.
    pub fn get(&self, file_path: &str) -> Option<&[FileModification]> {
        self.entries.get(file_path).map(|v| v.as_slice())
    }

    /// Find files matching a substring pattern.
    pub fn search(&self, pattern: &str) -> Vec<(&str, &[FileModification])> {
        self.entries
            .iter()
            .filter(|(path, _)| path.contains(pattern))
            .map(|(path, mods)| (path.as_str(), mods.as_slice()))
            .collect()
    }

    /// Total number of unique files tracked.
    pub fn file_count(&self) -> usize {
        self.entries.len()
    }

    /// Total number of modification records.
    pub fn modification_count(&self) -> usize {
        self.entries.values().map(|v| v.len()).sum()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::fake::{multi_artifact_key, FakeProvider};
    use crate::provider::{
        FileChangeDetail, FileChangeDiagnostics, FileChangeObservation, SourceProvider,
    };

    #[test]
    fn test_file_index_default() {
        let index = FileIndex::default();
        assert_eq!(index.file_count(), 0);
        assert_eq!(index.modification_count(), 0);
        assert!(index.get("/some/path").is_none());
    }

    #[test]
    fn test_file_index_search_empty() {
        let index = FileIndex::default();
        assert!(index.search("anything").is_empty());
    }

    #[test]
    fn provider_index_separates_applied_changes_from_attempts_and_matches_moves() {
        let provider = FakeProvider;
        let mut parsed = provider.parse(&multi_artifact_key()).unwrap();
        let owner = parsed.entries[2].id.clone();
        let record = parsed.entry_origins[&owner][0].clone();
        let change =
            |change_index, path: &str, outcome, move_path: Option<&str>| FileChangeObservation {
                owner: owner.clone(),
                operation_id: "call-7".into(),
                change_index,
                record: record.clone(),
                outcome_record: Some(record.clone()),
                path: path.into(),
                move_path: move_path.map(str::to_string),
                kind: FileChangeKind::Update,
                detail: FileChangeDetail::Patch("@@\n-old\n+new\n".into()),
                evidence: FileChangeEvidence::StructuredLifecycle,
                outcome,
                observed_at: None,
                native_version: None,
            };
        parsed.file_changes = vec![
            change(0, "src/0-retry.rs", FileChangeOutcome::Failed, None),
            change(
                1,
                "src/old.rs",
                FileChangeOutcome::Applied,
                Some("src/new.rs"),
            ),
            change(
                2,
                "src/old.rs",
                FileChangeOutcome::Applied,
                Some("docs/unrelated.md"),
            ),
        ];
        parsed.file_change_diagnostics = FileChangeDiagnostics {
            patch_calls: 1,
            calls_with_changes: 1,
            structured_changes: 3,
            ..Default::default()
        };
        assert!(
            parsed.validate_provenance().is_empty(),
            "{:?}",
            parsed.validate_provenance()
        );

        let index = ProviderFileIndex::from_parsed_sessions([("/work", &parsed)]);
        let moved = index.search_limited("new.rs", 10);
        assert_eq!(moved.total_files, 1);
        assert_eq!(moved.total_modifications, 1);
        assert_eq!(
            moved.selected.len(),
            1,
            "only the matching move is selected"
        );
        assert_eq!(moved.selected[0].1.outcome, FileChangeOutcome::Applied);
        assert_eq!(
            index.search_limited("retry.rs", 10).selected[0].1.outcome,
            FileChangeOutcome::Failed
        );
        let limited = index.search_limited("src/", 1);
        assert_eq!(limited.total_modifications, 2);
        assert_eq!(limited.total_attempts, 1);
        assert_eq!(limited.selected.len(), 1);
        assert_eq!(
            limited.selected[0].1.outcome,
            FileChangeOutcome::Failed,
            "one global limit follows deterministic path order instead of preferring applied rows"
        );

        let mut later = parsed.file_changes[1].clone();
        later.evidence = FileChangeEvidence::FileHistorySnapshot;
        later.native_version = Some(7);
        later.path = "src/cumulative.rs".into();
        later.observed_at = Some("2026-07-16T10:00:02Z".parse().unwrap());
        let mut earlier = later.clone();
        earlier.operation_id = "snapshot-earlier".into();
        earlier.observed_at = Some("2026-07-16T10:00:01Z".parse().unwrap());
        let mut projection = FileChangeProjection::default();
        projection.changes = vec![later, earlier];
        let mut builder = ProviderFileIndexBuilder::default();
        builder.add_projection("/work", &parsed.descriptor.key, &projection);
        let snapshots = builder.build();
        let snapshots = snapshots.search_limited("cumulative.rs", 10);
        assert_eq!(snapshots.total_modifications, 1);
        assert_eq!(
            snapshots.selected[0].1.timestamp,
            Some("2026-07-16T10:00:01Z".parse().unwrap()),
            "cumulative snapshots are deduplicated before indexing while retaining the earliest state"
        );

        parsed.semantics.get_mut(&owner).unwrap().activity = ActivityKind::InheritedHistory;
        let inherited = ProviderFileIndex::from_parsed_sessions([("/work", &parsed)]);
        assert_eq!(inherited.observation_count(), 0);
    }
}
