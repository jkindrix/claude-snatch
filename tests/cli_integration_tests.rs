//! CLI integration tests for the `snatch` binary.
//!
//! These tests exercise the snatch CLI as a subprocess using `assert_cmd`.
//! Each test creates a temporary Claude directory with fixture JSONL data,
//! sets SNATCH_CLAUDE_DIR to point at it, and verifies command output.

use assert_cmd::Command;
use predicates::prelude::*;
use tempfile::TempDir;

const PROJECT_PATH: &str = "/home/user/test-project";
const SESSION_ID: &str = "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee";

/// Encode a project path the way snatch does: hyphens -> %2D, then / -> -
fn encode_project_path(path: &str) -> String {
    path.replace('-', "%2D").replace('/', "-")
}

/// Create a temp Claude dir with a six-entry session fixture.
///
/// Six entries covering three exchanges -- including a Bash tool call and its
/// result -- mirrors the structure of `tests/fixtures/simple_session.jsonl`.
/// This produces meaningful output for `snatch stats` and `snatch validate`,
/// addressing the degenerate-output risk of a two-line fixture.
fn setup_fixture_dir() -> TempDir {
    let tmp = TempDir::new().expect("failed to create temp dir");
    let encoded = encode_project_path(PROJECT_PATH);
    let project_dir = tmp.path().join("projects").join(&encoded);
    std::fs::create_dir_all(&project_dir).expect("failed to create project dir");

    // Use a placeholder so the raw-string literal stays readable without
    // fighting format! double-brace escaping across six dense JSON lines.
    let lines: &[&str] = &[
        r#"{"type":"user","uuid":"11111111-1111-1111-1111-111111111111","parentUuid":null,"timestamp":"2025-01-15T10:00:00.000Z","sessionId":"SESSID","version":"2.0.74","message":{"role":"user","content":"Hello, Claude!"}}"#,
        r#"{"type":"assistant","uuid":"22222222-2222-2222-2222-222222222222","parentUuid":"11111111-1111-1111-1111-111111111111","timestamp":"2025-01-15T10:00:01.000Z","sessionId":"SESSID","version":"2.0.74","message":{"id":"msg_001","type":"message","role":"assistant","content":[{"type":"text","text":"Hello! How can I help you today?"}],"model":"claude-sonnet-4-20250514","stop_reason":"end_turn","usage":{"input_tokens":10,"output_tokens":15,"cache_creation_input_tokens":0,"cache_read_input_tokens":0}}}"#,
        r#"{"type":"user","uuid":"33333333-3333-3333-3333-333333333333","parentUuid":"22222222-2222-2222-2222-222222222222","timestamp":"2025-01-15T10:00:30.000Z","sessionId":"SESSID","version":"2.0.74","message":{"role":"user","content":"Can you list the files in the current directory?"}}"#,
        r#"{"type":"assistant","uuid":"44444444-4444-4444-4444-444444444444","parentUuid":"33333333-3333-3333-3333-333333333333","timestamp":"2025-01-15T10:00:31.000Z","sessionId":"SESSID","version":"2.0.74","message":{"id":"msg_002","type":"message","role":"assistant","content":[{"type":"text","text":"I will list the files for you."},{"type":"tool_use","id":"toolu_01","name":"Bash","input":{"command":"ls -la"}}],"model":"claude-sonnet-4-20250514","stop_reason":"tool_use","usage":{"input_tokens":25,"output_tokens":30,"cache_creation_input_tokens":0,"cache_read_input_tokens":0}}}"#,
        r#"{"type":"user","uuid":"55555555-5555-5555-5555-555555555555","parentUuid":"44444444-4444-4444-4444-444444444444","timestamp":"2025-01-15T10:00:32.000Z","sessionId":"SESSID","version":"2.0.74","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"toolu_01","content":"README.md\nsrc/\ntests/"}]}}"#,
        r#"{"type":"assistant","uuid":"66666666-6666-6666-6666-666666666666","parentUuid":"55555555-5555-5555-5555-555555555555","timestamp":"2025-01-15T10:00:33.000Z","sessionId":"SESSID","version":"2.0.74","message":{"id":"msg_003","type":"message","role":"assistant","content":[{"type":"text","text":"The directory contains: README.md, src/, and tests/."}],"model":"claude-sonnet-4-20250514","stop_reason":"end_turn","usage":{"input_tokens":40,"output_tokens":20,"cache_creation_input_tokens":0,"cache_read_input_tokens":0}}}"#,
    ];
    let jsonl = lines.join("\n").replace("SESSID", SESSION_ID) + "\n";

    let session_file = project_dir.join(format!("{SESSION_ID}.jsonl"));
    std::fs::write(&session_file, jsonl).expect("failed to write fixture");
    tmp
}

