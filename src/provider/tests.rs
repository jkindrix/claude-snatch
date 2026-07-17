//! Phase A.0 contract tests, exercised through the deliberately awkward
//! fake provider (non-file-backed, capability-poor, multi-artifact,
//! namespace-colliding, with a real record store and semantics).

use super::fake::{colliding_key, multi_artifact_key, native_records, FakeProvider};
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
    assert_eq!(a.to_string(), "fake:install-a:42");
}

#[test]
fn entry_ids_are_namespace_aware() {
    // Both fake sessions share provider + native id; their entries must not
    // collide (the pre-review EntryId did).
    let pa = FakeProvider.parse(&multi_artifact_key()).unwrap();
    let pb = FakeProvider.parse(&colliding_key()).unwrap();
    let ids_a: std::collections::BTreeSet<_> = pa.entries.iter().map(|e| &e.id).collect();
    let ids_b: std::collections::BTreeSet<_> = pb.entries.iter().map(|e| &e.id).collect();
    assert!(
        ids_a.is_disjoint(&ids_b),
        "cross-namespace sessions produced overlapping entry ids"
    );
    assert_eq!(pb.entries[0].id.to_string(), "fake:install-b:42:0:0");
}

#[test]
fn global_namespace_display_omits_namespace_but_id_includes_it() {
    let key = LogicalSessionKey {
        provider: ProviderId::codex(),
        namespace: SessionNamespace::global(),
        native_id: "019f6d4b-d408".into(),
    };
    assert_eq!(key.to_string(), "codex:019f6d4b-d408");
    // Entry-id encodings always include the namespace.
    assert_eq!(
        EntryId::deterministic(&key, 17, 2).to_string(),
        "codex:global:019f6d4b-d408:17:2"
    );
}

#[test]
fn qualified_encodings_are_injective_under_hostile_delimiters() {
    // namespace "a" + native "b:c" vs namespace "a:b" + native "c" — the
    // pre-review string concatenation rendered these identically.
    let k1 = LogicalSessionKey {
        provider: ProviderId("p".into()),
        namespace: SessionNamespace("a".into()),
        native_id: "b:c".into(),
    };
    let k2 = LogicalSessionKey {
        provider: ProviderId("p".into()),
        namespace: SessionNamespace("a:b".into()),
        native_id: "c".into(),
    };
    assert_ne!(k1, k2);
    assert_ne!(k1.to_string(), k2.to_string(), "display form must escape");
    assert_ne!(
        EntryId::deterministic(&k1, 0, 0).to_string(),
        EntryId::deterministic(&k2, 0, 0).to_string(),
        "entry-id encoding must escape"
    );

    // A global session whose native id embeds a colon must not render like a
    // namespaced session.
    let global_colon = LogicalSessionKey {
        provider: ProviderId("p".into()),
        namespace: SessionNamespace::global(),
        native_id: "a:b".into(),
    };
    let namespaced = LogicalSessionKey {
        provider: ProviderId("p".into()),
        namespace: SessionNamespace("a".into()),
        native_id: "b".into(),
    };
    assert_ne!(global_colon.to_string(), namespaced.to_string());
}

