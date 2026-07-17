//! Deliberately awkward in-memory provider for exercising the seam.
//!
//! Per the design review: a fake that merely resembles Claude JSONL would not
//! test the seam honestly. This one is **non-file-backed** (database form),
//! advertises **neither** `native` nor `raw-jsonl`, gives one logical session
//! **multiple artifacts** (a live DB range plus an archived imported copy),
//! hosts two sessions whose native ids collide across namespaces, carries a
//! **real native record store** so archives are demonstrably lossless, emits
//! **semantics** (steered prompt, dual usage observations, tool kinds,
//! fork-inherited history), and exercises every provenance cardinality:
//! 1:N, N:1, suppression, unknown-but-preserved, and inherited history.

use std::collections::BTreeMap;
use std::io::Write;

use super::*;

/// In-memory provider with hostile-to-assumptions shapes.
pub struct FakeProvider;

/// Namespace of the primary fake installation.
pub fn ns_primary() -> SessionNamespace {
    SessionNamespace("install-a".into())
}

/// Namespace of a second, unrelated installation whose native ids collide.
pub fn ns_secondary() -> SessionNamespace {
    SessionNamespace("install-b".into())
}

fn provider_id() -> ProviderId {
    ProviderId("fake".into())
}

/// The multi-artifact session's logical key ("db-local" integer-style id).
pub fn multi_artifact_key() -> LogicalSessionKey {
    LogicalSessionKey {
        provider: provider_id(),
        namespace: ns_primary(),
        native_id: "42".into(),
    }
}

/// The colliding session in the other namespace (same native id).
pub fn colliding_key() -> LogicalSessionKey {
    LogicalSessionKey {
        provider: provider_id(),
        namespace: ns_secondary(),
        native_id: "42".into(),
    }
}

fn live_artifact() -> SessionArtifact {
    SessionArtifact {
        snapshot: ArtifactSnapshot {
            id: ArtifactId {
                provider_instance: "mem://install-a".into(),
                locator: "table=sessions;rowid=42".into(),
            },
            revision: ArtifactRevision("rev-7".into()),
        },
        form: ArtifactForm::Database,
        archived: false,
    }
}

fn imported_artifact() -> SessionArtifact {
    SessionArtifact {
        snapshot: ArtifactSnapshot {
            id: ArtifactId {
                provider_instance: "mem://backup-root".into(),
                locator: "import/42.bundle".into(),
            },
            revision: ArtifactRevision("rev-frozen".into()),
        },
        form: ArtifactForm::Other("import-bundle".into()),
        archived: true,
    }
}

fn secondary_artifact() -> SessionArtifact {
    SessionArtifact {
        snapshot: ArtifactSnapshot {
            id: ArtifactId {
                provider_instance: "mem://install-b".into(),
                locator: "table=sessions;rowid=42".into(),
            },
            revision: ArtifactRevision("rev-1".into()),
        },
        form: ArtifactForm::Database,
        archived: false,
    }
}

/// The primary session's native record store: what the archive tier must
/// preserve losslessly. Six records exercising every cardinality:
///   0 -> two entries (1:N)
///   1+2 -> one entry (N:1)
///   3 suppressed (duplicate stream)
///   4 unknown-but-preserved (drift)
///   5 fork-inherited user message (Mapped + InheritedHistory)
pub fn native_records() -> Vec<serde_json::Value> {
    vec![
        serde_json::json!({"kind": "prompt", "text": "steered mid-turn ask", "steered": true}),
        serde_json::json!({"kind": "tool_call_part1", "tool": "fake_apply_patch"}),
        serde_json::json!({"kind": "tool_call_part2", "usage": {"last": 10, "total": 200}}),
        serde_json::json!({"kind": "mirror_event", "text": "steered mid-turn ask"}),
        serde_json::json!({"kind": "mystery_v9", "payload": {"future": true}}),
        serde_json::json!({"kind": "prompt", "text": "copied from fork source", "inherited": true}),
    ]
}

impl SourceProvider for FakeProvider {
    fn id(&self) -> ProviderId {
        provider_id()
    }

    fn capabilities(&self) -> ProviderCapabilities {
        // Non-file-backed: no exact source bytes, no JSONL stream.
        ProviderCapabilities {
            native_export: false,
            raw_jsonl: false,
            semantic_annotations: true,
        }
    }

    fn sessions(&self) -> Result<Vec<SessionDescriptor>, ProviderError> {
        Ok(vec![
            SessionDescriptor {
                key: multi_artifact_key(),
                artifacts: vec![imported_artifact(), live_artifact()],
            },
            SessionDescriptor {
                key: colliding_key(),
                artifacts: vec![secondary_artifact()],
            },
        ])
    }

