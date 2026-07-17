//! Provider seam type contracts (Phase A.0 of the multi-provider design).
//!
//! This module pins the identity, artifact, provenance, capability, and
//! semantic-metadata types that the `SourceProvider` seam is built on, per
//! `docs/multi-provider-design.md`. Nothing here is threaded through the
//! existing pipeline yet — that is Phase A. The contracts:
//!
//! - **Identity is logical and namespaced** ([`LogicalSessionKey`]); physical
//!   artifacts are separate ([`ArtifactId`]), and an artifact's identity never
//!   includes its mutable revision ([`ArtifactRevision`] lives in
//!   [`ArtifactSnapshot`]) — an append to an active session must not mint a
//!   new artifact identity. Entry identity derives from the full logical key
//!   (namespace included), so sessions colliding on native id cannot collide
//!   on entry ids.
//! - **Provenance cardinality is explicit**: one native record may produce
//!   several entries, several records may collapse into one entry, and some
//!   records produce none. Every native record gets exactly one
//!   self-identifying [`RecordDisposition`]; entries carry their ids
//!   ([`IdentifiedEntry`]); [`ParsedSession::entry_origins`] is the reverse
//!   index; [`ParsedSession::validate_provenance`] cross-checks all three
//!   plus the diagnostics counters.
//! - **Unmodeled is not unmapped**: a parseable-but-unknown record still maps
//!   to preserved entries ([`RecordOutcome::Unknown`] carries them) — the
//!   content-complete promise survives schema drift. Fork-inherited history
//!   is Mapped (it is part of the fork's content) and annotated
//!   [`ActivityKind::InheritedHistory`] so cross-session analytics can
//!   exclude it from "new work"; only compaction replacement snapshots are
//!   suppressed.
//! - **Export fidelity is capability-tiered and streaming**: the `archive`
//!   tier is universal (lossless, provider-defined bundle written to a
//!   caller-supplied writer); `native` (exact source bytes) and `raw-jsonl`
//!   are optional capabilities.
//! - **Semantic annotations are provider-neutral axes with a real carrier**:
//!   adapters emit [`EntrySemantics`] keyed by entry id (the Phase A.0
//!   sidecar; Phase A may move individual fields onto model types per the
//!   design's carrier decision).

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::io::Write;

use crate::model::LogEntry;

// ============================================================================
// Identity
// ============================================================================

/// Identifies a session-log provider ("claude-code", "codex", ...).
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ProviderId(pub String);

impl ProviderId {
    /// The Claude Code provider.
    pub fn claude_code() -> Self {
        ProviderId("claude-code".into())
    }

    /// The OpenAI Codex CLI provider.
    pub fn codex() -> Self {
        ProviderId("codex".into())
    }
}

impl fmt::Display for ProviderId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Provider-defined identity namespace.
///
/// Native session ids are only guaranteed unique within a namespace.
/// UUID-based providers (Claude Code, Codex) use [`SessionNamespace::global`];
/// providers with database-local integer ids must scope them. Equivalent
/// backup roots of the same installation share a namespace; genuinely
/// separate installations must not collide accidentally.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct SessionNamespace(pub String);

impl SessionNamespace {
    /// Namespace for providers whose native ids are globally unique (UUIDs).
    pub fn global() -> Self {
        SessionNamespace("global".into())
    }
}

/// Global logical identity of a session: what "the same session" means even
/// when several physical artifacts (archived copies, compressed twins,
/// backups, fork-embedded history) exist.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct LogicalSessionKey {
    /// Which provider owns the session.
    pub provider: ProviderId,
    /// Provider-defined uniqueness scope for `native_id`.
    pub namespace: SessionNamespace,
    /// The provider's own session identifier, verbatim.
    pub native_id: String,
}

/// Escape a key segment for the colon-delimited external form: `%` → `%25`,
/// `:` → `%3A`. Reversible, so two distinct keys can never render
/// identically (namespace "a" + native "b:c" vs namespace "a:b" + native
/// "c" differ once literal colons are escaped).
fn escape_segment(s: &str) -> String {
    s.replace('%', "%25").replace(':', "%3A")
}

