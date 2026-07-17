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
//!   new artifact identity.
//! - **Provenance cardinality is explicit**: one native record may produce
//!   several entries, several records may collapse into one entry, and some
//!   records produce none. Every native record gets exactly one
//!   self-identifying [`RecordDisposition`]; [`ParsedSession::entry_origins`]
//!   is the reverse index and the two are cross-validated by
//!   [`ParsedSession::validate_provenance`].
//! - **Export fidelity is capability-tiered**: the `archive` tier is
//!   universal (lossless, provider-defined bundle); `native` (exact source
//!   bytes) and `raw-jsonl` are optional capabilities.
//! - **Semantic annotations are provider-neutral axes**, emitted by adapters
//!   at normalization time (authorship vs delivery; usage scope vs
//!   aggregation; typed lineage edges).

use std::collections::BTreeMap;
use std::fmt;

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

impl fmt::Display for LogicalSessionKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Qualified-id form used by CLI/MCP: "codex:<native-id>". The
        // namespace is omitted from the display form when global.
        if self.namespace == SessionNamespace::global() {
            write!(f, "{}:{}", self.provider, self.native_id)
        } else {
            write!(
                f,
                "{}:{}:{}",
                self.provider, self.namespace.0, self.native_id
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
    /// All known physical artifacts (at least one).
    pub artifacts: Vec<SessionArtifact>,
}

impl SessionDescriptor {
    /// Twin precedence: the artifact reads/parses/native-export should use.
    ///
    /// Rules (documented contract, Phase A.0): active copies win over
    /// archived; plain files win over compressed twins; databases rank with
    /// plain files; otherwise first-discovered wins. Returns `None` only for
    /// a descriptor with no artifacts (invalid by construction).
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
            .enumerate()
            .min_by_key(|(i, a)| (a.archived, form_rank(&a.form), *i))
            .map(|(_, a)| a)
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
/// Stable across repeated parsing and append-only growth (acceptance
/// invariant #2). Canonical form: `<provider>:<session>:<ordinal>:<subindex>`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct EntryId(pub String);

impl EntryId {
    /// Build the canonical deterministic id.
    pub fn deterministic(
        provider: &ProviderId,
        native_session_id: &str,
        record_ordinal: u64,
        subindex: u32,
    ) -> Self {
        EntryId(format!(
            "{provider}:{native_session_id}:{record_ordinal}:{subindex}"
        ))
    }
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
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SuppressionReason {
    /// The record duplicates content carried by another stream of the same
    /// source (e.g. Codex `event_msg` mirroring a `response_item`).
    DuplicateStream,
    /// The record replays prior history rather than recording new activity
    /// (e.g. compaction `replacement_history`, fork-copied history).
    ReplayedHistory,
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
/// unknown, or unparseable — never silently dropped).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RecordOutcome {
    /// Normalized into these entries (one record may feed several).
    Mapped(Vec<EntryId>),
    /// Intentionally not normalized.
    Suppressed {
        /// Why.
        reason: SuppressionReason,
    },
    /// Structurally parseable but unmodeled (drift signal for doctor).
    Unknown,
    /// Could not be parsed.
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

/// Ingestion counters surfaced to doctor/diagnostics.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct IngestionDiagnostics {
    /// Records normalized into entries.
    pub mapped: usize,
    /// Records intentionally suppressed.
    pub suppressed: usize,
    /// Parseable-but-unmodeled records (drift).
    pub unknown: usize,
    /// Unparseable records.
    pub unparseable: usize,
}

/// A fully parsed session: normalized entries plus explicit provenance.
///
/// Native data is NOT embedded here — raw fidelity lives at the session
/// source, reachable through the provider's archive/native/raw streams.
#[derive(Debug, Clone)]
pub struct ParsedSession {
    /// Discovery-time descriptor (identity + artifacts).
    pub descriptor: SessionDescriptor,
    /// Normalized entries, in canonical order.
    pub entries: Vec<LogEntry>,
    /// Entry-id → native records that produced it (N:1 and 1:N expressible).
    pub entry_origins: BTreeMap<EntryId, Vec<RecordRef>>,
    /// Exactly one disposition per native record.
    pub record_dispositions: Vec<RecordDisposition>,
    /// Ingestion counters.
    pub diagnostics: IngestionDiagnostics,
}

impl ParsedSession {
    /// Cross-validate `entry_origins` against `record_dispositions`.
    ///
    /// Returns human-readable violations; empty means consistent. Checks:
    /// every disposition record is unique; every `Mapped` entry id appears in
    /// `entry_origins`; every origin record is `Mapped`; every origin's
    /// mapped set contains the entry; entry ids in `entry_origins` are
    /// distinct keys by construction (BTreeMap).
    pub fn validate_provenance(&self) -> Vec<String> {
        let mut violations = Vec::new();

        let mut mapped_records: BTreeMap<&RecordRef, &Vec<EntryId>> = BTreeMap::new();
        let mut seen: std::collections::BTreeSet<&RecordRef> = Default::default();
        for d in &self.record_dispositions {
            if !seen.insert(&d.record) {
                violations.push(format!(
                    "record {:?}#{} has more than one disposition",
                    d.record.artifact, d.record.ordinal
                ));
            }
            if let RecordOutcome::Mapped(entries) = &d.outcome {
                mapped_records.insert(&d.record, entries);
                for e in entries {
                    match self.entry_origins.get(e) {
                        None => violations.push(format!(
                            "disposition maps record #{} to entry {} which has no origins",
                            d.record.ordinal, e.0
                        )),
                        Some(origins) if !origins.contains(&d.record) => violations.push(format!(
                            "entry {} origins do not include record #{}",
                            e.0, d.record.ordinal
                        )),
                        Some(_) => {}
                    }
                }
            }
        }

        for (entry, origins) in &self.entry_origins {
            for r in origins {
                match mapped_records.get(r) {
                    None => violations.push(format!(
                        "entry {} claims origin record #{} which is not Mapped",
                        entry.0, r.ordinal
                    )),
                    Some(entries) if !entries.contains(entry) => violations.push(format!(
                        "record #{} is Mapped but not to entry {}",
                        r.ordinal, entry.0
                    )),
                    Some(_) => {}
                }
            }
        }

        violations
    }
}

// ============================================================================
// Semantic annotations (provider-neutral axes; adapters emit these)
// ============================================================================

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

/// Typed session-lineage edge. Lineage is a graph with typed edges, not a
/// generic "chain"; compaction window links are intra-session metadata and
/// deliberately absent here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LineageEdgeKind {
    /// Same conversation continued (Claude Code resume chains).
    Continuation,
    /// A new session branched from copied history (Codex fork).
    Fork,
    /// A subagent spawned by a parent session.
    Spawn,
}

/// Kind of compaction event.
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
            ProviderError::Other(msg) => f.write_str(msg),
        }
    }
}