#[allow(deprecated)] // cargo_bin_cmd! replacement is unstable
fn snatch_cmd() -> Command {
    Command::cargo_bin("snatch").expect("snatch binary not found")
}

// =============================================================================
// list
// =============================================================================

#[test]
fn test_list_sessions_shows_fixture_id() {
    let tmp = setup_fixture_dir();
    snatch_cmd()
        .env("SNATCH_CLAUDE_DIR", tmp.path())
        .args(["list", "sessions", "--full-ids"])
        .assert()
        .success()
        .stdout(predicate::str::contains(SESSION_ID));
}

/// Uses `-o json` to verify the session UUID survives JSON serialisation
/// and enables field-level assertions independent of text formatting.
#[test]
fn test_list_sessions_json_contains_session_id() {
    let tmp = setup_fixture_dir();
    snatch_cmd()
        .env("SNATCH_CLAUDE_DIR", tmp.path())
        .args(["list", "sessions", "-o", "json"])
        .assert()
        .success()
        // "aaaaaaaa" is the short-ID prefix of SESSION_ID and will appear in
        // the JSON whether the field uses full or short UUIDs.
        .stdout(predicate::str::contains("aaaaaaaa"));
}

// =============================================================================
// search
// =============================================================================

#[test]
fn test_search_finds_matching_text() {
    let tmp = setup_fixture_dir();
    snatch_cmd()
        .env("SNATCH_CLAUDE_DIR", tmp.path())
        .args(["search", "Hello, Claude!"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Hello, Claude!"));
}

#[test]
fn test_search_no_match_returns_empty() {
    let tmp = setup_fixture_dir();
    // snatch search exits 0 with no output when the pattern matches nothing.
    snatch_cmd()
        .env("SNATCH_CLAUDE_DIR", tmp.path())
        .args(["search", "xyzzy_no_such_text_in_fixture"])
        .assert()
        .success()
        .stdout(predicate::str::contains("aaaaaaaa").not());
}

// =============================================================================
// export
// =============================================================================

#[test]
fn test_export_produces_output() {
    let tmp = setup_fixture_dir();
    // Default export format is markdown; output goes to stdout.
    snatch_cmd()
        .env("SNATCH_CLAUDE_DIR", tmp.path())
        .args(["export", SESSION_ID])
        .assert()
        .success()
        .stdout(predicate::str::is_empty().not())
        .stdout(predicate::str::contains("Hello"));
}

// =============================================================================
// info
// =============================================================================

#[test]
fn test_info_shows_session_metadata() {
    let tmp = setup_fixture_dir();
    snatch_cmd()
        .env("SNATCH_CLAUDE_DIR", tmp.path())
        .args(["info", SESSION_ID])
        .assert()
        .success()
        // The short-ID prefix must appear regardless of display width.
        .stdout(predicate::str::contains("aaaaaaaa"));
}

// =============================================================================
// stats
// =============================================================================

#[test]
fn test_stats_shows_usage() {
    let tmp = setup_fixture_dir();
    // Fixture totals: 10+25+40=75 input tokens, 15+30+20=65 output tokens.
    snatch_cmd()
        .env("SNATCH_CLAUDE_DIR", tmp.path())
        .args(["stats", SESSION_ID])
        .assert()
        .success()
        .stdout(predicate::str::contains("75"))
        .stdout(predicate::str::contains("65"));
}

/// JSON output allows field-level assertions on token counts independent of
/// text-formatting changes.
#[test]
fn test_stats_json_contains_token_counts() {
    let tmp = setup_fixture_dir();
    // Fixture totals: 10+25+40=75 input tokens, 15+30+20=65 output tokens.
    snatch_cmd()
        .env("SNATCH_CLAUDE_DIR", tmp.path())
        .args(["stats", SESSION_ID, "--json"])
        .assert()
        .success()
        .stdout(predicate::str::contains("input_tokens"))
        .stdout(predicate::str::contains("75"))
        .stdout(predicate::str::contains("65"));
}

// =============================================================================
// validate
// =============================================================================

#[test]
fn test_validate_valid_fixture() {
    let tmp = setup_fixture_dir();
    // `snatch validate` accepts a session ID (see `snatch validate --help`).
    // The short-ID prefix must appear in output to confirm the fixture was
    // actually processed (not silently skipped).
    snatch_cmd()
        .env("SNATCH_CLAUDE_DIR", tmp.path())
        .args(["validate", SESSION_ID])
        .assert()
        .success()
        .stdout(predicate::str::contains("aaaaaaaa"));
}

// =============================================================================
// error cases
// =============================================================================

#[test]
fn test_info_nonexistent_session_fails() {
    let tmp = setup_fixture_dir();
    snatch_cmd()
        .env("SNATCH_CLAUDE_DIR", tmp.path())
        .args(["info", "00000000-0000-0000-0000-000000000000"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("not found"));
}
