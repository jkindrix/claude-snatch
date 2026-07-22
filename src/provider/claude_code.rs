//! Claude Code as a [`SourceProvider`] (Phase A, milestones 1 + 1.5).
//!
//! Additive adapter over the existing `discovery` machinery — nothing in the
//! established pipeline calls this yet; characterization tests pin its
//! output to what `Session::parse()` produces. Threading it through the
//! CLI/MCP call sites is the rest of Phase A.
//!
//! Identity: main sessions use the global namespace. Subagent transcripts
//! (`agent-*`) are only unique within their parent session (and workflow
//! subdirectory), so their namespace is parent-qualified. A native id seen
//! under several roots/projects becomes ONE logical descriptor with several
//! artifacts. Discovery deduplicates identical agent ids within one project
//! (most-recent wins), so the provider additionally enumerates each parent's
//! subagent links and merges them by parent-qualified key — same-project id
//! collisions stay content-complete at this seam.
//!
//! Parsing: line-by-line with `LogEntry`'s tolerant deserializer so every
//! physical line gets a true record ordinal and disposition; damaged lines
//! go through the parser's torn-line salvage and surface as
//! `RecordOutcome::Recovered`. A provider-level `max_file_size` mirrors the
//! CLI option until parse limits are threaded.

use std::collections::BTreeMap;
use std::fs::File;
use std::io::{BufRead, BufReader, Read, Write};
use std::time::UNIX_EPOCH;

use serde::Deserialize;

use super::{
    ArtifactForm, ArtifactId, ArtifactRevision, ArtifactSnapshot, EntryId, FileChangeDetail,
    FileChangeDiagnostics, FileChangeEvidence, FileChangeKind, FileChangeObservation,
    FileChangeOutcome, FileChangeProjection, IdentifiedEntry, IngestionDiagnostics, LineageEdge,
    LineageEdgeKind, LogicalSessionKey, ParseDiagnostic, ParsedSession, ProviderCapabilities,
    ProviderError, ProviderId, RecordDisposition, RecordOutcome, RecordRef, SessionArtifact,
    SessionDescriptor, SessionNamespace, SourceProvider, SuppressionReason,
};
use crate::discovery::chain::extract_session_link;
use crate::discovery::{ClaudeDirectory, Session};
use crate::model::LogEntry;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SnapshotProjectionLine {
    #[serde(rename = "type")]
    kind: Option<String>,
    message_id: Option<String>,
    snapshot: Option<crate::model::message::SnapshotData>,
}

fn contains_snapshot_discriminator(prefix: &[u8]) -> bool {
    const KEY: &[u8] = b"\"type\"";
    const VALUE: &[u8] = b"file-history-snapshot";
    for (start, _) in prefix
        .windows(KEY.len())
        .enumerate()
        .filter(|(_, window)| *window == KEY)
    {
        let mut cursor = start + KEY.len();
        while prefix.get(cursor).is_some_and(u8::is_ascii_whitespace) {
            cursor += 1;
        }
        if prefix.get(cursor) != Some(&b':') {
            continue;
        }
        cursor += 1;
        while prefix.get(cursor).is_some_and(u8::is_ascii_whitespace) {
            cursor += 1;
        }
        if prefix.get(cursor) != Some(&b'\"') {
            continue;
        }
        cursor += 1;
        if prefix
            .get(cursor..)
            .and_then(|rest| rest.strip_prefix(VALUE))
            .is_some_and(|rest| rest.first() == Some(&b'\"'))
        {
            return true;
        }
    }
    false
}

/// Read one physical JSONL record, retaining it only when its bounded prefix
/// carries Claude's top-level snapshot discriminator. Claude writes `type`
/// in the record header; 64 KiB is deliberately far beyond observed headers.
/// A false-positive string merely causes one extra parse, while unrelated
/// giant tool outputs are drained without a line-sized allocation.
fn read_snapshot_candidate<R: BufRead>(
    reader: &mut R,
    record: &mut Vec<u8>,
) -> std::io::Result<Option<bool>> {
    const HEADER_LIMIT: usize = 64 * 1024;
    record.clear();
    let mut read_any = false;
    let mut candidate = false;
    loop {
        let available = reader.fill_buf()?;
        if available.is_empty() {
            return Ok(read_any.then_some(candidate));
        }
        read_any = true;
        let through_newline = available
            .iter()
            .position(|byte| *byte == b'\n')
            .map_or(available.len(), |index| index + 1);
        let chunk = &available[..through_newline];
        let ended = chunk.last() == Some(&b'\n');
        if candidate {
            record.extend_from_slice(chunk);
        } else if record.len() < HEADER_LIMIT {
            let keep = (HEADER_LIMIT - record.len()).min(chunk.len());
            record.extend_from_slice(&chunk[..keep]);
            candidate = contains_snapshot_discriminator(record);
            if candidate && keep < chunk.len() {
                record.extend_from_slice(&chunk[keep..]);
            }
        }
        reader.consume(through_newline);
        if ended {
            return Ok(Some(candidate));
        }
    }
}
use crate::parser::salvage_torn_line;

/// Provider-qualified logical identity of any discovered Claude Code session.
///
/// Main sessions use the global namespace; subagents are parent-qualified.
/// Shared by [`ClaudeCodeProvider`] and the provider-context threading in
/// the established pipeline.
pub fn logical_key(session: &Session) -> LogicalSessionKey {
    match session.parent_session_id() {
        Some(parent) if session.is_subagent() => LogicalSessionKey {
            provider: ProviderId::claude_code(),
            namespace: ClaudeCodeProvider::subagent_namespace(parent, session.path()),
            native_id: session.session_id().to_string(),
        },
        _ => LogicalSessionKey {
            provider: ProviderId::claude_code(),
            namespace: SessionNamespace::global(),
            native_id: session.session_id().to_string(),
        },
    }
}

/// Claude Code sessions (`~/.claude/projects/**.jsonl`) behind the provider
/// seam.
pub struct ClaudeCodeProvider {
    claude_dir: ClaudeDirectory,
    /// Maximum session file size accepted by [`SourceProvider::parse`]
    /// (bytes; `None` = unlimited). Immutable provider configuration,
    /// mirroring the CLI's `--max-file-size` until limits are threaded.
    max_file_size: Option<u64>,
}

fn record_snapshot_changes(
    entry: &LogEntry,
    owner: &EntryId,
    record: &RecordRef,
    observations: &mut Vec<FileChangeObservation>,
    diagnostics: &mut FileChangeDiagnostics,
) {
    let LogEntry::FileHistorySnapshot(snapshot) = entry else {
        return;
    };
    diagnostics.snapshot_records += 1;
    for (index, (path, backup)) in snapshot.snapshot.tracked_file_backups.iter().enumerate() {
        diagnostics.snapshot_changes += 1;
        observations.push(FileChangeObservation {
            owner: owner.clone(),
            operation_id: snapshot.message_id.clone(),
            change_index: u32::try_from(index).unwrap_or(u32::MAX),
            record: record.clone(),
            outcome_record: Some(record.clone()),
            path: path.clone(),
            move_path: None,
            kind: if backup.backup_file_name.is_none() {
                FileChangeKind::Add
            } else {
                FileChangeKind::Update
            },
            detail: FileChangeDetail::PathOnly,
            evidence: FileChangeEvidence::FileHistorySnapshot,
            outcome: FileChangeOutcome::Applied,
            observed_at: Some(backup.backup_time),
            native_version: Some(backup.version),
        });
    }
}