    fn parse_cache_token(&self, key: &LogicalSessionKey) -> Result<String, ProviderError> {
        let descriptor = self
            .sessions()?
            .into_iter()
            .find(|d| d.key == *key)
            .ok_or_else(|| ProviderError::NotFound(key.to_string()))?;
        Ok(format!(
            "v1\x1efake\x1e{}",
            super::descriptor_state_token(&descriptor)
        ))
    }

    fn parse(&self, key: &LogicalSessionKey) -> Result<ParsedSession, ProviderError> {
        if *key == colliding_key() {
            // Minimal one-record session so cross-namespace entry ids can be
            // compared against the primary session's.
            let rec = RecordRef {
                artifact: secondary_artifact().snapshot.id,
                ordinal: 0,
            };
            let e = EntryId::deterministic(key, 0, 0);
            let mut entry_origins = BTreeMap::new();
            entry_origins.insert(e.clone(), vec![rec.clone()]);
            return Ok(ParsedSession {
                descriptor: SessionDescriptor {
                    key: key.clone(),
                    artifacts: vec![secondary_artifact()],
                },
                entries: vec![IdentifiedEntry {
                    id: e.clone(),
                    entry: LogEntry::Unknown(serde_json::json!({"fake": "b-entry"})),
                }],
                entry_origins,
                record_dispositions: vec![RecordDisposition {
                    record: rec,
                    outcome: RecordOutcome::Mapped(vec![e]),
                }],
                field_derivations: Vec::new(),
                semantics: BTreeMap::new(),
                diagnostics: IngestionDiagnostics {
                    mapped: 1,
                    ..Default::default()
                },
            });
        }
        if *key != multi_artifact_key() {
            return Err(ProviderError::NotFound(key.to_string()));
        }

        let art = live_artifact().snapshot.id;
        let rec = |ordinal| RecordRef {
            artifact: art.clone(),
            ordinal,
        };
        let e = |ordinal, sub| EntryId::deterministic(key, ordinal, sub);
        let records = native_records();

        let entries = vec![
            IdentifiedEntry {
                id: e(0, 0),
                entry: LogEntry::Unknown(records[0].clone()),
            },
            IdentifiedEntry {
                id: e(0, 1),
                entry: LogEntry::Unknown(serde_json::json!({"kind": "prompt_echo"})),
            },
            IdentifiedEntry {
                id: e(1, 0),
                // A real assistant entry with two tool calls, so per-call
                // semantics can be validated against actual call ids.
                entry: serde_json::from_value(serde_json::json!({
                    "type": "assistant",
                    "uuid": "fake-a1",
                    "parentUuid": null,
                    "timestamp": "2026-01-01T00:00:00Z",
                    "sessionId": "42",
                    "version": "0.0.0",
                    "message": {
                        "id": "fake-m1",
                        "type": "message",
                        "role": "assistant",
                        "model": "fake-model",
                        "content": [
                            {"type": "tool_use", "id": "call-7",
                             "name": "fake_apply_patch", "input": {}},
                            {"type": "tool_use", "id": "call-8",
                             "name": "fake_exec", "input": {}}
                        ]
                    }
                }))
                .expect("valid assistant entry"),
            },
            IdentifiedEntry {
                id: e(4, 0),
                entry: LogEntry::Unknown(records[4].clone()),
            },
            IdentifiedEntry {
                id: e(5, 0),
                entry: LogEntry::Unknown(records[5].clone()),
            },
        ];

        let mut entry_origins = BTreeMap::new();
        entry_origins.insert(e(0, 0), vec![rec(0)]);
        entry_origins.insert(e(0, 1), vec![rec(0)]);
        entry_origins.insert(e(1, 0), vec![rec(1), rec(2)]);
        entry_origins.insert(e(4, 0), vec![rec(4)]);
        entry_origins.insert(e(5, 0), vec![rec(5)]);

        let record_dispositions = vec![
            RecordDisposition {
                record: rec(0),
                outcome: RecordOutcome::Mapped(vec![e(0, 0), e(0, 1)]),
            },
            RecordDisposition {
                record: rec(1),
                outcome: RecordOutcome::Mapped(vec![e(1, 0)]),
            },
            RecordDisposition {
                record: rec(2),
                outcome: RecordOutcome::Mapped(vec![e(1, 0)]),
            },
            RecordDisposition {
                record: rec(3),
                outcome: RecordOutcome::Suppressed {
                    reason: SuppressionReason::DuplicateStream { twin: rec(0) },
                },
            },
            RecordDisposition {
                record: rec(4),
                outcome: RecordOutcome::Unknown {
                    entries: vec![e(4, 0)],
                },
            },
            RecordDisposition {
                record: rec(5),
                outcome: RecordOutcome::Mapped(vec![e(5, 0)]),
            },
        ];

        let mut semantics = BTreeMap::new();
        // Steered prompt: human-authored, mid-turn-delivered.
        semantics.insert(
            e(0, 0),
            EntrySemantics {
                prompt: Some(PromptSemantics {
                    authorship: PromptAuthorship::Human,
                    delivery: PromptDelivery::MidTurn,
                }),
                ..Default::default()
            },
        );
        // Merged tool call carrying TWO calls with different classifications
        // (one entry, several tool calls), plus a dual usage observation
        // (Codex token_count shape) where each annotation carries its own
        // values.
        let mut tools = BTreeMap::new();
        tools.insert(
            "call-7".to_string(),
            ToolSemantics {
                kind: ToolKind::FileWrite,
                native_name: "fake_apply_patch".into(),
            },
        );
        tools.insert(
            "call-8".to_string(),
            ToolSemantics {
                kind: ToolKind::Shell,
                native_name: "fake_exec".into(),
            },
        );
        semantics.insert(
            e(1, 0),
            EntrySemantics {
                tools,
                usage: vec![
                    UsageObservation {
                        scope: UsageScope::Call,
                        aggregation: UsageAggregation::Delta,
                        record: rec(2),
                        basis: super::UsageBasis::InputIncludesCached,
                        ambiguous: false,
                        input_tokens: 10,
                        cached_input_tokens: 0,
                        output_tokens: 5,
                    },
                    UsageObservation {
                        scope: UsageScope::Session,
                        aggregation: UsageAggregation::Cumulative,
                        record: rec(2),
                        basis: super::UsageBasis::InputIncludesCached,
                        ambiguous: false,
                        input_tokens: 200,
                        cached_input_tokens: 0,
                        output_tokens: 50,
                    },
                ],
                ..Default::default()
            },
        );
        // Fork-inherited history: mapped, present, excluded from "new work".
        semantics.insert(
            e(5, 0),
            EntrySemantics {
                activity: ActivityKind::InheritedHistory,
                ..Default::default()
            },
        );

        Ok(ParsedSession {
            descriptor: SessionDescriptor {
                key: key.clone(),
                artifacts: vec![imported_artifact(), live_artifact()],
            },
            entries,
            entry_origins,
            record_dispositions,
            field_derivations: Vec::new(),
            semantics,
            diagnostics: IngestionDiagnostics {
                mapped: 4,
                suppressed: 1,
                unknown: 1,
                recovered: 0,
                unparseable: 0,
            },
        })
    }