#[test]
fn artifact_identity_survives_revision_change() {
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

fn artifact(instance: &str, locator: &str, form: ArtifactForm, archived: bool) -> SessionArtifact {
    SessionArtifact {
        snapshot: ArtifactSnapshot {
            id: ArtifactId {
                provider_instance: instance.into(),
                locator: locator.into(),
            },
            revision: ArtifactRevision("1".into()),
        },
        form,
        archived,
    }
}

fn key_x() -> LogicalSessionKey {
    LogicalSessionKey {
        provider: ProviderId::claude_code(),
        namespace: SessionNamespace::global(),
        native_id: "x".into(),
    }
}

#[test]
fn twin_precedence_prefers_active_then_plain() {
    let plain_archived = artifact("r", "archived/s.jsonl", ArtifactForm::PlainFile, true);
    let compressed_active = artifact("r", "s.jsonl.zst", ArtifactForm::CompressedFile, false);
    let plain_active = artifact("r", "s.jsonl", ArtifactForm::PlainFile, false);

    // Active beats archived even when the archived twin is plain.
    let d = SessionDescriptor {
        key: key_x(),
        artifacts: vec![plain_archived.clone(), compressed_active.clone()],
    };
    assert_eq!(d.preferred_artifact(), Some(&compressed_active));

    // Among active copies, plain beats compressed.
    let d = SessionDescriptor {
        key: key_x(),
        artifacts: vec![compressed_active, plain_active.clone()],
    };
    assert_eq!(d.preferred_artifact(), Some(&plain_active));
}

#[test]
fn twin_precedence_is_stable_under_reordering() {
    // Two equivalent-rank artifacts: the tie-breaker must be stable
    // ArtifactId ordering, not discovery order.
    let a = artifact("root-a", "s.jsonl", ArtifactForm::PlainFile, false);
    let b = artifact("root-b", "s.jsonl", ArtifactForm::PlainFile, false);
    let d1 = SessionDescriptor {
        key: key_x(),
        artifacts: vec![a.clone(), b.clone()],
    };
    let d2 = SessionDescriptor {
        key: key_x(),
        artifacts: vec![b, a],
    };
    assert_eq!(
        d1.preferred_artifact(),
        d2.preferred_artifact(),
        "discovery order changed the preferred artifact"
    );
}

#[test]
fn descriptor_validation_catches_empty_and_duplicate_artifacts() {
    let empty = SessionDescriptor {
        key: key_x(),
        artifacts: vec![],
    };
    assert!(empty.validate().iter().any(|v| v.contains("no artifacts")));
    assert_eq!(empty.preferred_artifact(), None);

    let a = artifact("r", "s.jsonl", ArtifactForm::PlainFile, false);
    let dup = SessionDescriptor {
        key: key_x(),
        artifacts: vec![a.clone(), a],
    };
    assert!(dup
        .validate()
        .iter()
        .any(|v| v.contains("repeats artifact id")));
}

#[test]
fn fake_provider_multi_artifact_prefers_live_db_copy() {
    let sessions = FakeProvider.sessions().unwrap();
    for d in &sessions {
        assert!(d.validate().is_empty(), "invalid descriptor: {:?}", d.key);
    }
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
    let mut sink = Vec::new();
    // Universal archive tier always works.
    assert!(p.write_archive(&multi_artifact_key(), &mut sink).is_ok());
    // Optional tiers refuse loudly, not silently.
    let art = ArtifactId {
        provider_instance: "mem://install-a".into(),
        locator: "table=sessions;rowid=42".into(),
    };
    assert!(matches!(
        p.write_native(&art, &mut sink),
        Err(ProviderError::Unsupported { .. })
    ));
    assert!(matches!(
        p.write_raw_jsonl(&multi_artifact_key(), &mut sink),
        Err(ProviderError::Unsupported { .. })
    ));
}

#[test]
fn archive_bundle_round_trips_native_records() {
    // The archive tier's lossless promise, demonstrated: the bundle contains
    // a manifest and the exact native records.
    let mut buf = Vec::new();
    FakeProvider
        .write_archive(&multi_artifact_key(), &mut buf)
        .unwrap();
    let bundle: serde_json::Value = serde_json::from_slice(&buf).unwrap();

    let manifest = &bundle["manifest"];
    assert_eq!(manifest["provider"], "fake");
    assert_eq!(manifest["session"], multi_artifact_key().to_string());
    assert_eq!(manifest["artifacts"].as_array().unwrap().len(), 2);

    let recovered = bundle["records"].as_array().unwrap();
    let original = native_records();
    assert_eq!(recovered.len(), original.len());
    for (r, o) in recovered.iter().zip(original.iter()) {
        assert_eq!(r, o, "archive round-trip altered a native record");
    }
}

#[test]
fn provenance_expresses_every_cardinality() {
    let parsed = FakeProvider.parse(&multi_artifact_key()).unwrap();
    assert!(
        parsed.validate_provenance().is_empty(),
        "fake session must be internally consistent: {:?}",
        parsed.validate_provenance()
    );
    // 1:N — record 0 produced two entries.
    let from_zero = parsed
        .record_dispositions
        .iter()
        .find(|d| d.record.ordinal == 0)
        .unwrap();
    assert!(matches!(&from_zero.outcome, RecordOutcome::Mapped(e) if e.len() == 2));
    // N:1 — one entry has two origin records.
    assert!(parsed.entry_origins.values().any(|o| o.len() == 2));
    // Unknown is preserved, not dropped: record 4 produced an entry.
    let from_four = parsed
        .record_dispositions
        .iter()
        .find(|d| d.record.ordinal == 4)
        .unwrap();
    assert!(matches!(&from_four.outcome, RecordOutcome::Unknown { entries } if entries.len() == 1));
    // Every record accounted for (invariant #1) and tallies agree.
    assert_eq!(parsed.record_dispositions.len(), 6);
    assert_eq!(
        parsed.diagnostics,
        IngestionDiagnostics {
            mapped: 4,
            suppressed: 1,
            unknown: 1,
            recovered: 0,
            unparseable: 0
        }
    );
}

#[test]
fn validator_catches_broken_provenance() {
    let good = FakeProvider.parse(&multi_artifact_key()).unwrap();

    // Origin pointing at a record with no producing disposition.
    let mut parsed = good.clone();
    let bogus_record = RecordRef {
        artifact: parsed.record_dispositions[0].record.artifact.clone(),
        ordinal: 999,
    };
    let phantom = EntryId::deterministic(&multi_artifact_key(), 999, 0);
    parsed.entries.push(IdentifiedEntry {
        id: phantom.clone(),
        entry: LogEntry::Unknown(serde_json::json!({})),
    });
    parsed.entry_origins.insert(phantom, vec![bogus_record]);
    assert!(parsed
        .validate_provenance()
        .iter()
        .any(|v| v.contains("no producing disposition")));

    // Duplicate disposition for the same record.
    let mut parsed = good.clone();
    let dup = parsed.record_dispositions[0].clone();
    parsed.record_dispositions.push(dup);
    assert!(parsed
        .validate_provenance()
        .iter()
        .any(|v| v.contains("more than one disposition")));

    // Entry present but with no origins at all.
    let mut parsed = good.clone();
    parsed.entry_origins.clear();
    assert!(parsed
        .validate_provenance()
        .iter()
        .any(|v| v.contains("has no origins")));

    // Duplicate entry ids.
    let mut parsed = good.clone();
    let first = parsed.entries[0].clone();
    parsed.entries.push(first);
    assert!(parsed
        .validate_provenance()
        .iter()
        .any(|v| v.contains("duplicate entry id")));

    // Disposition naming a nonexistent entry.
    let mut parsed = good.clone();
    parsed.entries.remove(0);
    assert!(parsed
        .validate_provenance()
        .iter()
        .any(|v| v.contains("does not exist")));

    // Empty Mapped list.
    let mut parsed = good.clone();
    parsed.record_dispositions[1].outcome = RecordOutcome::Mapped(vec![]);
    assert!(parsed
        .validate_provenance()
        .iter()
        .any(|v| v.contains("empty entry list")));

    // Diagnostics disagreeing with the disposition tallies.
    let mut parsed = good.clone();
    parsed.diagnostics.mapped = 99;
    assert!(parsed
        .validate_provenance()
        .iter()
        .any(|v| v.contains("do not match disposition tallies")));

    // Semantics naming a nonexistent entry.
    let mut parsed = good;
    parsed.semantics.insert(
        EntryId::deterministic(&multi_artifact_key(), 777, 0),
        EntrySemantics::default(),
    );
    assert!(parsed
        .validate_provenance()
        .iter()
        .any(|v| v.contains("semantics names entry")));
}

#[test]
fn semantic_tool_calls_must_reference_actual_calls() {
    let good = FakeProvider.parse(&multi_artifact_key()).unwrap();
    // The fake's call-7/call-8 semantics reference real ToolUse blocks.
    assert!(good.validate_provenance().is_empty());

    // A semantic keyed by a call id the entry does not contain is caught.
    let mut parsed = good;
    let (id, sem) = parsed
        .semantics
        .iter()
        .find(|(_, s)| !s.tools.is_empty())
        .map(|(id, s)| (id.clone(), s.clone()))
        .unwrap();
    let mut sem = sem;
    let tool = sem.tools.values().next().unwrap().clone();
    sem.tools.insert("call-999".into(), tool);
    parsed.semantics.insert(id, sem);
    assert!(parsed
        .validate_provenance()
        .iter()
        .any(|v| v.contains("call-999") && v.contains("does not contain")));
}

#[test]
fn validator_rejects_references_to_nonexistent_artifacts() {
    let good = FakeProvider.parse(&multi_artifact_key()).unwrap();
    let mut parsed = good;
    // Forge a disposition whose RecordRef names an artifact absent from the
    // descriptor.
    let forged = RecordRef {
        artifact: ArtifactId {
            provider_instance: "mem://forged".into(),
            locator: "not-a-real-artifact".into(),
        },
        ordinal: 999,
    };
    parsed.record_dispositions.push(RecordDisposition {
        record: forged,
        outcome: RecordOutcome::Suppressed {
            reason: SuppressionReason::Other("forged".into()),
        },
    });
    assert!(parsed
        .validate_provenance()
        .iter()
        .any(|v| v.contains("not in the descriptor")));
}

#[test]
fn entry_ids_are_deterministic_and_stable() {
    let p1 = FakeProvider.parse(&multi_artifact_key()).unwrap();
    let p2 = FakeProvider.parse(&multi_artifact_key()).unwrap();
    assert_eq!(p1.entry_origins, p2.entry_origins);
    let ids1: Vec<_> = p1.entries.iter().map(|e| e.id.clone()).collect();
    let ids2: Vec<_> = p2.entries.iter().map(|e| e.id.clone()).collect();
    assert_eq!(ids1, ids2);
}

#[test]
fn semantics_are_emitted_and_consumable() {
    let parsed = FakeProvider.parse(&multi_artifact_key()).unwrap();

    // Steered prompt: the two axes are independently visible.
    let steered = parsed
        .semantics
        .values()
        .find_map(|s| s.prompt)
        .expect("fake emits a prompt semantic");
    assert_eq!(steered.authorship, PromptAuthorship::Human);
    assert_eq!(steered.delivery, PromptDelivery::MidTurn);

    // Tool semantics are per-call: one entry carries two calls with
    // different classifications, each pairing kind with its native name.
    let tools = parsed
        .semantics
        .values()
        .find(|s| !s.tools.is_empty())
        .map(|s| &s.tools)
        .expect("fake emits tool semantics");
    assert_eq!(tools.len(), 2);
    assert_eq!(tools["call-7"].kind, ToolKind::FileWrite);
    assert_eq!(tools["call-7"].native_name, "fake_apply_patch");
    assert_eq!(tools["call-8"].kind, ToolKind::Shell);
    assert_eq!(tools["call-8"].native_name, "fake_exec");

    // Dual usage observation (Codex token_count shape): each annotation is
    // paired with its own values, so the last-call and cumulative numbers
    // are distinguishable.
    let usage: Vec<_> = parsed
        .semantics
        .values()
        .flat_map(|s| s.usage.iter())
        .collect();
    let last = usage
        .iter()
        .find(|o| o.scope == UsageScope::Call && o.aggregation == UsageAggregation::Delta)
        .expect("last-call observation");
    assert_eq!(last.usage.input_tokens, 10);
    let total = usage
        .iter()
        .find(|o| o.scope == UsageScope::Session && o.aggregation == UsageAggregation::Cumulative)
        .expect("cumulative observation");
    assert_eq!(total.usage.input_tokens, 200);
}

#[test]
fn lineage_edges_are_typed_with_known_endpoints() {
    let edges = FakeProvider.lineage().unwrap();
    assert_eq!(edges.len(), 1);
    let edge = &edges[0];
    assert_eq!(edge.kind, LineageEdgeKind::Fork);
    let sessions: Vec<_> = FakeProvider
        .sessions()
        .unwrap()
        .into_iter()
        .map(|d| d.key)
        .collect();
    assert!(sessions.contains(&edge.from));
    assert!(sessions.contains(&edge.to));
}

#[test]
fn validator_catches_foreign_and_duplicate_edges() {
    let good = FakeProvider.parse(&multi_artifact_key()).unwrap();

    // An entry belonging to a different logical session.
    let mut parsed = good.clone();
    let foreign = EntryId::deterministic(&colliding_key(), 0, 0);
    parsed.entries.push(IdentifiedEntry {
        id: foreign.clone(),
        entry: LogEntry::Unknown(serde_json::json!({})),
    });
    parsed
        .entry_origins
        .insert(foreign, vec![parsed.record_dispositions[0].record.clone()]);
    assert!(parsed
        .validate_provenance()
        .iter()
        .any(|v| v.contains("not this session")));

    // A Mapped list naming the same entry twice.
    let mut parsed = good.clone();
    if let RecordOutcome::Mapped(entries) = &mut parsed.record_dispositions[1].outcome {
        let first = entries[0].clone();
        entries.push(first);
    }
    assert!(parsed
        .validate_provenance()
        .iter()
        .any(|v| v.contains("more than once")));

    // An origin list naming the same record twice.
    let mut parsed = good;
    let (id, origins) = parsed.entry_origins.iter_mut().next().unwrap();
    let _ = id;
    let first = origins[0].clone();
    origins.push(first);
    assert!(parsed
        .validate_provenance()
        .iter()
        .any(|v| v.contains("lists origin record") && v.contains("more than once")));
}

#[test]
fn inherited_history_is_present_but_not_new_work() {
    let parsed = FakeProvider.parse(&multi_artifact_key()).unwrap();
    let default_semantics = EntrySemantics::default();
    let activity = |id: &EntryId| {
        parsed
            .semantics
            .get(id)
            .unwrap_or(&default_semantics)
            .activity
    };

    // The inherited record is Mapped — present when viewing this session.
    let inherited = parsed
        .entries
        .iter()
        .filter(|e| activity(&e.id) == ActivityKind::InheritedHistory)
        .count();
    assert_eq!(inherited, 1, "fork-inherited entry must be present");

    // A "new work" projection (what cross-session analytics computes)
    // excludes it without losing it.
    let new_work = parsed
        .entries
        .iter()
        .filter(|e| activity(&e.id) == ActivityKind::New)
        .count();
    assert_eq!(new_work, parsed.entries.len() - 1);
}

// ============================================================================
// Qualified-id round-trip (B2 round-6 guardrail: parsing must be proven
// before ids become CLI/MCP inputs)
// ============================================================================

fn key(provider: &str, namespace: &str, native: &str) -> LogicalSessionKey {
    LogicalSessionKey {
        provider: ProviderId(provider.into()),
        namespace: SessionNamespace(namespace.into()),
        native_id: native.into(),
    }
}

#[test]
fn qualified_id_round_trips_simple() {
    let k = key("codex", "global", "0198c5c1-aaaa-7bbb-8ccc-0123456789ab");
    let shown = k.to_string();
    assert_eq!(shown, "codex:0198c5c1-aaaa-7bbb-8ccc-0123456789ab");
    assert_eq!(shown.parse::<LogicalSessionKey>().unwrap(), k);
}

#[test]
fn qualified_id_round_trips_hostile_segments() {
    // Colons, percents, pre-escaped-looking text, and non-ASCII in every
    // segment position must survive Display -> FromStr unchanged.
    let hostile = [
        key("claude-code", "subagent:parent:dir", "abc"),
        key("fake", "install-a", "b:c"),
        key("p%ro", "n%3As", "50%:done"),
        key("codex", "global", "%25 already escaped-looking"),
        key("codex", "глобал", "идентификатор:例"),
    ];
    for k in hostile {
        let shown = k.to_string();
        assert_eq!(
            shown.parse::<LogicalSessionKey>().unwrap(),
            k,
            "round-trip failed for display form '{shown}'"
        );
    }
}

#[test]
fn qualified_id_explicit_global_namespace_canonicalizes() {
    // "codex:global:abc" is accepted, parses to the same key as "codex:abc",
    // and re-displays in the canonical two-segment form.
    let explicit: LogicalSessionKey = "codex:global:abc".parse().unwrap();
    let implicit: LogicalSessionKey = "codex:abc".parse().unwrap();
    assert_eq!(explicit, implicit);
    assert_eq!(explicit.to_string(), "codex:abc");
}

#[test]
fn qualified_id_injectivity_pair_parses_distinctly() {
    // The doc-comment pair: namespace "a" + native "b:c" vs namespace "a:b" +
    // native "c" must have distinct display forms, each parsing to itself.
    let k1 = key("p", "a", "b:c");
    let k2 = key("p", "a:b", "c");
    assert_ne!(k1.to_string(), k2.to_string());
    assert_eq!(k1.to_string().parse::<LogicalSessionKey>().unwrap(), k1);
    assert_eq!(k2.to_string().parse::<LogicalSessionKey>().unwrap(), k2);
}

#[test]
fn qualified_id_rejects_malformed_inputs() {
    let bad = [
        "",              // no segments
        "codex",         // one segment (unqualified — caller's job)
        "a:b:c:d",       // four segments
        ":abc",          // empty provider
        "codex:",        // empty native id
        "codex::abc",    // empty namespace
        "codex:ab%",     // truncated escape
        "codex:ab%2",    // truncated escape
        "codex:ab%zz",   // invalid escape
        "codex:ab%3a",   // lowercase escape rejected (strict)
        "codex:ab%20cd", // valid percent-encoding, but not ours
    ];
    for input in bad {
        assert!(
            input.parse::<LogicalSessionKey>().is_err(),
            "'{input}' should have been rejected"
        );
    }
}

// ============================================================================
// Provider registry (B2: shared resolver seam, round-17 guardrails)
// ============================================================================

use super::registry::{ProviderRegistry, RegisteredProvider};

fn fake_entry() -> RegisteredProvider {
    RegisteredProvider {
        id: FakeProvider.id(),
        root: None,
        provider: Ok(Box::new(FakeProvider)),
    }
}

#[test]
fn registry_orders_entries_by_provider_id_regardless_of_registration_order() {
    let mut r = ProviderRegistry::new();
    r.register(RegisteredProvider {
        id: ProviderId("zzz".into()),
        root: None,
        provider: Err("not built".into()),
    })
    .unwrap();
    r.register(fake_entry()).unwrap();
    r.register(RegisteredProvider {
        id: ProviderId("aaa".into()),
        root: None,
        provider: Err("not built".into()),
    })
    .unwrap();
    let ids: Vec<String> = r.entries().iter().map(|e| e.id.to_string()).collect();
    assert_eq!(ids, ["aaa", "fake", "zzz"]);
    // available() preserves the same deterministic order.
    let avail: Vec<String> = r.available().map(|p| p.id().to_string()).collect();
    assert_eq!(avail, ["fake"]);
}

#[test]
fn registry_rejects_duplicate_provider_ids() {
    let mut r = ProviderRegistry::new();
    r.register(fake_entry()).unwrap();
    assert!(
        r.register(fake_entry()).is_err(),
        "second registration of the same id must fail"
    );
    assert_eq!(r.entries().len(), 1);
}

#[test]
fn registry_get_never_falls_back_to_another_provider() {
    let mut r = ProviderRegistry::new();
    r.register(fake_entry()).unwrap();
    r.register(RegisteredProvider {
        id: ProviderId("broken".into()),
        root: None,
        provider: Err("home not found".into()),
    })
    .unwrap();

    // Unknown id: error naming the known set — not some other provider.
    let unknown = r.get(&ProviderId("nope".into()));
    let msg = unknown.err().expect("unknown id must error").to_string();
    assert!(msg.contains("nope") && msg.contains("fake"), "got: {msg}");

    // Installed-but-unavailable id: error carrying the reason — again no
    // fallback.
    let broken = r.get(&ProviderId("broken".into()));
    let msg = broken.err().expect("unavailable must error").to_string();
    assert!(
        msg.contains("broken") && msg.contains("home not found"),
        "got: {msg}"
    );

    // The working provider is still reachable by its own id.
    assert_eq!(r.get(&FakeProvider.id()).unwrap().id(), FakeProvider.id());
}

// ============================================================================
// Provider selection + session resolution matrix (B2, round-17 guardrails)
// ============================================================================

use super::registry::{ProviderSelection, Selected};

/// Minimal second provider so cross-provider resolution is testable:
/// id "small", global namespace, native ids "421", "5", "56".
struct SmallProvider;

impl SmallProvider {
    fn key(native: &str) -> LogicalSessionKey {
        LogicalSessionKey {
            provider: ProviderId("small".into()),
            namespace: SessionNamespace::global(),
            native_id: native.into(),
        }
    }
}

impl SourceProvider for SmallProvider {
    fn id(&self) -> ProviderId {
        ProviderId("small".into())
    }
    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities::default()
    }
    fn sessions(&self) -> Result<Vec<SessionDescriptor>, ProviderError> {
        Ok(["421", "5", "56", "we:ird"]
            .iter()
            .map(|n| SessionDescriptor {
                key: Self::key(n),
                artifacts: vec![],
            })
            .collect())
    }
    fn lineage(&self) -> Result<Vec<LineageEdge>, ProviderError> {
        Ok(vec![])
    }
    fn parse(&self, key: &LogicalSessionKey) -> Result<ParsedSession, ProviderError> {
        Err(ProviderError::NotFound(key.to_string()))
    }
    fn parse_cache_token(&self, key: &LogicalSessionKey) -> Result<String, ProviderError> {
        Err(ProviderError::NotFound(key.to_string()))
    }
    fn write_archive(
        &self,
        key: &LogicalSessionKey,
        _out: &mut dyn std::io::Write,
    ) -> Result<(), ProviderError> {
        Err(ProviderError::NotFound(key.to_string()))
    }
    fn write_native(
        &self,
        _artifact: &ArtifactId,
        _out: &mut dyn std::io::Write,
    ) -> Result<(), ProviderError> {
        Err(ProviderError::Unsupported {
            capability: "native export",
        })
    }
    fn write_raw_jsonl(
        &self,
        _key: &LogicalSessionKey,
        _out: &mut dyn std::io::Write,
    ) -> Result<(), ProviderError> {
        Err(ProviderError::Unsupported {
            capability: "raw-jsonl export",
        })
    }
}