impl fmt::Display for LogicalSessionKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Qualified-id form used by CLI/MCP: "codex:<native-id>". The
        // namespace is omitted from the *display* form when global — segment
        // escaping keeps the two- and three-segment forms unambiguous.
        if self.namespace == SessionNamespace::global() {
            write!(
                f,
                "{}:{}",
                escape_segment(&self.provider.0),
                escape_segment(&self.native_id)
            )
        } else {
            write!(
                f,
                "{}:{}:{}",
                escape_segment(&self.provider.0),
                escape_segment(&self.namespace.0),
                escape_segment(&self.native_id)
            )
        }
    }
}

// ============================================================================
// Artifacts
// ============================================================================

/// Stable identity of one physical artifact holding (part of) a session.
///
/// Deliberately excludes any revision/mtime component: identity must survive
/// appends to an active session.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ArtifactId {
    /// Which discovered provider root/installation the artifact belongs to
    /// (e.g. a data-dir path or DB connection identity).
    pub provider_instance: String,
    /// Provider-meaningful locator within the instance (file path, table/row
    /// range descriptor, ...).
    pub locator: String,
}

/// Opaque provider-supplied revision token for cache invalidation
/// (path/size/mtime digest for files; row/index revision for databases).
/// Comparable for equality only — no ordering semantics.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ArtifactRevision(pub String);

/// An artifact at a specific revision: the cache key unit.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ArtifactSnapshot {
    /// Stable artifact identity.
    pub id: ArtifactId,
    /// Revision at observation time.
    pub revision: ArtifactRevision,
}

/// Physical form of an artifact, used for twin precedence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ArtifactForm {
    /// A plain JSONL file.
    PlainFile,
    /// A compressed file (e.g. `.jsonl.zst`).
    CompressedFile,
    /// Records inside a database.
    Database,
    /// Anything else, provider-described.
    Other(String),
}

/// One physical artifact of a session as discovered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionArtifact {
    /// Identity + revision at discovery time.
    pub snapshot: ArtifactSnapshot,
    /// Physical form (drives twin precedence).
    pub form: ArtifactForm,
    /// Whether the provider classifies this copy as archived.
    pub archived: bool,
}

/// Discovery-time description of a logical session and its artifacts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionDescriptor {
    /// Logical identity.
    pub key: LogicalSessionKey,
    /// All known physical artifacts (at least one; ids unique — see
    /// [`SessionDescriptor::validate`]).
    pub artifacts: Vec<SessionArtifact>,
}

impl SessionDescriptor {
    /// Twin precedence: the artifact reads/parses/native-export should use.
    ///
    /// Rules (documented contract, Phase A.0): active copies win over
    /// archived; plain files and databases win over compressed twins; the
    /// final tie-breaker is stable [`ArtifactId`] ordering — never discovery
    /// order, which filesystems/databases do not guarantee between runs.
    /// Returns `None` only for a descriptor with no artifacts (invalid — see
    /// [`SessionDescriptor::validate`]).
    pub fn preferred_artifact(&self) -> Option<&SessionArtifact> {
        fn form_rank(f: &ArtifactForm) -> u8 {
            match f {
                ArtifactForm::PlainFile | ArtifactForm::Database => 0,
                ArtifactForm::CompressedFile => 1,
                ArtifactForm::Other(_) => 2,
            }
        }
        self.artifacts
            .iter()
            .min_by_key(|a| (a.archived, form_rank(&a.form), &a.snapshot.id))
    }

    /// Structural validity: at least one artifact, and artifact ids unique
    /// within the descriptor. Returns human-readable violations.
    pub fn validate(&self) -> Vec<String> {
        let mut violations = Vec::new();
        if self.artifacts.is_empty() {
            violations.push(format!("descriptor {} has no artifacts", self.key));
        }
        let mut seen: BTreeSet<&ArtifactId> = BTreeSet::new();
        for a in &self.artifacts {
            if !seen.insert(&a.snapshot.id) {
                violations.push(format!(
                    "descriptor {} repeats artifact id {:?}",
                    self.key, a.snapshot.id
                ));
            }
        }
        violations
    }
}

// ============================================================================
// Capabilities
// ============================================================================

