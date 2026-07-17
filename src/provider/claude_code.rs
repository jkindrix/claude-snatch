//! Claude Code as a [`SourceProvider`] (Phase A, milestone 1).
//!
//! Additive adapter over the existing `discovery` machinery — nothing in the
//! established pipeline calls this yet; characterization tests pin its
//! output to what `Session::parse()` produces. Threading it through the
//! CLI/MCP call sites is the rest of Phase A.
//!
//! Parsing note: entries are produced line-by-line with `LogEntry`'s
//! tolerant deserializer so every physical line gets a true record ordinal
//! and disposition. This matches `Session::parse()` on well-formed files
//! (asserted by characterization test); torn-line salvage parity arrives
//! when the shared parser is threaded in milestone 2.

use std::collections::BTreeMap;
use std::fs::File;
use std::io::{BufRead, BufReader, Write};
use std::time::UNIX_EPOCH;

use super::{
    ArtifactForm, ArtifactId, ArtifactRevision, ArtifactSnapshot, EntryId, IdentifiedEntry,
    IngestionDiagnostics, LineageEdge, LineageEdgeKind, LogicalSessionKey, ParseDiagnostic,
    ParsedSession, ProviderCapabilities, ProviderError, ProviderId, RecordDisposition,
    RecordOutcome, RecordRef, SessionArtifact, SessionDescriptor, SessionNamespace, SourceProvider,
    SuppressionReason,
};
use crate::discovery::{detect_chains, ClaudeDirectory, Session};
use crate::model::LogEntry;

/// Claude Code sessions (`~/.claude/projects/**.jsonl`) behind the provider
/// seam.
pub struct ClaudeCodeProvider {
    claude_dir: ClaudeDirectory,
}

impl ClaudeCodeProvider {
    /// Wrap a discovered Claude Code data directory.
    pub fn new(claude_dir: ClaudeDirectory) -> Self {
        ClaudeCodeProvider { claude_dir }
    }

    fn key_for(&self, session_id: &str) -> LogicalSessionKey {
        LogicalSessionKey {
            provider: ProviderId::claude_code(),
            namespace: SessionNamespace::global(),
            native_id: session_id.to_string(),
        }
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

    fn find_session(&self, key: &LogicalSessionKey) -> Result<Session, ProviderError> {
        if key.provider != ProviderId::claude_code() || key.namespace != SessionNamespace::global()
        {
            return Err(ProviderError::NotFound(key.to_string()));
        }
        self.claude_dir
            .all_sessions()
            .map_err(|e| ProviderError::Other(e.to_string()))?
            .into_iter()
            .find(|s| s.session_id() == key.native_id)
            .ok_or_else(|| ProviderError::NotFound(key.to_string()))
    }

    fn stream_file(path: &std::path::Path, out: &mut dyn Write) -> Result<(), ProviderError> {
        let mut file = File::open(path)?;
        std::io::copy(&mut file, out)?;
        Ok(())
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
        }
    }

    fn sessions(&self) -> Result<Vec<SessionDescriptor>, ProviderError> {
        let sessions = self
            .claude_dir
            .all_sessions()
            .map_err(|e| ProviderError::Other(e.to_string()))?;
        Ok(sessions
            .iter()
            .map(|s| SessionDescriptor {
                key: self.key_for(s.session_id()),
                artifacts: vec![self.artifact_for(s)],
            })
            .collect())
    }