fn matrix_registry() -> ProviderRegistry {
    let mut r = ProviderRegistry::new();
    r.register(fake_entry()).unwrap();
    r.register(RegisteredProvider {
        id: ProviderId("small".into()),
        root: None,
        provider: Ok(Box::new(SmallProvider)),
    })
    .unwrap();
    r.register(RegisteredProvider {
        id: ProviderId("broken".into()),
        root: None,
        provider: Err("home not found".into()),
    })
    .unwrap();
    r
}

fn flags(v: &[&str]) -> Vec<String> {
    v.iter().map(|s| s.to_string()).collect()
}

#[test]
fn selection_flags_dedupe_and_reject_all_mixed_with_explicit() {
    assert_eq!(
        ProviderSelection::from_flags(&flags(&["small", "small"])).unwrap(),
        ProviderSelection::Explicit(vec![ProviderId("small".into())])
    );
    assert_eq!(
        ProviderSelection::from_flags(&flags(&["all"])).unwrap(),
        ProviderSelection::All
    );
    let err = ProviderSelection::from_flags(&flags(&["all", "small"])).unwrap_err();
    assert!(err.contains("cannot be combined"), "got: {err}");
}

#[test]
fn explicit_selection_is_atomic_over_unavailable_and_unknown() {
    let r = matrix_registry();
    // A working provider named alongside a broken one does not soften the
    // failure.
    let err = r
        .select(&ProviderSelection::from_flags(&flags(&["small", "broken"])).unwrap())
        .err()
        .expect("atomic failure")
        .to_string();
    assert!(
        err.contains("broken") && err.contains("home not found"),
        "got: {err}"
    );

    let err = r
        .select(&ProviderSelection::from_flags(&flags(&["nope"])).unwrap())
        .err()
        .expect("unknown provider")
        .to_string();
    assert!(err.contains("nope") && err.contains("small"), "got: {err}");
}