fn parse_claude_records(
    key: &LogicalSessionKey,
    descriptor: SessionDescriptor,
    path: &std::path::Path,
    max_file_size: Option<u64>,
) -> Result<ParsedSession, ProviderError> {
    if let Some(max) = max_file_size {
        let len = std::fs::metadata(path)?.len();
        if max > 0 && len > max {
            return Err(ProviderError::Other(format!(
                "session file {} exceeds max_file_size ({len} > {max} bytes)",
                path.display()
            )));
        }
    }
    let artifact_id = descriptor
        .preferred_artifact()
        .ok_or_else(|| ProviderError::Other(format!("descriptor {key} has no artifacts")))?
        .snapshot
        .id
        .clone();
    let reader = BufReader::new(File::open(path)?);
    let mut entries = Vec::new();
    let mut entry_origins = BTreeMap::new();
    let mut record_dispositions = Vec::new();
    let mut diagnostics = IngestionDiagnostics::default();
    let mut file_changes = Vec::new();
    let mut file_change_diagnostics = FileChangeDiagnostics::default();

    for (ordinal, line) in reader.lines().enumerate() {
        let ordinal = ordinal as u64;
        let record = RecordRef {
            artifact: artifact_id.clone(),
            ordinal,
        };
        // Line-read errors (e.g. invalid UTF-8) skip the record and
        // continue, mirroring the lenient parser — one corrupt line must
        // not turn a working session into total failure.
        let line = match line {
            Ok(l) => l,
            Err(e) => {
                diagnostics.unparseable += 1;
                record_dispositions.push(RecordDisposition {
                    record,
                    outcome: RecordOutcome::Unparseable {
                        error: ParseDiagnostic {
                            message: format!("I/O error: {e}"),
                        },
                    },
                });
                continue;
            }
        };
        if line.trim().is_empty() {
            diagnostics.suppressed += 1;
            record_dispositions.push(RecordDisposition {
                record,
                outcome: RecordOutcome::Suppressed {
                    reason: SuppressionReason::Other("blank line".into()),
                },
            });
            continue;
        }
        match serde_json::from_str::<LogEntry>(&line) {
            Ok(entry) => {
                let id = EntryId::deterministic(key, ordinal, 0);
                let unmodeled = matches!(entry, LogEntry::Unknown(_));
                record_snapshot_changes(
                    &entry,
                    &id,
                    &record,
                    &mut file_changes,
                    &mut file_change_diagnostics,
                );
                entries.push(IdentifiedEntry {
                    id: id.clone(),
                    entry,
                });
                entry_origins.insert(id.clone(), vec![record.clone()]);
                let outcome = if unmodeled {
                    diagnostics.unknown += 1;
                    RecordOutcome::Unknown { entries: vec![id] }
                } else {
                    diagnostics.mapped += 1;
                    RecordOutcome::Mapped(vec![id])
                };
                record_dispositions.push(RecordDisposition { record, outcome });
            }
            Err(e) => {
                // Torn/fused line? Mirror the established parser's salvage
                // before declaring the record unparseable.
                let salvaged = salvage_torn_line(&line);
                if salvaged.is_empty() {
                    diagnostics.unparseable += 1;
                    record_dispositions.push(RecordDisposition {
                        record,
                        outcome: RecordOutcome::Unparseable {
                            error: ParseDiagnostic {
                                message: e.to_string(),
                            },
                        },
                    });
                } else {
                    diagnostics.recovered += 1;
                    let mut ids = Vec::new();
                    for (sub, entry) in salvaged.into_iter().enumerate() {
                        let id = EntryId::deterministic(key, ordinal, sub as u32);
                        record_snapshot_changes(
                            &entry,
                            &id,
                            &record,
                            &mut file_changes,
                            &mut file_change_diagnostics,
                        );
                        entries.push(IdentifiedEntry {
                            id: id.clone(),
                            entry,
                        });
                        entry_origins.insert(id.clone(), vec![record.clone()]);
                        ids.push(id);
                    }
                    record_dispositions.push(RecordDisposition {
                        record,
                        outcome: RecordOutcome::Recovered {
                            entries: ids,
                            error: ParseDiagnostic {
                                message: e.to_string(),
                            },
                        },
                    });
                }
            }
        }
    }

    Ok(ParsedSession {
        descriptor,
        entries,
        entry_origins,
        record_dispositions,
        field_derivations: Vec::new(),
        semantics: BTreeMap::new(),
        file_changes,
        file_change_diagnostics,
        diagnostics,
    })
}

impl ClaudeCodeProvider {
    /// Wrap a discovered Claude Code data directory.
    pub fn new(claude_dir: ClaudeDirectory) -> Self {
        ClaudeCodeProvider {
            claude_dir,
            max_file_size: None,
        }
    }

    /// Configure the parse size limit (bytes; `None` = unlimited).
    #[must_use]
    pub fn with_max_file_size(mut self, max_file_size: Option<u64>) -> Self {
        self.max_file_size = max_file_size;
        self
    }

    /// Logical key for a main-session id (global namespace).
    fn key_for_main(&self, session_id: &str) -> LogicalSessionKey {
        LogicalSessionKey {
            provider: ProviderId::claude_code(),
            namespace: SessionNamespace::global(),
            native_id: session_id.to_string(),
        }
    }

    /// Parent-qualified namespace for a subagent transcript. Includes the
    /// workflow subdirectory when the transcript lives under one, so the
    /// same agent id under `subagents/` and `subagents/workflows/<wf>/`
    /// cannot collide either.
    fn subagent_namespace(parent_id: &str, transcript_path: &std::path::Path) -> SessionNamespace {
        let mut comps: Vec<&str> = transcript_path
            .components()
            .filter_map(|c| c.as_os_str().to_str())
            .collect();
        comps.pop(); // file name
                     // rposition: an ancestor directory may itself be named "subagents";
                     // the transcript's own subagents dir is the LAST one on the path.
        let sub_dirs = match comps.iter().rposition(|c| *c == "subagents") {
            Some(i) => comps[i + 1..].join("/"),
            None => String::new(),
        };
        if sub_dirs.is_empty() {
            SessionNamespace(format!("subagent:{parent_id}"))
        } else {
            SessionNamespace(format!("subagent:{parent_id}:{sub_dirs}"))
        }
    }

    /// Logical key for any discovered session.
    fn key_for_session(&self, session: &Session) -> LogicalSessionKey {
        logical_key(session)
    }

