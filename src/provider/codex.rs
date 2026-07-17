//! OpenAI Codex CLI as a [`SourceProvider`].
//!
//! Discovery covers plain + archived + `.zst` twins, active/truncated files,
//! and legacy detection. B3 normalization maps conversational content,
//! tools, usage, prompt delivery, and fork-inherited history; envelope
//! families not yet modeled remain content-complete `LogEntry::Unknown`
//! entries with explicit dispositions. Legacy pre-envelope files (Codex
//! ≤0.31.0, before 2025-09-10) are recognized, inventoried, and
//! native/raw-exportable; `parse()` reports them unsupported-legacy until
//! provenance-documented fixtures justify a parser.
//!
//! Layout (verified against codex-rs rust-v0.144.5 and the live corpus):
//! `$CODEX_HOME/sessions/YYYY/MM/DD/rollout-<local-ts>-<uuid>.jsonl`, an
//! `archived_sessions/` twin tree, cold copies as `.jsonl.zst` (plain wins
//! when both exist), and per-line envelopes `{timestamp, type, payload}`.
//! Compressed input is decoded through a streaming reader with
//! `window_log_max` and a decompressed-output limit — never `decode_all`.

use std::collections::BTreeMap;
use std::fs::File;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use super::{
    ActivityKind, ArtifactForm, ArtifactId, ArtifactRevision, ArtifactSnapshot,
    IngestionDiagnostics, LineageEdge, LineageEdgeKind, LogicalSessionKey, ParseDiagnostic,
    ParsedSession, ProviderCapabilities, ProviderError, ProviderId, RecordDisposition,
    RecordOutcome, RecordRef, SessionArtifact, SessionDescriptor, SessionNamespace, SourceProvider,
};

/// Default cap on decompressed bytes per session (decompression-bomb guard).
const DEFAULT_MAX_DECOMPRESSED: u64 = 1 << 32; // 4 GiB

/// Default cap on compressed input bytes per session file.
const DEFAULT_MAX_COMPRESSED: u64 = 1 << 30; // 1 GiB

/// zstd window_log_max: a PRACTICAL decoder-memory guard (2^27 = 128 MiB —
/// zstd's own default refusal threshold), not the 2 GiB format ceiling.
const WINDOW_LOG_MAX: u32 = 27;

/// Encode a path's native bytes as an injective, reversible locator string.
///
/// Distinct paths MUST produce distinct locators even when their lossy
/// display strings collide (non-UTF-8 components render as replacement
/// characters). Unix: printable ASCII passes through, `%` and every other
/// byte percent-encode. Windows paths are Unicode; the same escaping is
/// applied to their UTF-8 form.
fn encode_locator(path: &Path) -> String {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        let bytes = path.as_os_str().as_bytes();
        let mut out = String::with_capacity(bytes.len());
        for &b in bytes {
            match b {
                b'%' => out.push_str("%25"),
                0x20..=0x7e => out.push(b as char),
                other => out.push_str(&format!("%{other:02X}")),
            }
        }
        out
    }
    #[cfg(windows)]
    {
        // Windows paths are u16 units and may contain ill-formed UTF-16
        // (unpaired surrogates); encode_wide round-trips them losslessly —
        // to_string_lossy would collapse distinct surrogates into identical
        // replacement characters.
        use std::os::windows::ffi::OsStrExt;
        let mut out = String::new();
        for unit in path.as_os_str().encode_wide() {
            match unit {
                0x25 => out.push_str("%25"),
                0x20..=0x7e => out.push(unit as u8 as char),
                other => out.push_str(&format!("%u{other:04X}")),
            }
        }
        out
    }
    #[cfg(not(any(unix, windows)))]
    {
        // Fallback for exotic targets: escape the lossy form's bytes (still
        // deterministic; injectivity is only guaranteed on unix/windows).
        let mut out = String::new();
        for b in path.to_string_lossy().bytes() {
            match b {
                b'%' => out.push_str("%25"),
                0x20..=0x7e => out.push(b as char),
                other => out.push_str(&format!("%{other:02X}")),
            }
        }
        out
    }
}

/// Known rollout vocabularies at rust-v0.144.5 — the drift baseline. These
/// are NOT load-bearing for parsing (which is shape-based and preserves
/// everything); they exist so [`CodexProvider::drift_report`] can tell
/// "known vocabulary" from "schema drift".
const KNOWN_ENVELOPE_TYPES: [&str; 8] = [
    "session_meta",
    "response_item",
    "event_msg",
    "turn_context",
    "compacted",
    "world_state",
    "inter_agent_communication",
    "inter_agent_communication_metadata",
];
const KNOWN_RESPONSE_ITEM_TYPES: [&str; 16] = [
    "message",
    "agent_message",
    "reasoning",
    "local_shell_call",
    "function_call",
    "function_call_output",
    "custom_tool_call",
    "custom_tool_call_output",
    "tool_search_call",
    "tool_search_output",
    "web_search_call",
    "image_generation_call",
    "compaction",
    "compaction_summary",
    "context_compaction",
    "ghost_snapshot",
];
const KNOWN_EVENT_MSG_TYPES: [&str; 23] = [
    "token_count",
    "user_message",
    "agent_message",
    "agent_reasoning",
    "agent_reasoning_raw_content",
    "turn_started",
    "turn_complete",
    "turn_aborted",
    "thread_rolled_back",
    "thread_goal_updated",
    "thread_settings_applied",
    "context_compacted",
    "entered_review_mode",
    "exited_review_mode",
    "patch_apply_end",
    "mcp_tool_call_end",
    "web_search_end",
    "image_generation_end",
    "sub_agent_activity",
    "exec_command_end",
    "task_started",
    "task_complete",
    "item_completed",
];

/// Native-vocabulary drift report for a Codex corpus.
///
/// Inspects the envelope/payload vocabulary DIRECTLY — it deliberately does
/// not read `ParsedSession` diagnostics, whose `unknown` counts are the
/// intentional B1 preserved-Unknown representation, not drift. CLI `doctor`
/// surfacing and a provider-neutral diagnostics hook are explicitly Phase
/// B2 scope (recorded in the design doc); this is the analysis capability.
#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct CodexDriftReport {
    /// Envelope-era sessions scanned.
    pub sessions: usize,
    /// Legacy pre-envelope sessions (inventoried, not scanned).
    pub legacy_sessions: usize,
    /// Sessions whose preferred artifact could not be opened/sniffed —
    /// counted, never allowed to abort the rest of the report.
    pub unreadable_sessions: usize,
    /// Total records seen.
    pub records: u64,
    /// Records that failed to decode/parse mid-file (real damage/drift).
    pub unparseable: u64,
    /// Unterminated final records of active sessions — TRANSIENT, per the
    /// acceptance invariant these are never permanent drift findings.
    pub active_tails: u64,
    /// Records whose envelope `type` is missing or not a string.
    pub missing_type_discriminators: u64,
    /// Envelope type counts.
    pub envelope_types: BTreeMap<String, u64>,
    /// Envelope types outside the rust-v0.144.5 vocabulary (drift).
    pub unknown_envelope_types: BTreeMap<String, u64>,
    /// response_item payload type counts.
    pub response_item_types: BTreeMap<String, u64>,
    /// response_item payload types outside the known vocabulary (drift).
    pub unknown_response_item_types: BTreeMap<String, u64>,
    /// event_msg payload type counts.
    pub event_msg_types: BTreeMap<String, u64>,
    /// event_msg payload types outside the known vocabulary (drift).
    pub unknown_event_msg_types: BTreeMap<String, u64>,
    /// Unknown NESTED field paths (e.g.
    /// `event_msg/token_count/nested_future_field`), evaluated against
    /// curated per-payload-type key baselines; types without a baseline are
    /// not evaluated (absence of a baseline is not drift) and are counted in
    /// `unbaselined_payload_types` so partial coverage is machine-visible —
    /// "zero unknown paths" alone must never be read as full coverage.
    pub unknown_field_paths: BTreeMap<String, u64>,
    /// Records whose payload keys WERE checked against a baseline.
    pub field_schema_checked_records: u64,
    /// Payload variants seen but not baselined ("kind/type" -> count):
    /// the machine-visible complement of `field_schema_checked_records`.
    pub unbaselined_payload_types: BTreeMap<String, u64>,
    /// response_item/event_msg records whose payload `type` discriminator is
    /// missing or not a string (the envelope counter does not cover these).
    pub missing_payload_discriminators: u64,
    /// Distinct-key insertions dropped because a vocabulary map reached its
    /// cardinality cap (round-16/17 security guardrail: keys are
    /// attacker-controlled strings, capped DURING collection).
    pub vocabulary_keys_dropped: u64,
    /// Keys truncated to the length cap before storage (same guardrail).
    pub vocabulary_keys_truncated: u64,
    /// reasoning response items seen.
    pub reasoning_items: u64,
    /// reasoning items carrying a non-empty plaintext summary.
    pub reasoning_with_summary: u64,
    /// Era-bucketed availability: month ("YYYY-MM") -> (reasoning items,
    /// items with summary). The corpus-wide ratio alone hides era collapses
    /// (85-99% through 2026-03, 0% from 2026-04 can still aggregate to
    /// ~89%) — the exact error the original research made.
    pub reasoning_by_month: BTreeMap<String, (u64, u64)>,
}

/// OpenAI Codex CLI sessions behind the provider seam.
pub struct CodexProvider {
    codex_home: PathBuf,
    /// Cap on decompressed bytes per compressed session file.
    max_decompressed: u64,
    /// Cap on compressed input bytes per session file.
    max_compressed: u64,
}

/// What the first record of a rollout file says about its format family.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FormatFamily {
    /// Envelope era (Codex ≥0.32.0): `{timestamp, type, payload}` per line.
    Envelope,
    /// Pre-envelope era (≤0.31.0): bare meta/ResponseItem lines.
    Legacy,
    /// Empty or unreadable first record.
    Undetermined,
}

/// A fork's copied-history interval inside the CHILD rollout. Ordinal zero
/// is the child's own `session_meta`; copied parent records begin at one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct CopiedHistoryRange {
    first: u64,
    last: u64,
}

impl CopiedHistoryRange {
    fn contains(self, ordinal: u64) -> bool {
        (self.first..=self.last).contains(&ordinal)
    }
}

fn session_meta_payload(value: &serde_json::Value) -> Option<&serde_json::Value> {
    (value.get("type").and_then(serde_json::Value::as_str) == Some("session_meta"))
        .then(|| value.get("payload"))
        .flatten()
}

fn valid_thread_id(value: &str) -> bool {
    value.len() == 36
        && value.bytes().enumerate().all(|(i, b)| match i {
            8 | 13 | 18 | 23 => b == b'-',
            _ => b.is_ascii_hexdigit(),
        })
}

/// Strict old-format fork heuristic, derived from all 16 observed forks:
/// physical record zero is the child's metadata and record one is a copied
/// metadata record whose different id names the parent. A later second meta,
/// a same-id meta, or a first meta that disagrees with the filename is not a
/// fork signal.
fn embedded_fork_parent(
    key: &LogicalSessionKey,
    first: &serde_json::Value,
    second: &serde_json::Value,
) -> Option<String> {
    let child = session_meta_payload(first)?;
    let parent = session_meta_payload(second)?;
    (child.get("id").and_then(serde_json::Value::as_str) == Some(&key.native_id)).then_some(())?;
    let parent_id = parent.get("id").and_then(serde_json::Value::as_str)?;
    (parent_id != key.native_id && valid_thread_id(parent_id)).then(|| parent_id.to_string())
}

/// Copied rollout envelopes retain every field except their outer write
/// timestamp. Compare the complete remaining object, not just type/payload,
/// so future envelope fields cannot be silently ignored by the proof.
fn copied_record_eq(a: &serde_json::Value, b: &serde_json::Value) -> bool {
    let (Some(a), Some(b)) = (a.as_object(), b.as_object()) else {
        return false;
    };
    if !a.get("timestamp").is_some_and(serde_json::Value::is_string)
        || !b.get("timestamp").is_some_and(serde_json::Value::is_string)
    {
        return false;
    }
    let count_without_timestamp = |m: &serde_json::Map<String, serde_json::Value>| {
        m.keys().filter(|k| k.as_str() != "timestamp").count()
    };
    count_without_timestamp(a) == count_without_timestamp(b)
        && a.iter()
            .filter(|(k, _)| k.as_str() != "timestamp")
            .all(|(k, v)| b.get(k) == Some(v))
}

/// Security caps for drift-vocabulary maps (round-16/17): every key stored
/// in the report's maps is an attacker-controlled string read from native
/// files, so distinct-key cardinality and key length are capped DURING
/// collection (not at rendering) and control characters are escaped so the
/// report can never carry terminal/structured-output injection sequences.
const MAX_VOCAB_KEYS: usize = 64;
const MAX_VOCAB_KEY_LEN: usize = 120;

fn sanitize_vocab_key(raw: &str, truncated: &mut u64) -> String {
    // The cap applies to the ESCAPED representation (escape_debug expands a
    // control character up to ~10 chars) and INCLUDES the truncation
    // marker: the complete stored key never exceeds MAX_VOCAB_KEY_LEN
    // characters (round-19 — the previous version permitted cap+1).
    let mut out = String::new();
    let mut len = 0usize;
    let mut needs_marker = false;
    for c in raw.chars() {
        let piece: String = if c.is_control() {
            c.escape_debug().collect()
        } else {
            c.to_string()
        };
        let piece_len = piece.chars().count();
        if len + piece_len > MAX_VOCAB_KEY_LEN {
            needs_marker = true;
            break;
        }
        out.push_str(&piece);
        len += piece_len;
    }
    if needs_marker {
        while len > MAX_VOCAB_KEY_LEN - 1 {
            out.pop();
            len -= 1;
        }
        out.push('…');
        *truncated += 1;
    }
    out
}

fn bump_vocab(map: &mut BTreeMap<String, u64>, dropped: &mut u64, truncated: &mut u64, raw: &str) {
    let key = sanitize_vocab_key(raw, truncated);
    if let Some(count) = map.get_mut(&key) {
        *count += 1;
    } else if map.len() >= MAX_VOCAB_KEYS {
        *dropped += 1;
    } else {
        map.insert(key, 1);
    }
}

impl CodexProvider {
    /// Wrap a Codex home directory (`~/.codex` by default).
    pub fn new(codex_home: impl Into<PathBuf>) -> Self {
        CodexProvider {
            codex_home: codex_home.into(),
            max_decompressed: DEFAULT_MAX_DECOMPRESSED,
            max_compressed: DEFAULT_MAX_COMPRESSED,
        }
    }

    /// Discover the Codex home from `$CODEX_HOME` or `~/.codex`.
    pub fn discover() -> Result<Self, ProviderError> {
        let home = std::env::var_os("CODEX_HOME")
            .map(PathBuf::from)
            .or_else(|| crate::discovery::home_directory().map(|h| h.join(".codex")))
            .ok_or_else(|| ProviderError::Other("cannot determine Codex home".into()))?;
        Ok(Self::new(home))
    }

    /// The Codex home directory this provider reads.
    pub fn codex_home(&self) -> &Path {
        &self.codex_home
    }

    /// Tighten both safety caps to at most `limit` (the surface's global
    /// `--max-file-size`). Never loosens the defaults; the caps are parse
    /// cache token inputs, so a changed limit changes the token.
    #[must_use]
    pub fn tighten_limits(mut self, limit: u64) -> Self {
        // Zero means "no additional user cap" — the guards treat 0 as
        // unlimited, so min()-ing it in would DISABLE the default safety
        // ceilings (round-19 blocker 2). Defense in depth with the
        // registry-level normalization.
        if limit == 0 {
            return self;
        }
        self.max_compressed = self.max_compressed.min(limit);
        self.max_decompressed = self.max_decompressed.min(limit);
        self
    }

    /// Configure the decompressed-output cap (bytes).
    #[must_use]
    pub fn with_max_decompressed(mut self, max: u64) -> Self {
        self.max_decompressed = max;
        self
    }

    /// Configure the compressed-input cap (bytes).
    #[must_use]
    pub fn with_max_compressed(mut self, max: u64) -> Self {
        self.max_compressed = max;
        self
    }

    fn key_for(&self, thread_id: &str) -> LogicalSessionKey {
        LogicalSessionKey {
            provider: ProviderId::codex(),
            namespace: SessionNamespace::global(),
            native_id: thread_id.to_string(),
        }
    }

    /// Parse `rollout-<ts>-<uuid>` from a rollout file stem. The timestamp
    /// segment is `YYYY-MM-DDThh-mm-ss` (local time, `-` for `:`); the uuid
    /// is the thread id (the trailing 36 chars).
    fn thread_id_from_stem(stem: &str) -> Option<&str> {
        let rest = stem.strip_prefix("rollout-")?;
        if rest.len() < 36 {
            return None;
        }
        let uuid = &rest[rest.len() - 36..];
        valid_thread_id(uuid).then_some(uuid)
    }

    fn artifact_for(&self, path: &Path, archived: bool) -> SessionArtifact {
        let (mtime, len) = std::fs::metadata(path)
            .map(|m| {
                (
                    m.modified()
                        .ok()
                        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                        .map(|d| d.as_nanos())
                        .unwrap_or(0),
                    m.len(),
                )
            })
            .unwrap_or((0, 0));
        let compressed = path.extension().and_then(|e| e.to_str()) == Some("zst");
        SessionArtifact {
            snapshot: ArtifactSnapshot {
                id: ArtifactId {
                    provider_instance: encode_locator(&self.codex_home),
                    locator: encode_locator(path),
                },
                revision: ArtifactRevision(format!("mtime={mtime};len={len}")),
            },
            form: if compressed {
                ArtifactForm::CompressedFile
            } else {
                ArtifactForm::PlainFile
            },
            archived,
        }
    }

    /// Walk one rollout tree (`sessions/` or `archived_sessions/`).
    fn walk_tree(
        &self,
        root: &Path,
        archived: bool,
        out: &mut BTreeMap<LogicalSessionKey, Vec<(SessionArtifact, PathBuf)>>,
    ) -> Result<(), ProviderError> {
        if !root.exists() {
            return Ok(());
        }
        let mut stack = vec![root.to_path_buf()];
        while let Some(dir) = stack.pop() {
            for entry in std::fs::read_dir(&dir)? {
                let entry = entry?;
                // Symlink policy: the tree ROOT itself may be a symlink
                // (relocated storage is legitimate); within the tree nothing
                // is followed, and only regular files are accepted — a
                // matching FIFO/socket/device node could block indefinitely
                // on open.
                let file_type = entry.file_type()?;
                if file_type.is_symlink() {
                    continue;
                }
                let path = entry.path();
                if file_type.is_dir() {
                    stack.push(path);
                    continue;
                }
                if !file_type.is_file() {
                    continue;
                }
                let name = match path.file_name().and_then(|n| n.to_str()) {
                    Some(n) => n,
                    None => continue,
                };
                let stem = name
                    .strip_suffix(".jsonl.zst")
                    .or_else(|| name.strip_suffix(".jsonl"));
                let Some(stem) = stem else { continue };
                let Some(thread_id) = Self::thread_id_from_stem(stem) else {
                    continue;
                };
                let key = self.key_for(thread_id);
                let artifact = self.artifact_for(&path, archived);
                let slot = out.entry(key).or_default();
                if !slot
                    .iter()
                    .any(|(a, _)| a.snapshot.id == artifact.snapshot.id)
                {
                    slot.push((artifact, path));
                }
            }
        }
        Ok(())
    }

    /// Full inventory: descriptors (artifacts sorted by stable identity, so
    /// manifests and future cache tokens are deterministic regardless of
    /// filesystem read order) plus the authoritative artifact-to-path map.
    /// Paths are preserved as `PathBuf` — locator strings are display forms
    /// and cannot round-trip a non-UTF-8 `CODEX_HOME`.
    #[allow(clippy::type_complexity)]
    fn inventory(
        &self,
    ) -> Result<(Vec<SessionDescriptor>, BTreeMap<ArtifactId, PathBuf>), ProviderError> {
        let mut grouped: BTreeMap<LogicalSessionKey, Vec<(SessionArtifact, PathBuf)>> =
            BTreeMap::new();
        self.walk_tree(&self.codex_home.join("sessions"), false, &mut grouped)?;
        self.walk_tree(
            &self.codex_home.join("archived_sessions"),
            true,
            &mut grouped,
        )?;
        let mut paths = BTreeMap::new();
        let descriptors = grouped
            .into_iter()
            .map(|(key, mut pairs)| {
                pairs.sort_by(|(a, _), (b, _)| a.snapshot.id.cmp(&b.snapshot.id));
                let artifacts = pairs
                    .into_iter()
                    .map(|(artifact, path)| {
                        paths.insert(artifact.snapshot.id.clone(), path);
                        artifact
                    })
                    .collect();
                SessionDescriptor { key, artifacts }
            })
            .collect();
        Ok((descriptors, paths))
    }

    fn descriptors(&self) -> Result<Vec<SessionDescriptor>, ProviderError> {
        Ok(self.inventory()?.0)
    }

    fn resolve(
        &self,
        key: &LogicalSessionKey,
    ) -> Result<(SessionDescriptor, PathBuf), ProviderError> {
        if key.provider != ProviderId::codex() || key.namespace != SessionNamespace::global() {
            return Err(ProviderError::NotFound(key.to_string()));
        }
        let (descriptors, paths) = self.inventory()?;
        let descriptor = descriptors
            .into_iter()
            .find(|d| d.key == *key)
            .ok_or_else(|| ProviderError::NotFound(key.to_string()))?;
        let preferred = descriptor
            .preferred_artifact()
            .ok_or_else(|| ProviderError::Other(format!("descriptor {key} has no artifacts")))?;
        let path = paths
            .get(&preferred.snapshot.id)
            .cloned()
            .ok_or_else(|| ProviderError::NotFound(key.to_string()))?;
        Ok((descriptor, path))
    }

    /// Open a rollout artifact as a line reader: plain passthrough, or a
    /// streaming zstd decode guarded by `window_log_max` and the
    /// decompressed-output cap. Never buffers the whole file.
    fn open_records(&self, path: &Path) -> Result<Box<dyn BufRead>, ProviderError> {
        let file = File::open(path)?;
        if path.extension().and_then(|e| e.to_str()) == Some("zst") {
            let compressed_len = std::fs::metadata(path)?.len();
            if self.max_compressed > 0 && compressed_len > self.max_compressed {
                return Err(ProviderError::Other(format!(
                    "compressed input {} exceeds max_compressed ({compressed_len} > {} bytes)",
                    path.display(),
                    self.max_compressed
                )));
            }
            let mut decoder = zstd::stream::read::Decoder::new(file)
                .map_err(|e| ProviderError::Other(format!("zstd init: {e}")))?;
            decoder
                .window_log_max(WINDOW_LOG_MAX)
                .map_err(|e| ProviderError::Other(format!("zstd window_log_max: {e}")))?;
            Ok(Box::new(BufReader::new(LimitedReader {
                inner: decoder,
                remaining: self.max_decompressed,
            })))
        } else {
            // The decompressed-output cap is a general record-stream cap:
            // plain files are checked against it too, so a surface-supplied
            // `--max-file-size` (which tightens it) applies consistently to
            // every artifact form instead of silently skipping plain files
            // (round-18 blocker 4).
            let plain_len = std::fs::metadata(path)?.len();
            if self.max_decompressed > 0 && plain_len > self.max_decompressed {
                return Err(ProviderError::Other(format!(
                    "record stream {} exceeds the size limit ({plain_len} > {} bytes)",
                    path.display(),
                    self.max_decompressed
                )));
            }
            Ok(Box::new(BufReader::new(file)))
        }
    }

    /// Read the first `limit` PHYSICAL records. Lineage metadata is
    /// position-sensitive, so blank or damaged lines stop the read instead
    /// of being skipped and accidentally promoting a later record.
    fn initial_records(&self, path: &Path, limit: usize) -> Vec<serde_json::Value> {
        let Ok(mut reader) = self.open_records(path) else {
            return Vec::new();
        };
        let mut out = Vec::with_capacity(limit);
        let mut line = Vec::new();
        while out.len() < limit {
            line.clear();
            match reader.read_until(b'\n', &mut line) {
                Ok(0) | Err(_) => break,
                Ok(_) => match serde_json::from_slice(&line) {
                    Ok(value) => out.push(value),
                    Err(_) => break,
                },
            }
        }
        out
    }