#[test]
fn all_selection_is_partial_but_never_silent() {
    let r = matrix_registry();
    let Selected { providers, skipped } = r.select(&ProviderSelection::All).unwrap();
    let ids: Vec<String> = providers.iter().map(|p| p.id().to_string()).collect();
    assert_eq!(ids, ["fake", "small"]);
    assert_eq!(skipped.len(), 1);
    assert_eq!(skipped[0].0.to_string(), "broken");
    assert!(skipped[0].1.contains("home not found"));
}

#[test]
fn all_selection_with_nothing_working_errors() {
    let mut r = ProviderRegistry::new();
    r.register(RegisteredProvider {
        id: ProviderId("broken".into()),
        root: None,
        provider: Err("gone".into()),
    })
    .unwrap();
    let err = r.select(&ProviderSelection::All).err().unwrap().to_string();
    assert!(err.contains("no providers available"), "got: {err}");
}

#[test]
fn qualified_id_outside_selection_is_refused_not_widened() {
    let r = matrix_registry();
    let sel = ProviderSelection::from_flags(&flags(&["small"])).unwrap();
    let err = r
        .resolve_session(&sel, "fake:install-a:42")
        .err()
        .expect("refusal")
        .to_string();
    assert!(
        err.contains("outside the current provider selection"),
        "got: {err}"
    );

    // Same reference resolves fine once the selection includes its provider.
    let sel = ProviderSelection::from_flags(&flags(&["fake"])).unwrap();
    let res = r.resolve_session(&sel, "fake:install-a:42").unwrap();
    assert_eq!(res.key, multi_artifact_key());
    assert_eq!(res.provider.id().to_string(), "fake");
}