    fn artifact_for(&self, session: &Session) -> SessionArtifact {
        let revision = {
            let mtime = session
                .modified_time()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0);
            let len = std::fs::metadata(session.path())
                .map(|m| m.len())
                .unwrap_or(0);
            ArtifactRevision(format!("mtime={mtime};len={len}"))
        };
        SessionArtifact {
            snapshot: ArtifactSnapshot {
                id: ArtifactId {
                    provider_instance: self.claude_dir.root().display().to_string(),
                    locator: session.path().display().to_string(),
                },
                revision,
            },
            form: ArtifactForm::PlainFile,
            archived: false,
        }
    }

    fn all_sessions(&self) -> Result<Vec<Session>, ProviderError> {
        self.claude_dir
            .all_sessions()
            .map_err(|e| ProviderError::Other(e.to_string()))
    }

    /// Group discovered sessions into logical descriptors: one descriptor
    /// per logical key, merging duplicate copies (e.g. the same session
    /// uuid under two project directories) into multiple artifacts.
    /// Discovery deduplicates identical agent ids within one project
    /// (most-recent wins), so subagents are additionally enumerated through
    /// each parent's `subagent_links()` and merged by parent-qualified key —
    /// same-project id collisions stay content-complete at this seam.
    fn descriptors(&self) -> Result<Vec<(SessionDescriptor, Vec<Session>)>, ProviderError> {
        let mut grouped: BTreeMap<LogicalSessionKey, (Vec<SessionArtifact>, Vec<Session>)> =
            BTreeMap::new();
        let mut insert = |key: LogicalSessionKey, artifact: SessionArtifact, session: Session| {
            let slot = grouped.entry(key).or_default();
            if !slot.0.iter().any(|a| a.snapshot.id == artifact.snapshot.id) {
                slot.0.push(artifact);
                slot.1.push(session);
            }
        };
        let sessions = self.all_sessions()?;
        for session in &sessions {
            if session.is_subagent() {
                continue;
            }
            // Recover same-project subagents that discovery's per-project
            // id-dedup dropped, via the parent's sidecar links.
            for link in session.subagent_links() {
                let Ok(sub) = Session::from_path(&link.path, session.project_path()) else {
                    continue; // pruned transcript: lineage keeps the edge
                };
                let key = LogicalSessionKey {
                    provider: ProviderId::claude_code(),
                    namespace: Self::subagent_namespace(session.session_id(), &link.path),
                    native_id: link.agent_session_id.clone(),
                };
                let artifact = self.artifact_for(&sub);
                insert(key, artifact, sub);
            }
        }
        for session in sessions {
            let key = self.key_for_session(&session);
            let artifact = self.artifact_for(&session);
            insert(key, artifact, session);
        }
        Ok(grouped
            .into_iter()
            .map(|(key, (artifacts, sessions))| (SessionDescriptor { key, artifacts }, sessions))
            .collect())
    }

    /// Resolve a logical key to its descriptor and the session backing the
    /// preferred artifact.
    fn resolve(
        &self,
        key: &LogicalSessionKey,
    ) -> Result<(SessionDescriptor, Session), ProviderError> {
        if key.provider != ProviderId::claude_code() {
            return Err(ProviderError::NotFound(key.to_string()));
        }
        let (descriptor, sessions) = self
            .descriptors()?
            .into_iter()
            .find(|(d, _)| d.key == *key)
            .ok_or_else(|| ProviderError::NotFound(key.to_string()))?;
        let preferred = descriptor
            .preferred_artifact()
            .ok_or_else(|| ProviderError::Other(format!("descriptor {key} has no artifacts")))?
            .snapshot
            .id
            .clone();
        let session = sessions
            .into_iter()
            .find(|s| s.path().display().to_string() == preferred.locator)
            .ok_or_else(|| ProviderError::NotFound(key.to_string()))?;
        Ok((descriptor, session))
    }

    fn project_context_for_session(
        session: &Session,
    ) -> Result<super::project::SessionProjectContext, ProviderError> {
        // Project grouping needs only the small metadata prelude, not a
        // fully parsed transcript. `quick_metadata_cached()` currently parses
        // every entry; calling it for a large corpus made an otherwise linear
        // union scan take tens of seconds and peak above 1 GiB. Bound both
        // bytes and lines so one hostile/huge first record cannot recreate
        // that behavior.
        const PREFIX_BYTES: u64 = 256 * 1024;
        const PREFIX_LINES: usize = 32;
        let file = File::open(session.path())?;
        let mut reader = BufReader::new(file).take(PREFIX_BYTES);
        let mut line = Vec::new();
        let mut cwd = None;
        let mut git_branch = None;
        let mut started_at = None;
        for _ in 0..PREFIX_LINES {
            line.clear();
            if reader.read_until(b'\n', &mut line)? == 0 {
                break;
            }
            let Ok(value) = serde_json::from_slice::<serde_json::Value>(&line) else {
                continue;
            };
            cwd = cwd.or_else(|| {
                value
                    .get("cwd")
                    .and_then(|v| v.as_str())
                    .map(str::to_string)
            });
            git_branch = git_branch.or_else(|| {
                value
                    .get("gitBranch")
                    .or_else(|| value.get("git_branch"))
                    .and_then(|v| v.as_str())
                    .map(str::to_string)
            });
            started_at = started_at.or_else(|| {
                value
                    .get("timestamp")
                    .and_then(|v| v.as_str())
                    .and_then(|timestamp| chrono::DateTime::parse_from_rfc3339(timestamp).ok())
                    .map(|timestamp| timestamp.with_timezone(&chrono::Utc))
            });
            if cwd.is_some() && git_branch.is_some() && started_at.is_some() {
                break;
            }
        }
        Ok(super::project::SessionProjectContext {
            cwd: Some(cwd.unwrap_or_else(|| session.project_path().to_string())),
            git_branch,
            started_at,
            modified_at: Some(session.modified_datetime()),
            artifact_bytes: session.file_size(),
            ..Default::default()
        })
    }

    fn stream_file(path: &std::path::Path, out: &mut dyn Write) -> Result<(), ProviderError> {
        let mut file = File::open(path)?;
        std::io::copy(&mut file, out)?;
        Ok(())
    }

    /// Resolve the preferred artifact of a descriptor from the current
    /// inventory without rescanning every project. Lossy/non-local locator
    /// edge cases fall back to the established inventory-backed resolver.
    fn path_for_discovered(
        &self,
        descriptor: &SessionDescriptor,
    ) -> Result<std::path::PathBuf, ProviderError> {
        if descriptor.key.provider != ProviderId::claude_code() {
            return Err(ProviderError::NotFound(descriptor.key.to_string()));
        }
        let preferred = descriptor.preferred_artifact().ok_or_else(|| {
            ProviderError::Other(format!("descriptor {} has no artifacts", descriptor.key))
        })?;
        let expected_instance = self.claude_dir.root().display().to_string();
        let candidate = std::path::PathBuf::from(&preferred.snapshot.id.locator);
        let direct = preferred.snapshot.id.provider_instance == expected_instance
            && candidate.starts_with(self.claude_dir.root())
            && std::fs::symlink_metadata(&candidate)
                .is_ok_and(|metadata| metadata.file_type().is_file());
        if direct {
            return Ok(candidate);
        }
        self.resolve(&descriptor.key)
            .map(|(_, session)| session.path().to_path_buf())
    }
}

