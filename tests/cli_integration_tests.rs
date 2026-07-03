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

/// Run a batch (`-o json`) search and return an exact pattern -> count map,
/// so tests can assert per-pattern counts rather than a loose substring.
fn batch_counts(tmp: &TempDir, args: &[&str]) -> std::collections::HashMap<String, i64> {
    let out = snatch_cmd()
        .env("SNATCH_CLAUDE_DIR", tmp.path())
        .args(args)
        .output()
        .expect("failed to run snatch search");
    assert!(out.status.success(), "search failed: {out:?}");
    let json: serde_json::Value =
        serde_json::from_slice(&out.stdout).expect("batch output should be JSON");
    json.as_array()
        .expect("batch JSON is an array")
        .iter()
        .map(|row| {
            (
                row["pattern"].as_str().expect("pattern").to_string(),
                row["count"].as_i64().expect("count"),
            )
        })
        .collect()
}

/// Regression for #23: multi-pattern positional search routes through the batch
/// path, which previously ignored `--model`. A bogus model must yield zero
/// matches for every pattern; the real model keeps the assistant-text matches
/// (each pattern appears once in assistant text — the user-text occurrence is
/// excluded by the assistant-only model filter).
#[test]
fn test_search_batch_applies_model_filter() {
    let tmp = setup_fixture_dir();

    let bogus = batch_counts(
        &tmp,
        &[
            "search",
            "-m",
            "definitely-not-a-model",
            "directory",
            "help",
            "-o",
            "json",
        ],
    );
    assert_eq!(bogus.get("directory"), Some(&0));
    assert_eq!(bogus.get("help"), Some(&0));

    let real = batch_counts(
        &tmp,
        &["search", "-m", "sonnet", "directory", "help", "-o", "json"],
    );
    assert_eq!(real.get("directory"), Some(&1));
    assert_eq!(real.get("help"), Some(&1));
}

/// Regression for #23: the batch path also bypassed non-model filters. Both
/// patterns match twice (user + assistant text); the fixture has no error
/// entries, so `--errors` must reduce every count to exactly zero.
#[test]
fn test_search_batch_applies_errors_filter() {
    let tmp = setup_fixture_dir();

    let all = batch_counts(&tmp, &["search", "files", "directory", "-o", "json"]);
    assert_eq!(all.get("files"), Some(&2));
    assert_eq!(all.get("directory"), Some(&2));

    let errs = batch_counts(
        &tmp,
        &["search", "--errors", "files", "directory", "-o", "json"],
    );
    assert_eq!(errs.get("files"), Some(&0));
    assert_eq!(errs.get("directory"), Some(&0));
}