#[test]
fn qualified_id_naming_unavailable_or_unknown_provider_is_precise() {
    let r = matrix_registry();
    let err = r
        .resolve_session(&ProviderSelection::All, "broken:xyz")
        .err()
        .unwrap()
        .to_string();
    assert!(
        err.contains("unavailable") && err.contains("home not found"),
        "got: {err}"
    );

    // A colon-bearing reference whose first segment names NO registered
    // provider is NOT a qualified id (unified predicate, round-18): it is
    // searched as a plain prefix and misses, with an explanatory hint.
    let err = r
        .resolve_session(&ProviderSelection::All, "ghost:xyz")
        .err()
        .unwrap()
        .to_string();
    assert!(
        err.contains("no session matching") && err.contains("registered provider"),
        "got: {err}"
    );
}

#[test]
fn native_ids_with_encoded_colons_resolve_via_qualified_form() {
    let r = matrix_registry();
    // small owns native id "we:ird"; its qualified form escapes the colon.
    let shown = SmallProvider::key("we:ird").to_string();
    assert_eq!(shown, "small:we%3Aird");
    let res = r.resolve_session(&ProviderSelection::All, &shown).unwrap();
    assert_eq!(res.key, SmallProvider::key("we:ird"));

    // The RAW (unescaped) form is not a valid qualified id for it: it parses
    // as native id "ird" under namespace "we" and misses precisely.
    assert!(r
        .resolve_session(&ProviderSelection::All, "small:we:ird")
        .is_err());
}

