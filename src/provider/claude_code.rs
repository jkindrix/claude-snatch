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
use std::io::{BufRead, BufReader, Write};
use std::time::UNIX_EPOCH;

use super::{
    ArtifactForm, ArtifactId, ArtifactRevision, ArtifactSnapshot, EntryId, IdentifiedEntry,
    IngestionDiagnostics, LineageEdge, LineageEdgeKind, LogicalSessionKey, ParseDiagnostic,
    ParsedSession, ProviderCapabilities, ProviderError, ProviderId, RecordDisposition,
    RecordOutcome, RecordRef, SessionArtifact, SessionDescriptor, SessionNamespace, SourceProvider,
    SuppressionReason,
};
use crate::discovery::chain::extract_session_link;
use crate::discovery::{ClaudeDirectory, Session};
use crate::model::LogEntry;
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
            // The Claude adapter does not yet emit prompt/turn semantics;
            // surfaces must keep classic heuristics for it (round-23).
            semantic_annotations: false,
            pricing: crate::provider::ProviderPricing::KnownModelRates,
        }
    }

    fn sessions(&self) -> Result<Vec<SessionDescriptor>, ProviderError> {
        Ok(self.descriptors()?.into_iter().map(|(d, _)| d).collect())
    }

    fn project_context(
        &self,
        key: &LogicalSessionKey,
    ) -> Result<super::project::SessionProjectContext, ProviderError> {
        let (_, session) = self.resolve(key)?;
        let metadata = session
            .quick_metadata_cached()
            .map_err(|error| ProviderError::Other(error.to_string()))?;
        Ok(super::project::SessionProjectContext {
            cwd: Some(
                metadata
                    .extracted_cwd
                    .unwrap_or_else(|| session.project_path().to_string()),
            ),
            git_branch: metadata.git_branch,
            started_at: metadata.start_time,
            ended_at: metadata.end_time,
            modified_at: Some(session.modified_datetime()),
            artifact_bytes: session.file_size(),
            ..Default::default()
        })
    }

    fn parse_cache_token(&self, key: &LogicalSessionKey) -> Result<String, ProviderError> {
        let (descriptor, _) = self.resolve(key)?;
        Ok(format!(
            "v1\x1eclaude-code\x1e{}\x1emax_file={:?}",
            super::descriptor_state_token(&descriptor),
            self.max_file_size
        ))
    }

    fn parse(&self, key: &LogicalSessionKey) -> Result<ParsedSession, ProviderError> {
        let (descriptor, session) = self.resolve(key)?;
        if let Some(max) = self.max_file_size {
            let len = std::fs::metadata(session.path())?.len();
            if max > 0 && len > max {
                return Err(ProviderError::Other(format!(
                    "session file {} exceeds max_file_size ({len} > {max} bytes)",
                    session.path().display()
                )));
            }
        }
        let artifact_id = self.artifact_for(&session).snapshot.id.clone();

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
                    // Torn/fused line? Mirror the established parser's
                    // salvage before declaring the record unparseable.
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
            diagnostics,
        })
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
        let (_, session) = p.resolve(&key(SESSION_A)).unwrap();
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
