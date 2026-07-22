//! Registry-driven incremental construction for the provider search index.
//!
//! Inventory, project identity, and typed lineage come from the provider
//! registry. Unchanged source sessions are never parsed; changed partitions
//! stream through one bounded Tantivy transaction so explicit selections keep
//! generation-level atomicity without retaining the corpus in memory.

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::Serialize;

use super::provider::{
    project_parsed_session, IndexedSessionManifest, IndexedSkip, ProviderIndexBuildManifest,
    ProviderSearchIndex, PROVIDER_INDEX_SCHEMA_VERSION,
};
use crate::error::{Result, SnatchError};
use crate::provider::project::{history_units, SessionProjectContext};
use crate::provider::registry::{ProviderRegistry, ProviderSelection};
use crate::provider::{LineageEdgeKind, LogicalSessionKey, ProviderId, SessionDescriptor};

/// Inputs that make one build generation deterministic and testable.
pub struct ProviderIndexBuildOptions<'a> {
    /// Provider selection already resolved from the caller's flags.
    pub selection: &'a ProviderSelection,
    /// Optional unified-project substring. Filtered builds are upsert-only.
    pub project_filter: Option<&'a str>,
    /// Unique generation identifier.
    pub generation: String,
    /// Timestamp recorded in build/session manifests.
    pub built_at: DateTime<Utc>,
}

impl<'a> ProviderIndexBuildOptions<'a> {
    /// Create production options with a fresh generation identifier.
    #[must_use]
    pub fn new(selection: &'a ProviderSelection, project_filter: Option<&'a str>) -> Self {
        Self {
            selection,
            project_filter,
            generation: uuid::Uuid::new_v4().to_string(),
            built_at: Utc::now(),
        }
    }
}

/// Machine-readable result of one committed provider-index generation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ProviderIndexBuildReport {
    /// Committed generation id.
    pub generation: String,
    /// Source sessions in the selected project scope.
    pub sessions_scanned: usize,
    /// Sessions whose revision and metadata tokens were unchanged.
    pub sessions_unchanged: usize,
    /// Sessions parsed and replaced.
    pub sessions_replaced: usize,
    /// Stale source-session partitions removed after complete inventory.
    pub sessions_removed: usize,
    /// Provider/session failures retained in the build manifest.
    pub skipped: usize,
    /// Non-fatal project-context fallbacks retained in the build manifest.
    pub warnings: usize,
    /// Whether every selected provider had complete disappearance coverage.
    pub removal_coverage_complete: bool,
}

/// Result of a staged, recoverable provider-index directory replacement.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ProviderIndexRebuildReport {
    /// Build result for the activated replacement.
    pub build: ProviderIndexBuildReport,
    /// Whether an existing index directory was replaced.
    pub replaced_existing: bool,
    /// Old directory retained because cleanup failed or its type changed
    /// unexpectedly. The new index is active; callers should surface this.
    pub retained_backup: Option<PathBuf>,
}

#[derive(Clone, Copy)]
enum ActivationFault {
    None,
    #[cfg(test)]
    AfterBackup,
}

fn validate_rebuild_target(target: &Path) -> Result<&Path> {
    let parent = target.parent().ok_or_else(|| {
        SnatchError::IndexError(format!(
            "provider index rebuild target has no parent: {}",
            target.display()
        ))
    })?;
    if target.file_name().is_none() {
        return Err(SnatchError::IndexError(format!(
            "provider index rebuild target has no final component: {}",
            target.display()
        )));
    }
    std::fs::create_dir_all(parent).map_err(|error| {
        SnatchError::io(
            format!(
                "failed to create provider index parent: {}",
                parent.display()
            ),
            error,
        )
    })?;
    match std::fs::symlink_metadata(target) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            return Err(SnatchError::IndexError(format!(
                "refusing provider index rebuild target symlink: {}",
                target.display()
            )));
        }
        Ok(metadata) if !metadata.is_dir() => {
            return Err(SnatchError::IndexError(format!(
                "provider index rebuild target is not a directory: {}",
                target.display()
            )));
        }
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(SnatchError::io(
                format!(
                    "failed to inspect provider index rebuild target: {}",
                    target.display()
                ),
                error,
            ));
        }
    }
    Ok(parent)
}