/// Optional export/fidelity capabilities a provider advertises.
///
/// The `archive` tier (lossless provider-defined bundle) and normalized
/// output are universal and therefore not represented here.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ProviderCapabilities {
    /// Exact source-artifact bytes exist and can be streamed (`native` tier).
    pub native_export: bool,
    /// The provider's records form a JSONL stream (`raw-jsonl` tier).
    pub raw_jsonl: bool,
}

// ============================================================================
// Provenance
// ============================================================================

/// Deterministic, provider-qualified identity of one normalized entry.
///
/// Structured — identity comparisons never depend on string encoding, so
/// delimiter collisions are impossible by construction. Stable across
/// repeated parsing and append-only growth (acceptance invariant #2). The
/// external encoding ([`fmt::Display`]) escapes segments and always includes
/// the namespace:
/// `<provider>:<namespace>:<native-id>:<ordinal>:<subindex>`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct EntryId {
    /// The session the entry belongs to.
    pub session: LogicalSessionKey,
    /// Native record ordinal the entry derives from.
    pub ordinal: u64,
    /// Sub-index for records producing several entries.
    pub subindex: u32,
}

impl EntryId {
    /// Build the deterministic id from the full logical key.
    pub fn deterministic(key: &LogicalSessionKey, record_ordinal: u64, subindex: u32) -> Self {
        EntryId {
            session: key.clone(),
            ordinal: record_ordinal,
            subindex,
        }
    }
}

impl fmt::Display for EntryId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}:{}:{}:{}:{}",
            escape_segment(&self.session.provider.0),
            escape_segment(&self.session.namespace.0),
            escape_segment(&self.session.native_id),
            self.ordinal,
            self.subindex
        )
    }
}

/// A normalized entry together with its deterministic identity — the
/// association the provenance maps are validated against.
#[derive(Debug, Clone)]
pub struct IdentifiedEntry {
    /// Deterministic id (see [`EntryId::deterministic`]).
    pub id: EntryId,
    /// The normalized entry.
    pub entry: LogEntry,
}

/// Reference to one native record inside one artifact.
///
/// Artifact identity + ordinal only — no content hashes (unnecessary absent a
/// corruption-detection requirement, and hashes of low-entropy sensitive text
/// leak equality information).
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct RecordRef {
    /// Which artifact the record lives in.
    pub artifact: ArtifactId,
    /// Zero-based record ordinal within the artifact (line number for JSONL,
    /// provider-defined stable ordering otherwise).
    pub ordinal: u64,
}

/// Why a native record was intentionally not normalized.
///
/// Fork-inherited history is NOT a suppression case: it is part of the
/// fork's content and must be Mapped (annotated
/// [`ActivityKind::InheritedHistory`]) so the fork is complete when viewed
/// independently.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SuppressionReason {
    /// The record duplicates content carried by another stream of the same
    /// source (e.g. Codex `event_msg` mirroring a `response_item`).
    DuplicateStream,
    /// The record is a compaction replacement snapshot: it replays context,
    /// it does not record new activity.
    CompactionReplacement,
    /// Provider-described reason.
    Other(String),
}

/// Diagnostic captured for a record that could not be parsed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseDiagnostic {
    /// Human-readable parse failure description.
    pub message: String,
}

/// What became of one native record. Exactly one disposition exists per
/// record (acceptance invariant #1: mapped, suppressed-with-reason,
/// unknown-but-preserved, or unparseable — never silently dropped).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RecordOutcome {
    /// Normalized into these entries (non-empty; one record may feed several).
    Mapped(Vec<EntryId>),
    /// Intentionally not normalized.
    Suppressed {
        /// Why.
        reason: SuppressionReason,
    },
    /// Structurally parseable but unmodeled (drift signal for doctor). The
    /// content is still preserved — these entries (non-empty, normally one
    /// `LogEntry::Unknown`) carry it, keeping normalized output
    /// content-complete under schema drift.
    Unknown {
        /// Entries preserving the unmodeled content.
        entries: Vec<EntryId>,
    },
    /// Damaged but partially salvaged (e.g. a torn/fused JSONL line): the
    /// record had a parse error AND produced recovered entries. Separates
    /// record status from produced entries so salvage is representable.
    Recovered {
        /// Entries recovered from the damaged record (non-empty).
        entries: Vec<EntryId>,
        /// The original parse failure.
        error: ParseDiagnostic,
    },
    /// Could not be parsed (and nothing salvaged).
    Unparseable {
        /// What went wrong.
        error: ParseDiagnostic,
    },
}

