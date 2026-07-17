//! OpenAI Codex CLI as a [`SourceProvider`] (Phase B1: inventory & decoding).
//!
//! Additive: nothing in the established pipeline calls this. B1 scope is
//! discovery (plain + archived + `.zst` twins + active/truncated + legacy
//! detection), envelope decoding, and native diagnostics. **Normalization is
//! Phase B3** — envelope records are preserved as `LogEntry::Unknown` with
//! `RecordOutcome::Unknown` dispositions (content-complete, honestly
//! unmodeled). Legacy pre-envelope files (Codex ≤0.31.0, before 2025-09-10)
//! are recognized, inventoried, and native/raw-exportable; `parse()` reports
//! them unsupported-legacy until provenance-documented fixtures justify a
//! parser.
//!
//! Layout (verified against codex-rs rust-v0.144.5 and a 222-file corpus):
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
    ArtifactForm, ArtifactId, ArtifactRevision, ArtifactSnapshot, IngestionDiagnostics,
    LineageEdge, LineageEdgeKind, LogicalSessionKey, ParseDiagnostic, ParsedSession,
    ProviderCapabilities, ProviderError, ProviderId, RecordDisposition, RecordOutcome, RecordRef,
    SessionArtifact, SessionDescriptor, SessionNamespace, SourceProvider,
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
        let ok = uuid.chars().all(|c| c.is_ascii_hexdigit() || c == '-')
            && uuid.chars().filter(|c| *c == '-').count() == 4;
        ok.then_some(uuid)
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
        let (descriptor, _) = self.resolve(key)?;
        Ok(format!(
            "v1\x1ecodex\x1e{}\x1emax_c={}\x1emax_d={}\x1ewlog={WINDOW_LOG_MAX}",
            super::descriptor_state_token(&descriptor),
            self.max_compressed,
            self.max_decompressed
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

        // Normalize the parsed stream (B3 slice 1) and merge with the
        // read-level dispositions (blank/torn/unreadable) collected above.
        let normalized = super::codex_normalize::normalize(key, &parsed_records);
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
        // Fork/spawn edges come from each file's first session_meta payload
        // (forked_from_id / parent_thread_id). Dangling endpoints are kept.
        let mut edges = Vec::new();
        let (descriptors, paths) = self.inventory()?;
        for descriptor in descriptors {
            let Some(preferred) = descriptor.preferred_artifact() else {
                continue;
            };
            let Some(path) = paths.get(&preferred.snapshot.id) else {
                continue;
            };
            let mut reader = match self.open_records(path) {
                Ok(r) => r,
                Err(_) => continue,
            };
            let mut line: Vec<u8> = Vec::new();
            if reader.read_until(b'\n', &mut line).is_err()
                || line.iter().all(|b| b.is_ascii_whitespace())
            {
                continue;
            }
            let Ok(value) = serde_json::from_slice::<serde_json::Value>(&line) else {
                continue;
            };
            let payload = &value["payload"];
            if let Some(from) = payload["forked_from_id"].as_str() {
                edges.push(LineageEdge {
                    from: self.key_for(from),
                    to: descriptor.key.clone(),
                    kind: LineageEdgeKind::Fork,
                });
            }
            if let Some(parent) = payload["parent_thread_id"].as_str() {
                edges.push(LineageEdge {
                    from: self.key_for(parent),
                    to: descriptor.key.clone(),
                    kind: LineageEdgeKind::Spawn {
                        tool_use_id: None,
                        agent_type: payload["agent_role"].as_str().map(String::from),
                        description: None,
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

    const THREAD_A: &str = "019f6d4b-d408-7260-98b2-bf385f3a9763";
    const THREAD_B: &str = "019f6d11-3ce6-7662-8add-55d745876efe";
    const THREAD_LEGACY: &str = "574149a7-0712-4169-b789-67fb4742b8fc";
    const THREAD_FORK: &str = "019f7777-0000-7000-8000-000000000001";

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
        for d in &sessions {
            assert!(d.validate().is_empty(), "invalid descriptor");
            match p.parse(&d.key) {
                Ok(session) => {
                    parsed_ok += 1;
                    // Aggregate-only: no session keys in output.
                    if !session.validate_provenance().is_empty() {
                        violations += 1;
                    }
                    // Round-23 SOURCE-DERIVED semantic audits, asserted for
                    // EVERY session. The oracle reads the NATIVE record
                    // stream itself — never observations or any other
                    // normalizer output — and applies the documented rules
                    // as independently written test code.
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

                    // (a) Every DuplicateStream suppression names a mapped
                    // twin whose NATIVE record actually contains the
                    // suppressed event's content (type-correspondent twin).
                    for d in &session.record_dispositions {
                        if let RecordOutcome::Suppressed {
                            reason: super::super::SuppressionReason::DuplicateStream { twin },
                        } = &d.outcome
                        {
                            let ev = raw_by_ordinal[&d.record.ordinal];
                            let tw = raw_by_ordinal[&twin.ordinal];
                            let ev_payload = &ev["payload"];
                            let ev_text = ev_payload["message"]
                                .as_str()
                                .or_else(|| ev_payload["text"].as_str())
                                .unwrap_or("");
                            let tw_str = tw.to_string();
                            assert!(
                                tw_str.contains(
                                    serde_json::to_string(ev_text).unwrap().trim_matches('"')
                                ) || ev_text.is_empty(),
                                "duplicate-stream twin does not contain the event's content"
                            );
                        }
                    }

                    // (b) Usage: partition completeness + reconciliation
                    // from SOURCE records. Every native token_count carrying
                    // total_token_usage must be either an origin of some
                    // entry (attributed) or a preserved Unknown entry —
                    // never absent (no-loss). Expected sums come from the
                    // documented epoch/basis rules applied to the raw
                    // stream by THIS test.
                    let mut token_ordinals: Vec<(u64, (u64, u64, u64))> = Vec::new();
                    for (o, v) in &raw_records {
                        if v["type"] == "event_msg"
                            && v["payload"]["type"] == "token_count"
                            && v["payload"]["info"]["total_token_usage"].is_object()
                        {
                            let u = &v["payload"]["info"]["total_token_usage"];
                            let g = |k: &str| u[k].as_u64().unwrap_or(0);
                            token_ordinals.push((
                                *o,
                                (
                                    g("input_tokens"),
                                    g("cached_input_tokens"),
                                    g("output_tokens"),
                                ),
                            ));
                        }
                    }
                    let mut attributed: std::collections::BTreeSet<u64> =
                        std::collections::BTreeSet::new();
                    for origins in session.entry_origins.values() {
                        for r in &origins[1..] {
                            attributed.insert(r.ordinal);
                        }
                    }
                    let preserved: std::collections::BTreeSet<u64> = session
                        .entries
                        .iter()
                        .filter(|e| {
                            matches!(&e.entry, crate::model::LogEntry::Unknown(v)
                                if v["payload"]["type"] == "token_count"
                                    && v["payload"]["info"]["total_token_usage"].is_object())
                        })
                        .map(|e| e.id.ordinal)
                        .collect();
                    for (o, _) in &token_ordinals {
                        assert!(
                            attributed.contains(o) || preserved.contains(o),
                            "usage record neither attributed nor preserved (lost)"
                        );
                    }

                    // Independent basis detection (documented rule: cached >
                    // input anywhere proves the excludes basis).
                    let excludes_basis = token_ordinals.iter().any(|(_, t)| t.1 > t.0);
                    let fresh_of = |t: &(u64, u64, u64)| {
                        if excludes_basis {
                            t.0
                        } else {
                            t.0.saturating_sub(t.1)
                        }
                    };
                    let mut expected = (0u64, 0u64, 0u64);
                    let mut prev: Option<(u64, u64, u64)> = None;
                    for (o, t) in &token_ordinals {
                        let delta = match prev {
                            None => (fresh_of(t), t.1, t.2),
                            Some(pv) => {
                                if t.0 < pv.0 || t.2 < pv.2 {
                                    (fresh_of(t), t.1, t.2)
                                } else {
                                    let (fa, fb) = (fresh_of(t), fresh_of(&pv));
                                    // Ambiguous fresh decrease contributes
                                    // zero fresh (documented rule).
                                    (fa.saturating_sub(fb), t.1.saturating_sub(pv.1), t.2 - pv.2)
                                }
                            }
                        };
                        prev = Some(*t);
                        if attributed.contains(o) {
                            expected.0 += delta.0;
                            expected.1 += delta.1;
                            expected.2 += delta.2;
                        }
                    }
                    let mut entry_sum = (0u64, 0u64, 0u64);
                    for e in &session.entries {
                        if let crate::model::LogEntry::Assistant(m) = &e.entry {
                            if let Some(u) = &m.message.usage {
                                entry_sum.0 += u.input_tokens;
                                entry_sum.1 += u.cache_read_input_tokens.unwrap_or(0);
                                entry_sum.2 += u.output_tokens;
                            }
                        }
                    }
                    assert_eq!(
                        entry_sum, expected,
                        "canonical usage must reconcile against the source-derived oracle"
                    );

                    // (c) Every usage observation names its source record
                    // directly, and that record is a real native token_count.
                    for sem in session.semantics.values() {
                        for obs in &sem.usage {
                            let raw = raw_by_ordinal
                                .get(&obs.record.ordinal)
                                .expect("observation names a real record");
                            assert_eq!(
                                raw["payload"]["type"], "token_count",
                                "observation source must be a token_count record"
                            );
                        }
                    }
                    // Completeness: the parser must have reached every
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
        let edges = p.lineage().map(|e| e.len()).unwrap_or(0);
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
             {errors} errors, {violations} provenance violations, {edges} lineage edges, \
             {raced} raced, records: {totals:?}",
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
    fn excludes_basis_is_detected_and_ambiguity_is_surfaced() {
        // Era shape found in the corpus: cumulative cached exceeds
        // cumulative input → the input number EXCLUDES cached tokens.
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
            tc(100, 900, 10), // cached > input: proves excludes basis
            tc(150, 1400, 20),
        ]);
        let parsed = p.parse(&key(THREAD_A)).unwrap();
        let sem = parsed
            .semantics
            .values()
            .find(|s| !s.usage.is_empty())
            .expect("usage semantics");
        assert!(sem
            .usage
            .iter()
            .all(|o| matches!(o.basis, crate::provider::UsageBasis::InputExcludesCached)));
        // Under the excludes basis fresh = input: 100 then +50 = 150 total.
        let mut fresh = 0u64;
        for e in &parsed.entries {
            if let crate::model::LogEntry::Assistant(m) = &e.entry {
                if let Some(u) = &m.message.usage {
                    fresh += u.input_tokens;
                }
            }
        }
        assert_eq!(fresh, 150, "excludes basis: fresh input is the raw input");
        // Every observation names its source record directly.
        for o in &sem.usage {
            assert!(o.record.ordinal == 2 || o.record.ordinal == 3);
        }

        // Ambiguity: an includes-basis session whose derived fresh drops
        // without a reset is SURFACED, and contributes zero fresh.
        let (_tmp2, p2) = normalize_home(&[
            envelope_line("session_meta", serde_json::json!({"id": THREAD_A})),
            envelope_line(
                "response_item",
                serde_json::json!({"type": "message", "role": "assistant",
                "content": [{"type": "output_text", "text": "a"}]}),
            ),
            tc(1000, 100, 10), // fresh 900
            tc(1050, 700, 20), // fresh 350: dropped, input/output monotonic
        ]);
        let parsed2 = p2.parse(&key(THREAD_A)).unwrap();
        let sem2 = parsed2
            .semantics
            .values()
            .find(|s| !s.usage.is_empty())
            .unwrap();
        assert!(
            sem2.usage.iter().any(|o| o.ambiguous),
            "the uninterpretable transition must be surfaced, not clamped away"
        );
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
    fn web_search_call_maps_as_web_tool_use() {
        let (_tmp, p) = normalize_home(&[
            envelope_line("session_meta", serde_json::json!({"id": THREAD_A})),
            envelope_line(
                "response_item",
                serde_json::json!({"type": "web_search_call", "status": "completed",
                "action": {"type": "open_page", "url": "https://example.com"}}),
            ),
        ]);
        let parsed = p.parse(&key(THREAD_A)).unwrap();
        let id = crate::provider::EntryId::deterministic(&key(THREAD_A), 1, 0);
        let entry = parsed.entries.iter().find(|e| e.id == id).unwrap();
        let is_web_tool = matches!(&entry.entry, crate::model::LogEntry::Assistant(m)
            if m.message.content.iter().any(|b| matches!(b, crate::model::ContentBlock::ToolUse(t) if t.name == "web_search")));
        assert!(is_web_tool);
        let sem = parsed.semantics.get(&id).unwrap();
        assert!(sem
            .tools
            .values()
            .any(|t| matches!(t.kind, crate::provider::ToolKind::Web)));
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
