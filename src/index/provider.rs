//! Versioned provider-neutral persistent search index.
//!
//! This module is intentionally separate from the legacy Claude-only
//! [legacy index](crate::index::SearchIndex). An incompatible existing schema
//! is never opened with guessed field ids or silently replaced; callers must
//! request an explicit rebuild.

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::io::Write as _;
use std::ops::Bound;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use chrono::{DateTime, Utc};
use parking_lot::{RwLock, RwLockWriteGuard};
use serde::{Deserialize, Serialize};
use tantivy::collector::DocSetCollector;
use tantivy::query::{AllQuery, BooleanQuery, EnableScoring, Query, RangeQuery, TermQuery};
use tantivy::schema::{
    Field, IndexRecordOption, Schema, Value, FAST, INDEXED, STORED, STRING, TEXT,
};
use tantivy::{doc, Index, IndexReader, IndexWriter, ReloadPolicy, TantivyDocument, Term};

use crate::analysis::search::{
    project_entry_for_search, EntrySearchProjection, SearchProjectionCoverage, SearchSegmentKind,
};
use crate::error::{Result, SnatchError};
use crate::model::LogEntry;
use crate::provider::{
    ActivityKind, LogicalSessionKey, ParsedSession, PromptAuthorship, PromptDelivery, ToolKind,
};

/// Current provider-index schema. A change requires an explicit rebuild.
pub const PROVIDER_INDEX_SCHEMA_VERSION: u64 = 3;

const WRITER_MEMORY_BYTES: usize = 50_000_000;

mod fields {
    pub const DOC_KIND: &str = "doc_kind";
    pub const SCHEMA_VERSION: &str = "schema_version";
    pub const PROVIDER: &str = "provider";
    pub const SESSION_KEY: &str = "session_key";
    pub const LOGICAL_ROOT: &str = "logical_root";
    pub const PROJECT_KEY: &str = "project_key";
    pub const PROJECT_PATH: &str = "project_path";
    pub const ENTRY_ID: &str = "entry_id";
    pub const ENTRY_ORDER: &str = "entry_order";
    pub const TIMESTAMP_MILLIS: &str = "timestamp_millis";
    pub const MESSAGE_TYPE: &str = "message_type";
    pub const MODEL: &str = "model";
    pub const GIT_BRANCH: &str = "git_branch";
    pub const ACTIVITY: &str = "activity";
    pub const SPAWNED: &str = "spawned";
    pub const TOOL_NAME: &str = "tool_name";
    pub const TEXT: &str = "text";
    pub const REASONING: &str = "reasoning";
    pub const TOOL_TEXT: &str = "tool_text";
    pub const PAYLOAD: &str = "payload";
}

const KIND_ENTRY: &str = "entry";
const KIND_SESSION: &str = "session-manifest";
const KIND_BUILD: &str = "build-manifest";

/// One provider/build failure retained in the index snapshot metadata.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct IndexedSkip {
    /// Provider id, when the failure was provider-wide.
    pub provider: Option<String>,
    /// Qualified session key, when one session failed.
    pub session_key: Option<String>,
    /// Stable, caller-sanitized failure summary.
    pub reason: String,
}

/// Metadata for one complete source-session partition in the index.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IndexedSessionManifest {
    /// Index schema that produced this row.
    pub schema_version: u64,
    /// Provider id.
    pub provider: String,
    /// Complete escaped qualified source-session key.
    pub session_key: String,
    /// Typed continuation root used for chain-aware queries.
    pub logical_root: String,
    /// Stable unified-project identity.
    pub project_key: String,
    /// Human-readable project path captured at build time.
    pub project_path: String,
    /// Whether typed lineage classifies this source session as spawned.
    pub spawned: bool,
    /// Provider parse-cache token covering artifacts and parse policy.
    pub revision_token: String,
    /// Deterministic fingerprint of project/lineage/index metadata.
    pub metadata_fingerprint: String,
    /// Build generation that last replaced this partition.
    pub generation: String,
    /// Time this partition was indexed.
    pub indexed_at: DateTime<Utc>,
    /// Cheap native source bounds, when available.
    pub source_started_at: Option<DateTime<Utc>>,
    /// Cheap native source bounds, when available.
    pub source_ended_at: Option<DateTime<Utc>>,
    /// Source modification time, when available.
    pub source_modified_at: Option<DateTime<Utc>>,
    /// Number of indexed entry documents in this partition.
    pub entry_count: usize,
    /// Number of ordered searchable segments in this partition.
    pub segment_count: usize,
    /// Search-projection omissions retained as machine-visible coverage.
    pub coverage: SearchProjectionCoverage,
}

/// Metadata for the most recently committed build generation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderIndexBuildManifest {
    /// Index schema that produced this generation.
    pub schema_version: u64,
    /// Unique generation id.
    pub generation: String,
    /// Commit time.
    pub built_at: DateTime<Utc>,
    /// Provider ids explicitly selected for the build.
    pub selected_providers: Vec<String>,
    /// Providers whose selected scope was successfully processed without a
    /// provider/session failure. Inventory completeness is represented
    /// separately by `removal_coverage_complete`.
    pub complete_providers: Vec<String>,
    /// Whether disappearance pruning was complete for every successful
    /// provider (`false` for project-filtered upsert-only builds).
    pub removal_coverage_complete: bool,
    /// Provider/session failures preserved under a partial `all` build.
    pub skipped: Vec<IndexedSkip>,
    /// Non-fatal coverage warnings retained for successfully indexed
    /// sessions (for example, project identity falling back to session id).
    #[serde(default)]
    pub warnings: Vec<IndexedSkip>,
}

/// Statistics for the committed provider-index snapshot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ProviderIndexStats {
    /// Current provider-index schema.
    pub schema_version: u64,
    /// Total live Tantivy documents, including manifests.
    pub document_count: u64,
    /// Number of indexed source-session partitions.
    pub session_count: usize,
    /// Number of normalized entry documents across those partitions.
    pub entry_count: usize,
    /// Index size on disk in bytes.
    pub size_bytes: u64,
    /// Latest committed build, when one exists.
    pub build: Option<ProviderIndexBuildManifest>,
}

fn validate_indexed_records(
    generation: &str,
    label: &str,
    records: &[IndexedSkip],
    selected: &BTreeSet<String>,
    complete: &BTreeSet<String>,
    reject_complete_provider: bool,
    violations: &mut Vec<String>,
) {
    if !records.windows(2).all(|pair| pair[0] < pair[1]) {
        violations.push(format!(
            "generation {generation} has duplicate or unsorted {label} records"
        ));
    }
    for record in records {
        if record.provider.is_none() && record.session_key.is_none() {
            violations.push(format!(
                "generation {generation} has a {label} without a provider or session"
            ));
        }
        if record.reason.is_empty() {
            violations.push(format!(
                "generation {generation} has a {label} with an empty reason"
            ));
        }
        let session_provider = record.session_key.as_deref().and_then(|session_key| {
            match session_key.parse::<LogicalSessionKey>() {
                Ok(key) if key.to_string() == session_key => Some(key.provider.to_string()),
                Ok(_) => {
                    violations.push(format!(
                        "generation {generation} {label} session '{session_key}' is not canonical"
                    ));
                    None
                }
                Err(error) => {
                    violations.push(format!(
                        "generation {generation} {label} session '{session_key}' is invalid: {error}"
                    ));
                    None
                }
            }
        });
        let provider = record.provider.as_ref().or(session_provider.as_ref());
        if let Some(provider) = provider {
            if !selected.contains(provider) {
                violations.push(format!(
                    "generation {generation} {label} belongs to unselected provider {provider}"
                ));
            }
            if reject_complete_provider && complete.contains(provider) {
                violations.push(format!(
                    "generation {generation} marks provider {provider} both complete and skipped"
                ));
            }
        }
        if let (Some(provider), Some(session_provider)) =
            (record.provider.as_ref(), session_provider.as_ref())
        {
            if provider != session_provider {
                violations.push(format!(
                    "generation {generation} {label} provider {provider} disagrees with session provider {session_provider}"
                ));
            }
        }
    }
}