fn sibling_backup_path(target: &Path) -> Result<PathBuf> {
    let parent = target.parent().ok_or_else(|| {
        SnatchError::IndexError(format!(
            "rebuild target has no parent: {}",
            target.display()
        ))
    })?;
    let file_name = target.file_name().ok_or_else(|| {
        SnatchError::IndexError(format!(
            "rebuild target has no final component: {}",
            target.display()
        ))
    })?;
    let mut name = OsString::from(".");
    name.push(file_name);
    name.push(".backup-");
    name.push(uuid::Uuid::new_v4().to_string());
    Ok(parent.join(name))
}

fn activate_staged_directory(
    target: &Path,
    staged: &Path,
    generation: &str,
    fault: ActivationFault,
) -> Result<(bool, Option<PathBuf>)> {
    let _lock = super::provider::ProviderIndexRebuildLock::acquire(target, generation)?;
    validate_rebuild_target(target)?;
    let replaced_existing = target.exists();
    if !replaced_existing {
        std::fs::rename(staged, target).map_err(|error| {
            SnatchError::io(
                format!(
                    "failed to activate staged provider index {} at {}",
                    staged.display(),
                    target.display()
                ),
                error,
            )
        })?;
        return Ok((false, None));
    }

    let backup = sibling_backup_path(target)?;
    if backup.exists() {
        return Err(SnatchError::IndexError(format!(
            "provider index rebuild backup already exists: {}",
            backup.display()
        )));
    }
    std::fs::rename(target, &backup).map_err(|error| {
        SnatchError::io(
            format!(
                "failed to move current provider index to backup {}",
                backup.display()
            ),
            error,
        )
    })?;

    #[cfg(test)]
    if matches!(fault, ActivationFault::AfterBackup) {
        std::fs::rename(&backup, target).map_err(|restore| {
            SnatchError::io(
                format!(
                    "injected activation failure; restoring {} also failed",
                    target.display()
                ),
                restore,
            )
        })?;
        return Err(SnatchError::IndexError(
            "injected provider-index activation failure".to_string(),
        ));
    }
    #[cfg(not(test))]
    let _ = fault;

    if let Err(activate) = std::fs::rename(staged, target) {
        return match std::fs::rename(&backup, target) {
            Ok(()) => Err(SnatchError::io(
                format!(
                    "failed to activate staged provider index {}; previous index restored",
                    staged.display()
                ),
                activate,
            )),
            Err(restore) => Err(SnatchError::IndexError(format!(
                "failed to activate staged provider index ({activate}); failed to restore previous index ({restore}); previous index remains at {}",
                backup.display()
            ))),
        };
    }

    let retained_backup = match std::fs::symlink_metadata(&backup) {
        Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {
            std::fs::remove_dir_all(&backup)
                .err()
                .map(|_| backup.clone())
        }
        Ok(_) | Err(_) => Some(backup),
    };
    Ok((true, retained_backup))
}

fn selected_provider_ids(
    registry: &ProviderRegistry,
    selection: &ProviderSelection,
) -> Result<Vec<String>> {
    let mut ids = match selection {
        ProviderSelection::All => registry
            .entries()
            .iter()
            .map(|entry| entry.id.to_string())
            .collect::<Vec<_>>(),
        ProviderSelection::Explicit(ids) => ids.iter().map(ToString::to_string).collect(),
    };
    ids.sort();
    ids.dedup();
    if ids.is_empty() {
        return Err(SnatchError::IndexError(
            "provider index build requires an explicit non-empty provider selection".to_string(),
        ));
    }
    Ok(ids)
}

fn index_metadata_fingerprint(
    descriptor: &SessionDescriptor,
    context: &SessionProjectContext,
    logical_root: &LogicalSessionKey,
    project_key: &str,
    project_path: &str,
    spawned: bool,
) -> Result<String> {
    // The full canonical serialization is retained instead of a short hash:
    // collision-induced stale hits are less acceptable than a few hundred
    // local-cache bytes, and the index already stores these project fields.
    serde_json::to_string(&(
        PROVIDER_INDEX_SCHEMA_VERSION,
        descriptor.key.to_string(),
        logical_root.to_string(),
        project_key,
        project_path,
        spawned,
        context.cwd.as_deref(),
        context.git_root.as_deref(),
        context.git_repository_url.as_deref(),
        context.git_branch.as_deref(),
        context.started_at,
        context.ended_at,
        context.native_tail_unresolved,
        context.modified_at,
        context.artifact_bytes,
    ))
    .map_err(Into::into)
}

fn safe_provider_skip(provider: &ProviderId) -> IndexedSkip {
    IndexedSkip {
        provider: Some(provider.to_string()),
        session_key: None,
        reason: "provider inventory or lineage unavailable (details withheld)".to_string(),
    }
}

