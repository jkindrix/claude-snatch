//! Phase A.0 contract tests, exercised through the deliberately awkward
//! fake provider (non-file-backed, capability-poor, multi-artifact,
//! namespace-colliding).

use super::fake::{colliding_key, multi_artifact_key, FakeProvider};
use super::*;

#[test]
fn namespaces_prevent_native_id_collision() {
    let a = multi_artifact_key();
    let b = colliding_key();
    assert_eq!(a.native_id, b.native_id);
    assert_ne!(
        a, b,
        "same native id in different namespaces must not collide"
    );
    // Display form of a non-global namespace includes the namespace.
    assert_eq!(a.to_string(), "fake:install-a:42");
}

#[test]
fn global_namespace_display_omits_namespace() {
    let key = LogicalSessionKey {
        provider: ProviderId::codex(),
        namespace: SessionNamespace::global(),
        native_id: "019f6d4b-d408".into(),
    };
    assert_eq!(key.to_string(), "codex:019f6d4b-d408");
}

#[test]
fn artifact_identity_survives_revision_change() {
    // An append to an active session changes the revision, never the identity.
    let before = ArtifactSnapshot {
        id: ArtifactId {
            provider_instance: "root".into(),
            locator: "s.jsonl".into(),
        },
        revision: ArtifactRevision("size=100".into()),
    };
    let after = ArtifactSnapshot {
        id: before.id.clone(),
        revision: ArtifactRevision("size=200".into()),
    };
    assert_eq!(before.id, after.id);
    assert_ne!(before, after, "snapshots differ when revision differs");
}

#[test]
fn twin_precedence_prefers_active_then_plain() {
    let plain_archived = SessionArtifact {
        snapshot: ArtifactSnapshot {
            id: ArtifactId {
                provider_instance: "r".into(),
                locator: "archived/s.jsonl".into(),
            },
            revision: ArtifactRevision("1".into()),
        },
        form: ArtifactForm::PlainFile,
        archived: true,
    };
    let compressed_active = SessionArtifact {
        snapshot: ArtifactSnapshot {
            id: ArtifactId {
                provider_instance: "r".into(),
                locator: "s.jsonl.zst".into(),
            },
            revision: ArtifactRevision("1".into()),
        },
        form: ArtifactForm::CompressedFile,
        archived: false,
    };
    let plain_active = SessionArtifact {
        snapshot: ArtifactSnapshot {
            id: ArtifactId {
                provider_instance: "r".into(),
                locator: "s.jsonl".into(),
            },
            revision: ArtifactRevision("1".into()),
        },
        form: ArtifactForm::PlainFile,
        archived: false,
    };
    let key = LogicalSessionKey {
        provider: ProviderId::claude_code(),
        namespace: SessionNamespace::global(),
        native_id: "x".into(),
    };

    // Active beats archived even when the archived twin is plain.
    let d = SessionDescriptor {
        key: key.clone(),
        artifacts: vec![plain_archived.clone(), compressed_active.clone()],
    };
    assert_eq!(d.preferred_artifact(), Some(&compressed_active));

    // Among active copies, plain beats compressed.
    let d = SessionDescriptor {
        key,
        artifacts: vec![compressed_active, plain_active.clone()],
    };
    assert_eq!(d.preferred_artifact(), Some(&plain_active));
}

#[test]
fn fake_provider_multi_artifact_prefers_live_db_copy() {
    let sessions = FakeProvider.sessions().unwrap();
    let multi = sessions
        .iter()
        .find(|d| d.key == multi_artifact_key())
        .unwrap();
    assert_eq!(multi.artifacts.len(), 2);
    let preferred = multi.preferred_artifact().unwrap();
    assert!(!preferred.archived);
    assert_eq!(preferred.form, ArtifactForm::Database);
}

