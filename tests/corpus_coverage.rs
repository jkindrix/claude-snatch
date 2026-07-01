//! Behavioral coverage tests over the golden corpus fixtures.
//!
//! Unlike the snapshot tests, these assert that specific Claude Code log shapes
//! parse into the expected model variants (not the `Unknown` fallback) and that
//! key fields survive. See `docs/test-corpus.md` for the corpus strategy and the
//! per-fixture provenance in `tests/fixtures/PROVENANCE.md`.

use std::collections::HashSet;
use std::path::PathBuf;

use claude_snatch::discovery::Session;
use claude_snatch::export::{export_to_string, ContentType, ExportFormat, ExportOptions};
use claude_snatch::model::{
    CompactTrigger, ContentBlock, ImageSource, LogEntry, StopReason, SystemSubtype, ToolResult,
    ToolUse, UserContent,
};
use claude_snatch::parser::JsonlParser;
use claude_snatch::reconstruction::Conversation;

/// Render entries to a markdown string with the given options, through the
/// dispatch transform (`export_to_string`) — exporters no longer filter on their
/// own, so a test must go through the transform to exercise `--only`/`--no-*`.
fn markdown_with(entries: &[LogEntry], opts: &ExportOptions) -> String {
    let conversation = Conversation::from_entries(entries.to_vec()).expect("build conversation");
    export_to_string(&conversation, ExportFormat::Markdown, opts)
        .expect("markdown export should succeed")
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

/// Regression guard for issue 0018 (fixed): a `thinking` block missing the
/// `signature` field must still parse (preserving the reasoning text and its
/// entry) rather than failing deserialization and silently dropping the whole
/// assistant turn. `signature` is now `#[serde(default)]`.
#[test]
fn thinking_block_without_signature_parses() {
    let jsonl = concat!(
        r#"{"type":"user","uuid":"a0000000-0000-0000-0000-000000000001","parentUuid":null,"timestamp":"2025-01-15T10:00:00.000Z","sessionId":"s","version":"2.1.193","message":{"role":"user","content":"think"}}"#,
        "\n",
        r#"{"type":"assistant","uuid":"a0000000-0000-0000-0000-000000000002","parentUuid":"a0000000-0000-0000-0000-000000000001","timestamp":"2025-01-15T10:00:01.000Z","sessionId":"s","version":"2.1.193","message":{"id":"m1","type":"message","role":"assistant","content":[{"type":"thinking","thinking":"NOSIGTHINK"},{"type":"text","text":"the answer"}],"model":"claude-sonnet-4","stop_reason":"end_turn"}}"#,
    );
    let entries = JsonlParser::new()
        .parse_str(jsonl)
        .expect("parse inline entries");
    // The assistant turn survived (was previously dropped as unparseable).
    let asst = entries
        .iter()
        .find_map(|e| match e {
            LogEntry::Assistant(a) => Some(a),
            _ => None,
        })
        .expect("assistant entry should survive a signature-less thinking block");
    let thinking_ok = asst.message.content.iter().any(|b| {
        matches!(b, ContentBlock::Thinking(t) if t.thinking == "NOSIGTHINK" && t.signature.is_empty())
    });
    assert!(
        thinking_ok,
        "thinking block parses with the text preserved and an empty signature"
    );
}

/// Regression guard for issue 0015 (fixed): `AssistantContent` was annotated
/// `#[serde(rename_all = "camelCase")]`, so it expected a `stopReason` key, but
/// every real Claude Code session writes snake_case `stop_reason` (the inner
/// `message` mirrors the snake_case Anthropic API object). The field had no
/// alias, so `stop_reason`/`stop_sequence` parsed to `None` on all real assistant
/// messages. Fixed by dropping the camelCase rename; this asserts the value now
/// rounds through `StopReason::Other`.
#[test]
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

/// Regression guard for issue 0014 (resolved as intended behavior): the
/// human-readable exporters replace image base64 payloads with a compact
/// `[N base64 image omitted]` marker so a 100 KB+ blob never dumps into rendered
/// output, while the structured `json`/`json-pretty` exporters deliberately
/// preserve the full payload for machine consumers (`raw-jsonl` is the
/// byte-faithful archival path). The fixture plants a base64 image both as a
/// top-level user block (`iVBORw0KGgo`) and inside an array-variant tool result
/// (`TOOLRESULTIMGBLOB`); this locks in which formats strip vs. preserve.
#[test]
fn image_payloads_stripped_in_text_formats_preserved_in_json() {
    use claude_snatch::export::ExportFormat;
    let entries = parse_fixture("content_blocks_session.jsonl");
    let conversation = Conversation::from_entries(entries).expect("build conversation");
    let opts = ExportOptions::default();

    // Human-readable formats strip both image payloads to the size marker.
    for fmt in [ExportFormat::Markdown, ExportFormat::Text] {
        let out = export_to_string(&conversation, fmt, &opts)
            .unwrap_or_else(|e| panic!("{fmt:?} export should succeed: {e}"));
        assert!(
            !out.contains("TOOLRESULTIMGBLOB") && !out.contains("iVBORw0KGgo"),
            "{fmt:?} should strip image base64 payloads"
        );
        assert!(
            out.contains("base64 image omitted"),
            "{fmt:?} should leave the size marker in place"
        );
    }

    // Structured JSON deliberately preserves the full payloads.
    for fmt in [ExportFormat::Json, ExportFormat::JsonPretty] {
        let out = export_to_string(&conversation, fmt, &opts)
            .unwrap_or_else(|e| panic!("{fmt:?} export should succeed: {e}"));
        assert!(
            out.contains("TOOLRESULTIMGBLOB") && out.contains("iVBORw0KGgo"),
            "{fmt:?} should preserve full image payloads for machine consumers"
        );
    }
}

/// Regression guard for the transform/code-only interaction (issue 0016): the
/// dispatch transform prunes text blocks under an exclusive `--only` filter, but
/// `--only code` extracts code *from* text at render time, so text must survive.
/// Routed through `export_to_string` (which applies the transform) — the
/// `markdown_with` helper bypasses it and would not exercise the prune.
#[test]
fn only_code_extracts_code_through_transform() {
    let jsonl = r#"{"type":"assistant","uuid":"a0000000-0000-0000-0000-000000000001","parentUuid":null,"timestamp":"2025-01-15T10:00:01.000Z","sessionId":"s","version":"2.1.193","message":{"id":"m1","type":"message","role":"assistant","content":[{"type":"text","text":"Here:\n```rust\nfn demo() {}\n```\n"}],"model":"claude-sonnet-4","stop_reason":"end_turn"}}"#;
    let entries = JsonlParser::new()
        .parse_str(jsonl)
        .expect("parse inline entry");
    let conversation = Conversation::from_entries(entries).expect("build conversation");
    let only: HashSet<ContentType> = [ContentType::Code].into_iter().collect();
    let out = export_to_string(
        &conversation,
        ExportFormat::Markdown,
        &ExportOptions::default().with_only(only),
    )
    .expect("code-only export should succeed");
    assert!(
        out.contains("fn demo"),
        "--only code must still extract code from text blocks after the transform prune"
    );
}

/// Regression guard for issue 0005: `--only code` must suppress entries that
/// yield no code, rather than emitting an empty assistant header + token footer.
/// Two assistant turns (prose-only, then code) → exactly one rendered header.
#[test]
fn only_code_suppresses_empty_entries() {
    let jsonl = concat!(
        r#"{"type":"assistant","uuid":"a0000000-0000-0000-0000-000000000001","parentUuid":null,"timestamp":"2025-01-15T10:00:01.000Z","sessionId":"s","version":"2.1.193","message":{"id":"m1","type":"message","role":"assistant","content":[{"type":"text","text":"Just prose, no code."}],"model":"claude-sonnet-4","stop_reason":"end_turn"}}"#,
        "\n",
        r#"{"type":"assistant","uuid":"a0000000-0000-0000-0000-000000000002","parentUuid":"a0000000-0000-0000-0000-000000000001","timestamp":"2025-01-15T10:00:02.000Z","sessionId":"s","version":"2.1.193","message":{"id":"m2","type":"message","role":"assistant","content":[{"type":"text","text":"Code:\n```rust\nfn keeper() {}\n```"}],"model":"claude-sonnet-4","stop_reason":"end_turn"}}"#,
    );
    let entries = JsonlParser::new()
        .parse_str(jsonl)
        .expect("parse inline entries");
    let conversation = Conversation::from_entries(entries).expect("build conversation");
    let opts = ExportOptions::default().with_only([ContentType::Code].into_iter().collect());
    let out = export_to_string(&conversation, ExportFormat::Markdown, &opts)
        .expect("code-only export should succeed");
    assert!(
        out.contains("fn keeper"),
        "the code turn's code is extracted"
    );
    assert!(
        !out.contains("Just prose"),
        "prose is not shown under --only code"
    );
    assert_eq!(
        out.matches("Assistant").count(),
        1,
        "the prose-only turn is suppressed, leaving no empty header stub"
    );
}

/// Regression guard for the full-strip: with exporters no longer self-filtering,
/// the non-exclusive `--no-thinking` (`include_thinking = false`) flag must still
/// drop thinking via the dispatch transform, while other content survives. Run
/// across the human exporters that render thinking.
#[test]
fn no_thinking_flag_prunes_thinking_through_transform() {
    let entries = parse_fixture("thinking_session.jsonl");
    let conversation = Conversation::from_entries(entries).expect("build conversation");
    let default_opts = ExportOptions::default();
    let no_thinking = ExportOptions {
        include_thinking: false,
        ..ExportOptions::default()
    };
    // A phrase that appears only inside the thinking block.
    let thinking_marker = "break down this multiplication";
    for fmt in [ExportFormat::Markdown, ExportFormat::Text] {
        let with = export_to_string(&conversation, fmt, &default_opts)
            .unwrap_or_else(|e| panic!("{fmt:?}: {e}"));
        assert!(
            with.contains(thinking_marker),
            "{fmt:?}: thinking shows by default"
        );
        let without = export_to_string(&conversation, fmt, &no_thinking)
            .unwrap_or_else(|e| panic!("{fmt:?}: {e}"));
        assert!(
            !without.contains(thinking_marker),
            "{fmt:?}: --no-thinking drops the thinking block"
        );
        assert!(
            without.len() < with.len(),
            "{fmt:?}: --no-thinking output is smaller but non-empty"
        );
    }
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

/// Regression guard for issue 0016 residual (b): user-role text is gated by the
/// user-text filter, not the assistant filter. Under `--only prompts` the user's
/// (Blocks-variant) text must appear while assistant text is dropped — previously
/// markdown/text/xml/json wrongly dropped user text via a role-unaware gate, and
/// html dropped Blocks-variant user text entirely (issue 0017, now fixed). Routed
/// through `export_to_string` so the dispatch transform is exercised.
#[test]
fn only_prompts_keeps_user_text_role_aware() {
    let entries = parse_fixture("content_blocks_session.jsonl");
    let conversation = Conversation::from_entries(entries).expect("build conversation");
    let opts = ExportOptions::default().with_only([ContentType::Prompts].into_iter().collect());
    for fmt in [
        ExportFormat::Markdown,
        ExportFormat::Text,
        ExportFormat::Json,
        ExportFormat::Html,
    ] {
        let out = export_to_string(&conversation, fmt, &opts)
            .unwrap_or_else(|e| panic!("{fmt:?} export should succeed: {e}"));
        assert!(
            out.contains("Here are three images"),
            "{fmt:?}: --only prompts must keep user text"
        );
        assert!(
            !out.contains("Searching and looking up"),
            "{fmt:?}: --only prompts must drop assistant text"
        );
    }
}

/// Render entries in an arbitrary format through the dispatch transform.
fn export_with(entries: &[LogEntry], fmt: ExportFormat, opts: &ExportOptions) -> String {
    let conversation = Conversation::from_entries(entries.to_vec()).expect("build conversation");
    export_to_string(&conversation, fmt, opts).expect("export should succeed")
}

#[test]
fn tool_render_markdown_renders_common_tools_readably() {
    let entries = parse_fixture("tool_render_session.jsonl");
    let out = markdown_with(&entries, &ExportOptions::full());

    // Edit → unified diff (not escaped JSON).
    assert!(out.contains("**Edit:** `src/foo.rs`"), "Edit label missing");
    assert!(out.contains("```diff"), "Edit should render a diff fence");
    assert!(
        out.contains("-    println!(\"old\");") && out.contains("+    println!(\"new\");"),
        "Edit diff should show old/new lines"
    );

    // MultiEdit → one diff per edit, with a change count.
    assert!(
        out.contains("**Edit:** `src/bar.rs` (2 changes)"),
        "MultiEdit should label the change count"
    );
    assert!(out.contains("-let a = 1;") && out.contains("+let a = 2;"));
    assert!(out.contains("-let b = 3;") && out.contains("+let b = 4;"));

    // Bash → shell block + description.
    assert!(
        out.contains("*Run the test suite*"),
        "Bash description missing"
    );
    assert!(
        out.contains("```bash\ncargo test --all"),
        "Bash should render a shell block"
    );

    // Write → code block with the file content.
    assert!(
        out.contains("**Write:** `src/new.rs`"),
        "Write label missing"
    );
    assert!(
        out.contains("pub fn answer() -> u32 {"),
        "Write should render the content"
    );

    // TodoWrite → checklist with status markers.
    assert!(out.contains("**Todos:**"), "Todos label missing");
    assert!(out.contains("- [x] Write the parser"));
    assert!(out.contains("- [~] Wire the exporter"));
    assert!(out.contains("- [ ] Add tests"));

    // Unknown tool (Read) → JSON fallback (today's behavior preserved).
    assert!(
        out.contains("Tool: `Read`") && out.contains("```json"),
        "Read should fall back to JSON rendering"
    );
}

#[test]
fn no_images_prunes_image_blocks() {
    let entries = parse_fixture("content_blocks_session.jsonl");

    // With images included (default), the markers are present.
    let with = markdown_with(&entries, &ExportOptions::full());
    assert!(
        with.contains("[Image:") || with.contains("![Image]"),
        "image markers should render when images are included"
    );

    // Nested tool-result images render too (the array-variant tool result's
    // image is shown via the base64-omission marker).
    assert!(
        with.contains("base64 image omitted"),
        "a nested tool-result image marker should render when images are included"
    );

    // The json export carries full image data, including the `toolUseResult`
    // sidecar image (its unique base64 prefix).
    let json_with = export_with(&entries, ExportFormat::Json, &ExportOptions::full());
    assert!(
        json_with.contains("\"type\":\"image\""),
        "json should carry image content blocks when images are included"
    );
    assert!(
        json_with.contains("VFVSU0lERUNBUmltYWdl"),
        "json should carry the toolUseResult image when images are included"
    );

    // include_images=false (the `--no-images` flag) prunes image blocks via the
    // transform, so no image marker survives in any human format — including
    // nested tool-result images.
    let without_opts = ExportOptions {
        include_images: false,
        ..ExportOptions::full()
    };
    for fmt in [
        ExportFormat::Markdown,
        ExportFormat::Text,
        ExportFormat::Html,
    ] {
        let out = export_with(&entries, fmt, &without_opts);
        assert!(
            !out.contains("[Image:")
                && !out.contains("![Image]")
                && !out.contains("Image file:")
                && !out.contains("base64 image omitted"),
            "{fmt:?}: --no-images must prune image markers (including nested tool-result images)"
        );
    }

    // json: every image is stripped — both content-block/tool-result images and
    // the `toolUseResult` sidecar image.
    let json_without = export_with(&entries, ExportFormat::Json, &without_opts);
    assert!(
        !json_without.contains("\"type\":\"image\"")
            && !json_without.contains("VFVSU0lERUNBUmltYWdl")
            && !json_without.contains("TOOLRESULTIMGBLOB"),
        "--no-images must strip all images from json, including the toolUseResult sidecar"
    );
}

#[test]
fn tool_render_text_and_html_render_readably() {
    let entries = parse_fixture("tool_render_session.jsonl");

    let text = export_with(&entries, ExportFormat::Text, &ExportOptions::full());
    assert!(
        text.contains("  Edit: src/foo.rs"),
        "text Edit label missing"
    );
    assert!(
        text.contains("  | +    println!(\"new\");"),
        "text diff should be line-prefixed"
    );
    assert!(
        text.contains("  | [x] Write the parser"),
        "text checklist missing"
    );

    let html = export_with(&entries, ExportFormat::Html, &ExportOptions::full());
    assert!(
        html.contains("language-diff"),
        "html Edit should use a diff code block"
    );
    assert!(
        html.contains("<ul class=\"todos\">") && html.contains("<li>[~] Wire the exporter</li>"),
        "html should render a todos list"
    );
}
