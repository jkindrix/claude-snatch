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
    ArtifactForm, ArtifactId, ArtifactRevision, ArtifactSnapshot, EntryId, IdentifiedEntry,
    IngestionDiagnostics, LineageEdge, LineageEdgeKind, LogicalSessionKey, ParseDiagnostic,
    ParsedSession, ProviderCapabilities, ProviderError, ProviderId, RecordDisposition,
    RecordOutcome, RecordRef, SessionArtifact, SessionDescriptor, SessionNamespace, SourceProvider,
};
use crate::model::LogEntry;

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
    let bytes: Vec<u8> = {
        use std::os::unix::ffi::OsStrExt;
        path.as_os_str().as_bytes().to_vec()
    };
    #[cfg(not(unix))]
    let bytes: Vec<u8> = path.to_string_lossy().into_owned().into_bytes();

    let mut out = String::with_capacity(bytes.len());
    for b in bytes {
        match b {
            b'%' => out.push_str("%25"),
            0x20..=0x7e => out.push(b as char),
            other => out.push_str(&format!("%{other:02X}")),
        }
    }
    out
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

impl SourceProvider for CodexProvider {
    fn id(&self) -> ProviderId {
        ProviderId::codex()
    }

    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities {
            native_export: true,
            raw_jsonl: true,
        }
    }

    fn sessions(&self) -> Result<Vec<SessionDescriptor>, ProviderError> {
        self.descriptors()
    }

    fn parse(&self, key: &LogicalSessionKey) -> Result<ParsedSession, ProviderError> {
        let (descriptor, path) = self.resolve(key)?;

        if self.sniff_format(&path)? == FormatFamily::Legacy {
            return Err(ProviderError::Unsupported {
                capability: "legacy pre-envelope rollout normalization (Codex ≤0.31.0); \
                             native/raw export remains available",
            });
        }

        let artifact_id = ArtifactId {
            provider_instance: self.codex_home.display().to_string(),
            locator: path.display().to_string(),
        };
        let mut reader = self.open_records(&path)?;
        let mut entries = Vec::new();
        let mut entry_origins = BTreeMap::new();
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
                    // B1: envelope records are preserved, honestly unmodeled.
                    // Normalization (B3) flips these to Mapped with the same
                    // deterministic ids.
                    let id = EntryId::deterministic(key, record.ordinal, 0);
                    entries.push(IdentifiedEntry {
                        id: id.clone(),
                        entry: LogEntry::Unknown(value),
                    });
                    entry_origins.insert(id.clone(), vec![record.clone()]);
                    diagnostics.unknown += 1;
                    record_dispositions.push(RecordDisposition {
                        record,
                        outcome: RecordOutcome::Unknown { entries: vec![id] },
                    });
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

        Ok(ParsedSession {
            descriptor,
            entries,
            entry_origins,
            record_dispositions,
            semantics: BTreeMap::new(),
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
        // 4 envelope records preserved as Unknown (incl. the unknown type),
        // 1 truncated trailing line unparseable.
        assert_eq!(
            parsed.diagnostics,
            IngestionDiagnostics {
                mapped: 0,
                suppressed: 0,
                unknown: 4,
                recovered: 0,
                unparseable: 1
            }
        );
        assert_eq!(parsed.entries.len(), 4);
    }

    #[test]
    fn zst_only_session_parses_via_streaming_decode() {
        let (_tmp, p) = fixture();
        let parsed = p.parse(&key(THREAD_B)).unwrap();
        assert_eq!(parsed.diagnostics.unknown, 2);
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
        assert_eq!(parsed.diagnostics.unknown, 4);
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
            assert_eq!(
                parsed.diagnostics.unknown, 2,
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
        assert_eq!(parsed.diagnostics.unknown, 4);
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
        assert_eq!(a.entries.len(), 8);
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
        assert!(
            parsed.diagnostics.unparseable >= 1,
            "checksum failure must be recorded: {:?}",
            parsed.diagnostics
        );
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
        assert!(parsed.diagnostics.unparseable >= 1);
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
        // Their display strings collide (replacement characters)...
        assert_eq!(d1.display().to_string(), d2.display().to_string());
        std::fs::write(
            d1.join(format!("rollout-2026-07-16T01-00-00-{THREAD_A}.jsonl")),
            session_a_content(),
        )
        .unwrap();
        std::fs::write(
            d2.join(format!("rollout-2026-07-16T02-00-00-{THREAD_B}.jsonl")),
            session_a_content(),
        )
        .unwrap();
        let p = CodexProvider::new(tmp.path());
        let sessions = p.sessions().unwrap();
        assert_eq!(sessions.len(), 2, "both non-UTF-8 siblings discovered");
        // ...but their encoded locators must NOT.
        let locators: std::collections::BTreeSet<_> = sessions
            .iter()
            .map(|d| d.preferred_artifact().unwrap().snapshot.id.locator.clone())
            .collect();
        assert_eq!(locators.len(), 2, "locator encoding must stay injective");
        // Parse, lineage, and archive all work through the preserved paths.
        for d in &sessions {
            assert!(p.parse(&d.key).unwrap().validate_provenance().is_empty());
        }
        p.lineage().unwrap();
        let mut bundle = Vec::new();
        p.write_archive(&key(THREAD_A), &mut bundle).unwrap();
        assert!(!bundle.is_empty());
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
