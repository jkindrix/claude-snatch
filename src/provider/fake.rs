//! Deliberately awkward in-memory provider for exercising the seam.
//!
//! Per the design review: a fake that merely resembles Claude JSONL would not
//! test the seam honestly. This one is **non-file-backed** (database form),
//! advertises **neither** `native` nor `raw-jsonl`, gives one logical session
//! **multiple artifacts** (a live DB range plus an archived imported copy),
//! and hosts two sessions whose native ids collide across namespaces.

use std::collections::BTreeMap;

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

impl SourceProvider for FakeProvider {
    fn id(&self) -> ProviderId {
        provider_id()
    }

    fn capabilities(&self) -> ProviderCapabilities {
        // Non-file-backed: no exact source bytes, no JSONL stream.
        ProviderCapabilities {
            native_export: false,
            raw_jsonl: false,
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
                artifacts: vec![live_artifact()],
            },
        ])
    }

    fn parse(&self, key: &LogicalSessionKey) -> Result<ParsedSession, ProviderError> {
        if *key != multi_artifact_key() {
            return Err(ProviderError::NotFound(key.to_string()));
        }
        // Five native records exercising every cardinality:
        //   record 0 -> two entries (1:N)
        //   records 1+2 -> one entry (N:1)
        //   record 3 suppressed (duplicate stream)
        //   record 4 unknown (drift)
        let art = live_artifact().snapshot.id;
        let rec = |ordinal| RecordRef {
            artifact: art.clone(),
            ordinal,
        };
        let e = |ordinal, sub| EntryId::deterministic(&provider_id(), "42", ordinal, sub);

        let entries = vec![
            LogEntry::Unknown(serde_json::json!({"fake": "entry-0-0"})),
            LogEntry::Unknown(serde_json::json!({"fake": "entry-0-1"})),
            LogEntry::Unknown(serde_json::json!({"fake": "entry-1-0"})),
        ];
        let mut entry_origins = BTreeMap::new();
        entry_origins.insert(e(0, 0), vec![rec(0)]);
        entry_origins.insert(e(0, 1), vec![rec(0)]);
        entry_origins.insert(e(1, 0), vec![rec(1), rec(2)]);

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
                    reason: SuppressionReason::DuplicateStream,
                },
            },
            RecordDisposition {
                record: rec(4),
                outcome: RecordOutcome::Unknown,
            },
        ];

        Ok(ParsedSession {
            descriptor: SessionDescriptor {
                key: key.clone(),
                artifacts: vec![imported_artifact(), live_artifact()],
            },
            entries,
            entry_origins,
            record_dispositions,
            diagnostics: IngestionDiagnostics {
                mapped: 3,
                suppressed: 1,
                unknown: 1,
                unparseable: 0,
            },
        })
    }

    fn read_archive(&self, key: &LogicalSessionKey) -> Result<Vec<u8>, ProviderError> {
        // Universal tier: a provider-defined lossless bundle.
        Ok(format!("FAKE-ARCHIVE-BUNDLE {key}").into_bytes())
    }

    fn read_native(&self, _artifact: &ArtifactId) -> Result<Vec<u8>, ProviderError> {
        Err(ProviderError::Unsupported {
            capability: "native export",
        })
    }

    fn read_raw_jsonl(&self, _key: &LogicalSessionKey) -> Result<Vec<u8>, ProviderError> {
        Err(ProviderError::Unsupported {
            capability: "raw-jsonl",
        })
    }
}