impl SourceProvider for ClaudeCodeProvider {
    fn id(&self) -> ProviderId {
        ProviderId::claude_code()
    }

    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities {
            native_export: true,
            raw_jsonl: true,
            // The Claude adapter does not yet emit prompt/turn semantics;
            // surfaces must keep classic heuristics for it (round-23).
            semantic_annotations: false,
            pricing: crate::provider::ProviderPricing::KnownModelRates,
        }
    }

    fn sessions(&self) -> Result<Vec<SessionDescriptor>, ProviderError> {
        Ok(self.descriptors()?.into_iter().map(|(d, _)| d).collect())
    }

    fn sessions_with_project_context(
        &self,
    ) -> Result<super::SessionProjectContexts, ProviderError> {
        self.descriptors()?
            .into_iter()
            .map(|(descriptor, sessions)| {
                let preferred = descriptor
                    .preferred_artifact()
                    .ok_or_else(|| {
                        ProviderError::Other(format!(
                            "descriptor {} has no artifacts",
                            descriptor.key
                        ))
                    })?
                    .snapshot
                    .id
                    .clone();
                let session = descriptor
                    .artifacts
                    .iter()
                    .position(|artifact| artifact.snapshot.id == preferred)
                    .and_then(|index| sessions.get(index))
                    .ok_or_else(|| ProviderError::NotFound(descriptor.key.to_string()));
                let context = session.and_then(Self::project_context_for_session);
                Ok((descriptor, context))
            })
            .collect()
    }

    fn project_context(
        &self,
        key: &LogicalSessionKey,
    ) -> Result<super::project::SessionProjectContext, ProviderError> {
        let (_, session) = self.resolve(key)?;
        Self::project_context_for_session(&session)
    }

    fn parse_cache_token(&self, key: &LogicalSessionKey) -> Result<String, ProviderError> {
        let (descriptor, _) = self.resolve(key)?;
        self.parse_cache_token_for_descriptor(&descriptor)
    }

    fn parse_cache_token_for_descriptor(
        &self,
        descriptor: &SessionDescriptor,
    ) -> Result<String, ProviderError> {
        if descriptor.key.provider != ProviderId::claude_code() {
            return Err(ProviderError::NotFound(descriptor.key.to_string()));
        }
        Ok(format!(
            "v1\x1eclaude-code\x1e{}\x1emax_file={:?}",
            super::descriptor_state_token(descriptor),
            self.max_file_size
        ))
    }

    fn parse(&self, key: &LogicalSessionKey) -> Result<ParsedSession, ProviderError> {
        let (descriptor, session) = self.resolve(key)?;
        parse_claude_records(key, descriptor, session.path(), self.max_file_size)
    }

    fn parse_discovered(
        &self,
        descriptor: &SessionDescriptor,
    ) -> Result<ParsedSession, ProviderError> {
        let path = self.path_for_discovered(descriptor)?;
        parse_claude_records(
            &descriptor.key,
            descriptor.clone(),
            &path,
            self.max_file_size,
        )
    }

    fn file_change_projection(
        &self,
        descriptor: &SessionDescriptor,
    ) -> Result<FileChangeProjection, ProviderError> {
        let path = self.path_for_discovered(descriptor)?;
        if let Some(max) = self.max_file_size {
            let len = std::fs::metadata(&path)?.len();
            if max > 0 && len > max {
                return Err(ProviderError::Other(format!(
                    "session file {} exceeds max_file_size ({len} > {max} bytes)",
                    path.display()
                )));
            }
        }
        let artifact = descriptor
            .preferred_artifact()
            .ok_or_else(|| {
                ProviderError::Other(format!("descriptor {} has no artifacts", descriptor.key))
            })?
            .snapshot
            .id
            .clone();
        let mut reader = BufReader::new(File::open(path)?);
        let mut line = Vec::new();
        let mut ordinal = 0_u64;
        let mut projection = FileChangeProjection::default();
        while let Some(candidate) = read_snapshot_candidate(&mut reader, &mut line)? {
            let current = ordinal;
            ordinal = ordinal.saturating_add(1);
            if !candidate {
                continue;
            }
            let Ok(native) = serde_json::from_slice::<SnapshotProjectionLine>(&line) else {
                continue;
            };
            if native.kind.as_deref() != Some("file-history-snapshot") {
                continue;
            }
            let operation_id = native.message_id.ok_or_else(|| {
                ProviderError::Other(format!(
                    "file-history-snapshot record #{current} has no messageId"
                ))
            })?;
            let snapshot = native.snapshot.ok_or_else(|| {
                ProviderError::Other(format!(
                    "file-history-snapshot record #{current} has no snapshot"
                ))
            })?;
            let owner = EntryId::deterministic(&descriptor.key, current, 0);
            projection
                .owner_timestamps
                .insert(owner.clone(), snapshot.timestamp);
            let record = RecordRef {
                artifact: artifact.clone(),
                ordinal: current,
            };
            for (index, (path, backup)) in snapshot.tracked_file_backups.into_iter().enumerate() {
                projection.changes.push(FileChangeObservation {
                    owner: owner.clone(),
                    operation_id: operation_id.clone(),
                    change_index: u32::try_from(index).unwrap_or(u32::MAX),
                    record: record.clone(),
                    outcome_record: Some(record.clone()),
                    path,
                    move_path: None,
                    kind: if backup.backup_file_name.is_none() {
                        FileChangeKind::Add
                    } else {
                        FileChangeKind::Update
                    },
                    detail: FileChangeDetail::PathOnly,
                    evidence: FileChangeEvidence::FileHistorySnapshot,
                    outcome: FileChangeOutcome::Applied,
                    observed_at: Some(backup.backup_time),
                    native_version: Some(backup.version),
                });
            }
        }
        Ok(projection)
    }

    fn lineage(&self) -> Result<Vec<LineageEdge>, ProviderError> {
        let sessions = self.all_sessions()?;
        let mut edges = Vec::new();

        for session in sessions.iter().filter(|s| !s.is_subagent()) {
            // Continuation: direct parent link from the file's internal
            // sessionId — independent of complete-chain reconstruction, so a
            // pruned/missing parent still yields a (dangling) edge.
            if let Some((internal_sid, _slug, _started)) = extract_session_link(session.path(), 10)
            {
                if internal_sid != session.session_id() {
                    edges.push(LineageEdge {
                        from: self.key_for_main(&internal_sid),
                        to: self.key_for_main(session.session_id()),
                        kind: LineageEdgeKind::Continuation,
                    });
                }
            }

            // Spawn: subagent sidecars, carrying the metadata downstream
            // matching/presentation needs. Endpoints may dangle if a
            // transcript was pruned; the edge is kept.
            for link in session.subagent_links() {
                edges.push(LineageEdge {
                    from: self.key_for_main(session.session_id()),
                    to: LogicalSessionKey {
                        provider: ProviderId::claude_code(),
                        namespace: Self::subagent_namespace(session.session_id(), &link.path),
                        native_id: link.agent_session_id.clone(),
                    },
                    kind: LineageEdgeKind::Spawn {
                        tool_use_id: link.tool_use_id.clone(),
                        agent_type: link.agent_type.clone(),
                        description: link.description.clone(),
                    },
                });
            }
        }

        edges.sort();
        edges.dedup();
        Ok(edges)
    }

    fn write_archive(
        &self,
        key: &LogicalSessionKey,
        out: &mut dyn Write,
    ) -> Result<(), ProviderError> {
        // Lossless framed multipart bundle: line 1 is the manifest carrying
        // per-artifact byte lengths; the body is EVERY artifact's bytes
        // concatenated in manifest order (streamed). Divergent duplicate
        // copies are all preserved — archiving only one would silently drop
        // the others' content.
        let (descriptor, _) = self.resolve(key)?;
        let mut lens = Vec::with_capacity(descriptor.artifacts.len());
        for a in &descriptor.artifacts {
            lens.push(std::fs::metadata(&a.snapshot.id.locator)?.len());
        }
        let manifest = serde_json::json!({
            "manifest": {
                "provider": self.id().0,
                "session": key.to_string(),
                "artifacts": descriptor
                    .artifacts
                    .iter()
                    .zip(&lens)
                    .map(|(a, len)| serde_json::json!({
                        "instance": a.snapshot.id.provider_instance,
                        "locator": a.snapshot.id.locator,
                        "revision": a.snapshot.revision.0,
                        "archived": a.archived,
                        "bytes": len,
                    }))
                    .collect::<Vec<_>>(),
            }
        });
        serde_json::to_writer(&mut *out, &manifest)
            .map_err(|e| ProviderError::Other(format!("manifest serialization: {e}")))?;
        out.write_all(b"\n")?;
        for (a, expected) in descriptor.artifacts.iter().zip(&lens) {
            let mut file = File::open(&a.snapshot.id.locator)?;
            let copied = std::io::copy(&mut file, out)?;
            if copied != *expected {
                return Err(ProviderError::Other(format!(
                    "artifact {} changed while archiving ({copied} != {expected} bytes)",
                    a.snapshot.id.locator
                )));
            }
        }
        Ok(())
    }

    fn write_native(
        &self,
        artifact: &ArtifactId,
        out: &mut dyn Write,
    ) -> Result<(), ProviderError> {
        // Resolve the id against DISCOVERED artifacts and stream the stored
        // path — never a caller-supplied string. (A lexical prefix check is
        // forgeable: `<root>/../outside` passes `Path::starts_with`.)
        for session in self.all_sessions()? {
            let known = self.artifact_for(&session);
            if known.snapshot.id == *artifact {
                return Self::stream_file(session.path(), out);
            }
        }
        Err(ProviderError::NotFound(format!(
            "artifact {}",
            artifact.locator
        )))
    }

    fn write_raw_jsonl(
        &self,
        key: &LogicalSessionKey,
        out: &mut dyn Write,
    ) -> Result<(), ProviderError> {
        // This session's preferred artifact, verbatim. (Chain-order
        // concatenation across resume chains stays a consumer concern, as in
        // the CLI's chain-aware raw export.)
        let (_, session) = self.resolve(key)?;
        Self::stream_file(session.path(), out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SESSION_A: &str = "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee";
    const SESSION_B: &str = "bbbbbbbb-cccc-dddd-eeee-ffffffffffff";
    const SESSION_GONE: &str = "99999999-9999-9999-9999-999999999999";
    const SESSION_UTF8: &str = "e8e8e8e8-aaaa-bbbb-cccc-444444444444";

    /// Independently derive the snapshot projection from native JSON. This
    /// intentionally does not use the production snapshot mapper or generic
    /// provenance validator, so wrong path/version projections cannot make a
    /// confirmatory test pass by agreeing with themselves.
    fn audit_native_snapshot_projection(native: &str, parsed: &ParsedSession) -> Vec<String> {
        let mut expected = Vec::new();
        for (ordinal, line) in native.lines().enumerate() {
            let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else {
                continue;
            };
            if value.get("type").and_then(serde_json::Value::as_str)
                != Some("file-history-snapshot")
            {
                continue;
            }
            let operation = value
                .get("messageId")
                .and_then(serde_json::Value::as_str)
                .unwrap();
            let backups = value
                .pointer("/snapshot/trackedFileBackups")
                .and_then(serde_json::Value::as_object)
                .unwrap();
            for (path, backup) in backups {
                expected.push((
                    u64::try_from(ordinal).unwrap(),
                    operation.to_string(),
                    path.clone(),
                    if backup
                        .get("backupFileName")
                        .is_some_and(serde_json::Value::is_null)
                    {
                        FileChangeKind::Add
                    } else {
                        FileChangeKind::Update
                    },
                    backup
                        .get("backupTime")
                        .and_then(serde_json::Value::as_str)
                        .unwrap()
                        .parse::<chrono::DateTime<chrono::Utc>>()
                        .unwrap(),
                    u32::try_from(
                        backup
                            .get("version")
                            .and_then(serde_json::Value::as_u64)
                            .unwrap(),
                    )
                    .unwrap(),
                ));
            }
        }
        expected.sort();

        let mut actual: Vec<_> = parsed
            .file_changes
            .iter()
            .filter(|change| change.evidence == FileChangeEvidence::FileHistorySnapshot)
            .map(|change| {
                (
                    change.record.ordinal,
                    change.operation_id.clone(),
                    change.path.clone(),
                    change.kind,
                    change.observed_at.unwrap(),
                    change.native_version.unwrap(),
                )
            })
            .collect();
        actual.sort();
        if actual == expected {
            Vec::new()
        } else {
            vec![format!(
                "native snapshot projection mismatch: expected {expected:?}, got {actual:?}"
            )]
        }
    }

    fn user_line(uuid: &str, session: &str, text: &str) -> String {
        format!(
            r#"{{"type":"user","uuid":"{uuid}","parentUuid":null,"timestamp":"2026-01-01T00:00:00Z","sessionId":"{session}","version":"2.1.0","cwd":"/tmp/proj","message":{{"role":"user","content":"{text}"}}}}"#
        )
    }

    fn agent_line(session: &str) -> String {
        format!(
            r#"{{"type":"assistant","uuid":"s1","parentUuid":null,"timestamp":"2026-01-01T00:30:00Z","sessionId":"{session}","version":"2.1.0","isSidechain":true,"message":{{"id":"sm1","type":"message","role":"assistant","content":[{{"type":"text","text":"sub"}}],"model":"claude-x"}}}}"#
        )
    }

    fn write_subagent(project: &std::path::Path, parent: &str, agent: &str) {
        let dir = project.join(parent).join("subagents");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join(format!("{agent}.jsonl")),
            agent_line(parent) + "\n",
        )
        .unwrap();
        std::fs::write(
            dir.join(format!("{agent}.meta.json")),
            r#"{"agentType":"Explore","description":"scan"}"#,
        )
        .unwrap();
    }

    /// Fixture: project P1 has session A (valid + blank + garbage + torn +
    /// unknown lines), session B continuing A, session C continuing a
    /// MISSING parent, subagent agent-x1 under A. Project P2 has a COPY of
    /// session A's file (same uuid) and its own parent D with an agent-x1
    /// subagent (identity collision with P1's agent-x1).
    fn fixture() -> (tempfile::TempDir, ClaudeCodeProvider) {
        let tmp = tempfile::tempdir().unwrap();
        let p1 = tmp.path().join("projects").join("-tmp-proj");
        let p2 = tmp.path().join("projects").join("-tmp-other");
        std::fs::create_dir_all(&p1).unwrap();
        std::fs::create_dir_all(&p2).unwrap();

        let torn = format!(
            "{}{}",
            user_line("t1", SESSION_A, "torn-first"),
            user_line("t2", SESSION_A, "torn-second")
        );
        let a = format!(
            "{}\n{}\n\nnot json at all\n{}\n{}\n",
            user_line("u1", SESSION_A, "hello"),
            format_args!(
                r#"{{"type":"assistant","uuid":"a1","parentUuid":"u1","timestamp":"2026-01-01T00:00:01Z","sessionId":"{SESSION_A}","version":"2.1.0","message":{{"id":"m1","type":"message","role":"assistant","content":[{{"type":"text","text":"hi"}}],"model":"claude-x"}}}}"#
            ),
            torn,
            r#"{"type":"never-heard-of-it","payload":{"x":1}}"#,
        );
        std::fs::write(p1.join(format!("{SESSION_A}.jsonl")), &a).unwrap();

        // B continues A; C continues a parent that no longer exists.
        std::fs::write(
            p1.join(format!("{SESSION_B}.jsonl")),
            user_line("u2", SESSION_A, "resumed") + "\n",
        )
        .unwrap();
        let session_c = "cccccccc-dddd-eeee-ffff-000000000000";
        std::fs::write(
            p1.join(format!("{session_c}.jsonl")),
            user_line("u3", SESSION_GONE, "orphan") + "\n",
        )
        .unwrap();

        write_subagent(&p1, SESSION_A, "agent-x1");

        // Same-project collision: a second parent in P1 with the SAME agent
        // id (discovery's per-project dedup would hide one of them).
        let session_d2 = "d2d2d2d2-aaaa-bbbb-cccc-333333333333";
        std::fs::write(
            p1.join(format!("{session_d2}.jsonl")),
            user_line("u5", session_d2, "second parent") + "\n",
        )
        .unwrap();
        write_subagent(&p1, session_d2, "agent-x1");

        // A session containing an invalid-UTF-8 line between valid entries.
        let mut utf8_bytes = Vec::new();
        utf8_bytes.extend_from_slice(user_line("v1", SESSION_UTF8, "before").as_bytes());
        utf8_bytes.extend_from_slice(b"\n\xff\xfe broken bytes \xff\n");
        utf8_bytes.extend_from_slice(user_line("v2", SESSION_UTF8, "after").as_bytes());
        utf8_bytes.push(b'\n');
        std::fs::write(p1.join(format!("{SESSION_UTF8}.jsonl")), utf8_bytes).unwrap();

        // P2: DIVERGENT duplicate copy of session A (extra trailing entry) +
        // a different parent with the SAME agent id.
        std::fs::write(
            p2.join(format!("{SESSION_A}.jsonl")),
            format!("{a}{}\n", user_line("u9", SESSION_A, "divergent extra")),
        )
        .unwrap();
        let session_d = "dddddddd-eeee-ffff-0000-111111111111";
        std::fs::write(
            p2.join(format!("{session_d}.jsonl")),
            user_line("u4", session_d, "other project") + "\n",
        )
        .unwrap();
        write_subagent(&p2, session_d, "agent-x1");

        let dir = ClaudeDirectory::from_path(tmp.path()).unwrap();
        (tmp, ClaudeCodeProvider::new(dir))
    }

    fn key(id: &str) -> LogicalSessionKey {
        LogicalSessionKey {
            provider: ProviderId::claude_code(),
            namespace: SessionNamespace::global(),
            native_id: id.into(),
        }
    }

    #[test]
    fn duplicate_main_uuid_becomes_one_descriptor_with_two_artifacts() {
        let (_tmp, p) = fixture();
        let sessions = p.sessions().unwrap();
        let a: Vec<_> = sessions
            .iter()
            .filter(|d| d.key.native_id == SESSION_A)
            .collect();
        assert_eq!(a.len(), 1, "one logical descriptor, not duplicates");
        assert_eq!(a[0].artifacts.len(), 2, "both copies as artifacts");
        assert!(a[0].validate().is_empty());
    }

    #[test]
    fn bulk_project_context_comes_from_bounded_native_prelude() {
        let tmp = tempfile::tempdir().unwrap();
        let project = tmp.path().join("projects").join("-tmp-fast-project");
        std::fs::create_dir_all(&project).unwrap();
        let path = project.join(format!("{SESSION_A}.jsonl"));
        let first = format!(
            r#"{{"type":"user","uuid":"u1","parentUuid":null,"timestamp":"2026-02-03T04:05:06Z","sessionId":"{SESSION_A}","version":"2.1.0","cwd":"/tmp/authoritative","gitBranch":"feature/fast-union","message":{{"role":"user","content":"hello"}}}}"#
        );
        let mut bytes = first.into_bytes();
        bytes.push(b'\n');
        // A large, invalid transcript tail must be irrelevant to project
        // inventory; the old full-parser path needlessly consumed it.
        bytes.resize(bytes.len() + 512 * 1024, b'x');
        std::fs::write(&path, &bytes).unwrap();

        let provider = ClaudeCodeProvider::new(ClaudeDirectory::from_path(tmp.path()).unwrap());
        let rows = provider.sessions_with_project_context().unwrap();
        let (_, context) = rows
            .into_iter()
            .find(|(descriptor, _)| descriptor.key.native_id == SESSION_A)
            .unwrap();
        let context = context.unwrap();
        assert_eq!(context.cwd.as_deref(), Some("/tmp/authoritative"));
        assert_eq!(context.git_branch.as_deref(), Some("feature/fast-union"));
        assert_eq!(
            context.started_at.unwrap().to_rfc3339(),
            "2026-02-03T04:05:06+00:00"
        );
        assert_eq!(context.artifact_bytes, bytes.len() as u64);
    }

    #[test]
    fn subagent_identity_is_parent_qualified() {
        let (_tmp, p) = fixture();
        let sessions = p.sessions().unwrap();
        let agents: Vec<_> = sessions
            .iter()
            .filter(|d| d.key.native_id == "agent-x1")
            .collect();
        // Three parents (two in the SAME project — the case discovery's
        // per-project id-dedup hides — plus one in the other project).
        assert_eq!(agents.len(), 3, "same agent id under three parents");
        let keys: std::collections::BTreeSet<_> = agents.iter().map(|d| &d.key).collect();
        assert_eq!(keys.len(), 3, "parent-qualified namespaces must differ");
        for d in &agents {
            assert!(d.key.namespace.0.starts_with("subagent:"));
            // Link-recovered subagents parse successfully too.
            assert!(FakeCheck::parse_ok(&p, &d.key));
        }
    }

    /// Helper: parse succeeds and validates for a key.
    struct FakeCheck;
    impl FakeCheck {
        fn parse_ok(p: &ClaudeCodeProvider, key: &LogicalSessionKey) -> bool {
            p.parse(key)
                .map(|parsed| parsed.validate_provenance().is_empty())
                .unwrap_or(false)
        }
    }

    #[test]
    fn parse_matches_session_parse_and_accounts_every_line() {
        let (_tmp, p) = fixture();
        let parsed = p.parse(&key(SESSION_A)).unwrap();
        assert!(
            parsed.validate_provenance().is_empty(),
            "{:?}",
            parsed.validate_provenance()
        );

        // Characterization: same entries the established path produces
        // (including salvage — both paths recover the torn line).
        let (descriptor, session) = p.resolve(&key(SESSION_A)).unwrap();
        let discovered = p.parse_discovered(&descriptor).unwrap();
        assert_eq!(discovered.descriptor, parsed.descriptor);
        assert_eq!(discovered.record_dispositions, parsed.record_dispositions);
        assert_eq!(discovered.entries.len(), parsed.entries.len());
        for (direct, resolved) in discovered.entries.iter().zip(&parsed.entries) {
            assert_eq!(direct.id, resolved.id);
            assert_eq!(
                serde_json::to_value(&direct.entry).unwrap(),
                serde_json::to_value(&resolved.entry).unwrap(),
                "descriptor-aware parsing must match key-based resolution"
            );
        }
        assert_eq!(
            p.parse_cache_token_for_descriptor(&descriptor).unwrap(),
            p.parse_cache_token(&key(SESSION_A)).unwrap(),
            "descriptor-aware revision tokens must match key-based resolution"
        );
        let baseline = session.parse().unwrap();
        assert_eq!(parsed.entries.len(), baseline.len());
        for (mine, theirs) in parsed.entries.iter().zip(baseline.iter()) {
            assert_eq!(
                serde_json::to_value(&mine.entry).unwrap(),
                serde_json::to_value(theirs).unwrap(),
                "provider entry diverged from Session::parse"
            );
        }

        // Preferred artifact is the DIVERGENT P2 copy (stable ArtifactId
        // tie-break: "-tmp-other" sorts before "-tmp-proj"), which carries
        // one extra mapped entry. Every physical line accounted for:
        // 3 mapped, 1 blank suppressed, 1 garbage unparseable, 1 torn line
        // recovered (salvage treats the damaged prefix as lost and recovers
        // the clean tail — matching the established parser), 1 unknown-typed
        // preserved.
        assert_eq!(parsed.record_dispositions.len(), 7);
        assert_eq!(
            parsed.diagnostics,
            IngestionDiagnostics {
                mapped: 3,
                suppressed: 1,
                unknown: 1,
                recovered: 1,
                unparseable: 1
            }
        );
        let recovered = parsed
            .record_dispositions
            .iter()
            .find_map(|d| match &d.outcome {
                RecordOutcome::Recovered { entries, .. } => Some(entries.len()),
                _ => None,
            })
            .expect("torn line recovered");
        assert_eq!(recovered, 1, "the clean tail entry is salvaged");
    }

    #[test]
    fn file_history_snapshots_project_provider_neutral_changes() {
        let tmp = tempfile::tempdir().unwrap();
        let project = tmp.path().join("projects/-tmp-snapshots");
        std::fs::create_dir_all(&project).unwrap();
        let native = serde_json::json!({
            "type": "file-history-snapshot",
            "messageId": "snapshot-1",
            "isSnapshotUpdate": false,
            "snapshot": {
                "messageId": "snapshot-1",
                "timestamp": "2026-07-22T10:00:00Z",
                "trackedFileBackups": {
                    "/work/new.rs": {
                        "backupFileName": null,
                        "version": 1,
                        "backupTime": "2026-07-22T10:00:01Z"
                    },
                    "/work/existing.rs": {
                        "backupFileName": "existing.rs@v2",
                        "version": 2,
                        "backupTime": "2026-07-22T10:00:02Z"
                    }
                }
            }
        })
        .to_string()
            + "\n";
        std::fs::write(project.join(format!("{SESSION_A}.jsonl")), &native).unwrap();
        let provider = ClaudeCodeProvider::new(ClaudeDirectory::from_path(tmp.path()).unwrap());
        let parsed = provider.parse(&key(SESSION_A)).unwrap();
        let descriptor = provider.sessions().unwrap().remove(0);
        let compact = provider.file_change_projection(&descriptor).unwrap();
        assert!(
            parsed.validate_provenance().is_empty(),
            "{:?}",
            parsed.validate_provenance()
        );
        assert_eq!(parsed.file_change_diagnostics.snapshot_records, 1);
        assert_eq!(parsed.file_change_diagnostics.snapshot_changes, 2);
        assert_eq!(parsed.file_changes.len(), 2);
        assert_eq!(parsed.file_changes[0].operation_id, "snapshot-1");
        assert_eq!(parsed.file_changes[0].kind, FileChangeKind::Add);
        assert_eq!(parsed.file_changes[0].native_version, Some(1));
        assert_eq!(parsed.file_changes[1].kind, FileChangeKind::Update);
        assert_eq!(parsed.file_changes[1].native_version, Some(2));
        assert!(parsed.file_changes.iter().all(|change| change.evidence
            == FileChangeEvidence::FileHistorySnapshot
            && change.outcome == FileChangeOutcome::Applied
            && change.outcome_record.as_ref() == Some(&change.record)));
        assert_eq!(
            compact.changes, parsed.file_changes,
            "lightweight snapshot projection must match the complete parser"
        );
        assert!(audit_native_snapshot_projection(&native, &parsed).is_empty());

        let mut wrong_path = parsed.clone();
        wrong_path.file_changes[0].path = "/work/wrong.rs".into();
        assert!(audit_native_snapshot_projection(&native, &wrong_path)
            .iter()
            .any(|violation| violation.contains("projection mismatch")));

        let mut wrong_version = parsed.clone();
        wrong_version.file_changes[0].native_version = Some(99);
        assert!(audit_native_snapshot_projection(&native, &wrong_version)
            .iter()
            .any(|violation| violation.contains("projection mismatch")));

        let mut forged = parsed;
        forged.file_changes[0].operation_id = "different-snapshot".into();
        assert!(forged
            .validate_provenance()
            .iter()
            .any(|violation| violation.contains("does not contain snapshot")));
    }

    #[test]
    fn lightweight_snapshot_scan_skips_large_and_malformed_records_without_losing_later_evidence() {
        assert!(!contains_snapshot_discriminator(
            br#"{"type":"user","content":"file-history-snapshot"}"#
        ));
        assert!(!contains_snapshot_discriminator(b"{}\n"));
        assert!(!contains_snapshot_discriminator(br#"{"type":"fi"#));
        assert!(contains_snapshot_discriminator(
            br#"{"type" : "file-history-snapshot","messageId":"x"}"#
        ));
        let tmp = tempfile::tempdir().unwrap();
        let project = tmp.path().join("projects/-tmp-snapshots");
        std::fs::create_dir_all(&project).unwrap();
        let irrelevant = serde_json::json!({
            "type": "user",
            "messageId": "not-a-snapshot",
            "message": {"content": format!("file-history-snapshot {}", "x".repeat(1_000_000))}
        });
        let snapshot = serde_json::json!({
            "type": "file-history-snapshot",
            "messageId": "snapshot-late",
            "snapshot": {
                "messageId": "snapshot-late",
                "timestamp": "2026-07-22T10:00:00Z",
                "trackedFileBackups": {
                    "/work/late.rs": {
                        "backupFileName": null,
                        "version": 1,
                        "backupTime": "2026-07-22T10:00:01Z"
                    }
                }
            }
        });
        // Exercise a discriminator spanning several BufReader chunks near
        // the documented bound. The native corpus audit currently sees a
        // maximum byte offset of 2,859 across 42,875 snapshot records.
        let snapshot = format!("{}{}", " ".repeat(60 * 1024), snapshot);
        std::fs::write(
            project.join(format!("{SESSION_A}.jsonl")),
            format!("{irrelevant}\n{{broken\n{snapshot}\n"),
        )
        .unwrap();
        let provider = ClaudeCodeProvider::new(ClaudeDirectory::from_path(tmp.path()).unwrap());
        let descriptor = provider.sessions().unwrap().remove(0);
        let compact = provider.file_change_projection(&descriptor).unwrap();
        assert_eq!(compact.changes.len(), 1);
        assert_eq!(compact.changes[0].path, "/work/late.rs");
        assert_eq!(compact.changes[0].record.ordinal, 2);
    }

    #[test]
    fn max_file_size_is_enforced() {
        let (_tmp, p) = fixture();
        let p = p.with_max_file_size(Some(16));
        let err = p.parse(&key(SESSION_A)).unwrap_err();
        assert!(err.to_string().contains("max_file_size"), "{err}");
    }

    #[test]
    fn lineage_keeps_dangling_edges_and_spawn_metadata_deterministically() {
        let (_tmp, p) = fixture();
        let edges = p.lineage().unwrap();

        // Continuation A -> B.
        assert!(edges.iter().any(|e| e.kind == LineageEdgeKind::Continuation
            && e.from.native_id == SESSION_A
            && e.to.native_id == SESSION_B));

        // Dangling continuation: C's parent file does not exist, the edge
        // survives anyway.
        assert!(
            edges.iter().any(
                |e| e.kind == LineageEdgeKind::Continuation && e.from.native_id == SESSION_GONE
            ),
            "dangling continuation edge lost: {edges:?}"
        );

        // Spawn edges carry sidecar metadata and parent-qualified targets.
        let spawns: Vec<_> = edges
            .iter()
            .filter(|e| matches!(e.kind, LineageEdgeKind::Spawn { .. }))
            .collect();
        assert_eq!(spawns.len(), 3);
        for s in &spawns {
            let LineageEdgeKind::Spawn {
                agent_type,
                description,
                ..
            } = &s.kind
            else {
                unreachable!()
            };
            assert_eq!(agent_type.as_deref(), Some("Explore"));
            assert_eq!(description.as_deref(), Some("scan"));
            assert!(s.to.namespace.0.starts_with("subagent:"));
        }
        let targets: std::collections::BTreeSet<_> = spawns.iter().map(|s| &s.to).collect();
        assert_eq!(targets.len(), 3, "spawn targets must not collide");

        // Deterministic output: sorted and deduplicated.
        let mut resorted = edges.clone();
        resorted.sort();
        resorted.dedup();
        assert_eq!(edges, resorted);
    }

    #[test]
    fn raw_jsonl_native_and_archive_are_byte_faithful() {
        let (_tmp, p) = fixture();
        let (_, session) = p.resolve(&key(SESSION_A)).unwrap();
        let native = std::fs::read(session.path()).unwrap();

        let mut raw = Vec::new();
        p.write_raw_jsonl(&key(SESSION_A), &mut raw).unwrap();
        assert_eq!(raw, native, "raw-jsonl must be byte-faithful");

        let mut nat = Vec::new();
        let artifact = p.artifact_for(&session).snapshot.id;
        p.write_native(&artifact, &mut nat).unwrap();
        assert_eq!(nat, native, "native must be byte-faithful");

        // Framed multipart archive: EVERY artifact's bytes are preserved,
        // including divergent duplicate copies.
        let mut bundle = Vec::new();
        p.write_archive(&key(SESSION_A), &mut bundle).unwrap();
        let newline = bundle.iter().position(|b| *b == b'\n').unwrap();
        let manifest: serde_json::Value = serde_json::from_slice(&bundle[..newline]).unwrap();
        assert_eq!(manifest["manifest"]["provider"], "claude-code");
        let artifacts = manifest["manifest"]["artifacts"].as_array().unwrap();
        assert_eq!(artifacts.len(), 2, "both copies listed in the manifest");
        let mut offset = newline + 1;
        let mut payloads = Vec::new();
        for a in artifacts {
            let len = a["bytes"].as_u64().unwrap() as usize;
            let body = &bundle[offset..offset + len];
            let on_disk = std::fs::read(a["locator"].as_str().unwrap()).unwrap();
            assert_eq!(body, &on_disk[..], "artifact bytes must round-trip");
            payloads.push(body.to_vec());
            offset += len;
        }
        assert_eq!(offset, bundle.len(), "no trailing bytes beyond the frames");
        assert_ne!(
            payloads[0], payloads[1],
            "fixture copies must actually diverge for this test to bite"
        );
    }

    #[test]
    fn write_native_rejects_traversal_and_unknown_artifacts() {
        let (tmp, p) = fixture();
        // A real file addressed via a traversal locator that would pass a
        // lexical starts_with check but resolves outside projects/.
        let secret = tmp.path().join("outside-secret.txt");
        std::fs::write(&secret, b"secret").unwrap();
        let traversal = ArtifactId {
            provider_instance: p.claude_dir.root().display().to_string(),
            locator: format!(
                "{}/projects/../outside-secret.txt",
                p.claude_dir.root().display()
            ),
        };
        let mut sink = Vec::new();
        assert!(
            matches!(
                p.write_native(&traversal, &mut sink),
                Err(ProviderError::NotFound(_))
            ),
            "traversal locator must not resolve"
        );
        assert!(
            sink.is_empty(),
            "nothing may be streamed for a forged locator"
        );

        let unknown = ArtifactId {
            provider_instance: "mem://elsewhere".into(),
            locator: "nope.jsonl".into(),
        };
        assert!(matches!(
            p.write_native(&unknown, &mut sink),
            Err(ProviderError::NotFound(_))
        ));
    }

    #[test]
    fn invalid_utf8_line_is_unparseable_not_fatal() {
        let (_tmp, p) = fixture();
        let parsed = p.parse(&key(SESSION_UTF8)).unwrap();
        assert!(parsed.validate_provenance().is_empty());
        assert_eq!(parsed.diagnostics.mapped, 2, "valid lines survive");
        assert_eq!(parsed.diagnostics.unparseable, 1, "corrupt line recorded");

        // Parity: the established lenient parser also yields the two valid
        // entries rather than failing the session.
        let (_, session) = p.resolve(&key(SESSION_UTF8)).unwrap();
        let baseline = session.parse().unwrap();
        assert_eq!(parsed.entries.len(), baseline.len());
    }

    #[test]
    fn unknown_keys_are_refused() {
        let (_tmp, p) = fixture();
        let foreign = LogicalSessionKey {
            provider: ProviderId::codex(),
            namespace: SessionNamespace::global(),
            native_id: SESSION_A.into(),
        };
        assert!(matches!(p.parse(&foreign), Err(ProviderError::NotFound(_))));
        let mut sink = Vec::new();
        assert!(matches!(
            p.write_raw_jsonl(&key("no-such-session"), &mut sink),
            Err(ProviderError::NotFound(_))
        ));
    }
}