#[test]
fn ambiguity_candidates_are_sorted_before_truncation() {
    // Register a provider with many sessions sharing a prefix; the error
    // must list the lexicographically FIRST five, not an arbitrary five.
    struct ManyProvider;
    impl SourceProvider for ManyProvider {
        fn id(&self) -> ProviderId {
            ProviderId("many".into())
        }
        fn capabilities(&self) -> ProviderCapabilities {
            ProviderCapabilities::default()
        }
        fn sessions(&self) -> Result<Vec<SessionDescriptor>, ProviderError> {
            // Emitted in DESCENDING order so unsorted truncation would show
            // p9..p5.
            Ok((0..10)
                .rev()
                .map(|i| SessionDescriptor {
                    key: LogicalSessionKey {
                        provider: ProviderId("many".into()),
                        namespace: SessionNamespace::global(),
                        native_id: format!("p{i}"),
                    },
                    artifacts: vec![],
                })
                .collect())
        }
        fn lineage(&self) -> Result<Vec<LineageEdge>, ProviderError> {
            Ok(vec![])
        }
        fn parse(&self, key: &LogicalSessionKey) -> Result<ParsedSession, ProviderError> {
            Err(ProviderError::NotFound(key.to_string()))
        }
        fn parse_cache_token(&self, key: &LogicalSessionKey) -> Result<String, ProviderError> {
            Err(ProviderError::NotFound(key.to_string()))
        }
        fn write_archive(
            &self,
            key: &LogicalSessionKey,
            _out: &mut dyn std::io::Write,
        ) -> Result<(), ProviderError> {
            Err(ProviderError::NotFound(key.to_string()))
        }
        fn write_native(
            &self,
            _artifact: &ArtifactId,
            _out: &mut dyn std::io::Write,
        ) -> Result<(), ProviderError> {
            Err(ProviderError::Unsupported {
                capability: "native export",
            })
        }
        fn write_raw_jsonl(
            &self,
            _key: &LogicalSessionKey,
            _out: &mut dyn std::io::Write,
        ) -> Result<(), ProviderError> {
            Err(ProviderError::Unsupported {
                capability: "raw-jsonl export",
            })
        }
    }
    let mut r = ProviderRegistry::new();
    r.register(RegisteredProvider {
        id: ProviderId("many".into()),
        root: None,
        provider: Ok(Box::new(ManyProvider)),
    })
    .unwrap();
    let err = r
        .resolve_session(&ProviderSelection::All, "p")
        .err()
        .expect("ambiguous")
        .to_string();
    for shown in ["many:p0", "many:p1", "many:p2", "many:p3", "many:p4"] {
        assert!(err.contains(shown), "expected {shown} in: {err}");
    }
    assert!(
        !err.contains("many:p9"),
        "unsorted truncation leaked: {err}"
    );
}