fn safe_session_skip(key: &LogicalSessionKey, stage: &str) -> IndexedSkip {
    IndexedSkip {
        provider: Some(key.provider.to_string()),
        session_key: Some(key.to_string()),
        reason: format!("session {stage} failed (details withheld)"),
    }
}

fn safe_context_warning(key: &LogicalSessionKey) -> IndexedSkip {
    IndexedSkip {
        provider: Some(key.provider.to_string()),
        session_key: Some(key.to_string()),
        reason: "project context unavailable; session-identity fallback used".to_string(),
    }
}

fn retain_failure_or_abort(
    atomic: bool,
    error: impl Into<SnatchError>,
    key: &LogicalSessionKey,
    stage: &str,
    complete: &mut BTreeSet<String>,
    skipped: &mut Vec<IndexedSkip>,
) -> Result<()> {
    if atomic {
        return Err(error.into());
    }
    complete.remove(&key.provider.to_string());
    skipped.push(safe_session_skip(key, stage));
    Ok(())
}

/// Incrementally update a provider index from one registry selection.
///
/// Explicit selections roll the entire writer transaction back on any
/// inventory, revision, or parse failure. `all` preserves failed source
/// partitions, commits successful changed sessions, and records bounded safe
/// reasons. Disappearance pruning occurs only for providers with a complete
/// unfiltered inventory and no session failures.
pub fn update_provider_index(
    index: &ProviderSearchIndex,
    registry: &ProviderRegistry,
    options: &ProviderIndexBuildOptions<'_>,
) -> Result<ProviderIndexBuildReport> {
    let selected_providers = selected_provider_ids(registry, options.selection)?;
    let atomic = matches!(options.selection, ProviderSelection::Explicit(_));
    let collected = registry.collect_project_union(options.selection)?;
    let skipped_providers: BTreeSet<String> = collected
        .skipped
        .iter()
        .map(|(provider, _)| provider.to_string())
        .collect();
    let mut complete_providers: BTreeSet<String> = selected_providers
        .iter()
        .filter(|provider| !skipped_providers.contains(*provider))
        .cloned()
        .collect();
    let mut skipped: Vec<IndexedSkip> = collected
        .skipped
        .iter()
        .map(|(provider, _)| safe_provider_skip(provider))
        .collect();
    let context_warning_keys: BTreeSet<LogicalSessionKey> = collected
        .context_warnings
        .iter()
        .map(|warning| warning.key.clone())
        .collect();
    let mut warnings = Vec::new();
    let existing: BTreeMap<String, IndexedSessionManifest> = index
        .session_manifests()?
        .into_iter()
        .map(|manifest| (manifest.session_key.clone(), manifest))
        .collect();
    let spawned: BTreeSet<LogicalSessionKey> = collected
        .lineage
        .iter()
        .filter(|edge| matches!(edge.kind, LineageEdgeKind::Spawn { .. }))
        .map(|edge| edge.to.clone())
        .collect();
    let mut current_keys = BTreeSet::new();
    let mut transaction = index.begin_generation(&options.generation, &selected_providers)?;
    let mut sessions_scanned = 0_usize;
    let mut sessions_unchanged = 0_usize;
    let mut sessions_replaced = 0_usize;

    for project in &collected.projects {
        if options
            .project_filter
            .is_some_and(|filter| !project.matches(filter))
        {
            continue;
        }
        let project_key = project.identity.to_string();
        let project_path = project
            .display_path
            .clone()
            .unwrap_or_else(|| project_key.clone());
        let mut roots = BTreeMap::new();
        for unit in history_units(project, &collected.lineage) {
            for member in unit.members {
                roots.insert(member, unit.root.clone());
            }
        }

        for session in &project.sessions {
            sessions_scanned = sessions_scanned.saturating_add(1);
            let key = &session.descriptor.key;
            current_keys.insert(key.to_string());
            if context_warning_keys.contains(key) {
                warnings.push(safe_context_warning(key));
            }
            let is_spawned = spawned.contains(key);
            let logical_root = roots.get(key).unwrap_or(key);
            let metadata_fingerprint = index_metadata_fingerprint(
                &session.descriptor,
                &session.context,
                logical_root,
                &project_key,
                &project_path,
                is_spawned,
            )?;
            let provider = match registry.get(&key.provider) {
                Ok(provider) => provider,
                Err(error) => {
                    retain_failure_or_abort(
                        atomic,
                        error,
                        key,
                        "provider resolution",
                        &mut complete_providers,
                        &mut skipped,
                    )?;
                    continue;
                }
            };
            let revision_token =
                match provider.parse_cache_token_for_descriptor(&session.descriptor) {
                    Ok(token) => token,
                    Err(error) => {
                        retain_failure_or_abort(
                            atomic,
                            error,
                            key,
                            "revision check",
                            &mut complete_providers,
                            &mut skipped,
                        )?;
                        continue;
                    }
                };
            if existing.get(&key.to_string()).is_some_and(|manifest| {
                manifest.revision_token == revision_token
                    && manifest.metadata_fingerprint == metadata_fingerprint
            }) {
                sessions_unchanged = sessions_unchanged.saturating_add(1);
                continue;
            }

            let parsed = match provider.parse_discovered(&session.descriptor) {
                Ok(parsed) => parsed,
                Err(error) => {
                    retain_failure_or_abort(
                        atomic,
                        error,
                        key,
                        "parse",
                        &mut complete_providers,
                        &mut skipped,
                    )?;
                    continue;
                }
            };
            let batch = match project_parsed_session(
                &parsed,
                logical_root,
                &project_key,
                &project_path,
                is_spawned,
                revision_token,
                metadata_fingerprint,
                options.generation.clone(),
                options.built_at,
                session.context.started_at,
                session.context.ended_at,
                session.context.modified_at,
            ) {
                Ok(batch) => batch,
                Err(error) => {
                    retain_failure_or_abort(
                        atomic,
                        error,
                        key,
                        "projection",
                        &mut complete_providers,
                        &mut skipped,
                    )?;
                    continue;
                }
            };
            // Writer failures are generation-fatal even under `all`: unlike
            // provider parse errors, Tantivy has no per-session savepoint.
            transaction.replace(batch)?;
            sessions_replaced = sessions_replaced.saturating_add(1);
        }
    }

    let mut sessions_removed = 0_usize;
    if options.project_filter.is_none() {
        for manifest in existing.values() {
            if !complete_providers.contains(&manifest.provider)
                || current_keys.contains(&manifest.session_key)
            {
                continue;
            }
            let key: LogicalSessionKey =
                manifest.session_key.parse().map_err(|error: String| {
                    SnatchError::IndexError(format!(
                        "indexed session key '{}' is invalid: {error}",
                        manifest.session_key
                    ))
                })?;
            transaction.remove(&key)?;
            sessions_removed = sessions_removed.saturating_add(1);
        }
    }

    skipped.sort();
    skipped.dedup();
    warnings.sort();
    warnings.dedup();
    let complete_providers: Vec<String> = complete_providers.into_iter().collect();
    let removal_coverage_complete = options.project_filter.is_none()
        && skipped.is_empty()
        && complete_providers == selected_providers;
    let build = ProviderIndexBuildManifest {
        schema_version: PROVIDER_INDEX_SCHEMA_VERSION,
        generation: options.generation.clone(),
        built_at: options.built_at,
        selected_providers,
        complete_providers,
        removal_coverage_complete,
        skipped,
        warnings,
    };
    let skipped_count = build.skipped.len();
    let warning_count = build.warnings.len();
    transaction.commit(&build)?;

    Ok(ProviderIndexBuildReport {
        generation: options.generation.clone(),
        sessions_scanned,
        sessions_unchanged,
        sessions_replaced,
        sessions_removed,
        skipped: skipped_count,
        warnings: warning_count,
        removal_coverage_complete,
    })
}