    fn lineage(&self) -> Result<Vec<LineageEdge>, ProviderError> {
        Ok(vec![LineageEdge {
            from: multi_artifact_key(),
            to: colliding_key(),
            kind: LineageEdgeKind::Fork,
        }])
    }

    fn write_archive(
        &self,
        key: &LogicalSessionKey,
        out: &mut dyn Write,
    ) -> Result<(), ProviderError> {
        if *key != multi_artifact_key() && *key != colliding_key() {
            return Err(ProviderError::NotFound(key.to_string()));
        }
        // Lossless bundle: manifest + the native records, round-trippable.
        let records = if *key == multi_artifact_key() {
            native_records()
        } else {
            vec![serde_json::json!({"kind": "prompt", "text": "b"})]
        };
        let descriptor = self
            .sessions()?
            .into_iter()
            .find(|d| d.key == *key)
            .expect("session listed above");
        let bundle = serde_json::json!({
            "manifest": {
                "provider": self.id().0,
                "session": key.to_string(),
                "artifacts": descriptor
                    .artifacts
                    .iter()
                    .map(|a| serde_json::json!({
                        "instance": a.snapshot.id.provider_instance,
                        "locator": a.snapshot.id.locator,
                        "revision": a.snapshot.revision.0,
                        "archived": a.archived,
                    }))
                    .collect::<Vec<_>>(),
            },
            "records": records,
        });
        serde_json::to_writer(&mut *out, &bundle)
            .map_err(|e| ProviderError::Other(format!("bundle serialization: {e}")))?;
        Ok(())
    }

    fn write_native(
        &self,
        _artifact: &ArtifactId,
        _out: &mut dyn Write,
    ) -> Result<(), ProviderError> {
        Err(ProviderError::Unsupported {
            capability: "native export",
        })
    }

    fn write_raw_jsonl(
        &self,
        _key: &LogicalSessionKey,
        _out: &mut dyn Write,
    ) -> Result<(), ProviderError> {
        Err(ProviderError::Unsupported {
            capability: "raw-jsonl",
        })
    }
}