impl ProviderIndexBuildManifest {
    /// Validate deterministic selection and coverage metadata independently
    /// of the staged session documents.
    #[must_use]
    pub fn validate(&self) -> Vec<String> {
        let mut violations = Vec::new();
        if self.schema_version != PROVIDER_INDEX_SCHEMA_VERSION {
            violations.push(format!(
                "build generation {} uses schema {}, expected {}",
                self.generation, self.schema_version, PROVIDER_INDEX_SCHEMA_VERSION
            ));
        }
        if self.generation.is_empty() {
            violations.push("provider index build generation cannot be empty".to_string());
        }

        let selected: BTreeSet<_> = self.selected_providers.iter().cloned().collect();
        if selected.is_empty()
            || selected.len() != self.selected_providers.len()
            || selected.contains("")
            || !self
                .selected_providers
                .windows(2)
                .all(|pair| pair[0] < pair[1])
        {
            violations.push(format!(
                "generation {} has empty, duplicate, or unsorted selected providers",
                self.generation
            ));
        }

        let complete: BTreeSet<_> = self.complete_providers.iter().cloned().collect();
        if complete.len() != self.complete_providers.len()
            || complete.contains("")
            || !complete.is_subset(&selected)
            || !self
                .complete_providers
                .windows(2)
                .all(|pair| pair[0] < pair[1])
        {
            violations.push(format!(
                "generation {} has empty, duplicate, unsorted, or unselected complete providers",
                self.generation
            ));
        }

        validate_indexed_records(
            &self.generation,
            "skipped",
            &self.skipped,
            &selected,
            &complete,
            true,
            &mut violations,
        );
        validate_indexed_records(
            &self.generation,
            "warning",
            &self.warnings,
            &selected,
            &complete,
            false,
            &mut violations,
        );

        if self.removal_coverage_complete && (selected != complete || !self.skipped.is_empty()) {
            violations.push(format!(
                "generation {} claims complete removal coverage without complete, failure-free providers",
                self.generation
            ));
        }
        violations
    }
}

/// Stored, provider-neutral projection of one normalized entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IndexedSearchEntry {
    /// Source session whose partition owns this entry.
    pub session_key: String,
    /// Typed continuation root.
    pub logical_root: String,
    /// Unified project identity and display path.
    pub project_key: String,
    /// Human-readable project path.
    pub project_path: String,
    /// Deterministic normalized entry identity.
    pub entry_id: String,
    /// Zero-based normalized entry order inside the source-session
    /// partition. Escaped entry-id strings are identity, not sortable
    /// ordinals.
    pub entry_order: usize,
    /// Normalized/native event time.
    pub timestamp: Option<DateTime<Utc>>,
    /// Normalized entry discriminator.
    pub message_type: String,
    /// Assistant model, when present.
    pub model: Option<String>,
    /// Recorded git branch, when present.
    pub git_branch: Option<String>,
    /// `new` or `inherited-history`.
    pub activity: String,
    /// Typed spawn classification.
    pub spawned: bool,
    /// Prompt authorship, when annotated.
    pub prompt_authorship: Option<String>,
    /// Prompt delivery, when annotated.
    pub prompt_delivery: Option<String>,
    /// Canonical tool kinds keyed by native call id.
    pub tool_kinds: BTreeMap<String, String>,
    /// Canonical processed-token count carried by this entry, when present.
    pub processed_tokens: Option<u64>,
    /// Ordered searchable segments plus coverage.
    pub projection: EntrySearchProjection,
}

/// One fully staged source-session replacement.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexedSessionBatch {
    /// Partition metadata.
    pub manifest: IndexedSessionManifest,
    /// Exact replacement entry documents.
    pub entries: Vec<IndexedSearchEntry>,
}

impl IndexedSessionBatch {
    /// Validate cross-document identity/cardinality before touching a writer.
    #[must_use]
    pub fn validate(&self) -> Vec<String> {
        let mut violations = Vec::new();
        if self.manifest.schema_version != PROVIDER_INDEX_SCHEMA_VERSION {
            violations.push(format!(
                "session {} uses schema {}, expected {}",
                self.manifest.session_key,
                self.manifest.schema_version,
                PROVIDER_INDEX_SCHEMA_VERSION
            ));
        }
        match self.manifest.session_key.parse::<LogicalSessionKey>() {
            Ok(key)
                if key.to_string() == self.manifest.session_key
                    && key.provider.to_string() == self.manifest.provider => {}
            Ok(key) if key.to_string() != self.manifest.session_key => violations.push(format!(
                "session key '{}' is not in canonical form",
                self.manifest.session_key
            )),
            Ok(key) => violations.push(format!(
                "session {} provider '{}' disagrees with key provider '{}'",
                self.manifest.session_key, self.manifest.provider, key.provider
            )),
            Err(error) => violations.push(format!(
                "session key '{}' is not canonical: {error}",
                self.manifest.session_key
            )),
        }
        match self.manifest.logical_root.parse::<LogicalSessionKey>() {
            Ok(root)
                if root.to_string() == self.manifest.logical_root
                    && root.provider.to_string() == self.manifest.provider => {}
            Ok(root) if root.to_string() != self.manifest.logical_root => violations.push(format!(
                "logical root '{}' is not in canonical form",
                self.manifest.logical_root
            )),
            Ok(root) => violations.push(format!(
                "logical root '{}' belongs to provider {}, expected {}",
                self.manifest.logical_root, root.provider, self.manifest.provider
            )),
            Err(error) => violations.push(format!(
                "logical root '{}' is not a canonical qualified key: {error}",
                self.manifest.logical_root
            )),
        }
        if self.manifest.revision_token.is_empty()
            || self.manifest.metadata_fingerprint.is_empty()
            || self.manifest.generation.is_empty()
        {
            violations.push(format!(
                "session {} has an empty revision, metadata, or generation token",
                self.manifest.session_key
            ));
        }
        if self.manifest.entry_count != self.entries.len() {
            violations.push(format!(
                "session {} manifest entry_count {} != {} staged entries",
                self.manifest.session_key,
                self.manifest.entry_count,
                self.entries.len()
            ));
        }
        let segment_count = self
            .entries
            .iter()
            .map(|entry| entry.projection.segments.len())
            .sum::<usize>();
        if self.manifest.segment_count != segment_count {
            violations.push(format!(
                "session {} manifest segment_count {} != {segment_count}",
                self.manifest.session_key, self.manifest.segment_count
            ));
        }
        let projected_coverage = self.entries.iter().fold(
            SearchProjectionCoverage::default(),
            |mut coverage, entry| {
                coverage.images_omitted = coverage
                    .images_omitted
                    .saturating_add(entry.projection.coverage.images_omitted);
                coverage.unknown_blocks_omitted = coverage
                    .unknown_blocks_omitted
                    .saturating_add(entry.projection.coverage.unknown_blocks_omitted);
                coverage.unknown_entries_omitted = coverage
                    .unknown_entries_omitted
                    .saturating_add(entry.projection.coverage.unknown_entries_omitted);
                coverage
            },
        );
        if self.manifest.coverage != projected_coverage {
            violations.push(format!(
                "session {} manifest coverage does not match staged entries",
                self.manifest.session_key
            ));
        }
        let mut entry_ids = BTreeSet::new();
        for (expected_order, entry) in self.entries.iter().enumerate() {
            if entry.session_key != self.manifest.session_key {
                violations.push(format!(
                    "entry {} belongs to {}, expected {}",
                    entry.entry_id, entry.session_key, self.manifest.session_key
                ));
            }
            if entry.logical_root != self.manifest.logical_root
                || entry.project_key != self.manifest.project_key
                || entry.project_path != self.manifest.project_path
                || entry.spawned != self.manifest.spawned
            {
                violations.push(format!(
                    "entry {} metadata disagrees with session manifest",
                    entry.entry_id
                ));
            }
            if !entry_ids.insert(&entry.entry_id) {
                violations.push(format!(
                    "session {} repeats entry id {}",
                    self.manifest.session_key, entry.entry_id
                ));
            }
            if entry.entry_order != expected_order {
                violations.push(format!(
                    "session {} entry {} has order {}, expected {}",
                    self.manifest.session_key, entry.entry_id, entry.entry_order, expected_order
                ));
            }
        }
        violations
    }
}

fn activity_label(activity: ActivityKind) -> String {
    match activity {
        ActivityKind::New => "new",
        ActivityKind::InheritedHistory => "inherited-history",
    }
    .to_string()
}

fn authorship_label(authorship: PromptAuthorship) -> String {
    match authorship {
        PromptAuthorship::Human => "human",
        PromptAuthorship::Harness => "harness",
        PromptAuthorship::Tool => "tool",
    }
    .to_string()
}

fn delivery_label(delivery: PromptDelivery) -> String {
    match delivery {
        PromptDelivery::TurnBoundary => "turn-boundary",
        PromptDelivery::MidTurn => "mid-turn",
    }
    .to_string()
}

fn tool_kind_label(kind: &ToolKind) -> String {
    match kind {
        ToolKind::Shell => "shell",
        ToolKind::FileRead => "file-read",
        ToolKind::FileWrite => "file-write",
        ToolKind::Search => "search",
        ToolKind::Web => "web",
        ToolKind::Subagent => "subagent",
        ToolKind::Mcp => "mcp",
        ToolKind::Orchestration => "orchestration",
        ToolKind::Other(value) => value,
    }
    .to_string()
}