    fn parse(&self, key: &LogicalSessionKey) -> Result<ParsedSession, ProviderError> {
        let session = self.find_session(key)?;
        let artifact_id = self.artifact_for(&session).snapshot.id.clone();
        let descriptor = SessionDescriptor {
            key: key.clone(),
            artifacts: vec![self.artifact_for(&session)],
        };

        let reader = BufReader::new(File::open(session.path())?);
        let mut entries = Vec::new();
        let mut entry_origins = BTreeMap::new();
        let mut record_dispositions = Vec::new();
        let mut diagnostics = IngestionDiagnostics::default();

        for (ordinal, line) in reader.lines().enumerate() {
            let ordinal = ordinal as u64;
            let record = RecordRef {
                artifact: artifact_id.clone(),
                ordinal,
            };
            let line = line?;
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
        let sessions = self
            .claude_dir
            .all_sessions()
            .map_err(|e| ProviderError::Other(e.to_string()))?;
        let mut edges = Vec::new();

        // Continuation edges: resume chains, consecutive members in
        // chronological order.
        let chains = detect_chains(
            sessions
                .iter()
                .filter(|s| !s.is_subagent())
                .map(|s| (s.session_id(), s.path())),
        );
        for chain in chains.values() {
            for pair in chain.members.windows(2) {
                edges.push(LineageEdge {
                    from: self.key_for(&pair[0].file_id),
                    to: self.key_for(&pair[1].file_id),
                    kind: LineageEdgeKind::Continuation,
                });
            }
        }

        // Spawn edges: subagent sidecars linked from their parent session.
        // Endpoints may dangle if a transcript was pruned; the edge is kept.
        for session in sessions.iter().filter(|s| !s.is_subagent()) {
            for link in session.subagent_links() {
                edges.push(LineageEdge {
                    from: self.key_for(session.session_id()),
                    to: self.key_for(&link.agent_session_id),
                    kind: LineageEdgeKind::Spawn,
                });
            }
        }

        Ok(edges)
    }

    fn write_archive(
        &self,
        key: &LogicalSessionKey,
        out: &mut dyn Write,
    ) -> Result<(), ProviderError> {
        // Lossless JSONL bundle: line 1 is the manifest, every subsequent
        // line is a native record verbatim (streaming; no buffering).
        let session = self.find_session(key)?;
        let artifact = self.artifact_for(&session);
        let manifest = serde_json::json!({
            "manifest": {
                "provider": self.id().0,
                "session": key.to_string(),
                "artifacts": [{
                    "instance": artifact.snapshot.id.provider_instance,
                    "locator": artifact.snapshot.id.locator,
                    "revision": artifact.snapshot.revision.0,
                    "archived": artifact.archived,
                }],
            }
        });
        serde_json::to_writer(&mut *out, &manifest)
            .map_err(|e| ProviderError::Other(format!("manifest serialization: {e}")))?;
        out.write_all(b"\n")?;
        Self::stream_file(session.path(), out)
    }

    fn write_native(
        &self,
        artifact: &ArtifactId,
        out: &mut dyn Write,
    ) -> Result<(), ProviderError> {
        if artifact.provider_instance != self.claude_dir.root().display().to_string() {
            return Err(ProviderError::NotFound(format!(
                "artifact instance {}",
                artifact.provider_instance
            )));
        }
        // The locator is a path under this provider instance's root.
        let path = std::path::Path::new(&artifact.locator);
        if !path.starts_with(self.claude_dir.root()) || !path.exists() {
            return Err(ProviderError::NotFound(format!(
                "artifact {}",
                artifact.locator
            )));
        }
        Self::stream_file(path, out)
    }

    fn write_raw_jsonl(
        &self,
        key: &LogicalSessionKey,
        out: &mut dyn Write,
    ) -> Result<(), ProviderError> {
        // This session file's records, verbatim. (Chain-order concatenation
        // across resume chains stays a consumer concern, as in the CLI's
        // chain-aware raw export.)
        let session = self.find_session(key)?;
        Self::stream_file(session.path(), out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SESSION_A: &str = "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee";
    const SESSION_B: &str = "bbbbbbbb-cccc-dddd-eeee-ffffffffffff";

    /// Claude dir with: session A (2 valid entries + 1 garbage line + 1
    /// blank line + 1 unknown-typed entry), session B continuing A
    /// (sessionId = A), and a subagent sidecar under A.
    fn fixture() -> (tempfile::TempDir, ClaudeCodeProvider) {
        let tmp = tempfile::tempdir().unwrap();
        let project = tmp.path().join("projects").join("-tmp-proj");
        std::fs::create_dir_all(&project).unwrap();

        let a = format!(
            "{}\n{}\n\nnot json at all\n{}\n",
            format_args!(
                r#"{{"type":"user","uuid":"u1","parentUuid":null,"timestamp":"2026-01-01T00:00:00Z","sessionId":"{SESSION_A}","version":"2.1.0","cwd":"/tmp/proj","message":{{"role":"user","content":"hello"}}}}"#
            ),
            format_args!(
                r#"{{"type":"assistant","uuid":"a1","parentUuid":"u1","timestamp":"2026-01-01T00:00:01Z","sessionId":"{SESSION_A}","version":"2.1.0","message":{{"id":"m1","type":"message","role":"assistant","content":[{{"type":"text","text":"hi"}}],"model":"claude-x"}}}}"#
            ),
            r#"{"type":"never-heard-of-it","payload":{"x":1}}"#,
        );
        std::fs::write(project.join(format!("{SESSION_A}.jsonl")), &a).unwrap();

        let b = format!(
            r#"{{"type":"user","uuid":"u2","parentUuid":null,"timestamp":"2026-01-01T01:00:00Z","sessionId":"{SESSION_A}","version":"2.1.0","cwd":"/tmp/proj","message":{{"role":"user","content":"resumed"}}}}"#
        );
        std::fs::write(project.join(format!("{SESSION_B}.jsonl")), format!("{b}\n")).unwrap();

        let subagents = project.join(SESSION_A).join("subagents");
        std::fs::create_dir_all(&subagents).unwrap();
        std::fs::write(
            subagents.join("agent-x1.jsonl"),
            format!(
                r#"{{"type":"assistant","uuid":"s1","parentUuid":null,"timestamp":"2026-01-01T00:30:00Z","sessionId":"{SESSION_A}","version":"2.1.0","isSidechain":true,"message":{{"id":"sm1","type":"message","role":"assistant","content":[{{"type":"text","text":"sub"}}],"model":"claude-x"}}}}"#
            ) + "\n",
        )
        .unwrap();
        std::fs::write(
            subagents.join("agent-x1.meta.json"),
            r#"{"agentType":"Explore","description":"scan"}"#,
        )
        .unwrap();

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
    fn sessions_enumerate_with_plainfile_artifacts() {
        let (_tmp, p) = fixture();
        let sessions = p.sessions().unwrap();
        let a = sessions
            .iter()
            .find(|d| d.key.native_id == SESSION_A)
            .expect("session A discovered");
        assert!(a.validate().is_empty());
        let artifact = a.preferred_artifact().unwrap();
        assert_eq!(artifact.form, ArtifactForm::PlainFile);
        assert!(!artifact.archived);
        assert_eq!(
            std::path::Path::new(&artifact.snapshot.id.locator)
                .extension()
                .and_then(|e| e.to_str()),
            Some("jsonl")
        );
        assert!(artifact.snapshot.revision.0.contains("len="));
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

        // Characterization: same entries the established path produces.
        let session = p.find_session(&key(SESSION_A)).unwrap();
        let baseline = session.parse().unwrap();
        assert_eq!(parsed.entries.len(), baseline.len());
        for (mine, theirs) in parsed.entries.iter().zip(baseline.iter()) {
            assert_eq!(
                serde_json::to_value(&mine.entry).unwrap(),
                serde_json::to_value(theirs).unwrap(),
                "provider entry diverged from Session::parse"
            );
        }

        // Every physical line accounted for: 2 mapped, 1 blank (suppressed),
        // 1 garbage (unparseable), 1 unknown-typed (preserved as
        // LogEntry::Unknown — content-complete under drift).
        assert_eq!(parsed.record_dispositions.len(), 5);
        assert_eq!(
            parsed.diagnostics,
            IngestionDiagnostics {
                mapped: 2,
                suppressed: 1,
                unknown: 1,
                unparseable: 1
            }
        );
        assert!(parsed
            .entries
            .iter()
            .any(|e| matches!(e.entry, LogEntry::Unknown(_))));
    }

    #[test]
    fn lineage_has_continuation_and_spawn_edges() {
        let (_tmp, p) = fixture();
        let edges = p.lineage().unwrap();
        assert!(
            edges.iter().any(|e| e.kind == LineageEdgeKind::Continuation
                && e.from.native_id == SESSION_A
                && e.to.native_id == SESSION_B),
            "missing continuation edge A -> B: {edges:?}"
        );
        assert!(
            edges.iter().any(|e| e.kind == LineageEdgeKind::Spawn
                && e.from.native_id == SESSION_A
                && e.to.native_id == "agent-x1"),
            "missing spawn edge A -> agent-x1: {edges:?}"
        );
    }

    #[test]
    fn raw_jsonl_is_byte_faithful_and_archive_round_trips() {
        let (_tmp, p) = fixture();
        let session = p.find_session(&key(SESSION_A)).unwrap();
        let native = std::fs::read(session.path()).unwrap();

        let mut raw = Vec::new();
        p.write_raw_jsonl(&key(SESSION_A), &mut raw).unwrap();
        assert_eq!(raw, native, "raw-jsonl must be byte-faithful");

        let mut nat = Vec::new();
        let artifact = p.artifact_for(&session).snapshot.id;
        p.write_native(&artifact, &mut nat).unwrap();
        assert_eq!(nat, native, "native must be byte-faithful");

        let mut bundle = Vec::new();
        p.write_archive(&key(SESSION_A), &mut bundle).unwrap();
        let mut lines = bundle.split(|b| *b == b'\n');
        let manifest: serde_json::Value = serde_json::from_slice(lines.next().unwrap()).unwrap();
        assert_eq!(manifest["manifest"]["provider"], "claude-code");
        let rest: Vec<u8> = bundle[bundle.iter().position(|b| *b == b'\n').unwrap() + 1..].to_vec();
        assert_eq!(
            rest, native,
            "archive body must carry native records verbatim"
        );
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