/// Build a complete sibling index, verify its generation, then activate it
/// with a cooperative-lock, backup-and-restore two-phase directory swap.
///
/// Portable Rust cannot atomically exchange two non-empty directories on all
/// supported platforms. This operation therefore makes the weaker guarantee
/// explicit: the old directory is untouched until the replacement is fully
/// built; an activation failure restores it; an unexpected cleanup failure
/// retains the old directory and reports its exact path.
pub fn rebuild_provider_index(
    target: impl AsRef<Path>,
    registry: &ProviderRegistry,
    options: &ProviderIndexBuildOptions<'_>,
) -> Result<ProviderIndexRebuildReport> {
    let target = target.as_ref();
    let parent = validate_rebuild_target(target)?;
    let staging = tempfile::Builder::new()
        .prefix(".snatch-provider-index-staging-")
        .tempdir_in(parent)
        .map_err(|error| {
            SnatchError::io(
                format!(
                    "failed to create staged provider index beside {}",
                    target.display()
                ),
                error,
            )
        })?;
    let staged_index = ProviderSearchIndex::open(staging.path())?;
    let build = update_provider_index(&staged_index, registry, options)?;
    if !build.removal_coverage_complete {
        return Err(SnatchError::IndexError(
            "staged provider-index replacement requires a complete, unfiltered, failure-free build; previous index left untouched"
                .to_string(),
        ));
    }
    let staged_manifest = staged_index.build_manifest()?.ok_or_else(|| {
        SnatchError::IndexError("staged provider index has no build manifest".to_string())
    })?;
    if staged_manifest.generation != options.generation {
        return Err(SnatchError::IndexError(format!(
            "staged provider index generation {} != expected {}",
            staged_manifest.generation, options.generation
        )));
    }
    drop(staged_index);
    let staged_path = staging.keep();
    let activated = activate_staged_directory(
        target,
        &staged_path,
        &options.generation,
        ActivationFault::None,
    );
    let (replaced_existing, retained_backup) = activated.map_err(|error| {
        SnatchError::IndexError(format!(
            "{error}; staged replacement remains at {}",
            staged_path.display()
        ))
    })?;
    Ok(ProviderIndexRebuildReport {
        build,
        replaced_existing,
        retained_backup,
    })
}