fn entry_model(entry: &LogEntry) -> Option<String> {
    match entry {
        LogEntry::Assistant(message) if !message.message.model.is_empty() => {
            Some(message.message.model.clone())
        }
        _ => None,
    }
}

/// Build one staged search partition from an already validated provider
/// bundle. This performs no source I/O and retains deterministic entry ids.
#[allow(clippy::too_many_arguments)]
pub fn project_parsed_session(
    parsed: &ParsedSession,
    logical_root: &LogicalSessionKey,
    project_key: &str,
    project_path: &str,
    spawned: bool,
    revision_token: String,
    metadata_fingerprint: String,
    generation: String,
    indexed_at: DateTime<Utc>,
    source_started_at: Option<DateTime<Utc>>,
    source_ended_at: Option<DateTime<Utc>>,
    source_modified_at: Option<DateTime<Utc>>,
) -> Result<IndexedSessionBatch> {
    let provenance_violations = parsed.validate_provenance();
    if !provenance_violations.is_empty() {
        return Err(SnatchError::IndexError(format!(
            "session {} has invalid provenance: {}",
            parsed.descriptor.key,
            provenance_violations.join("; ")
        )));
    }
    let session_key = parsed.descriptor.key.to_string();
    let logical_root = logical_root.to_string();
    let mut coverage = SearchProjectionCoverage::default();
    let mut segment_count = 0_usize;
    let entries = parsed
        .entries
        .iter()
        .enumerate()
        .map(|(entry_order, identified)| {
            let projection = project_entry_for_search(&identified.entry);
            segment_count = segment_count.saturating_add(projection.segments.len());
            coverage.images_omitted = coverage
                .images_omitted
                .saturating_add(projection.coverage.images_omitted);
            coverage.unknown_blocks_omitted = coverage
                .unknown_blocks_omitted
                .saturating_add(projection.coverage.unknown_blocks_omitted);
            coverage.unknown_entries_omitted = coverage
                .unknown_entries_omitted
                .saturating_add(projection.coverage.unknown_entries_omitted);
            let semantics = parsed.semantics.get(&identified.id);
            let prompt = semantics.and_then(|value| value.prompt);
            let tool_kinds = semantics.map_or_else(BTreeMap::new, |value| {
                value
                    .tools
                    .iter()
                    .map(|(call_id, tool)| (call_id.clone(), tool_kind_label(&tool.kind)))
                    .collect()
            });
            IndexedSearchEntry {
                session_key: session_key.clone(),
                logical_root: logical_root.clone(),
                project_key: project_key.to_string(),
                project_path: project_path.to_string(),
                entry_id: identified.id.to_string(),
                entry_order,
                timestamp: identified.entry.timestamp(),
                message_type: identified.entry.message_type().to_string(),
                model: entry_model(&identified.entry),
                git_branch: identified.entry.git_branch().map(str::to_string),
                activity: activity_label(
                    semantics.map_or(ActivityKind::New, |value| value.activity),
                ),
                spawned,
                prompt_authorship: prompt.map(|value| authorship_label(value.authorship)),
                prompt_delivery: prompt.map(|value| delivery_label(value.delivery)),
                tool_kinds,
                processed_tokens: identified.entry.usage().map(|usage| usage.total_tokens()),
                projection,
            }
        })
        .collect::<Vec<_>>();
    let manifest = IndexedSessionManifest {
        schema_version: PROVIDER_INDEX_SCHEMA_VERSION,
        provider: parsed.descriptor.key.provider.to_string(),
        session_key,
        logical_root,
        project_key: project_key.to_string(),
        project_path: project_path.to_string(),
        spawned,
        revision_token,
        metadata_fingerprint,
        generation,
        indexed_at,
        source_started_at,
        source_ended_at,
        source_modified_at,
        entry_count: entries.len(),
        segment_count,
        coverage,
    };
    Ok(IndexedSessionBatch { manifest, entries })
}

#[derive(Clone, Copy)]
struct ProviderIndexFields {
    doc_kind: Field,
    schema_version: Field,
    provider: Field,
    session_key: Field,
    logical_root: Field,
    project_key: Field,
    project_path: Field,
    entry_id: Field,
    entry_order: Field,
    timestamp_millis: Field,
    message_type: Field,
    model: Field,
    git_branch: Field,
    activity: Field,
    spawned: Field,
    tool_name: Field,
    text: Field,
    reasoning: Field,
    tool_text: Field,
    payload: Field,
}

impl ProviderIndexFields {
    fn from_schema(schema: &Schema) -> Result<Self> {
        let field = |name: &str| {
            schema.get_field(name).map_err(|error| {
                SnatchError::IndexError(format!(
                    "provider index schema is missing field '{name}': {error}"
                ))
            })
        };
        Ok(Self {
            doc_kind: field(fields::DOC_KIND)?,
            schema_version: field(fields::SCHEMA_VERSION)?,
            provider: field(fields::PROVIDER)?,
            session_key: field(fields::SESSION_KEY)?,
            logical_root: field(fields::LOGICAL_ROOT)?,
            project_key: field(fields::PROJECT_KEY)?,
            project_path: field(fields::PROJECT_PATH)?,
            entry_id: field(fields::ENTRY_ID)?,
            entry_order: field(fields::ENTRY_ORDER)?,
            timestamp_millis: field(fields::TIMESTAMP_MILLIS)?,
            message_type: field(fields::MESSAGE_TYPE)?,
            model: field(fields::MODEL)?,
            git_branch: field(fields::GIT_BRANCH)?,
            activity: field(fields::ACTIVITY)?,
            spawned: field(fields::SPAWNED)?,
            tool_name: field(fields::TOOL_NAME)?,
            text: field(fields::TEXT)?,
            reasoning: field(fields::REASONING)?,
            tool_text: field(fields::TOOL_TEXT)?,
            payload: field(fields::PAYLOAD)?,
        })
    }
}

fn build_schema() -> Schema {
    let mut builder = Schema::builder();
    builder.add_text_field(fields::DOC_KIND, STRING | STORED);
    builder.add_u64_field(fields::SCHEMA_VERSION, INDEXED | STORED);
    builder.add_text_field(fields::PROVIDER, STRING | STORED);
    builder.add_text_field(fields::SESSION_KEY, STRING | STORED);
    builder.add_text_field(fields::LOGICAL_ROOT, STRING | STORED);
    builder.add_text_field(fields::PROJECT_KEY, STRING | STORED);
    builder.add_text_field(fields::PROJECT_PATH, STRING | STORED);
    builder.add_text_field(fields::ENTRY_ID, STRING | STORED);
    builder.add_u64_field(fields::ENTRY_ORDER, INDEXED | FAST | STORED);
    builder.add_i64_field(fields::TIMESTAMP_MILLIS, INDEXED | FAST | STORED);
    builder.add_text_field(fields::MESSAGE_TYPE, STRING | STORED);
    builder.add_text_field(fields::MODEL, STRING | STORED);
    builder.add_text_field(fields::GIT_BRANCH, STRING | STORED);
    builder.add_text_field(fields::ACTIVITY, STRING | STORED);
    builder.add_u64_field(fields::SPAWNED, INDEXED | FAST | STORED);
    builder.add_text_field(fields::TOOL_NAME, STRING | STORED);
    builder.add_text_field(fields::TEXT, TEXT);
    builder.add_text_field(fields::REASONING, TEXT);
    builder.add_text_field(fields::TOOL_TEXT, TEXT);
    builder.add_text_field(fields::PAYLOAD, STORED);
    builder.build()
}

pub(super) fn rebuild_lock_path(path: &Path) -> Result<PathBuf> {
    let parent = path.parent().ok_or_else(|| {
        SnatchError::IndexError(format!(
            "provider index target has no parent: {}",
            path.display()
        ))
    })?;
    let file_name = path.file_name().ok_or_else(|| {
        SnatchError::IndexError(format!(
            "provider index target has no final component: {}",
            path.display()
        ))
    })?;
    let mut lock_name = OsString::from(".");
    lock_name.push(file_name);
    lock_name.push(".snatch-rebuild.lock");
    Ok(parent.join(lock_name))
}

pub(super) fn refuse_rebuild_in_progress(path: &Path) -> Result<()> {
    let lock = rebuild_lock_path(path)?;
    match std::fs::symlink_metadata(&lock) {
        Ok(_) => {
            return Err(SnatchError::IndexError(format!(
                "search index rebuild is in progress for {}; retry after it completes",
                path.display()
            )));
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(SnatchError::io(
                format!("failed to inspect rebuild lock {}", lock.display()),
                error,
            ));
        }
    }
    Ok(())
}