#[test]
fn capability_gating_errors_are_explicit() {
    let p = FakeProvider;
    assert!(!p.capabilities().native_export);
    assert!(!p.capabilities().raw_jsonl);
    // Universal archive tier always works.
    assert!(p.read_archive(&multi_artifact_key()).is_ok());
    // Optional tiers refuse loudly, not silently.
    let art = ArtifactId {
        provider_instance: "mem://install-a".into(),
        locator: "table=sessions;rowid=42".into(),
    };
    assert!(matches!(
        p.read_native(&art),
        Err(ProviderError::Unsupported { .. })
    ));
    assert!(matches!(
        p.read_raw_jsonl(&multi_artifact_key()),
        Err(ProviderError::Unsupported { .. })
    ));
}

#[test]
fn provenance_expresses_one_to_many_and_many_to_one() {
    let parsed = FakeProvider.parse(&multi_artifact_key()).unwrap();
    assert!(
        parsed.validate_provenance().is_empty(),
        "fake session must be internally consistent: {:?}",
        parsed.validate_provenance()
    );
    // 1:N — record 0 produced two entries.
    let mapped_from_zero = parsed
        .record_dispositions
        .iter()
        .find(|d| d.record.ordinal == 0)
        .unwrap();
    assert!(matches!(&mapped_from_zero.outcome, RecordOutcome::Mapped(e) if e.len() == 2));
    // N:1 — one entry has two origin records.
    assert!(parsed.entry_origins.values().any(|o| o.len() == 2));
    // Every record accounted for (invariant #1).
    assert_eq!(parsed.record_dispositions.len(), 5);
    assert_eq!(
        parsed.diagnostics,
        IngestionDiagnostics {
            mapped: 3,
            suppressed: 1,
            unknown: 1,
            unparseable: 0
        }
    );
}

#[test]
fn validator_catches_broken_provenance() {
    let mut parsed = FakeProvider.parse(&multi_artifact_key()).unwrap();

    // Origin pointing at a record that is not Mapped.
    let bogus_record = RecordRef {
        artifact: parsed.record_dispositions[0].record.artifact.clone(),
        ordinal: 999,
    };
    parsed
        .entry_origins
        .insert(EntryId("fake:42:999:0".into()), vec![bogus_record]);
    assert!(parsed
        .validate_provenance()
        .iter()
        .any(|v| v.contains("not Mapped")));

    // Duplicate disposition for the same record.
    let mut parsed = FakeProvider.parse(&multi_artifact_key()).unwrap();
    let dup = parsed.record_dispositions[0].clone();
    parsed.record_dispositions.push(dup);
    assert!(parsed
        .validate_provenance()
        .iter()
        .any(|v| v.contains("more than one disposition")));

    // Mapped entry missing from entry_origins.
    let mut parsed = FakeProvider.parse(&multi_artifact_key()).unwrap();
    parsed.entry_origins.clear();
    assert!(parsed
        .validate_provenance()
        .iter()
        .any(|v| v.contains("no origins")));
}

#[test]
fn entry_ids_are_deterministic_and_stable() {
    let a = EntryId::deterministic(&ProviderId::codex(), "thread-1", 17, 2);
    let b = EntryId::deterministic(&ProviderId::codex(), "thread-1", 17, 2);
    assert_eq!(a, b);
    assert_eq!(a.0, "codex:thread-1:17:2");
    // Re-parsing (same fake input) yields identical ids and origins.
    let p1 = FakeProvider.parse(&multi_artifact_key()).unwrap();
    let p2 = FakeProvider.parse(&multi_artifact_key()).unwrap();
    assert_eq!(p1.entry_origins, p2.entry_origins);
}

#[test]
fn semantic_axes_are_independent() {
    // A steered message: human-authored, mid-turn-delivered — the two axes
    // must be expressible independently (the old single enum could not).
    let authorship = PromptAuthorship::Human;
    let delivery = PromptDelivery::MidTurn;
    assert_eq!(authorship, PromptAuthorship::Human);
    assert_eq!(delivery, PromptDelivery::MidTurn);
    // Codex token_count: same event carries a Call/Delta and a
    // Session/Cumulative observation.
    let last = (UsageScope::Call, UsageAggregation::Delta);
    let total = (UsageScope::Session, UsageAggregation::Cumulative);
    assert_ne!(last, total);
}
