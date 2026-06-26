//! Behavioral coverage tests over the golden corpus fixtures.
//!
//! Unlike the snapshot tests, these assert that specific Claude Code log shapes
//! parse into the expected model variants (not the `Unknown` fallback) and that
//! key fields survive. See `docs/test-corpus.md` for the corpus strategy and the
//! per-fixture provenance in `tests/fixtures/PROVENANCE.md`.

use std::collections::HashSet;
use std::io::Cursor;
use std::path::PathBuf;

use claude_snatch::discovery::Session;
use claude_snatch::export::{
    export_to_string, ContentType, ExportFormat, ExportOptions, Exporter, MarkdownExporter,
};
use claude_snatch::model::{
    CompactTrigger, ContentBlock, ImageSource, LogEntry, StopReason, SystemSubtype, ToolResult,
    ToolUse, UserContent,
};
use claude_snatch::parser::JsonlParser;
use claude_snatch::reconstruction::Conversation;

/// Render entries to a markdown string with the given options.
fn markdown_with(entries: &[LogEntry], opts: &ExportOptions) -> String {
    let mut buf = Cursor::new(Vec::new());
    MarkdownExporter::new()
        .export_entries(entries, &mut buf, opts)
        .expect("markdown export should succeed");
    String::from_utf8(buf.into_inner()).expect("export output should be valid UTF-8")
}

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

#[test]
fn rich_entries_route_to_their_variants() {
    let entries = parse_fixture("rich_entries_session.jsonl");
    assert_eq!(entries.len(), 9, "expected 9 rare-type entries");

    // None of the rare types fell back to the Unknown catch-all.
    assert!(
        !entries.iter().any(|e| matches!(e, LogEntry::Unknown(_))),
        "every modeled entry type should route to its own variant, not Unknown"
    );

    assert!(entries
        .iter()
        .any(|e| matches!(e, LogEntry::FileHistorySnapshot(_))));
    assert!(entries
        .iter()
        .any(|e| matches!(e, LogEntry::QueueOperation(_))));
    assert!(entries.iter().any(|e| matches!(e, LogEntry::Attachment(_))));
    assert!(entries.iter().any(|e| matches!(e, LogEntry::Progress(_))));
    assert!(entries.iter().any(|e| matches!(e, LogEntry::LastPrompt(_))));
    assert!(entries.iter().any(|e| matches!(e, LogEntry::Mode(_))));
    assert!(entries
        .iter()
        .any(|e| matches!(e, LogEntry::PermissionMode(_))));
    assert!(entries.iter().any(|e| matches!(e, LogEntry::AiTitle(_))));

    // The user entry carries its todo list.
    let todos_ok = entries
        .iter()
        .any(|e| matches!(e, LogEntry::User(u) if u.todos.len() == 2));
    assert!(todos_ok, "the user entry should carry two todos");
}

/// Collect every content block across user (Blocks) and assistant messages.
fn all_content_blocks(entries: &[LogEntry]) -> Vec<&ContentBlock> {
    let mut blocks = Vec::new();
    for e in entries {
        match e {
            LogEntry::User(u) => {
                if let UserContent::Blocks(b) = &u.message {
                    blocks.extend(b.content.iter());
                }
            }
            LogEntry::Assistant(a) => blocks.extend(a.message.content.iter()),
            _ => {}
        }
    }
    blocks
}