    /// Prove the maximal prefix copied from the fork parent. The old rollout
    /// format rewrites outer envelope timestamps, so equality deliberately
    /// ignores ONLY that field. Missing/corrupt parents yield no inherited
    /// range: a dangling edge remains useful, but activity is never guessed.
    fn copied_history_range(
        &self,
        key: &LogicalSessionKey,
        child_records: &[(RecordRef, serde_json::Value)],
    ) -> Option<CopiedHistoryRange> {
        let [(first_ref, first), (second_ref, second), ..] = child_records else {
            return None;
        };
        if first_ref.ordinal != 0 || second_ref.ordinal != 1 {
            return None;
        }
        let parent_id = embedded_fork_parent(key, first, second)?;
        let (_, parent_path) = self.resolve(&self.key_for(&parent_id)).ok()?;
        let mut parent_reader = self.open_records(&parent_path).ok()?;
        let mut parent_line = Vec::new();
        let mut expected_child_ordinal = 1_u64;
        let mut last = None;

        for (child_ref, child) in child_records.iter().skip(1) {
            if child_ref.ordinal != expected_child_ordinal {
                break;
            }
            parent_line.clear();
            match parent_reader.read_until(b'\n', &mut parent_line) {
                Ok(0) | Err(_) => break,
                Ok(_) => {}
            }
            let Ok(parent) = serde_json::from_slice::<serde_json::Value>(&parent_line) else {
                break;
            };
            if !copied_record_eq(&parent, child) {
                break;
            }
            last = Some(child_ref.ordinal);
            expected_child_ordinal = expected_child_ordinal.saturating_add(1);
        }

        last.map(|last| CopiedHistoryRange { first: 1, last })
    }

    /// Apply inherited-history semantics after normalization. An entry is
    /// inherited only when ALL of its producing origins are inside the
    /// independently proven copied prefix. This prevents a mixed-boundary
    /// entry from hiding genuinely new activity.
    fn mark_inherited_history(
        range: CopiedHistoryRange,
        artifact: &ArtifactId,
        normalized: &mut super::codex_normalize::NormalizeOutput,
    ) {
        for entry in &normalized.entries {
            let inherited = normalized
                .entry_origins
                .get(&entry.id)
                .is_some_and(|origins| {
                    !origins.is_empty()
                        && origins
                            .iter()
                            .all(|r| r.artifact == *artifact && range.contains(r.ordinal))
                });
            if inherited {
                normalized
                    .semantics
                    .entry(entry.id.clone())
                    .or_default()
                    .activity = ActivityKind::InheritedHistory;
            }
        }
    }

    /// Sniff a rollout file's format family from its first non-blank record.
    ///
    /// Detection is by envelope SHAPE (string `timestamp`, string `type`, a
    /// `payload` member), independent of the known type vocabulary — a
    /// future envelope whose first record carries a new type must not be
    /// misclassified as legacy. Explicit `Undetermined` policy: empty or
    /// undecodable-first-record files proceed through envelope parsing,
    /// where every record still lands as a preserved-Unknown or Unparseable
    /// disposition — nothing is silently dropped either way.
    pub fn sniff_format(&self, path: &Path) -> Result<FormatFamily, ProviderError> {
        let mut reader = self.open_records(path)?;
        let mut line = Vec::new();
        loop {
            line.clear();
            let n = match reader.read_until(b'\n', &mut line) {
                Ok(n) => n,
                Err(_) => return Ok(FormatFamily::Undetermined),
            };
            if n == 0 {
                return Ok(FormatFamily::Undetermined);
            }
            if line.iter().all(|b| b.is_ascii_whitespace()) {
                continue;
            }
            let Ok(value) = serde_json::from_slice::<serde_json::Value>(&line) else {
                return Ok(FormatFamily::Undetermined);
            };
            let envelope_shape = value.get("timestamp").is_some_and(|t| t.is_string())
                && value.get("type").is_some_and(|t| t.is_string())
                && value.get("payload").is_some();
            return Ok(if envelope_shape {
                FormatFamily::Envelope
            } else {
                FormatFamily::Legacy
            });
        }
    }
}

/// Reader guard that fails once more than `remaining` bytes have been
/// produced — the decompression-bomb backstop.
struct LimitedReader<R> {
    inner: R,
    remaining: u64,
}

impl<R: Read> Read for LimitedReader<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if self.remaining == 0 {
            // A stream whose decompressed size is EXACTLY the limit is
            // valid: probe one byte for EOF before declaring the limit
            // crossed.
            let mut probe = [0u8; 1];
            return match self.inner.read(&mut probe)? {
                0 => Ok(0),
                _ => Err(std::io::Error::other(
                    "decompressed output exceeds the configured limit",
                )),
            };
        }
        let cap = usize::try_from(self.remaining.min(buf.len() as u64)).unwrap_or(usize::MAX);
        let n = self.inner.read(&mut buf[..cap])?;
        self.remaining -= n as u64;
        Ok(n)
    }
}

impl CodexProvider {
    /// Curated known-key baselines for nested-field drift, keyed by
    /// (envelope kind, payload type). Only listed types are evaluated.
    fn payload_key_baseline(kind: &str, payload_type: &str) -> Option<&'static [&'static str]> {
        match (kind, payload_type) {
            ("event_msg", "token_count") => Some(&["type", "info", "rate_limits"]),
            ("event_msg", "user_message") => Some(&[
                "type",
                "message",
                "images",
                "local_images",
                "text_elements",
                "kind",
            ]),
            // "memory_citation" and "phase" discovered by the corpus run
            // after this baseline landed (2,992 occurrences) — same
            // instrument-discovery provenance as the metadata fields.
            ("event_msg", "agent_message") => {
                Some(&["type", "message", "memory_citation", "phase"])
            }
            ("event_msg", "agent_reasoning") => Some(&["type", "text"]),
            ("event_msg", "agent_reasoning_raw_content") => Some(&["type", "text"]),
            ("event_msg", "thread_rolled_back") => Some(&["type", "num_turns"]),
            ("response_item", "custom_tool_call") => Some(&[
                "type",
                "id",
                "status",
                "call_id",
                "name",
                "input",
                "metadata",
                "internal_chat_message_metadata_passthrough",
            ]),
            ("response_item", "custom_tool_call_output") => Some(&[
                "type",
                "id",
                "call_id",
                "output",
                "metadata",
                "internal_chat_message_metadata_passthrough",
            ]),
            ("response_item", "web_search_call") => Some(&[
                "type",
                "id",
                "status",
                "action",
                "metadata",
                "internal_chat_message_metadata_passthrough",
            ]),
            // NOTE: "metadata" and reasoning's passthrough field were
            // DISCOVERED by this instrument's first real-corpus run (2,339
            // occurrences) — vocabulary the rust-v0.144.5 source research
            // missed. Absorbed into the baselines with that provenance.
            ("response_item", "reasoning") => Some(&[
                "type",
                "id",
                "summary",
                "content",
                "encrypted_content",
                "metadata",
                "internal_chat_message_metadata_passthrough",
            ]),
            ("response_item", "message") => Some(&[
                "type",
                "id",
                "role",
                "content",
                "phase",
                "metadata",
                "internal_chat_message_metadata_passthrough",
            ]),
            ("response_item", "function_call") => Some(&[
                "type",
                "id",
                "name",
                "namespace",
                "arguments",
                "call_id",
                "metadata",
                "internal_chat_message_metadata_passthrough",
            ]),
            ("response_item", "function_call_output") => Some(&[
                "type",
                "id",
                "call_id",
                "output",
                "metadata",
                "internal_chat_message_metadata_passthrough",
            ]),
            _ => None,
        }
    }

    /// Scan the corpus's native vocabulary for schema drift and
    /// reasoning-summary availability. Streams every envelope-era session's
    /// preferred artifact; legacy sessions are counted, not scanned; a
    /// session that cannot be opened/sniffed is counted as unreadable and
    /// never suppresses the rest of the report.
    pub fn drift_report(&self) -> Result<CodexDriftReport, ProviderError> {
        let (descriptors, paths) = self.inventory()?;
        let mut report = CodexDriftReport::default();
        for descriptor in descriptors {
            let Some(preferred) = descriptor.preferred_artifact() else {
                continue;
            };
            let Some(path) = paths.get(&preferred.snapshot.id) else {
                continue;
            };
            match self.sniff_format(path) {
                Ok(FormatFamily::Legacy) => {
                    report.legacy_sessions += 1;
                    continue;
                }
                Ok(_) => {}
                Err(_) => {
                    report.unreadable_sessions += 1;
                    continue;
                }
            }
            let mut reader = match self.open_records(path) {
                Ok(r) => r,
                Err(_) => {
                    report.unreadable_sessions += 1;
                    continue;
                }
            };
            report.sessions += 1;
            let preferred_archived = preferred.archived;
            let mut buf: Vec<u8> = Vec::new();
            loop {
                buf.clear();
                match reader.read_until(b'\n', &mut buf) {
                    Ok(0) => break,
                    Ok(_) => {}
                    Err(_) => {
                        report.unparseable += 1;
                        break;
                    }
                }
                if buf.iter().all(|b| b.is_ascii_whitespace()) {
                    continue;
                }
                report.records += 1;
                // read_until only omits the trailing newline at EOF: an
                // unterminated final record of an ACTIVE artifact is a
                // transient tail. An ARCHIVED file is finalized — the same
                // damage there is permanent corruption, not a tail.
                let is_final_unterminated = !buf.ends_with(b"\n");
                let Ok(value) = serde_json::from_slice::<serde_json::Value>(&buf) else {
                    if is_final_unterminated && !preferred_archived {
                        report.active_tails += 1;
                    } else {
                        report.unparseable += 1;
                    }
                    continue;
                };
                let Some(kind) = value.get("type").and_then(|t| t.as_str()) else {
                    report.missing_type_discriminators += 1;
                    continue;
                };
                let kind = kind.to_string();
                bump_vocab(
                    &mut report.envelope_types,
                    &mut report.vocabulary_keys_dropped,
                    &mut report.vocabulary_keys_truncated,
                    &kind,
                );
                if !KNOWN_ENVELOPE_TYPES.contains(&kind.as_str()) {
                    bump_vocab(
                        &mut report.unknown_envelope_types,
                        &mut report.vocabulary_keys_dropped,
                        &mut report.vocabulary_keys_truncated,
                        &kind,
                    );
                }
                // Envelope-level unknown keys.
                if let Some(obj) = value.as_object() {
                    for k in obj.keys() {
                        if !matches!(k.as_str(), "timestamp" | "type" | "payload") {
                            bump_vocab(
                                &mut report.unknown_field_paths,
                                &mut report.vocabulary_keys_dropped,
                                &mut report.vocabulary_keys_truncated,
                                &format!("{kind}/$envelope/{k}"),
                            );
                        }
                    }
                }
                let month = value
                    .get("timestamp")
                    .and_then(|t| t.as_str())
                    .map(|t| t.chars().take(7).collect::<String>())
                    .unwrap_or_else(|| "unknown".into());
                let payload = value.get("payload");
                let requires_payload_type = matches!(kind.as_str(), "response_item" | "event_msg");
                let payload_type = payload
                    .and_then(|p| p.get("type"))
                    .and_then(|t| t.as_str())
                    .map(str::to_string);
                if requires_payload_type && payload_type.is_none() {
                    report.missing_payload_discriminators += 1;
                }
                if let (Some(payload), Some(pt)) = (payload, payload_type.as_deref()) {
                    // Nested-field drift against curated baselines, with
                    // coverage made machine-visible either way.
                    if let Some(baseline) = Self::payload_key_baseline(&kind, pt) {
                        report.field_schema_checked_records += 1;
                        if let Some(obj) = payload.as_object() {
                            for k in obj.keys() {
                                if !baseline.contains(&k.as_str()) {
                                    bump_vocab(
                                        &mut report.unknown_field_paths,
                                        &mut report.vocabulary_keys_dropped,
                                        &mut report.vocabulary_keys_truncated,
                                        &format!("{kind}/{pt}/{k}"),
                                    );
                                }
                            }
                        }
                    } else if requires_payload_type {
                        bump_vocab(
                            &mut report.unbaselined_payload_types,
                            &mut report.vocabulary_keys_dropped,
                            &mut report.vocabulary_keys_truncated,
                            &format!("{kind}/{pt}"),
                        );
                    }
                    match kind.as_str() {
                        "response_item" => {
                            bump_vocab(
                                &mut report.response_item_types,
                                &mut report.vocabulary_keys_dropped,
                                &mut report.vocabulary_keys_truncated,
                                pt,
                            );
                            if !KNOWN_RESPONSE_ITEM_TYPES.contains(&pt) {
                                bump_vocab(
                                    &mut report.unknown_response_item_types,
                                    &mut report.vocabulary_keys_dropped,
                                    &mut report.vocabulary_keys_truncated,
                                    pt,
                                );
                            }
                            if pt == "reasoning" {
                                report.reasoning_items += 1;
                                let has_summary = payload["summary"].as_array().is_some_and(|sm| {
                                    sm.iter().any(|item| {
                                        item.get("text")
                                            .and_then(|t| t.as_str())
                                            .is_some_and(|t| !t.trim().is_empty())
                                    })
                                });
                                let month_key = sanitize_vocab_key(
                                    &month,
                                    &mut report.vocabulary_keys_truncated,
                                );
                                if report.reasoning_by_month.contains_key(&month_key)
                                    || report.reasoning_by_month.len() < MAX_VOCAB_KEYS
                                {
                                    let bucket =
                                        report.reasoning_by_month.entry(month_key).or_default();
                                    bucket.0 += 1;
                                    if has_summary {
                                        bucket.1 += 1;
                                    }
                                } else {
                                    report.vocabulary_keys_dropped += 1;
                                }
                                if has_summary {
                                    report.reasoning_with_summary += 1;
                                }
                            }
                        }
                        "event_msg" => {
                            bump_vocab(
                                &mut report.event_msg_types,
                                &mut report.vocabulary_keys_dropped,
                                &mut report.vocabulary_keys_truncated,
                                pt,
                            );
                            if !KNOWN_EVENT_MSG_TYPES.contains(&pt) {
                                bump_vocab(
                                    &mut report.unknown_event_msg_types,
                                    &mut report.vocabulary_keys_dropped,
                                    &mut report.vocabulary_keys_truncated,
                                    pt,
                                );
                            }
                        }
                        _ => {}
                    }
                }
            }
        }
        Ok(report)
    }
}

