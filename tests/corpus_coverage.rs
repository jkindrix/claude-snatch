//! Behavioral coverage tests over the golden corpus fixtures.
//!
//! Unlike the snapshot tests, these assert that specific Claude Code log shapes
//! parse into the expected model variants (not the `Unknown` fallback) and that
//! key fields survive. See `docs/test-corpus.md` for the corpus strategy and the
//! per-fixture provenance in `tests/fixtures/PROVENANCE.md`.

use std::path::PathBuf;

use claude_snatch::model::{CompactTrigger, LogEntry, SystemSubtype};
use claude_snatch::parser::JsonlParser;

fn parse_fixture(name: &str) -> Vec<LogEntry> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join(name);
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