/// Self-identifying record accounting: names its record explicitly so
/// accounting works across multiple artifacts without implicit ordering.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordDisposition {
    /// The native record this disposition describes.
    pub record: RecordRef,
    /// What became of it.
    pub outcome: RecordOutcome,
}

/// Ingestion counters surfaced to doctor/diagnostics. Cross-checked against
/// `record_dispositions` by [`ParsedSession::validate_provenance`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct IngestionDiagnostics {
    /// Records normalized into entries.
    pub mapped: usize,
    /// Records intentionally suppressed.
    pub suppressed: usize,
    /// Parseable-but-unmodeled records (drift; content preserved).
    pub unknown: usize,
    /// Damaged records partially salvaged (torn lines).
    pub recovered: usize,
    /// Unparseable records (nothing salvaged).
    pub unparseable: usize,
}

// ============================================================================
// Semantic annotations (provider-neutral axes; adapters emit these)
// ============================================================================

/// Whether an entry records new activity or replays inherited history.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ActivityKind {
    /// New activity in this session.
    #[default]
    New,
    /// History copied from another session (e.g. Codex fork-embedded
    /// history). Present when viewing this session; excluded from "new work"
    /// in cross-session analytics.
    InheritedHistory,
}

/// Who authored a prompt-shaped input (axis 1 of prompt semantics).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PromptAuthorship {
    /// A human typed it.
    Human,
    /// The harness/runtime injected it (reminders, hooks, notifications).
    Harness,
    /// It carries tool output.
    Tool,
}

/// How a prompt-shaped input was delivered (axis 2 — independent of
/// authorship: a steered message is human-authored but mid-turn-delivered).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PromptDelivery {
    /// At a turn boundary.
    TurnBoundary,
    /// Injected mid-turn (steering/queued input).
    MidTurn,
}

/// Prompt semantics: the two independent axes together.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PromptSemantics {
    /// Who authored it.
    pub authorship: PromptAuthorship,
    /// How it was delivered.
    pub delivery: PromptDelivery,
}

/// Coarse provider-neutral tool classification.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolKind {
    /// Shell/command execution.
    Shell,
    /// Reading files/content.
    FileRead,
    /// Writing or editing files.
    FileWrite,
    /// Searching (code, files).
    Search,
    /// Web search/fetch.
    Web,
    /// Spawning a subagent.
    Subagent,
    /// An MCP tool.
    Mcp,
    /// Provider-described.
    Other(String),
}

/// Tool semantics: canonical kind plus the provider's own tool name,
/// preserved verbatim.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolSemantics {
    /// Canonical classification.
    pub kind: ToolKind,
    /// The native tool name, unmodified.
    pub native_name: String,
}

/// What a usage observation covers (axis 1 of usage semantics).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UsageScope {
    /// One model call.
    Call,
    /// One turn.
    Turn,
    /// The whole session so far.
    Session,
}

/// How a usage observation aggregates (axis 2 — Codex `token_count` events
/// carry a last-call delta and a session-cumulative total side by side).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UsageAggregation {
    /// A standalone measurement for its scope.
    Delta,
    /// A running cumulative total.
    Cumulative,
}

/// One usage observation attached to an entry: both axes plus the observed
/// values, so an annotation is never separated from the numbers it
/// describes.
#[derive(Debug, Clone)]
pub struct UsageObservation {
    /// What the numbers cover.
    pub scope: UsageScope,
    /// How they aggregate.
    pub aggregation: UsageAggregation,
    /// The observed token values.
    pub usage: crate::model::usage::Usage,
}