#[cfg(test)]
mod tests {
    use std::io::Write;
    use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
    use std::sync::Arc;

    use tempfile::tempdir;

    use super::*;
    use crate::provider::fake::{colliding_key, FakeProvider};
    use crate::provider::registry::RegisteredProvider;
    use crate::provider::{
        ArtifactId, LineageEdge, ParsedSession, ProviderCapabilities, ProviderError,
        SessionProjectContexts, SourceProvider,
    };

    #[derive(Default)]
    struct ProbeState {
        parses: AtomicUsize,
        tokens: AtomicUsize,
        revision: AtomicU64,
        context_revision: AtomicU64,
        omit_collision: AtomicBool,
        fail_collision: AtomicBool,
        spawn_collision: AtomicBool,
    }

    struct ProbeProvider {
        state: Arc<ProbeState>,
    }

    impl ProbeProvider {
        fn descriptors(&self) -> Vec<SessionDescriptor> {
            let mut sessions = FakeProvider.sessions().unwrap();
            if self.state.omit_collision.load(Ordering::SeqCst) {
                sessions.retain(|descriptor| descriptor.key != colliding_key());
            }
            sessions
        }
    }

    impl SourceProvider for ProbeProvider {
        fn id(&self) -> ProviderId {
            FakeProvider.id()
        }

        fn capabilities(&self) -> ProviderCapabilities {
            FakeProvider.capabilities()
        }

        fn sessions(&self) -> std::result::Result<Vec<SessionDescriptor>, ProviderError> {
            Ok(self.descriptors())
        }

        fn sessions_with_project_context(
            &self,
        ) -> std::result::Result<SessionProjectContexts, ProviderError> {
            let context_revision = self.state.context_revision.load(Ordering::SeqCst);
            Ok(self
                .descriptors()
                .into_iter()
                .map(|descriptor| {
                    (
                        descriptor,
                        Ok(SessionProjectContext {
                            cwd: Some(format!("/work/fake-{context_revision}")),
                            modified_at: Some("2026-07-22T00:00:00Z".parse().unwrap()),
                            artifact_bytes: 100,
                            ..Default::default()
                        }),
                    )
                })
                .collect())
        }

        fn lineage(&self) -> std::result::Result<Vec<LineageEdge>, ProviderError> {
            let spawn_collision = self.state.spawn_collision.load(Ordering::SeqCst);
            Ok(FakeProvider
                .lineage()?
                .into_iter()
                .map(|mut edge| {
                    if spawn_collision && edge.to == colliding_key() {
                        edge.kind = LineageEdgeKind::Spawn {
                            tool_use_id: Some("probe-spawn".to_string()),
                            agent_type: None,
                            description: None,
                        };
                    }
                    edge
                })
                .filter(|edge| {
                    !self.state.omit_collision.load(Ordering::SeqCst)
                        || (edge.from != colliding_key() && edge.to != colliding_key())
                })
                .collect())
        }

        fn parse(
            &self,
            key: &LogicalSessionKey,
        ) -> std::result::Result<ParsedSession, ProviderError> {
            let descriptor = self
                .descriptors()
                .into_iter()
                .find(|descriptor| descriptor.key == *key)
                .ok_or_else(|| ProviderError::NotFound(key.to_string()))?;
            self.parse_discovered(&descriptor)
        }