#[test]
fn unqualified_ambiguity_errors_with_qualified_candidates() {
    let r = matrix_registry();
    // "42" matches fake:install-a:42, fake:install-b:42 (both exact) and
    // small:421 — two exact matches cannot break the tie.
    let err = r
        .resolve_session(&ProviderSelection::All, "42")
        .err()
        .expect("ambiguous")
        .to_string();
    assert!(
        err.contains("ambiguous")
            && err.contains("install-a")
            && err.contains("install-b")
            && err.contains("small:421"),
        "got: {err}"
    );
}

#[test]
fn unqualified_unique_prefix_resolves_across_providers() {
    let r = matrix_registry();
    let res = r.resolve_session(&ProviderSelection::All, "421").unwrap();
    assert_eq!(res.key, SmallProvider::key("421"));
    assert_eq!(res.provider.id().to_string(), "small");
}

#[test]
fn one_exact_match_beats_longer_prefix_matches() {
    let r = matrix_registry();
    // "5" prefixes both small:5 and small:56, but is an exact id for one.
    let res = r.resolve_session(&ProviderSelection::All, "5").unwrap();
    assert_eq!(res.key, SmallProvider::key("5"));
}

#[test]
fn not_found_under_all_names_unsearched_providers() {
    let r = matrix_registry();
    let err = r
        .resolve_session(&ProviderSelection::All, "zzz")
        .err()
        .expect("not found")
        .to_string();
    assert!(
        err.contains("no session matching") && err.contains("broken"),
        "skipped providers must be named so 'not found' is honest: {err}"
    );
}