pub(super) struct ProviderIndexRebuildLock {
    path: PathBuf,
}

impl ProviderIndexRebuildLock {
    pub(super) fn acquire(target: &Path, generation: &str) -> Result<Self> {
        let path = rebuild_lock_path(target)?;
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&path).map_err(|error| {
            SnatchError::io(
                format!(
                    "failed to acquire search-index rebuild lock {}",
                    path.display()
                ),
                error,
            )
        })?;
        if let Err(error) = writeln!(file, "generation={generation}") {
            drop(file);
            let _ = std::fs::remove_file(&path);
            return Err(SnatchError::io(
                format!("failed to write rebuild lock {}", path.display()),
                error,
            ));
        }
        Ok(Self { path })
    }
}

impl Drop for ProviderIndexRebuildLock {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

fn ensure_index_directory(path: &Path) -> Result<()> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            return Err(SnatchError::IndexError(format!(
                "refusing provider index directory symlink: {}",
                path.display()
            )));
        }
        Ok(metadata) if !metadata.is_dir() => {
            return Err(SnatchError::IndexError(format!(
                "provider index path is not a directory: {}",
                path.display()
            )));
        }
        Ok(_) => return Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(SnatchError::io(
                format!("failed to inspect provider index path: {}", path.display()),
                error,
            ));
        }
    }
    std::fs::create_dir_all(path).map_err(|error| {
        SnatchError::io(
            format!(
                "failed to create provider index directory: {}",
                path.display()
            ),
            error,
        )
    })?;
    secure_index_storage(path)?;
    Ok(())
}

fn secure_index_storage(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).map_err(
            |error| {
                SnatchError::io(
                    format!(
                        "failed to secure provider index directory: {}",
                        path.display()
                    ),
                    error,
                )
            },
        )?;
        for entry in walkdir::WalkDir::new(path).max_depth(1).follow_links(false) {
            let entry = entry.map_err(|error| {
                SnatchError::IndexError(format!(
                    "failed to inspect provider index permissions: {error}"
                ))
            })?;
            if !entry.file_type().is_file() {
                continue;
            }
            std::fs::set_permissions(entry.path(), std::fs::Permissions::from_mode(0o600))
                .map_err(|error| {
                    SnatchError::io(
                        format!(
                            "failed to secure provider index file: {}",
                            entry.path().display()
                        ),
                        error,
                    )
                })?;
        }
    }
    Ok(())
}

/// Versioned provider-neutral Tantivy index.
pub struct ProviderSearchIndex {
    index: Index,
    fields: ProviderIndexFields,
    reader: IndexReader,
    writer: Option<Arc<RwLock<IndexWriter>>>,
    path: PathBuf,
}

/// Exact indexed fields that may safely narrow the candidate scan without
/// changing regex/fuzzy semantics. Substring fields remain post-filters in
/// the query layer rather than being interpolated into Tantivy syntax.
#[derive(Debug, Clone, Default)]
pub(crate) struct IndexedEntryCandidateFilter {
    pub providers: Vec<String>,
    pub session_keys: Vec<String>,
    pub session_keys_match_none: bool,
    pub logical_roots: Vec<String>,
    pub project_keys: Vec<String>,
    pub message_types: Vec<String>,
    pub activities: Vec<String>,
    pub spawned: Option<bool>,
    pub timestamp_from_millis: Option<i64>,
    pub timestamp_until_millis: Option<i64>,
}

/// One bounded writer transaction for a provider-index generation.
///
/// Session batches are validated and staged one at a time, so callers do not
/// retain a corpus of parsed transcripts merely to preserve generation-level
/// atomicity. Dropping an uncommitted transaction rolls every staged change
/// back.
pub struct ProviderIndexTransaction<'a> {
    index: &'a ProviderSearchIndex,
    writer: Option<RwLockWriteGuard<'a, IndexWriter>>,
    generation: String,
    selected: BTreeSet<String>,
    batch_keys: BTreeSet<String>,
    removed_keys: BTreeSet<String>,
}

impl ProviderIndexTransaction<'_> {
    fn writer(&mut self) -> Result<&mut IndexWriter> {
        self.writer.as_deref_mut().ok_or_else(|| {
            SnatchError::IndexError("provider index transaction is already closed".to_string())
        })
    }

    /// Stage an exact replacement for one source-session partition.
    pub fn replace(&mut self, batch: IndexedSessionBatch) -> Result<()> {
        let violations = batch.validate();
        if !violations.is_empty() {
            return Err(SnatchError::IndexError(violations.join("; ")));
        }
        if batch.manifest.generation != self.generation {
            return Err(SnatchError::IndexError(format!(
                "session {} generation {} != transaction generation {}",
                batch.manifest.session_key, batch.manifest.generation, self.generation
            )));
        }
        if !self.selected.contains(&batch.manifest.provider) {
            return Err(SnatchError::IndexError(format!(
                "session {} belongs to unselected provider {}",
                batch.manifest.session_key, batch.manifest.provider
            )));
        }
        if self.removed_keys.contains(&batch.manifest.session_key) {
            return Err(SnatchError::IndexError(format!(
                "generation {} both replaces and removes session {}",
                self.generation, batch.manifest.session_key
            )));
        }
        if self.batch_keys.contains(&batch.manifest.session_key) {
            return Err(SnatchError::IndexError(format!(
                "generation {} repeats session {}",
                self.generation, batch.manifest.session_key
            )));
        }

        let session_key = batch.manifest.session_key.clone();
        let provider = batch.manifest.provider.clone();
        let session_document = self.index.session_document(&batch.manifest)?;
        let entry_documents = batch
            .entries
            .iter()
            .map(|entry| {
                self.index
                    .entry_document(entry, &provider)
                    .map(|document| (entry.entry_id.clone(), document))
            })
            .collect::<Result<Vec<_>>>()?;
        self.batch_keys.insert(session_key.clone());
        let session_field = self.index.fields.session_key;
        self.writer()?
            .delete_term(Term::from_field_text(session_field, &session_key));
        self.writer()?
            .add_document(session_document)
            .map_err(|error| {
                SnatchError::IndexError(format!(
                    "failed to stage session manifest {session_key}: {error}"
                ))
            })?;
        for (entry_id, document) in entry_documents {
            self.writer()?.add_document(document).map_err(|error| {
                SnatchError::IndexError(format!("failed to stage search entry {entry_id}: {error}"))
            })?;
        }
        Ok(())
    }

    /// Stage removal of one session proven absent from a complete provider
    /// inventory. Completeness is rechecked against the final build manifest
    /// at commit time.
    pub fn remove(&mut self, key: &LogicalSessionKey) -> Result<()> {
        let provider = key.provider.to_string();
        if !self.selected.contains(&provider) {
            return Err(SnatchError::IndexError(format!(
                "generation {} removes a session from unselected provider {}",
                self.generation, key.provider
            )));
        }
        let session_key = key.to_string();
        if self.batch_keys.contains(&session_key) {
            return Err(SnatchError::IndexError(format!(
                "generation {} both replaces and removes session {session_key}",
                self.generation
            )));
        }
        if !self.removed_keys.insert(session_key.clone()) {
            return Err(SnatchError::IndexError(format!(
                "generation {} repeats removed session {session_key}",
                self.generation
            )));
        }
        let session_field = self.index.fields.session_key;
        self.writer()?
            .delete_term(Term::from_field_text(session_field, &session_key));
        Ok(())
    }

    /// Commit the staged generation and publish it to this index's reader.
    pub fn commit(mut self, build: &ProviderIndexBuildManifest) -> Result<()> {
        let violations = build.validate();
        if !violations.is_empty() {
            return Err(SnatchError::IndexError(violations.join("; ")));
        }
        if build.generation != self.generation {
            return Err(SnatchError::IndexError(format!(
                "build generation {} != transaction generation {}",
                build.generation, self.generation
            )));
        }
        let build_selected: BTreeSet<_> = build.selected_providers.iter().cloned().collect();
        if build_selected != self.selected {
            return Err(SnatchError::IndexError(format!(
                "generation {} changed its selected provider set during staging",
                self.generation
            )));
        }
        let complete: BTreeSet<_> = build.complete_providers.iter().cloned().collect();
        for session_key in &self.removed_keys {
            let key: LogicalSessionKey = session_key.parse().map_err(|error: String| {
                SnatchError::IndexError(format!(
                    "removed session key '{session_key}' became invalid: {error}"
                ))
            })?;
            if !complete.contains(&key.provider.to_string()) {
                return Err(SnatchError::IndexError(format!(
                    "generation {} removes a session from incomplete provider {}",
                    self.generation, key.provider
                )));
            }
        }

        let build_document = self.index.build_document(build)?;
        let build_field = self.index.fields.doc_kind;
        let writer = self.writer()?;
        writer.delete_term(Term::from_field_text(build_field, KIND_BUILD));
        writer.add_document(build_document).map_err(|error| {
            SnatchError::IndexError(format!("failed to stage build manifest: {error}"))
        })?;
        writer.commit().map_err(|error| {
            SnatchError::IndexError(format!("failed to commit provider index: {error}"))
        })?;
        let writer = self
            .writer
            .take()
            .expect("writer remains open until commit");
        drop(writer);
        self.index.reader.reload().map_err(|error| {
            SnatchError::IndexError(format!("failed to reload provider index: {error}"))
        })?;
        secure_index_storage(&self.index.path)?;
        Ok(())
    }
}