/// The Phase A.0 semantic carrier.
///
/// Everything an adapter asserts about one entry, keyed by entry id in
/// [`ParsedSession::semantics`]. Phase A may migrate individual fields onto
/// model types per the design's carrier decision; the sidecar is the
/// seam-level contract.
#[derive(Debug, Clone, Default)]
pub struct EntrySemantics {
    /// New activity vs inherited history.
    pub activity: ActivityKind,
    /// Prompt axes, when the entry is prompt-shaped.
    pub prompt: Option<PromptSemantics>,
    /// Tool axes per tool call, keyed by the native tool-call id — one entry
    /// can carry several tool calls with different classifications.
    pub tools: BTreeMap<String, ToolSemantics>,
    /// Usage observations carried by the entry (may be several: Codex emits
    /// a Call/Delta and a Session/Cumulative side by side; each carries its
    /// own values so annotations stay paired with numbers).
    pub usage: Vec<UsageObservation>,
}

/// Typed session-lineage edge. Lineage is a graph with typed edges, not a
/// generic "chain"; compaction window links are intra-session metadata and
/// deliberately absent here.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum LineageEdgeKind {
    /// Same conversation continued (Claude Code resume chains).
    Continuation,
    /// A new session branched from copied history (Codex fork).
    Fork,
    /// A subagent spawned by a parent session, with the sidecar metadata
    /// downstream matching/presentation needs.
    Spawn {
        /// Spawning Agent/Task tool_use id, when the provider records it.
        tool_use_id: Option<String>,
        /// Agent type (e.g. "Explore"), when recorded.
        agent_type: Option<String>,
        /// Spawn description, when recorded.
        description: Option<String>,
    },
}

/// One typed lineage edge between two logical sessions. Providers emit
/// edges sorted and deduplicated for deterministic output.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct LineageEdge {
    /// The predecessor/parent session.
    pub from: LogicalSessionKey,
    /// The successor/child session.
    pub to: LogicalSessionKey,
    /// The relationship.
    pub kind: LineageEdgeKind,
}

/// Kind of compaction event. Carrier for compaction window metadata is
/// explicitly deferred to Phase B3/C.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CompactionKind {
    /// Full context compaction.
    Full,
    /// Partial/micro compaction.
    Micro,
    /// Provider-described variant.
    Other(String),
}

// ============================================================================
// ParsedSession
// ============================================================================

/// A fully parsed session: identified normalized entries plus explicit
/// provenance and semantics.
///
/// Native data is NOT embedded here — raw fidelity lives at the session
/// source, reachable through the provider's archive/native/raw streams.
#[derive(Debug, Clone)]
pub struct ParsedSession {
    /// Discovery-time descriptor (identity + artifacts).
    pub descriptor: SessionDescriptor,
    /// Normalized entries with their deterministic ids, in canonical order.
    pub entries: Vec<IdentifiedEntry>,
    /// Entry-id → native records that produced it (N:1 and 1:N expressible).
    pub entry_origins: BTreeMap<EntryId, Vec<RecordRef>>,
    /// Exactly one disposition per native record.
    pub record_dispositions: Vec<RecordDisposition>,
    /// Adapter-asserted semantics, keyed by entry id.
    pub semantics: BTreeMap<EntryId, EntrySemantics>,
    /// Ingestion counters (cross-checked against dispositions).
    pub diagnostics: IngestionDiagnostics,
}