#[test]
fn content_blocks_images_tools_and_results_parse() {
    let entries = parse_fixture("content_blocks_session.jsonl");
    let blocks = all_content_blocks(&entries);

    // Known block types must not fall back to the Unknown catch-all.
    assert!(
        !blocks
            .iter()
            .any(|b| matches!(b, ContentBlock::Unknown { .. })),
        "image/tool_use/tool_result blocks should parse as their own variants"
    );

    // All three ImageSource variants round-trip.
    let sources: Vec<&ImageSource> = blocks
        .iter()
        .filter_map(|b| match b {
            ContentBlock::Image(img) => Some(&img.source),
            _ => None,
        })
        .collect();
    assert_eq!(sources.len(), 3, "three image blocks");
    assert!(sources
        .iter()
        .any(|s| matches!(s, ImageSource::Base64 { .. })));
    assert!(sources.iter().any(|s| matches!(s, ImageSource::Url { .. })));
    assert!(sources
        .iter()
        .any(|s| matches!(s, ImageSource::File { .. })));

    // MCP and server tool-use are distinguished.
    let tools: Vec<&ToolUse> = blocks
        .iter()
        .filter_map(|b| match b {
            ContentBlock::ToolUse(t) => Some(t),
            _ => None,
        })
        .collect();
    assert!(tools.iter().any(|t| t.is_mcp_tool()), "mcp__ tool detected");
    assert!(
        tools.iter().any(|t| t.is_server_tool()),
        "srvtoolu_ server tool detected"
    );

    // Tool-result three-state error flag (true / false / absent) is preserved.
    let results: Vec<&ToolResult> = blocks
        .iter()
        .filter_map(|b| match b {
            ContentBlock::ToolResult(r) => Some(r),
            _ => None,
        })
        .collect();
    assert!(results.iter().any(|r| r.is_error == Some(true)));
    assert!(results.iter().any(|r| r.is_error == Some(false)));
    assert!(results.iter().any(|r| r.is_error.is_none()));
}

#[test]
fn forward_compat_preserves_unknown_shapes() {
    let entries = parse_fixture("forward_compat_session.jsonl");
    assert_eq!(entries.len(), 4);

    // A future top-level entry type is preserved as Unknown, not dropped.
    assert!(
        entries.iter().any(|e| matches!(e, LogEntry::Unknown(_))),
        "unmodeled entry type should round-trip as Unknown"
    );

    // Future content-block types become Unknown blocks; a future stop_reason
    // becomes StopReason::Other (preserving the original string).
    let asst = entries
        .iter()
        .find_map(|e| match e {
            LogEntry::Assistant(a) => Some(a),
            _ => None,
        })
        .expect("assistant entry");
    let unknown_blocks = asst
        .message
        .content
        .iter()
        .filter(|b| matches!(b, ContentBlock::Unknown { .. }))
        .count();
    assert_eq!(
        unknown_blocks, 2,
        "redacted_thinking + some_future_block should be Unknown blocks"
    );
    // NOTE: a future stop_reason SHOULD become StopReason::Other, but is
    // currently dropped to None (see `assistant_stop_reason_parses` below and
    // .tmp/issues/0015) — so that assertion lives in an ignored test until fixed.

    // Future system subtype and compact trigger fall back to their Other arms.
    assert!(
        entries.iter().any(|e| matches!(
            e,
            LogEntry::System(s) if matches!(s.subtype, Some(SystemSubtype::Other(_)))
        )),
        "future system subtype should be SystemSubtype::Other"
    );
    assert!(
        entries.iter().any(|e| matches!(
            e,
            LogEntry::System(s) if s
                .compact_metadata
                .as_ref()
                .is_some_and(|m| matches!(m.trigger, CompactTrigger::Other(_)))
        )),
        "future compact trigger should be CompactTrigger::Other"
    );
}

#[test]
fn malformed_lines_skipped_with_diagnostics() {
    // Lenient mode (the default) skips malformed lines and retains a diagnostic
    // copy of each, rather than failing the whole parse.
    let content = std::fs::read_to_string(fixture_path("malformed_session.jsonl"))
        .expect("read malformed fixture");
    let mut parser = JsonlParser::new();
    let entries = parser
        .parse_str(&content)
        .expect("lenient parse should recover, not fail");

    // The three well-formed entries survive (incl. the duplicate-UUID pair,
    // which is preserved at parse time and only reconciled in reconstruction).
    assert_eq!(
        entries.len(),
        3,
        "valid entries should survive malformed lines"
    );

    let stats = parser.stats();
    assert_eq!(
        stats.lines_skipped, 2,
        "two malformed lines should be skipped"
    );
    assert!(
        stats
            .errors
            .iter()
            .any(|e| e.raw_line.contains("truncated line")),
        "the full malformed line should be retained for diagnostics"
    );
}