impl SourceProvider for CodexProvider {
    fn id(&self) -> ProviderId {
        ProviderId::codex()
    }

    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities {
            native_export: true,
            raw_jsonl: true,
            semantic_annotations: true,
        }
    }

    fn sessions(&self) -> Result<Vec<SessionDescriptor>, ProviderError> {
        self.descriptors()
    }

    fn diagnostics(&self) -> Result<Option<serde_json::Value>, ProviderError> {
        let report = self.drift_report()?;
        serde_json::to_value(&report)
            .map(Some)
            .map_err(|e| ProviderError::Other(format!("diagnostics serialization: {e}")))
    }

    fn parse_cache_token(&self, key: &LogicalSessionKey) -> Result<String, ProviderError> {
        let (descriptor, path) = self.resolve(key)?;
        // Inherited-history classification depends on the embedded parent
        // prefix. Include that parent's descriptor state in the CHILD token
        // so a parent appearing, disappearing, or changing cannot serve a
        // stale cached activity classification.
        let initial = self.initial_records(&path, 2);
        let parent_dependency = initial
            .first()
            .zip(initial.get(1))
            .and_then(|(first, second)| embedded_fork_parent(key, first, second))
            .map(|parent_id| {
                let state = self
                    .resolve(&self.key_for(&parent_id))
                    .map(|(parent, _)| super::descriptor_state_token(&parent))
                    .unwrap_or_else(|_| "missing".to_string());
                format!(
                    "id={}:{};state={}:{}",
                    parent_id.len(),
                    parent_id,
                    state.len(),
                    state
                )
            })
            .unwrap_or_else(|| "none".to_string());
        Ok(format!(
            "v2\x1ecodex\x1e{}\x1emax_c={}\x1emax_d={}\x1ewlog={WINDOW_LOG_MAX}\x1eparent={}:{}",
            super::descriptor_state_token(&descriptor),
            self.max_compressed,
            self.max_decompressed,
            parent_dependency.len(),
            parent_dependency
        ))
    }

    fn parse(&self, key: &LogicalSessionKey) -> Result<ParsedSession, ProviderError> {
        let (descriptor, path) = self.resolve(key)?;

        if self.sniff_format(&path)? == FormatFamily::Legacy {
            return Err(ProviderError::Unsupported {
                capability: "legacy pre-envelope rollout normalization (Codex ≤0.31.0); \
                             native/raw export remains available",
            });
        }

        // The record artifact id comes from the RESOLVED descriptor — never
        // reconstructed from a path (a lossy reconstruction made every
        // RecordRef name a nonexistent artifact under non-UTF-8 homes).
        let artifact_id = descriptor
            .preferred_artifact()
            .ok_or_else(|| ProviderError::Other(format!("descriptor {key} has no artifacts")))?
            .snapshot
            .id
            .clone();
        let mut reader = self.open_records(&path)?;
        let mut parsed_records: Vec<(RecordRef, serde_json::Value)> = Vec::new();
        let mut record_dispositions = Vec::new();
        let mut diagnostics = IngestionDiagnostics::default();

        // Byte-level records: content-level damage (invalid UTF-8, bad JSON)
        // in one record must not lose later records — only unrecoverable
        // decoder I/O errors stop the stream (a compressed stream cannot be
        // resynchronized past a bad frame).
        let mut buf: Vec<u8> = Vec::new();
        let mut ordinal: u64 = 0;
        loop {
            let record = RecordRef {
                artifact: artifact_id.clone(),
                ordinal,
            };
            buf.clear();
            let n = match reader.read_until(b'\n', &mut buf) {
                Ok(n) => n,
                Err(e) => {
                    diagnostics.unparseable += 1;
                    record_dispositions.push(RecordDisposition {
                        record,
                        outcome: RecordOutcome::Unparseable {
                            error: ParseDiagnostic {
                                message: format!("read error: {e}"),
                            },
                        },
                    });
                    break;
                }
            };
            if n == 0 {
                break;
            }
            ordinal += 1;
            if buf.iter().all(|b| b.is_ascii_whitespace()) {
                diagnostics.suppressed += 1;
                record_dispositions.push(RecordDisposition {
                    record,
                    outcome: RecordOutcome::Suppressed {
                        reason: super::SuppressionReason::Other("blank line".into()),
                    },
                });
                continue;
            }
            match serde_json::from_slice::<serde_json::Value>(&buf) {
                Ok(value) => {
                    // B3: parsed records are collected and normalized after
                    // the read loop (mapped records keep the B1 deterministic
                    // ids `(ordinal, 0)` — round-21 constraint 1).
                    parsed_records.push((record, value));
                }
                Err(e) => {
                    // Partial trailing line of an ACTIVE session is expected;
                    // any mid-file damage is also honestly unparseable.
                    diagnostics.unparseable += 1;
                    record_dispositions.push(RecordDisposition {
                        record,
                        outcome: RecordOutcome::Unparseable {
                            error: ParseDiagnostic {
                                message: e.to_string(),
                            },
                        },
                    });
                }
            }
        }

        // Old-format forks copy a prefix of the parent's rollout after the
        // child's own metadata. Prove that prefix against the available
        // parent before normalizing: its end is also a semantic window
        // boundary, preventing dedup/usage attribution from crossing from
        // inherited history into new fork work.
        let inherited_range = self.copied_history_range(key, &parsed_records);

        // Normalize the parsed stream (B3) and merge with the
        // read-level dispositions (blank/torn/unreadable) collected above.
        let mut normalized = super::codex_normalize::normalize(
            key,
            &parsed_records,
            inherited_range.map(|r| (r.first, r.last)),
        );
        if let Some(range) = inherited_range {
            Self::mark_inherited_history(range, &artifact_id, &mut normalized);
        }
        record_dispositions.extend(normalized.record_dispositions);
        record_dispositions.sort_by_key(|d| d.record.ordinal);
        diagnostics.mapped += normalized.diagnostics.mapped;
        diagnostics.suppressed += normalized.diagnostics.suppressed;
        diagnostics.unknown += normalized.diagnostics.unknown;
        diagnostics.recovered += normalized.diagnostics.recovered;
        diagnostics.unparseable += normalized.diagnostics.unparseable;

        Ok(ParsedSession {
            descriptor,
            entries: normalized.entries,
            entry_origins: normalized.entry_origins,
            record_dispositions,
            semantics: normalized.semantics,
            diagnostics,
        })
    }

    fn lineage(&self) -> Result<Vec<LineageEdge>, ProviderError> {
        // Modern fork/spawn edges live in the first session_meta. Older
        // forks in the observed corpus predate `forked_from_id`: their child
        // meta is followed immediately by a copied parent meta. Dangling
        // endpoints are kept, but positional metadata is interpreted only
        // under the strict embedded-fork heuristic above.
        let mut edges = Vec::new();
        let (descriptors, paths) = self.inventory()?;
        for descriptor in descriptors {
            let Some(preferred) = descriptor.preferred_artifact() else {
                continue;
            };
            let Some(path) = paths.get(&preferred.snapshot.id) else {
                continue;
            };
            let initial = self.initial_records(path, 2);
            let Some(first) = initial.first() else {
                continue;
            };
            let Some(payload) = session_meta_payload(first) else {
                continue;
            };
            if payload.get("id").and_then(serde_json::Value::as_str)
                != Some(&descriptor.key.native_id)
            {
                continue;
            }

            let direct_fork = payload
                .get("forked_from_id")
                .and_then(serde_json::Value::as_str)
                .filter(|id| valid_thread_id(id))
                .map(str::to_string);
            let embedded_fork = initial
                .get(1)
                .and_then(|second| embedded_fork_parent(&descriptor.key, first, second));
            if let Some(from) = direct_fork.or(embedded_fork) {
                edges.push(LineageEdge {
                    from: self.key_for(&from),
                    to: descriptor.key.clone(),
                    kind: LineageEdgeKind::Fork,
                });
            }

            // Current Codex stores spawn identity twice: a denormalized
            // `parent_thread_id`, and the authoritative typed source shape
            // `{subagent:{thread_spawn:{...}}}`. Support either without
            // inventing a parent for non-linkable `review`/`compact` sources.
            let spawn = payload.pointer("/source/subagent/thread_spawn");
            let spawn_parent = payload
                .get("parent_thread_id")
                .and_then(serde_json::Value::as_str)
                .or_else(|| {
                    spawn.and_then(|s| {
                        s.get("parent_thread_id")
                            .and_then(serde_json::Value::as_str)
                    })
                })
                .filter(|id| valid_thread_id(id));
            if let Some(parent) = spawn_parent {
                let nested_string = |field: &str| {
                    spawn
                        .and_then(|s| s.get(field))
                        .and_then(serde_json::Value::as_str)
                        .map(str::to_string)
                };
                edges.push(LineageEdge {
                    from: self.key_for(parent),
                    to: descriptor.key.clone(),
                    kind: LineageEdgeKind::Spawn {
                        tool_use_id: nested_string("tool_use_id"),
                        agent_type: payload
                            .get("agent_role")
                            .and_then(serde_json::Value::as_str)
                            .map(str::to_string)
                            .or_else(|| nested_string("agent_role")),
                        description: nested_string("task_name"),
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
        // Framed multipart bundle, same contract as the Claude provider:
        // manifest with per-artifact byte lengths, then every artifact's
        // exact bytes in manifest order.
        let (descriptor, _) = self.resolve(key)?;
        let (_, paths) = self.inventory()?;
        let artifact_path = |id: &ArtifactId| -> Result<&PathBuf, ProviderError> {
            paths
                .get(id)
                .ok_or_else(|| ProviderError::NotFound(format!("artifact {}", id.locator)))
        };
        let mut lens = Vec::with_capacity(descriptor.artifacts.len());
        for a in &descriptor.artifacts {
            lens.push(std::fs::metadata(artifact_path(&a.snapshot.id)?)?.len());
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
            let mut file = File::open(artifact_path(&a.snapshot.id)?)?;
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
        // Resolve against discovered artifacts; stream the stored PathBuf —
        // never a locator string (lossy on non-UTF-8 homes).
        let (_, paths) = self.inventory()?;
        match paths.get(artifact) {
            Some(path) => {
                let mut file = File::open(path)?;
                std::io::copy(&mut file, out)?;
                Ok(())
            }
            None => Err(ProviderError::NotFound(format!(
                "artifact {}",
                artifact.locator
            ))),
        }
    }

    fn write_raw_jsonl(
        &self,
        key: &LogicalSessionKey,
        out: &mut dyn Write,
    ) -> Result<(), ProviderError> {
        // The record stream, decompressed where applicable.
        let (_, path) = self.resolve(key)?;
        let mut reader = self.open_records(&path)?;
        std::io::copy(&mut reader, out)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::EntryId;

    const THREAD_A: &str = "019f6d4b-d408-7260-98b2-bf385f3a9763";
    const THREAD_B: &str = "019f6d11-3ce6-7662-8add-55d745876efe";
    const THREAD_LEGACY: &str = "574149a7-0712-4169-b789-67fb4742b8fc";
    const THREAD_FORK: &str = "019f7777-0000-7000-8000-000000000001";
    const THREAD_LINEAGE_PARENT: &str = "019f7777-0000-7000-8000-000000000101";
    const THREAD_EMBEDDED_FORK: &str = "019f7777-0000-7000-8000-000000000102";
    const THREAD_SPAWN: &str = "019f7777-0000-7000-8000-000000000103";
    const THREAD_NOT_A_FORK: &str = "019f7777-0000-7000-8000-000000000104";

    fn envelope_line(kind: &str, payload: serde_json::Value) -> String {
        serde_json::json!({
            "timestamp": "2026-07-16T23:39:21.575Z",
            "type": kind,
            "payload": payload,
        })
        .to_string()
    }

    fn session_a_content() -> String {
        [
            envelope_line("session_meta", serde_json::json!({"id": THREAD_A, "cwd": "/tmp/p", "cli_version": "0.144.5"})),
            envelope_line("response_item", serde_json::json!({"type": "message", "role": "user", "content": [{"type": "input_text", "text": "hi"}]})),
            envelope_line("event_msg", serde_json::json!({"type": "token_count", "info": {}})),
            envelope_line("brand_new_type_v99", serde_json::json!({"future": true})),
        ]
        .join("\n")
            + "\n"
    }

    /// Fixture home: session A plain (with an unknown envelope type and an
    /// active/truncated tail), session B as .zst ONLY, an archived plain copy
    /// of A (divergent), a legacy pre-envelope file, and a fork of A.
    fn fixture() -> (tempfile::TempDir, CodexProvider) {
        let tmp = tempfile::tempdir().unwrap();
        let day = tmp.path().join("sessions/2026/07/16");
        std::fs::create_dir_all(&day).unwrap();

        // A: plain, plus a truncated partial trailing line (active session).
        let a = format!(
            "{}{}",
            session_a_content(),
            r#"{"timestamp":"2026-07-16T23:59:59.000Z","type":"response_item","payload":{"type":"mess"#
        );
        std::fs::write(
            day.join(format!("rollout-2026-07-16T23-38-33-{THREAD_A}.jsonl")),
            &a,
        )
        .unwrap();

        // B: compressed only.
        let b = [
            envelope_line(
                "session_meta",
                serde_json::json!({"id": THREAD_B, "cwd": "/tmp/q"}),
            ),
            envelope_line(
                "response_item",
                serde_json::json!({"type": "message", "role": "assistant", "content": []}),
            ),
        ]
        .join("\n")
            + "\n";
        let zst = zstd::stream::encode_all(b.as_bytes(), 3).unwrap();
        std::fs::write(
            day.join(format!("rollout-2026-07-16T22-34-34-{THREAD_B}.jsonl.zst")),
            zst,
        )
        .unwrap();

        // Archived divergent copy of A.
        let arch = tmp.path().join("archived_sessions/2026/07/16");
        std::fs::create_dir_all(&arch).unwrap();
        std::fs::write(
            arch.join(format!("rollout-2026-07-16T23-38-33-{THREAD_A}.jsonl")),
            session_a_content(),
        )
        .unwrap();

        // Legacy pre-envelope file (bare meta line, no envelope type).
        std::fs::write(
            day.join(format!("rollout-2025-09-14T03-41-28-{THREAD_LEGACY}.jsonl")),
            format!("{{\"id\":\"{THREAD_LEGACY}\",\"timestamp\":\"2025-09-14T03:41:28.574Z\",\"instructions\":null}}\n{{\"type\":\"message\",\"role\":\"user\",\"content\":[]}}\n"),
        )
        .unwrap();

        // Fork of A (forked_from_id in meta).
        std::fs::write(
            day.join(format!("rollout-2026-07-17T00-00-00-{THREAD_FORK}.jsonl")),
            envelope_line(
                "session_meta",
                serde_json::json!({"id": THREAD_FORK, "forked_from_id": THREAD_A}),
            ) + "\n",
        )
        .unwrap();

        let p = CodexProvider::new(tmp.path());
        (tmp, p)
    }

    fn key(id: &str) -> LogicalSessionKey {
        LogicalSessionKey {
            provider: ProviderId::codex(),
            namespace: SessionNamespace::global(),
            native_id: id.into(),
        }
    }

    /// Deliberately awkward lineage fixture:
    /// - an old-format fork whose copied parent records rewrite timestamps;
    /// - usage pending at the copied-prefix boundary (must NOT attach to the
    ///   fork's first new assistant);
    /// - a current typed thread-spawn source; and
    /// - a later second session_meta that must NOT be mistaken for a fork.
    fn lineage_fixture() -> (tempfile::TempDir, CodexProvider) {
        let tmp = tempfile::tempdir().unwrap();
        let day = tmp.path().join("sessions/2026/07/17");
        std::fs::create_dir_all(&day).unwrap();
        let record = |kind: &str, payload: serde_json::Value| {
            serde_json::from_str::<serde_json::Value>(&envelope_line(kind, payload)).unwrap()
        };
        let parent = vec![
            record(
                "session_meta",
                serde_json::json!({"id": THREAD_LINEAGE_PARENT, "cwd": "/tmp/lineage"}),
            ),
            record(
                "event_msg",
                serde_json::json!({
                    "type": "token_count",
                    "info": {
                        "last_token_usage": {"input_tokens": 5, "cached_input_tokens": 1, "output_tokens": 2},
                        "total_token_usage": {"input_tokens": 5, "cached_input_tokens": 1, "output_tokens": 2}
                    }
                }),
            ),
        ];
        let serialize = |records: &[serde_json::Value]| {
            records
                .iter()
                .map(serde_json::Value::to_string)
                .collect::<Vec<_>>()
                .join("\n")
                + "\n"
        };
        std::fs::write(
            day.join(format!(
                "rollout-2026-07-17T00-00-00-{THREAD_LINEAGE_PARENT}.jsonl"
            )),
            serialize(&parent),
        )
        .unwrap();

        let mut copied = parent.clone();
        for value in &mut copied {
            value["timestamp"] = serde_json::json!("2026-07-17T01:00:00.000Z");
        }
        let mut child = vec![record(
            "session_meta",
            serde_json::json!({"id": THREAD_EMBEDDED_FORK, "cwd": "/tmp/lineage"}),
        )];
        child.extend(copied);
        child.push(record(
            "response_item",
            serde_json::json!({
                "type": "message",
                "role": "assistant",
                "content": [{"type": "output_text", "text": "new fork work"}]
            }),
        ));
        std::fs::write(
            day.join(format!(
                "rollout-2026-07-17T01-00-00-{THREAD_EMBEDDED_FORK}.jsonl"
            )),
            serialize(&child),
        )
        .unwrap();

        std::fs::write(
            day.join(format!("rollout-2026-07-17T02-00-00-{THREAD_SPAWN}.jsonl")),
            envelope_line(
                "session_meta",
                serde_json::json!({
                    "id": THREAD_SPAWN,
                    "source": {"subagent": {"thread_spawn": {
                        "parent_thread_id": THREAD_LINEAGE_PARENT,
                        "depth": 1,
                        "agent_role": "explore"
                    }}}
                }),
            ) + "\n",
        )
        .unwrap();

        std::fs::write(
            day.join(format!(
                "rollout-2026-07-17T03-00-00-{THREAD_NOT_A_FORK}.jsonl"
            )),
            [
                envelope_line("session_meta", serde_json::json!({"id": THREAD_NOT_A_FORK})),
                envelope_line(
                    "response_item",
                    serde_json::json!({"type": "message", "role": "assistant", "content": []}),
                ),
                envelope_line(
                    "session_meta",
                    serde_json::json!({"id": THREAD_LINEAGE_PARENT}),
                ),
            ]
            .join("\n")
                + "\n",
        )
        .unwrap();

        let provider = CodexProvider::new(tmp.path());
        (tmp, provider)
    }

    /// Opt-in real-corpus conformance check (never in public CI): run with
    /// `cargo test --features codex -- --ignored codex_real_corpus`.
    /// Emits AGGREGATE results only — no transcript content.
    #[test]
    #[ignore = "requires a real $CODEX_HOME; aggregate-only, opt-in"]
    fn codex_real_corpus_conformance() {
        let Ok(p) = CodexProvider::discover() else {
            eprintln!("no Codex home; skipping");
            return;
        };
        let Ok(sessions) = p.sessions() else {
            eprintln!("no sessions; skipping");
            return;
        };
        let mut totals = IngestionDiagnostics::default();
        let (mut parsed_ok, mut legacy, mut errors, mut violations) = (0u32, 0u32, 0u32, 0u32);
        let (mut count_mismatches, mut raced) = (0u32, 0u32);
        let (mut human_boundaries, mut human_midturn) = (0usize, 0usize);
        let mut expected_lineage: std::collections::BTreeSet<LineageEdge> =
            std::collections::BTreeSet::new();
        let mut inherited_sessions = 0usize;
        for d in &sessions {
            assert!(d.validate().is_empty(), "invalid descriptor");
            match p.parse(&d.key) {
                Ok(session) => {
                    parsed_ok += 1;
                    // Aggregate-only: no session keys in output.
                    if !session.validate_provenance().is_empty() {
                        violations += 1;
                    }
                    // Round-24 SOURCE-DERIVED semantic audits, asserted for
                    // EVERY session. The oracle reads the NATIVE record
                    // stream itself; expectations are computed by
                    // independently written rule code, never by replaying
                    // production outputs.
                    let (_, audit_path) = p.resolve(&d.key).unwrap();
                    let raw_records: Vec<(u64, serde_json::Value)> = {
                        let mut reader = p.open_records(&audit_path).unwrap();
                        let mut buf = Vec::new();
                        let mut ordinal = 0u64;
                        let mut recs = Vec::new();
                        loop {
                            buf.clear();
                            match reader.read_until(b'\n', &mut buf) {
                                Ok(0) => break,
                                Ok(_) => {
                                    if let Ok(v) = serde_json::from_slice::<serde_json::Value>(&buf)
                                    {
                                        recs.push((ordinal, v));
                                    }
                                    ordinal += 1;
                                }
                                Err(_) => break,
                            }
                        }
                        recs
                    };
                    let raw_by_ordinal: std::collections::BTreeMap<u64, &serde_json::Value> =
                        raw_records.iter().map(|(o, v)| (*o, v)).collect();
                    let copied = independent_copied_history(&p, &d.key, &raw_records);
                    let inherited_range = copied.as_ref().map(|(_, range)| *range);
                    let first_new_after_fork =
                        inherited_range.map(|(_, last)| last.saturating_add(1));
                    if inherited_range.is_some() {
                        inherited_sessions += 1;
                    }
                    let activity_violations =
                        audit_inherited_activity(&d.key, inherited_range, &session);
                    assert!(
                        activity_violations.is_empty(),
                        "fork activity audit failed: {} violation(s)",
                        activity_violations.len()
                    );

                    // Derive expected lineage directly from native metadata,
                    // independently of SourceProvider::lineage().
                    if let Some((zero, first)) = raw_records.first() {
                        if *zero == 0
                            && first["type"] == "session_meta"
                            && first["payload"]["id"].as_str() == Some(&d.key.native_id)
                        {
                            let direct_fork = first["payload"]["forked_from_id"]
                                .as_str()
                                .filter(|id| uuid::Uuid::parse_str(id).is_ok())
                                .map(str::to_string);
                            let embedded = independent_embedded_parent_id(&d.key, &raw_records);
                            if let Some(parent) = direct_fork.or(embedded) {
                                expected_lineage.insert(LineageEdge {
                                    from: key(&parent),
                                    to: d.key.clone(),
                                    kind: LineageEdgeKind::Fork,
                                });
                            }
                            let nested = first["payload"].pointer("/source/subagent/thread_spawn");
                            let parent = first["payload"]["parent_thread_id"]
                                .as_str()
                                .or_else(|| nested.and_then(|n| n["parent_thread_id"].as_str()))
                                .filter(|id| uuid::Uuid::parse_str(id).is_ok());
                            if let Some(parent) = parent {
                                let nested_string = |field: &str| {
                                    nested.and_then(|n| n[field].as_str()).map(str::to_string)
                                };
                                expected_lineage.insert(LineageEdge {
                                    from: key(parent),
                                    to: d.key.clone(),
                                    kind: LineageEdgeKind::Spawn {
                                        tool_use_id: nested_string("tool_use_id"),
                                        agent_type: first["payload"]["agent_role"]
                                            .as_str()
                                            .map(str::to_string)
                                            .or_else(|| nested_string("agent_role")),
                                        description: nested_string("task_name"),
                                    },
                                });
                            }
                        }
                    }
                    let joined = |payload: &serde_json::Value| -> String {
                        payload["content"]
                            .as_array()
                            .map(|items| {
                                items
                                    .iter()
                                    .filter_map(|i| i["text"].as_str())
                                    .collect::<Vec<_>>()
                                    .join("")
                            })
                            .unwrap_or_default()
                    };

                    // (a) STRUCTURAL twin verification: type correspondence
                    // plus exact extracted content (or exact fingerprint for
                    // event-to-event duplicates). No empty-text escape.
                    for disp in &session.record_dispositions {
                        if let RecordOutcome::Suppressed {
                            reason: super::super::SuppressionReason::DuplicateStream { twin },
                        } = &disp.outcome
                        {
                            let ev = raw_by_ordinal[&disp.record.ordinal];
                            let tw = raw_by_ordinal[&twin.ordinal];
                            let ev_pt = ev["payload"]["type"].as_str().unwrap_or("");
                            let ev_text = ev["payload"]["message"]
                                .as_str()
                                .or_else(|| ev["payload"]["text"].as_str())
                                .unwrap_or("");
                            if tw["type"] == "event_msg" {
                                // Event-to-event duplicate: exact fingerprint.
                                assert_eq!(
                                    ev["payload"], tw["payload"],
                                    "event duplicate must share the exact payload"
                                );
                                assert_eq!(
                                    ev["timestamp"], tw["timestamp"],
                                    "event duplicate must share the timestamp"
                                );
                            } else {
                                assert_eq!(tw["type"], "response_item");
                                match ev_pt {
                                    "user_message" => {
                                        assert_eq!(tw["payload"]["type"], "message");
                                        assert_eq!(tw["payload"]["role"], "user");
                                        assert_eq!(
                                            ev_text,
                                            joined(&tw["payload"]),
                                            "user twin content must match exactly"
                                        );
                                    }
                                    "agent_message" => {
                                        assert_eq!(tw["payload"]["type"], "message");
                                        assert_eq!(tw["payload"]["role"], "assistant");
                                        assert_eq!(
                                            ev_text,
                                            joined(&tw["payload"]),
                                            "agent twin content must match exactly"
                                        );
                                    }
                                    "agent_reasoning" | "agent_reasoning_raw_content" => {
                                        assert_eq!(tw["payload"]["type"], "reasoning");
                                        let mut found = false;
                                        for list in ["summary", "content"] {
                                            if let Some(items) = tw["payload"][list].as_array() {
                                                for item in items {
                                                    if item["text"].as_str() == Some(ev_text) {
                                                        found = true;
                                                    }
                                                }
                                            }
                                        }
                                        assert!(
                                            found,
                                            "reasoning twin must contain the exact section"
                                        );
                                    }
                                    other => panic!("unexpected suppressed type {other}"),
                                }
                            }
                        }
                    }

                    // (b) INDEPENDENTLY DERIVED usage-allocation audit
                    // (round-25): the reusable oracle computes expected
                    // partition, owner, cardinality, values, basis, and
                    // ambiguity from the native stream alone — deliberately-
                    // altered negative-control tests prove it rejects broken
                    // output. Aggregate-only reporting (count, no ids).
                    let usage_violations = audit_usage_allocation(
                        &d.key,
                        &raw_records,
                        &session,
                        first_new_after_fork,
                    );
                    assert!(
                        usage_violations.is_empty(),
                        "usage allocation audit failed: {} violation(s)",
                        usage_violations.len()
                    );

                    // (c) Native-derived human-prompt partition: response
                    // twins open turns; unique same-window user events are
                    // steering. The oracle is mutation-tested below and
                    // checks the exact set, not merely aggregate counts.
                    let prompt_audit = audit_prompt_semantics(
                        &d.key,
                        &raw_records,
                        &session,
                        first_new_after_fork,
                    );
                    assert!(
                        prompt_audit.violations.is_empty(),
                        "prompt semantics audit failed: {} violation(s)",
                        prompt_audit.violations.len()
                    );
                    human_boundaries += prompt_audit.boundary_count;
                    human_midturn += prompt_audit.midturn_count;
                    let conversation = crate::reconstruction::Conversation::from_parsed_session(
                        std::sync::Arc::new(session.clone()),
                    )
                    .expect("normalized corpus session reconstructs");
                    let turns = crate::analysis::timeline::semantic_turns(&conversation);
                    let retained_steering: usize =
                        turns.iter().map(|t| t.steering_messages.len()).sum();
                    assert_eq!(
                        retained_steering, prompt_audit.midturn_count,
                        "every native-derived midturn prompt must survive semantic grouping"
                    );
                    let rendered = crate::analysis::timeline::build_semantic_timeline(
                        &turns,
                        &crate::analysis::timeline::TimelineOptions {
                            limit: usize::MAX,
                            ..Default::default()
                        },
                    );
                    let rendered_steering: usize =
                        rendered.iter().map(|t| t.steering_prompts.len()).sum();
                    assert_eq!(
                        rendered_steering, prompt_audit.midturn_count,
                        "every native-derived midturn prompt must survive timeline rendering"
                    );

                    // physical record — compare against an independent count
                    // of the preferred artifact's records. Active sessions
                    // can append between the parse and the count; a mismatch
                    // with a CHANGED revision is a raced result, not a
                    // correctness failure (retried once).
                    let (_, path) = p.resolve(&d.key).unwrap();
                    let count_records = || {
                        let mut reader = p.open_records(&path).unwrap();
                        let mut independent = 0usize;
                        let mut buf = Vec::new();
                        loop {
                            buf.clear();
                            match reader.read_until(b'\n', &mut buf) {
                                Ok(0) => break,
                                Ok(_) => independent += 1,
                                Err(_) => {
                                    independent += 1;
                                    break;
                                }
                            }
                        }
                        independent
                    };
                    let revision = || {
                        std::fs::metadata(&path)
                            .map(|m| (m.len(), m.modified().ok()))
                            .ok()
                    };
                    let rev_a = revision();
                    let mut mismatched = session.record_dispositions.len() != count_records();
                    if mismatched {
                        // Retry once against a fresh parse.
                        if let Ok(fresh) = p.parse(&d.key) {
                            mismatched = fresh.record_dispositions.len() != count_records();
                        }
                        if mismatched {
                            if rev_a != revision() {
                                raced += 1;
                            } else {
                                count_mismatches += 1;
                            }
                        }
                    }
                    totals.mapped += session.diagnostics.mapped;
                    totals.suppressed += session.diagnostics.suppressed;
                    totals.unknown += session.diagnostics.unknown;
                    totals.recovered += session.diagnostics.recovered;
                    totals.unparseable += session.diagnostics.unparseable;
                }
                Err(ProviderError::Unsupported { .. }) => legacy += 1,
                Err(_) => errors += 1,
            }
        }
        let actual_lineage: std::collections::BTreeSet<_> = p
            .lineage()
            .expect("lineage collection")
            .into_iter()
            .collect();
        assert_eq!(
            actual_lineage, expected_lineage,
            "lineage must equal the independently derived native edge set"
        );
        let edges = actual_lineage.len();
        let drift = p.drift_report().unwrap_or_default();
        eprintln!(
            "codex drift: {} envelope types ({} unknown), {} response_item types ({} unknown), \
             {} event_msg types ({} unknown), reasoning summary {}/{}",
            drift.envelope_types.len(),
            drift.unknown_envelope_types.len(),
            drift.response_item_types.len(),
            drift.unknown_response_item_types.len(),
            drift.event_msg_types.len(),
            drift.unknown_event_msg_types.len(),
            drift.reasoning_with_summary,
            drift.reasoning_items
        );
        eprintln!(
            "codex drift detail: no nested drift among {} CHECKED records \
             ({} unknown paths {:?}); {} unbaselined variants ({} records) NOT checked; \
             {} months, active_tails={}, missing_types={}, missing_payload_types={}, \
             unreadable={}",
            drift.field_schema_checked_records,
            drift.unknown_field_paths.len(),
            drift
                .unknown_field_paths
                .iter()
                .take(10)
                .collect::<Vec<_>>(),
            drift.unbaselined_payload_types.len(),
            drift.unbaselined_payload_types.values().sum::<u64>(),
            drift.reasoning_by_month.len(),
            drift.active_tails,
            drift.missing_type_discriminators,
            drift.missing_payload_discriminators,
            drift.unreadable_sessions
        );
        eprintln!(
            "codex corpus: {n} sessions, {parsed_ok} parsed, {legacy} legacy-refused, \
             {errors} errors, {violations} provenance violations, {edges} lineage edges \
             ({inherited_sessions} copied-history sessions), \
             {raced} raced, human prompts: {human_boundaries} boundary + \
             {human_midturn} midturn, records: {totals:?}",
            n = sessions.len()
        );
        assert_eq!(errors, 0, "no session may fail outside the legacy contract");
        assert_eq!(violations, 0, "provenance must validate corpus-wide");
        assert_eq!(
            count_mismatches, 0,
            "every physical record must be reached and dispositioned"
        );
    }

    #[test]
    fn discovery_finds_plain_zst_archived_and_legacy() {
        let (_tmp, p) = fixture();
        let sessions = p.sessions().unwrap();
        assert_eq!(sessions.len(), 4);
        for d in &sessions {
            assert!(d.validate().is_empty());
        }

        // A: active plain + archived plain = two artifacts, plain active wins.
        let a = sessions
            .iter()
            .find(|d| d.key.native_id == THREAD_A)
            .unwrap();
        assert_eq!(a.artifacts.len(), 2);
        let preferred = a.preferred_artifact().unwrap();
        assert!(!preferred.archived);
        assert_eq!(preferred.form, ArtifactForm::PlainFile);

        // B: compressed-only.
        let b = sessions
            .iter()
            .find(|d| d.key.native_id == THREAD_B)
            .unwrap();
        assert_eq!(b.artifacts.len(), 1);
        assert_eq!(
            b.preferred_artifact().unwrap().form,
            ArtifactForm::CompressedFile
        );
    }

    #[test]
    fn envelope_parse_preserves_everything_with_honest_dispositions() {
        let (_tmp, p) = fixture();
        let parsed = p.parse(&key(THREAD_A)).unwrap();
        assert!(
            parsed.validate_provenance().is_empty(),
            "{:?}",
            parsed.validate_provenance()
        );
        // B3 slice 1: the user message maps, the info-less token_count is
        // suppressed (no attributable assistant emission), session_meta and
        // the unknown type stay preserved Unknown, torn tail unparseable.
        assert_eq!(
            parsed.diagnostics,
            IngestionDiagnostics {
                mapped: 1,
                suppressed: 1,
                unknown: 2,
                recovered: 0,
                unparseable: 1
            }
        );
        assert_eq!(parsed.entries.len(), 3);
        // Constraint 1 (round-21): the mapped record keeps the deterministic
        // id its B1 Unknown entry had — ordinal 1, subindex 0.
        let user_entry = parsed
            .entries
            .iter()
            .find(|e| matches!(e.entry, crate::model::LogEntry::User(_)))
            .expect("mapped user entry");
        assert_eq!(
            user_entry.id,
            crate::provider::EntryId::deterministic(&key(THREAD_A), 1, 0)
        );
    }

    #[test]
    fn zst_only_session_parses_via_streaming_decode() {
        let (_tmp, p) = fixture();
        let parsed = p.parse(&key(THREAD_B)).unwrap();
        // session_meta preserved Unknown; the assistant message maps.
        assert_eq!(parsed.diagnostics.unknown, 1);
        assert_eq!(parsed.diagnostics.mapped, 1);
        assert_eq!(parsed.diagnostics.unparseable, 0);
    }

    #[test]
    fn plain_and_zst_twins_normalize_identically() {
        // The SAME content, plain vs compressed, in two separate homes.
        let content = session_a_content();
        let make = |compress: bool| {
            let tmp = tempfile::tempdir().unwrap();
            let day = tmp.path().join("sessions/2026/07/16");
            std::fs::create_dir_all(&day).unwrap();
            let name = format!(
                "rollout-2026-07-16T23-38-33-{THREAD_A}.jsonl{}",
                if compress { ".zst" } else { "" }
            );
            if compress {
                let z = zstd::stream::encode_all(content.as_bytes(), 3).unwrap();
                std::fs::write(day.join(name), z).unwrap();
            } else {
                std::fs::write(day.join(name), &content).unwrap();
            }
            let p = CodexProvider::new(tmp.path());
            (tmp, p)
        };
        let (_t1, plain) = make(false);
        let (_t2, compressed) = make(true);
        let a = plain.parse(&key(THREAD_A)).unwrap();
        let b = compressed.parse(&key(THREAD_A)).unwrap();
        assert_eq!(a.entries.len(), b.entries.len());
        for (x, y) in a.entries.iter().zip(b.entries.iter()) {
            assert_eq!(x.id, y.id);
            assert_eq!(
                serde_json::to_value(&x.entry).unwrap(),
                serde_json::to_value(&y.entry).unwrap()
            );
        }
        assert_eq!(a.diagnostics, b.diagnostics);
    }

    #[test]
    fn decompression_limit_is_enforced() {
        let (_tmp, p) = fixture();
        let p = p.with_max_decompressed(16);
        let err_session = p.parse(&key(THREAD_B)).unwrap();
        // The stream is cut at the limit: the parse records an unparseable
        // read error rather than silently succeeding.
        assert!(
            err_session.diagnostics.unparseable >= 1,
            "{:?}",
            err_session.diagnostics
        );
    }

    #[test]
    fn legacy_files_are_inventoried_but_normalization_is_refused() {
        let (_tmp, p) = fixture();
        let legacy_key = key(THREAD_LEGACY);
        // Discovered:
        assert!(p.sessions().unwrap().iter().any(|d| d.key == legacy_key));
        assert_eq!(
            p.sniff_format(Path::new(&p.resolve(&legacy_key).unwrap().1))
                .unwrap(),
            FormatFamily::Legacy
        );
        // Normalization refused loudly:
        assert!(matches!(
            p.parse(&legacy_key),
            Err(ProviderError::Unsupported { .. })
        ));
        // Native/raw export still available:
        let mut sink = Vec::new();
        p.write_raw_jsonl(&legacy_key, &mut sink).unwrap();
        assert!(!sink.is_empty());
    }

    #[test]
    fn lineage_reports_fork_edges() {
        let (_tmp, p) = fixture();
        let edges = p.lineage().unwrap();
        assert!(edges.iter().any(|e| e.kind == LineageEdgeKind::Fork
            && e.from.native_id == THREAD_A
            && e.to.native_id == THREAD_FORK));
    }

    #[test]
    fn copied_record_proof_ignores_only_the_outer_timestamp() {
        let a: serde_json::Value = serde_json::from_str(&envelope_line(
            "session_meta",
            serde_json::json!({"id": THREAD_A, "cwd": "/tmp/a"}),
        ))
        .unwrap();
        let mut rewritten = a.clone();
        rewritten["timestamp"] = serde_json::json!("2026-07-17T04:00:00.000Z");
        assert!(copied_record_eq(&a, &rewritten));

        rewritten["payload"]["cwd"] = serde_json::json!("/tmp/different");
        assert!(
            !copied_record_eq(&a, &rewritten),
            "payload drift must end the copied prefix"
        );
        let mut missing_timestamp = a.clone();
        missing_timestamp
            .as_object_mut()
            .unwrap()
            .remove("timestamp");
        assert!(!copied_record_eq(&a, &missing_timestamp));
    }

    #[test]
    fn embedded_fork_marks_only_the_proven_copy_as_inherited() {
        let (_tmp, p) = lineage_fixture();
        let parsed = p.parse(&key(THREAD_EMBEDDED_FORK)).unwrap();
        assert!(parsed.validate_provenance().is_empty());

        let activity = |ordinal: u64| {
            parsed
                .semantics
                .get(&EntryId::deterministic(
                    &key(THREAD_EMBEDDED_FORK),
                    ordinal,
                    0,
                ))
                .map_or(ActivityKind::New, |s| s.activity)
        };
        assert_eq!(activity(0), ActivityKind::New, "child metadata is new");
        assert_eq!(activity(1), ActivityKind::InheritedHistory);
        assert_eq!(activity(2), ActivityKind::InheritedHistory);
        assert_eq!(activity(3), ActivityKind::New, "first fork work is new");

        // The copied token_count was waiting for an assistant in the parent.
        // The synthetic fork boundary must preserve it as inherited instead
        // of attaching it to the child's first new assistant.
        let copied_usage = parsed
            .record_dispositions
            .iter()
            .find(|d| d.record.ordinal == 2)
            .unwrap();
        assert!(matches!(
            copied_usage.outcome,
            RecordOutcome::Unknown { .. }
        ));
        let new_assistant = EntryId::deterministic(&key(THREAD_EMBEDDED_FORK), 3, 0);
        assert_eq!(
            parsed.entry_origins[&new_assistant]
                .iter()
                .map(|r| r.ordinal)
                .collect::<Vec<_>>(),
            vec![3]
        );

        let edges = p.lineage().unwrap();
        assert!(edges.iter().any(|e| {
            e.from.native_id == THREAD_LINEAGE_PARENT
                && e.to.native_id == THREAD_EMBEDDED_FORK
                && e.kind == LineageEdgeKind::Fork
        }));
    }

    #[test]
    fn fork_activity_oracle_rejects_both_partition_directions() {
        let (_tmp, p) = lineage_fixture();
        let child_key = key(THREAD_EMBEDDED_FORK);
        let mut parsed = p.parse(&child_key).unwrap();
        let (_, path) = p.resolve(&child_key).unwrap();
        let mut reader = p.open_records(&path).unwrap();
        let mut raw = Vec::new();
        let mut ordinal = 0_u64;
        let mut line = Vec::new();
        loop {
            line.clear();
            match reader.read_until(b'\n', &mut line) {
                Ok(0) => break,
                Ok(_) => {
                    raw.push((ordinal, serde_json::from_slice(&line).unwrap()));
                    ordinal += 1;
                }
                Err(error) => panic!("fixture read: {error}"),
            }
        }
        let (_, range) = independent_copied_history(&p, &child_key, &raw).unwrap();
        assert!(audit_inherited_activity(&child_key, Some(range), &parsed).is_empty());

        let inherited = EntryId::deterministic(&child_key, 1, 0);
        parsed.semantics.get_mut(&inherited).unwrap().activity = ActivityKind::New;
        assert!(audit_inherited_activity(&child_key, Some(range), &parsed)
            .iter()
            .any(|v| v.contains("expected InheritedHistory")));

        parsed.semantics.get_mut(&inherited).unwrap().activity = ActivityKind::InheritedHistory;
        let new_work = EntryId::deterministic(&child_key, 3, 0);
        parsed.semantics.get_mut(&new_work).unwrap().activity = ActivityKind::InheritedHistory;
        assert!(audit_inherited_activity(&child_key, Some(range), &parsed)
            .iter()
            .any(|v| v.contains("expected New")));
    }

    #[test]
    fn typed_spawn_source_is_linked_but_a_late_meta_is_not_a_fork() {
        let (_tmp, p) = lineage_fixture();
        let edges = p.lineage().unwrap();
        assert!(edges.iter().any(|e| {
            e.from.native_id == THREAD_LINEAGE_PARENT
                && e.to.native_id == THREAD_SPAWN
                && matches!(
                    &e.kind,
                    LineageEdgeKind::Spawn {
                        tool_use_id: None,
                        agent_type: Some(role),
                        description: None,
                    } if role == "explore"
                )
        }));
        assert!(
            !edges.iter().any(|e| {
                e.to.native_id == THREAD_NOT_A_FORK && e.kind == LineageEdgeKind::Fork
            }),
            "only physical record one may carry the embedded-parent heuristic"
        );
    }

    #[test]
    fn fork_parent_state_participates_in_the_child_cache_token() {
        use crate::cache::CacheManager;
        use crate::config::CacheConfig;
        use crate::provider::registry::cached_parsed_session;

        let tmp = tempfile::tempdir().unwrap();
        let day = tmp.path().join("sessions/2026/07/17");
        std::fs::create_dir_all(&day).unwrap();
        let parent_meta = envelope_line(
            "session_meta",
            serde_json::json!({"id": THREAD_LINEAGE_PARENT, "cwd": "/tmp/cache-parent"}),
        );
        let child = [
            envelope_line(
                "session_meta",
                serde_json::json!({"id": THREAD_EMBEDDED_FORK, "cwd": "/tmp/cache-parent"}),
            ),
            parent_meta.clone(),
        ]
        .join("\n")
            + "\n";
        std::fs::write(
            day.join(format!(
                "rollout-2026-07-17T01-00-00-{THREAD_EMBEDDED_FORK}.jsonl"
            )),
            child,
        )
        .unwrap();
        let p = CodexProvider::new(tmp.path());
        let child_key = key(THREAD_EMBEDDED_FORK);
        let missing_parent_token = p.parse_cache_token(&child_key).unwrap();
        let cache = CacheManager::new(&CacheConfig {
            enabled: true,
            ..Default::default()
        });
        let before = cached_parsed_session(&cache, &p, &child_key).unwrap();
        assert!(before
            .semantics
            .values()
            .all(|s| s.activity == ActivityKind::New));

        std::fs::write(
            day.join(format!(
                "rollout-2026-07-17T00-00-00-{THREAD_LINEAGE_PARENT}.jsonl"
            )),
            parent_meta + "\n",
        )
        .unwrap();
        let present_parent_token = p.parse_cache_token(&child_key).unwrap();
        assert_ne!(
            missing_parent_token, present_parent_token,
            "a newly available parent changes inherited classification"
        );
        let after = cached_parsed_session(&cache, &p, &child_key).unwrap();
        assert!(
            !std::sync::Arc::ptr_eq(&before, &after),
            "the production cache must reparse when the parent appears"
        );
        assert_eq!(
            after
                .semantics
                .get(&EntryId::deterministic(&child_key, 1, 0))
                .unwrap()
                .activity,
            ActivityKind::InheritedHistory
        );
    }

    #[test]
    fn raw_jsonl_decompresses_and_native_is_exact_bytes() {
        let (_tmp, p) = fixture();
        // raw-jsonl of the compressed session = decompressed records.
        let mut raw = Vec::new();
        p.write_raw_jsonl(&key(THREAD_B), &mut raw).unwrap();
        assert!(raw.starts_with(b"{"));
        let newlines = raw.split(|b| *b == b'\n').count() - 1;
        assert_eq!(newlines, 2);

        // native of the compressed artifact = the exact .zst bytes.
        let d = p
            .sessions()
            .unwrap()
            .into_iter()
            .find(|d| d.key.native_id == THREAD_B)
            .unwrap();
        let art = d.preferred_artifact().unwrap().snapshot.id.clone();
        let mut native = Vec::new();
        p.write_native(&art, &mut native).unwrap();
        let on_disk = std::fs::read(&art.locator).unwrap();
        assert_eq!(native, on_disk);
        assert!(native.starts_with(&[0x28, 0xb5, 0x2f, 0xfd]), "zstd magic");
    }

    #[test]
    fn decompressed_size_exactly_at_limit_is_valid() {
        let content = session_a_content();
        let tmp = tempfile::tempdir().unwrap();
        let day = tmp.path().join("sessions/2026/07/16");
        std::fs::create_dir_all(&day).unwrap();
        let z = zstd::stream::encode_all(content.as_bytes(), 3).unwrap();
        std::fs::write(
            day.join(format!("rollout-2026-07-16T23-38-33-{THREAD_A}.jsonl.zst")),
            z,
        )
        .unwrap();
        // Exactly the limit: parses completely.
        let p = CodexProvider::new(tmp.path()).with_max_decompressed(content.len() as u64);
        let parsed = p.parse(&key(THREAD_A)).unwrap();
        assert_eq!(parsed.record_dispositions.len(), 4);
        assert_eq!(parsed.diagnostics.unparseable, 0);
        // One byte short: the limit crossing is recorded, later data is cut.
        let p = CodexProvider::new(tmp.path()).with_max_decompressed(content.len() as u64 - 1);
        let parsed = p.parse(&key(THREAD_A)).unwrap();
        assert!(parsed.diagnostics.unparseable >= 1);
    }

    #[test]
    fn compressed_input_limit_is_enforced() {
        let (_tmp, p) = fixture();
        let p = p.with_max_compressed(4);
        let err = p.parse(&key(THREAD_B)).unwrap_err();
        assert!(err.to_string().contains("max_compressed"), "{err}");
    }

    #[test]
    fn corrupt_zst_stream_is_recorded_not_fatal() {
        let content = session_a_content();
        let tmp = tempfile::tempdir().unwrap();
        let day = tmp.path().join("sessions/2026/07/16");
        std::fs::create_dir_all(&day).unwrap();
        let mut z = zstd::stream::encode_all(content.as_bytes(), 3).unwrap();
        // Corrupt a byte in the middle of the frame body.
        let mid = z.len() / 2;
        z[mid] ^= 0xFF;
        std::fs::write(
            day.join(format!("rollout-2026-07-16T23-38-33-{THREAD_A}.jsonl.zst")),
            z,
        )
        .unwrap();
        let p = CodexProvider::new(tmp.path());
        let parsed = p.parse(&key(THREAD_A)).unwrap();
        assert!(
            parsed.diagnostics.unparseable >= 1,
            "decoder failure must surface as a disposition: {:?}",
            parsed.diagnostics
        );
        assert!(parsed.validate_provenance().is_empty());
    }

    #[test]
    fn invalid_utf8_record_does_not_lose_later_records() {
        // valid -> invalid UTF-8 -> valid, in BOTH plain and compressed form.
        let head = envelope_line("session_meta", serde_json::json!({"id": THREAD_A}));
        let tail = envelope_line("event_msg", serde_json::json!({"type": "token_count"}));
        let mut bytes = Vec::new();
        bytes.extend_from_slice(head.as_bytes());
        bytes.extend_from_slice(b"\n\xff\xfe broken \xff\n");
        bytes.extend_from_slice(tail.as_bytes());
        bytes.push(b'\n');

        for compress in [false, true] {
            let tmp = tempfile::tempdir().unwrap();
            let day = tmp.path().join("sessions/2026/07/16");
            std::fs::create_dir_all(&day).unwrap();
            let name = format!(
                "rollout-2026-07-16T23-38-33-{THREAD_A}.jsonl{}",
                if compress { ".zst" } else { "" }
            );
            if compress {
                let z = zstd::stream::encode_all(&bytes[..], 3).unwrap();
                std::fs::write(day.join(name), z).unwrap();
            } else {
                std::fs::write(day.join(name), &bytes).unwrap();
            }
            let p = CodexProvider::new(tmp.path());
            let parsed = p.parse(&key(THREAD_A)).unwrap();
            // session_meta stays Unknown; the token_count AFTER the corrupt
            // line survives to a disposition (suppressed heartbeat).
            assert_eq!(
                (parsed.diagnostics.unknown, parsed.diagnostics.suppressed),
                (1, 1),
                "the record AFTER the corrupt line must survive (compress={compress})"
            );
            assert_eq!(parsed.diagnostics.unparseable, 1);
            assert!(parsed.validate_provenance().is_empty());
        }
    }

    #[test]
    fn sniffing_is_shape_based_not_vocabulary_based() {
        // First record carries a FUTURE envelope type: still envelope-era.
        let tmp = tempfile::tempdir().unwrap();
        let day = tmp.path().join("sessions/2026/07/16");
        std::fs::create_dir_all(&day).unwrap();
        let path = day.join(format!("rollout-2026-07-16T23-38-33-{THREAD_A}.jsonl"));
        std::fs::write(
            &path,
            envelope_line("hologram_state_v12", serde_json::json!({"future": true})) + "\n",
        )
        .unwrap();
        let p = CodexProvider::new(tmp.path());
        assert_eq!(p.sniff_format(&path).unwrap(), FormatFamily::Envelope);
        let parsed = p.parse(&key(THREAD_A)).unwrap();
        assert_eq!(parsed.diagnostics.unknown, 1);
    }

    #[test]
    fn artifacts_are_sorted_by_stable_identity() {
        let (_tmp, p) = fixture();
        for d in p.sessions().unwrap() {
            let ids: Vec<_> = d.artifacts.iter().map(|a| &a.snapshot.id).collect();
            let mut sorted = ids.clone();
            sorted.sort();
            assert_eq!(ids, sorted, "artifact order must not follow read_dir");
        }
    }

    #[cfg(unix)]
    #[test]
    fn symlinks_are_never_followed() {
        use std::os::unix::fs::symlink;
        let (tmp, p) = fixture();
        let day = tmp.path().join("sessions/2026/07/16");
        // A directory symlink cycle and an external-file symlink.
        symlink(tmp.path().join("sessions"), day.join("loop")).unwrap();
        let outside = tmp.path().join("outside.jsonl");
        std::fs::write(&outside, session_a_content()).unwrap();
        symlink(
            &outside,
            day.join(format!("rollout-2026-07-16T01-01-01-{THREAD_FORK}.jsonl")),
        )
        .ok();
        // Discovery completes (no cycle hang) and neither symlink target is
        // discovered as a new artifact.
        let before = p.sessions().unwrap();
        let fork = before
            .iter()
            .find(|d| d.key.native_id == THREAD_FORK)
            .unwrap();
        assert_eq!(
            fork.artifacts.len(),
            1,
            "external symlinked file must not become an artifact"
        );
    }

    // Linux-only: APFS (macOS) rejects non-UTF-8 filenames at creation, so
    // the scenario is unrepresentable there — not a platform behavior
    // difference in the provider.
    #[cfg(target_os = "linux")]
    #[test]
    fn non_utf8_codex_home_round_trips() {
        use std::ffi::OsStr;
        use std::os::unix::ffi::OsStrExt;
        let tmp = tempfile::tempdir().unwrap();
        let weird = tmp.path().join(OsStr::from_bytes(b"codex-\xff-home"));
        let day = weird.join("sessions/2026/07/16");
        std::fs::create_dir_all(&day).unwrap();
        std::fs::write(
            day.join(format!("rollout-2026-07-16T23-38-33-{THREAD_A}.jsonl")),
            session_a_content(),
        )
        .unwrap();
        let p = CodexProvider::new(&weird);
        let sessions = p.sessions().unwrap();
        assert_eq!(sessions.len(), 1);
        // Parse and native export work despite the lossy locator display —
        // resolution goes through the preserved PathBuf, never the string.
        let parsed = p.parse(&key(THREAD_A)).unwrap();
        assert_eq!(parsed.record_dispositions.len(), 4);
        // Every provenance reference names an artifact that actually exists
        // in the descriptor (against 48513e3 this failed: parse
        // reconstructed a lossy id that matched nothing).
        let members: std::collections::BTreeSet<_> = parsed
            .descriptor
            .artifacts
            .iter()
            .map(|a| a.snapshot.id.clone())
            .collect();
        for d in &parsed.record_dispositions {
            assert!(
                members.contains(&d.record.artifact),
                "disposition references a nonexistent artifact"
            );
        }
        let art = sessions[0]
            .preferred_artifact()
            .unwrap()
            .snapshot
            .id
            .clone();
        let mut native = Vec::new();
        p.write_native(&art, &mut native).unwrap();
        assert_eq!(native, session_a_content().as_bytes());
        let mut bundle = Vec::new();
        p.write_archive(&key(THREAD_A), &mut bundle).unwrap();
        let newline = bundle.iter().position(|b| *b == b'\n').unwrap();
        assert_eq!(
            &bundle[newline + 1..],
            session_a_content().as_bytes(),
            "archive works under a non-UTF-8 home"
        );
    }

    fn fixture_path(name: &str) -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/codex")
            .join(name)
    }

    fn home_with(name: &str, bytes: &[u8], compressed: bool) -> (tempfile::TempDir, CodexProvider) {
        let tmp = tempfile::tempdir().unwrap();
        let day = tmp.path().join("sessions/2026/07/16");
        std::fs::create_dir_all(&day).unwrap();
        let file = format!(
            "rollout-2026-07-16T23-38-33-{name}.jsonl{}",
            if compressed { ".zst" } else { "" }
        );
        std::fs::write(day.join(file), bytes).unwrap();
        let p = CodexProvider::new(tmp.path());
        (tmp, p)
    }

    #[test]
    fn external_zst_fixture_decodes_identically_to_plain() {
        // Interop: the .zst was produced by the SYSTEM zstd CLI, not this
        // crate's bundled encoder (fixture manifest records provenance).
        let plain_bytes = std::fs::read(fixture_path("envelope_session.jsonl")).unwrap();
        let zst_bytes = std::fs::read(fixture_path("envelope_session.jsonl.zst")).unwrap();
        let (_t1, plain) = home_with(THREAD_A, &plain_bytes, false);
        let (_t2, compressed) = home_with(THREAD_A, &zst_bytes, true);
        let a = plain.parse(&key(THREAD_A)).unwrap();
        let b = compressed.parse(&key(THREAD_A)).unwrap();
        // B3 slice 1 over the 8 fixture records: user msg + function_call +
        // output + assistant msg mapped (token_count maps INTO the call
        // entry), agent_message deduped, meta + turn_context preserved.
        assert_eq!(a.entries.len(), 6);
        assert_eq!(a.diagnostics.mapped, 5);
        assert_eq!(a.diagnostics.suppressed, 1);
        assert_eq!(a.diagnostics.unknown, 2);
        assert_eq!(a.entries.len(), b.entries.len());
        for (x, y) in a.entries.iter().zip(b.entries.iter()) {
            assert_eq!(x.id, y.id);
            assert_eq!(
                serde_json::to_value(&x.entry).unwrap(),
                serde_json::to_value(&y.entry).unwrap()
            );
        }
        assert_eq!(a.diagnostics, b.diagnostics);
    }

    #[test]
    fn corrupted_content_checksum_is_rejected() {
        // The external fixture carries an XXH64 content checksum; corrupting
        // its trailing checksum bytes must surface as a rejected stream, not
        // a silent success.
        let mut zst = std::fs::read(fixture_path("envelope_session.jsonl.zst")).unwrap();
        let last = zst.len() - 1;
        zst[last] ^= 0xFF;
        let (_t, p) = home_with(THREAD_A, &zst, true);
        let parsed = p.parse(&key(THREAD_A)).unwrap();
        // Observed libzstd behavior: for a frame smaller than the decode
        // buffer, the checksum is verified before ANY output is yielded, so
        // zero records emerge (incremental yield would give 8-then-error on
        // multi-buffer frames). The essential property is that the rejection
        // is checksum-specific and dispositioned.
        assert_eq!(parsed.diagnostics.unknown, 0, "{:?}", parsed.diagnostics);
        assert_eq!(parsed.diagnostics.unparseable, 1);
        let msg = parsed
            .record_dispositions
            .iter()
            .find_map(|d| match &d.outcome {
                RecordOutcome::Unparseable { error } => Some(error.message.to_lowercase()),
                _ => None,
            })
            .unwrap();
        assert!(msg.contains("checksum"), "not a checksum rejection: {msg}");
    }

    #[test]
    fn frame_window_above_guard_is_refused_cheaply() {
        // window28.bin.zst declares windowLog=28 (286 MiB decompressed);
        // the provider's window_log_max=27 must refuse it before any
        // meaningful decompression happens.
        let zst = std::fs::read(fixture_path("window28.bin.zst")).unwrap();
        let (_t, p) = home_with(THREAD_A, &zst, true);
        let parsed = p.parse(&key(THREAD_A)).unwrap();
        assert_eq!(parsed.entries.len(), 0, "nothing may decode");
        assert_eq!(parsed.diagnostics.unparseable, 1);
        // The disposition must specifically be the window/memory refusal —
        // decompressing 286 MiB and then failing on JSON would be a
        // different (wrong) outcome.
        let msg = parsed
            .record_dispositions
            .iter()
            .find_map(|d| match &d.outcome {
                RecordOutcome::Unparseable { error } => Some(error.message.to_lowercase()),
                _ => None,
            })
            .unwrap();
        assert!(
            msg.contains("memory") || msg.contains("window"),
            "not a window refusal: {msg}"
        );
    }

    #[test]
    fn legacy_fixture_sniffs_and_refuses_per_contract() {
        let bytes = std::fs::read(fixture_path("legacy_session.jsonl")).unwrap();
        let (_t, p) = home_with(THREAD_LEGACY, &bytes, false);
        let (_, path) = p.resolve(&key(THREAD_LEGACY)).unwrap();
        assert_eq!(p.sniff_format(&path).unwrap(), FormatFamily::Legacy);
        assert!(matches!(
            p.parse(&key(THREAD_LEGACY)),
            Err(ProviderError::Unsupported { .. })
        ));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn colliding_non_utf8_sibling_paths_stay_distinct() {
        use std::ffi::OsStr;
        use std::os::unix::ffi::OsStrExt;
        let tmp = tempfile::tempdir().unwrap();
        let base = tmp.path().join("sessions/2026/07");
        let d1 = base.join(OsStr::from_bytes(b"day-\xff"));
        let d2 = base.join(OsStr::from_bytes(b"day-\xfe"));
        std::fs::create_dir_all(&d1).unwrap();
        std::fs::create_dir_all(&d2).unwrap();
        // Display strings collide (replacement characters)...
        assert_eq!(d1.display().to_string(), d2.display().to_string());
        // ...and the FILENAMES are IDENTICAL, so the complete lossy path
        // strings collide too — the pre-fix locators were equal. Divergent
        // content proves both copies survive.
        let file = format!("rollout-2026-07-16T01-00-00-{THREAD_A}.jsonl");
        std::fs::write(d1.join(&file), session_a_content()).unwrap();
        std::fs::write(
            d2.join(&file),
            format!(
                "{}{}\n",
                session_a_content(),
                envelope_line("event_msg", serde_json::json!({"type": "divergent_extra"}))
            ),
        )
        .unwrap();
        // Plus a fork session under a non-UTF-8 dir: lineage must find its
        // edge through the preserved path (the silently-empty lineage bug).
        std::fs::write(
            d1.join(format!("rollout-2026-07-16T03-00-00-{THREAD_FORK}.jsonl")),
            envelope_line(
                "session_meta",
                serde_json::json!({"id": THREAD_FORK, "forked_from_id": THREAD_A}),
            ) + "\n",
        )
        .unwrap();

        let p = CodexProvider::new(tmp.path());
        let sessions = p.sessions().unwrap();
        // ONE logical session for THREAD_A with TWO distinct artifacts.
        let a = sessions
            .iter()
            .find(|d| d.key.native_id == THREAD_A)
            .unwrap();
        assert_eq!(a.artifacts.len(), 2, "both colliding copies must survive");
        let locators: std::collections::BTreeSet<_> = a
            .artifacts
            .iter()
            .map(|art| art.snapshot.id.locator.clone())
            .collect();
        assert_eq!(locators.len(), 2, "locator encoding must stay injective");
        assert!(p.parse(&a.key).unwrap().validate_provenance().is_empty());

        // The exact fork edge, found through a non-UTF-8 path.
        let edges = p.lineage().unwrap();
        assert!(
            edges.iter().any(|e| e.kind == LineageEdgeKind::Fork
                && e.from.native_id == THREAD_A
                && e.to.native_id == THREAD_FORK),
            "fork edge lost under non-UTF-8 paths: {edges:?}"
        );

        // Divergent two-frame archive: both artifacts' exact bytes framed.
        let mut bundle = Vec::new();
        p.write_archive(&key(THREAD_A), &mut bundle).unwrap();
        let newline = bundle.iter().position(|b| *b == b'\n').unwrap();
        let manifest: serde_json::Value = serde_json::from_slice(&bundle[..newline]).unwrap();
        let artifacts = manifest["manifest"]["artifacts"].as_array().unwrap();
        assert_eq!(artifacts.len(), 2);
        let mut offset = newline + 1;
        let mut frames = Vec::new();
        for art in artifacts {
            let len = art["bytes"].as_u64().unwrap() as usize;
            frames.push(bundle[offset..offset + len].to_vec());
            offset += len;
        }
        assert_eq!(offset, bundle.len());
        assert_ne!(frames[0], frames[1], "divergent copies must both be framed");
    }

    #[cfg(windows)]
    #[test]
    fn windows_unpaired_surrogates_stay_distinct() {
        use std::ffi::OsString;
        use std::os::windows::ffi::OsStringExt;
        // Two paths differing only in an unpaired surrogate: to_string_lossy
        // collapses both to the replacement character.
        let a = PathBuf::from(OsString::from_wide(&[0x64, 0xD800, 0x31]));
        let b = PathBuf::from(OsString::from_wide(&[0x64, 0xD801, 0x31]));
        assert_eq!(a.to_string_lossy(), b.to_string_lossy());
        assert_ne!(
            encode_locator(&a),
            encode_locator(&b),
            "u16-unit encoding must stay injective"
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn special_files_are_skipped() {
        let (tmp, p) = fixture();
        let day = tmp.path().join("sessions/2026/07/16");
        let fifo = day.join(format!("rollout-2026-07-16T09-09-09-{THREAD_FORK}.jsonl"));
        let status = std::process::Command::new("mkfifo")
            .arg(&fifo)
            .status()
            .expect("mkfifo available on linux");
        assert!(status.success());
        // Discovery completes without blocking, and the FIFO is not an
        // artifact (the fork session keeps exactly its one regular file).
        let sessions = p.sessions().unwrap();
        let fork = sessions
            .iter()
            .find(|d| d.key.native_id == THREAD_FORK)
            .unwrap();
        assert_eq!(fork.artifacts.len(), 1);
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_tree_root_is_supported() {
        use std::os::unix::fs::symlink;
        let tmp = tempfile::tempdir().unwrap();
        let real = tmp.path().join("real-store/2026/07/16");
        std::fs::create_dir_all(&real).unwrap();
        std::fs::write(
            real.join(format!("rollout-2026-07-16T23-38-33-{THREAD_A}.jsonl")),
            session_a_content(),
        )
        .unwrap();
        let home = tmp.path().join("home");
        std::fs::create_dir_all(&home).unwrap();
        symlink(tmp.path().join("real-store"), home.join("sessions")).unwrap();
        let p = CodexProvider::new(&home);
        assert_eq!(p.sessions().unwrap().len(), 1, "root symlink is followed");
    }

    // Extend the non-UTF-8 round-trip with the archive tier (the design doc
    // claims it; make it true).
    #[test]
    fn drift_report_flags_unknown_vocabulary_not_b1_unknown_dispositions() {
        let (_tmp, p) = fixture();
        let report = p.drift_report().unwrap();
        // Envelope sessions scanned; the legacy file counted separately.
        assert!(report.sessions >= 3);
        assert_eq!(report.legacy_sessions, 1);
        // The fixture's future envelope type is drift; known types are not.
        assert!(report
            .unknown_envelope_types
            .contains_key("brand_new_type_v99"));
        assert!(report.envelope_types.contains_key("session_meta"));
        assert!(!report.unknown_envelope_types.contains_key("session_meta"));
        // Reasoning availability is measured (none in this fixture).
        assert_eq!(report.reasoning_items, 0);
    }

    #[test]
    fn drift_reports_the_committed_nested_future_field_exactly() {
        let bytes = std::fs::read(fixture_path("envelope_session.jsonl")).unwrap();
        let (_t, p) = home_with(THREAD_A, &bytes, false);
        let report = p.drift_report().unwrap();
        assert_eq!(
            report
                .unknown_field_paths
                .get("event_msg/token_count/nested_future_field"),
            Some(&1),
            "the committed nested drift fixture must be reported at its exact path: {:?}",
            report.unknown_field_paths
        );
        // Known keys are not drift.
        assert!(!report
            .unknown_field_paths
            .keys()
            .any(|k| k.ends_with("/info") || k.ends_with("/rate_limits")));
    }

    #[test]
    fn drift_counts_missing_type_discriminators() {
        let content = format!(
            "{}\n{{\"timestamp\":\"2026-07-16T23:40:00.000Z\",\"payload\":{{}}}}\n{{\"timestamp\":\"2026-07-16T23:41:00.000Z\",\"type\":42,\"payload\":{{}}}}\n",
            envelope_line("session_meta", serde_json::json!({"id": THREAD_A}))
        );
        let (_t, p) = home_with(THREAD_A, content.as_bytes(), false);
        let report = p.drift_report().unwrap();
        assert_eq!(report.missing_type_discriminators, 2);
    }

    #[test]
    fn drift_classifies_active_tail_as_transient_not_permanent() {
        let content = format!(
            "{}\n{}",
            envelope_line("session_meta", serde_json::json!({"id": THREAD_A})),
            r#"{"timestamp":"2026-07-16T23:59:59.000Z","type":"response_item","payload":{"type":"mess"#
        );
        let (_t, p) = home_with(THREAD_A, content.as_bytes(), false);
        let report = p.drift_report().unwrap();
        assert_eq!(report.active_tails, 1, "{report:?}");
        assert_eq!(
            report.unparseable, 0,
            "a partial tail is not permanent drift"
        );
    }

    #[test]
    fn archived_malformed_tail_is_permanent_corruption_not_transient() {
        let content = format!(
            "{}\n{}",
            envelope_line("session_meta", serde_json::json!({"id": THREAD_A})),
            r#"{"timestamp":"2026-07-16T23:59:59.000Z","type":"response_item","payload":{"type":"mess"#
        );
        let tmp = tempfile::tempdir().unwrap();
        let day = tmp.path().join("archived_sessions/2026/07/16");
        std::fs::create_dir_all(&day).unwrap();
        std::fs::write(
            day.join(format!("rollout-2026-07-16T23-38-33-{THREAD_A}.jsonl")),
            content,
        )
        .unwrap();
        let p = CodexProvider::new(tmp.path());
        let report = p.drift_report().unwrap();
        // An archived file is finalized: identical damage is corruption.
        assert_eq!(report.active_tails, 0, "{report:?}");
        assert_eq!(report.unparseable, 1, "{report:?}");
    }

    #[test]
    fn drift_counts_missing_payload_discriminators() {
        let content = [
            envelope_line("session_meta", serde_json::json!({"id": THREAD_A})),
            envelope_line(
                "response_item",
                serde_json::json!({"type": 42, "oops": true}),
            ),
            envelope_line("event_msg", serde_json::json!({"no_type_at_all": 1})),
        ]
        .join("\n")
            + "\n";
        let (_t, p) = home_with(THREAD_A, content.as_bytes(), false);
        let report = p.drift_report().unwrap();
        assert_eq!(report.missing_payload_discriminators, 2, "{report:?}");
        // The envelope discriminators were fine.
        assert_eq!(report.missing_type_discriminators, 0);
    }

    #[test]
    fn drift_coverage_is_machine_visible() {
        let content = [
            envelope_line("session_meta", serde_json::json!({"id": THREAD_A})),
            // Baselined: checked.
            envelope_line(
                "event_msg",
                serde_json::json!({"type": "token_count", "info": {}}),
            ),
            // Known vocabulary but NOT baselined: must appear as unbaselined,
            // never as silent "zero drift".
            envelope_line(
                "event_msg",
                serde_json::json!({"type": "web_search_end", "query": "q"}),
            ),
        ]
        .join("\n")
            + "\n";
        let (_t, p) = home_with(THREAD_A, content.as_bytes(), false);
        let report = p.drift_report().unwrap();
        assert_eq!(report.field_schema_checked_records, 1);
        assert_eq!(
            report
                .unbaselined_payload_types
                .get("event_msg/web_search_end"),
            Some(&1),
            "{report:?}"
        );
    }

    #[test]
    fn drift_buckets_reasoning_availability_by_era() {
        // March (summaries present) vs April (encrypted-only): the aggregate
        // ratio must not be the only signal — the exact original research
        // error.
        let content = [
            envelope_line("session_meta", serde_json::json!({"id": THREAD_A})),
            serde_json::json!({"timestamp": "2026-03-15T10:00:00.000Z", "type": "response_item", "payload": {"type": "reasoning", "summary": [{"type": "summary_text", "text": "march headline"}], "encrypted_content": "x"}}).to_string(),
            serde_json::json!({"timestamp": "2026-04-15T10:00:00.000Z", "type": "response_item", "payload": {"type": "reasoning", "summary": [], "encrypted_content": "y"}}).to_string(),
        ]
        .join("\n")
            + "\n";
        let (_t, p) = home_with(THREAD_A, content.as_bytes(), false);
        let report = p.drift_report().unwrap();
        assert_eq!(report.reasoning_by_month.get("2026-03"), Some(&(1, 1)));
        assert_eq!(report.reasoning_by_month.get("2026-04"), Some(&(1, 0)));
    }

    #[test]
    fn one_unreadable_session_does_not_suppress_healthy_results() {
        let tmp = tempfile::tempdir().unwrap();
        let day = tmp.path().join("sessions/2026/07/16");
        std::fs::create_dir_all(&day).unwrap();
        // Healthy session.
        std::fs::write(
            day.join(format!("rollout-2026-07-16T23-38-33-{THREAD_A}.jsonl")),
            session_a_content(),
        )
        .unwrap();
        // A compressed artifact whose OPEN fails (compressed-input cap):
        // the genuine unreadable path. (Garbage .zst bytes do NOT trigger
        // it — libzstd errors lazily on first read, so those sessions are
        // scanned and show up as unparseable records instead; either way
        // nothing is silently skipped.)
        std::fs::write(
            day.join(format!("rollout-2026-07-16T22-00-00-{THREAD_B}.jsonl.zst")),
            b"this is not a zstd frame at all",
        )
        .unwrap();
        let p = CodexProvider::new(tmp.path()).with_max_compressed(4);
        let report = p.drift_report().unwrap();
        assert!(report.sessions >= 1, "healthy session must be reported");
        assert_eq!(
            report.unreadable_sessions, 1,
            "the unopenable session must be COUNTED, not silently skipped: {report:?}"
        );
        assert!(
            report.envelope_types.contains_key("session_meta"),
            "healthy content present: {report:?}"
        );
    }

    #[test]
    fn drift_report_measures_reasoning_summary_availability() {
        let content = [
            envelope_line("session_meta", serde_json::json!({"id": THREAD_A})),
            envelope_line("response_item", serde_json::json!({"type": "reasoning", "summary": [{"type": "summary_text", "text": "thinking headline"}], "encrypted_content": "xxx"})),
            envelope_line("response_item", serde_json::json!({"type": "reasoning", "summary": [], "encrypted_content": "yyy"})),
        ]
        .join("\n")
            + "\n";
        let (_t, p) = home_with(THREAD_A, content.as_bytes(), false);
        let report = p.drift_report().unwrap();
        assert_eq!(report.reasoning_items, 2);
        assert_eq!(report.reasoning_with_summary, 1);
    }

    #[test]
    fn cache_token_covers_policy_and_descriptor_state() {
        let (_tmp, p) = fixture();
        let k = key(THREAD_B);
        // Stable across calls.
        let t1 = p.parse_cache_token(&k).unwrap();
        let t2 = p.parse_cache_token(&k).unwrap();
        assert_eq!(t1, t2);
        // Policy inputs change the token with the SAME storage root — the
        // only changed input is the limit, so this genuinely proves policy
        // participation (a second fixture would differ for root/locator
        // reasons alone).
        let strict = CodexProvider::new(p.codex_home.clone()).with_max_decompressed(1234);
        assert_ne!(t1, strict.parse_cache_token(&k).unwrap());
    }

    #[test]
    fn descriptor_token_is_injective_under_hostile_field_contents() {
        use super::super::descriptor_state_token;
        // These two states COLLIDE under the previous \x1f-joined encoding:
        // locator "a\x1fb" + revision "c" vs locator "a" + revision
        // "b\x1fc" join to identical bytes. Length-prefixing must
        // distinguish them.
        let art = |locator: &str, revision: &str| SessionArtifact {
            snapshot: ArtifactSnapshot {
                id: ArtifactId {
                    provider_instance: "r".into(),
                    locator: locator.into(),
                },
                revision: ArtifactRevision(revision.into()),
            },
            form: ArtifactForm::PlainFile,
            archived: false,
        };
        let d1 = SessionDescriptor {
            key: key(THREAD_A),
            artifacts: vec![art("a\u{1f}b", "c")],
        };
        let d2 = SessionDescriptor {
            key: key(THREAD_A),
            artifacts: vec![art("a", "b\u{1f}c")],
        };
        assert_ne!(
            descriptor_state_token(&d1),
            descriptor_state_token(&d2),
            "delimiter smuggling must not collide tokens"
        );
    }

    #[test]
    fn cache_token_gates_the_provider_keyed_cache_end_to_end() {
        use crate::cache::LruCache;
        let (_tmp, p) = fixture();
        let k = key(THREAD_B);
        let token = p.parse_cache_token(&k).unwrap();
        let parsed_len = p.parse(&k).unwrap().entries.len();

        let mut cache: LruCache<usize> = LruCache::new(8, 1024);
        cache.insert_keyed(&k, token.clone(), parsed_len, 64);
        // Same provider config: hit.
        assert_eq!(
            cache.get_keyed(&k, &p.parse_cache_token(&k).unwrap()),
            Some(&parsed_len)
        );
        // Stricter limits over the SAME root => different token => the
        // cached parse from the laxer configuration is NOT shared. (Same
        // root is essential: a second fixture's token would differ for
        // root/locator reasons and prove nothing about the policy.)
        let strict = CodexProvider::new(p.codex_home.clone()).with_max_decompressed(16);
        assert_eq!(
            cache.get_keyed(&k, &strict.parse_cache_token(&k).unwrap()),
            None,
            "different safety limits must never share a cached parse"
        );
    }

    #[test]
    fn descriptor_token_distinguishes_preferred_flip_with_identical_revisions() {
        use super::super::descriptor_state_token;
        let art = |instance: &str, locator: &str| SessionArtifact {
            snapshot: ArtifactSnapshot {
                id: ArtifactId {
                    provider_instance: instance.into(),
                    locator: locator.into(),
                },
                revision: ArtifactRevision("same-rev".into()),
            },
            form: ArtifactForm::PlainFile,
            archived: false,
        };
        let both = SessionDescriptor {
            key: key(THREAD_A),
            artifacts: vec![art("r", "a.jsonl"), art("r", "b.jsonl")],
        };
        let only_b = SessionDescriptor {
            key: key(THREAD_A),
            artifacts: vec![art("r", "b.jsonl")],
        };
        // Identical revision TEXT everywhere, but the artifact set and the
        // selected preferred artifact differ — the round-11 stale-hit
        // scenario the plain revision string could not distinguish.
        assert_ne!(
            descriptor_state_token(&both),
            descriptor_state_token(&only_b)
        );
        // (The canonical encoding has no human-readable marker; the
        // preferred artifact id is covered by the trailing length-prefixed
        // fields, proven by the inequality above.)
    }

    #[test]
    fn drift_vocabulary_is_capped_and_escaped_during_collection() {
        // Round-16/17 security guardrail: field names are attacker-controlled
        // strings. Cardinality and length are capped while COLLECTING (not at
        // rendering), overflow is counted, and control characters can never
        // reach the stored report.
        let mut lines = vec![envelope_line(
            "session_meta",
            serde_json::json!({"id": THREAD_A, "cwd": "/tmp/p"}),
        )];
        // 65 distinct unknown nested fields on a baselined type (cap is 64):
        // 63 plain, one with an ANSI-escape name (inside the cap), and one
        // exceeding the length cap (arrives last, overflowing cardinality).
        let mut hostile = serde_json::Map::new();
        hostile.insert("type".into(), "token_count".into());
        hostile.insert("info".into(), serde_json::json!({}));
        for i in 0..63 {
            hostile.insert(format!("evil{i}"), serde_json::json!(1));
        }
        hostile.insert("\u{1b}[31minjected".into(), serde_json::json!(1));
        hostile.insert("L".repeat(500), serde_json::json!(1));
        lines.push(envelope_line(
            "event_msg",
            serde_json::Value::Object(hostile),
        ));
        let content = lines.join("\n") + "\n";
        let (_tmp, p) = home_with(THREAD_A, content.as_bytes(), false);

        let report = p.drift_report().unwrap();
        assert!(
            report.unknown_field_paths.len() <= MAX_VOCAB_KEYS,
            "cardinality cap must hold during collection, got {}",
            report.unknown_field_paths.len()
        );
        assert!(
            report.vocabulary_keys_dropped > 0,
            "overflow past the cap must be counted"
        );
        assert!(
            report.vocabulary_keys_truncated > 0,
            "length-capped keys must be counted"
        );
        for key in report.unknown_field_paths.keys() {
            assert!(
                !key.chars().any(char::is_control),
                "stored key contains a raw control character: {key:?}"
            );
            assert!(
                key.chars().count() <= MAX_VOCAB_KEY_LEN,
                "stored key exceeds the length cap (marker included): {key:?}"
            );
        }
        // The escape sequence survives only in escaped form.
        assert!(
            report
                .unknown_field_paths
                .keys()
                .any(|k| k.contains("\\u{1b}[31minjected")),
            "escaped control character not found: {:?}",
            report.unknown_field_paths.keys().collect::<Vec<_>>()
        );
    }

    #[test]
    fn vocab_key_length_cap_applies_to_escaped_representation() {
        // Round-18: escape_debug expands control characters, so the cap
        // must bound the ESCAPED output — 300 raw control chars would
        // otherwise store ~1800 chars.
        let mut truncated = 0u64;
        let hostile: String = "\u{1}".repeat(300);
        let out = sanitize_vocab_key(&hostile, &mut truncated);
        assert!(
            out.chars().count() <= MAX_VOCAB_KEY_LEN,
            "escaped key exceeds cap: {} chars",
            out.chars().count()
        );
        assert_eq!(truncated, 1);
        assert!(!out.chars().any(char::is_control));
    }

    #[test]
    fn zero_and_huge_limits_keep_default_caps_and_canonical_tokens() {
        // Round-19 blocker 2: a zero user limit means "no additional cap" —
        // it must NOT disable the default bomb guards, and zero/omitted/huge
        // must all produce the identical provider state and cache token (no
        // behaviorally redundant token variants).
        let (tmp, p) = home_with(THREAD_A, session_a_content().as_bytes(), false);
        let k = key(THREAD_A);
        let default_token = p.parse_cache_token(&k).unwrap();

        let zero = CodexProvider::new(tmp.path()).tighten_limits(0);
        assert_eq!(zero.parse_cache_token(&k).unwrap(), default_token);
        assert!(
            zero.parse(&k).is_ok(),
            "defaults still guard and still parse"
        );

        let huge = CodexProvider::new(tmp.path()).tighten_limits(u64::MAX);
        assert_eq!(huge.parse_cache_token(&k).unwrap(), default_token);
    }

    #[test]
    fn surface_size_limit_changes_token_and_bounds_plain_parses() {
        // Round-18 blocker 4: the surface's --max-file-size must reach the
        // provider, change its parse cache token, and actually bound a
        // production parse — for PLAIN files too, not only compressed ones.
        let (tmp, p) = home_with(THREAD_A, session_a_content().as_bytes(), false);
        let k = key(THREAD_A);
        let default_token = p.parse_cache_token(&k).unwrap();
        assert!(p.parse(&k).is_ok(), "default limits parse the fixture");

        let tight = CodexProvider::new(tmp.path()).tighten_limits(4);
        let tight_token = tight.parse_cache_token(&k).unwrap();
        assert_ne!(
            default_token, tight_token,
            "a changed limit must change the cache token"
        );
        let err = tight
            .parse(&k)
            .expect_err("4-byte limit must refuse the plain file");
        assert!(err.to_string().contains("size limit"), "got: {err}");

        // Tightening never loosens: a huge limit keeps the defaults.
        let loose = CodexProvider::new(tmp.path()).tighten_limits(u64::MAX);
        assert_eq!(loose.parse_cache_token(&k).unwrap(), default_token);
    }

    #[test]
    fn diagnostics_hook_returns_capped_report() {
        let (_tmp, p) = home_with(THREAD_A, session_a_content().as_bytes(), false);
        let value = p.diagnostics().unwrap().expect("codex has diagnostics");
        assert_eq!(value["sessions"], 1);
        assert!(value["vocabulary_keys_dropped"].is_u64());
        // No session ids or file paths in the report (aggregate only).
        let text = value.to_string();
        assert!(
            !text.contains(THREAD_A) && !text.contains("rollout-"),
            "diagnostics must not leak session ids or file paths: {text}"
        );
    }

    #[test]
    fn cached_consumer_revalidates_on_artifact_revision_change() {
        // Round-17 guardrail: the production cache consumer must reparse
        // when an artifact revision changes BETWEEN lookups, and must serve
        // from cache when nothing changed.
        use crate::cache::CacheManager;
        use crate::config::CacheConfig;
        use crate::provider::registry::cached_parsed_session;

        let (tmp, p) = home_with(THREAD_A, session_a_content().as_bytes(), false);
        let cache = CacheManager::new(&CacheConfig {
            enabled: true,
            ..Default::default()
        });

        let first = cached_parsed_session(&cache, &p, &key(THREAD_A)).unwrap();
        let n = first.entries.len();
        assert_eq!(n, 3, "meta + brand-new preserved, user message mapped");
        assert_eq!(
            first.record_dispositions.len(),
            4,
            "one disposition per record, retained in the bundle"
        );

        // Unchanged revision: the same Arc comes back — a genuine cache hit.
        let again = cached_parsed_session(&cache, &p, &key(THREAD_A)).unwrap();
        assert!(
            std::sync::Arc::ptr_eq(&first, &again),
            "unchanged revision must be served from cache"
        );

        // Append one well-formed record: file length changes, so the
        // artifact revision — and therefore the parse cache token — moves.
        let file = tmp.path().join(format!(
            "sessions/2026/07/16/rollout-2026-07-16T23-38-33-{THREAD_A}.jsonl"
        ));
        let extra = envelope_line(
            "response_item",
            serde_json::json!({"type": "message", "role": "assistant", "content": []}),
        ) + "\n";
        use std::io::Write as _;
        let mut f = std::fs::OpenOptions::new()
            .append(true)
            .open(&file)
            .unwrap();
        f.write_all(extra.as_bytes()).unwrap();
        drop(f);

        let after = cached_parsed_session(&cache, &p, &key(THREAD_A)).unwrap();
        assert!(
            !std::sync::Arc::ptr_eq(&first, &after),
            "revision change must invalidate the cached parse"
        );
        assert_eq!(
            after.entries.len(),
            n + 1,
            "reparse must see the appended record"
        );
        assert_eq!(
            after.record_dispositions.len(),
            5,
            "provenance tracks the reparse too"
        );
    }

    // ========================================================================
    // B3 slice 1: normalization
    // ========================================================================

    fn normalize_home(lines: &[String]) -> (tempfile::TempDir, CodexProvider) {
        let content = lines.join("\n") + "\n";
        home_with(THREAD_A, content.as_bytes(), false)
    }

    /// Parse a fixture and return, alongside it, the native record stream as
    /// `(ordinal, value)` pairs — the INDEPENDENT input for the usage-
    /// allocation audit (never taken from normalized output).
    fn parse_and_raw(
        lines: &[String],
    ) -> (
        tempfile::TempDir,
        crate::provider::ParsedSession,
        Vec<(u64, serde_json::Value)>,
    ) {
        let (tmp, p) = normalize_home(lines);
        let parsed = p.parse(&key(THREAD_A)).unwrap();
        let raw = lines
            .iter()
            .enumerate()
            .map(|(i, l)| (i as u64, serde_json::from_str(l).unwrap()))
            .collect();
        (tmp, parsed, raw)
    }

    fn joined_content(payload: &serde_json::Value) -> String {
        payload["content"]
            .as_array()
            .map(|items| {
                items
                    .iter()
                    .filter_map(|i| i["text"].as_str())
                    .collect::<Vec<_>>()
                    .join("")
            })
            .unwrap_or_default()
    }

    fn is_window_boundary_raw(val: &serde_json::Value) -> bool {
        let et = val["type"].as_str().unwrap_or("");
        let pt = val["payload"]["type"].as_str().unwrap_or("");
        matches!(et, "session_meta" | "turn_context" | "compacted")
            || (et == "event_msg" && pt == "task_started")
    }

    fn is_audit_boundary(
        ordinal: u64,
        val: &serde_json::Value,
        first_new_after_fork: Option<u64>,
    ) -> bool {
        is_window_boundary_raw(val) || first_new_after_fork == Some(ordinal)
    }

    /// Independently derive the old-format copied prefix. This intentionally
    /// does not call `embedded_fork_parent`, `copied_record_eq`, or
    /// `copied_history_range`: it is the source-side oracle for those
    /// production rules.
    fn independent_embedded_parent_id(
        session_key: &LogicalSessionKey,
        child: &[(u64, serde_json::Value)],
    ) -> Option<String> {
        let [(zero, first), (one, second), ..] = child else {
            return None;
        };
        if *zero != 0
            || *one != 1
            || first["type"] != "session_meta"
            || second["type"] != "session_meta"
            || first["payload"]["id"].as_str() != Some(&session_key.native_id)
        {
            return None;
        }
        let parent_id = second["payload"]["id"].as_str()?;
        (parent_id != session_key.native_id && uuid::Uuid::parse_str(parent_id).is_ok())
            .then(|| parent_id.to_string())
    }

    fn independent_copied_history(
        provider: &CodexProvider,
        session_key: &LogicalSessionKey,
        child: &[(u64, serde_json::Value)],
    ) -> Option<(String, (u64, u64))> {
        let parent_id = independent_embedded_parent_id(session_key, child)?;
        let (_, parent_path) = provider.resolve(&key(&parent_id)).ok()?;
        let mut reader = provider.open_records(&parent_path).ok()?;
        let mut line = Vec::new();
        let mut last = None;
        let mut expected_child = 1_u64;
        for (child_ordinal, child_value) in child.iter().skip(1) {
            if *child_ordinal != expected_child {
                break;
            }
            line.clear();
            match reader.read_until(b'\n', &mut line) {
                Ok(0) | Err(_) => break,
                Ok(_) => {}
            }
            let Ok(mut parent_value) = serde_json::from_slice::<serde_json::Value>(&line) else {
                break;
            };
            let mut child_value = child_value.clone();
            parent_value
                .as_object_mut()
                .expect("envelope object")
                .remove("timestamp");
            child_value
                .as_object_mut()
                .expect("envelope object")
                .remove("timestamp");
            if parent_value != child_value {
                break;
            }
            last = Some(*child_ordinal);
            expected_child = expected_child.saturating_add(1);
        }
        last.map(|last| (parent_id, (1, last)))
    }

    /// Verify the exact activity partition and that producing edges never
    /// cross the fork boundary. Expectations come from entry ids and the
    /// independently proven native prefix, not from emitted semantics.
    fn audit_inherited_activity(
        key: &LogicalSessionKey,
        inherited: Option<(u64, u64)>,
        session: &crate::provider::ParsedSession,
    ) -> Vec<String> {
        let in_range =
            |ordinal: u64| inherited.is_some_and(|(first, last)| (first..=last).contains(&ordinal));
        let mut violations = Vec::new();
        for entry in &session.entries {
            let expected = if in_range(entry.id.ordinal) {
                ActivityKind::InheritedHistory
            } else {
                ActivityKind::New
            };
            let actual = session
                .semantics
                .get(&entry.id)
                .map_or(ActivityKind::New, |s| s.activity);
            if actual != expected {
                violations.push(format!(
                    "entry #{} activity is {actual:?}, expected {expected:?}",
                    entry.id.ordinal
                ));
            }
            if entry.id.session != *key {
                violations.push(format!(
                    "entry #{} belongs to another session",
                    entry.id.ordinal
                ));
            }
        }
        for disposition in &session.record_dispositions {
            let produced = match &disposition.outcome {
                RecordOutcome::Mapped(ids)
                | RecordOutcome::Unknown { entries: ids }
                | RecordOutcome::Recovered { entries: ids, .. } => Some(ids),
                RecordOutcome::Suppressed { .. } | RecordOutcome::Unparseable { .. } => None,
            };
            if let Some(ids) = produced {
                for id in ids {
                    if in_range(disposition.record.ordinal) != in_range(id.ordinal) {
                        violations.push(format!(
                            "record #{} crosses the inherited/new boundary to entry #{}",
                            disposition.record.ordinal, id.ordinal
                        ));
                    }
                }
            }
            if let RecordOutcome::Suppressed {
                reason: super::super::SuppressionReason::DuplicateStream { twin },
            } = &disposition.outcome
            {
                if in_range(disposition.record.ordinal) != in_range(twin.ordinal) {
                    violations.push(format!(
                        "duplicate record #{} crosses the inherited/new boundary to twin #{}",
                        disposition.record.ordinal, twin.ordinal
                    ));
                }
            }
        }
        violations
    }

    /// Claim-once helper for independent twin matching.
    fn claim_first(pool: &mut [(String, bool)], text: &str) -> bool {
        if let Some(c) = pool.iter_mut().find(|(t, claimed)| !*claimed && t == text) {
            c.1 = true;
            true
        } else {
            false
        }
    }

    /// Event ordinals the normalizer SHOULD suppress, derived independently
    /// from the native stream (fresh code, never the emitted dispositions):
    /// window-scoped identical-event dedup plus content-confirmed twin
    /// matching. Needed only to know which agent events are NOT assistant
    /// entries (and so cannot be usage owners).
    fn independent_suppressed(
        raw: &[(u64, serde_json::Value)],
        first_new_after_fork: Option<u64>,
    ) -> std::collections::BTreeSet<u64> {
        let mut suppressed = std::collections::BTreeSet::new();
        let mut start = 0usize;
        let mut i = 0usize;
        loop {
            let at_boundary =
                i == raw.len() || is_audit_boundary(raw[i].0, &raw[i].1, first_new_after_fork);
            if at_boundary && i > start {
                independent_suppress_window(&raw[start..i], &mut suppressed);
                start = i;
            }
            if i == raw.len() {
                break;
            }
            i += 1;
        }
        suppressed
    }

    /// Human prompt delivery expected from the native stream, independently
    /// of the normalizer's match plan or semantic sidecar.
    ///
    /// A response_item user message with a claim-once, same-window
    /// user_message event is a turn boundary. A unique user_message event
    /// without such a response twin is same-turn input (Codex steering).
    /// Exact duplicate native events are one emission and add no expectation.
    fn independent_prompt_expectations(
        raw: &[(u64, serde_json::Value)],
        first_new_after_fork: Option<u64>,
    ) -> (
        std::collections::BTreeSet<u64>,
        std::collections::BTreeSet<u64>,
    ) {
        let mut boundaries = std::collections::BTreeSet::new();
        let mut midturn = std::collections::BTreeSet::new();
        let mut start = 0usize;
        let mut i = 0usize;
        loop {
            let at_boundary =
                i == raw.len() || is_audit_boundary(raw[i].0, &raw[i].1, first_new_after_fork);
            if at_boundary && i > start {
                let window = &raw[start..i];
                let mut representatives: std::collections::HashSet<(String, String, String)> =
                    std::collections::HashSet::new();
                let mut users: Vec<(u64, String, bool)> = window
                    .iter()
                    .filter(|(_, val)| {
                        val["type"] == "response_item"
                            && val["payload"]["type"] == "message"
                            && val["payload"]["role"] == "user"
                    })
                    .map(|(ordinal, val)| (*ordinal, joined_content(&val["payload"]), false))
                    .collect();

                for (ordinal, val) in window {
                    if val["type"] != "event_msg" || val["payload"]["type"] != "user_message" {
                        continue;
                    }
                    let fingerprint = (
                        val["payload"]["type"].as_str().unwrap_or("").to_string(),
                        val["payload"].to_string(),
                        val["timestamp"].as_str().unwrap_or("").to_string(),
                    );
                    if !representatives.insert(fingerprint) {
                        continue;
                    }
                    let text = val["payload"]["message"].as_str().unwrap_or("");
                    if let Some(candidate) = users
                        .iter_mut()
                        .find(|(_, candidate, claimed)| !*claimed && candidate == text)
                    {
                        candidate.2 = true;
                        boundaries.insert(candidate.0);
                    } else {
                        midturn.insert(*ordinal);
                    }
                }
            }
            if i == raw.len() {
                break;
            }
            if at_boundary {
                start = i;
            }
            i += 1;
        }
        (boundaries, midturn)
    }

    #[derive(Default)]
    struct PromptAudit {
        boundary_count: usize,
        midturn_count: usize,
        violations: Vec<String>,
    }

    /// Source-derived audit for human-prompt authorship and delivery.
    ///
    /// Expected ids and axes come only from native records. Emitted semantics
    /// are read afterward as the object under test, and mutation controls
    /// prove that boundary/midturn and harness/human mistakes are rejected.
    fn audit_prompt_semantics(
        key: &LogicalSessionKey,
        raw: &[(u64, serde_json::Value)],
        session: &crate::provider::ParsedSession,
        first_new_after_fork: Option<u64>,
    ) -> PromptAudit {
        use crate::provider::{PromptAuthorship, PromptDelivery};

        let (boundaries, midturn) = independent_prompt_expectations(raw, first_new_after_fork);
        let mut expected: std::collections::BTreeMap<crate::provider::EntryId, PromptDelivery> =
            std::collections::BTreeMap::new();
        for ordinal in &boundaries {
            expected.insert(
                crate::provider::EntryId::deterministic(key, *ordinal, 0),
                PromptDelivery::TurnBoundary,
            );
        }
        for ordinal in &midturn {
            expected.insert(
                crate::provider::EntryId::deterministic(key, *ordinal, 0),
                PromptDelivery::MidTurn,
            );
        }

        let entries: std::collections::BTreeMap<_, _> =
            session.entries.iter().map(|e| (&e.id, &e.entry)).collect();
        let mut violations = Vec::new();
        for (id, delivery) in &expected {
            if !matches!(entries.get(id), Some(crate::model::LogEntry::User(_))) {
                violations.push(format!(
                    "expected human prompt entry {id} is missing or not user"
                ));
                continue;
            }
            match session.semantics.get(id).and_then(|s| s.prompt) {
                Some(prompt) if prompt.authorship != PromptAuthorship::Human => violations.push(
                    format!("expected human prompt {id} has non-human authorship"),
                ),
                Some(prompt) if prompt.delivery != *delivery => violations.push(format!(
                    "expected human prompt {id} has wrong delivery (expected {delivery:?})"
                )),
                None => violations.push(format!("expected human prompt {id} has no semantics")),
                _ => {}
            }
        }

        for (id, sem) in &session.semantics {
            if sem
                .prompt
                .is_some_and(|p| p.authorship == PromptAuthorship::Human)
                && !expected.contains_key(id)
            {
                violations.push(format!("unexpected human prompt semantics on {id}"));
            }
        }

        PromptAudit {
            boundary_count: boundaries.len(),
            midturn_count: midturn.len(),
            violations,
        }
    }

    fn independent_suppress_window(
        window: &[(u64, serde_json::Value)],
        suppressed: &mut std::collections::BTreeSet<u64>,
    ) {
        // Identical native events (type + payload + timestamp) are one
        // emission; later copies are suppressed.
        let mut reps: std::collections::HashMap<(String, String, String), u64> =
            std::collections::HashMap::new();
        let mut dups: std::collections::BTreeSet<u64> = std::collections::BTreeSet::new();
        for (o, val) in window {
            let et = val["type"].as_str().unwrap_or("");
            let pt = val["payload"]["type"].as_str().unwrap_or("");
            if et != "event_msg"
                || !matches!(
                    pt,
                    "user_message"
                        | "agent_message"
                        | "agent_reasoning"
                        | "agent_reasoning_raw_content"
                )
            {
                continue;
            }
            let fp = (
                pt.to_string(),
                val["payload"].to_string(),
                val["timestamp"].as_str().unwrap_or("").to_string(),
            );
            if reps.insert(fp, *o).is_some() {
                dups.insert(*o);
            }
        }
        // Twin candidates from response_items in this window.
        let mut users: Vec<(String, bool)> = Vec::new();
        let mut agents: Vec<(String, bool)> = Vec::new();
        let mut sections: Vec<(String, bool)> = Vec::new();
        for (_, val) in window {
            if val["type"] != "response_item" {
                continue;
            }
            match val["payload"]["type"].as_str().unwrap_or("") {
                "message" => match val["payload"]["role"].as_str().unwrap_or("") {
                    "user" => users.push((joined_content(&val["payload"]), false)),
                    "assistant" => agents.push((joined_content(&val["payload"]), false)),
                    _ => {}
                },
                "reasoning" => {
                    for list in ["summary", "content"] {
                        if let Some(items) = val["payload"][list].as_array() {
                            for item in items {
                                if let Some(t) = item["text"].as_str() {
                                    sections.push((t.to_string(), false));
                                }
                            }
                        }
                    }
                }
                _ => {}
            }
        }
        for (o, val) in window {
            if val["type"] != "event_msg" || dups.contains(o) {
                continue;
            }
            let pt = val["payload"]["type"].as_str().unwrap_or("");
            let matched = match pt {
                "user_message" => {
                    claim_first(&mut users, val["payload"]["message"].as_str().unwrap_or(""))
                }
                "agent_message" => claim_first(
                    &mut agents,
                    val["payload"]["message"].as_str().unwrap_or(""),
                ),
                "agent_reasoning" | "agent_reasoning_raw_content" => {
                    claim_first(&mut sections, val["payload"]["text"].as_str().unwrap_or(""))
                }
                _ => false,
            };
            if matched {
                suppressed.insert(*o);
            }
        }
        suppressed.extend(dups);
    }

    /// INDEPENDENTLY derived usage-allocation audit (round-24/25).
    ///
    /// Every expectation is computed from the native record stream by fresh
    /// rule code; NOTHING is read from emitted observations or normalized
    /// Unknown entries to decide the expected partition/owner. Returns a
    /// list of violation strings (empty = clean). This is the reusable
    /// oracle that both the corpus conformance test and the deliberately-
    /// altered negative-control tests run — a green corpus is evidence only
    /// because these controls prove the audit rejects broken output.
    fn audit_usage_allocation(
        key: &LogicalSessionKey,
        raw: &[(u64, serde_json::Value)],
        session: &crate::provider::ParsedSession,
        first_new_after_fork: Option<u64>,
    ) -> Vec<String> {
        use crate::provider::{RecordOutcome, UsageAggregation, UsageBasis, UsageScope};
        let mut v: Vec<String> = Vec::new();

        // Window index per ordinal (same boundary rule, fresh walk).
        let mut window_of: std::collections::BTreeMap<u64, u64> = std::collections::BTreeMap::new();
        let mut w = 0u64;
        for (o, val) in raw {
            if is_audit_boundary(*o, val, first_new_after_fork) {
                w += 1;
            }
            window_of.insert(*o, w);
        }

        let suppressed = independent_suppressed(raw, first_new_after_fork);

        // Assistant-EMISSION ordinals: native records that become an
        // assistant entry (a valid usage owner).
        let assistant_ordinals: Vec<u64> =
            raw.iter()
                .filter(|(o, val)| {
                    let et = val["type"].as_str().unwrap_or("");
                    let pt = val["payload"]["type"].as_str().unwrap_or("");
                    if et == "response_item" {
                        match pt {
                            "message" => val["payload"]["role"] == "assistant",
                            "reasoning" | "function_call" | "custom_tool_call"
                            | "web_search_call" => true,
                            _ => false,
                        }
                    } else {
                        et == "event_msg"
                            && matches!(
                                pt,
                                "agent_message" | "agent_reasoning" | "agent_reasoning_raw_content"
                            )
                            && !suppressed.contains(o)
                    }
                })
                .map(|(o, _)| *o)
                .collect();

        // Native usage records (token_count carrying a cumulative total).
        let token_ords: Vec<u64> = raw
            .iter()
            .filter(|(_, val)| {
                val["type"] == "event_msg"
                    && val["payload"]["type"] == "token_count"
                    && val["payload"]["info"]["total_token_usage"].is_object()
            })
            .map(|(o, _)| *o)
            .collect();
        let raw_by_ordinal: std::collections::BTreeMap<u64, &serde_json::Value> =
            raw.iter().map(|(o, val)| (*o, val)).collect();
        let triple = |val: &serde_json::Value, sub: &str| -> (u64, u64, u64) {
            let u = &val["payload"]["info"][sub];
            let g = |k: &str| u[k].as_u64().unwrap_or(0);
            (
                g("input_tokens"),
                g("cached_input_tokens"),
                g("output_tokens"),
            )
        };

        // Expected owner per token: most-recent assistant emission BEFORE it
        // in the same window, else the FIRST assistant emission after it in
        // the same window, else preserved (no owner).
        let expected_owner = |t: u64| -> Option<u64> {
            let tw = window_of[&t];
            let before = assistant_ordinals
                .iter()
                .filter(|&&a| a < t && window_of[&a] == tw)
                .max()
                .copied();
            before.or_else(|| {
                assistant_ordinals
                    .iter()
                    .filter(|&&a| a > t && window_of[&a] == tw)
                    .min()
                    .copied()
            })
        };

        // Full source identity: the preferred artifact carries the native
        // records, so an attributed token's expected RecordRef is
        // `{preferred, ordinal}` — same-ordinal artifact swaps are caught
        // (round-25).
        let preferred = session
            .descriptor
            .preferred_artifact()
            .expect("descriptor has a preferred artifact")
            .snapshot
            .id
            .clone();
        let expected_ref = |t: u64| crate::provider::RecordRef {
            artifact: preferred.clone(),
            ordinal: t,
        };

        // Actual state, gathered but never used to decide EXPECTATIONS.
        // Dispositions are keyed by FULL RecordRef.
        let disp_by_ref: std::collections::BTreeMap<crate::provider::RecordRef, &RecordOutcome> =
            session
                .record_dispositions
                .iter()
                .map(|d| (d.record.clone(), &d.outcome))
                .collect();
        let mut obs_by_token: std::collections::BTreeMap<
            u64,
            Vec<(crate::provider::EntryId, &crate::provider::UsageObservation)>,
        > = std::collections::BTreeMap::new();
        for (eid, sem) in &session.semantics {
            for obs in &sem.usage {
                obs_by_token
                    .entry(obs.record.ordinal)
                    .or_default()
                    .push((eid.clone(), obs));
            }
        }
        let preserved_ords: std::collections::BTreeSet<u64> = session
            .entries
            .iter()
            .filter(|e| {
                matches!(&e.entry, crate::model::LogEntry::Unknown(val)
                    if val["payload"]["type"] == "token_count"
                        && val["payload"]["info"]["total_token_usage"].is_object())
            })
            .map(|e| e.id.ordinal)
            .collect();

        // Independent ambiguity of cumulative transitions (source-backed
        // includes-cached basis: fresh = input − cached).
        let fresh = |x: (u64, u64, u64)| x.0.saturating_sub(x.1);
        let mut ambiguous_cumulative: std::collections::BTreeSet<u64> =
            std::collections::BTreeSet::new();
        let mut prev: Option<(u64, u64, u64)> = None;
        for &t in &token_ords {
            let total = triple(raw_by_ordinal[&t], "total_token_usage");
            if let Some(pv) = prev {
                if !(total.0 < pv.0 || total.2 < pv.2) && fresh(total) < fresh(pv) {
                    ambiguous_cumulative.insert(t);
                }
            }
            prev = Some(total);
        }

        // Per-token partition/owner/cardinality checks.
        for &t in &token_ords {
            let attributed = obs_by_token.get(&t).is_some_and(|x| !x.is_empty());
            let preserved = preserved_ords.contains(&t);
            if attributed == preserved {
                v.push(format!(
                    "record #{t} not in exactly one partition (attributed={attributed}, preserved={preserved})"
                ));
            }
            match expected_owner(t) {
                None => {
                    if attributed {
                        v.push(format!(
                            "record #{t} expected preserved (no assistant emission in window) but was attributed"
                        ));
                    }
                }
                Some(owner) => {
                    let owner_id = crate::provider::EntryId::deterministic(key, owner, 0);
                    if preserved {
                        v.push(format!(
                            "record #{t} expected attributed to entry {owner_id} but was preserved"
                        ));
                    }
                    if !attributed {
                        v.push(format!(
                            "record #{t} expected attributed to entry {owner_id} but has no observations"
                        ));
                    }
                    let obs = obs_by_token.get(&t).cloned().unwrap_or_default();
                    for (eid, _) in &obs {
                        if *eid != owner_id {
                            v.push(format!(
                                "record #{t} observation attached to {eid}, expected owner {owner_id}"
                            ));
                        }
                    }
                    match disp_by_ref.get(&expected_ref(t)) {
                        Some(RecordOutcome::Mapped(ids))
                            if ids.as_slice() == [owner_id.clone()] => {}
                        _ => v.push(format!(
                            "record #{t} disposition is not Mapped to owner {owner_id} at the preferred artifact"
                        )),
                    }
                    match session.entry_origins.get(&owner_id) {
                        Some(origins) if origins.contains(&expected_ref(t)) => {}
                        _ => v.push(format!(
                            "owner {owner_id} origins do not include usage record #{t} at the preferred artifact"
                        )),
                    }
                    let call = obs
                        .iter()
                        .filter(|(eid, o)| {
                            *eid == owner_id
                                && matches!(o.scope, UsageScope::Call)
                                && matches!(o.aggregation, UsageAggregation::Delta)
                        })
                        .count();
                    let sess = obs
                        .iter()
                        .filter(|(eid, o)| {
                            *eid == owner_id
                                && matches!(o.scope, UsageScope::Session)
                                && matches!(o.aggregation, UsageAggregation::Cumulative)
                        })
                        .count();
                    let total_on_owner = obs.iter().filter(|(eid, _)| *eid == owner_id).count();
                    if call != 1 {
                        v.push(format!(
                            "record #{t} owner {owner_id} has {call} Call/Delta observations (expected 1)"
                        ));
                    }
                    if sess != 1 {
                        v.push(format!(
                            "record #{t} owner {owner_id} has {sess} Session/Cumulative observations (expected 1)"
                        ));
                    }
                    if total_on_owner != 2 {
                        v.push(format!(
                            "record #{t} owner {owner_id} has {total_on_owner} observations (expected exactly 2)"
                        ));
                    }
                }
            }
        }

        // PER-OWNER canonical reconciliation (round-25): a global sum lets a
        // broken impl move usage between assistants undetected. Expected
        // canonical is accumulated onto each token's independently derived
        // OWNER EntryId, then compared entry by entry — including the
        // requirement that an entry with no expected usage carries none.
        let mut expected_by_owner: std::collections::BTreeMap<
            crate::provider::EntryId,
            (u64, u64, u64),
        > = std::collections::BTreeMap::new();
        let mut prev: Option<(u64, u64, u64)> = None;
        for &t in &token_ords {
            let total = triple(raw_by_ordinal[&t], "total_token_usage");
            let delta = match prev {
                None => (fresh(total), total.1, total.2),
                Some(pv) => {
                    if total.0 < pv.0 || total.2 < pv.2 {
                        (fresh(total), total.1, total.2)
                    } else {
                        (
                            fresh(total).saturating_sub(fresh(pv)),
                            total.1.saturating_sub(pv.1),
                            total.2 - pv.2,
                        )
                    }
                }
            };
            prev = Some(total);
            if let Some(owner) = expected_owner(t) {
                let e = expected_by_owner
                    .entry(crate::provider::EntryId::deterministic(key, owner, 0))
                    .or_default();
                e.0 += delta.0;
                e.1 += delta.1;
                e.2 += delta.2;
            }
        }
        for e in &session.entries {
            if let crate::model::LogEntry::Assistant(m) = &e.entry {
                let actual = m.message.usage.as_ref().map_or((0, 0, 0), |u| {
                    (
                        u.input_tokens,
                        u.cache_read_input_tokens.unwrap_or(0),
                        u.output_tokens,
                    )
                });
                let want = expected_by_owner.get(&e.id).copied().unwrap_or((0, 0, 0));
                if actual != want {
                    v.push(format!(
                        "entry {} canonical usage {actual:?} does not match expected {want:?}",
                        e.id
                    ));
                }
            }
        }

        // Per-observation verification against the native record.
        for (eid, sem) in &session.semantics {
            for obs in &sem.usage {
                // Full-identity check: the source must be at the PREFERRED
                // artifact (round-25 — a same-ordinal sibling swap must not
                // pass just because the ordinal exists).
                if obs.record.artifact != preferred {
                    v.push(format!(
                        "observation on {eid} record #{} names the wrong artifact",
                        obs.record.ordinal
                    ));
                    continue;
                }
                let Some(src) = raw_by_ordinal.get(&obs.record.ordinal) else {
                    v.push(format!(
                        "observation on {eid} references missing record #{}",
                        obs.record.ordinal
                    ));
                    continue;
                };
                if src["type"] != "event_msg" || src["payload"]["type"] != "token_count" {
                    v.push(format!(
                        "observation on {eid} references non-token_count record #{}",
                        obs.record.ordinal
                    ));
                    continue;
                }
                let coherent = matches!(
                    (obs.scope, obs.aggregation),
                    (UsageScope::Call, UsageAggregation::Delta)
                        | (UsageScope::Session, UsageAggregation::Cumulative)
                );
                if !coherent {
                    v.push(format!(
                        "observation on {eid} record #{} has incoherent scope/aggregation",
                        obs.record.ordinal
                    ));
                    continue;
                }
                let sub = match obs.aggregation {
                    UsageAggregation::Delta => "last_token_usage",
                    UsageAggregation::Cumulative => "total_token_usage",
                };
                let want = triple(src, sub);
                if (obs.input_tokens, obs.cached_input_tokens, obs.output_tokens) != want {
                    v.push(format!(
                        "observation on {eid} record #{} values {:?} != native {want:?}",
                        obs.record.ordinal,
                        (obs.input_tokens, obs.cached_input_tokens, obs.output_tokens)
                    ));
                }
                let contradicts = obs.cached_input_tokens > obs.input_tokens;
                let want_basis = if contradicts {
                    UsageBasis::Unknown
                } else {
                    UsageBasis::InputIncludesCached
                };
                if obs.basis != want_basis {
                    v.push(format!(
                        "observation on {eid} record #{} basis {:?} != {want_basis:?}",
                        obs.record.ordinal, obs.basis
                    ));
                }
                let want_ambiguous = match obs.aggregation {
                    UsageAggregation::Delta => contradicts,
                    UsageAggregation::Cumulative => {
                        contradicts || ambiguous_cumulative.contains(&obs.record.ordinal)
                    }
                };
                if obs.ambiguous != want_ambiguous {
                    v.push(format!(
                        "observation on {eid} record #{} ambiguity {} != {want_ambiguous}",
                        obs.record.ordinal, obs.ambiguous
                    ));
                }
            }
        }

        v
    }

    // ========================================================================
    // Usage-allocation audit: negative controls (round-25). Each starts from
    // a VALID parsed session, alters exactly one property, and asserts the
    // audit rejects it for the specific reason — proving a green corpus is
    // evidence only because the oracle demonstrably fails broken output.
    // ========================================================================

    /// Two windows, three assistant emissions (two in the first window so
    /// the owner is unambiguously the nearest-before), two attributed usage
    /// records. Ordinals: 2 = owner of token 3, 6 = owner of token 7.
    fn allocation_fixture() -> Vec<String> {
        let usage = |i: u64, c: u64, o: u64| serde_json::json!({"input_tokens": i, "cached_input_tokens": c, "output_tokens": o});
        vec![
            envelope_line("session_meta", serde_json::json!({"id": THREAD_A})),
            envelope_line(
                "turn_context",
                serde_json::json!({"turn_id": "t1", "model": "m"}),
            ),
            envelope_line(
                "response_item",
                serde_json::json!({"type": "message", "role": "assistant",
                    "content": [{"type": "output_text", "text": "a"}]}),
            ),
            envelope_line(
                "event_msg",
                serde_json::json!({"type": "token_count", "info": {
                    "last_token_usage": usage(100, 40, 10),
                    "total_token_usage": usage(100, 40, 10)}}),
            ),
            envelope_line(
                "response_item",
                serde_json::json!({"type": "message", "role": "assistant",
                    "content": [{"type": "output_text", "text": "a2"}]}),
            ),
            envelope_line(
                "turn_context",
                serde_json::json!({"turn_id": "t2", "model": "m"}),
            ),
            envelope_line(
                "response_item",
                serde_json::json!({"type": "message", "role": "assistant",
                    "content": [{"type": "output_text", "text": "b"}]}),
            ),
            envelope_line(
                "event_msg",
                serde_json::json!({"type": "token_count", "info": {
                    "last_token_usage": usage(120, 40, 10),
                    "total_token_usage": usage(200, 80, 20)}}),
            ),
        ]
    }

    fn eid(ordinal: u64) -> crate::provider::EntryId {
        crate::provider::EntryId::deterministic(&key(THREAD_A), ordinal, 0)
    }

    /// The audit passes the VALID fixture (positive control).
    #[test]
    fn allocation_audit_accepts_the_valid_fixture() {
        let (_t, parsed, raw) = parse_and_raw(&allocation_fixture());
        let violations = audit_usage_allocation(&key(THREAD_A), &raw, &parsed, None);
        assert!(
            violations.is_empty(),
            "valid fixture rejected: {violations:?}"
        );
    }

    /// The check must fire on a corruption; used by every negative control.
    fn assert_rejects(
        parsed: &crate::provider::ParsedSession,
        raw: &[(u64, serde_json::Value)],
        needle: &str,
    ) {
        let violations = audit_usage_allocation(&key(THREAD_A), raw, parsed, None);
        assert!(
            violations.iter().any(|m| m.contains(needle)),
            "expected a violation containing {needle:?}, got {violations:?}"
        );
    }

    fn make_preserved(
        parsed: &mut crate::provider::ParsedSession,
        raw: &[(u64, serde_json::Value)],
        ord: u64,
    ) {
        use crate::provider::{RecordOutcome, RecordRef};
        let tid = eid(ord);
        let artifact = parsed
            .descriptor
            .preferred_artifact()
            .unwrap()
            .snapshot
            .id
            .clone();
        for sem in parsed.semantics.values_mut() {
            sem.usage.retain(|o| o.record.ordinal != ord);
        }
        for origins in parsed.entry_origins.values_mut() {
            origins.retain(|r| r.ordinal != ord);
        }
        for d in parsed.record_dispositions.iter_mut() {
            if d.record.ordinal == ord {
                d.outcome = RecordOutcome::Unknown {
                    entries: vec![tid.clone()],
                };
            }
        }
        let raw_val = raw.iter().find(|(o, _)| *o == ord).unwrap().1.clone();
        parsed.entries.push(crate::provider::IdentifiedEntry {
            id: tid.clone(),
            entry: crate::model::LogEntry::Unknown(raw_val),
        });
        parsed.entry_origins.insert(
            tid,
            vec![RecordRef {
                artifact,
                ordinal: ord,
            }],
        );
    }

    #[test]
    fn nc_all_usage_preserved_with_no_observations_is_rejected() {
        // The headline broken impl: preserve every token_count, emit no
        // observations, attach zero canonical usage. Must fail.
        let (_t, mut parsed, raw) = parse_and_raw(&allocation_fixture());
        make_preserved(&mut parsed, &raw, 3);
        make_preserved(&mut parsed, &raw, 7);
        // Zero canonical usage, matching the broken impl.
        for e in parsed.entries.iter_mut() {
            if let crate::model::LogEntry::Assistant(m) = &mut e.entry {
                m.message.usage = None;
            }
        }
        assert_rejects(&parsed, &raw, "was preserved");
    }

    #[test]
    fn nc_missing_call_observation_is_rejected() {
        let (_t, mut parsed, raw) = parse_and_raw(&allocation_fixture());
        parsed
            .semantics
            .get_mut(&eid(2))
            .unwrap()
            .usage
            .retain(|o| !matches!(o.scope, crate::provider::UsageScope::Call));
        assert_rejects(&parsed, &raw, "Call/Delta observations (expected 1)");
    }

    #[test]
    fn nc_missing_session_observation_is_rejected() {
        let (_t, mut parsed, raw) = parse_and_raw(&allocation_fixture());
        parsed
            .semantics
            .get_mut(&eid(2))
            .unwrap()
            .usage
            .retain(|o| !matches!(o.scope, crate::provider::UsageScope::Session));
        assert_rejects(
            &parsed,
            &raw,
            "Session/Cumulative observations (expected 1)",
        );
    }

    #[test]
    fn nc_duplicate_observations_are_rejected() {
        let (_t, mut parsed, raw) = parse_and_raw(&allocation_fixture());
        let usage = &mut parsed.semantics.get_mut(&eid(2)).unwrap().usage;
        let dup = usage[0].clone();
        usage.push(dup);
        assert_rejects(&parsed, &raw, "observations (expected exactly 2)");
    }

    #[test]
    fn nc_swapped_scope_aggregation_is_rejected() {
        let (_t, mut parsed, raw) = parse_and_raw(&allocation_fixture());
        for o in parsed.semantics.get_mut(&eid(2)).unwrap().usage.iter_mut() {
            if matches!(o.scope, crate::provider::UsageScope::Call) {
                // Call now claims Cumulative aggregation: incoherent.
                o.aggregation = crate::provider::UsageAggregation::Cumulative;
            }
        }
        assert_rejects(&parsed, &raw, "incoherent scope/aggregation");
    }

    #[test]
    fn nc_observation_referencing_non_token_record_is_rejected() {
        let (_t, mut parsed, raw) = parse_and_raw(&allocation_fixture());
        // Point a Call observation at the assistant message record (ord 2).
        for o in parsed.semantics.get_mut(&eid(2)).unwrap().usage.iter_mut() {
            if matches!(o.scope, crate::provider::UsageScope::Call) {
                o.record.ordinal = 2;
            }
        }
        assert_rejects(&parsed, &raw, "references non-token_count record");
    }

    #[test]
    fn nc_attribution_to_wrong_assistant_same_window_is_rejected() {
        let (_t, mut parsed, raw) = parse_and_raw(&allocation_fixture());
        // Move token 3's observations from owner 2 to owner 4 (same window).
        let moved: Vec<_> = parsed
            .semantics
            .get_mut(&eid(2))
            .unwrap()
            .usage
            .drain(..)
            .collect();
        parsed
            .semantics
            .entry(eid(4))
            .or_default()
            .usage
            .extend(moved);
        assert_rejects(&parsed, &raw, "expected owner");
    }

    #[test]
    fn nc_attribution_across_window_boundary_is_rejected() {
        let (_t, mut parsed, raw) = parse_and_raw(&allocation_fixture());
        // Move token 3's observations to owner 6 — a DIFFERENT window.
        let moved: Vec<_> = parsed
            .semantics
            .get_mut(&eid(2))
            .unwrap()
            .usage
            .drain(..)
            .collect();
        parsed
            .semantics
            .entry(eid(6))
            .or_default()
            .usage
            .extend(moved);
        assert_rejects(&parsed, &raw, "expected owner");
    }

    #[test]
    fn nc_record_in_both_partitions_is_rejected() {
        use crate::provider::RecordRef;
        let (_t, mut parsed, raw) = parse_and_raw(&allocation_fixture());
        // token 3 is attributed; ALSO add a preserved Unknown entry for it.
        let artifact = parsed
            .descriptor
            .preferred_artifact()
            .unwrap()
            .snapshot
            .id
            .clone();
        let raw_val = raw.iter().find(|(o, _)| *o == 3).unwrap().1.clone();
        parsed.entries.push(crate::provider::IdentifiedEntry {
            id: eid(3),
            entry: crate::model::LogEntry::Unknown(raw_val),
        });
        parsed.entry_origins.insert(
            eid(3),
            vec![RecordRef {
                artifact,
                ordinal: 3,
            }],
        );
        assert_rejects(&parsed, &raw, "attributed=true, preserved=true");
    }

    #[test]
    fn nc_record_in_neither_partition_is_rejected() {
        let (_t, mut parsed, raw) = parse_and_raw(&allocation_fixture());
        // Remove token 3's observations without preserving it: neither form.
        for sem in parsed.semantics.values_mut() {
            sem.usage.retain(|o| o.record.ordinal != 3);
        }
        assert_rejects(&parsed, &raw, "attributed=false, preserved=false");
    }

    #[test]
    fn nc_observation_wrong_sibling_artifact_is_rejected() {
        // A session with a plain rollout AND an archived copy has two
        // SIBLING artifacts. Swapping an observation to the sibling keeps
        // the ordinal (and passes descriptor membership), so only full-
        // RecordRef identity can catch it — in the audit AND in the generic
        // validator's origin-correspondence check.
        let tmp = tempfile::tempdir().unwrap();
        let content = allocation_fixture().join("\n") + "\n";
        let file = format!("rollout-2026-07-16T23-38-33-{THREAD_A}.jsonl");
        let day = tmp.path().join("sessions/2026/07/16");
        std::fs::create_dir_all(&day).unwrap();
        std::fs::write(day.join(&file), &content).unwrap();
        let arch = tmp.path().join("archived_sessions/2026/07/16");
        std::fs::create_dir_all(&arch).unwrap();
        std::fs::write(arch.join(&file), &content).unwrap();

        let p = CodexProvider::new(tmp.path());
        let mut parsed = p.parse(&key(THREAD_A)).unwrap();
        let raw: Vec<(u64, serde_json::Value)> = allocation_fixture()
            .iter()
            .enumerate()
            .map(|(i, l)| (i as u64, serde_json::from_str(l).unwrap()))
            .collect();
        assert_eq!(
            parsed.descriptor.artifacts.len(),
            2,
            "fixture must have a sibling artifact"
        );
        let preferred = parsed
            .descriptor
            .preferred_artifact()
            .unwrap()
            .snapshot
            .id
            .clone();
        let sibling = parsed
            .descriptor
            .artifacts
            .iter()
            .map(|a| a.snapshot.id.clone())
            .find(|id| *id != preferred)
            .expect("a distinct sibling artifact");
        assert!(parsed.validate_provenance().is_empty());

        for o in parsed.semantics.get_mut(&eid(2)).unwrap().usage.iter_mut() {
            if matches!(o.scope, crate::provider::UsageScope::Call) {
                o.record.artifact = sibling.clone();
            }
        }
        // The audit rejects on full identity...
        assert_rejects(&parsed, &raw, "names the wrong artifact");
        // ...and so does the generic validator (sibling record is not an
        // origin of the annotated entry).
        assert!(
            parsed
                .validate_provenance()
                .iter()
                .any(|m| m.contains("usage observation on entry")),
            "validate_provenance must reject the sibling swap: {:?}",
            parsed.validate_provenance()
        );
    }

    #[test]
    fn nc_canonical_usage_moved_between_assistants_is_rejected() {
        // Move canonical usage from owner 2 to owner 6, leaving
        // observations, dispositions, origins, and the GLOBAL sum unchanged.
        // A global reconciliation would pass; per-owner must reject.
        let (_t, mut parsed, raw) = parse_and_raw(&allocation_fixture());
        let take_usage = |parsed: &mut crate::provider::ParsedSession,
                          id: &crate::provider::EntryId| {
            parsed
                .entries
                .iter_mut()
                .find(|e| e.id == *id)
                .and_then(|e| match &mut e.entry {
                    crate::model::LogEntry::Assistant(m) => m.message.usage.take(),
                    _ => None,
                })
        };
        let moved = take_usage(&mut parsed, &eid(2)).expect("owner 2 has usage");
        if let Some(e) = parsed.entries.iter_mut().find(|e| e.id == eid(6)) {
            if let crate::model::LogEntry::Assistant(m) = &mut e.entry {
                let u = m.message.usage.get_or_insert_with(Default::default);
                u.input_tokens += moved.input_tokens;
                u.output_tokens += moved.output_tokens;
                let add = moved.cache_read_input_tokens.unwrap_or(0);
                *u.cache_read_input_tokens.get_or_insert(0) += add;
            }
        }
        assert_rejects(&parsed, &raw, "canonical usage");
    }

    #[test]
    fn dual_stream_dedup_marks_human_prompts_and_suppresses_event_twins() {
        let (_tmp, p) = normalize_home(&[
            envelope_line(
                "session_meta",
                serde_json::json!({"id": THREAD_A, "cwd": "/w", "cli_version": "0.9"}),
            ),
            // Harness-injected user context: NO user_message event pairs it.
            envelope_line(
                "response_item",
                serde_json::json!({"type": "message", "role": "user", "content": [{"type": "input_text", "text": "<environment_context>"}]}),
            ),
            // Genuine human prompt: response_item + its event twin.
            envelope_line(
                "response_item",
                serde_json::json!({"type": "message", "role": "user", "content": [{"type": "input_text", "text": "fix the bug"}]}),
            ),
            envelope_line(
                "event_msg",
                serde_json::json!({"type": "user_message", "message": "fix the bug", "kind": "plain"}),
            ),
            envelope_line(
                "event_msg",
                serde_json::json!({"type": "agent_message", "message": "on it"}),
            ),
            envelope_line(
                "response_item",
                serde_json::json!({"type": "message", "role": "assistant", "content": [{"type": "output_text", "text": "on it"}]}),
            ),
        ]);
        let parsed = p.parse(&key(THREAD_A)).unwrap();
        assert!(parsed.validate_provenance().is_empty());

        // Both event twins suppressed as duplicate stream.
        let dup = parsed
            .record_dispositions
            .iter()
            .filter(|d| {
                matches!(
                    &d.outcome,
                    RecordOutcome::Suppressed {
                        reason: super::super::SuppressionReason::DuplicateStream { .. }
                    }
                )
            })
            .count();
        assert_eq!(dup, 2);

        // Ordinal 1 (harness context) stays Harness; ordinal 2 (claimed by
        // the event) is Human. (Records are zero-ordinal-based.)
        let sem = |ordinal| {
            parsed
                .semantics
                .get(&crate::provider::EntryId::deterministic(
                    &key(THREAD_A),
                    ordinal,
                    0,
                ))
                .and_then(|s| s.prompt)
        };
        assert_eq!(
            sem(1).map(|p| p.authorship),
            Some(crate::provider::PromptAuthorship::Harness)
        );
        assert_eq!(
            sem(2).map(|p| p.authorship),
            Some(crate::provider::PromptAuthorship::Human)
        );
    }

    #[test]
    fn usage_deltas_accumulate_without_double_counting() {
        // Two token_counts: cumulative 100 then 250 (deltas 100 and 150).
        // Summing normalized entry usage must equal the FINAL cumulative
        // total — the reviewer-required no-double-count proof.
        let usage = |input: u64, cached: u64, output: u64| {
            serde_json::json!({"input_tokens": input, "cached_input_tokens": cached,
                               "output_tokens": output, "total_tokens": input + output})
        };
        let (_tmp, p) = normalize_home(&[
            envelope_line("session_meta", serde_json::json!({"id": THREAD_A})),
            envelope_line(
                "response_item",
                serde_json::json!({"type": "message", "role": "assistant", "content": [{"type": "output_text", "text": "a"}]}),
            ),
            envelope_line(
                "event_msg",
                serde_json::json!({"type": "token_count", "info": {
                "last_token_usage": usage(80, 30, 20), "total_token_usage": usage(80, 30, 20)}}),
            ),
            envelope_line(
                "response_item",
                serde_json::json!({"type": "message", "role": "assistant", "content": [{"type": "output_text", "text": "b"}]}),
            ),
            envelope_line(
                "event_msg",
                serde_json::json!({"type": "token_count", "info": {
                "last_token_usage": usage(120, 100, 30), "total_token_usage": usage(200, 130, 50)}}),
            ),
        ]);
        let parsed = p.parse(&key(THREAD_A)).unwrap();
        assert!(parsed.validate_provenance().is_empty());

        let mut fresh = 0u64;
        let mut cached = 0u64;
        let mut output = 0u64;
        for e in &parsed.entries {
            if let crate::model::LogEntry::Assistant(m) = &e.entry {
                if let Some(u) = &m.message.usage {
                    fresh += u.input_tokens;
                    cached += u.cache_read_input_tokens.unwrap_or(0);
                    output += u.output_tokens;
                }
            }
        }
        // Final cumulative: input 200 (130 cached => 70 fresh), output 50.
        assert_eq!((fresh, cached, output), (70, 130, 50));

        // Each token_count attached BOTH observations with their axes.
        let first_assistant = crate::provider::EntryId::deterministic(&key(THREAD_A), 1, 0);
        let sem = parsed.semantics.get(&first_assistant).unwrap();
        assert_eq!(sem.usage.len(), 2);
        assert!(matches!(
            (sem.usage[0].scope, sem.usage[0].aggregation),
            (
                crate::provider::UsageScope::Call,
                crate::provider::UsageAggregation::Delta
            )
        ));
        assert!(matches!(
            (sem.usage[1].scope, sem.usage[1].aggregation),
            (
                crate::provider::UsageScope::Session,
                crate::provider::UsageAggregation::Cumulative
            )
        ));
        // N:1 provenance: the token_count record joined the entry's origins.
        assert_eq!(parsed.entry_origins[&first_assistant].len(), 2);
    }

    #[test]
    fn turn_id_rides_the_semantics_sidecar() {
        let (_tmp, p) = normalize_home(&[
            envelope_line("session_meta", serde_json::json!({"id": THREAD_A})),
            envelope_line(
                "event_msg",
                serde_json::json!({"type": "task_started", "turn_id": "turn-1"}),
            ),
            envelope_line(
                "response_item",
                serde_json::json!({"type": "message", "role": "user", "content": [{"type": "input_text", "text": "hi"}]}),
            ),
            envelope_line(
                "turn_context",
                serde_json::json!({"turn_id": "turn-2", "model": "gpt-x"}),
            ),
            envelope_line(
                "response_item",
                serde_json::json!({"type": "message", "role": "assistant", "content": [{"type": "output_text", "text": "hello"}]}),
            ),
        ]);
        let parsed = p.parse(&key(THREAD_A)).unwrap();
        let turn = |ordinal| {
            parsed
                .semantics
                .get(&crate::provider::EntryId::deterministic(
                    &key(THREAD_A),
                    ordinal,
                    0,
                ))
                .and_then(|s| s.turn_id.clone())
        };
        assert_eq!(turn(2).as_deref(), Some("turn-1"));
        assert_eq!(turn(4).as_deref(), Some("turn-2"));
        // Model from turn_context reached the assistant entry.
        let m = parsed
            .entries
            .iter()
            .find_map(|e| match &e.entry {
                crate::model::LogEntry::Assistant(m) => Some(m),
                _ => None,
            })
            .expect("assistant entry expected");
        assert_eq!(m.message.model, "gpt-x");
    }

    #[test]
    fn single_stream_sessions_map_event_content_directly() {
        // No response_item records at all (hypothetical era — zero in the
        // corpus): event content maps instead of being suppressed.
        let (_tmp, p) = normalize_home(&[
            envelope_line("session_meta", serde_json::json!({"id": THREAD_A})),
            envelope_line(
                "event_msg",
                serde_json::json!({"type": "user_message", "message": "hello"}),
            ),
            envelope_line(
                "event_msg",
                serde_json::json!({"type": "agent_reasoning", "text": "thinking"}),
            ),
            envelope_line(
                "event_msg",
                serde_json::json!({"type": "agent_message", "message": "hi"}),
            ),
        ]);
        let parsed = p.parse(&key(THREAD_A)).unwrap();
        assert!(parsed.validate_provenance().is_empty());
        assert_eq!(parsed.diagnostics.mapped, 3);
        assert_eq!(parsed.diagnostics.suppressed, 0);
        let kinds: Vec<&str> = parsed
            .entries
            .iter()
            .map(|e| match &e.entry {
                crate::model::LogEntry::User(_) => "user",
                crate::model::LogEntry::Assistant(_) => "assistant",
                _ => "other",
            })
            .collect();
        assert_eq!(kinds, ["other", "user", "assistant", "assistant"]);
    }

    #[test]
    fn normalized_entries_thread_into_a_conversation_main_line() {
        let plain_bytes = std::fs::read(fixture_path("envelope_session.jsonl")).unwrap();
        let (_tmp, p) = home_with(THREAD_A, &plain_bytes, false);
        let parsed = std::sync::Arc::new(p.parse(&key(THREAD_A)).unwrap());
        let conversation =
            crate::reconstruction::Conversation::from_parsed_session(parsed.clone()).unwrap();
        // The 4 mapped emissions form one linear thread; Unknown entries
        // (meta, turn_context) are uuid-less orphans by design.
        assert_eq!(conversation.len(), 4);
        assert_eq!(conversation.main_thread().len(), 4);
        // Semantics reachable through the conversation for a mapped uuid
        // (the synthetic uuid IS the injective EntryId encoding).
        let uuid = crate::provider::EntryId::deterministic(&key(THREAD_A), 3, 0).to_string();
        assert!(
            conversation.semantics_for_uuid(&uuid).is_some(),
            "tool-call entry semantics reachable via conversation"
        );
    }

    // ========================================================================
    // B3.1 hardening (round-22): real corpus failure shapes
    // ========================================================================

    #[test]
    fn unmatched_agent_message_after_compaction_is_mapped_not_discarded() {
        // Corpus shape: "Compact task completed" arrives as an agent_message
        // with NO response_item twin after a compaction boundary.
        let (_tmp, p) = normalize_home(&[
            envelope_line("session_meta", serde_json::json!({"id": THREAD_A})),
            envelope_line("compacted", serde_json::json!({"message": "..."})),
            envelope_line(
                "event_msg",
                serde_json::json!({"type": "agent_message", "message": "Compact task completed"}),
            ),
        ]);
        let parsed = p.parse(&key(THREAD_A)).unwrap();
        assert!(parsed.validate_provenance().is_empty());
        assert_eq!(parsed.diagnostics.suppressed, 0, "nothing may be discarded");
        let mapped_text = parsed.entries.iter().any(|e| {
            matches!(&e.entry, crate::model::LogEntry::Assistant(m)
                if m.message.content.iter().any(|b| matches!(b, crate::model::ContentBlock::Text(t) if t.text == "Compact task completed")))
        });
        assert!(mapped_text, "unique event content must map");
    }

    #[test]
    fn reasoning_before_aborted_turn_with_no_twin_is_mapped() {
        // Corpus shape: agent_reasoning emitted, then turn_aborted — no
        // response_item reasoning ever lands.
        let (_tmp, p) = normalize_home(&[
            envelope_line("session_meta", serde_json::json!({"id": THREAD_A})),
            envelope_line(
                "event_msg",
                serde_json::json!({"type": "agent_reasoning", "text": "half-finished thought"}),
            ),
            envelope_line(
                "event_msg",
                serde_json::json!({"type": "turn_aborted", "reason": "user"}),
            ),
        ]);
        let parsed = p.parse(&key(THREAD_A)).unwrap();
        assert_eq!(parsed.diagnostics.suppressed, 0);
        let mapped = parsed.entries.iter().any(|e| {
            matches!(&e.entry, crate::model::LogEntry::Assistant(m)
                if m.message.content.iter().any(|b| matches!(b, crate::model::ContentBlock::Thinking(t) if t.thinking == "half-finished thought")))
        });
        assert!(mapped, "pre-abort reasoning must survive as thinking");
    }

    #[test]
    fn duplicate_user_message_events_cannot_claim_a_harness_entry() {
        // Corpus shape: repeated user_message events; only one response twin
        // exists. The second event must NOT claim the harness context entry
        // (the old LIFO claimant did) — it maps as its own human entry.
        let (_tmp, p) = normalize_home(&[
            envelope_line("session_meta", serde_json::json!({"id": THREAD_A})),
            envelope_line(
                "response_item",
                serde_json::json!({"type": "message", "role": "user", "content": [{"type": "input_text", "text": "<environment_context>"}]}),
            ),
            envelope_line(
                "response_item",
                serde_json::json!({"type": "message", "role": "user", "content": [{"type": "input_text", "text": "same prompt"}]}),
            ),
            envelope_line(
                "event_msg",
                serde_json::json!({"type": "user_message", "message": "same prompt"}),
            ),
            envelope_line(
                "event_msg",
                serde_json::json!({"type": "user_message", "message": "same prompt"}),
            ),
        ]);
        let parsed = p.parse(&key(THREAD_A)).unwrap();
        let auth = |ordinal| {
            parsed
                .semantics
                .get(&crate::provider::EntryId::deterministic(
                    &key(THREAD_A),
                    ordinal,
                    0,
                ))
                .and_then(|s| s.prompt)
                .map(|p| p.authorship)
        };
        assert_eq!(
            auth(1),
            Some(crate::provider::PromptAuthorship::Harness),
            "harness context must never be claimed human"
        );
        assert_eq!(auth(2), Some(crate::provider::PromptAuthorship::Human));
        // Round-23: the repeated identical event is ONE semantic emission —
        // both events suppress against the same authoritative response
        // (ordinal 2); no duplicate human entry exists at all.
        let twins: Vec<u64> = parsed
            .record_dispositions
            .iter()
            .filter_map(|d| match &d.outcome {
                RecordOutcome::Suppressed {
                    reason: super::super::SuppressionReason::DuplicateStream { twin },
                } => Some(twin.ordinal),
                _ => None,
            })
            .collect();
        assert_eq!(twins, vec![2, 2]);
        let human_entries = parsed
            .semantics
            .values()
            .filter(|s| {
                s.prompt.is_some_and(|p| {
                    matches!(p.authorship, crate::provider::PromptAuthorship::Human)
                })
            })
            .count();
        assert_eq!(human_entries, 1, "exactly one human emission");
    }

    fn prompt_audit_fixture() -> (
        tempfile::TempDir,
        crate::provider::ParsedSession,
        Vec<(u64, serde_json::Value)>,
    ) {
        // Mined corpus shapes (226-session census): 1,705 response/event
        // human prompt pairs, two exact duplicate events, and two unique
        // unmatched user events. One unmatched event precedes the first
        // assistant response in its turn; the other occurs between assistant
        // emissions. This fixture covers the former placement and the current
        // five-key user_message payload.
        parse_and_raw(&[
            envelope_line("session_meta", serde_json::json!({"id": THREAD_A})),
            envelope_line(
                "turn_context",
                serde_json::json!({"turn_id": "turn-1", "model": "gpt-test"}),
            ),
            envelope_line(
                "response_item",
                serde_json::json!({"type": "message", "role": "user",
                    "content": [{"type": "input_text", "text": "<environment_context>"}]}),
            ),
            envelope_line(
                "response_item",
                serde_json::json!({"type": "message", "role": "user",
                    "content": [{"type": "input_text", "text": "start the task"}]}),
            ),
            envelope_line(
                "event_msg",
                serde_json::json!({"type": "user_message", "message": "start the task",
                    "images": [], "local_images": [], "text_elements": []}),
            ),
            envelope_line(
                "event_msg",
                serde_json::json!({"type": "user_message", "message": "check tests first",
                    "images": [], "local_images": [], "text_elements": []}),
            ),
            envelope_line(
                "response_item",
                serde_json::json!({"type": "message", "role": "assistant",
                    "content": [{"type": "output_text", "text": "tests checked"}]}),
            ),
        ])
    }

    #[test]
    fn prompt_semantics_audit_accepts_native_boundary_and_steering() {
        let (_tmp, parsed, raw) = prompt_audit_fixture();
        let audit = audit_prompt_semantics(&key(THREAD_A), &raw, &parsed, None);
        assert!(audit.violations.is_empty(), "{:?}", audit.violations);
        assert_eq!(audit.boundary_count, 1);
        assert_eq!(audit.midturn_count, 1);
    }

    #[test]
    fn nc_midturn_prompt_reclassified_as_boundary_is_rejected() {
        let (_tmp, mut parsed, raw) = prompt_audit_fixture();
        let id = crate::provider::EntryId::deterministic(&key(THREAD_A), 5, 0);
        parsed.semantics.get_mut(&id).unwrap().prompt = Some(crate::provider::PromptSemantics {
            authorship: crate::provider::PromptAuthorship::Human,
            delivery: crate::provider::PromptDelivery::TurnBoundary,
        });
        let audit = audit_prompt_semantics(&key(THREAD_A), &raw, &parsed, None);
        assert!(
            audit
                .violations
                .iter()
                .any(|v| v.contains("wrong delivery")),
            "{:?}",
            audit.violations
        );
    }

    #[test]
    fn nc_harness_prompt_reclassified_as_human_is_rejected() {
        let (_tmp, mut parsed, raw) = prompt_audit_fixture();
        let id = crate::provider::EntryId::deterministic(&key(THREAD_A), 2, 0);
        parsed.semantics.get_mut(&id).unwrap().prompt = Some(crate::provider::PromptSemantics {
            authorship: crate::provider::PromptAuthorship::Human,
            delivery: crate::provider::PromptDelivery::TurnBoundary,
        });
        let audit = audit_prompt_semantics(&key(THREAD_A), &raw, &parsed, None);
        assert!(
            audit
                .violations
                .iter()
                .any(|v| v.contains("unexpected human prompt semantics")),
            "{:?}",
            audit.violations
        );
    }

    #[test]
    fn canonical_usage_from_cumulative_transitions() {
        // Corpus shapes: usage BEFORE its response; repeated unchanged
        // cumulative totals; a normal increment; a cumulative RESET.
        let tc = |input: u64, cached: u64, output: u64| {
            envelope_line(
                "event_msg",
                serde_json::json!({"type": "token_count", "info": {
                "last_token_usage": {"input_tokens": input, "cached_input_tokens": cached, "output_tokens": output},
                "total_token_usage": {"input_tokens": input, "cached_input_tokens": cached, "output_tokens": output}}}),
            )
        };
        let assistant = |text: &str| {
            envelope_line(
                "response_item",
                serde_json::json!({"type": "message", "role": "assistant", "content": [{"type": "output_text", "text": text}]}),
            )
        };
        let (_tmp, p) = normalize_home(&[
            envelope_line("session_meta", serde_json::json!({"id": THREAD_A})),
            // Usage arrives BEFORE any assistant record: held, not lost.
            tc(100, 40, 10),
            assistant("a"),
            // Unchanged cumulative repeated (old-format shape): zero delta.
            tc(100, 40, 10),
            // Normal increment.
            tc(250, 150, 30),
            // RESET (new epoch): totals drop.
            tc(50, 20, 5),
            assistant("b"),
        ]);
        let parsed = p.parse(&key(THREAD_A)).unwrap();
        assert!(parsed.validate_provenance().is_empty());

        let mut fresh = 0u64;
        let mut cached = 0u64;
        let mut output = 0u64;
        for e in &parsed.entries {
            if let crate::model::LogEntry::Assistant(m) = &e.entry {
                if let Some(u) = &m.message.usage {
                    fresh += u.input_tokens;
                    cached += u.cache_read_input_tokens.unwrap_or(0);
                    output += u.output_tokens;
                }
            }
        }
        // Epoch 1 final: 250/150/30 (fresh 100); epoch 2 final: 50/20/5
        // (fresh 30). Canonical sums telescope to epoch finals — NOT the
        // blind last-usage sum (which would be 100+100+250+50=500 input).
        assert_eq!(
            (fresh, cached, output),
            (100 + 30, 150 + 20, 30 + 5),
            "entry usage must equal the sum of epoch finals"
        );
        // The pre-response event was HELD and attached to the first
        // assistant; the three later events in the same window attach
        // backward to it as well (1 message + 4 usage records = 5 origins).
        let first = crate::provider::EntryId::deterministic(&key(THREAD_A), 2, 0);
        assert_eq!(
            parsed.entry_origins[&first].len(),
            5,
            "held + backward usage events all attach with N:1 provenance"
        );
    }

    #[test]
    fn pending_usage_with_no_assistant_is_preserved_not_lost() {
        let (_tmp, p) = normalize_home(&[
            envelope_line("session_meta", serde_json::json!({"id": THREAD_A})),
            envelope_line(
                "event_msg",
                serde_json::json!({"type": "token_count", "info": {
                "total_token_usage": {"input_tokens": 10, "cached_input_tokens": 0, "output_tokens": 1}}}),
            ),
        ]);
        let parsed = p.parse(&key(THREAD_A)).unwrap();
        assert!(parsed.validate_provenance().is_empty());
        let preserved = parsed.entries.iter().any(|e| {
            matches!(&e.entry, crate::model::LogEntry::Unknown(v)
                if v["payload"]["type"] == "token_count")
        });
        assert!(preserved, "orphan usage must remain a preserved entry");
        assert_eq!(parsed.diagnostics.suppressed, 0);
    }

    #[test]
    fn per_item_metadata_is_honored_over_stale_ambient_turn() {
        let (_tmp, p) = normalize_home(&[
            envelope_line("session_meta", serde_json::json!({"id": THREAD_A})),
            // Ambient says turn-old (stale); the item carries turn-new.
            envelope_line(
                "turn_context",
                serde_json::json!({"turn_id": "turn-old", "model": "m"}),
            ),
            envelope_line(
                "response_item",
                serde_json::json!({"type": "message", "role": "assistant",
                "content": [{"type": "output_text", "text": "x"}],
                "internal_chat_message_metadata_passthrough": {"turn_id": "turn-new"}}),
            ),
            // Metadata-only carrier: no ambient source at all for turn-only.
            envelope_line(
                "response_item",
                serde_json::json!({"type": "function_call", "name": "shell",
                "arguments": "{}", "call_id": "c1", "metadata": {"turn_id": "turn-new"}}),
            ),
        ]);
        let parsed = p.parse(&key(THREAD_A)).unwrap();
        let turn = |ordinal| {
            parsed
                .semantics
                .get(&crate::provider::EntryId::deterministic(
                    &key(THREAD_A),
                    ordinal,
                    0,
                ))
                .and_then(|s| s.turn_id.clone())
        };
        assert_eq!(turn(2).as_deref(), Some("turn-new"));
        assert_eq!(turn(3).as_deref(), Some("turn-new"));
    }

    #[test]
    fn same_text_at_different_timestamps_stays_distinct() {
        // Fingerprint includes the timestamp: repeated text later in the
        // window is a REAL second emission, not a duplicate.
        let mk = |ts: &str| {
            format!(
                "{{\"timestamp\":\"{ts}\",\"type\":\"event_msg\",\"payload\":{{\"type\":\"agent_message\",\"message\":\"again\"}}}}"
            )
        };
        let (_tmp, p) = normalize_home(&[
            envelope_line("session_meta", serde_json::json!({"id": THREAD_A})),
            mk("2026-07-16T10:00:01.000Z"),
            mk("2026-07-16T10:00:05.000Z"),
        ]);
        let parsed = p.parse(&key(THREAD_A)).unwrap();
        assert_eq!(parsed.diagnostics.suppressed, 0);
        let texts = parsed
            .entries
            .iter()
            .filter(|e| matches!(&e.entry, crate::model::LogEntry::Assistant(_)))
            .count();
        assert_eq!(texts, 2, "distinct timestamps are distinct emissions");
    }

    #[test]
    fn pending_usage_never_crosses_a_window_boundary() {
        // Corpus shape: token_count → abort/boundary → later assistant. The
        // usage must be preserved unattributed, never attached to the later
        // turn.
        let (_tmp, p) = normalize_home(&[
            envelope_line("session_meta", serde_json::json!({"id": THREAD_A})),
            envelope_line(
                "event_msg",
                serde_json::json!({"type": "token_count", "info": {
                "total_token_usage": {"input_tokens": 100, "cached_input_tokens": 0, "output_tokens": 10}}}),
            ),
            envelope_line(
                "turn_context",
                serde_json::json!({"turn_id": "t2", "model": "m"}),
            ),
            envelope_line(
                "response_item",
                serde_json::json!({"type": "message", "role": "assistant",
                "content": [{"type": "output_text", "text": "later turn"}]}),
            ),
        ]);
        let parsed = p.parse(&key(THREAD_A)).unwrap();
        assert!(parsed.validate_provenance().is_empty());
        // The assistant in the later window received NO usage.
        for e in &parsed.entries {
            if let crate::model::LogEntry::Assistant(m) = &e.entry {
                assert!(
                    m.message.usage.is_none(),
                    "usage leaked across the boundary"
                );
            }
        }
        // The token record is preserved, not lost.
        assert!(parsed.entries.iter().any(|e| {
            matches!(&e.entry, crate::model::LogEntry::Unknown(v)
                if v["payload"]["type"] == "token_count")
        }));
    }

    #[test]
    fn contradictory_call_observation_is_unknown_without_reinterpreting_the_session() {
        // REAL corpus shape (round-24 census: four last_token_usage
        // observations in one January session where cached > input; ZERO
        // cumulative observations do). The contradictory Call observation
        // becomes Unknown/ambiguous with raw values preserved; the Session
        // observation and the canonical accounting keep the source-backed
        // includes-cached basis.
        let (_tmp, p) = normalize_home(&[
            envelope_line("session_meta", serde_json::json!({"id": THREAD_A})),
            envelope_line(
                "response_item",
                serde_json::json!({"type": "message", "role": "assistant",
                "content": [{"type": "output_text", "text": "a"}]}),
            ),
            envelope_line(
                "event_msg",
                serde_json::json!({"type": "token_count", "info": {
                    "last_token_usage": {"input_tokens": 50, "cached_input_tokens": 400, "output_tokens": 5},
                    "total_token_usage": {"input_tokens": 1000, "cached_input_tokens": 400, "output_tokens": 50}}}),
            ),
        ]);
        let parsed = p.parse(&key(THREAD_A)).unwrap();
        let sem = parsed
            .semantics
            .values()
            .find(|s| !s.usage.is_empty())
            .expect("usage semantics");
        let call = sem
            .usage
            .iter()
            .find(|o| matches!(o.scope, crate::provider::UsageScope::Call))
            .unwrap();
        assert!(matches!(call.basis, crate::provider::UsageBasis::Unknown));
        assert!(call.ambiguous);
        assert_eq!((call.input_tokens, call.cached_input_tokens), (50, 400));
        let session = sem
            .usage
            .iter()
            .find(|o| matches!(o.scope, crate::provider::UsageScope::Session))
            .unwrap();
        assert!(matches!(
            session.basis,
            crate::provider::UsageBasis::InputIncludesCached
        ));
        assert!(!session.ambiguous);
        // Canonical accounting unaffected: fresh = 1000 − 400.
        let fresh: u64 = parsed
            .entries
            .iter()
            .filter_map(|e| match &e.entry {
                crate::model::LogEntry::Assistant(m) => {
                    m.message.usage.as_ref().map(|u| u.input_tokens)
                }
                _ => None,
            })
            .sum();
        assert_eq!(fresh, 600);
    }

    #[test]
    fn ambiguous_fresh_transition_zeroes_only_the_fresh_delta() {
        // Field-specific ambiguity (round-24): a fresh decrease without a
        // reset zeroes ONLY the fresh contribution; the cached and output
        // deltas remain well-defined and still contribute.
        let tc = |input: u64, cached: u64, output: u64| {
            envelope_line(
                "event_msg",
                serde_json::json!({"type": "token_count", "info": {
                "total_token_usage": {"input_tokens": input, "cached_input_tokens": cached, "output_tokens": output}}}),
            )
        };
        let (_tmp, p) = normalize_home(&[
            envelope_line("session_meta", serde_json::json!({"id": THREAD_A})),
            envelope_line(
                "response_item",
                serde_json::json!({"type": "message", "role": "assistant",
                "content": [{"type": "output_text", "text": "a"}]}),
            ),
            tc(1000, 100, 10), // fresh 900
            tc(1050, 700, 20), // fresh 350: dropped, input/output monotonic
        ]);
        let parsed = p.parse(&key(THREAD_A)).unwrap();
        let sem = parsed
            .semantics
            .values()
            .find(|s| !s.usage.is_empty())
            .unwrap();
        let flagged = sem
            .usage
            .iter()
            .filter(|o| {
                matches!(o.aggregation, crate::provider::UsageAggregation::Cumulative)
                    && o.ambiguous
            })
            .count();
        assert_eq!(flagged, 1, "the uninterpretable transition is surfaced");
        let (mut fresh, mut cached, mut output) = (0u64, 0u64, 0u64);
        for e in &parsed.entries {
            if let crate::model::LogEntry::Assistant(m) = &e.entry {
                if let Some(u) = &m.message.usage {
                    fresh += u.input_tokens;
                    cached += u.cache_read_input_tokens.unwrap_or(0);
                    output += u.output_tokens;
                }
            }
        }
        // fresh: 900 + 0 (ambiguous delta zeroed); cached: 100 + 600;
        // output: 10 + 10.
        assert_eq!((fresh, cached, output), (900, 700, 20));
    }

    #[test]
    fn forged_duplicate_twin_is_a_provenance_violation() {
        // The validator must reject twins that are not mapped records or
        // reference foreign artifacts.
        let (_tmp, p) = normalize_home(&[
            envelope_line("session_meta", serde_json::json!({"id": THREAD_A})),
            envelope_line(
                "response_item",
                serde_json::json!({"type": "message", "role": "assistant",
                "content": [{"type": "output_text", "text": "hi"}]}),
            ),
            envelope_line(
                "event_msg",
                serde_json::json!({"type": "agent_message", "message": "hi"}),
            ),
        ]);
        let mut parsed = p.parse(&key(THREAD_A)).unwrap();
        assert!(parsed.validate_provenance().is_empty());
        for d in &mut parsed.record_dispositions {
            if let RecordOutcome::Suppressed {
                reason: super::super::SuppressionReason::DuplicateStream { twin },
            } = &mut d.outcome
            {
                twin.ordinal = 0; // session_meta: an UNKNOWN record, not mapped
            }
        }
        assert!(
            parsed
                .validate_provenance()
                .iter()
                .any(|v| v.contains("not a mapped record")),
            "forged twin must be a violation"
        );
    }

    #[test]
    fn web_search_call_preserves_native_id_status_and_action() {
        // Mined corpus shapes: 158/341 records carry a native `id` (a
        // `ws_...` value alongside status + action); the remainder have no
        // id and fall back to a synthesized one.
        let (_tmp, p) = normalize_home(&[
            envelope_line("session_meta", serde_json::json!({"id": THREAD_A})),
            envelope_line(
                "response_item",
                serde_json::json!({"type": "web_search_call",
                "id": "ws_0824df55bcb45159016a3e88021de881919c1c600526d97948",
                "status": "completed",
                "action": {"type": "search", "query": "site:docs.example.dev q"}}),
            ),
            envelope_line(
                "response_item",
                serde_json::json!({"type": "web_search_call", "status": "completed",
                "action": {"type": "open_page", "url": "https://example.com"}}),
            ),
        ]);
        let parsed = p.parse(&key(THREAD_A)).unwrap();
        let tool_of = |ordinal| {
            let id = crate::provider::EntryId::deterministic(&key(THREAD_A), ordinal, 0);
            let entry = parsed.entries.iter().find(|e| e.id == id).unwrap();
            let crate::model::LogEntry::Assistant(m) = &entry.entry else {
                panic!("assistant expected at ordinal {ordinal}");
            };
            m.message
                .content
                .iter()
                .find_map(|b| match b {
                    crate::model::ContentBlock::ToolUse(t) => Some(t.clone()),
                    _ => None,
                })
                .expect("tool use present")
        };
        // Native id, status, and action all preserved.
        let with_id = tool_of(1);
        assert_eq!(
            with_id.id,
            "ws_0824df55bcb45159016a3e88021de881919c1c600526d97948"
        );
        assert_eq!(with_id.input["status"], "completed");
        assert_eq!(with_id.input["action"]["type"], "search");
        assert_eq!(with_id.input["action"]["query"], "site:docs.example.dev q");
        // Id-less era: synthesized fallback, status/action still preserved.
        let without_id = tool_of(2);
        assert_eq!(without_id.id, "ws_2");
        assert_eq!(without_id.input["status"], "completed");
        assert_eq!(without_id.input["action"]["url"], "https://example.com");
        for ordinal in [1u64, 2] {
            let id = crate::provider::EntryId::deterministic(&key(THREAD_A), ordinal, 0);
            let sem = parsed.semantics.get(&id).unwrap();
            assert!(sem
                .tools
                .values()
                .any(|t| matches!(t.kind, crate::provider::ToolKind::Web)));
        }
    }

    #[test]
    fn archive_frames_all_artifacts() {
        let (_tmp, p) = fixture();
        let mut bundle = Vec::new();
        p.write_archive(&key(THREAD_A), &mut bundle).unwrap();
        let newline = bundle.iter().position(|b| *b == b'\n').unwrap();
        let manifest: serde_json::Value = serde_json::from_slice(&bundle[..newline]).unwrap();
        assert_eq!(manifest["manifest"]["provider"], "codex");
        let artifacts = manifest["manifest"]["artifacts"].as_array().unwrap();
        assert_eq!(artifacts.len(), 2);
        let mut offset = newline + 1;
        for a in artifacts {
            let len = a["bytes"].as_u64().unwrap() as usize;
            let on_disk = std::fs::read(a["locator"].as_str().unwrap()).unwrap();
            assert_eq!(&bundle[offset..offset + len], &on_disk[..]);
            offset += len;
        }
        assert_eq!(offset, bundle.len());
    }
}