impl Drop for ProviderIndexTransaction<'_> {
    fn drop(&mut self) {
        if let Some(mut writer) = self.writer.take() {
            let _ = writer.rollback();
        }
    }
}

impl ProviderSearchIndex {
    /// Default path shared with the legacy search index. Opening an old schema
    /// is refused; `snatch index rebuild` performs the explicit replacement.
    #[must_use]
    pub fn default_index_dir() -> PathBuf {
        directories::ProjectDirs::from("", "", "claude-snatch")
            .map(|directories| directories.cache_dir().to_path_buf())
            .unwrap_or_else(|| PathBuf::from(".claude-snatch-cache"))
            .join("search-index")
    }

    fn open_with_access(path: &Path, create: bool, writable: bool) -> Result<Self> {
        refuse_rebuild_in_progress(path)?;
        if create {
            ensure_index_directory(path)?;
        } else {
            match std::fs::symlink_metadata(path) {
                Ok(metadata) if metadata.file_type().is_symlink() => {
                    return Err(SnatchError::IndexError(format!(
                        "refusing provider index directory symlink: {}",
                        path.display()
                    )));
                }
                Ok(metadata) if !metadata.is_dir() => {
                    return Err(SnatchError::IndexError(format!(
                        "provider index path is not a directory: {}",
                        path.display()
                    )));
                }
                Ok(_) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    return Err(SnatchError::IndexError(format!(
                        "provider search index is not built at {}; run 'snatch index build'",
                        path.display()
                    )));
                }
                Err(error) => {
                    return Err(SnatchError::io(
                        format!("failed to inspect provider index path: {}", path.display()),
                        error,
                    ));
                }
            }
        }
        let expected = build_schema();
        let index = if path.join("meta.json").exists() {
            let index = Index::open_in_dir(path).map_err(|error| {
                SnatchError::IndexError(format!("failed to open provider index: {error}"))
            })?;
            let actual = index.schema();
            if actual != expected {
                return Err(SnatchError::IndexError(format!(
                    "incompatible search index schema at {}; run 'snatch index rebuild' to replace it explicitly",
                    path.display()
                )));
            }
            index
        } else if create {
            Index::create_in_dir(path, expected.clone()).map_err(|error| {
                SnatchError::IndexError(format!("failed to create provider index: {error}"))
            })?
        } else {
            return Err(SnatchError::IndexError(format!(
                "provider search index is not built at {}; run 'snatch index build'",
                path.display()
            )));
        };
        // Tighten an existing current-schema directory before opening a
        // writer that may create lock/segment files. Legacy schemas return
        // above without any permission or content mutation.
        if writable {
            secure_index_storage(path)?;
        }
        let schema = index.schema();
        let fields = ProviderIndexFields::from_schema(&schema)?;
        let reader = index
            .reader_builder()
            .reload_policy(ReloadPolicy::Manual)
            .try_into()
            .map_err(|error| {
                SnatchError::IndexError(format!("failed to create provider index reader: {error}"))
            })?;
        let writer = if writable {
            let writer = index.writer(WRITER_MEMORY_BYTES).map_err(|error| {
                SnatchError::IndexError(format!("failed to create provider index writer: {error}"))
            })?;
            // Cover lock files created while constructing the writer as well.
            secure_index_storage(path)?;
            Some(Arc::new(RwLock::new(writer)))
        } else {
            None
        };
        Ok(Self {
            index,
            fields,
            reader,
            writer,
            path: path.to_path_buf(),
        })
    }

    /// Open or create a writable provider index. Existing incompatible
    /// schemas are rejected without mutation and require an explicit rebuild.
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        Self::open_with_access(path.as_ref(), true, true)
    }

    /// Open an existing provider index without acquiring Tantivy's writer
    /// lock or creating storage as a query side effect.
    pub fn open_read_only(path: impl AsRef<Path>) -> Result<Self> {
        Self::open_with_access(path.as_ref(), false, false)
    }

    fn writer(&self) -> Result<&Arc<RwLock<IndexWriter>>> {
        self.writer.as_ref().ok_or_else(|| {
            SnatchError::IndexError("provider search index was opened read-only".to_string())
        })
    }

    /// Exact on-disk path.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Whether no committed entry/session/build documents exist.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.reader.searcher().num_docs() == 0
    }

    /// Snapshot statistics without consulting provider sources.
    pub fn stats(&self) -> Result<ProviderIndexStats> {
        let manifests = self.session_manifests()?;
        let entry_count = manifests.iter().fold(0_usize, |total, manifest| {
            total.saturating_add(manifest.entry_count)
        });
        let size_bytes = walkdir::WalkDir::new(&self.path)
            .follow_links(false)
            .into_iter()
            .filter_map(std::result::Result::ok)
            .filter(|entry| entry.file_type().is_file())
            .map(|entry| entry.metadata().map_or(0, |metadata| metadata.len()))
            .fold(0_u64, u64::saturating_add);
        Ok(ProviderIndexStats {
            schema_version: PROVIDER_INDEX_SCHEMA_VERSION,
            document_count: self.reader.searcher().num_docs(),
            session_count: manifests.len(),
            entry_count,
            size_bytes,
            build: self.build_manifest()?,
        })
    }

    /// Clear the current provider-index schema and publish the empty snapshot.
    pub fn clear(&self) -> Result<()> {
        let mut writer = self.writer()?.write();
        writer.delete_all_documents().map_err(|error| {
            SnatchError::IndexError(format!("failed to clear provider index: {error}"))
        })?;
        writer.commit().map_err(|error| {
            SnatchError::IndexError(format!("failed to commit cleared provider index: {error}"))
        })?;
        drop(writer);
        self.reader.reload().map_err(|error| {
            SnatchError::IndexError(format!("failed to reload cleared provider index: {error}"))
        })?;
        secure_index_storage(&self.path)?;
        Ok(())
    }

    fn base_document(&self, kind: &str, provider: &str, session_key: &str) -> TantivyDocument {
        doc!(
            self.fields.doc_kind => kind,
            self.fields.schema_version => PROVIDER_INDEX_SCHEMA_VERSION,
            self.fields.provider => provider,
            self.fields.session_key => session_key,
        )
    }

    fn entry_document(
        &self,
        entry: &IndexedSearchEntry,
        provider: &str,
    ) -> Result<TantivyDocument> {
        let mut document = self.base_document(KIND_ENTRY, provider, &entry.session_key);
        document.add_text(self.fields.logical_root, &entry.logical_root);
        document.add_text(self.fields.project_key, &entry.project_key);
        document.add_text(self.fields.project_path, &entry.project_path);
        document.add_text(self.fields.entry_id, &entry.entry_id);
        document.add_u64(
            self.fields.entry_order,
            u64::try_from(entry.entry_order).unwrap_or(u64::MAX),
        );
        document.add_text(self.fields.message_type, &entry.message_type);
        document.add_text(self.fields.activity, &entry.activity);
        document.add_u64(self.fields.spawned, u64::from(entry.spawned));
        if let Some(timestamp) = entry.timestamp {
            document.add_i64(self.fields.timestamp_millis, timestamp.timestamp_millis());
        }
        if let Some(model) = &entry.model {
            document.add_text(self.fields.model, model);
        }
        if let Some(branch) = &entry.git_branch {
            document.add_text(self.fields.git_branch, branch);
        }
        for segment in &entry.projection.segments {
            match segment.kind {
                kind if kind.is_text() => document.add_text(self.fields.text, &segment.text),
                SearchSegmentKind::Reasoning => {
                    document.add_text(self.fields.reasoning, &segment.text);
                }
                kind if kind.is_tool() => {
                    document.add_text(self.fields.tool_text, &segment.text);
                }
                _ => {}
            }
            if let Some(name) = &segment.tool_name {
                document.add_text(self.fields.tool_name, name);
            }
        }
        let payload = serde_json::to_string(entry)?;
        document.add_text(self.fields.payload, payload);
        Ok(document)
    }

    fn session_document(&self, manifest: &IndexedSessionManifest) -> Result<TantivyDocument> {
        let mut document =
            self.base_document(KIND_SESSION, &manifest.provider, &manifest.session_key);
        document.add_text(self.fields.logical_root, &manifest.logical_root);
        document.add_text(self.fields.project_key, &manifest.project_key);
        document.add_text(self.fields.project_path, &manifest.project_path);
        document.add_u64(self.fields.spawned, u64::from(manifest.spawned));
        document.add_text(self.fields.payload, serde_json::to_string(manifest)?);
        Ok(document)
    }

    fn build_document(&self, manifest: &ProviderIndexBuildManifest) -> Result<TantivyDocument> {
        let mut document = self.base_document(KIND_BUILD, "", "");
        document.add_text(self.fields.payload, serde_json::to_string(manifest)?);
        Ok(document)
    }

    /// Begin a bounded generation transaction. The provider selection is
    /// fixed up front; completion/skips remain final-manifest facts decided
    /// after the caller has streamed every changed session.
    pub fn begin_generation(
        &self,
        generation: &str,
        selected_providers: &[String],
    ) -> Result<ProviderIndexTransaction<'_>> {
        let selected: BTreeSet<_> = selected_providers.iter().cloned().collect();
        if generation.is_empty() {
            return Err(SnatchError::IndexError(
                "provider index transaction generation cannot be empty".to_string(),
            ));
        }
        if selected.is_empty()
            || selected.len() != selected_providers.len()
            || selected.contains("")
            || !selected_providers.windows(2).all(|pair| pair[0] < pair[1])
        {
            return Err(SnatchError::IndexError(format!(
                "generation {generation} has empty, duplicate, or unsorted selected providers"
            )));
        }
        Ok(ProviderIndexTransaction {
            index: self,
            writer: Some(self.writer()?.write()),
            generation: generation.to_string(),
            selected,
            batch_keys: BTreeSet::new(),
            removed_keys: BTreeSet::new(),
        })
    }

    /// Atomically apply staged source-session replacements/removals plus one
    /// build manifest. Validation occurs before the writer is touched.
    pub fn apply_generation(
        &self,
        batches: &[IndexedSessionBatch],
        removed: &[LogicalSessionKey],
        build: &ProviderIndexBuildManifest,
    ) -> Result<()> {
        let build_violations = build.validate();
        if !build_violations.is_empty() {
            return Err(SnatchError::IndexError(build_violations.join("; ")));
        }
        let selected: BTreeSet<_> = build.selected_providers.iter().cloned().collect();
        let complete: BTreeSet<_> = build.complete_providers.iter().cloned().collect();
        let mut batch_keys = BTreeSet::new();
        for batch in batches {
            let violations = batch.validate();
            if !violations.is_empty() {
                return Err(SnatchError::IndexError(violations.join("; ")));
            }
            if batch.manifest.generation != build.generation {
                return Err(SnatchError::IndexError(format!(
                    "session {} generation {} != build generation {}",
                    batch.manifest.session_key, batch.manifest.generation, build.generation
                )));
            }
            if !selected.contains(&batch.manifest.provider) {
                return Err(SnatchError::IndexError(format!(
                    "session {} belongs to unselected provider {}",
                    batch.manifest.session_key, batch.manifest.provider
                )));
            }
            if !batch_keys.insert(batch.manifest.session_key.clone()) {
                return Err(SnatchError::IndexError(format!(
                    "generation {} repeats session {}",
                    build.generation, batch.manifest.session_key
                )));
            }
        }
        let mut removed_keys = BTreeSet::new();
        for key in removed {
            if !selected.contains(&key.provider.to_string()) {
                return Err(SnatchError::IndexError(format!(
                    "generation {} removes a session from unselected provider {}",
                    build.generation, key.provider
                )));
            }
            if !complete.contains(&key.provider.to_string()) {
                return Err(SnatchError::IndexError(format!(
                    "generation {} removes a session from incomplete provider {}",
                    build.generation, key.provider
                )));
            }
            if !removed_keys.insert(key.to_string()) {
                return Err(SnatchError::IndexError(format!(
                    "generation {} repeats removed session {}",
                    build.generation, key
                )));
            }
        }
        if let Some(overlap) = batch_keys.intersection(&removed_keys).next() {
            return Err(SnatchError::IndexError(format!(
                "generation {} both replaces and removes session {overlap}",
                build.generation
            )));
        }

        let mut writer = self.writer()?.write();
        let apply = (|| -> Result<()> {
            for key in batch_keys.iter().chain(removed_keys.iter()) {
                writer.delete_term(Term::from_field_text(self.fields.session_key, key));
            }
            writer.delete_term(Term::from_field_text(self.fields.doc_kind, KIND_BUILD));
            for batch in batches {
                writer
                    .add_document(self.session_document(&batch.manifest)?)
                    .map_err(|error| {
                        SnatchError::IndexError(format!(
                            "failed to stage session manifest {}: {error}",
                            batch.manifest.session_key
                        ))
                    })?;
                for entry in &batch.entries {
                    writer
                        .add_document(self.entry_document(entry, &batch.manifest.provider)?)
                        .map_err(|error| {
                            SnatchError::IndexError(format!(
                                "failed to stage search entry {}: {error}",
                                entry.entry_id
                            ))
                        })?;
                }
            }
            writer
                .add_document(self.build_document(build)?)
                .map_err(|error| {
                    SnatchError::IndexError(format!("failed to stage build manifest: {error}"))
                })?;
            writer.commit().map_err(|error| {
                SnatchError::IndexError(format!("failed to commit provider index: {error}"))
            })?;
            Ok(())
        })();
        if let Err(error) = apply {
            let rollback = writer.rollback().map_err(|rollback| {
                SnatchError::IndexError(format!(
                    "{error}; provider index rollback also failed: {rollback}"
                ))
            });
            rollback?;
            return Err(error);
        }
        drop(writer);
        self.reader.reload().map_err(|error| {
            SnatchError::IndexError(format!("failed to reload provider index: {error}"))
        })?;
        secure_index_storage(&self.path)?;
        Ok(())
    }

    fn documents_of_kind<T>(&self, kind: &str) -> Result<Vec<T>>
    where
        T: for<'de> Deserialize<'de>,
    {
        let searcher = self.reader.searcher();
        let addresses = searcher
            .search(&AllQuery, &DocSetCollector)
            .map_err(|error| SnatchError::IndexError(format!("index scan failed: {error}")))?;
        let mut values = Vec::new();
        for address in addresses {
            let document: TantivyDocument = searcher.doc(address).map_err(|error| {
                SnatchError::IndexError(format!("failed to load indexed document: {error}"))
            })?;
            let actual_kind = document
                .get_first(self.fields.doc_kind)
                .and_then(|value| value.as_str());
            if actual_kind != Some(kind) {
                continue;
            }
            let payload = document
                .get_first(self.fields.payload)
                .and_then(|value| value.as_str())
                .ok_or_else(|| {
                    SnatchError::IndexError(format!(
                        "indexed {kind} document has no stored payload"
                    ))
                })?;
            values.push(serde_json::from_str(payload).map_err(|error| {
                SnatchError::IndexError(format!(
                    "indexed {kind} payload is invalid for schema {}: {error}",
                    PROVIDER_INDEX_SCHEMA_VERSION
                ))
            })?);
        }
        Ok(values)
    }

    fn exact_terms(field: Field, values: &[String]) -> Option<Box<dyn Query>> {
        if values.is_empty() {
            return None;
        }
        let terms = values
            .iter()
            .map(|value| {
                Box::new(TermQuery::new(
                    Term::from_field_text(field, value),
                    IndexRecordOption::Basic,
                )) as Box<dyn Query>
            })
            .collect();
        Some(Box::new(BooleanQuery::union(terms)))
    }

    /// Visit stored entry projections after narrowing only on exact typed
    /// fields. This deliberately never parses user text as Tantivy query
    /// syntax and never uses tokenized full-text terms as a necessary filter.
    ///
    /// Candidates are decoded one at a time rather than accumulated into a
    /// corpus-sized vector. Manual iteration mirrors Tantivy's collector path
    /// by consulting each segment's alive bitset, so replaced partitions
    /// cannot leak deleted documents into a query.
    pub(crate) fn visit_candidate_entries(
        &self,
        filter: &IndexedEntryCandidateFilter,
        mut visitor: impl FnMut(IndexedSearchEntry) -> Result<()>,
    ) -> Result<()> {
        let mut required: Vec<Box<dyn Query>> = vec![Box::new(TermQuery::new(
            Term::from_field_text(self.fields.doc_kind, KIND_ENTRY),
            IndexRecordOption::Basic,
        ))];
        for (field, values) in [
            (self.fields.provider, &filter.providers),
            (self.fields.session_key, &filter.session_keys),
            (self.fields.logical_root, &filter.logical_roots),
            (self.fields.project_key, &filter.project_keys),
            (self.fields.message_type, &filter.message_types),
            (self.fields.activity, &filter.activities),
        ] {
            if let Some(query) = Self::exact_terms(field, values) {
                required.push(query);
            }
        }
        if filter.session_keys_match_none {
            required.push(Box::new(TermQuery::new(
                Term::from_field_text(self.fields.session_key, ""),
                IndexRecordOption::Basic,
            )));
        }
        if let Some(spawned) = filter.spawned {
            required.push(Box::new(TermQuery::new(
                Term::from_field_u64(self.fields.spawned, u64::from(spawned)),
                IndexRecordOption::Basic,
            )));
        }
        if filter.timestamp_from_millis.is_some() || filter.timestamp_until_millis.is_some() {
            let lower = filter
                .timestamp_from_millis
                .map_or(Bound::Unbounded, |value| {
                    Bound::Included(Term::from_field_i64(self.fields.timestamp_millis, value))
                });
            let upper = filter
                .timestamp_until_millis
                .map_or(Bound::Unbounded, |value| {
                    Bound::Included(Term::from_field_i64(self.fields.timestamp_millis, value))
                });
            required.push(Box::new(RangeQuery::new(lower, upper)));
        }

        let query = BooleanQuery::intersection(required);
        let searcher = self.reader.searcher();
        let weight = query
            .weight(EnableScoring::disabled_from_searcher(&searcher))
            .map_err(|error| {
                SnatchError::IndexError(format!("index candidate query failed: {error}"))
            })?;
        for segment in searcher.segment_readers() {
            let store = segment.get_store_reader(1).map_err(|error| {
                SnatchError::IndexError(format!("failed to open indexed candidates: {error}"))
            })?;
            let alive = segment.alive_bitset();
            let mut visit_error = None;
            weight
                .for_each_no_score(segment, &mut |docs| {
                    for &doc_id in docs {
                        if visit_error.is_some()
                            || alive.is_some_and(|alive| !alive.is_alive(doc_id))
                        {
                            continue;
                        }
                        let result = (|| {
                            let document: TantivyDocument = store.get(doc_id).map_err(|error| {
                                SnatchError::IndexError(format!(
                                    "failed to load indexed candidate: {error}"
                                ))
                            })?;
                            let payload = document
                                .get_first(self.fields.payload)
                                .and_then(|value| value.as_str())
                                .ok_or_else(|| {
                                    SnatchError::IndexError(
                                        "indexed entry candidate has no stored payload".to_string(),
                                    )
                                })?;
                            let entry = serde_json::from_str(payload).map_err(|error| {
                                SnatchError::IndexError(format!(
                                    "indexed entry payload is invalid for schema {}: {error}",
                                    PROVIDER_INDEX_SCHEMA_VERSION
                                ))
                            })?;
                            visitor(entry)
                        })();
                        if let Err(error) = result {
                            visit_error = Some(error);
                        }
                    }
                })
                .map_err(|error| {
                    SnatchError::IndexError(format!("index candidate scan failed: {error}"))
                })?;
            if let Some(error) = visit_error {
                return Err(error);
            }
        }
        Ok(())
    }

    /// Sorted session manifests in the current committed snapshot.
    pub fn session_manifests(&self) -> Result<Vec<IndexedSessionManifest>> {
        let mut manifests: Vec<IndexedSessionManifest> = self.documents_of_kind(KIND_SESSION)?;
        manifests.sort_by(|a, b| a.session_key.cmp(&b.session_key));
        Ok(manifests)
    }

    /// Current build-generation metadata, when the index has been built.
    pub fn build_manifest(&self) -> Result<Option<ProviderIndexBuildManifest>> {
        let mut manifests = self.documents_of_kind(KIND_BUILD)?;
        if manifests.len() > 1 {
            return Err(SnatchError::IndexError(format!(
                "provider index contains {} build manifests",
                manifests.len()
            )));
        }
        Ok(manifests.pop())
    }

    /// Sorted entry projections in the current committed snapshot. This
    /// full scan is for tests/status and the later exact-query layer.
    pub fn entries(&self) -> Result<Vec<IndexedSearchEntry>> {
        let mut entries: Vec<IndexedSearchEntry> = self.documents_of_kind(KIND_ENTRY)?;
        entries.sort_by(|a, b| {
            a.session_key
                .cmp(&b.session_key)
                .then_with(|| a.entry_order.cmp(&b.entry_order))
                .then_with(|| a.entry_id.cmp(&b.entry_id))
        });
        Ok(entries)
    }

    /// Underlying schema, exposed for contract tests and diagnostics.
    #[must_use]
    pub fn schema(&self) -> Schema {
        self.index.schema()
    }
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use tempfile::tempdir;

    use super::*;
    use crate::provider::fake::{colliding_key, multi_artifact_key, FakeProvider};
    use crate::provider::SourceProvider;

    fn build_manifest(generation: &str) -> ProviderIndexBuildManifest {
        ProviderIndexBuildManifest {
            schema_version: PROVIDER_INDEX_SCHEMA_VERSION,
            generation: generation.to_string(),
            built_at: "2026-07-22T00:00:00Z".parse().unwrap(),
            selected_providers: vec!["fake".to_string()],
            complete_providers: vec!["fake".to_string()],
            removal_coverage_complete: true,
            skipped: Vec::new(),
            warnings: Vec::new(),
        }
    }

    fn fake_batch(generation: &str) -> IndexedSessionBatch {
        fake_batch_for(&multi_artifact_key(), generation)
    }

    fn fake_batch_for(key: &LogicalSessionKey, generation: &str) -> IndexedSessionBatch {
        let provider = FakeProvider;
        let parsed = provider.parse(key).unwrap();
        project_parsed_session(
            &parsed,
            &parsed.descriptor.key,
            "cwd:/work/fake",
            "/work/fake",
            false,
            "revision-1".to_string(),
            "metadata-1".to_string(),
            generation.to_string(),
            "2026-07-22T00:00:00Z".parse().unwrap(),
            None,
            None,
            None,
        )
        .unwrap()
    }

    #[test]
    fn schema_rejects_legacy_index_without_mutating_it() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("index");
        let legacy = super::super::SearchIndex::open(&path).unwrap();
        drop(legacy);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let before = std::fs::read(path.join("meta.json")).unwrap();
        let error = ProviderSearchIndex::open(&path)
            .err()
            .expect("legacy schema must be rejected")
            .to_string();
        assert!(error.contains("incompatible search index schema"));
        assert!(error.contains("index rebuild"));
        assert_eq!(std::fs::read(path.join("meta.json")).unwrap(), before);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o755,
                "schema rejection must not mutate legacy permissions"
            );
        }
    }

    #[test]
    fn repeated_generation_replaces_instead_of_duplicating() {
        let dir = tempdir().unwrap();
        let index = ProviderSearchIndex::open(dir.path().join("index")).unwrap();
        let first = fake_batch("generation-1");
        let expected_entries = first.entries.len();
        index
            .apply_generation(&[first], &[], &build_manifest("generation-1"))
            .unwrap();
        assert_eq!(index.session_manifests().unwrap().len(), 1);
        assert_eq!(index.entries().unwrap().len(), expected_entries);

        let mut second = fake_batch("generation-2");
        second.manifest.revision_token = "revision-2".to_string();
        index
            .apply_generation(&[second], &[], &build_manifest("generation-2"))
            .unwrap();
        let manifests = index.session_manifests().unwrap();
        assert_eq!(manifests.len(), 1);
        assert_eq!(manifests[0].revision_token, "revision-2");
        assert_eq!(index.entries().unwrap().len(), expected_entries);
        assert_eq!(
            index.build_manifest().unwrap().unwrap().generation,
            "generation-2"
        );
    }

    #[test]
    fn invalid_replacement_cannot_delete_the_previous_partition() {
        let dir = tempdir().unwrap();
        let index = ProviderSearchIndex::open(dir.path().join("index")).unwrap();
        let first = fake_batch("generation-1");
        let expected_entries = first.entries.len();
        index
            .apply_generation(&[first], &[], &build_manifest("generation-1"))
            .unwrap();

        let mut invalid = fake_batch("generation-2");
        invalid.entries[0].session_key = "fake:other".to_string();
        assert!(index
            .apply_generation(&[invalid], &[], &build_manifest("generation-2"))
            .is_err());
        assert_eq!(index.entries().unwrap().len(), expected_entries);
        assert_eq!(
            index.build_manifest().unwrap().unwrap().generation,
            "generation-1"
        );
    }

    #[test]
    fn invalid_build_metadata_cannot_replace_the_previous_snapshot() {
        let dir = tempdir().unwrap();
        let index = ProviderSearchIndex::open(dir.path().join("index")).unwrap();
        index
            .apply_generation(
                &[fake_batch("generation-1")],
                &[],
                &build_manifest("generation-1"),
            )
            .unwrap();
        let expected_manifests = index.session_manifests().unwrap();
        let expected_entries = index.entries().unwrap();
        let expected_build = index.build_manifest().unwrap();

        let invalid_builds = [
            ProviderIndexBuildManifest {
                selected_providers: Vec::new(),
                complete_providers: Vec::new(),
                removal_coverage_complete: false,
                ..build_manifest("generation-2")
            },
            ProviderIndexBuildManifest {
                selected_providers: vec!["fake".to_string(), "fake".to_string()],
                complete_providers: vec!["fake".to_string()],
                ..build_manifest("generation-2")
            },
            ProviderIndexBuildManifest {
                selected_providers: vec!["zeta".to_string(), "fake".to_string()],
                complete_providers: vec!["fake".to_string(), "zeta".to_string()],
                ..build_manifest("generation-2")
            },
            ProviderIndexBuildManifest {
                complete_providers: vec!["other".to_string()],
                ..build_manifest("generation-2")
            },
            ProviderIndexBuildManifest {
                complete_providers: Vec::new(),
                removal_coverage_complete: true,
                ..build_manifest("generation-2")
            },
            ProviderIndexBuildManifest {
                complete_providers: Vec::new(),
                removal_coverage_complete: false,
                skipped: vec![IndexedSkip {
                    provider: Some("fake".to_string()),
                    session_key: None,
                    reason: String::new(),
                }],
                ..build_manifest("generation-2")
            },
        ];

        for invalid in invalid_builds {
            assert!(index.apply_generation(&[], &[], &invalid).is_err());
            assert_eq!(index.session_manifests().unwrap(), expected_manifests);
            assert_eq!(index.entries().unwrap(), expected_entries);
            assert_eq!(index.build_manifest().unwrap(), expected_build);
        }
    }

    #[test]
    fn partial_generation_can_replace_successes_without_claiming_completeness() {
        let dir = tempdir().unwrap();
        let index = ProviderSearchIndex::open(dir.path().join("index")).unwrap();
        let batch = fake_batch("generation-1");
        let skipped_key = colliding_key().to_string();
        let partial = ProviderIndexBuildManifest {
            complete_providers: Vec::new(),
            removal_coverage_complete: false,
            skipped: vec![IndexedSkip {
                provider: Some("fake".to_string()),
                session_key: Some(skipped_key.clone()),
                reason: "parse failed".to_string(),
            }],
            ..build_manifest("generation-1")
        };
        index.apply_generation(&[batch], &[], &partial).unwrap();
        assert_eq!(index.session_manifests().unwrap().len(), 1);
        let actual = index.build_manifest().unwrap().unwrap();
        assert!(!actual.removal_coverage_complete);
        assert!(actual.complete_providers.is_empty());
        assert_eq!(
            actual.skipped[0].session_key.as_deref(),
            Some(&*skipped_key)
        );
    }

    #[test]
    fn removal_requires_a_complete_selected_provider_and_unique_key() {
        let dir = tempdir().unwrap();
        let index = ProviderSearchIndex::open(dir.path().join("index")).unwrap();
        let key = multi_artifact_key();
        index
            .apply_generation(
                &[fake_batch("generation-1")],
                &[],
                &build_manifest("generation-1"),
            )
            .unwrap();
        let expected = index.session_manifests().unwrap();

        let partial = ProviderIndexBuildManifest {
            complete_providers: Vec::new(),
            removal_coverage_complete: false,
            skipped: vec![IndexedSkip {
                provider: Some("fake".to_string()),
                session_key: Some(colliding_key().to_string()),
                reason: "parse failed".to_string(),
            }],
            ..build_manifest("generation-2")
        };
        let error = index
            .apply_generation(&[], std::slice::from_ref(&key), &partial)
            .unwrap_err()
            .to_string();
        assert!(error.contains("incomplete provider"));
        assert_eq!(index.session_manifests().unwrap(), expected);

        let error = index
            .apply_generation(&[], &[key.clone(), key], &build_manifest("generation-2"))
            .unwrap_err()
            .to_string();
        assert!(error.contains("repeats removed session"));
        assert_eq!(index.session_manifests().unwrap(), expected);
    }

    #[test]
    fn manifest_coverage_is_cross_checked_before_replacement() {
        let dir = tempdir().unwrap();
        let index = ProviderSearchIndex::open(dir.path().join("index")).unwrap();
        let mut invalid = fake_batch("generation-1");
        invalid.manifest.coverage.images_omitted =
            invalid.manifest.coverage.images_omitted.saturating_add(1);
        let error = index
            .apply_generation(&[invalid], &[], &build_manifest("generation-1"))
            .unwrap_err()
            .to_string();
        assert!(error.contains("manifest coverage"));
        assert!(index.is_empty());
    }

    #[test]
    fn invalid_provider_provenance_never_becomes_an_index_batch() {
        let provider = FakeProvider;
        let mut parsed = provider.parse(&multi_artifact_key()).unwrap();
        let removed = parsed.entries[0].id.clone();
        parsed.entry_origins.remove(&removed);
        let error = project_parsed_session(
            &parsed,
            &parsed.descriptor.key,
            "cwd:/work/fake",
            "/work/fake",
            false,
            "revision-1".to_string(),
            "metadata-1".to_string(),
            "generation-1".to_string(),
            "2026-07-22T00:00:00Z".parse().unwrap(),
            None,
            None,
            None,
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("invalid provenance"));
        assert!(error.contains(&removed.to_string()));
    }

    #[test]
    fn removal_deletes_exact_qualified_session_only() {
        let dir = tempdir().unwrap();
        let index = ProviderSearchIndex::open(dir.path().join("index")).unwrap();
        let first = fake_batch_for(&multi_artifact_key(), "generation-1");
        let sibling = fake_batch_for(&colliding_key(), "generation-1");
        let first_key = LogicalSessionKey::from_str(&first.manifest.session_key).unwrap();
        let sibling_key = sibling.manifest.session_key.clone();
        index
            .apply_generation(&[first, sibling], &[], &build_manifest("generation-1"))
            .unwrap();
        index
            .apply_generation(&[], &[first_key], &build_manifest("generation-2"))
            .unwrap();
        let manifests = index.session_manifests().unwrap();
        assert_eq!(manifests.len(), 1);
        assert_eq!(manifests[0].session_key, sibling_key);
        assert!(index
            .entries()
            .unwrap()
            .iter()
            .all(|entry| entry.session_key == sibling_key));
        assert!(!index.is_empty(), "the build manifest remains committed");
    }

    #[test]
    fn read_only_open_neither_contends_for_the_writer_nor_permits_mutation() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("index");
        let writable = ProviderSearchIndex::open(&path).unwrap();
        writable
            .apply_generation(
                &[fake_batch("generation-1")],
                &[],
                &build_manifest("generation-1"),
            )
            .unwrap();

        // The writable handle still owns Tantivy's writer lock. A read-only
        // query handle must nevertheless open and see the committed snapshot.
        let read_only = ProviderSearchIndex::open_read_only(&path).unwrap();
        assert_eq!(read_only.session_manifests().unwrap().len(), 1);
        assert!(read_only
            .clear()
            .unwrap_err()
            .to_string()
            .contains("read-only"));

        let absent = dir.path().join("absent");
        assert!(ProviderSearchIndex::open_read_only(&absent).is_err());
        assert!(!absent.exists(), "a read-only open must not create storage");
    }

    #[cfg(unix)]
    #[test]
    fn index_directory_is_owner_only_and_symlinks_are_refused() {
        use std::os::unix::fs::{symlink, PermissionsExt};

        let dir = tempdir().unwrap();
        let path = dir.path().join("index");
        let index = ProviderSearchIndex::open(&path).unwrap();
        assert_eq!(
            std::fs::metadata(index.path())
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
        assert_eq!(
            std::fs::metadata(index.path().join("meta.json"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
        drop(index);

        let target = dir.path().join("target");
        std::fs::create_dir(&target).unwrap();
        let link = dir.path().join("link");
        symlink(&target, &link).unwrap();
        assert!(ProviderSearchIndex::open(&link).is_err());
        assert!(!target.join("meta.json").exists());
    }
}