        fn parse_discovered(
            &self,
            descriptor: &SessionDescriptor,
        ) -> std::result::Result<ParsedSession, ProviderError> {
            self.state.parses.fetch_add(1, Ordering::SeqCst);
            if self.state.fail_collision.load(Ordering::SeqCst) && descriptor.key == colliding_key()
            {
                return Err(ProviderError::Other("deliberate parse failure".to_string()));
            }
            FakeProvider.parse(&descriptor.key)
        }

        fn parse_cache_token(
            &self,
            key: &LogicalSessionKey,
        ) -> std::result::Result<String, ProviderError> {
            let descriptor = self
                .descriptors()
                .into_iter()
                .find(|descriptor| descriptor.key == *key)
                .ok_or_else(|| ProviderError::NotFound(key.to_string()))?;
            self.parse_cache_token_for_descriptor(&descriptor)
        }

        fn parse_cache_token_for_descriptor(
            &self,
            descriptor: &SessionDescriptor,
        ) -> std::result::Result<String, ProviderError> {
            self.state.tokens.fetch_add(1, Ordering::SeqCst);
            Ok(format!(
                "probe-v{}:{}",
                self.state.revision.load(Ordering::SeqCst),
                crate::provider::descriptor_state_token(descriptor)
            ))
        }

        fn write_archive(
            &self,
            key: &LogicalSessionKey,
            out: &mut dyn Write,
        ) -> std::result::Result<(), ProviderError> {
            FakeProvider.write_archive(key, out)
        }

        fn write_native(
            &self,
            artifact: &ArtifactId,
            out: &mut dyn Write,
        ) -> std::result::Result<(), ProviderError> {
            FakeProvider.write_native(artifact, out)
        }

        fn write_raw_jsonl(
            &self,
            key: &LogicalSessionKey,
            out: &mut dyn Write,
        ) -> std::result::Result<(), ProviderError> {
            FakeProvider.write_raw_jsonl(key, out)
        }
    }

    fn registry(state: Arc<ProbeState>) -> ProviderRegistry {
        let mut registry = ProviderRegistry::new();
        registry
            .register(RegisteredProvider {
                id: ProviderId("fake".to_string()),
                root: None,
                provider: Ok(Box::new(ProbeProvider { state })),
            })
            .unwrap();
        registry
    }