/// Regression guard for a real fidelity bug found while building this corpus:
/// `AssistantContent` is `#[serde(rename_all = "camelCase")]`, so it expects a
/// `stopReason` key, but every real Claude Code session writes snake_case
/// `stop_reason`. The field has no alias, so `stop_reason` (and `stop_sequence`)
/// parse to `None` on all real assistant messages. Ignored until the parser is
/// fixed; see `.tmp/issues/0015`. Flip to active to verify the fix.
#[test]
#[ignore = "blocked by issue 0015: stop_reason camelCase mismatch drops the field"]
fn assistant_stop_reason_parses() {
    let entries = parse_fixture("forward_compat_session.jsonl");
    let asst = entries
        .iter()
        .find_map(|e| match e {
            LogEntry::Assistant(a) => Some(a),
            _ => None,
        })
        .expect("assistant entry");
    assert!(
        matches!(asst.message.stop_reason, Some(StopReason::Other(_))),
        "snake_case stop_reason should parse (currently dropped to None)"
    );
}

/// Regression guard for issue 0001: `--redact all` must remove secrets from
/// export output. Fixed in Phase 1 by the dispatch-level redaction transform
/// (`export_to_string`/`export_to_file` → `Conversation::map_entries`).
#[test]
fn redaction_removes_planted_secret() {
    let entries = parse_fixture("redaction_session.jsonl");
    let conversation = Conversation::from_entries(entries).expect("build conversation");
    let opts = ExportOptions::default().with_full_redaction();
    let out = export_to_string(&conversation, ExportFormat::Markdown, &opts)
        .expect("redacted export should succeed");
    assert!(
        !out.contains("secret@example.com"),
        "the planted email should be redacted from --redact all output"
    );
}

/// Regression guard for issue 0003: `--only tool-results` returns the tool
/// results that live in user-role entries. Fixed in Phase 1 by making
/// `should_include_user` reach user entries when tool results are requested.
#[test]
fn only_tool_results_includes_tool_results() {
    let entries = parse_fixture("content_blocks_session.jsonl");
    let only: HashSet<ContentType> = [ContentType::ToolResults].into_iter().collect();
    let opts = ExportOptions::default().with_only(only);
    let out = markdown_with(&entries, &opts);
    assert!(
        out.contains("ok success"),
        "tool-result content should be present under --only tool-results"
    );
    // ...and the assistant's prose should not leak in (exclusive filter).
    assert!(
        !out.contains("Searching and looking up"),
        "--only tool-results must not include assistant text"
    );
}

/// Regression guard for issue 0004 (resolved as option A): `--only user`
/// includes the tool results within user entries (matching its help text), so it
/// differs from `--only prompts`, which is human-typed text only.
#[test]
fn only_user_includes_tool_results_unlike_prompts() {
    let entries = parse_fixture("content_blocks_session.jsonl");
    let user_out = markdown_with(
        &entries,
        &ExportOptions::default().with_only([ContentType::User].into_iter().collect()),
    );
    let prompts_out = markdown_with(
        &entries,
        &ExportOptions::default().with_only([ContentType::Prompts].into_iter().collect()),
    );
    assert!(
        user_out.contains("ok success"),
        "--only user should include tool results within user entries"
    );
    assert!(
        !prompts_out.contains("ok success"),
        "--only prompts should exclude tool results"
    );
    assert_ne!(
        user_out, prompts_out,
        "--only user must differ from --only prompts"
    );
}