impl ParsedSession {
    /// Cross-validate entries, `entry_origins`, `record_dispositions`,
    /// `semantics`, and `diagnostics`.
    ///
    /// Returns human-readable violations; empty means consistent. Checks:
    /// descriptor validity; entry ids unique; every entry has a non-empty
    /// origin set; every origin key names an existing entry; dispositions
    /// name each record at most once; `Mapped`/`Unknown` entry lists are
    /// non-empty, name existing entries, and agree with the reverse origin
    /// index; every origin record is producing (`Mapped` or `Unknown`);
    /// semantics keys name existing entries; diagnostics counters equal the
    /// disposition tallies.
    pub fn validate_provenance(&self) -> Vec<String> {
        let mut violations = self.descriptor.validate();

        // Entry ids: unique, and the authoritative id set.
        let mut entry_ids: BTreeSet<&EntryId> = BTreeSet::new();
        for e in &self.entries {
            if !entry_ids.insert(&e.id) {
                violations.push(format!("duplicate entry id {}", e.id));
            }
            if e.id.session != self.descriptor.key {
                violations.push(format!(
                    "entry {} belongs to session {}, not this session ({})",
                    e.id, e.id.session, self.descriptor.key
                ));
            }
        }

        // Dispositions: unique records, valid outcome shapes, tallies.
        let mut producing: BTreeMap<&RecordRef, &Vec<EntryId>> = BTreeMap::new();
        let mut seen_records: BTreeSet<&RecordRef> = BTreeSet::new();
        let mut tally = IngestionDiagnostics::default();
        for d in &self.record_dispositions {
            if !seen_records.insert(&d.record) {
                violations.push(format!(
                    "record {:?}#{} has more than one disposition",
                    d.record.artifact, d.record.ordinal
                ));
            }
            let produced = match &d.outcome {
                RecordOutcome::Mapped(entries) => {
                    tally.mapped += 1;
                    Some(entries)
                }
                RecordOutcome::Unknown { entries } => {
                    tally.unknown += 1;
                    Some(entries)
                }
                RecordOutcome::Recovered { entries, .. } => {
                    tally.recovered += 1;
                    Some(entries)
                }
                RecordOutcome::Suppressed { .. } => {
                    tally.suppressed += 1;
                    None
                }
                RecordOutcome::Unparseable { .. } => {
                    tally.unparseable += 1;
                    None
                }
            };
            if let Some(entries) = produced {
                if entries.is_empty() {
                    violations.push(format!(
                        "record #{} has a producing outcome with an empty entry list",
                        d.record.ordinal
                    ));
                }
                let mut edge_dedup: BTreeSet<&EntryId> = BTreeSet::new();
                for e in entries {
                    if !edge_dedup.insert(e) {
                        violations.push(format!(
                            "record #{} names entry {} more than once",
                            d.record.ordinal, e
                        ));
                    }
                }
                producing.insert(&d.record, entries);
                for e in entries {
                    if !entry_ids.contains(e) {
                        violations.push(format!(
                            "record #{} names entry {} which does not exist",
                            d.record.ordinal, e
                        ));
                    }
                    match self.entry_origins.get(e) {
                        None => violations.push(format!(
                            "record #{} maps to entry {} which has no origins",
                            d.record.ordinal, e
                        )),
                        Some(origins) if !origins.contains(&d.record) => violations.push(format!(
                            "entry {} origins do not include record #{}",
                            e, d.record.ordinal
                        )),
                        Some(_) => {}
                    }
                }
            }
        }
        if tally != self.diagnostics {
            violations.push(format!(
                "diagnostics {:?} do not match disposition tallies {:?}",
                self.diagnostics, tally
            ));
        }

        // Every entry must have provenance; every origin must be real.
        for e in &self.entries {
            match self.entry_origins.get(&e.id) {
                None => violations.push(format!("entry {} has no origins", e.id)),
                Some(origins) if origins.is_empty() => {
                    violations.push(format!("entry {} has an empty origin list", e.id));
                }
                Some(_) => {}
            }
        }
        for (entry, origins) in &self.entry_origins {
            let mut origin_dedup: BTreeSet<&RecordRef> = BTreeSet::new();
            for r in origins {
                if !origin_dedup.insert(r) {
                    violations.push(format!(
                        "entry {} lists origin record #{} more than once",
                        entry, r.ordinal
                    ));
                }
            }
            if !entry_ids.contains(entry) {
                violations.push(format!(
                    "entry_origins names entry {} which does not exist",
                    entry
                ));
            }
            for r in origins {
                match producing.get(r) {
                    None => violations.push(format!(
                        "entry {} claims origin record #{} which has no producing disposition",
                        entry, r.ordinal
                    )),
                    Some(entries) if !entries.contains(entry) => violations.push(format!(
                        "record #{} produces entries but not entry {}",
                        r.ordinal, entry
                    )),
                    Some(_) => {}
                }
            }
        }

        // Semantics may only describe existing entries, and per-call tool
        // semantics must reference tool calls the entry actually contains.
        let entry_by_id: BTreeMap<&EntryId, &LogEntry> =
            self.entries.iter().map(|e| (&e.id, &e.entry)).collect();
        for (id, sem) in &self.semantics {
            match entry_by_id.get(id) {
                None => {
                    violations.push(format!("semantics names entry {id} which does not exist"));
                }
                Some(entry) if !sem.tools.is_empty() => {
                    let call_ids: BTreeSet<&str> = match entry {
                        LogEntry::Assistant(a) => a
                            .message
                            .tool_uses()
                            .iter()
                            .map(|t| t.id.as_str())
                            .collect(),
                        _ => BTreeSet::new(),
                    };
                    for call in sem.tools.keys() {
                        if !call_ids.contains(call.as_str()) {
                            violations.push(format!(
                                "semantics for entry {id} references tool call {call} which the entry does not contain"
                            ));
                        }
                    }
                }
                Some(_) => {}
            }
        }

        violations
    }
}