/// Regression for #24: when the decoded directory name is ambiguous (a real
/// dash vs a path separator) and the real dir isn't on disk, the displayed
/// project path must come from the JSONL `cwd`, not the speculative slash-decode.
#[test]
fn test_list_sessions_prefers_cwd_for_ambiguous_path() {
    let tmp = TempDir::new().expect("temp dir");
    // Encoded dir decodes speculatively to /home/user/proj/alpha (not on disk);
    // the session's cwd records the true dashed path /home/user/proj-alpha.
    let encoded = "-home-user-proj-alpha";
    let project_dir = tmp.path().join("projects").join(encoded);
    std::fs::create_dir_all(&project_dir).unwrap();
    let sid = "dddddddd-dddd-dddd-dddd-dddddddddddd";
    let jsonl = format!(
        r#"{{"type":"user","uuid":"d1111111-1111-1111-1111-111111111111","parentUuid":null,"timestamp":"2025-01-15T10:00:00.000Z","sessionId":"{sid}","version":"2.0.74","cwd":"/home/user/proj-alpha","message":{{"role":"user","content":"hi"}}}}"#
    ) + "\n";
    std::fs::write(project_dir.join(format!("{sid}.jsonl")), jsonl).unwrap();

    snatch_cmd()
        .env("SNATCH_CLAUDE_DIR", tmp.path())
        .args(["list", "sessions", "-o", "json", "--full-ids"])
        .assert()
        .success()
        .stdout(predicate::str::contains("/home/user/proj-alpha"))
        .stdout(predicate::str::contains("/home/user/proj/alpha").not());
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

/// Build a temp Claude dir holding a session whose user prompt contains a planted
/// secret email, for end-to-end redaction tests.
fn setup_secret_fixture_dir() -> TempDir {
    let tmp = TempDir::new().expect("failed to create temp dir");
    let encoded = encode_project_path(PROJECT_PATH);
    let project_dir = tmp.path().join("projects").join(&encoded);
    std::fs::create_dir_all(&project_dir).expect("failed to create project dir");

    let lines: &[&str] = &[
        r#"{"type":"user","uuid":"11111111-1111-1111-1111-111111111111","parentUuid":null,"timestamp":"2025-01-15T10:00:00.000Z","sessionId":"SESSID","version":"2.1.193","message":{"role":"user","content":"My email is secret@example.com, please remember it."}}"#,
        r#"{"type":"assistant","uuid":"22222222-2222-2222-2222-222222222222","parentUuid":"11111111-1111-1111-1111-111111111111","timestamp":"2025-01-15T10:00:01.000Z","sessionId":"SESSID","version":"2.1.193","message":{"id":"msg_001","type":"message","role":"assistant","content":[{"type":"text","text":"Noted."}],"model":"claude-sonnet-4-20250514","stop_reason":"end_turn","usage":{"input_tokens":10,"output_tokens":5}}}"#,
    ];
    let jsonl = lines.join("\n").replace("SESSID", SESSION_ID) + "\n";
    let session_file = project_dir.join(format!("{SESSION_ID}.jsonl"));
    std::fs::write(&session_file, jsonl).expect("failed to write secret fixture");
    tmp
}

/// Regression guard for issue 0016: `--redact all` must remove secrets through the
/// real CLI dispatch (not just the module-level `export_to_string`). The control
/// assertion (secret present without `--redact`) proves redaction is acting rather
/// than the secret merely being absent.
#[test]
fn test_export_redact_removes_secret_via_cli() {
    let tmp = setup_secret_fixture_dir();

    // Control: without --redact, the secret is present.
    snatch_cmd()
        .env("SNATCH_CLAUDE_DIR", tmp.path())
        .args(["export", SESSION_ID, "-f", "markdown"])
        .assert()
        .success()
        .stdout(predicate::str::contains("secret@example.com"));

    // With --redact all, the secret must be gone.
    snatch_cmd()
        .env("SNATCH_CLAUDE_DIR", tmp.path())
        .args(["export", SESSION_ID, "-f", "markdown", "--redact", "all"])
        .assert()
        .success()
        .stdout(predicate::str::contains("secret@example.com").not());
}

/// Regression guard for issue 0022: the multi-session (`--all`) SQLite export
/// path builds its own options and calls the exporter directly, so it must apply
/// the redaction transform too — 0016 fixed only the single-session path. The
/// control (secret present without `--redact`) proves redaction is acting.
#[test]
fn test_export_all_sqlite_redacts_secret() {
    let tmp = setup_secret_fixture_dir();

    // Control: without --redact, the secret is written to the database.
    let ctrl = TempDir::new().unwrap();
    let ctrl_db = ctrl.path().join("ctrl.db");
    snatch_cmd()
        .env("SNATCH_CLAUDE_DIR", tmp.path())
        .args([
            "export",
            "--all",
            "-f",
            "sqlite",
            "--out",
            ctrl_db.to_str().unwrap(),
        ])
        .assert()
        .success();
    let ctrl_bytes = std::fs::read(&ctrl_db).expect("read control db");
    assert!(
        String::from_utf8_lossy(&ctrl_bytes).contains("secret@example.com"),
        "control: the secret should be present in the db without --redact"
    );

    // With --redact all, the secret must not reach the database.
    let red = TempDir::new().unwrap();
    let red_db = red.path().join("red.db");
    snatch_cmd()
        .env("SNATCH_CLAUDE_DIR", tmp.path())
        .args([
            "export",
            "--all",
            "-f",
            "sqlite",
            "--redact",
            "all",
            "--out",
            red_db.to_str().unwrap(),
        ])
        .assert()
        .success();
    let red_bytes = std::fs::read(&red_db).expect("read redacted db");
    assert!(
        !String::from_utf8_lossy(&red_bytes).contains("secret@example.com"),
        "issue 0022: batch sqlite --redact all must remove the secret"
    );
}

/// Build a temp Claude dir whose only PII (an email) lives inside a tool result
/// in a user-role entry — the case `--warn-pii` was blind to (issue 0002).
fn setup_tool_result_pii_dir() -> TempDir {
    let tmp = TempDir::new().expect("failed to create temp dir");
    let encoded = encode_project_path(PROJECT_PATH);
    let project_dir = tmp.path().join("projects").join(&encoded);
    std::fs::create_dir_all(&project_dir).expect("failed to create project dir");

    let lines: &[&str] = &[
        r#"{"type":"user","uuid":"11111111-1111-1111-1111-111111111111","parentUuid":null,"timestamp":"2025-01-15T10:00:00.000Z","sessionId":"SESSID","version":"2.1.193","message":{"role":"user","content":"show the git log"}}"#,
        r#"{"type":"assistant","uuid":"22222222-2222-2222-2222-222222222222","parentUuid":"11111111-1111-1111-1111-111111111111","timestamp":"2025-01-15T10:00:01.000Z","sessionId":"SESSID","version":"2.1.193","message":{"id":"m1","type":"message","role":"assistant","content":[{"type":"tool_use","id":"toolu_01","name":"Bash","input":{"command":"git log"}}],"model":"claude-sonnet-4","stop_reason":"tool_use","usage":{"input_tokens":1,"output_tokens":1}}}"#,
        r#"{"type":"user","uuid":"33333333-3333-3333-3333-333333333333","parentUuid":"22222222-2222-2222-2222-222222222222","timestamp":"2025-01-15T10:00:02.000Z","sessionId":"SESSID","version":"2.1.193","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"toolu_01","content":"Author: leaked@example.com committed"}]}}"#,
    ];
    let jsonl = lines.join("\n").replace("SESSID", SESSION_ID) + "\n";
    std::fs::write(project_dir.join(format!("{SESSION_ID}.jsonl")), jsonl)
        .expect("failed to write tool-result PII fixture");
    tmp
}

/// Regression guard for issue 0002: `--warn-pii` must scan tool-result content,
/// not just user/assistant prose. The email lives only inside a tool result in a
/// user-role entry; the warning is printed to stderr.
#[test]
fn test_warn_pii_detects_tool_result_pii() {
    let tmp = setup_tool_result_pii_dir();
    snatch_cmd()
        .env("SNATCH_CLAUDE_DIR", tmp.path())
        .args(["export", SESSION_ID, "-f", "markdown", "--warn-pii"])
        .assert()
        .success()
        .stderr(predicate::str::contains("PII").and(predicate::str::contains("email")));
}

/// Build a temp Claude dir with uniquely-marked tool-use input and tool-result
/// content (no overlap with prose) so the negation flags can be asserted exactly.
fn setup_negation_fixture_dir() -> TempDir {
    let tmp = TempDir::new().expect("failed to create temp dir");
    let encoded = encode_project_path(PROJECT_PATH);
    let project_dir = tmp.path().join("projects").join(&encoded);
    std::fs::create_dir_all(&project_dir).expect("failed to create project dir");

    let lines: &[&str] = &[
        r#"{"type":"user","uuid":"11111111-1111-1111-1111-111111111111","parentUuid":null,"timestamp":"2025-01-15T10:00:00.000Z","sessionId":"SESSID","version":"2.1.193","message":{"role":"user","content":"run it"}}"#,
        r#"{"type":"assistant","uuid":"22222222-2222-2222-2222-222222222222","parentUuid":"11111111-1111-1111-1111-111111111111","timestamp":"2025-01-15T10:00:01.000Z","sessionId":"SESSID","version":"2.1.193","message":{"id":"m1","type":"message","role":"assistant","content":[{"type":"tool_use","id":"toolu_01","name":"Bash","input":{"command":"echo NEGTEST_TOOLUSE"}}],"model":"claude-sonnet-4","stop_reason":"tool_use","usage":{"input_tokens":1,"output_tokens":1}}}"#,
        r#"{"type":"user","uuid":"33333333-3333-3333-3333-333333333333","parentUuid":"22222222-2222-2222-2222-222222222222","timestamp":"2025-01-15T10:00:02.000Z","sessionId":"SESSID","version":"2.1.193","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"toolu_01","content":"NEGTEST_TOOLRESULT"}]}}"#,
        r#"{"type":"assistant","uuid":"44444444-4444-4444-4444-444444444444","parentUuid":"33333333-3333-3333-3333-333333333333","timestamp":"2025-01-15T10:00:03.000Z","sessionId":"SESSID","version":"2.1.193","message":{"id":"m2","type":"message","role":"assistant","content":[{"type":"text","text":"done"}],"model":"claude-sonnet-4","stop_reason":"end_turn","usage":{"input_tokens":1,"output_tokens":1}}}"#,
    ];
    let jsonl = lines.join("\n").replace("SESSID", SESSION_ID) + "\n";
    std::fs::write(project_dir.join(format!("{SESSION_ID}.jsonl")), jsonl)
        .expect("failed to write negation fixture");
    tmp
}

/// Regression guard for issue 0009: the documented `--no-tool-use` /
/// `--no-tool-results` flags must actually disable content (they previously
/// didn't exist and were a parse error). Control asserts the markers appear by
/// default, then each `--no-*` removes its marker.
#[test]
fn test_negation_flags_disable_content() {
    let tmp = setup_negation_fixture_dir();

    // Control: tool use + tool result markers appear by default.
    snatch_cmd()
        .env("SNATCH_CLAUDE_DIR", tmp.path())
        .args(["export", SESSION_ID, "-f", "markdown"])
        .assert()
        .success()
        .stdout(predicate::str::contains("NEGTEST_TOOLUSE"))
        .stdout(predicate::str::contains("NEGTEST_TOOLRESULT"));

    // --no-tool-use removes the tool call.
    snatch_cmd()
        .env("SNATCH_CLAUDE_DIR", tmp.path())
        .args(["export", SESSION_ID, "-f", "markdown", "--no-tool-use"])
        .assert()
        .success()
        .stdout(predicate::str::contains("NEGTEST_TOOLUSE").not());

    // --no-tool-results removes the tool result.
    snatch_cmd()
        .env("SNATCH_CLAUDE_DIR", tmp.path())
        .args(["export", SESSION_ID, "-f", "markdown", "--no-tool-results"])
        .assert()
        .success()
        .stdout(predicate::str::contains("NEGTEST_TOOLRESULT").not());
}

/// Build a temp Claude dir with a parent session plus one on-disk subagent
/// transcript (`<id>/subagents/agent-*.jsonl`).
fn setup_session_with_subagents_dir() -> TempDir {
    let tmp = TempDir::new().expect("failed to create temp dir");
    let encoded = encode_project_path(PROJECT_PATH);
    let project_dir = tmp.path().join("projects").join(&encoded);
    std::fs::create_dir_all(&project_dir).expect("failed to create project dir");

    let parent = r#"{"type":"user","uuid":"11111111-1111-1111-1111-111111111111","parentUuid":null,"timestamp":"2025-01-15T10:00:00.000Z","sessionId":"SESSID","version":"2.1.193","message":{"role":"user","content":"hi"}}"#
        .replace("SESSID", SESSION_ID);
    std::fs::write(
        project_dir.join(format!("{SESSION_ID}.jsonl")),
        parent + "\n",
    )
    .expect("failed to write parent");

    let sub_dir = project_dir.join(SESSION_ID).join("subagents");
    std::fs::create_dir_all(&sub_dir).expect("failed to create subagents dir");
    let sub = r#"{"type":"user","uuid":"a1111111-1111-1111-1111-111111111111","parentUuid":null,"timestamp":"2025-01-15T10:00:00.000Z","sessionId":"agent-test","version":"2.1.193","message":{"role":"user","content":"subagent work"}}"#;
    std::fs::write(sub_dir.join("agent-test.jsonl"), format!("{sub}\n"))
        .expect("failed to write subagent");
    tmp
}

/// Regression guard for issue 0012: `raw-jsonl` is single-file and silently
/// excludes subagent transcripts; it must at least warn (to stderr) that they
/// are not included, rather than dropping them silently.
#[test]
fn test_raw_jsonl_warns_about_excluded_subagents() {
    let tmp = setup_session_with_subagents_dir();
    snatch_cmd()
        .env("SNATCH_CLAUDE_DIR", tmp.path())
        .args(["export", SESSION_ID, "-f", "raw-jsonl"])
        .assert()
        .success()
        .stderr(predicate::str::contains("subagent").and(predicate::str::contains("not included")));
}

/// Regression guard for issue 0011: the markdown stats must headline the on-disk
/// subagent transcript count, not the billed-usage count. The fixture has one
/// transcript on disk and zero billed invocations — previously the Subagents line
/// was omitted entirely (billed == 0), silently hiding the subagent.
#[test]
fn test_subagent_count_headlines_inventory() {
    let tmp = setup_session_with_subagents_dir();
    snatch_cmd()
        .env("SNATCH_CLAUDE_DIR", tmp.path())
        .args(["export", SESSION_ID, "-f", "markdown"])
        .assert()
        .success()
        .stdout(predicate::str::contains("**Subagents:** 1"));
}

/// End-to-end guard that `--only` filtering works through the real CLI dispatch
/// (the transform), now that exporters no longer self-filter (full strip). Uses
/// the uniquely-marked tool-use / tool-result fixture.
#[test]
fn test_only_filter_via_cli() {
    let tmp = setup_negation_fixture_dir();

    // --only prompts: human text only — no tool use or tool result.
    snatch_cmd()
        .env("SNATCH_CLAUDE_DIR", tmp.path())
        .args(["export", SESSION_ID, "-f", "markdown", "--only", "prompts"])
        .assert()
        .success()
        .stdout(predicate::str::contains("run it"))
        .stdout(predicate::str::contains("NEGTEST_TOOLUSE").not())
        .stdout(predicate::str::contains("NEGTEST_TOOLRESULT").not());

    // --only tool-results: the tool result, but not the human prompt.
    snatch_cmd()
        .env("SNATCH_CLAUDE_DIR", tmp.path())
        .args([
            "export",
            SESSION_ID,
            "-f",
            "markdown",
            "--only",
            "tool-results",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("NEGTEST_TOOLRESULT"))
        .stdout(predicate::str::contains("run it").not());
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

/// Regression for #25: `--models` must emit a per-model breakdown for a single
/// session, and must not appear without the flag.
#[test]
fn test_stats_single_session_models_breakdown() {
    let tmp = setup_fixture_dir();
    snatch_cmd()
        .env("SNATCH_CLAUDE_DIR", tmp.path())
        .args(["stats", SESSION_ID, "--models"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Model Usage:"));

    snatch_cmd()
        .env("SNATCH_CLAUDE_DIR", tmp.path())
        .args(["stats", SESSION_ID])
        .assert()
        .success()
        .stdout(predicate::str::contains("Model Usage:").not());
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

// =============================================================================
// subagent markers (issue 0004): meta.json-less subagents must never vanish
// =============================================================================

const SUBAGENT_SESSION_ID: &str = "5ababae0-1111-2222-3333-444444444444";

/// Build a fixture with one `Task` spawn call and a single subagent transcript
/// that has NO `agent-*.meta.json` sidecar (the ~51% common case). The matcher
/// must still surface it (single-spawn fallback / unlinked marker).
fn setup_subagent_fixture() -> TempDir {
    let tmp = TempDir::new().expect("failed to create temp dir");
    let encoded = encode_project_path(PROJECT_PATH);
    let project_dir = tmp.path().join("projects").join(&encoded);
    std::fs::create_dir_all(&project_dir).expect("failed to create project dir");

    let lines: &[&str] = &[
        r#"{"type":"user","uuid":"a1111111-1111-1111-1111-111111111111","parentUuid":null,"timestamp":"2025-01-15T10:00:00.000Z","sessionId":"SUBSESS","version":"2.0.74","message":{"role":"user","content":"Explore the codebase."}}"#,
        r#"{"type":"assistant","uuid":"a2222222-2222-2222-2222-222222222222","parentUuid":"a1111111-1111-1111-1111-111111111111","timestamp":"2025-01-15T10:00:01.000Z","sessionId":"SUBSESS","version":"2.0.74","message":{"id":"msg_t01","type":"message","role":"assistant","content":[{"type":"tool_use","id":"toolu_spawn","name":"Task","input":{"description":"Explore codebase structure","subagent_type":"Explore","prompt":"go"}}],"model":"claude-sonnet-4-20250514","stop_reason":"tool_use","usage":{"input_tokens":10,"output_tokens":15,"cache_creation_input_tokens":0,"cache_read_input_tokens":0}}}"#,
        r#"{"type":"user","uuid":"a3333333-3333-3333-3333-333333333333","parentUuid":"a2222222-2222-2222-2222-222222222222","timestamp":"2025-01-15T10:01:00.000Z","sessionId":"SUBSESS","version":"2.0.74","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"toolu_spawn","content":"exploration summary returned"}]}}"#,
        r#"{"type":"assistant","uuid":"a4444444-4444-4444-4444-444444444444","parentUuid":"a3333333-3333-3333-3333-333333333333","timestamp":"2025-01-15T10:01:01.000Z","sessionId":"SUBSESS","version":"2.0.74","message":{"id":"msg_t02","type":"message","role":"assistant","content":[{"type":"text","text":"Now I understand the architecture."}],"model":"claude-sonnet-4-20250514","stop_reason":"end_turn","usage":{"input_tokens":40,"output_tokens":20,"cache_creation_input_tokens":0,"cache_read_input_tokens":0}}}"#,
    ];
    let jsonl = lines.join("\n").replace("SUBSESS", SUBAGENT_SESSION_ID) + "\n";
    let session_file = project_dir.join(format!("{SUBAGENT_SESSION_ID}.jsonl"));
    std::fs::write(&session_file, jsonl).expect("failed to write fixture");

    // Subagent transcript WITHOUT a meta.json sidecar.
    let sub_dir = project_dir.join(SUBAGENT_SESSION_ID).join("subagents");
    std::fs::create_dir_all(&sub_dir).expect("failed to create subagents dir");
    let sub_line = r#"{"type":"assistant","uuid":"b1111111-1111-1111-1111-111111111111","parentUuid":null,"timestamp":"2025-01-15T10:00:30.000Z","sessionId":"agent-deadbeef","version":"2.0.74","message":{"id":"msg_s01","type":"message","role":"assistant","content":[{"type":"text","text":"comprehensive exploration report of the codebase"}],"model":"claude-sonnet-4-20250514","stop_reason":"end_turn","usage":{"input_tokens":5,"output_tokens":5,"cache_creation_input_tokens":0,"cache_read_input_tokens":0}}}"#;
    std::fs::write(
        sub_dir.join("agent-deadbeef.jsonl"),
        format!("{sub_line}\n"),
    )
    .expect("failed to write subagent transcript");

    tmp
}

#[test]
fn test_messages_full_surfaces_meta_less_subagent() {
    let tmp = setup_subagent_fixture();
    snatch_cmd()
        .env("SNATCH_CLAUDE_DIR", tmp.path())
        .args(["messages", SUBAGENT_SESSION_ID, "--detail", "full"])
        .assert()
        .success()
        // Single-spawn fallback links the meta-less subagent to its Task call.
        .stdout(predicate::str::contains("subagent agent-deadbeef"));
}

#[test]
fn test_timeline_surfaces_meta_less_subagent() {
    let tmp = setup_subagent_fixture();
    snatch_cmd()
        .env("SNATCH_CLAUDE_DIR", tmp.path())
        .args(["timeline", SUBAGENT_SESSION_ID])
        .assert()
        .success()
        // Timeline previously had zero subagent handling; now a marker appears.
        .stdout(predicate::str::contains("Subagents:"))
        .stdout(predicate::str::contains("agent-deadbeef"));
}

// =============================================================================
// resume-chain collapse (list / recent / export --all)
// =============================================================================

const CHAIN_ROOT_ID: &str = "aaaaaaaa-1111-1111-1111-111111111111";
const CHAIN_CONT_ID: &str = "bbbbbbbb-2222-2222-2222-222222222222";

/// Create a temp Claude dir holding one two-file resume chain.
///
/// The continuation file's internal `sessionId` points at the root file's
/// UUID, which is how Claude Code links resumed sessions into one logical
/// conversation.
fn setup_chain_dir() -> TempDir {
    let tmp = TempDir::new().expect("failed to create temp dir");
    let encoded = encode_project_path(PROJECT_PATH);
    let project_dir = tmp.path().join("projects").join(&encoded);
    std::fs::create_dir_all(&project_dir).expect("failed to create project dir");

    let root_jsonl = format!(
        r#"{{"type":"user","uuid":"c1111111-1111-1111-1111-111111111111","parentUuid":null,"timestamp":"2025-01-15T10:00:00.000Z","sessionId":"{CHAIN_ROOT_ID}","version":"2.0.74","message":{{"role":"user","content":"first half of the conversation"}}}}"#
    ) + "\n";
    std::fs::write(
        project_dir.join(format!("{CHAIN_ROOT_ID}.jsonl")),
        root_jsonl,
    )
    .expect("write root");

    let cont_jsonl = format!(
        r#"{{"type":"user","uuid":"c2222222-2222-2222-2222-222222222222","parentUuid":null,"timestamp":"2025-01-15T11:00:00.000Z","sessionId":"{CHAIN_ROOT_ID}","version":"2.0.74","message":{{"role":"user","content":"resumed second half of the conversation with extra text"}}}}"#
    ) + "\n";
    std::fs::write(
        project_dir.join(format!("{CHAIN_CONT_ID}.jsonl")),
        cont_jsonl,
    )
    .expect("write continuation");

    // Set mtimes to the embedded timestamps. Sessions are sorted by mtime; with
    // both files written ~simultaneously (equal mtimes), the stable sort falls
    // back to readdir order, which differs by platform and made chain collapse
    // non-deterministic (mac/Windows-only failures). Distinct mtimes fix the order.
    set_file_mtime(&project_dir, CHAIN_ROOT_ID, "2025-01-15T10:00:00.000Z");
    set_file_mtime(&project_dir, CHAIN_CONT_ID, "2025-01-15T11:00:00.000Z");

    tmp
}

/// Set a session file's mtime to an RFC3339 timestamp, so mtime-based ordering
/// is deterministic across platforms (fixtures otherwise share a write-time
/// mtime and fall back to readdir order).
fn set_file_mtime(project_dir: &std::path::Path, file_id: &str, ts: &str) {
    let t: std::time::SystemTime = chrono::DateTime::parse_from_rfc3339(ts).unwrap().into();
    std::fs::OpenOptions::new()
        .write(true)
        .open(project_dir.join(format!("{file_id}.jsonl")))
        .unwrap()
        .set_modified(t)
        .unwrap();
}

#[test]
fn test_list_collapses_chain_by_default_json() {
    let tmp = setup_chain_dir();
    let output = snatch_cmd()
        .env("SNATCH_CLAUDE_DIR", tmp.path())
        .args(["list", "sessions", "-o", "json", "--full-ids"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let text = String::from_utf8(output).unwrap();
    let rows: serde_json::Value = serde_json::from_str(&text).unwrap();
    let arr = rows.as_array().unwrap();
    assert_eq!(arr.len(), 1, "chain collapses to a single logical row");
    let row = &arr[0];
    assert_eq!(row["session_id"], CHAIN_ROOT_ID);
    assert_eq!(row["latest_session_id"], CHAIN_CONT_ID);
    assert_eq!(row["chain_member_count"], 2);
    let members = row["chain_members"].as_array().unwrap();
    assert_eq!(members.len(), 2);
    assert_eq!(members[0], CHAIN_ROOT_ID);
    assert_eq!(members[1], CHAIN_CONT_ID);
}

#[test]
fn test_list_no_chain_shows_each_member() {
    let tmp = setup_chain_dir();
    let output = snatch_cmd()
        .env("SNATCH_CLAUDE_DIR", tmp.path())
        .args(["list", "sessions", "-o", "json", "--full-ids", "--no-chain"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let text = String::from_utf8(output).unwrap();
    let rows: serde_json::Value = serde_json::from_str(&text).unwrap();
    let arr = rows.as_array().unwrap();
    assert_eq!(arr.len(), 2, "--no-chain restores per-file rows");
    // Flat rows do not carry the collapsed chain fields.
    assert!(text.contains(CHAIN_ROOT_ID));
    assert!(text.contains(CHAIN_CONT_ID));
    assert!(!text.contains("chain_member_count"));
}

#[test]
fn test_list_chain_text_marker() {
    let tmp = setup_chain_dir();
    snatch_cmd()
        .env("SNATCH_CLAUDE_DIR", tmp.path())
        .args(["list", "sessions"])
        .assert()
        .success()
        .stdout(predicate::str::contains("chain: 2 files"));
}

#[test]
fn test_list_sort_size_uses_aggregate() {
    // A standalone session larger than either chain member individually, but
    // smaller than the chain's summed size, must rank below the chain when
    // sorting by aggregate size.
    let tmp = setup_chain_dir();
    let encoded = encode_project_path(PROJECT_PATH);
    let project_dir = tmp.path().join("projects").join(&encoded);
    let big_id = "dddddddd-3333-3333-3333-333333333333";
    // Size the standalone file to fall strictly between the largest single
    // chain member and the chain's summed size, so it only ranks below the
    // chain when sorting uses the aggregate (not a single member's) size.
    let root_sz = std::fs::metadata(project_dir.join(format!("{CHAIN_ROOT_ID}.jsonl")))
        .unwrap()
        .len();
    let cont_sz = std::fs::metadata(project_dir.join(format!("{CHAIN_CONT_ID}.jsonl")))
        .unwrap()
        .len();
    let target = (root_sz.max(cont_sz) + (root_sz + cont_sz)) / 2;
    let empty = format!(
        r#"{{"type":"user","uuid":"d3333333-3333-3333-3333-333333333333","parentUuid":null,"timestamp":"2025-01-14T09:00:00.000Z","sessionId":"{big_id}","version":"2.0.74","message":{{"role":"user","content":""}}}}"#
    );
    let pad = (target as usize).saturating_sub(empty.len() + 1);
    let big_jsonl = format!(
        r#"{{"type":"user","uuid":"d3333333-3333-3333-3333-333333333333","parentUuid":null,"timestamp":"2025-01-14T09:00:00.000Z","sessionId":"{big_id}","version":"2.0.74","message":{{"role":"user","content":"{}"}}}}"#,
        "z".repeat(pad)
    ) + "\n";
    std::fs::write(project_dir.join(format!("{big_id}.jsonl")), big_jsonl).expect("write big");

    let output = snatch_cmd()
        .env("SNATCH_CLAUDE_DIR", tmp.path())
        .args(["list", "sessions", "-o", "json", "--full-ids", "-s", "size"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let text = String::from_utf8(output).unwrap();
    let rows: serde_json::Value = serde_json::from_str(&text).unwrap();
    let arr = rows.as_array().unwrap();
    assert_eq!(arr.len(), 2);
    // Chain (aggregate) sorts first ahead of the larger single file.
    assert_eq!(arr[0]["session_id"], CHAIN_ROOT_ID);
    assert_eq!(arr[1]["session_id"], big_id);
}

#[test]
fn test_recent_collapses_chain_json() {
    let tmp = setup_chain_dir();
    let output = snatch_cmd()
        .env("SNATCH_CLAUDE_DIR", tmp.path())
        .args(["recent", "-o", "json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let text = String::from_utf8(output).unwrap();
    let rows: serde_json::Value = serde_json::from_str(&text).unwrap();
    let arr = rows.as_array().unwrap();
    assert_eq!(arr.len(), 1);
    assert_eq!(arr[0]["id"], CHAIN_ROOT_ID);
    assert_eq!(arr[0]["chain_member_count"], 2);
}

#[test]
fn test_recent_no_chain_shows_members() {
    let tmp = setup_chain_dir();
    let output = snatch_cmd()
        .env("SNATCH_CLAUDE_DIR", tmp.path())
        .args(["recent", "-o", "json", "--no-chain"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let text = String::from_utf8(output).unwrap();
    let rows: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(rows.as_array().unwrap().len(), 2);
}

#[test]
fn test_export_all_one_file_per_chain() {
    let tmp = setup_chain_dir();
    let out_dir = TempDir::new().unwrap();
    snatch_cmd()
        .env("SNATCH_CLAUDE_DIR", tmp.path())
        .args([
            "export",
            "--all",
            "-f",
            "markdown",
            "--out",
            out_dir.path().to_str().unwrap(),
        ])
        .assert()
        .success();

    let names: Vec<String> = std::fs::read_dir(out_dir.path())
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().to_string())
        .collect();
    let md: Vec<&String> = names
        .iter()
        .filter(|n| {
            std::path::Path::new(n)
                .extension()
                .is_some_and(|e| e == "md")
        })
        .collect();
    assert_eq!(
        md.len(),
        1,
        "one artifact per logical chain, got: {names:?}"
    );
    assert!(md[0].contains(CHAIN_ROOT_ID), "filename keyed by root id");
    assert!(!md[0].contains(CHAIN_CONT_ID));
}

#[test]
fn test_export_all_no_chain_one_file_per_member() {
    let tmp = setup_chain_dir();
    let out_dir = TempDir::new().unwrap();
    snatch_cmd()
        .env("SNATCH_CLAUDE_DIR", tmp.path())
        .args([
            "export",
            "--all",
            "--no-chain",
            "-f",
            "markdown",
            "--out",
            out_dir.path().to_str().unwrap(),
        ])
        .assert()
        .success();

    let count = std::fs::read_dir(out_dir.path())
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| {
            std::path::Path::new(&e.file_name())
                .extension()
                .is_some_and(|x| x == "md")
        })
        .count();
    assert_eq!(count, 2, "--no-chain exports each member file");
}

// =============================================================================
// chain-aware date filtering (export --all) and metadata visibility (list)
// =============================================================================

/// Build a temp Claude dir with a two-file chain whose members carry the given
/// timestamps (root first, continuation second).
fn setup_dated_chain_dir(root_ts: &str, cont_ts: &str) -> TempDir {
    let tmp = TempDir::new().expect("failed to create temp dir");
    let encoded = encode_project_path(PROJECT_PATH);
    let project_dir = tmp.path().join("projects").join(&encoded);
    std::fs::create_dir_all(&project_dir).expect("failed to create project dir");

    let root_jsonl = format!(
        r#"{{"type":"user","uuid":"c1111111-1111-1111-1111-111111111111","parentUuid":null,"timestamp":"{root_ts}","sessionId":"{CHAIN_ROOT_ID}","version":"2.0.74","message":{{"role":"user","content":"first half"}}}}"#
    ) + "\n";
    std::fs::write(
        project_dir.join(format!("{CHAIN_ROOT_ID}.jsonl")),
        root_jsonl,
    )
    .unwrap();

    let cont_jsonl = format!(
        r#"{{"type":"user","uuid":"c2222222-2222-2222-2222-222222222222","parentUuid":null,"timestamp":"{cont_ts}","sessionId":"{CHAIN_ROOT_ID}","version":"2.0.74","message":{{"role":"user","content":"resumed second half"}}}}"#
    ) + "\n";
    std::fs::write(
        project_dir.join(format!("{CHAIN_CONT_ID}.jsonl")),
        cont_jsonl,
    )
    .unwrap();

    // Set each file's mtime to its embedded conversation timestamp. Real
    // session files have mtime ≈ conversation time; leaving mtime at write-time
    // while the embedded timestamps are historical made the date-filter code's
    // ordering non-deterministic (readdir-order fallback on equal mtimes), which
    // failed only in CI. Aligning them keeps every `--all` path deterministic.
    set_file_mtime(&project_dir, CHAIN_ROOT_ID, root_ts);
    set_file_mtime(&project_dir, CHAIN_CONT_ID, cont_ts);

    tmp
}

fn md_count(dir: &std::path::Path) -> usize {
    std::fs::read_dir(dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| {
            std::path::Path::new(&e.file_name())
                .extension()
                .is_some_and(|x| x == "md")
        })
        .count()
}

fn md_names(dir: &std::path::Path) -> Vec<String> {
    std::fs::read_dir(dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().to_string())
        .filter(|n| {
            std::path::Path::new(n)
                .extension()
                .is_some_and(|x| x == "md")
        })
        .collect()
}

#[test]
fn test_export_all_until_excludes_chain_by_latest_member() {
    // Root before the cutoff, continuation after it: the chain's latest member
    // is after --until, so the whole chain must be excluded (even though the
    // root file alone is before the cutoff).
    let tmp = setup_dated_chain_dir("2025-01-10T10:00:00.000Z", "2025-01-20T10:00:00.000Z");
    let out_dir = TempDir::new().unwrap();
    snatch_cmd()
        .env("SNATCH_CLAUDE_DIR", tmp.path())
        .args([
            "export",
            "--all",
            "--until",
            "2025-01-15",
            "-f",
            "markdown",
            "--out",
            out_dir.path().to_str().unwrap(),
        ])
        .assert()
        .success();
    assert_eq!(
        md_count(out_dir.path()),
        0,
        "chain excluded by latest member"
    );
}

#[test]
fn test_export_all_since_includes_chain_by_latest_member() {
    // Root before the cutoff, continuation after it: the chain's latest member
    // is after --since, so the chain is included and exported once using the
    // root-id filename.
    let tmp = setup_dated_chain_dir("2025-01-10T10:00:00.000Z", "2025-01-20T10:00:00.000Z");
    let out_dir = TempDir::new().unwrap();
    snatch_cmd()
        .env("SNATCH_CLAUDE_DIR", tmp.path())
        .args([
            "export",
            "--all",
            "--since",
            "2025-01-15",
            "-f",
            "markdown",
            "--out",
            out_dir.path().to_str().unwrap(),
        ])
        .assert()
        .success();
    let names = md_names(out_dir.path());
    assert_eq!(names.len(), 1, "chain exported once, got: {names:?}");
    assert!(
        names[0].contains(CHAIN_ROOT_ID),
        "filename keyed by root id"
    );
    assert!(!names[0].contains(CHAIN_CONT_ID));
}

#[test]
fn test_export_all_date_filter_uses_embedded_not_mtime() {
    // #22: chain date filtering must follow embedded conversation timestamps,
    // not file mtime. Here the root's conversation is Jan 1 but its file was
    // touched last (mtime Jan 30), while the continuation's conversation is
    // Jan 30 but its file mtime is Jan 1. Selecting the representative by mtime
    // would pick the root (embedded Jan 1) and wrongly exclude the chain from
    // `--since 2025-01-15`; selecting by embedded end keeps it (Jan 30).
    let tmp = setup_dated_chain_dir("2025-01-01T00:00:00.000Z", "2025-01-30T00:00:00.000Z");
    let encoded = encode_project_path(PROJECT_PATH);
    let project_dir = tmp.path().join("projects").join(&encoded);
    // Diverge mtime from embedded: root newest by mtime, continuation oldest.
    set_file_mtime(&project_dir, CHAIN_ROOT_ID, "2025-01-30T00:00:00.000Z");
    set_file_mtime(&project_dir, CHAIN_CONT_ID, "2025-01-01T00:00:00.000Z");

    let out_dir = TempDir::new().unwrap();
    snatch_cmd()
        .env("SNATCH_CLAUDE_DIR", tmp.path())
        .args([
            "export",
            "--all",
            "--since",
            "2025-01-15",
            "-f",
            "markdown",
            "--out",
            out_dir.path().to_str().unwrap(),
        ])
        .assert()
        .success();
    let names = md_names(out_dir.path());
    assert_eq!(
        names.len(),
        1,
        "chain kept by embedded latest activity (Jan 30), got: {names:?}"
    );
    assert!(names[0].contains(CHAIN_ROOT_ID));
}

#[test]
fn test_export_all_sqlite_date_filter_once_per_chain() {
    let tmp = setup_dated_chain_dir("2025-01-10T10:00:00.000Z", "2025-01-20T10:00:00.000Z");

    // --since before the latest member: chain included exactly once.
    let db_in = TempDir::new().unwrap();
    snatch_cmd()
        .env("SNATCH_CLAUDE_DIR", tmp.path())
        .args([
            "export",
            "--all",
            "--since",
            "2025-01-15",
            "-f",
            "sqlite",
            "--out",
            db_in.path().join("in.db").to_str().unwrap(),
        ])
        .assert()
        .success()
        .stderr(predicate::str::contains("Exported 1 sessions"));

    // --until before the latest member: chain excluded.
    let db_out = TempDir::new().unwrap();
    snatch_cmd()
        .env("SNATCH_CLAUDE_DIR", tmp.path())
        .args([
            "export",
            "--all",
            "--until",
            "2025-01-15",
            "-f",
            "sqlite",
            "--out",
            db_out.path().join("out.db").to_str().unwrap(),
        ])
        .assert()
        .success()
        .stderr(predicate::str::contains("Exported 0 sessions"));
}

/// Write a tags.json under an isolated XDG config home so the CLI subprocess
/// reads it instead of the real user store.
fn write_tags_config(config_home: &std::path::Path, json: &str) {
    let dir = config_home.join("claude-snatch");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("tags.json"), json).unwrap();
}

#[test]
fn test_list_tag_matches_continuation_member() {
    let tmp = setup_chain_dir();
    let cfg = TempDir::new().unwrap();
    write_tags_config(
        cfg.path(),
        &format!(r#"{{"version":1,"sessions":{{"{CHAIN_CONT_ID}":{{"tags":["mychaintag"]}}}}}}"#),
    );

    let output = snatch_cmd()
        .env("SNATCH_CLAUDE_DIR", tmp.path())
        .env("SNATCH_CONFIG_DIR", cfg.path())
        .args([
            "list",
            "sessions",
            "-o",
            "json",
            "--full-ids",
            "--tag",
            "mychaintag",
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let rows: serde_json::Value =
        serde_json::from_str(&String::from_utf8(output).unwrap()).unwrap();
    let arr = rows.as_array().unwrap();
    assert_eq!(
        arr.len(),
        1,
        "tag on continuation still returns the chain row"
    );
    assert_eq!(arr[0]["session_id"], CHAIN_ROOT_ID);
    // The non-root match is surfaced for JSON consumers.
    let matched = arr[0]["matched_member_ids"].as_array().unwrap();
    assert!(matched.iter().any(|m| m == CHAIN_CONT_ID));
}

#[test]
fn test_list_by_name_matches_continuation_member() {
    let tmp = setup_chain_dir();
    let cfg = TempDir::new().unwrap();
    write_tags_config(
        cfg.path(),
        &format!(r#"{{"version":1,"sessions":{{"{CHAIN_CONT_ID}":{{"name":"mychainname"}}}}}}"#),
    );

    let output = snatch_cmd()
        .env("SNATCH_CLAUDE_DIR", tmp.path())
        .env("SNATCH_CONFIG_DIR", cfg.path())
        .args([
            "list",
            "sessions",
            "-o",
            "json",
            "--full-ids",
            "--by-name",
            "mychainname",
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let rows: serde_json::Value =
        serde_json::from_str(&String::from_utf8(output).unwrap()).unwrap();
    let arr = rows.as_array().unwrap();
    assert_eq!(
        arr.len(),
        1,
        "name on continuation still returns the chain row"
    );
    assert_eq!(arr[0]["session_id"], CHAIN_ROOT_ID);
    let matched = arr[0]["matched_member_ids"].as_array().unwrap();
    assert!(matched.iter().any(|m| m == CHAIN_CONT_ID));
}

/// The active monitor (0023): a project with no recurring errors yields a clear
/// "no insights" message rather than an error.
#[test]
fn test_monitor_no_insights_is_clean() {
    let tmp = setup_fixture_dir();

    // Plain on-demand view: a clear "nothing here" message, success.
    snatch_cmd()
        .env("SNATCH_CLAUDE_DIR", tmp.path())
        .args(["monitor", "test-project"])
        .assert()
        .success()
        .stdout(predicate::str::contains("No monitor insights"));
}
