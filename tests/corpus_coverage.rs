//! Behavioral coverage tests over the golden corpus fixtures.
//!
//! Unlike the snapshot tests, these assert that specific Claude Code log shapes
//! parse into the expected model variants (not the `Unknown` fallback) and that
//! key fields survive. See `docs/test-corpus.md` for the corpus strategy and the
//! per-fixture provenance in `tests/fixtures/PROVENANCE.md`.

use std::path::PathBuf;

use claude_snatch::discovery::Session;
use claude_snatch::model::{CompactTrigger, LogEntry, SystemSubtype};
use claude_snatch::parser::JsonlParser;

fn fixture_path(rel: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join(rel)
}

fn parse_fixture(name: &str) -> Vec<LogEntry> {
    let path = fixture_path(name);
    let content = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("Failed to read fixture {name}: {e}"));
    JsonlParser::new()
        .parse_str(&content)
        .unwrap_or_else(|e| panic!("Failed to parse fixture {name}: {e}"))
}

#[test]
fn compaction_session_shapes_parse() {
    let entries = parse_fixture("compaction_session.jsonl");
    assert_eq!(
        entries.len(),
        6,
        "expected 6 entries in the compaction fixture"
    );

    // The compact_boundary system entry parses as System (not Unknown) and
    // preserves compactMetadata + the cross-boundary logicalParentUuid link.
    let boundary = entries
        .iter()
        .find_map(|e| match e {
            LogEntry::System(s) if s.subtype == Some(SystemSubtype::CompactBoundary) => Some(s),
            _ => None,
        })
        .expect("compact_boundary system entry should parse as System");

    let meta = boundary
        .compact_metadata
        .as_ref()
        .expect("compactMetadata should be parsed");
    assert_eq!(meta.trigger, CompactTrigger::Manual);
    assert_eq!(meta.pre_tokens, Some(145_450));
    assert!(
        boundary.logical_parent_uuid.is_some(),
        "logicalParentUuid should be preserved across the boundary"
    );

    // The injected compaction summary is a user entry flagged isCompactSummary.
    let has_compact_summary = entries
        .iter()
        .any(|e| matches!(e, LogEntry::User(u) if u.is_compact_summary == Some(true)));
    assert!(
        has_compact_summary,
        "an isCompactSummary user entry should be present"
    );
}

#[test]
fn subagent_session_links_resolve() {
    // The parent session discovers its subagents via the on-disk layout
    // <uuid>/subagents/agent-*.jsonl + agent-*.meta.json sidecars.
    let parent = fixture_path("subagent_session/c0ffee00-1111-2222-3333-444455556666.jsonl");
    let session = Session::from_path(&parent, "test-project").expect("parent session should load");

    let mut links = session.subagent_links();
    assert_eq!(
        links.len(),
        3,
        "all three on-disk subagents should be discovered, not undercounted"
    );
    links.sort_by(|a, b| a.agent_session_id.cmp(&b.agent_session_id));

    // Sidecar metadata resolves: agentType, description, and (newer versions) toolUseId.
    let first = &links[0];
    assert_eq!(first.agent_type.as_deref(), Some("Explore"));
    assert_eq!(first.description.as_deref(), Some("inspect public API"));
    assert_eq!(first.tool_use_id.as_deref(), Some("toolu_001"));

    // A partial sidecar (no toolUseId, as older Claude Code versions wrote) must
    // still surface the subagent rather than hiding it.
    let third = &links[2];
    assert_eq!(third.agent_type.as_deref(), Some("Plan"));
    assert_eq!(third.tool_use_id, None);

    // Every link points at a transcript that exists and parses.
    for link in &links {
        assert!(link.path.exists(), "subagent transcript should exist");
        let content = std::fs::read_to_string(&link.path).expect("read subagent transcript");
        let entries = JsonlParser::new()
            .parse_str(&content)
            .expect("subagent transcript should parse");
        assert!(
            !entries.is_empty(),
            "subagent transcript should have entries"
        );
    }
}