// ============================================================================
// SourceProvider seam
// ============================================================================

/// Errors a provider operation can produce.
#[derive(Debug)]
pub enum ProviderError {
    /// The operation needs a capability this provider does not advertise.
    Unsupported {
        /// Which capability was required.
        capability: &'static str,
    },
    /// The session/artifact was not found.
    NotFound(String),
    /// An I/O failure while streaming.
    Io(std::io::Error),
    /// Any other provider failure.
    Other(String),
}

impl fmt::Display for ProviderError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ProviderError::Unsupported { capability } => {
                write!(f, "provider does not support {capability}")
            }
            ProviderError::NotFound(what) => write!(f, "not found: {what}"),
            ProviderError::Io(e) => write!(f, "stream error: {e}"),
            ProviderError::Other(msg) => f.write_str(msg),
        }
    }
}

impl std::error::Error for ProviderError {}

impl From<std::io::Error> for ProviderError {
    fn from(e: std::io::Error) -> Self {
        ProviderError::Io(e)
    }
}

/// The discovery+parse seam: the dual of the existing `Exporter` trait.
///
/// Phase A.0 pins the signatures; production implementations arrive in
/// Phase A (`claude-code`) and Phase B (`codex`). All fidelity tiers stream
/// to a caller-supplied writer — archives and compressed logs can be
/// multi-gigabyte and must never require full in-memory buffering.
pub trait SourceProvider {
    /// Provider identity.
    fn id(&self) -> ProviderId;

    /// Optional capabilities beyond the universal archive/normalized tiers.
    fn capabilities(&self) -> ProviderCapabilities;

    /// Enumerate discovered sessions (logical identity + artifacts).
    fn sessions(&self) -> Result<Vec<SessionDescriptor>, ProviderError>;

    /// Typed session-lineage edges across this provider's corpus
    /// (continuations, forks, spawns). Endpoints are normally keys returned
    /// by [`SourceProvider::sessions`], but dangling endpoints are allowed —
    /// real corpora reference deleted or unavailable parents, and the edge
    /// is still information worth keeping.
    fn lineage(&self) -> Result<Vec<LineageEdge>, ProviderError>;

    /// Parse one session into identified entries with full provenance and
    /// semantics.
    fn parse(&self, key: &LogicalSessionKey) -> Result<ParsedSession, ProviderError>;

    /// Universal lossless tier: write a provider-defined bundle (with
    /// manifest) for the session. Must be lossless: the session's native
    /// records are recoverable from the bundle.
    fn write_archive(
        &self,
        key: &LogicalSessionKey,
        out: &mut dyn Write,
    ) -> Result<(), ProviderError>;

    /// `native` tier: stream exact bytes of one source artifact. Errs with
    /// [`ProviderError::Unsupported`] unless `capabilities().native_export`.
    fn write_native(&self, artifact: &ArtifactId, out: &mut dyn Write)
        -> Result<(), ProviderError>;

    /// `raw-jsonl` tier: stream the unmodified JSONL record stream. Errs
    /// with [`ProviderError::Unsupported`] unless `capabilities().raw_jsonl`.
    fn write_raw_jsonl(
        &self,
        key: &LogicalSessionKey,
        out: &mut dyn Write,
    ) -> Result<(), ProviderError>;
}

pub mod claude_code;
#[cfg(feature = "codex")]
pub mod codex;

#[cfg(test)]
pub(crate) mod fake;
#[cfg(test)]
mod tests;