#[test]
fn qualified_reference_supports_native_prefix_within_its_provider() {
    let r = matrix_registry();
    let res = r
        .resolve_session(&ProviderSelection::All, "small:4")
        .unwrap();
    assert_eq!(res.key, SmallProvider::key("421"));
}

// ============================================================================
// Conversation bridge (B2: centralized from_parsed_session)
// ============================================================================

#[test]
fn from_parsed_session_threads_source_identity() {
    let parsed = std::sync::Arc::new(FakeProvider.parse(&multi_artifact_key()).unwrap());
    let expected = parsed.descriptor.key.clone();
    let conversation = crate::reconstruction::Conversation::from_parsed_session(parsed).unwrap();
    assert_eq!(conversation.source(), Some(&expected));
}

#[test]
fn from_parsed_session_retains_bundle_and_correlates_semantics() {
    // Round-18 survival rule: the bundle (ids, provenance, dispositions,
    // semantics, diagnostics) survives reconstruction, and node uuids
    // correlate back to deterministic entry ids.
    let parsed = std::sync::Arc::new(FakeProvider.parse(&multi_artifact_key()).unwrap());
    assert!(!parsed.semantics.is_empty(), "fixture must carry semantics");
    let conversation =
        crate::reconstruction::Conversation::from_parsed_session(parsed.clone()).unwrap();

    let bundle = conversation.provider_bundle().expect("bundle retained");
    assert!(std::sync::Arc::ptr_eq(bundle, &parsed));
    assert_eq!(
        bundle.record_dispositions.len(),
        parsed.record_dispositions.len()
    );
    assert_eq!(bundle.diagnostics, parsed.diagnostics);

    // At least one semantics-bearing entry with a uuid must be reachable
    // through the conversation-side lookup, and the lookup must agree with
    // the bundle's own map.
    let mut correlated = 0;
    for (id, expected_semantics) in &parsed.semantics {
        let entry = parsed
            .entries
            .iter()
            .find(|e| e.id == *id)
            .expect("semantics key names an entry");
        if let Some(uuid) = entry.entry.uuid() {
            assert_eq!(conversation.entry_id_for_uuid(uuid), Some(id));
            let via_conversation = conversation
                .semantics_for_uuid(uuid)
                .expect("semantics reachable via conversation");
            assert_eq!(
                format!("{via_conversation:?}"),
                format!("{expected_semantics:?}")
            );
            correlated += 1;
        }
    }
    assert!(correlated > 0, "no uuid-bearing semantic entry correlated");
}

#[test]
fn cached_parsed_session_preserves_bundle_across_miss_and_hit() {
    // Round-18: provenance and semantics must survive BOTH cache paths.
    use super::registry::cached_parsed_session;
    let cache = crate::cache::CacheManager::new(&crate::config::CacheConfig {
        enabled: true,
        ..Default::default()
    });
    let key = multi_artifact_key();

    let miss = cached_parsed_session(&cache, &FakeProvider, &key).unwrap();
    assert!(!miss.semantics.is_empty());
    assert!(!miss.record_dispositions.is_empty());
    assert!(!miss.entry_origins.is_empty());

    let hit = cached_parsed_session(&cache, &FakeProvider, &key).unwrap();
    assert!(
        std::sync::Arc::ptr_eq(&miss, &hit),
        "unchanged token must be a cache hit"
    );
    assert!(!hit.semantics.is_empty() && !hit.record_dispositions.is_empty());

    // The hit still builds a conversation with reachable semantics.
    let conversation = crate::reconstruction::Conversation::from_parsed_session(hit).unwrap();
    assert!(conversation.provider_bundle().is_some());
}