    fn options<'a>(
        selection: &'a ProviderSelection,
        generation: &str,
    ) -> ProviderIndexBuildOptions<'a> {
        ProviderIndexBuildOptions {
            selection,
            project_filter: None,
            generation: generation.to_string(),
            built_at: "2026-07-22T00:00:00Z".parse().unwrap(),
        }
    }

    #[test]
    fn unchanged_sessions_are_not_reparsed_and_revision_changes_replace_once() {
        let dir = tempdir().unwrap();
        let index = ProviderSearchIndex::open(dir.path().join("index")).unwrap();
        let state = Arc::new(ProbeState::default());
        let registry = registry(Arc::clone(&state));
        let selection = ProviderSelection::Explicit(vec![ProviderId("fake".to_string())]);

        let first =
            update_provider_index(&index, &registry, &options(&selection, "generation-1")).unwrap();
        assert_eq!(first.sessions_replaced, 2);
        assert_eq!(state.parses.load(Ordering::SeqCst), 2);

        let second =
            update_provider_index(&index, &registry, &options(&selection, "generation-2")).unwrap();
        assert_eq!(second.sessions_unchanged, 2);
        assert_eq!(second.sessions_replaced, 0);
        assert_eq!(state.parses.load(Ordering::SeqCst), 2);

        state.context_revision.store(1, Ordering::SeqCst);
        let third =
            update_provider_index(&index, &registry, &options(&selection, "generation-3")).unwrap();
        assert_eq!(third.sessions_replaced, 2);
        assert_eq!(state.parses.load(Ordering::SeqCst), 4);

        state.spawn_collision.store(true, Ordering::SeqCst);
        let fourth =
            update_provider_index(&index, &registry, &options(&selection, "generation-4")).unwrap();
        assert_eq!(fourth.sessions_replaced, 1);
        assert_eq!(state.parses.load(Ordering::SeqCst), 5);

        state.revision.store(1, Ordering::SeqCst);
        let fifth =
            update_provider_index(&index, &registry, &options(&selection, "generation-5")).unwrap();
        assert_eq!(fifth.sessions_replaced, 2);
        assert_eq!(state.parses.load(Ordering::SeqCst), 7);
        assert_eq!(index.session_manifests().unwrap().len(), 2);
    }

    #[test]
    fn complete_inventory_prunes_disappeared_sessions_but_filtered_build_does_not() {
        let dir = tempdir().unwrap();
        let index = ProviderSearchIndex::open(dir.path().join("index")).unwrap();
        let state = Arc::new(ProbeState::default());
        let registry = registry(Arc::clone(&state));
        let selection = ProviderSelection::Explicit(vec![ProviderId("fake".to_string())]);
        update_provider_index(&index, &registry, &options(&selection, "generation-1")).unwrap();

        state.omit_collision.store(true, Ordering::SeqCst);
        let mut filtered = options(&selection, "generation-2");
        filtered.project_filter = Some("/work/fake");
        let report = update_provider_index(&index, &registry, &filtered).unwrap();
        assert_eq!(report.sessions_removed, 0);
        assert!(!report.removal_coverage_complete);
        assert_eq!(index.session_manifests().unwrap().len(), 2);

        let report =
            update_provider_index(&index, &registry, &options(&selection, "generation-3")).unwrap();
        assert_eq!(report.sessions_removed, 1);
        assert!(report.removal_coverage_complete);
        assert_eq!(index.session_manifests().unwrap().len(), 1);
    }

    #[test]
    fn all_preserves_failed_partition_while_explicit_failure_rolls_back() {
        let dir = tempdir().unwrap();
        let index = ProviderSearchIndex::open(dir.path().join("index")).unwrap();
        let state = Arc::new(ProbeState::default());
        let registry = registry(Arc::clone(&state));
        let all = ProviderSelection::All;
        update_provider_index(&index, &registry, &options(&all, "generation-1")).unwrap();

        state.revision.store(1, Ordering::SeqCst);
        state.fail_collision.store(true, Ordering::SeqCst);
        let partial =
            update_provider_index(&index, &registry, &options(&all, "generation-2")).unwrap();
        assert_eq!(partial.sessions_replaced, 1);
        assert_eq!(partial.skipped, 1);
        assert!(!partial.removal_coverage_complete);
        let manifests = index.session_manifests().unwrap();
        assert_eq!(manifests.len(), 2);
        let failed = manifests
            .iter()
            .find(|manifest| manifest.session_key == colliding_key().to_string())
            .unwrap();
        assert_eq!(failed.generation, "generation-1");

        let before_manifests = manifests;
        let before_entries = index.entries().unwrap();
        let before_build = index.build_manifest().unwrap();
        state.revision.store(2, Ordering::SeqCst);
        let explicit = ProviderSelection::Explicit(vec![ProviderId("fake".to_string())]);
        assert!(
            update_provider_index(&index, &registry, &options(&explicit, "generation-3")).is_err()
        );
        assert_eq!(index.session_manifests().unwrap(), before_manifests);
        assert_eq!(index.entries().unwrap(), before_entries);
        assert_eq!(index.build_manifest().unwrap(), before_build);
    }

    #[test]
    fn staged_rebuild_replaces_legacy_only_after_a_complete_build() {
        let dir = tempdir().unwrap();
        let target = dir.path().join("index");
        let legacy = super::super::SearchIndex::open(&target).unwrap();
        drop(legacy);
        let legacy_schema = std::fs::read(target.join("meta.json")).unwrap();
        let state = Arc::new(ProbeState::default());
        let registry = registry(state);
        let selection = ProviderSelection::Explicit(vec![ProviderId("fake".to_string())]);

        let report = rebuild_provider_index(
            &target,
            &registry,
            &options(&selection, "generation-rebuild"),
        )
        .unwrap();
        assert!(report.replaced_existing);
        assert!(report.retained_backup.is_none());
        assert_ne!(
            std::fs::read(target.join("meta.json")).unwrap(),
            legacy_schema
        );
        let replacement = ProviderSearchIndex::open(&target).unwrap();
        assert_eq!(replacement.session_manifests().unwrap().len(), 2);
        assert_eq!(
            replacement.build_manifest().unwrap().unwrap().generation,
            "generation-rebuild"
        );
    }

    #[test]
    fn failed_staged_build_never_touches_the_legacy_target() {
        let dir = tempdir().unwrap();
        let target = dir.path().join("index");
        let legacy = super::super::SearchIndex::open(&target).unwrap();
        drop(legacy);
        let before = std::fs::read(target.join("meta.json")).unwrap();
        let state = Arc::new(ProbeState::default());
        state.fail_collision.store(true, Ordering::SeqCst);
        let registry = registry(state);
        let selection = ProviderSelection::Explicit(vec![ProviderId("fake".to_string())]);

        assert!(rebuild_provider_index(
            &target,
            &registry,
            &options(&selection, "generation-fails")
        )
        .is_err());
        assert_eq!(std::fs::read(target.join("meta.json")).unwrap(), before);
        assert!(super::super::SearchIndex::open(&target).is_ok());
    }

    #[test]
    fn partial_all_rebuild_cannot_replace_a_complete_existing_snapshot() {
        let dir = tempdir().unwrap();
        let target = dir.path().join("index");
        let legacy = super::super::SearchIndex::open(&target).unwrap();
        drop(legacy);
        let before = std::fs::read(target.join("meta.json")).unwrap();
        let state = Arc::new(ProbeState::default());
        state.fail_collision.store(true, Ordering::SeqCst);
        let registry = registry(state);
        let all = ProviderSelection::All;

        let error =
            rebuild_provider_index(&target, &registry, &options(&all, "generation-partial"))
                .unwrap_err()
                .to_string();
        assert!(error.contains("complete, unfiltered, failure-free"));
        assert_eq!(std::fs::read(target.join("meta.json")).unwrap(), before);
        assert!(super::super::SearchIndex::open(&target).is_ok());
    }

    #[test]
    fn activation_failure_restores_the_previous_directory() {
        let dir = tempdir().unwrap();
        let target = dir.path().join("index");
        let staged = dir.path().join("staged");
        std::fs::create_dir(&target).unwrap();
        std::fs::create_dir(&staged).unwrap();
        std::fs::write(target.join("marker"), b"old").unwrap();
        std::fs::write(staged.join("marker"), b"new").unwrap();

        let error = activate_staged_directory(
            &target,
            &staged,
            "generation-restore",
            ActivationFault::AfterBackup,
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("injected"));
        assert_eq!(std::fs::read(target.join("marker")).unwrap(), b"old");
        assert_eq!(std::fs::read(staged.join("marker")).unwrap(), b"new");
        assert!(!std::fs::read_dir(dir.path()).unwrap().any(|entry| {
            entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .contains(".index.backup-")
        }));
        assert!(!super::super::provider::rebuild_lock_path(&target)
            .unwrap()
            .exists());
    }

    #[test]
    fn cooperative_rebuild_lock_refuses_legacy_and_provider_opens() {
        let dir = tempdir().unwrap();
        let target = dir.path().join("index");
        let lock =
            super::super::provider::ProviderIndexRebuildLock::acquire(&target, "generation-lock")
                .unwrap();
        let provider_error = ProviderSearchIndex::open(&target)
            .err()
            .unwrap()
            .to_string();
        let legacy_error = super::super::SearchIndex::open(&target)
            .err()
            .unwrap()
            .to_string();
        assert!(provider_error.contains("rebuild is in progress"));
        assert!(legacy_error.contains("rebuild is in progress"));
        drop(lock);
        assert!(ProviderSearchIndex::open(&target).is_ok());
    }

    #[cfg(unix)]
    #[test]
    fn broken_symlink_lock_still_blocks_index_creation() {
        use std::os::unix::fs::symlink;

        let dir = tempdir().unwrap();
        let target = dir.path().join("index");
        let lock = super::super::provider::rebuild_lock_path(&target).unwrap();
        symlink(dir.path().join("missing-lock-target"), &lock).unwrap();
        let error = ProviderSearchIndex::open(&target)
            .err()
            .unwrap()
            .to_string();
        assert!(error.contains("rebuild is in progress"));
        assert!(!target.exists());
    }

    #[cfg(unix)]
    #[test]
    fn staged_rebuild_refuses_a_target_symlink_without_following_it() {
        use std::os::unix::fs::symlink;

        let dir = tempdir().unwrap();
        let real = dir.path().join("real");
        std::fs::create_dir(&real).unwrap();
        std::fs::write(real.join("marker"), b"untouched").unwrap();
        let target = dir.path().join("index");
        symlink(&real, &target).unwrap();
        let state = Arc::new(ProbeState::default());
        let registry = registry(state);
        let selection = ProviderSelection::Explicit(vec![ProviderId("fake".to_string())]);

        assert!(rebuild_provider_index(
            &target,
            &registry,
            &options(&selection, "generation-symlink")
        )
        .is_err());
        assert_eq!(std::fs::read(real.join("marker")).unwrap(), b"untouched");
        assert!(!real.join("meta.json").exists());
    }
}