impl std::error::Error for ProviderError {}

/// The discovery+parse seam: the dual of the existing `Exporter` trait.
///
/// Phase A.0 pins the signatures; production implementations arrive in
/// Phase A (`claude-code`) and Phase B (`codex`). Streaming refinements
/// (readers instead of byte buffers) are a Phase A concern.
pub trait SourceProvider {
    /// Provider identity.
    fn id(&self) -> ProviderId;

    /// Optional capabilities beyond the universal archive/normalized tiers.
    fn capabilities(&self) -> ProviderCapabilities;

    /// Enumerate discovered sessions (logical identity + artifacts).
    fn sessions(&self) -> Result<Vec<SessionDescriptor>, ProviderError>;

    /// Parse one session into normalized entries with full provenance.
    fn parse(&self, key: &LogicalSessionKey) -> Result<ParsedSession, ProviderError>;

    /// Universal lossless tier: a provider-defined bundle for the session.
    fn read_archive(&self, key: &LogicalSessionKey) -> Result<Vec<u8>, ProviderError>;

    /// `native` tier: exact bytes of one source artifact. Errs with
    /// [`ProviderError::Unsupported`] unless `capabilities().native_export`.
    fn read_native(&self, artifact: &ArtifactId) -> Result<Vec<u8>, ProviderError>;

    /// `raw-jsonl` tier: the unmodified JSONL record stream. Errs with
    /// [`ProviderError::Unsupported`] unless `capabilities().raw_jsonl`.
    fn read_raw_jsonl(&self, key: &LogicalSessionKey) -> Result<Vec<u8>, ProviderError>;
}

#[cfg(test)]
mod fake;
#[cfg(test)]
mod tests;
