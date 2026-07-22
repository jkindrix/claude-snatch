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

#[test]
fn flagless_health_and_priorities_keep_classic_json_shapes() {
    let claude = setup_fixture_dir();
    let health = snatch_cmd()
        .env("SNATCH_CLAUDE_DIR", claude.path())
        .args(["-o", "json", "health", PROJECT_PATH])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let health: serde_json::Value = serde_json::from_slice(&health).unwrap();
    assert_eq!(health["sessions_analyzed"], 1);
    assert!(health.get("providers").is_none());
    assert_eq!(health["session_stats"][0]["session_id"], SESSION_ID);

    let priorities = snatch_cmd()
        .env("SNATCH_CLAUDE_DIR", claude.path())
        .args(["-o", "json", "priorities", PROJECT_PATH])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let priorities: serde_json::Value = serde_json::from_slice(&priorities).unwrap();
    assert_eq!(priorities["sessions_analyzed"], 1);
    assert!(priorities["open_goals"].is_number());
    assert!(priorities["proposed_decisions"].is_number());
    assert!(priorities.get("providers").is_none());
}

#[test]
fn flagless_standup_keeps_the_classic_json_shape() {
    let claude = setup_fixture_dir();
    let output = snatch_cmd()
        .env("SNATCH_CLAUDE_DIR", claude.path())
        .args(["-o", "json", "standup", "--period", "30d"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let value: serde_json::Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(value["total_sessions"], 1);
    assert!(value.get("providers").is_none());
    assert!(value.get("session_descriptors_analyzed").is_none());
    assert!(value.get("period_basis").is_none());
}

fn setup_code_fixture_dir() -> TempDir {
    let tmp = TempDir::new().expect("failed to create temp dir");
    let project_dir = tmp
        .path()
        .join("projects")
        .join(encode_project_path(PROJECT_PATH));
    std::fs::create_dir_all(&project_dir).unwrap();
    let lines = [
        serde_json::json!({
            "type": "user", "uuid": "code-user", "parentUuid": null,
            "timestamp": "2025-01-15T10:00:00Z", "sessionId": SESSION_ID,
            "version": "2.0.74", "message": {"role": "user",
                "content": "Please inspect:\n```rust\nlet user_side = true;\n```"}
        }),
        serde_json::json!({
            "type": "assistant", "uuid": "code-assistant", "parentUuid": "code-user",
            "timestamp": "2025-01-15T10:00:01Z", "sessionId": SESSION_ID,
            "version": "2.0.74", "message": {"id": "code-message", "type": "message", "role": "assistant",
                "model": "claude-sonnet-4-20250514", "usage": {"input_tokens": 1, "output_tokens": 1},
                "content": [{"type": "text", "text": "Result:\n```python\nprint('assistant')\n```"}]}
        }),
    ];
    let content = lines
        .iter()
        .map(serde_json::Value::to_string)
        .collect::<Vec<_>>()
        .join("\n")
        + "\n";
    std::fs::write(project_dir.join(format!("{SESSION_ID}.jsonl")), content).unwrap();
    tmp
}

fn setup_file_snapshot_dir() -> TempDir {
    let tmp = TempDir::new().expect("failed to create temp dir");
    let project_dir = tmp
        .path()
        .join("projects")
        .join(encode_project_path(PROJECT_PATH));
    std::fs::create_dir_all(&project_dir).unwrap();
    let snapshot = serde_json::json!({
        "type": "file-history-snapshot",
        "messageId": "snapshot-message",
        "isSnapshotUpdate": false,
        "snapshot": {
            "messageId": "snapshot-message",
            "timestamp": "2025-01-15T10:00:00Z",
            "trackedFileBackups": {
                "/work/src/lib.rs": {
                    "backupFileName": "lib.rs@v3",
                    "version": 3,
                    "backupTime": "2025-01-15T10:00:01Z"
                }
            }
        }
    });
    std::fs::write(
        project_dir.join(format!("{SESSION_ID}.jsonl")),
        format!("{snapshot}\n"),
    )
    .unwrap();
    tmp
}

/// Add a minimal searchable session to the standard test project.
fn write_search_session(tmp: &TempDir, session_id: &str, text: &str, model: Option<&str>) {
    let encoded = encode_project_path(PROJECT_PATH);
    let project_dir = tmp.path().join("projects").join(encoded);
    std::fs::create_dir_all(&project_dir).expect("failed to create project dir");

    let entry = if let Some(model) = model {
        serde_json::json!({
            "type": "assistant",
            "uuid": format!("entry-{session_id}"),
            "parentUuid": null,
            "timestamp": "2025-01-15T10:00:01.000Z",
            "sessionId": session_id,
            "version": "2.0.74",
            "message": {
                "id": format!("message-{session_id}"),
                "type": "message",
                "role": "assistant",
                "content": [{"type": "text", "text": text}],
                "model": model,
                "stop_reason": "end_turn",
                "usage": {"input_tokens": 1, "output_tokens": 1}
            }
        })
    } else {
        serde_json::json!({
            "type": "user",
            "uuid": format!("entry-{session_id}"),
            "parentUuid": null,
            "timestamp": "2025-01-15T10:00:00.000Z",
            "sessionId": session_id,
            "version": "2.0.74",
            "message": {"role": "user", "content": text}
        })
    };

    std::fs::write(
        project_dir.join(format!("{session_id}.jsonl")),
        format!("{entry}\n"),
    )
    .expect("failed to write search session");
}

#[allow(deprecated)] // cargo_bin_cmd! replacement is unstable
fn snatch_cmd() -> Command {
    Command::cargo_bin("snatch").expect("snatch binary not found")
}

fn write_index_config(tmp: &TempDir) -> (std::path::PathBuf, std::path::PathBuf) {
    let index = tmp.path().join("provider-search-index");
    let config = tmp.path().join("index-config.toml");
    let encoded_path = serde_json::to_string(&index.to_string_lossy()).unwrap();
    std::fs::write(&config, format!("[index]\ndirectory = {encoded_path}\n")).unwrap();
    (config, index)
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

#[test]
fn flagless_file_history_keeps_the_classic_json_shape() {
    let tmp = setup_file_snapshot_dir();
    let output = snatch_cmd()
        .env("SNATCH_CLAUDE_DIR", tmp.path())
        .args(["-o", "json", "file-history", "src/lib.rs"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let value: serde_json::Value = serde_json::from_slice(&output).unwrap();
    let rows = value
        .as_array()
        .expect("classic route remains a bare array");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["version"], 3);
    assert!(rows[0].get("provider").is_none());
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

#[test]
fn provider_index_cli_build_search_status_and_clear_are_snapshot_backed() {
    let claude = setup_fixture_dir();
    let config_home = TempDir::new().unwrap();
    let (config, index_path) = write_index_config(&config_home);
    let missing_codex = config_home.path().join("missing-codex");

    let empty_status = snatch_cmd()
        .args([
            "--config",
            config.to_str().unwrap(),
            "-o",
            "json",
            "index",
            "status",
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let empty_status: serde_json::Value = serde_json::from_slice(&empty_status).unwrap();
    assert_eq!(empty_status["document_count"], 0);
    assert!(
        !index_path.exists(),
        "read-only status must not create an index"
    );

    let build = snatch_cmd()
        .env("SNATCH_CLAUDE_DIR", claude.path())
        .env("CODEX_HOME", &missing_codex)
        .args([
            "--config",
            config.to_str().unwrap(),
            "-o",
            "json",
            "index",
            "build",
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let build: serde_json::Value = serde_json::from_slice(&build).unwrap();
    assert_eq!(build["sessions_replaced"], 1);
    assert_eq!(build["entries_replaced"], 6);
    assert_eq!(build["removal_coverage_complete"], true);

    let status = snatch_cmd()
        .args([
            "--config",
            config.to_str().unwrap(),
            "-o",
            "json",
            "index",
            "status",
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let status: serde_json::Value = serde_json::from_slice(&status).unwrap();
    assert_eq!(status["schema_version"], 3);
    assert_eq!(status["session_count"], 1);
    assert_eq!(status["entry_count"], 6);
    assert_eq!(status["build"]["complete_providers"][0], "claude-code");

    let indexed = snatch_cmd()
        .args([
            "--config",
            config.to_str().unwrap(),
            "-o",
            "json",
            "index",
            "search",
            "directory",
            "--session",
            "aaaaaaaa",
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let indexed: serde_json::Value = serde_json::from_slice(&indexed).unwrap();
    assert!(indexed["total_matches"].as_u64().unwrap() > 0);
    assert_eq!(indexed["sessions_matched"], 1);
    assert_eq!(
        indexed["by_session"][0]["session_key"],
        format!("claude-code:{SESSION_ID}")
    );
    assert_eq!(indexed["coverage"]["incomplete"], false);

    let count = snatch_cmd()
        .args([
            "--config",
            config.to_str().unwrap(),
            "-o",
            "json",
            "search",
            "directory",
            "--provider",
            "claude-code",
            "--count",
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let count: serde_json::Value = serde_json::from_slice(&count).unwrap();
    assert!(count["total"].as_u64().unwrap() > 0);
    assert_eq!(count["count_basis"], "occurrences");
    assert_eq!(count["coverage"]["incomplete"], false);

    snatch_cmd()
        .args([
            "--config",
            config.to_str().unwrap(),
            "search",
            "directory",
            "--session",
            &format!("claude-code:{SESSION_ID}"),
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains(format!(
            "Session: claude-code:{SESSION_ID}"
        )));

    snatch_cmd()
        .args([
            "--config",
            config.to_str().unwrap(),
            "search",
            "directory",
            "--provider",
            "claude-code",
            "--errors",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains("--errors").and(predicate::str::contains("refused")));

    snatch_cmd()
        .args(["--config", config.to_str().unwrap(), "index", "clear"])
        .assert()
        .success();
    let cleared = snatch_cmd()
        .args([
            "--config",
            config.to_str().unwrap(),
            "-o",
            "json",
            "index",
            "status",
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let cleared: serde_json::Value = serde_json::from_slice(&cleared).unwrap();
    assert_eq!(cleared["document_count"], 0);
    assert!(cleared["build"].is_null());
}

#[test]
fn provider_index_requires_explicit_rebuild_of_a_legacy_schema() {
    let claude = setup_fixture_dir();
    let config_home = TempDir::new().unwrap();
    let (config, index_path) = write_index_config(&config_home);
    let legacy = claude_snatch::index::SearchIndex::open(&index_path).unwrap();
    drop(legacy);

    snatch_cmd()
        .env("SNATCH_CLAUDE_DIR", claude.path())
        .args(["--config", config.to_str().unwrap(), "index", "build"])
        .assert()
        .failure()
        .stderr(
            predicate::str::contains("incompatible search index schema")
                .and(predicate::str::contains("index rebuild")),
        );

    snatch_cmd()
        .env("SNATCH_CLAUDE_DIR", claude.path())
        .args(["--config", config.to_str().unwrap(), "index", "rebuild"])
        .assert()
        .success();
    let replacement = claude_snatch::index::provider::ProviderSearchIndex::open(index_path)
        .expect("provider schema activated");
    assert_eq!(replacement.stats().unwrap().session_count, 1);
}

/// `--files-only` is grep-like: one very noisy session consumes one result,
/// not the entire raw-match budget. The noisy session is written last so it is
/// searched first under the newest-first discovery contract.
#[test]
fn test_search_files_only_limits_distinct_sessions_not_matches() {
    let tmp = TempDir::new().expect("temp dir");
    // The noisy ID also sorts first when filesystem timestamp resolution
    // collapses the writes into a tie (notably on some Windows filesystems).
    let noisy_id = "00000000-0000-0000-0000-000000000001";
    let quiet_id = "00000000-0000-0000-0000-000000000002";
    write_search_session(&tmp, quiet_id, "needle", None);
    std::thread::sleep(std::time::Duration::from_millis(10));
    write_search_session(&tmp, noisy_id, &"needle\n".repeat(60), None);

    let out = snatch_cmd()
        .env("SNATCH_CLAUDE_DIR", tmp.path())
        .args(["search", "--files-only", "needle", "-o", "json"])
        .output()
        .expect("files-only search failed");
    assert!(out.status.success(), "search failed: {out:?}");
    let ids: Vec<String> = serde_json::from_slice(&out.stdout).expect("JSON session-id array");
    assert_eq!(ids, vec![noisy_id, quiet_id]);
}

/// The visible limit applies to distinct sessions, truncation is reported, and
/// text/JSON preserve the same deterministic order.
#[test]
fn test_search_files_only_limit_and_order_contract() {
    let tmp = TempDir::new().expect("temp dir");
    for i in 0..51 {
        let session_id = format!("{i:08x}-0000-0000-0000-000000000000");
        write_search_session(&tmp, &session_id, "common-target", None);
    }

    let limited = snatch_cmd()
        .env("SNATCH_CLAUDE_DIR", tmp.path())
        .args([
            "search",
            "--files-only",
            "--limit",
            "50",
            "common-target",
            "-o",
            "json",
        ])
        .output()
        .expect("limited files-only search failed");
    assert!(limited.status.success(), "search failed: {limited:?}");
    let limited_ids: Vec<String> =
        serde_json::from_slice(&limited.stdout).expect("JSON session-id array");
    assert_eq!(limited_ids.len(), 50);
    assert!(String::from_utf8_lossy(&limited.stderr).contains("use --no-limit for all"));

    let zero = snatch_cmd()
        .env("SNATCH_CLAUDE_DIR", tmp.path())
        .args([
            "search",
            "--files-only",
            "--limit",
            "0",
            "common-target",
            "-o",
            "json",
        ])
        .output()
        .expect("zero-limit files-only search failed");
    assert!(zero.status.success(), "search failed: {zero:?}");
    assert_eq!(zero.stdout, b"[]\n");
    assert!(String::from_utf8_lossy(&zero.stderr).contains("use --no-limit for all"));

    let json = snatch_cmd()
        .env("SNATCH_CLAUDE_DIR", tmp.path())
        .args([
            "search",
            "--files-only",
            "--no-limit",
            "common-target",
            "-o",
            "json",
        ])
        .output()
        .expect("unbounded JSON search failed");
    assert!(json.status.success(), "search failed: {json:?}");
    let json_ids: Vec<String> =
        serde_json::from_slice(&json.stdout).expect("JSON session-id array");
    assert_eq!(json_ids.len(), 51);

    let text = snatch_cmd()
        .env("SNATCH_CLAUDE_DIR", tmp.path())
        .args(["search", "--files-only", "--no-limit", "common-target"])
        .output()
        .expect("unbounded text search failed");
    assert!(text.status.success(), "search failed: {text:?}");
    let text_ids: Vec<String> = String::from_utf8(text.stdout)
        .expect("UTF-8 text output")
        .lines()
        .map(String::from)
        .collect();
    assert_eq!(text_ids, json_ids);
}

/// Model filtering remains entry-scoped in the files-only fast path.
#[test]
fn test_search_files_only_preserves_model_filter() {
    let tmp = TempDir::new().expect("temp dir");
    let opus_id = "10000000-0000-0000-0000-000000000001";
    let sonnet_id = "10000000-0000-0000-0000-000000000002";
    write_search_session(&tmp, opus_id, "model-target", Some("claude-opus-4-8"));
    write_search_session(&tmp, sonnet_id, "model-target", Some("claude-sonnet-4-6"));

    let matching = snatch_cmd()
        .env("SNATCH_CLAUDE_DIR", tmp.path())
        .args([
            "search",
            "--files-only",
            "--model",
            "opus",
            "model-target",
            "-o",
            "json",
        ])
        .output()
        .expect("model-filtered search failed");
    assert!(matching.status.success(), "search failed: {matching:?}");
    let ids: Vec<String> = serde_json::from_slice(&matching.stdout).expect("JSON session-id array");
    assert_eq!(ids, vec![opus_id]);

    snatch_cmd()
        .env("SNATCH_CLAUDE_DIR", tmp.path())
        .args([
            "search",
            "--files-only",
            "--model",
            "definitely-not-a-model",
            "model-target",
            "-o",
            "json",
        ])
        .assert()
        .success()
        .stdout(predicate::eq("[]\n"));
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

#[test]
fn test_summary_counts_every_project_not_only_the_top_five() {
    let tmp = TempDir::new().unwrap();
    for index in 0..6_u8 {
        let project = format!("/tmp/summary-project-{index}");
        let project_dir = tmp
            .path()
            .join("projects")
            .join(encode_project_path(&project));
        std::fs::create_dir_all(&project_dir).unwrap();
        let session_id = format!("00000000-0000-0000-0000-{index:012}");
        let entry = serde_json::json!({
            "type": "user",
            "uuid": format!("summary-entry-{index}"),
            "parentUuid": null,
            "timestamp": "2026-07-22T00:00:00Z",
            "sessionId": session_id,
            "version": "2.0.74",
            "message": {"role": "user", "content": "summary fixture"}
        });
        std::fs::write(
            project_dir.join(format!("{session_id}.jsonl")),
            format!("{entry}\n"),
        )
        .unwrap();
    }

    let output = snatch_cmd()
        .env("SNATCH_CLAUDE_DIR", tmp.path())
        .args(["-o", "json", "summary", "--period", "1d"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let value: serde_json::Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(value["projects"], 6);
    assert_eq!(value["sessions"], 6);
    assert_eq!(value["top_projects"].as_array().unwrap().len(), 5);
}

#[cfg(feature = "codex")]
#[test]
fn test_provider_summary_all_reports_unavailable_providers_and_keeps_known_usage() {
    let claude = setup_fixture_dir();
    let output = snatch_cmd()
        .env("SNATCH_CLAUDE_DIR", claude.path())
        .env("CODEX_HOME", claude.path().join("missing-codex-home"))
        .args([
            "-o",
            "json",
            "summary",
            "--provider",
            "all",
            "--period",
            "24m",
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let value: serde_json::Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(value["sessions"], 1);
    assert_eq!(value["total_tokens"], 140);
    assert_eq!(value["pricing_coverage"], "complete");
    assert_eq!(value["skipped_providers"][0]["provider"], "codex");
}

#[test]
fn provider_routed_claude_stats_preserve_flagless_numeric_output() {
    let tmp = setup_fixture_dir();
    let missing_codex = tmp.path().join("no-codex-home");
    let run = |provider: bool| {
        let mut cmd = snatch_cmd();
        cmd.env("SNATCH_CLAUDE_DIR", tmp.path())
            .env("CODEX_HOME", &missing_codex)
            .args(["-o", "json", "stats", SESSION_ID]);
        if provider {
            cmd.args(["--provider", "claude-code"]);
        }
        let output = cmd.assert().success().get_output().stdout.clone();
        serde_json::from_slice::<serde_json::Value>(&output).unwrap()
    };

    let classic = run(false);
    assert!(classic.get("provider").is_none());
    assert!(classic.get("qualified_id").is_none());
    assert!(classic.get("pricing_policy").is_none());
    assert!(classic.get("unpriced_models").is_none());

    let mut routed = run(true);
    assert_eq!(routed["provider"], "claude-code");
    assert_eq!(routed["qualified_id"], format!("claude-code:{SESSION_ID}"));
    assert_eq!(routed["pricing_policy"], "known-model-rates");
    let object = routed.as_object_mut().unwrap();
    object.remove("provider");
    object.remove("qualified_id");
    object.remove("pricing_policy");
    object.remove("unpriced_models");
    assert_eq!(routed, classic);
}

#[test]
fn provider_routed_claude_prompts_and_code_preserve_classic_results() {
    let prompts_home = setup_fixture_dir();
    let missing_codex = prompts_home.path().join("no-codex-home");
    let run_prompts = |provider: bool| {
        let mut cmd = snatch_cmd();
        cmd.env("SNATCH_CLAUDE_DIR", prompts_home.path())
            .env("CODEX_HOME", &missing_codex)
            .args(["-o", "json", "prompts", SESSION_ID]);
        if provider {
            cmd.args(["--provider", "claude-code"]);
        }
        let output = cmd.assert().success().get_output().stdout.clone();
        serde_json::from_slice::<serde_json::Value>(&output).unwrap()
    };
    let classic_prompts = run_prompts(false);
    let mut routed_prompts = run_prompts(true);
    assert_eq!(routed_prompts["provider"], "claude-code");
    assert_eq!(
        routed_prompts["qualified_id"],
        format!("claude-code:{SESSION_ID}")
    );
    routed_prompts.as_object_mut().unwrap().remove("provider");
    routed_prompts
        .as_object_mut()
        .unwrap()
        .remove("qualified_id");
    assert_eq!(routed_prompts, classic_prompts);

    let code_home = setup_code_fixture_dir();
    let missing_codex = code_home.path().join("no-codex-home");
    let run_code = |provider: bool| {
        let mut cmd = snatch_cmd();
        cmd.env("SNATCH_CLAUDE_DIR", code_home.path())
            .env("CODEX_HOME", &missing_codex)
            .args(["-o", "json", "code", SESSION_ID]);
        if provider {
            cmd.args(["--provider", "claude-code"]);
        }
        let output = cmd.assert().success().get_output().stdout.clone();
        serde_json::from_slice::<serde_json::Value>(&output).unwrap()
    };
    let classic_code = run_code(false);
    assert_eq!(
        classic_code.as_array().unwrap().len(),
        2,
        "{classic_code:#}"
    );
    let mut routed_code = run_code(true);
    for block in routed_code.as_array_mut().unwrap() {
        assert_eq!(block["provider"], "claude-code");
        assert_eq!(block["qualified_id"], format!("claude-code:{SESSION_ID}"));
        block.as_object_mut().unwrap().remove("provider");
        block.as_object_mut().unwrap().remove("qualified_id");
    }
    assert_eq!(routed_code, classic_code);
}

#[test]
fn provider_routed_claude_prompt_union_preserves_classic_content() {
    let home = setup_fixture_dir();
    let missing_codex = home.path().join("no-codex-home");
    let run = |provider: bool| {
        let mut cmd = snatch_cmd();
        cmd.env("SNATCH_CLAUDE_DIR", home.path())
            .env("CODEX_HOME", &missing_codex)
            .args(["-o", "json", "prompts", "--all"]);
        if provider {
            cmd.args(["--provider", "claude-code"]);
        }
        let output = cmd.assert().success().get_output().stdout.clone();
        serde_json::from_slice::<serde_json::Value>(&output).unwrap()
    };

    let classic = run(false);
    let mut routed = run(true);
    assert_eq!(routed["providers"], serde_json::json!(["claude-code"]));
    assert_eq!(routed["session_descriptors_analyzed"], 1);
    for prompt in routed["prompts"].as_array_mut().unwrap() {
        let object = prompt.as_object_mut().unwrap();
        object.remove("provider");
        object.remove("qualified_id");
        object.remove("project_key");
        let native = object["session_id"]
            .as_str()
            .unwrap()
            .strip_prefix("claude-code:")
            .unwrap()
            .to_string();
        object.insert("session_id".to_string(), serde_json::json!(native));
    }
    let object = routed.as_object_mut().unwrap();
    object.remove("session_descriptors_analyzed");
    object.remove("date_filter_fallback_descriptors");
    object.remove("providers");
    object.remove("skipped_providers");
    object.remove("warnings");
    assert_eq!(routed, classic);
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
fn test_provider_recent_collapses_typed_continuations_and_no_chain_is_flat() {
    let tmp = setup_chain_dir();
    let output = snatch_cmd()
        .env("SNATCH_CLAUDE_DIR", tmp.path())
        .args([
            "recent",
            "-o",
            "json",
            "--provider",
            "claude-code",
            "-n",
            "10",
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let value: serde_json::Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(value["total"], 1);
    assert_eq!(value["sessions"][0]["provider"], "claude-code");
    assert_eq!(
        value["sessions"][0]["qualified_id"],
        format!("claude-code:{CHAIN_ROOT_ID}")
    );
    assert_eq!(value["sessions"][0]["continuation_member_count"], 2);
    assert_eq!(
        value["sessions"][0]["continuation_members"],
        serde_json::json!([
            format!("claude-code:{CHAIN_ROOT_ID}"),
            format!("claude-code:{CHAIN_CONT_ID}"),
        ])
    );

    let output = snatch_cmd()
        .env("SNATCH_CLAUDE_DIR", tmp.path())
        .args([
            "recent",
            "-o",
            "json",
            "--provider",
            "claude-code",
            "--no-chain",
            "-n",
            "10",
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let value: serde_json::Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(value["total"], 2);
    assert!(value["sessions"]
        .as_array()
        .unwrap()
        .iter()
        .all(|row| row["continuation_member_count"] == 1));
}

#[cfg(feature = "codex")]
#[test]
fn test_provider_recent_all_reports_unavailable_providers_without_dropping_successes() {
    let claude = setup_fixture_dir();
    let output = snatch_cmd()
        .env("SNATCH_CLAUDE_DIR", claude.path())
        .env("CODEX_HOME", claude.path().join("missing-codex-home"))
        .args(["recent", "-o", "json", "--provider", "all", "-n", "10"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let value: serde_json::Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(value["total"], 1);
    assert_eq!(value["sessions"][0]["provider"], "claude-code");
    assert_eq!(value["skipped_providers"][0]["provider"], "codex");
    assert!(value["skipped_providers"][0]["reason"]
        .as_str()
        .unwrap()
        .contains("not found"));
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

// =============================================================================
// chunks / prompt-boundary retrieval
// =============================================================================

const QUEUED_SESSION_ID: &str = "bbbbbbbb-cccc-dddd-eeee-ffffffffffff";

/// Create a temp Claude dir with a session containing a queued steering
/// prompt: user -> assistant -> queued_command attachment (human) -> assistant.
fn setup_queued_fixture_dir() -> TempDir {
    let tmp = TempDir::new().expect("failed to create temp dir");
    let encoded = encode_project_path(PROJECT_PATH);
    let project_dir = tmp.path().join("projects").join(&encoded);
    std::fs::create_dir_all(&project_dir).expect("failed to create project dir");

    let lines: &[&str] = &[
        r#"{"type":"user","uuid":"11111111-1111-1111-1111-111111111111","parentUuid":null,"timestamp":"2025-01-15T10:00:00.000Z","sessionId":"SESSID","version":"2.0.74","message":{"role":"user","content":"Start the refactor"}}"#,
        r#"{"type":"assistant","uuid":"22222222-2222-2222-2222-222222222222","parentUuid":"11111111-1111-1111-1111-111111111111","timestamp":"2025-01-15T10:00:01.000Z","sessionId":"SESSID","version":"2.0.74","message":{"id":"msg_001","type":"message","role":"assistant","content":[{"type":"text","text":"Working on it."}],"model":"claude-sonnet-4-20250514","stop_reason":"end_turn","usage":{"input_tokens":10,"output_tokens":15,"cache_creation_input_tokens":0,"cache_read_input_tokens":0}}}"#,
        r#"{"type":"attachment","uuid":"33333333-3333-3333-3333-333333333333","parentUuid":"22222222-2222-2222-2222-222222222222","timestamp":"2025-01-15T10:00:30.000Z","sessionId":"SESSID","isSidechain":false,"attachment":{"type":"queued_command","commandMode":"prompt","origin":{"kind":"human"},"prompt":"actually use the builder pattern instead"}}"#,
        r#"{"type":"assistant","uuid":"44444444-4444-4444-4444-444444444444","parentUuid":"33333333-3333-3333-3333-333333333333","timestamp":"2025-01-15T10:00:31.000Z","sessionId":"SESSID","version":"2.0.74","message":{"id":"msg_002","type":"message","role":"assistant","content":[{"type":"text","text":"Switching to the builder pattern."}],"model":"claude-sonnet-4-20250514","stop_reason":"end_turn","usage":{"input_tokens":20,"output_tokens":10,"cache_creation_input_tokens":0,"cache_read_input_tokens":0}}}"#,
    ];
    let jsonl = lines.join("\n").replace("SESSID", QUEUED_SESSION_ID) + "\n";
    let session_file = project_dir.join(format!("{QUEUED_SESSION_ID}.jsonl"));
    std::fs::write(&session_file, jsonl).expect("failed to write fixture");
    tmp
}

/// A queued steering prompt is a chunk boundary, and `chunks -o json` exposes
/// it via the public `prompt_source` field (regression guard for the API).
#[test]
fn test_chunks_json_marks_queued_prompt_source() {
    let tmp = setup_queued_fixture_dir();
    let output = snatch_cmd()
        .env("SNATCH_CLAUDE_DIR", tmp.path())
        .args(["chunks", QUEUED_SESSION_ID, "-o", "json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let json: serde_json::Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(json["total_chunks"], 2);
    assert!(json.get("provider").is_none());
    assert!(json.get("qualified_id").is_none());
    let chunks = json["chunks"].as_array().unwrap();
    assert_eq!(chunks[0]["prompt_source"], "user");
    assert_eq!(chunks[1]["prompt_source"], "queued");
    assert_eq!(
        chunks[1]["prompt"],
        "actually use the builder pattern instead"
    );
}

/// Text-mode pagination header stays honest when some paginated entries have
/// nothing to render at the chosen detail level (here: the bare tool_result
/// user entry at full detail). Regression guard for the "showing 1-5 but only
/// 3 rows" bug.
#[test]
fn test_messages_header_reports_unrenderable_entries() {
    let tmp = setup_fixture_dir();
    snatch_cmd()
        .env("SNATCH_CLAUDE_DIR", tmp.path())
        .args(["messages", SESSION_ID, "-D", "full", "-l", "6"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "5 rendered, 1 with no content at this detail",
        ));
}

const ERROR_SESSION_ID: &str = "cccccccc-dddd-eeee-ffff-000000000000";

/// Create a temp Claude dir with a session containing one failed tool call
/// among successful ones.
fn setup_error_fixture_dir() -> TempDir {
    let tmp = TempDir::new().expect("failed to create temp dir");
    let encoded = encode_project_path(PROJECT_PATH);
    let project_dir = tmp.path().join("projects").join(&encoded);
    std::fs::create_dir_all(&project_dir).expect("failed to create project dir");

    let lines: &[&str] = &[
        r#"{"type":"user","uuid":"11111111-1111-1111-1111-111111111111","parentUuid":null,"timestamp":"2025-01-15T10:00:00.000Z","sessionId":"SESSID","version":"2.0.74","message":{"role":"user","content":"Run the build"}}"#,
        r#"{"type":"assistant","uuid":"22222222-2222-2222-2222-222222222222","parentUuid":"11111111-1111-1111-1111-111111111111","timestamp":"2025-01-15T10:00:01.000Z","sessionId":"SESSID","version":"2.0.74","message":{"id":"msg_001","type":"message","role":"assistant","content":[{"type":"tool_use","id":"toolu_bad","name":"Bash","input":{"command":"cargo buidl"}}],"model":"claude-sonnet-4-20250514","stop_reason":"tool_use","usage":{"input_tokens":10,"output_tokens":10,"cache_creation_input_tokens":0,"cache_read_input_tokens":0}}}"#,
        r#"{"type":"user","uuid":"33333333-3333-3333-3333-333333333333","parentUuid":"22222222-2222-2222-2222-222222222222","timestamp":"2025-01-15T10:00:02.000Z","sessionId":"SESSID","version":"2.0.74","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"toolu_bad","is_error":true,"content":"error: no such command: buidl"}]}}"#,
        r#"{"type":"assistant","uuid":"44444444-4444-4444-4444-444444444444","parentUuid":"33333333-3333-3333-3333-333333333333","timestamp":"2025-01-15T10:00:03.000Z","sessionId":"SESSID","version":"2.0.74","message":{"id":"msg_002","type":"message","role":"assistant","content":[{"type":"tool_use","id":"toolu_ok","name":"Bash","input":{"command":"cargo build"}}],"model":"claude-sonnet-4-20250514","stop_reason":"tool_use","usage":{"input_tokens":20,"output_tokens":10,"cache_creation_input_tokens":0,"cache_read_input_tokens":0}}}"#,
        r#"{"type":"user","uuid":"55555555-5555-5555-5555-555555555555","parentUuid":"44444444-4444-4444-4444-444444444444","timestamp":"2025-01-15T10:00:04.000Z","sessionId":"SESSID","version":"2.0.74","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"toolu_ok","content":"Finished dev profile"}]}}"#,
        r#"{"type":"assistant","uuid":"66666666-6666-6666-6666-666666666666","parentUuid":"55555555-5555-5555-5555-555555555555","timestamp":"2025-01-15T10:00:05.000Z","sessionId":"SESSID","version":"2.0.74","message":{"id":"msg_003","type":"message","role":"assistant","content":[{"type":"text","text":"Build succeeded after fixing the typo."}],"model":"claude-sonnet-4-20250514","stop_reason":"end_turn","usage":{"input_tokens":30,"output_tokens":15,"cache_creation_input_tokens":0,"cache_read_input_tokens":0}}}"#,
    ];
    let jsonl = lines.join("\n").replace("SESSID", ERROR_SESSION_ID) + "\n";
    std::fs::write(project_dir.join(format!("{ERROR_SESSION_ID}.jsonl")), jsonl)
        .expect("failed to write fixture");
    tmp
}

/// --errors-only keeps the failing call (the command) and its failed result,
/// dropping the successful retry — and the chunks listing counts the error.
#[test]
fn test_errors_only_keeps_failing_call_and_chunks_counts_it() {
    let tmp = setup_error_fixture_dir();
    let output = snatch_cmd()
        .env("SNATCH_CLAUDE_DIR", tmp.path())
        .args(["messages", ERROR_SESSION_ID, "--errors-only", "-D", "full"])
        .assert()
        .success()
        .stdout(predicate::str::contains("cargo buidl"))
        .stdout(predicate::str::contains("no such command: buidl"))
        .get_output()
        .stdout
        .clone();
    let text = String::from_utf8_lossy(&output);
    assert!(
        !text.contains("cargo build\n"),
        "successful retry must be filtered out"
    );

    snatch_cmd()
        .env("SNATCH_CLAUDE_DIR", tmp.path())
        .args(["chunks", ERROR_SESSION_ID])
        .assert()
        .success()
        .stdout(predicate::str::contains("⚠1 errors"));
}

const BIG_CHUNK_SESSION_ID: &str = "dddddddd-eeee-ffff-0000-111111111111";

/// Create a temp Claude dir with one prompt followed by 60 assistant
/// entries — a single chunk larger than the default pagination limit.
fn setup_big_chunk_fixture_dir() -> TempDir {
    let tmp = TempDir::new().expect("failed to create temp dir");
    let encoded = encode_project_path(PROJECT_PATH);
    let project_dir = tmp.path().join("projects").join(&encoded);
    std::fs::create_dir_all(&project_dir).expect("failed to create project dir");

    let mut lines = vec![format!(
        r#"{{"type":"user","uuid":"u0","parentUuid":null,"timestamp":"2025-01-15T10:00:00.000Z","sessionId":"{BIG_CHUNK_SESSION_ID}","version":"2.0.74","message":{{"role":"user","content":"Do a long thing"}}}}"#
    )];
    for i in 0..60 {
        let parent = if i == 0 {
            "u0".to_string()
        } else {
            format!("a{}", i - 1)
        };
        lines.push(format!(
            r#"{{"type":"assistant","uuid":"a{i}","parentUuid":"{parent}","timestamp":"2025-01-15T10:00:{:02}.000Z","sessionId":"{BIG_CHUNK_SESSION_ID}","version":"2.0.74","message":{{"id":"m{i}","type":"message","role":"assistant","content":[{{"type":"text","text":"step {i}"}}],"model":"claude-sonnet-4-20250514","stop_reason":"end_turn","usage":{{"input_tokens":1,"output_tokens":1,"cache_creation_input_tokens":0,"cache_read_input_tokens":0}}}}}}"#,
            (i + 1).min(59)
        ));
    }
    let jsonl = lines.join("\n") + "\n";
    std::fs::write(
        project_dir.join(format!("{BIG_CHUNK_SESSION_ID}.jsonl")),
        jsonl,
    )
    .expect("failed to write fixture");
    tmp
}

/// A chunk request returns the whole chunk by default — the generic limit of
/// 50 must not silently truncate it. An explicit --limit still paginates.
#[test]
fn test_messages_chunk_defaults_to_unlimited() {
    let tmp = setup_big_chunk_fixture_dir();
    snatch_cmd()
        .env("SNATCH_CLAUDE_DIR", tmp.path())
        .args([
            "messages",
            BIG_CHUNK_SESSION_ID,
            "--chunk",
            "0",
            "-D",
            "full",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("showing 1-61"));

    snatch_cmd()
        .env("SNATCH_CLAUDE_DIR", tmp.path())
        .args([
            "messages",
            BIG_CHUNK_SESSION_ID,
            "--chunk",
            "0",
            "-l",
            "5",
            "-D",
            "full",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("showing 1-5"));
}

/// `-l 0` means unlimited (matching `list -n 0`), not "take zero".
#[test]
fn test_messages_limit_zero_is_unlimited() {
    let tmp = setup_fixture_dir();
    snatch_cmd()
        .env("SNATCH_CLAUDE_DIR", tmp.path())
        .args(["messages", SESSION_ID, "-l", "0", "-D", "full"])
        .assert()
        .success()
        .stdout(predicate::str::contains("showing 1-6"));
}

// =============================================================================
// providers
// =============================================================================

#[test]
fn test_providers_reports_claude_code_with_root_and_count() {
    let tmp = setup_fixture_dir();
    snatch_cmd()
        .env("SNATCH_CLAUDE_DIR", tmp.path())
        .env("CODEX_HOME", tmp.path().join("no-such-codex-home"))
        .arg("providers")
        .assert()
        .success()
        .stdout(predicate::str::contains("claude-code"))
        .stdout(predicate::str::contains("status: available"))
        .stdout(predicate::str::contains("sessions: 1"));
}

#[cfg(feature = "codex")]
#[test]
fn test_providers_missing_codex_home_is_visible_not_dropped() {
    let tmp = setup_fixture_dir();
    let out = snatch_cmd()
        .env("SNATCH_CLAUDE_DIR", tmp.path())
        .env("CODEX_HOME", tmp.path().join("no-such-codex-home"))
        .args(["-o", "json", "providers"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let reports: serde_json::Value =
        serde_json::from_slice(&out).expect("providers JSON must parse");
    let reports = reports.as_array().expect("array of providers");
    // Deterministic id order: claude-code before codex.
    let ids: Vec<&str> = reports
        .iter()
        .map(|r| r["provider"].as_str().unwrap())
        .collect();
    assert_eq!(ids, ["claude-code", "codex"]);
    let codex = &reports[1];
    assert_eq!(codex["available"], false);
    assert!(
        codex["unavailable_reason"]
            .as_str()
            .expect("unavailable provider carries a reason")
            .contains("not found"),
        "reason should say the home was not found: {codex}"
    );
}

// =============================================================================
// provider-routed list / info / export (B2)
// =============================================================================

#[test]
fn test_provider_flag_unknown_provider_is_refused_with_known_set() {
    let tmp = setup_fixture_dir();
    snatch_cmd()
        .env("SNATCH_CLAUDE_DIR", tmp.path())
        .args(["list", "sessions", "--provider", "gemini"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("gemini"))
        .stderr(predicate::str::contains("claude-code"));
}

#[test]
fn test_provider_all_mixed_with_explicit_is_refused() {
    let tmp = setup_fixture_dir();
    snatch_cmd()
        .env("SNATCH_CLAUDE_DIR", tmp.path())
        .args([
            "list",
            "sessions",
            "--provider",
            "all",
            "--provider",
            "claude-code",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains("cannot be combined"));
}

#[cfg(feature = "codex")]
mod codex_provider_cli {
    use super::*;

    const CODEX_THREAD: &str = "0198aaaa-bbbb-7ccc-8ddd-eeeeffff0001";

    /// Minimal real-shape codex home: one envelope session.
    fn setup_codex_home_with_cwd(cwd: &str) -> (TempDir, String) {
        let tmp = TempDir::new().expect("tempdir");
        let day = tmp.path().join("sessions/2026/07/16");
        std::fs::create_dir_all(&day).unwrap();
        let content = format!(
            concat!(
                "{{\"timestamp\":\"2026-07-16T10:00:00.000Z\",\"type\":\"session_meta\",",
                "\"payload\":{{\"id\":\"{id}\",\"cwd\":\"{cwd}\"}}}}\n",
                "{{\"timestamp\":\"2026-07-16T10:00:01.000Z\",\"type\":\"response_item\",",
                "\"payload\":{{\"type\":\"message\",\"role\":\"user\",",
                "\"content\":[{{\"type\":\"input_text\",\"text\":\"hello codex user@example.com\"}}]}}}}\n",
                "{{\"timestamp\":\"2026-07-16T10:00:01.000Z\",\"type\":\"event_msg\",",
                "\"payload\":{{\"type\":\"user_message\",\"message\":\"hello codex user@example.com\",",
                "\"images\":[],\"local_images\":[],\"text_elements\":[]}}}}\n",
            ),
            id = CODEX_THREAD,
            cwd = cwd,
        );
        std::fs::write(
            day.join(format!("rollout-2026-07-16T10-00-00-{CODEX_THREAD}.jsonl")),
            &content,
        )
        .unwrap();
        (tmp, content)
    }

    fn setup_codex_home() -> (TempDir, String) {
        setup_codex_home_with_cwd("/tmp/p")
    }

    #[test]
    fn list_provider_codex_shows_qualified_ids() {
        let claude = setup_fixture_dir();
        let (codex, _) = setup_codex_home();
        snatch_cmd()
            .env("SNATCH_CLAUDE_DIR", claude.path())
            .env("CODEX_HOME", codex.path())
            .args(["list", "sessions", "--provider", "codex"])
            .assert()
            .success()
            .stdout(predicate::str::contains(format!("codex:{CODEX_THREAD}")));
    }

    #[test]
    fn provider_index_all_build_and_search_span_both_providers() {
        let claude = setup_fixture_dir();
        let (codex, _) = setup_codex_home();
        let config_home = TempDir::new().unwrap();
        let (config, _) = write_index_config(&config_home);

        let build = snatch_cmd()
            .env("SNATCH_CLAUDE_DIR", claude.path())
            .env("CODEX_HOME", codex.path())
            .args([
                "--config",
                config.to_str().unwrap(),
                "-o",
                "json",
                "index",
                "build",
                "--provider",
                "all",
            ])
            .assert()
            .success()
            .get_output()
            .stdout
            .clone();
        let build: serde_json::Value = serde_json::from_slice(&build).unwrap();
        assert_eq!(build["sessions_replaced"], 2);
        assert_eq!(build["skipped"], 0);
        assert_eq!(build["removal_coverage_complete"], true);

        let result = snatch_cmd()
            .args([
                "--config",
                config.to_str().unwrap(),
                "-o",
                "json",
                "search",
                "hello",
                "--ignore-case",
                "--provider",
                "all",
            ])
            .assert()
            .success()
            .get_output()
            .stdout
            .clone();
        let result: serde_json::Value = serde_json::from_slice(&result).unwrap();
        assert_eq!(result["sessions_matched"], 2);
        assert_eq!(
            result["coverage"]["searched_providers"],
            serde_json::json!(["claude-code", "codex"])
        );
        assert_eq!(result["coverage"]["incomplete"], false);
        let providers: std::collections::BTreeSet<_> = result["matches"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|row| row["provider"].as_str())
            .collect();
        assert_eq!(
            providers,
            std::collections::BTreeSet::from(["claude-code", "codex"])
        );

        snatch_cmd()
            .args([
                "--config",
                config.to_str().unwrap(),
                "index",
                "search",
                "hello",
                "--provider",
                "codex",
                "--session",
                "0198aaaa",
            ])
            .assert()
            .success()
            .stdout(predicate::str::contains(format!(
                "Session: codex:{CODEX_THREAD}"
            )));
    }

    #[test]
    fn digest_and_thread_route_qualified_codex_sessions() {
        let claude = setup_fixture_dir();
        let (codex, _) = setup_codex_home();
        let target = format!("codex:{CODEX_THREAD}");

        let digest = snatch_cmd()
            .env("SNATCH_CLAUDE_DIR", claude.path())
            .env("CODEX_HOME", codex.path())
            .args(["-o", "json", "digest", &target])
            .assert()
            .success()
            .get_output()
            .stdout
            .clone();
        let digest: serde_json::Value = serde_json::from_slice(&digest).unwrap();
        assert_eq!(digest["provider"], "codex");
        assert_eq!(digest["qualified_id"], target);
        assert_eq!(digest["total_prompts"], 1);
        assert!(digest.get("formatted").is_none());

        let thread = snatch_cmd()
            .env("SNATCH_CLAUDE_DIR", claude.path())
            .env("CODEX_HOME", codex.path())
            .args(["-o", "json", "thread", "hello codex", "--session", &target])
            .assert()
            .success()
            .get_output()
            .stdout
            .clone();
        let thread: serde_json::Value = serde_json::from_slice(&thread).unwrap();
        assert_eq!(thread[0]["provider"], "codex");
        assert_eq!(thread[0]["qualified_id"], target);
        assert_eq!(thread[0]["match_provenance"], "primary");

        let no_match = snatch_cmd()
            .env("SNATCH_CLAUDE_DIR", claude.path())
            .env("CODEX_HOME", codex.path())
            .args([
                "-o",
                "json",
                "thread",
                "definitely-not-in-the-session",
                "--session",
                &target,
            ])
            .assert()
            .success()
            .get_output()
            .stdout
            .clone();
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&no_match).unwrap(),
            serde_json::json!([])
        );
    }

    #[test]
    fn list_provider_all_spans_both_providers() {
        let claude = setup_fixture_dir();
        let (codex, _) = setup_codex_home();
        snatch_cmd()
            .env("SNATCH_CLAUDE_DIR", claude.path())
            .env("CODEX_HOME", codex.path())
            .args(["list", "sessions", "--provider", "all"])
            .assert()
            .success()
            .stdout(predicate::str::contains(format!("codex:{CODEX_THREAD}")))
            .stdout(predicate::str::contains(format!(
                "claude-code:{SESSION_ID}"
            )));
    }

    #[test]
    fn list_provider_projects_unifies_same_cwd_and_filters_the_session_union() {
        let claude = setup_fixture_dir();
        let (codex, _) = setup_codex_home_with_cwd(PROJECT_PATH);
        let output = snatch_cmd()
            .env("SNATCH_CLAUDE_DIR", claude.path())
            .env("CODEX_HOME", codex.path())
            .args(["-o", "json", "list", "projects", "--provider", "all"])
            .assert()
            .success()
            .get_output()
            .stdout
            .clone();
        let value: serde_json::Value = serde_json::from_slice(&output).unwrap();
        assert_eq!(value["total"], 1);
        assert_eq!(value["projects"][0]["session_count"], 2);
        assert_eq!(
            value["projects"][0]["providers"],
            serde_json::json!(["claude-code", "codex"])
        );
        assert_eq!(value["projects"][0]["path"], PROJECT_PATH);

        let sessions = snatch_cmd()
            .env("SNATCH_CLAUDE_DIR", claude.path())
            .env("CODEX_HOME", codex.path())
            .args([
                "-o",
                "json",
                "list",
                "sessions",
                "--provider",
                "all",
                "--project",
                "test-project",
            ])
            .assert()
            .success()
            .get_output()
            .stdout
            .clone();
        let value: serde_json::Value = serde_json::from_slice(&sessions).unwrap();
        assert_eq!(value["total"], 2);
        let providers: std::collections::BTreeSet<_> = value["sessions"]
            .as_array()
            .unwrap()
            .iter()
            .map(|session| session["provider"].as_str().unwrap())
            .collect();
        assert_eq!(providers, ["claude-code", "codex"].into_iter().collect());
    }

    #[test]
    fn recent_provider_all_uses_the_unified_project_and_qualified_ids() {
        let claude = setup_fixture_dir();
        let (codex, _) = setup_codex_home_with_cwd(PROJECT_PATH);
        let output = snatch_cmd()
            .env("SNATCH_CLAUDE_DIR", claude.path())
            .env("CODEX_HOME", codex.path())
            .args([
                "-o",
                "json",
                "recent",
                "--provider",
                "all",
                "--project",
                "test-project",
                "-n",
                "10",
            ])
            .assert()
            .success()
            .get_output()
            .stdout
            .clone();
        let value: serde_json::Value = serde_json::from_slice(&output).unwrap();
        assert_eq!(value["total"], 2);
        assert_eq!(value["skipped_providers"], serde_json::json!([]));
        assert_eq!(value["warnings"], serde_json::json!([]));
        let rows = value["sessions"].as_array().unwrap();
        let providers: std::collections::BTreeSet<_> = rows
            .iter()
            .map(|row| row["provider"].as_str().unwrap())
            .collect();
        assert_eq!(providers, ["claude-code", "codex"].into_iter().collect());
        assert!(rows.iter().all(|row| {
            row["qualified_id"]
                .as_str()
                .is_some_and(|id| id.starts_with(row["provider"].as_str().unwrap()))
                && row["project"] == PROJECT_PATH
                && row["continuation_member_count"] == 1
        }));
    }

    #[test]
    fn info_resolves_qualified_prefix_without_provider_flag() {
        let claude = setup_fixture_dir();
        let (codex, _) = setup_codex_home();
        snatch_cmd()
            .env("SNATCH_CLAUDE_DIR", claude.path())
            .env("CODEX_HOME", codex.path())
            .args(["info", "codex:0198aaaa"])
            .assert()
            .success()
            .stdout(predicate::str::contains(format!("codex:{CODEX_THREAD}")))
            .stdout(predicate::str::contains("Provider: codex"))
            .stdout(predicate::str::contains("Entries: 2"))
            // Provenance survives into the production consumer (round-18);
            // B3 slice 1 maps the user message and preserves session_meta.
            .stdout(predicate::str::contains(
                "Record dispositions: mapped 1, suppressed 1, unknown 1, recovered 0, unparseable 0",
            ));
    }

    #[test]
    fn qualified_id_outside_explicit_selection_is_refused() {
        let claude = setup_fixture_dir();
        let (codex, _) = setup_codex_home();
        snatch_cmd()
            .env("SNATCH_CLAUDE_DIR", claude.path())
            .env("CODEX_HOME", codex.path())
            .args(["info", "codex:0198aaaa", "--provider", "claude-code"])
            .assert()
            .failure()
            .stderr(predicate::str::contains(
                "outside the current provider selection",
            ));
    }

    #[test]
    fn export_raw_jsonl_is_byte_identical_to_source() {
        let claude = setup_fixture_dir();
        let (codex, content) = setup_codex_home();
        let out = snatch_cmd()
            .env("SNATCH_CLAUDE_DIR", claude.path())
            .env("CODEX_HOME", codex.path())
            .args([
                "export",
                &format!("codex:{CODEX_THREAD}"),
                "-f",
                "raw-jsonl",
            ])
            .assert()
            .success()
            .get_output()
            .stdout
            .clone();
        assert_eq!(String::from_utf8_lossy(&out), content);
    }

    #[test]
    fn export_native_streams_exact_artifact_bytes() {
        let claude = setup_fixture_dir();
        let (codex, content) = setup_codex_home();
        let out = snatch_cmd()
            .env("SNATCH_CLAUDE_DIR", claude.path())
            .env("CODEX_HOME", codex.path())
            .args(["export", &format!("codex:{CODEX_THREAD}"), "-f", "native"])
            .assert()
            .success()
            .get_output()
            .stdout
            .clone();
        assert_eq!(out, content.as_bytes());
    }

    #[test]
    fn export_archive_carries_manifest() {
        let claude = setup_fixture_dir();
        let (codex, _) = setup_codex_home();
        snatch_cmd()
            .env("SNATCH_CLAUDE_DIR", claude.path())
            .env("CODEX_HOME", codex.path())
            .args(["export", &format!("codex:{CODEX_THREAD}"), "-f", "archive"])
            .assert()
            .success()
            .stdout(predicate::str::contains("\"provider\":\"codex\""));
    }

    #[test]
    fn max_file_size_reaches_codex_end_to_end() {
        // Round-19 blocker 2, end to end through the CLI: omitted and zero
        // limits keep the default caps (session parses); a small nonzero
        // limit tightens them (parse refused).
        let claude = setup_fixture_dir();
        let (codex, _) = setup_codex_home();
        let target = format!("codex:{CODEX_THREAD}");
        for limit in [None, Some("0")] {
            let mut cmd = snatch_cmd();
            cmd.env("SNATCH_CLAUDE_DIR", claude.path())
                .env("CODEX_HOME", codex.path());
            if let Some(l) = limit {
                cmd.args(["--max-file-size", l]);
            }
            cmd.args(["info", &target])
                .assert()
                .success()
                .stdout(predicate::str::contains("Entries: 2"));
        }
        snatch_cmd()
            .env("SNATCH_CLAUDE_DIR", claude.path())
            .env("CODEX_HOME", codex.path())
            .args(["--max-file-size", "10", "info", &target])
            .assert()
            .failure()
            .stderr(predicate::str::contains("size limit"));
    }

    #[test]
    fn normalized_exports_render_codex_and_honor_redaction() {
        let claude = setup_fixture_dir();
        let (codex, _) = setup_codex_home();
        let target = format!("codex:{CODEX_THREAD}");
        for format in [
            "markdown",
            "json",
            "json-pretty",
            "text",
            "jsonl",
            "csv",
            "html",
        ] {
            snatch_cmd()
                .env("SNATCH_CLAUDE_DIR", claude.path())
                .env("CODEX_HOME", codex.path())
                .args(["export", &target, "-f", format])
                .assert()
                .success()
                .stdout(predicate::str::contains("hello codex"));
        }

        snatch_cmd()
            .env("SNATCH_CLAUDE_DIR", claude.path())
            .env("CODEX_HOME", codex.path())
            .args(["export", &target, "-f", "markdown", "--redact", "all"])
            .assert()
            .success()
            .stdout(predicate::str::contains("hello codex"))
            .stdout(predicate::str::contains("user@example.com").not());

        let db = codex.path().join("normalized.db");
        snatch_cmd()
            .env("SNATCH_CLAUDE_DIR", claude.path())
            .env("CODEX_HOME", codex.path())
            .args([
                "export",
                &target,
                "-f",
                "sqlite",
                "-O",
                db.to_str().unwrap(),
            ])
            .assert()
            .success();
        let connection = rusqlite::Connection::open(&db).unwrap();
        let messages: i64 = connection
            .query_row("SELECT COUNT(*) FROM entries", [], |row| row.get(0))
            .unwrap();
        assert!(messages > 0, "SQLite normalized export contains entries");
    }

    #[test]
    fn normalized_json_exports_machine_readable_derivation_without_source_paths() {
        let claude = setup_fixture_dir();
        let (codex, _) = setup_codex_home();
        let target = format!("codex:{CODEX_THREAD}");

        let json = snatch_cmd()
            .env("SNATCH_CLAUDE_DIR", claude.path())
            .env("CODEX_HOME", codex.path())
            .args(["export", &target, "-f", "json"])
            .assert()
            .success()
            .get_output()
            .stdout
            .clone();
        let json_text = String::from_utf8(json).unwrap();
        assert!(
            !json_text.contains(&codex.path().to_string_lossy().to_string()),
            "normalized provenance must not leak the source locator"
        );
        let value: serde_json::Value = serde_json::from_str(&json_text).unwrap();
        let provider = &value["provider"];
        assert_eq!(provider["format_version"], 1);
        assert_eq!(provider["id"], "codex");
        assert_eq!(provider["qualified_session_id"], target);
        assert_eq!(provider["unidentified_entry_count"], 0);
        let fields: Vec<_> = provider["field_derivations"]
            .as_array()
            .unwrap()
            .iter()
            .map(|item| item["field"].as_str().unwrap())
            .collect();
        assert_eq!(
            fields,
            ["uuid", "parentUuid", "logicalParentUuid", "message.id"]
        );
        let derivations = provider["entries"].as_array().unwrap();
        assert_eq!(
            derivations.len(),
            value["entries"].as_array().unwrap().len()
        );
        assert!(derivations.iter().all(|entry| {
            entry["entry_id"].as_str().is_some()
                && entry["origins"]
                    .as_array()
                    .is_some_and(|origins| !origins.is_empty())
        }));
        assert!(derivations
            .iter()
            .flat_map(|entry| {
                entry["origins"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|origin| origin["artifact"].as_str().unwrap())
            })
            .all(|artifact| artifact.starts_with("artifact-")));

        let jsonl = snatch_cmd()
            .env("SNATCH_CLAUDE_DIR", claude.path())
            .env("CODEX_HOME", codex.path())
            .args(["export", &format!("codex:{CODEX_THREAD}"), "-f", "jsonl"])
            .assert()
            .success()
            .get_output()
            .stdout
            .clone();
        let jsonl_text = String::from_utf8(jsonl).unwrap();
        assert!(!jsonl_text.contains(&codex.path().to_string_lossy().to_string()));
        let lines: Vec<serde_json::Value> = jsonl_text
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        assert_eq!(lines[0]["type"], "snatch_normalized_metadata");
        assert_eq!(lines[0]["provider"]["id"], "codex");
        assert!(lines[1..].iter().all(|line| {
            line["type"] == "snatch_normalized_entry"
                && line["derivation"]["entry_id"].as_str().is_some()
                && line["entry"].is_object()
        }));

        let redacted = snatch_cmd()
            .env("SNATCH_CLAUDE_DIR", claude.path())
            .env("CODEX_HOME", codex.path())
            .args([
                "export",
                &format!("codex:{CODEX_THREAD}"),
                "-f",
                "json",
                "--redact",
                "all",
            ])
            .assert()
            .success()
            .get_output()
            .stdout
            .clone();
        let redacted = String::from_utf8(redacted).unwrap();
        assert!(redacted.contains("\"id\":\"codex\""));
        assert!(!redacted.contains("user@example.com"));
    }
}

#[cfg(feature = "codex")]
#[test]
fn doctor_provider_codex_reports_capped_drift_diagnostics() {
    let claude = setup_fixture_dir();
    let codex = TempDir::new().unwrap();
    let day = codex.path().join("sessions/2026/07/16");
    std::fs::create_dir_all(&day).unwrap();
    let tid = "0198bbbb-cccc-7ddd-8eee-ffff00001111";
    std::fs::write(
        day.join(format!("rollout-2026-07-16T10-00-00-{tid}.jsonl")),
        format!(
            "{{\"timestamp\":\"2026-07-16T10:00:00.000Z\",\"type\":\"session_meta\",\"payload\":{{\"id\":\"{tid}\"}}}}\n\
             {{\"timestamp\":\"2026-03-15T10:00:00.000Z\",\"type\":\"response_item\",\"payload\":{{\"type\":\"reasoning\",\"summary\":[{{\"type\":\"summary_text\",\"text\":\"available\"}}],\"encrypted_content\":\"x\"}}}}\n\
             {{\"timestamp\":\"2026-04-15T10:00:00.000Z\",\"type\":\"response_item\",\"payload\":{{\"type\":\"reasoning\",\"summary\":[],\"encrypted_content\":\"y\"}}}}\n"
        ),
    )
    .unwrap();
    snatch_cmd()
        .env("SNATCH_CLAUDE_DIR", claude.path())
        .env("CODEX_HOME", codex.path())
        .args(["doctor", "--provider", "codex"])
        .assert()
        .success()
        .stdout(predicate::str::contains("sessions scanned: 1"))
        .stdout(predicate::str::contains(
            "reasoning summary availability by month (with/total)",
        ))
        .stdout(predicate::str::contains("2026-03: 1/1"))
        .stdout(predicate::str::contains("2026-04: 0/1"))
        .stdout(predicate::str::contains(
            "vocabulary keys dropped at cap: 0",
        ));

    let json = snatch_cmd()
        .env("SNATCH_CLAUDE_DIR", claude.path())
        .env("CODEX_HOME", codex.path())
        .args(["-o", "json", "doctor", "--provider", "codex"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let report: serde_json::Value = serde_json::from_slice(&json).unwrap();
    assert_eq!(
        report["codex"]["reasoning_by_month"]["2026-03"],
        serde_json::json!([1, 1])
    );
    assert_eq!(
        report["codex"]["reasoning_by_month"]["2026-04"],
        serde_json::json!([1, 0])
    );
}

// =============================================================================
// B2.8: complete option refusal + unified qualification (round-18)
// =============================================================================

/// Every non-universal flag must be individually refused on the provider
/// route — silently ignored options are the round-18 blocker-3 hazard.
#[test]
fn provider_list_refuses_every_unsupported_flag_individually() {
    let tmp = setup_fixture_dir();
    let cases: &[&[&str]] = &[
        &["--subagents"],
        &["--subagents-only"],
        &["--active"],
        &["--compacted"],
        &["--full-ids"],
        &["--pager"],
        &["--since", "1d"],
        &["--until", "1d"],
        &["--tag", "t"],
        &["--tags", "t"],
        &["--bookmarked"],
        &["--outcome", "success"],
        &["--by-name", "x"],
        &["--min-size", "1k"],
        &["--max-size", "1m"],
        &["--context"],
        &["--context-length", "200"],
        &["--hide-empty"],
        &["--no-chain"],
    ];
    for extra in cases {
        let mut args = vec!["list", "sessions", "--provider", "claude-code"];
        args.extend_from_slice(extra);
        snatch_cmd()
            .env("SNATCH_CLAUDE_DIR", tmp.path())
            .args(&args)
            .assert()
            .failure()
            .stderr(predicate::str::contains("not supported with"));
    }
}

#[test]
fn provider_info_refuses_every_unsupported_flag_individually() {
    let tmp = setup_fixture_dir();
    let target = format!("claude-code:{SESSION_ID}");
    let cases: &[&[&str]] = &[
        &["--no-chain"],
        &["--tree"],
        &["--raw"],
        &["--entry", "u"],
        &["--paths"],
        &["-m", "3"],
        &["--files"],
    ];
    for extra in cases {
        let mut args = vec!["info", target.as_str()];
        args.extend_from_slice(extra);
        snatch_cmd()
            .env("SNATCH_CLAUDE_DIR", tmp.path())
            .args(&args)
            .assert()
            .failure()
            .stderr(predicate::str::contains("not supported with"));
    }
}

#[test]
fn provider_export_refuses_every_unsupported_flag_individually() {
    let tmp = setup_fixture_dir();
    let target = format!("claude-code:{SESSION_ID}");
    let cases: &[&[&str]] = &[
        &["--all"],
        &["-p", "x"],
        &["--since", "1d"],
        &["--until", "1d"],
        &["--subagents"],
        &["--combine-agents"],
        &["--resolve-tool-results"],
        &["--no-thinking"],
        &["--no-tool-use"],
        &["--no-tool-results"],
        &["--no-images"],
        &["--system"],
        &["--only", "prompts"],
        &["--metadata"],
        &["--main-thread"],
        &["--no-chain"],
        &["--pretty"],
        &["--full"],
        &["--progress"],
        &["--redact", "security"],
        &["--warn-pii"],
        &["--redact-preview"],
        &["--gist"],
        &["--toc"],
        &["--dark"],
        &["--clipboard"],
        &["--template", "summary"],
    ];
    for extra in cases {
        let mut args = vec!["export", target.as_str(), "-f", "raw-jsonl"];
        args.extend_from_slice(extra);
        snatch_cmd()
            .env("SNATCH_CLAUDE_DIR", tmp.path())
            .args(&args)
            .assert()
            .failure()
            .stderr(predicate::str::contains("not supported with"));
    }
}

#[test]
fn provider_normalized_export_refuses_nonportable_flags_individually() {
    let tmp = setup_fixture_dir();
    let target = format!("claude-code:{SESSION_ID}");
    let cases: &[&[&str]] = &[
        &["--all"],
        &["-p", "x"],
        &["--since", "1d"],
        &["--until", "1d"],
        &["--subagents"],
        &["--combine-agents"],
        &["--resolve-tool-results"],
        &["--no-chain"],
        &["--progress"],
        &["--gist"],
        &["--gist-public"],
        &["--gist-description", "x"],
        &["--clipboard"],
        &["--template", "summary"],
    ];
    for extra in cases {
        let mut args = vec!["export", target.as_str(), "-f", "markdown"];
        args.extend_from_slice(extra);
        snatch_cmd()
            .env("SNATCH_CLAUDE_DIR", tmp.path())
            .args(&args)
            .assert()
            .failure()
            .stderr(predicate::str::contains("not supported with"));
    }
}

#[test]
fn provider_doctor_refuses_unsupported_filters_individually() {
    let tmp = setup_fixture_dir();
    let cases: &[&[&str]] = &[&["proj"], &["--since", "1d"], &["--subagents"]];
    for extra in cases {
        let mut args = vec!["doctor", "--provider", "claude-code"];
        args.extend_from_slice(extra);
        snatch_cmd()
            .env("SNATCH_CLAUDE_DIR", tmp.path())
            .args(&args)
            .assert()
            .failure()
            .stderr(predicate::str::contains("not supported with"));
    }
}

/// Unified qualification predicate at the command level (round-18
/// blocker 5): only references whose first segment names a REGISTERED
/// provider take the qualified path.
#[test]
fn qualification_predicate_is_unified_at_command_level() {
    let tmp = setup_fixture_dir();

    // Windows-style path: classic path, no qualified-id parse error.
    snatch_cmd()
        .env("SNATCH_CLAUDE_DIR", tmp.path())
        .env("CODEX_HOME", tmp.path().join("no-such"))
        .args(["info", r"C:\Users\someone\project"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("Session not found"))
        .stderr(predicate::str::contains("segments").not());

    // Unknown colon-bearing reference: classic path, plain not-found.
    snatch_cmd()
        .env("SNATCH_CLAUDE_DIR", tmp.path())
        .args(["info", "ghost:xyz"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("Session not found"))
        .stderr(predicate::str::contains("segments").not());

    // Registered-provider qualified id resolves.
    snatch_cmd()
        .env("SNATCH_CLAUDE_DIR", tmp.path())
        .args(["info", &format!("claude-code:{SESSION_ID}")])
        .assert()
        .success()
        .stdout(predicate::str::contains("Provider: claude-code"));

    // Malformed qualified id (registered prefix, bad escape) errors loudly.
    snatch_cmd()
        .env("SNATCH_CLAUDE_DIR", tmp.path())
        .args(["info", "claude-code:ab%zz"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("invalid escape"));
}

#[cfg(feature = "codex")]
#[test]
fn doctor_withholds_unavailability_details() {
    // Doctor promises no filesystem paths in its output; unavailability
    // detail lives in `snatch providers` instead (round-18).
    let claude = setup_fixture_dir();
    let missing = claude.path().join("definitely-no-codex-here");
    let out = snatch_cmd()
        .env("SNATCH_CLAUDE_DIR", claude.path())
        .env("CODEX_HOME", &missing)
        .args(["doctor", "--provider", "all"])
        .assert()
        .success()
        .get_output()
        .clone();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("details withheld"), "got: {stdout}");
    assert!(
        !stdout.contains("definitely-no-codex-here"),
        "doctor leaked a filesystem path: {stdout}"
    );
}

#[test]
fn provider_export_preflight_never_creates_the_output_file() {
    // Round-18: a refused/unstartable export must not create or truncate
    // the destination.
    let tmp = setup_fixture_dir();
    let out_path = tmp.path().join("never-created.md");
    snatch_cmd()
        .env("SNATCH_CLAUDE_DIR", tmp.path())
        .args([
            "export",
            "claude-code:definitely-missing",
            "-f",
            "markdown",
            "-O",
        ])
        .arg(&out_path)
        .assert()
        .failure();
    assert!(
        !out_path.exists(),
        "preflight must reject before the file is created"
    );
}

#[test]
fn provider_routed_claude_diff_preserves_the_classic_semantic_result() {
    let claude = setup_fixture_dir();
    let missing_codex = claude.path().join("no-codex-home");
    let run = |provider: bool| {
        let mut command = snatch_cmd();
        command
            .env("SNATCH_CLAUDE_DIR", claude.path())
            .env("CODEX_HOME", &missing_codex)
            .args(["-o", "json", "diff", SESSION_ID, SESSION_ID]);
        if provider {
            command.args(["--provider", "claude-code"]);
        }
        let output = command.assert().success().get_output().stdout.clone();
        serde_json::from_slice::<serde_json::Value>(&output).unwrap()
    };

    let classic = run(false);
    let routed = run(true);
    assert_eq!(routed["first_source"]["provider"], "claude-code");
    assert_eq!(
        routed["first_source"]["qualified_id"],
        format!("claude-code:{SESSION_ID}")
    );
    for field in [
        "mode",
        "identical",
        "comparison_basis",
        "filter",
        "summary",
        "details",
    ] {
        assert_eq!(routed[field], classic[field], "field {field}");
    }
}

#[test]
fn export_list_templates_rejects_provider_selection() {
    let tmp = setup_fixture_dir();
    snatch_cmd()
        .env("SNATCH_CLAUDE_DIR", tmp.path())
        .args(["export", "--list-templates", "--provider", "claude-code"])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "cannot be combined with a provider selection",
        ));
}

#[test]
fn export_list_templates_rejects_qualified_session_reference() {
    let tmp = setup_fixture_dir();
    snatch_cmd()
        .env("SNATCH_CLAUDE_DIR", tmp.path())
        .args(["export", "claude-code:whatever", "--list-templates"])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "provider-qualified session reference",
        ));
}

/// Round-20: doctor's ERROR paths (not just partial-success rendering) must
/// never expose raw provider reasons or filesystem paths, on stdout or
/// stderr. `snatch providers` stays the intentionally detailed surface.
#[test]
#[cfg(feature = "codex")]
fn doctor_error_paths_never_leak_paths() {
    const SENTINEL: &str = "sentinel-secret-root";
    let scratch = TempDir::new().unwrap();
    let missing_claude = scratch.path().join(format!("{SENTINEL}-claude"));
    let missing_codex = scratch.path().join(format!("{SENTINEL}-codex"));

    // Case 1: every provider unavailable at construction (zero-success).
    let out = snatch_cmd()
        .env("SNATCH_CLAUDE_DIR", &missing_claude)
        .env("CODEX_HOME", &missing_codex)
        .args(["doctor", "--provider", "all"])
        .assert()
        .failure()
        .get_output()
        .clone();
    let all_text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(all_text.contains("details withheld"), "got: {all_text}");
    assert!(
        !all_text.contains(SENTINEL),
        "doctor leaked a sentinel path: {all_text}"
    );

    // Case 2/3 need a provider that CONSTRUCTS but fails diagnostics at
    // runtime: a codex home whose sessions tree is a regular file.
    let claude = setup_fixture_dir();
    let broken_codex = TempDir::new().unwrap();
    std::fs::write(
        broken_codex.path().join("sessions"),
        format!("not-a-directory {SENTINEL}"),
    )
    .unwrap();

    // Case 2: explicit provider, runtime diagnostics failure (atomic).
    let out = snatch_cmd()
        .env("SNATCH_CLAUDE_DIR", claude.path())
        .env("CODEX_HOME", broken_codex.path())
        .args(["doctor", "--provider", "codex"])
        .assert()
        .failure()
        .get_output()
        .clone();
    let all_text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(all_text.contains("details withheld"), "got: {all_text}");
    assert!(
        !all_text.contains(SENTINEL) && !all_text.contains("sessions"),
        "doctor leaked runtime failure detail: {all_text}"
    );

    // Case 3: `all` where the only runtime diagnostics call fails and the
    // other provider is unavailable at construction (zero successes).
    let out = snatch_cmd()
        .env("SNATCH_CLAUDE_DIR", &missing_claude)
        .env("CODEX_HOME", broken_codex.path())
        .args(["doctor", "--provider", "all"])
        .assert()
        .failure()
        .get_output()
        .clone();
    let all_text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(all_text.contains("details withheld"), "got: {all_text}");
    assert!(!all_text.contains(SENTINEL), "leak: {all_text}");

    // Case 4: partial success — safe per-provider placeholder, no leak.
    let out = snatch_cmd()
        .env("SNATCH_CLAUDE_DIR", claude.path())
        .env("CODEX_HOME", &missing_codex)
        .args(["doctor", "--provider", "all"])
        .assert()
        .success()
        .get_output()
        .clone();
    let all_text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(all_text.contains("details withheld"), "got: {all_text}");
    assert!(!all_text.contains(SENTINEL), "leak: {all_text}");
}

/// Round-20: the constructed/scan_ok/available triple as committed
/// assertions across all three states.
#[test]
fn providers_status_triple_reflects_construction_and_scan() {
    let claude = setup_fixture_dir();

    // Not constructed (missing home).
    let out = snatch_cmd()
        .env("SNATCH_CLAUDE_DIR", claude.path())
        .env("CODEX_HOME", claude.path().join("nope"))
        .args(["-o", "json", "providers"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let reports: serde_json::Value = serde_json::from_slice(&out).unwrap();
    let claude_row = &reports[0];
    assert_eq!(
        (
            claude_row["constructed"].as_bool(),
            claude_row["scan_ok"].as_bool(),
            claude_row["available"].as_bool()
        ),
        (Some(true), Some(true), Some(true))
    );
    if let Some(codex_row) = reports.as_array().unwrap().get(1) {
        assert_eq!(
            (
                codex_row["constructed"].as_bool(),
                codex_row["scan_ok"].as_bool(),
                codex_row["available"].as_bool()
            ),
            (Some(false), Some(false), Some(false))
        );
    }

    // Constructed but scan-failed (sessions tree is a regular file).
    #[cfg(feature = "codex")]
    {
        let broken = TempDir::new().unwrap();
        std::fs::write(broken.path().join("sessions"), "not-a-dir").unwrap();
        let out = snatch_cmd()
            .env("SNATCH_CLAUDE_DIR", claude.path())
            .env("CODEX_HOME", broken.path())
            .args(["-o", "json", "providers"])
            .assert()
            .success()
            .get_output()
            .stdout
            .clone();
        let reports: serde_json::Value = serde_json::from_slice(&out).unwrap();
        let codex_row = &reports[1];
        assert_eq!(
            (
                codex_row["constructed"].as_bool(),
                codex_row["scan_ok"].as_bool(),
                codex_row["available"].as_bool()
            ),
            (Some(true), Some(false), Some(false)),
            "constructed-but-scan-failed must be visible as such: {codex_row}"
        );
    }
}

#[cfg(feature = "codex")]
mod codex_normalization_cli {
    use super::*;

    const THREAD: &str = "0198cccc-dddd-7eee-8fff-000011112222";
    const SUMMARY_PARENT: &str = "0198dddd-0000-7000-8000-000000000001";
    const SUMMARY_FORK: &str = "0198dddd-0000-7000-8000-000000000002";
    const SUMMARY_SPAWN: &str = "0198dddd-0000-7000-8000-000000000003";
    const SUMMARY_COMPRESSED: &str = "0198dddd-0000-7000-8000-000000000004";
    const SUMMARY_ACTIVE_TAIL: &str = "0198dddd-0000-7000-8000-000000000005";

    /// A dual-stream codex fixture exercising the whole slice: human
    /// prompt (with event twin), reasoning, tool call/result, usage.
    fn slice_home() -> TempDir {
        let tmp = TempDir::new().unwrap();
        let day = tmp.path().join("sessions/2026/07/16");
        std::fs::create_dir_all(&day).unwrap();
        let lines = [
            serde_json::json!({"timestamp": "2026-07-16T10:00:00.000Z", "type": "session_meta",
                "payload": {"id": THREAD, "cwd": "/tmp/p", "cli_version": "0.9"}}),
            serde_json::json!({"timestamp": "2026-07-16T10:00:01.000Z", "type": "turn_context",
                "payload": {"turn_id": "t-1", "model": "gpt-test"}}),
            serde_json::json!({"timestamp": "2026-07-16T10:00:02.000Z", "type": "response_item",
                "payload": {"type": "message", "role": "user",
                            "content": [{"type": "input_text", "text": "run the tests"}]}}),
            serde_json::json!({"timestamp": "2026-07-16T10:00:02.500Z", "type": "event_msg",
                "payload": {"type": "user_message", "message": "run the tests"}}),
            serde_json::json!({"timestamp": "2026-07-16T10:00:03.000Z", "type": "response_item",
                "payload": {"type": "reasoning", "summary": [{"type": "summary_text", "text": "Plan the test run"}],
                            "content": [], "encrypted_content": "sig"}}),
            serde_json::json!({"timestamp": "2026-07-16T10:00:04.000Z", "type": "response_item",
                "payload": {"type": "function_call", "name": "shell",
                            "arguments": "{\"command\":[\"cargo\",\"test\"]}", "call_id": "call_1"}}),
            serde_json::json!({"timestamp": "2026-07-16T10:00:05.000Z", "type": "response_item",
                "payload": {"type": "function_call_output", "call_id": "call_1", "output": "ok: 12 passed"}}),
            serde_json::json!({"timestamp": "2026-07-16T10:00:06.000Z", "type": "event_msg",
                "payload": {"type": "agent_message", "message": "All green."}}),
            serde_json::json!({"timestamp": "2026-07-16T10:00:06.500Z", "type": "response_item",
                "payload": {"type": "message", "role": "assistant",
                            "content": [{"type": "output_text", "text": "All green."}]}}),
            serde_json::json!({"timestamp": "2026-07-16T10:00:07.000Z", "type": "event_msg",
                "payload": {"type": "token_count", "info": {
                    "last_token_usage": {"input_tokens": 100, "cached_input_tokens": 60, "output_tokens": 25, "total_tokens": 125},
                    "total_token_usage": {"input_tokens": 100, "cached_input_tokens": 60, "output_tokens": 25, "total_tokens": 125}}}}),
            serde_json::json!({"timestamp": "2026-07-16T10:00:08.000Z", "type": "compacted",
                "payload": {"message": "context summary", "replacement_history": [],
                    "window_number": 2, "first_window_id": "w1",
                    "previous_window_id": "w1", "window_id": "w2"}}),
        ];
        let content = lines
            .iter()
            .map(|l| l.to_string())
            .collect::<Vec<_>>()
            .join("\n")
            + "\n";
        std::fs::write(
            day.join(format!("rollout-2026-07-16T10-00-00-{THREAD}.jsonl")),
            content,
        )
        .unwrap();
        tmp
    }

    fn fork_summary_home() -> TempDir {
        let tmp = TempDir::new().unwrap();
        let day = tmp.path().join("sessions/2026/07/16");
        std::fs::create_dir_all(&day).unwrap();
        let line = |timestamp: &str, kind: &str, payload: serde_json::Value| serde_json::json!({"timestamp": timestamp, "type": kind, "payload": payload});
        let serialize = |records: &[serde_json::Value]| {
            records
                .iter()
                .map(serde_json::Value::to_string)
                .collect::<Vec<_>>()
                .join("\n")
                + "\n"
        };
        let parent = vec![
            line(
                "2026-07-16T08:00:00Z",
                "session_meta",
                serde_json::json!({
                    "id": SUMMARY_PARENT, "cwd": "/tmp/fork-summary"
                }),
            ),
            line(
                "2026-07-16T08:00:01Z",
                "turn_context",
                serde_json::json!({
                    "turn_id": "parent-turn", "model": "gpt-test"
                }),
            ),
            line(
                "2026-07-16T08:00:02Z",
                "response_item",
                serde_json::json!({
                    "type": "message", "role": "user",
                    "content": [{"type": "input_text", "text": "parent prompt"}]
                }),
            ),
            line(
                "2026-07-16T08:00:02.1Z",
                "event_msg",
                serde_json::json!({
                    "type": "user_message", "message": "parent prompt"
                }),
            ),
            line(
                "2026-07-16T08:00:03Z",
                "response_item",
                serde_json::json!({
                    "type": "message", "role": "assistant",
                    "content": [{"type": "output_text", "text": "parent response"}]
                }),
            ),
            line(
                "2026-07-16T08:00:04Z",
                "event_msg",
                serde_json::json!({
                    "type": "token_count", "info": {
                        "last_token_usage": {"input_tokens": 100, "cached_input_tokens": 60,
                            "output_tokens": 25, "total_tokens": 125},
                        "total_token_usage": {"input_tokens": 100, "cached_input_tokens": 60,
                            "output_tokens": 25, "total_tokens": 125}
                    }
                }),
            ),
        ];
        std::fs::write(
            day.join(format!(
                "rollout-2026-07-16T08-00-00-{SUMMARY_PARENT}.jsonl"
            )),
            serialize(&parent),
        )
        .unwrap();

        let mut copied = parent.clone();
        for record in &mut copied {
            record["timestamp"] = serde_json::json!("2026-07-16T09:00:00Z");
        }
        let mut fork = vec![line(
            "2026-07-16T09:00:00Z",
            "session_meta",
            serde_json::json!({"id": SUMMARY_FORK, "cwd": "/tmp/fork-summary"}),
        )];
        fork.extend(copied);
        fork.extend([
            line(
                "2026-07-16T09:00:01Z",
                "turn_context",
                serde_json::json!({
                    "turn_id": "fork-turn", "model": "gpt-test"
                }),
            ),
            line(
                "2026-07-16T09:00:02Z",
                "response_item",
                serde_json::json!({
                    "type": "message", "role": "user",
                    "content": [{"type": "input_text", "text": "fork prompt"}]
                }),
            ),
            line(
                "2026-07-16T09:00:02.1Z",
                "event_msg",
                serde_json::json!({
                    "type": "user_message", "message": "fork prompt"
                }),
            ),
            line(
                "2026-07-16T09:00:03Z",
                "response_item",
                serde_json::json!({
                    "type": "message", "role": "assistant",
                    "content": [{"type": "output_text", "text": "fork response"}]
                }),
            ),
        ]);
        std::fs::write(
            day.join(format!("rollout-2026-07-16T09-00-00-{SUMMARY_FORK}.jsonl")),
            serialize(&fork),
        )
        .unwrap();

        let spawn = vec![
            line(
                "2026-07-16T10:00:00Z",
                "session_meta",
                serde_json::json!({
                    "id": SUMMARY_SPAWN, "cwd": "/tmp/fork-summary",
                    "source": {"subagent": {"thread_spawn": {
                        "parent_thread_id": SUMMARY_PARENT, "depth": 1
                    }}}
                }),
            ),
            line(
                "2026-07-16T10:00:01Z",
                "turn_context",
                serde_json::json!({
                    "turn_id": "spawn-turn", "model": "gpt-test"
                }),
            ),
            line(
                "2026-07-16T10:00:01.5Z",
                "response_item",
                serde_json::json!({
                    "type": "message", "role": "user",
                    "content": [{"type": "input_text", "text": "spawn prompt"}]
                }),
            ),
            line(
                "2026-07-16T10:00:01.6Z",
                "event_msg",
                serde_json::json!({
                    "type": "user_message", "message": "spawn prompt"
                }),
            ),
            line(
                "2026-07-16T10:00:02Z",
                "response_item",
                serde_json::json!({
                    "type": "message", "role": "assistant",
                    "content": [{"type": "output_text", "text": "spawn response"}]
                }),
            ),
            line(
                "2026-07-16T10:00:03Z",
                "event_msg",
                serde_json::json!({
                    "type": "token_count", "info": {
                        "last_token_usage": {"input_tokens": 1000, "cached_input_tokens": 0,
                            "output_tokens": 1000, "total_tokens": 2000},
                        "total_token_usage": {"input_tokens": 1000, "cached_input_tokens": 0,
                            "output_tokens": 1000, "total_tokens": 2000}
                    }
                }),
            ),
        ];
        std::fs::write(
            day.join(format!("rollout-2026-07-16T10-00-00-{SUMMARY_SPAWN}.jsonl")),
            serialize(&spawn),
        )
        .unwrap();
        tmp
    }

    fn activity_summary_content(id: &str) -> String {
        let records = [
            serde_json::json!({
                "timestamp": "2020-01-01T00:00:00Z",
                "type": "session_meta",
                "payload": {"id": id, "cwd": "/tmp/activity-summary"}
            }),
            serde_json::json!({
                "timestamp": "2020-01-01T00:00:01Z",
                "type": "turn_context",
                "payload": {"turn_id": "compressed-turn", "model": "gpt-test"}
            }),
            serde_json::json!({
                "timestamp": "2020-01-01T00:00:01.5Z",
                "type": "response_item",
                "payload": {"type": "message", "role": "user",
                    "content": [{"type": "input_text", "text": "compressed prompt"}]}
            }),
            serde_json::json!({
                "timestamp": "2020-01-01T00:00:01.6Z",
                "type": "event_msg",
                "payload": {"type": "user_message", "message": "compressed prompt"}
            }),
            serde_json::json!({
                "timestamp": "2020-01-01T00:00:02Z",
                "type": "response_item",
                "payload": {"type": "message", "role": "assistant",
                    "content": [{"type": "output_text", "text": "compressed response"}]}
            }),
        ];
        records
            .iter()
            .map(serde_json::Value::to_string)
            .collect::<Vec<_>>()
            .join("\n")
            + "\n"
    }

    fn compressed_summary_home() -> TempDir {
        let tmp = TempDir::new().unwrap();
        let day = tmp.path().join("sessions/2020/01/01");
        std::fs::create_dir_all(&day).unwrap();
        let content = activity_summary_content(SUMMARY_COMPRESSED);
        let compressed = zstd::stream::encode_all(content.as_bytes(), 3).unwrap();
        std::fs::write(
            day.join(format!(
                "rollout-2020-01-01T00-00-00-{SUMMARY_COMPRESSED}.jsonl.zst"
            )),
            compressed,
        )
        .unwrap();
        tmp
    }

    fn active_tail_summary_home() -> TempDir {
        let tmp = TempDir::new().unwrap();
        let day = tmp.path().join("sessions/2020/01/01");
        std::fs::create_dir_all(&day).unwrap();
        let mut content = activity_summary_content(SUMMARY_ACTIVE_TAIL);
        content.push_str(r#"{"timestamp":"2026-07-22T00:00:00Z","type":"response_item""#);
        std::fs::write(
            day.join(format!(
                "rollout-2020-01-01T00-00-00-{SUMMARY_ACTIVE_TAIL}.jsonl"
            )),
            content,
        )
        .unwrap();
        tmp
    }

    #[test]
    fn provider_summary_aggregates_canonical_usage_and_marks_partial_pricing() {
        let claude = setup_fixture_dir();
        let codex = slice_home();
        let output = snatch_cmd()
            .env("SNATCH_CLAUDE_DIR", claude.path())
            .env("CODEX_HOME", codex.path())
            .args([
                "-o",
                "json",
                "summary",
                "--provider",
                "all",
                "--period",
                "24m",
            ])
            .assert()
            .success()
            .get_output()
            .stdout
            .clone();
        let value: serde_json::Value = serde_json::from_slice(&output).unwrap();
        assert_eq!(value["sessions"], 2);
        assert_eq!(value["session_descriptors_analyzed"], 2);
        assert_eq!(value["projects"], 2);
        // Claude fixture: 140 work/processed. Native fixture: 65 work,
        // 125 processed. The cumulative observation is reconciliation data,
        // never a second usage emission.
        assert_eq!(value["total_tokens"], 205);
        assert_eq!(value["total_processed_tokens"], 265);
        assert_eq!(value["pricing_coverage"], "partial");
        assert!(value["estimated_cost"].as_f64().unwrap() > 0.0);
        assert_eq!(value["unpriced_providers"], serde_json::json!(["codex"]));
        assert_eq!(value["unpriced_models"], serde_json::json!(["gpt-test"]));
        assert_eq!(value["skipped_providers"], serde_json::json!([]));

        // Activity filtering uses the native last-event time before source
        // mtime, so writing an old fixture today does not make it recent.
        let output = snatch_cmd()
            .env("SNATCH_CLAUDE_DIR", claude.path())
            .env("CODEX_HOME", codex.path())
            .args([
                "-o",
                "json",
                "summary",
                "--provider",
                "codex",
                "--period",
                "1d",
            ])
            .assert()
            .success()
            .get_output()
            .stdout
            .clone();
        let value: serde_json::Value = serde_json::from_slice(&output).unwrap();
        assert_eq!(value["sessions"], 0);
        assert_eq!(value["activity_time_fallback_sessions"], 0);
        assert_eq!(value["total_processed_tokens"], 0);
        assert_eq!(value["pricing_coverage"], "not-applicable");
    }

    #[test]
    fn provider_summary_reports_conservative_source_time_fallbacks() {
        let claude = setup_fixture_dir();
        let codex = compressed_summary_home();
        let output = snatch_cmd()
            .env("SNATCH_CLAUDE_DIR", claude.path())
            .env("CODEX_HOME", codex.path())
            .args([
                "-o",
                "json",
                "summary",
                "--provider",
                "codex",
                "--period",
                "1d",
            ])
            .assert()
            .success()
            .get_output()
            .stdout
            .clone();
        let value: serde_json::Value = serde_json::from_slice(&output).unwrap();
        // Inventory deliberately does not decompress a cold artifact merely
        // to find its tail. Its old native start plus current source mtime is
        // included conservatively, and the weaker evidence is made visible.
        assert_eq!(value["sessions"], 1);
        assert_eq!(value["session_descriptors_analyzed"], 1);
        assert_eq!(value["activity_time_fallback_sessions"], 1);

        let active = active_tail_summary_home();
        let output = snatch_cmd()
            .env("SNATCH_CLAUDE_DIR", claude.path())
            .env("CODEX_HOME", active.path())
            .args([
                "-o",
                "json",
                "summary",
                "--provider",
                "codex",
                "--period",
                "1d",
            ])
            .assert()
            .success()
            .get_output()
            .stdout
            .clone();
        let value: serde_json::Value = serde_json::from_slice(&output).unwrap();
        // An unresolved physical tail is evidence that an actively-appended
        // plain file may be newer than its last complete native event.
        assert_eq!(value["sessions"], 1);
        assert_eq!(value["activity_time_fallback_sessions"], 1);
    }

    #[test]
    fn provider_summary_excludes_fork_copies_and_spawned_transcripts() {
        let claude = setup_fixture_dir();
        let codex = fork_summary_home();
        let output = snatch_cmd()
            .env("SNATCH_CLAUDE_DIR", claude.path())
            .env("CODEX_HOME", codex.path())
            .args([
                "-o",
                "json",
                "summary",
                "--provider",
                "codex",
                "--period",
                "24m",
            ])
            .assert()
            .success()
            .get_output()
            .stdout
            .clone();
        let value: serde_json::Value = serde_json::from_slice(&output).unwrap();
        // Parent and fork are independent logical sessions. The spawn is
        // excluded, and the copied parent usage inside the fork is not summed.
        assert_eq!(value["sessions"], 2);
        assert_eq!(value["session_descriptors_analyzed"], 2);
        assert_eq!(value["total_tokens"], 65);
        assert_eq!(value["total_processed_tokens"], 125);
        assert_eq!(value["messages"], 4);
    }

    fn content_provenance_home() -> TempDir {
        let tmp = TempDir::new().unwrap();
        let day = tmp.path().join("sessions/2026/07/16");
        std::fs::create_dir_all(&day).unwrap();
        let human = "Please use this context:\n> relayed but still part of my prompt\n```rust\nlet human = true;\n```";
        let lines = [
            serde_json::json!({"timestamp": "2026-07-16T11:00:00Z", "type": "session_meta",
                "payload": {"id": THREAD, "cwd": "/tmp/p", "cli_version": "0.9"}}),
            serde_json::json!({"timestamp": "2026-07-16T11:00:01Z", "type": "turn_context",
                "payload": {"turn_id": "content-turn", "model": "gpt-test"}}),
            // User-role harness context: no user_message twin, therefore not human-authored.
            serde_json::json!({"timestamp": "2026-07-16T11:00:02Z", "type": "response_item",
                "payload": {"type": "message", "role": "user", "content": [{"type": "input_text",
                    "text": "harness context\n```rust\nlet harness = true;\n```"}]}}),
            serde_json::json!({"timestamp": "2026-07-16T11:00:03Z", "type": "response_item",
                "payload": {"type": "message", "role": "user",
                    "content": [{"type": "input_text", "text": human}]}}),
            serde_json::json!({"timestamp": "2026-07-16T11:00:03.100Z", "type": "event_msg",
                "payload": {"type": "user_message", "message": human}}),
            serde_json::json!({"timestamp": "2026-07-16T11:00:04Z", "type": "response_item",
                "payload": {"type": "message", "role": "assistant", "content": [{"type": "output_text",
                    "text": "Assistant code:\n```python\nprint('assistant')\n```"}]}}),
            // A unique user event is a human mid-turn steering prompt.
            serde_json::json!({"timestamp": "2026-07-16T11:00:05Z", "type": "event_msg",
                "payload": {"type": "user_message",
                    "message": "Steer with:\n```go\npackage main\n```"}}),
        ];
        let content = lines
            .iter()
            .map(serde_json::Value::to_string)
            .collect::<Vec<_>>()
            .join("\n")
            + "\n";
        std::fs::write(
            day.join(format!("rollout-2026-07-16T11-00-00-{THREAD}.jsonl")),
            content,
        )
        .unwrap();
        tmp
    }

    fn lessons_home() -> TempDir {
        let tmp = TempDir::new().unwrap();
        let day = tmp.path().join("sessions/2026/07/16");
        std::fs::create_dir_all(&day).unwrap();
        let lines = [
            serde_json::json!({"timestamp": "2026-07-16T10:00:00.000Z", "type": "session_meta",
                "payload": {"id": THREAD, "cwd": "/tmp/p", "cli_version": "0.9"}}),
            serde_json::json!({"timestamp": "2026-07-16T10:00:01.000Z", "type": "turn_context",
                "payload": {"turn_id": "t-1", "model": "gpt-test"}}),
            serde_json::json!({"timestamp": "2026-07-16T10:00:02.000Z", "type": "response_item",
                "payload": {"type": "message", "role": "user",
                    "content": [{"type": "input_text", "text": "make the change"}]}}),
            serde_json::json!({"timestamp": "2026-07-16T10:00:02.100Z", "type": "event_msg",
                "payload": {"type": "user_message", "message": "make the change"}}),
            serde_json::json!({"timestamp": "2026-07-16T10:00:03.000Z", "type": "response_item",
                "payload": {"type": "custom_tool_call", "name": "apply_patch",
                    "call_id": "patch-1", "input": "patch"}}),
            serde_json::json!({"timestamp": "2026-07-16T10:00:04.000Z", "type": "response_item",
                "payload": {"type": "custom_tool_call_output", "call_id": "patch-1",
                    "output": "apply_patch verification failed: Failed to find expected lines"}}),
            serde_json::json!({"timestamp": "2026-07-16T10:00:05.000Z", "type": "response_item",
                "payload": {"type": "message", "role": "assistant",
                    "content": [{"type": "output_text", "text": "I will inspect the exact context."}]}}),
            serde_json::json!({"timestamp": "2026-07-16T10:00:06.000Z", "type": "response_item",
                "payload": {"type": "function_call", "name": "exec_command", "call_id": "ok-1",
                    "arguments": "{\"cmd\":\"cargo check\"}"}}),
            serde_json::json!({"timestamp": "2026-07-16T10:00:07.000Z", "type": "response_item",
                "payload": {"type": "function_call_output", "call_id": "ok-1",
                    "output": "Process exited with code 0\nFinal output:\nerror[E0308] quoted fixture"}}),
            serde_json::json!({"timestamp": "2026-07-16T10:00:08.000Z", "type": "response_item",
                "payload": {"type": "function_call", "name": "exec_command", "call_id": "fail-1",
                    "arguments": "{\"cmd\":\"cargo test\"}"}}),
            serde_json::json!({"timestamp": "2026-07-16T10:00:09.000Z", "type": "response_item",
                "payload": {"type": "function_call_output", "call_id": "fail-1",
                    "output": "Process exited with code 101\nFinal output:\ntest failed"}}),
            serde_json::json!({"timestamp": "2026-07-16T10:00:10.000Z", "type": "response_item",
                "payload": {"type": "message", "role": "assistant",
                    "content": [{"type": "output_text", "text": "I will fix the failing test."}]}}),
            // Harness text with correction words must not count.
            serde_json::json!({"timestamp": "2026-07-16T10:00:11.000Z", "type": "response_item",
                "payload": {"type": "message", "role": "user",
                    "content": [{"type": "input_text", "text": "No, this harness reminder is wrong instead"}]}}),
            serde_json::json!({"timestamp": "2026-07-16T10:00:12.000Z", "type": "response_item",
                "payload": {"type": "message", "role": "user",
                    "content": [{"type": "input_text", "text": "No, use the exact context instead"}]}}),
            serde_json::json!({"timestamp": "2026-07-16T10:00:12.100Z", "type": "event_msg",
                "payload": {"type": "user_message", "message": "No, use the exact context instead"}}),
        ];
        let content = lines
            .iter()
            .map(serde_json::Value::to_string)
            .collect::<Vec<_>>()
            .join("\n")
            + "\n";
        std::fs::write(
            day.join(format!("rollout-2026-07-16T10-00-00-{THREAD}.jsonl")),
            content,
        )
        .unwrap();
        tmp
    }

    fn file_changes_home() -> TempDir {
        let tmp = TempDir::new().unwrap();
        let day = tmp.path().join("sessions/2026/07/16");
        std::fs::create_dir_all(&day).unwrap();
        let applied = serde_json::json!({
            "output": "Success. Updated files.",
            "metadata": {"exit_code": 0, "duration_seconds": 0.1}
        })
        .to_string();
        let lines = [
            serde_json::json!({"timestamp": "2026-07-16T10:00:00Z", "type": "session_meta",
                "payload": {"id": THREAD, "cwd": "/tmp/p", "cli_version": "0.9"}}),
            serde_json::json!({"timestamp": "2026-07-16T10:00:01Z", "type": "turn_context",
                "payload": {"turn_id": "t-files", "model": "gpt-test"}}),
            serde_json::json!({"timestamp": "2026-07-16T10:00:02Z", "type": "response_item",
                "payload": {"type": "message", "role": "user",
                    "content": [{"type": "input_text", "text": "move the module"}]}}),
            serde_json::json!({"timestamp": "2026-07-16T10:00:03Z", "type": "response_item",
                "payload": {"type": "custom_tool_call", "name": "apply_patch", "call_id": "patch-ok",
                    "input": "*** Begin Patch\n*** Update File: src/old.rs\n*** Move to: src/new.rs\n@@\n-old\n+new\n*** End Patch\n"}}),
            serde_json::json!({"timestamp": "2026-07-16T10:00:04Z", "type": "response_item",
                "payload": {"type": "custom_tool_call_output", "call_id": "patch-ok", "output": applied}}),
            serde_json::json!({"timestamp": "2026-07-16T10:00:05Z", "type": "response_item",
                "payload": {"type": "custom_tool_call", "name": "apply_patch", "call_id": "patch-fail",
                    "input": "*** Begin Patch\n*** Update File: src/0-retry.rs\n@@\n-old\n+new\n*** End Patch\n"}}),
            serde_json::json!({"timestamp": "2026-07-16T10:00:06Z", "type": "response_item",
                "payload": {"type": "custom_tool_call_output", "call_id": "patch-fail",
                    "output": "apply_patch verification failed: missing context"}}),
        ];
        let content = lines
            .iter()
            .map(serde_json::Value::to_string)
            .collect::<Vec<_>>()
            .join("\n")
            + "\n";
        std::fs::write(
            day.join(format!("rollout-2026-07-16T10-00-00-{THREAD}.jsonl")),
            content,
        )
        .unwrap();
        tmp
    }

    fn event_context_home() -> TempDir {
        let tmp = TempDir::new().unwrap();
        let day = tmp.path().join("sessions/2026/07/16");
        std::fs::create_dir_all(&day).unwrap();
        let line = |timestamp: &str, kind: &str, payload: serde_json::Value| serde_json::json!({"timestamp": timestamp, "type": kind, "payload": payload});
        let lines = [
            line(
                "2026-07-16T12:00:00Z",
                "session_meta",
                serde_json::json!({"id": THREAD, "cwd": "/tmp/context", "cli_version": "0.9"}),
            ),
            line(
                "2026-07-16T12:00:01Z",
                "turn_context",
                serde_json::json!({"turn_id": "turn-before", "model": "gpt-test"}),
            ),
            line(
                "2026-07-16T12:00:02Z",
                "response_item",
                serde_json::json!({"type": "message", "role": "user",
                    "content": [{"type": "input_text", "text": "before prompt"}]}),
            ),
            line(
                "2026-07-16T12:00:02.100Z",
                "event_msg",
                serde_json::json!({"type": "user_message", "message": "before prompt"}),
            ),
            line(
                "2026-07-16T12:00:03Z",
                "response_item",
                serde_json::json!({"type": "message", "role": "assistant",
                    "content": [{"type": "output_text", "text": "before response"}]}),
            ),
            line(
                "2026-07-16T12:01:00Z",
                "turn_context",
                serde_json::json!({"turn_id": "turn-focus", "model": "gpt-test"}),
            ),
            line(
                "2026-07-16T12:01:01Z",
                "response_item",
                serde_json::json!({"type": "message", "role": "user",
                    "content": [{"type": "input_text", "text": "harness-only context"}]}),
            ),
            line(
                "2026-07-16T12:01:02Z",
                "response_item",
                serde_json::json!({"type": "message", "role": "user",
                    "content": [{"type": "input_text", "text": "focus prompt"}]}),
            ),
            line(
                "2026-07-16T12:01:02.100Z",
                "event_msg",
                serde_json::json!({"type": "user_message", "message": "focus prompt"}),
            ),
            line(
                "2026-07-16T12:01:03Z",
                "response_item",
                serde_json::json!({"type": "function_call", "name": "exec_command",
                    "call_id": "context-exec", "arguments": "{\"cmd\":\"false\"}"}),
            ),
            line(
                "2026-07-16T12:01:04Z",
                "event_msg",
                serde_json::json!({"type": "exec_command_end", "call_id": "context-exec",
                    "turn_id": "turn-focus", "command": ["false"], "cwd": "/tmp/context",
                    "exit_code": 7, "status": "failed", "stderr": "boom", "stdout": ""}),
            ),
            line(
                "2026-07-16T12:01:05Z",
                "response_item",
                serde_json::json!({"type": "function_call_output", "call_id": "context-exec",
                    "output": "boom"}),
            ),
            line(
                "2026-07-16T12:01:06Z",
                "response_item",
                serde_json::json!({"type": "custom_tool_call", "name": "apply_patch",
                    "call_id": "context-patch", "input": "*** Begin Patch"}),
            ),
            line(
                "2026-07-16T12:01:07Z",
                "event_msg",
                serde_json::json!({"type": "patch_apply_end", "call_id": "context-patch",
                    "turn_id": "turn-focus", "status": "completed", "success": true,
                    "changes": {"src/changed.rs": {"type": "update",
                        "unified_diff": "@@\n-old\n+new", "move_path": "src/moved.rs"}}}),
            ),
            line(
                "2026-07-16T12:01:08Z",
                "response_item",
                serde_json::json!({"type": "custom_tool_call_output", "call_id": "context-patch",
                    "output": "Exit code: 0\nOutput:\ndone"}),
            ),
            line(
                "2026-07-16T12:01:09Z",
                "event_msg",
                serde_json::json!({"type": "user_message", "message": "focus steering"}),
            ),
            line(
                "2026-07-16T12:01:10Z",
                "response_item",
                serde_json::json!({"type": "message", "role": "assistant",
                    "content": [{"type": "output_text", "text": "focus response"}]}),
            ),
            line(
                "2026-07-16T12:02:00Z",
                "turn_context",
                serde_json::json!({"turn_id": "turn-after", "model": "gpt-test"}),
            ),
            line(
                "2026-07-16T12:02:01Z",
                "response_item",
                serde_json::json!({"type": "message", "role": "user",
                    "content": [{"type": "input_text", "text": "after prompt"}]}),
            ),
            line(
                "2026-07-16T12:02:01.100Z",
                "event_msg",
                serde_json::json!({"type": "user_message", "message": "after prompt"}),
            ),
            line(
                "2026-07-16T12:02:02Z",
                "response_item",
                serde_json::json!({"type": "message", "role": "assistant",
                    "content": [{"type": "output_text", "text": "after response"}]}),
            ),
        ];
        let content = lines
            .iter()
            .map(serde_json::Value::to_string)
            .collect::<Vec<_>>()
            .join("\n")
            + "\n";
        std::fs::write(
            day.join(format!("rollout-2026-07-16T12-00-00-{THREAD}.jsonl")),
            content,
        )
        .unwrap();
        tmp
    }

    #[test]
    fn diff_resolves_each_provider_target_and_preserves_both_identities() {
        let claude = setup_fixture_dir();
        let codex = slice_home();
        let first = format!("claude-code:{SESSION_ID}");
        let second = format!("codex:{THREAD}");
        let output = snatch_cmd()
            .env("SNATCH_CLAUDE_DIR", claude.path())
            .env("CODEX_HOME", codex.path())
            .args(["-o", "json", "diff", &first, &second])
            .assert()
            .success()
            .get_output()
            .stdout
            .clone();
        let value: serde_json::Value = serde_json::from_slice(&output).unwrap();
        assert_eq!(value["mode"], "semantic");
        assert_eq!(
            value["comparison_basis"],
            "ordered_identity_neutral_payloads"
        );
        assert_eq!(value["first"], first);
        assert_eq!(value["second"], second);
        assert_eq!(value["first_source"]["provider"], "claude-code");
        assert_eq!(value["first_source"]["qualified_id"], first);
        assert_eq!(value["second_source"]["provider"], "codex");
        assert_eq!(value["second_source"]["qualified_id"], second);

        // `--prompts` is an authorship projection, not merely every
        // user-role record: the tool result and harness traffic stay out.
        let prompt_output = snatch_cmd()
            .env("SNATCH_CLAUDE_DIR", claude.path())
            .env("CODEX_HOME", codex.path())
            .args(["-o", "json", "diff", &second, &second, "--prompts"])
            .assert()
            .success()
            .get_output()
            .stdout
            .clone();
        let prompt_output: serde_json::Value = serde_json::from_slice(&prompt_output).unwrap();
        assert_eq!(prompt_output["summary"]["first_message_count"], 1);
        assert_eq!(prompt_output["summary"]["second_message_count"], 1);
    }

    #[test]
    fn provider_line_diff_uses_the_raw_jsonl_capability() {
        let claude = setup_fixture_dir();
        let codex = slice_home();
        let target = format!("codex:{THREAD}");
        let output = snatch_cmd()
            .env("SNATCH_CLAUDE_DIR", claude.path())
            .env("CODEX_HOME", codex.path())
            .args(["-o", "json", "diff", &target, &target, "--line-based"])
            .assert()
            .success()
            .get_output()
            .stdout
            .clone();
        let value: serde_json::Value = serde_json::from_slice(&output).unwrap();
        assert_eq!(value["mode"], "line-based");
        assert_eq!(value["identical"], true);
        assert_eq!(value["first_source"]["provider"], "codex");
        assert_eq!(value["second_source"]["qualified_id"], target);
        assert_eq!(value["summary"]["matching"], 11);

        snatch_cmd()
            .env("SNATCH_CLAUDE_DIR", claude.path())
            .env("CODEX_HOME", codex.path())
            .args([
                "--max-file-size",
                "100",
                "diff",
                &target,
                &target,
                "--line-based",
            ])
            .assert()
            .failure()
            .stderr(predicate::str::contains("size limit"));
    }

    #[test]
    fn messages_renders_normalized_codex_conversation() {
        let claude = setup_fixture_dir();
        let codex = slice_home();
        snatch_cmd()
            .env("SNATCH_CLAUDE_DIR", claude.path())
            .env("CODEX_HOME", codex.path())
            .args([
                "messages",
                &format!("codex:{THREAD}"),
                "--detail",
                "full",
                "--include-thinking",
            ])
            .assert()
            .success()
            .stdout(predicate::str::contains("run the tests"))
            .stdout(predicate::str::contains("All green."))
            .stdout(predicate::str::contains("(gpt-test)"))
            .stdout(predicate::str::contains("shell"))
            .stdout(predicate::str::contains("Plan the test run"));
    }

    #[test]
    fn file_history_separates_applied_changes_from_failed_attempts() {
        let claude = setup_fixture_dir();
        let codex = file_changes_home();
        let output = snatch_cmd()
            .env("SNATCH_CLAUDE_DIR", claude.path())
            .env("CODEX_HOME", codex.path())
            .args(["-o", "json", "file-history", "src/", "--provider", "codex"])
            .assert()
            .success()
            .get_output()
            .stdout
            .clone();
        let value: serde_json::Value = serde_json::from_slice(&output).unwrap();
        assert_eq!(value["total_modifications"], 1);
        assert_eq!(value["total_attempts"], 1);
        assert_eq!(value["modifications"][0]["outcome"], "applied");
        assert_eq!(value["modifications"][0]["move_path"], "src/new.rs");
        assert_eq!(value["attempts"][0]["outcome"], "failed");
        assert!(value["modifications"][0]["version"].is_null());
        assert!(value["coverage_note"]
            .as_str()
            .unwrap()
            .contains("shell writes"));

        let limited = snatch_cmd()
            .env("SNATCH_CLAUDE_DIR", claude.path())
            .env("CODEX_HOME", codex.path())
            .args([
                "-o",
                "json",
                "file-history",
                "src/",
                "--provider",
                "codex",
                "--limit",
                "1",
            ])
            .assert()
            .success()
            .get_output()
            .stdout
            .clone();
        let limited: serde_json::Value = serde_json::from_slice(&limited).unwrap();
        assert_eq!(limited["returned"], 1);
        assert_eq!(limited["modifications"].as_array().unwrap().len(), 0);
        assert_eq!(limited["attempts"][0]["outcome"], "failed");
    }

    #[test]
    fn file_evolution_uses_provider_entry_identity_without_fabricating_versions() {
        let claude = setup_fixture_dir();
        let codex = file_changes_home();
        let output = snatch_cmd()
            .env("SNATCH_CLAUDE_DIR", claude.path())
            .env("CODEX_HOME", codex.path())
            .args([
                "-o",
                "json",
                "file-evolution",
                "src/",
                "/tmp/p",
                "--provider",
                "codex",
            ])
            .assert()
            .success()
            .get_output()
            .stdout
            .clone();
        let value: serde_json::Value = serde_json::from_slice(&output).unwrap();
        assert_eq!(value["files"].as_array().unwrap().len(), 2);
        let applied = value["files"]
            .as_array()
            .unwrap()
            .iter()
            .find(|file| file["total_changes"] == 1)
            .unwrap();
        assert_eq!(applied["changes"][0]["provider"], "codex");
        assert_eq!(applied["changes"][0]["operation_id"], "patch-ok");
        assert!(applied["changes"][0]["version"].is_null());
        let failed = value["files"]
            .as_array()
            .unwrap()
            .iter()
            .find(|file| file["total_attempts"] == 1)
            .unwrap();
        assert_eq!(failed["attempts"][0]["outcome"], "failed");
    }

    #[test]
    fn provider_health_uses_typed_changes_and_honest_registry_coverage() {
        let claude = setup_fixture_dir();
        let codex = file_changes_home();
        let output = snatch_cmd()
            .env("SNATCH_CLAUDE_DIR", claude.path())
            .env("CODEX_HOME", codex.path())
            .args(["-o", "json", "health", "/tmp/p", "--provider", "codex"])
            .assert()
            .success()
            .get_output()
            .stdout
            .clone();
        let value: serde_json::Value = serde_json::from_slice(&output).unwrap();
        assert_eq!(value["providers"], serde_json::json!(["codex"]));
        assert_eq!(value["sessions_analyzed"], 1);
        assert_eq!(value["session_descriptors_analyzed"], 1);
        assert_eq!(value["total_tool_calls"], 2);
        assert_eq!(value["confirmed_tool_failures"], 0);
        assert_eq!(value["inferred_failure_signals"], 1);
        assert_eq!(value["hotspot_files"].as_array().unwrap().len(), 1);
        assert_eq!(value["hotspot_files"][0]["path"], "src/old.rs");
        assert_eq!(value["hotspot_files"][0]["edit_count"], 1);
        assert!(value["rework_files"].as_array().unwrap().is_empty());
        assert_eq!(value["registry_coverage"]["available"], false);
        assert!(value["decision_churn"].is_null());
        assert!(value["coverage_note"]
            .as_str()
            .unwrap()
            .contains("shell writes"));

        let lessons = snatch_cmd()
            .env("SNATCH_CLAUDE_DIR", claude.path())
            .env("CODEX_HOME", codex.path())
            .args([
                "-o",
                "json",
                "lessons",
                "--project",
                "/tmp/p",
                "--provider",
                "codex",
                "--category",
                "errors",
            ])
            .assert()
            .success()
            .get_output()
            .stdout
            .clone();
        let lessons: serde_json::Value = serde_json::from_slice(&lessons).unwrap();
        assert_eq!(
            value["total_tool_failures"],
            lessons["summary"]["total_errors"]
        );
        assert_eq!(
            value["confirmed_tool_failures"],
            lessons["summary"]["confirmed_tool_failures"]
        );
        assert_eq!(
            value["inferred_failure_signals"],
            lessons["summary"]["inferred_failure_signals"]
        );
    }

    #[test]
    fn provider_standup_uses_logical_sessions_and_typed_activity() {
        let claude = setup_fixture_dir();
        let codex = file_changes_home();
        let output = snatch_cmd()
            .env("SNATCH_CLAUDE_DIR", claude.path())
            .env("CODEX_HOME", codex.path())
            .args([
                "-o",
                "json",
                "standup",
                "--period",
                "30d",
                "--provider",
                "codex",
                "--all",
            ])
            .assert()
            .success()
            .get_output()
            .stdout
            .clone();
        let value: serde_json::Value = serde_json::from_slice(&output).unwrap();
        assert_eq!(value["providers"], serde_json::json!(["codex"]));
        assert_eq!(value["total_sessions"], 1);
        assert_eq!(value["session_descriptors_analyzed"], 1);
        assert_eq!(value["projects"][0]["sessions"], 1);
        assert_eq!(value["files"]["files_created"], 0);
        assert_eq!(value["files"]["files_modified"], 1);
        assert_eq!(value["files"]["files_deleted"], 0);
        assert_eq!(value["files"]["recognized_files_read"], 0);
        assert_eq!(
            value["files"]["unique_files"],
            serde_json::json!(["new.rs"])
        );
        assert_eq!(value["tools"]["total_invocations"], 2);
        assert_eq!(
            value["tools"]["by_tool"][0],
            serde_json::json!(["file-write", 2])
        );
        assert!(value.get("usage").is_none());
        assert!(value["coverage_note"]
            .as_str()
            .unwrap()
            .contains("shell writes"));
    }

    #[test]
    fn provider_standup_all_is_partial_but_explicit_failure_is_atomic() {
        let claude = setup_fixture_dir();
        let missing_codex = claude.path().join("missing-codex-home");
        let output = snatch_cmd()
            .env("SNATCH_CLAUDE_DIR", claude.path())
            .env("CODEX_HOME", &missing_codex)
            .args([
                "-o",
                "json",
                "standup",
                "--period",
                "30d",
                "--provider",
                "all",
            ])
            .assert()
            .success()
            .get_output()
            .stdout
            .clone();
        let value: serde_json::Value = serde_json::from_slice(&output).unwrap();
        assert_eq!(value["providers"], serde_json::json!(["claude-code"]));
        assert_eq!(value["total_sessions"], 1);
        assert_eq!(value["skipped_providers"][0]["provider"], "codex");

        snatch_cmd()
            .env("SNATCH_CLAUDE_DIR", claude.path())
            .env("CODEX_HOME", &missing_codex)
            .args(["standup", "--period", "30d", "--provider", "codex"])
            .assert()
            .failure();
    }

    #[test]
    fn provider_picker_refuses_open_without_a_path_capability() {
        let claude = setup_fixture_dir();
        let codex = file_changes_home();
        snatch_cmd()
            .env("SNATCH_CLAUDE_DIR", claude.path())
            .env("CODEX_HOME", codex.path())
            .args(["pick", "--provider", "codex", "--action", "open"])
            .assert()
            .failure()
            .stderr(predicates::str::contains(
                "do not promise a local source path",
            ));
    }

    #[test]
    fn provider_priorities_cluster_failures_without_fabricating_registry_zeroes() {
        let claude = setup_fixture_dir();
        let codex = file_changes_home();
        let original = std::fs::read_dir(codex.path().join("sessions/2026/07/16"))
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path();
        let content = std::fs::read_to_string(&original).unwrap();
        let sibling = "019f7000-0000-7000-8000-000000000123";
        std::fs::write(
            original
                .parent()
                .unwrap()
                .join(format!("rollout-2026-07-16T11-00-00-{sibling}.jsonl")),
            content.replace(THREAD, sibling),
        )
        .unwrap();

        let output = snatch_cmd()
            .env("SNATCH_CLAUDE_DIR", claude.path())
            .env("CODEX_HOME", codex.path())
            .args(["-o", "json", "priorities", "/tmp/p", "--provider", "codex"])
            .assert()
            .success()
            .get_output()
            .stdout
            .clone();
        let value: serde_json::Value = serde_json::from_slice(&output).unwrap();
        assert_eq!(value["sessions_analyzed"], 2);
        assert_eq!(value["session_descriptors_analyzed"], 2);
        assert_eq!(value["total_tool_failures"], 2);
        assert!(value["open_goals"].is_null());
        assert!(value["proposed_decisions"].is_null());
        assert_eq!(value["registry_coverage"]["goals_available"], false);
        assert_eq!(value["registry_coverage"]["decisions_available"], false);
        assert_eq!(value["priorities"][0]["category"], "reliability");
        assert!(value["priorities"][0]["summary"]
            .as_str()
            .unwrap()
            .contains("2x across 2 sessions"));
    }

    #[test]
    fn provider_health_all_is_partial_but_explicit_failure_is_atomic() {
        let claude = setup_fixture_dir();
        let missing_codex = claude.path().join("missing-codex-home");
        let output = snatch_cmd()
            .env("SNATCH_CLAUDE_DIR", claude.path())
            .env("CODEX_HOME", &missing_codex)
            .args(["-o", "json", "health", PROJECT_PATH, "--provider", "all"])
            .assert()
            .success()
            .get_output()
            .stdout
            .clone();
        let value: serde_json::Value = serde_json::from_slice(&output).unwrap();
        assert_eq!(value["providers"], serde_json::json!(["claude-code"]));
        assert_eq!(value["sessions_analyzed"], 1);
        assert_eq!(value["skipped_providers"][0]["provider"], "codex");

        snatch_cmd()
            .env("SNATCH_CLAUDE_DIR", claude.path())
            .env("CODEX_HOME", &missing_codex)
            .args(["health", PROJECT_PATH, "--provider", "codex"])
            .assert()
            .failure();
    }

    #[test]
    fn info_separates_native_usage_axes_and_keeps_codex_unpriced() {
        let claude = setup_fixture_dir();
        let codex = slice_home();
        let output = snatch_cmd()
            .env("SNATCH_CLAUDE_DIR", claude.path())
            .env("CODEX_HOME", codex.path())
            .args(["-o", "json", "info", &format!("codex:{THREAD}")])
            .assert()
            .success()
            .get_output()
            .stdout
            .clone();
        let value: serde_json::Value = serde_json::from_slice(&output).unwrap();
        assert_eq!(value["usage"]["observation_counts"]["call/delta"], 1);
        assert_eq!(
            value["usage"]["observation_counts"]["session/cumulative"],
            1
        );
        assert_eq!(value["usage"]["canonical"]["input_tokens"], 40);
        assert_eq!(value["usage"]["canonical"]["cache_read_tokens"], 60);
        assert_eq!(value["usage"]["canonical"]["output_tokens"], 25);
        assert_eq!(value["usage"]["canonical"]["total_processed_tokens"], 125);
        assert_eq!(value["usage"]["pricing"]["policy"], "unpriced");
        assert!(value["usage"]["pricing"]["estimated_cost"].is_null());
        assert_eq!(
            value["usage"]["pricing"]["unpriced_models"],
            serde_json::json!(["gpt-test"])
        );
    }

    #[test]
    fn stats_routes_qualified_session_with_canonical_unpriced_usage() {
        let claude = setup_fixture_dir();
        let codex = slice_home();
        let qualified = format!("codex:{THREAD}");
        let output = snatch_cmd()
            .env("SNATCH_CLAUDE_DIR", claude.path())
            .env("CODEX_HOME", codex.path())
            .args(["-o", "json", "stats", &qualified])
            .assert()
            .success()
            .get_output()
            .stdout
            .clone();
        let value: serde_json::Value = serde_json::from_slice(&output).unwrap();
        assert_eq!(value["provider"], "codex");
        assert_eq!(value["qualified_id"], qualified);
        assert_eq!(value["pricing_policy"], "unpriced");
        assert!(value["estimated_cost"].is_null());
        assert_eq!(value["unpriced_models"], serde_json::json!(["gpt-test"]));
        assert_eq!(value["input_tokens"], 40);
        assert_eq!(value["cache_read_tokens"], 60);
        assert_eq!(value["output_tokens"], 25);
        assert_eq!(value["total_tokens"], 65);
        assert_eq!(value["total_processed_tokens"], 125);
        // Canonical message entries: prompt, reasoning, tool call, tool
        // result, and assistant text. Turns are a separate semantic axis.
        assert_eq!(value["messages"], 5);
        assert_eq!(value["tool_invocations"], 1);
    }

    #[test]
    fn provider_stats_require_a_session_and_refuse_every_unsupported_mode() {
        let claude = setup_fixture_dir();
        let codex = slice_home();
        snatch_cmd()
            .env("SNATCH_CLAUDE_DIR", claude.path())
            .env("CODEX_HOME", codex.path())
            .args(["stats", "--provider", "codex"])
            .assert()
            .failure()
            .stderr(predicate::str::contains("requires one session"));

        let qualified = format!("codex:{THREAD}");
        let unsupported: &[(&str, &[&str])] = &[
            ("--project", &["--project", "some-project"]),
            ("--global", &["--global"]),
            ("--costs", &["--costs"]),
            ("--blocks", &["--blocks"]),
            ("--token-limit", &["--token-limit", "100"]),
            ("--sparkline", &["--sparkline"]),
            ("--history", &["--history"]),
            ("--record", &["--record"]),
            ("--weekly", &["--weekly"]),
            ("--monthly", &["--monthly"]),
            ("--csv", &["--csv"]),
            ("--clear-history", &["--clear-history"]),
            ("--timeline", &["--timeline"]),
            ("--graph", &["--graph"]),
        ];
        for (flag, extra) in unsupported {
            snatch_cmd()
                .env("SNATCH_CLAUDE_DIR", claude.path())
                .env("CODEX_HOME", codex.path())
                .args(["stats", &qualified, "--provider", "codex"])
                .args(*extra)
                .assert()
                .failure()
                .stderr(predicate::str::contains(*flag));
        }
    }

    #[test]
    fn prompts_use_native_authorship_but_preserve_the_complete_human_text() {
        let claude = setup_fixture_dir();
        let codex = content_provenance_home();
        let qualified = format!("codex:{THREAD}");
        let output = snatch_cmd()
            .env("SNATCH_CLAUDE_DIR", claude.path())
            .env("CODEX_HOME", codex.path())
            .args(["-o", "json", "prompts", &qualified])
            .assert()
            .success()
            .get_output()
            .stdout
            .clone();
        let value: serde_json::Value = serde_json::from_slice(&output).unwrap();
        assert_eq!(value["provider"], "codex");
        assert_eq!(value["qualified_id"], qualified);
        assert_eq!(value["total_count"], 2);
        let rendered = value["prompts"].to_string();
        assert!(rendered.contains("let human = true"));
        assert!(rendered.contains("relayed but still part of my prompt"));
        assert!(rendered.contains("package main"));
        assert!(!rendered.contains("let harness = true"));
    }

    #[test]
    fn provider_prompt_analysis_modes_keep_source_identity() {
        let claude = setup_fixture_dir();
        let codex = content_provenance_home();
        let qualified = format!("codex:{THREAD}");
        let modes: &[&[&str]] = &[
            &["--stats"],
            &["--frequency", "--min-count", "1", "--sort-by-length"],
            &["--contains", "human"],
        ];
        for mode in modes {
            let output = snatch_cmd()
                .env("SNATCH_CLAUDE_DIR", claude.path())
                .env("CODEX_HOME", codex.path())
                .args(["-o", "json", "prompts", &qualified])
                .args(*mode)
                .assert()
                .success()
                .get_output()
                .stdout
                .clone();
            let value: serde_json::Value = serde_json::from_slice(&output).unwrap();
            assert_eq!(value["provider"], "codex");
            assert_eq!(value["qualified_id"], qualified);
        }
    }

    #[test]
    fn code_extraction_excludes_harness_user_code_and_keeps_human_and_assistant_code() {
        let claude = setup_fixture_dir();
        let codex = content_provenance_home();
        let qualified = format!("codex:{THREAD}");
        let run = |user_only: bool| {
            let mut command = snatch_cmd();
            command
                .env("SNATCH_CLAUDE_DIR", claude.path())
                .env("CODEX_HOME", codex.path())
                .args(["-o", "json", "code", &qualified]);
            if user_only {
                command.arg("--user-only");
            }
            let output = command.assert().success().get_output().stdout.clone();
            serde_json::from_slice::<serde_json::Value>(&output).unwrap()
        };

        let all = run(false);
        let blocks = all.as_array().unwrap();
        assert_eq!(blocks.len(), 3);
        assert!(blocks.iter().all(|block| block["provider"] == "codex"));
        assert!(blocks
            .iter()
            .all(|block| block["qualified_id"] == qualified));
        let rendered = all.to_string();
        assert!(rendered.contains("let human = true"));
        assert!(rendered.contains("print('assistant')"));
        assert!(rendered.contains("package main"));
        assert!(!rendered.contains("let harness = true"));

        let user = run(true);
        assert_eq!(user.as_array().unwrap().len(), 2);
        assert!(!user.to_string().contains("print('assistant')"));
    }

    #[test]
    fn provider_prompts_require_union_scope_and_single_session_refuses_cross_flags() {
        let claude = setup_fixture_dir();
        let codex = content_provenance_home();
        snatch_cmd()
            .env("SNATCH_CLAUDE_DIR", claude.path())
            .env("CODEX_HOME", codex.path())
            .args(["prompts", "--provider", "codex"])
            .assert()
            .failure()
            .stderr(predicate::str::contains("Specify a session ID"));

        let qualified = format!("codex:{THREAD}");
        let unsupported: &[(&str, &[&str])] = &[
            ("--all", &["--all"]),
            ("--project", &["--project", "some-project"]),
            ("--since", &["--since", "2026-01-01"]),
            ("--until", &["--until", "2026-12-31"]),
            ("--subagents", &["--subagents"]),
            ("--separators", &["--separators"]),
        ];
        for (flag, extra) in unsupported {
            snatch_cmd()
                .env("SNATCH_CLAUDE_DIR", claude.path())
                .env("CODEX_HOME", codex.path())
                .args(["prompts", &qualified, "--provider", "codex"])
                .args(*extra)
                .assert()
                .failure()
                .stderr(predicate::str::contains(*flag));
        }
    }

    #[test]
    fn provider_prompt_union_uses_new_work_and_typed_spawn_projection() {
        let claude = setup_fixture_dir();
        let codex = fork_summary_home();
        let run = |extra: &[&str]| {
            let output = snatch_cmd()
                .env("SNATCH_CLAUDE_DIR", claude.path())
                .env("CODEX_HOME", codex.path())
                .args(["-o", "json", "prompts", "--provider", "codex", "--all"])
                .args(extra)
                .assert()
                .success()
                .get_output()
                .stdout
                .clone();
            serde_json::from_slice::<serde_json::Value>(&output).unwrap()
        };

        let value = run(&[]);
        assert_eq!(value["providers"], serde_json::json!(["codex"]));
        assert_eq!(value["session_descriptors_analyzed"], 2);
        assert_eq!(value["session_count"], 2);
        assert_eq!(value["total_count"], 2);
        assert_eq!(value["skipped_providers"], serde_json::json!([]));
        let prompts = value["prompts"].as_array().unwrap();
        assert_eq!(prompts.len(), 2);
        assert_eq!(prompts[0]["text"], "parent prompt");
        assert_eq!(prompts[1]["text"], "fork prompt");
        assert!(prompts.iter().all(|prompt| prompt["provider"] == "codex"));
        assert!(prompts
            .iter()
            .all(|prompt| prompt["session_id"].as_str().unwrap().starts_with("codex:")));
        assert!(prompts
            .iter()
            .all(|prompt| prompt["qualified_id"] == prompt["session_id"]));
        assert!(prompts
            .iter()
            .all(|prompt| prompt["project_key"].is_string()));

        let limited = run(&["--limit", "1"]);
        assert_eq!(limited["total_count"], 2);
        assert_eq!(limited["prompts"].as_array().unwrap().len(), 1);
        assert_eq!(limited["prompts"][0]["text"], "parent prompt");

        let with_spawn = run(&["--subagents"]);
        assert_eq!(with_spawn["session_descriptors_analyzed"], 3);
        assert_eq!(with_spawn["session_count"], 3);
        assert_eq!(with_spawn["total_count"], 3);
        assert_eq!(with_spawn["prompts"][2]["text"], "spawn prompt");

        let output = snatch_cmd()
            .env("SNATCH_CLAUDE_DIR", claude.path())
            .env("CODEX_HOME", codex.path())
            .args([
                "-o",
                "json",
                "prompts",
                "--provider",
                "codex",
                "--project",
                "fork-summary",
            ])
            .assert()
            .success()
            .get_output()
            .stdout
            .clone();
        let project: serde_json::Value = serde_json::from_slice(&output).unwrap();
        assert_eq!(project["total_count"], 2);
        assert_eq!(project["session_count"], 2);
    }

    #[test]
    fn provider_prompt_union_filters_by_native_time_before_parsing() {
        let claude = setup_fixture_dir();
        let codex = fork_summary_home();
        let output = snatch_cmd()
            .env("SNATCH_CLAUDE_DIR", claude.path())
            .env("CODEX_HOME", codex.path())
            .args([
                "-o",
                "json",
                "prompts",
                "--provider",
                "codex",
                "--all",
                "--since",
                "2026-07-17",
            ])
            .assert()
            .success()
            .get_output()
            .stdout
            .clone();
        let value: serde_json::Value = serde_json::from_slice(&output).unwrap();
        assert_eq!(value["session_descriptors_analyzed"], 0);
        assert_eq!(value["session_count"], 0);
        assert_eq!(value["total_count"], 0);
        assert_eq!(value["prompts"], serde_json::json!([]));
    }

    #[test]
    fn provider_prompt_union_surfaces_conservative_date_fallbacks() {
        let claude = setup_fixture_dir();
        let codex = compressed_summary_home();
        let output = snatch_cmd()
            .env("SNATCH_CLAUDE_DIR", claude.path())
            .env("CODEX_HOME", codex.path())
            .args([
                "-o",
                "json",
                "prompts",
                "--provider",
                "codex",
                "--all",
                "--since",
                "2026-07-17",
            ])
            .assert()
            .success()
            .stderr(predicate::str::contains(
                "used conservative source-time evidence",
            ))
            .get_output()
            .stdout
            .clone();
        let value: serde_json::Value = serde_json::from_slice(&output).unwrap();
        assert_eq!(value["session_descriptors_analyzed"], 1);
        assert_eq!(value["date_filter_fallback_descriptors"], 1);
        assert_eq!(value["total_count"], 1);
        assert_eq!(value["prompts"][0]["text"], "compressed prompt");
        assert_eq!(value["warnings"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn provider_prompt_union_excludes_harness_context_and_reports_partial_all() {
        let claude = setup_fixture_dir();
        let codex = content_provenance_home();
        let output = snatch_cmd()
            .env("SNATCH_CLAUDE_DIR", claude.path())
            .env("CODEX_HOME", codex.path())
            .args(["-o", "json", "prompts", "--provider", "codex", "--all"])
            .assert()
            .success()
            .get_output()
            .stdout
            .clone();
        let value: serde_json::Value = serde_json::from_slice(&output).unwrap();
        assert_eq!(value["total_count"], 2);
        let rendered = value["prompts"].to_string();
        assert!(rendered.contains("let human = true"));
        assert!(rendered.contains("package main"));
        assert!(!rendered.contains("let harness = true"));

        let output = snatch_cmd()
            .env("SNATCH_CLAUDE_DIR", claude.path())
            .env("CODEX_HOME", claude.path().join("missing-codex-home"))
            .args(["-o", "json", "prompts", "--provider", "all", "--all"])
            .assert()
            .success()
            .stderr(predicate::str::contains("provider 'codex' skipped"))
            .get_output()
            .stdout
            .clone();
        let partial: serde_json::Value = serde_json::from_slice(&output).unwrap();
        assert_eq!(partial["providers"], serde_json::json!(["claude-code"]));
        assert_eq!(partial["skipped_providers"][0]["provider"], "codex");
        assert!(partial["total_count"].as_u64().unwrap() > 0);

        snatch_cmd()
            .env("SNATCH_CLAUDE_DIR", claude.path())
            .env("CODEX_HOME", claude.path().join("missing-codex-home"))
            .args(["-o", "json", "prompts", "--provider", "codex", "--all"])
            .assert()
            .failure()
            .stderr(predicate::str::contains("not found"));
    }

    #[test]
    fn provider_prompt_union_json_modes_report_collection_coverage() {
        let claude = setup_fixture_dir();
        let codex = fork_summary_home();
        let modes: &[&[&str]] = &[
            &[],
            &["--stats"],
            &["--frequency"],
            &["--contains", "prompt"],
        ];
        for mode in modes {
            let output = snatch_cmd()
                .env("SNATCH_CLAUDE_DIR", claude.path())
                .env("CODEX_HOME", codex.path())
                .args(["-o", "json", "prompts", "--provider", "codex", "--all"])
                .args(*mode)
                .assert()
                .success()
                .get_output()
                .stdout
                .clone();
            let value: serde_json::Value = serde_json::from_slice(&output).unwrap();
            assert_eq!(value["providers"], serde_json::json!(["codex"]));
            assert_eq!(value["session_descriptors_analyzed"], 2);
            assert_eq!(value["date_filter_fallback_descriptors"], 0);
            assert_eq!(value["session_count"], 2);
            assert_eq!(value["skipped_providers"], serde_json::json!([]));
            assert_eq!(value["warnings"], serde_json::json!([]));
        }
    }

    #[test]
    fn event_context_uses_compact_semantic_turns_and_typed_evidence() {
        let claude = setup_fixture_dir();
        let codex = event_context_home();
        let qualified = format!("codex:{THREAD}");
        let output = snatch_cmd()
            .env("SNATCH_CLAUDE_DIR", claude.path())
            .env("CODEX_HOME", codex.path())
            .args([
                "-o",
                "json",
                "context",
                THREAD,
                "--provider",
                "codex",
                "--timestamp",
                "2026-07-16T12:01:03Z",
                "--context-window",
                "1",
            ])
            .assert()
            .success()
            .get_output()
            .stdout
            .clone();
        let value: serde_json::Value = serde_json::from_slice(&output).unwrap();
        assert_eq!(value["provider"], "codex");
        assert_eq!(value["qualified_id"], qualified);
        assert_eq!(value["session_id"], THREAD);
        assert_eq!(value["before"], serde_json::json!([]));
        assert_eq!(value["after"], serde_json::json!([]));
        assert_eq!(
            value["semantic_window"]["before"].as_array().unwrap().len(),
            1
        );
        assert_eq!(
            value["semantic_window"]["after"].as_array().unwrap().len(),
            1
        );
        let focus = &value["semantic_window"]["focus"];
        assert_eq!(focus["turn_id"], "turn-focus");
        assert_eq!(focus["user_prompt"], "focus prompt");
        assert_eq!(focus["steering_prompts"][0], "focus steering");
        assert_eq!(focus["assistant_response"], "focus response");
        assert!(focus["event_count"].as_u64().unwrap() > 4);
        assert!(!focus.to_string().contains("harness-only context"));
        assert!(focus.get("related_files").is_none());
        assert_eq!(value["confirmed_failure_count"], 1);
        assert_eq!(value["inferred_failure_count"], 0);
        assert_eq!(value["error_count"], 1);
        assert!(value["related_files"]
            .as_array()
            .unwrap()
            .iter()
            .any(|path| path == "src/changed.rs"));
        assert!(value["related_files"]
            .as_array()
            .unwrap()
            .iter()
            .any(|path| path == "src/moved.rs"));

        // A directly targeted harness entry is still findable even though it
        // is deliberately absent from the canonical conversational turns.
        // It gets a one-event derived focus between the neighboring turns.
        let harness = snatch_cmd()
            .env("SNATCH_CLAUDE_DIR", claude.path())
            .env("CODEX_HOME", codex.path())
            .args([
                "-o",
                "json",
                "context",
                &qualified,
                "--timestamp",
                "2026-07-16T12:01:01Z",
                "--context-window",
                "1",
            ])
            .assert()
            .success()
            .get_output()
            .stdout
            .clone();
        let harness: serde_json::Value = serde_json::from_slice(&harness).unwrap();
        assert_eq!(harness["target"]["text"], "harness-only context");
        assert_eq!(harness["semantic_window"]["focus"]["event_count"], 1);
        assert!(harness["semantic_window"]["focus"]["user_prompt"].is_null());
        assert_eq!(
            harness["semantic_window"]["before"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
        assert_eq!(
            harness["semantic_window"]["after"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
    }

    #[test]
    fn provider_routed_claude_event_context_preserves_classic_output() {
        let claude = setup_fixture_dir();
        let missing_codex = claude.path().join("no-codex-home");
        let run = |provider: bool| {
            let mut command = snatch_cmd();
            command
                .env("SNATCH_CLAUDE_DIR", claude.path())
                .env("CODEX_HOME", &missing_codex)
                .args([
                    "-o",
                    "json",
                    "context",
                    SESSION_ID,
                    "--message-id",
                    "22222222",
                ]);
            if provider {
                command.args(["--provider", "claude-code"]);
            }
            let output = command.assert().success().get_output().stdout.clone();
            serde_json::from_slice::<serde_json::Value>(&output).unwrap()
        };
        let classic = run(false);
        let mut routed = run(true);
        assert_eq!(routed["provider"], "claude-code");
        assert_eq!(routed["qualified_id"], format!("claude-code:{SESSION_ID}"));
        assert!(routed.get("semantic_window").is_none());
        routed.as_object_mut().unwrap().remove("provider");
        routed.as_object_mut().unwrap().remove("qualified_id");
        assert_eq!(routed, classic);
    }

    #[test]
    fn lessons_use_tool_and_prompt_semantics_without_content_false_positives() {
        let claude = setup_fixture_dir();
        let codex = lessons_home();
        let output = snatch_cmd()
            .env("SNATCH_CLAUDE_DIR", claude.path())
            .env("CODEX_HOME", codex.path())
            .args(["-o", "json", "lessons", &format!("codex:{THREAD}")])
            .assert()
            .success()
            .get_output()
            .stdout
            .clone();
        let value: serde_json::Value = serde_json::from_slice(&output).unwrap();
        assert_eq!(value["provider"], "codex");
        assert_eq!(value["qualified_id"], format!("codex:{THREAD}"));
        assert_eq!(value["summary"]["total_errors"], 2);
        assert_eq!(value["summary"]["total_corrections"], 1);
        let tools: std::collections::BTreeSet<_> = value["error_fix_pairs"]
            .as_array()
            .unwrap()
            .iter()
            .map(|pair| pair["tool_name"].as_str().unwrap())
            .collect();
        assert_eq!(tools, ["apply_patch", "exec_command"].into_iter().collect());
        assert_eq!(
            value["user_corrections"][0]["user_text"],
            "No, use the exact context instead"
        );
        assert_eq!(
            value["user_corrections"][0]["correction_basis"],
            "explicit_rejection"
        );

        let filtered = snatch_cmd()
            .env("SNATCH_CLAUDE_DIR", claude.path())
            .env("CODEX_HOME", codex.path())
            .args([
                "-o",
                "json",
                "lessons",
                &format!("codex:{THREAD}"),
                "--category",
                "corrections",
            ])
            .assert()
            .success()
            .get_output()
            .stdout
            .clone();
        let filtered: serde_json::Value = serde_json::from_slice(&filtered).unwrap();
        assert_eq!(filtered["summary"]["total_errors"], 2);
        assert_eq!(filtered["summary"]["total_corrections"], 1);
        assert!(filtered.get("error_fix_pairs").is_none());
    }

    #[test]
    fn cross_session_lessons_use_provider_union_and_reject_inert_qualified_ids() {
        let claude = setup_fixture_dir();
        let codex = lessons_home();
        let output = snatch_cmd()
            .env("SNATCH_CLAUDE_DIR", claude.path())
            .env("CODEX_HOME", codex.path())
            .args(["-o", "json", "lessons", "--all", "--provider", "all"])
            .assert()
            .success()
            .get_output()
            .stdout
            .clone();
        let value: serde_json::Value = serde_json::from_slice(&output).unwrap();
        assert_eq!(
            value["providers"],
            serde_json::json!(["claude-code", "codex"])
        );
        assert_eq!(value["sessions_scanned"], 2);
        assert_eq!(value["summary"]["total_errors"], 2);
        assert_eq!(value["summary"]["total_corrections"], 1);
        assert_eq!(value["activity_basis"], "new-activity-only");
        assert_eq!(
            value["error_fix_pairs"][0]["session_id"],
            format!("codex:{THREAD}")
        );

        let projected = snatch_cmd()
            .env("SNATCH_CLAUDE_DIR", claude.path())
            .env("CODEX_HOME", codex.path())
            .args([
                "-o",
                "json",
                "lessons",
                "--all",
                "--provider",
                "all",
                "--category",
                "corrections",
            ])
            .assert()
            .success()
            .get_output()
            .stdout
            .clone();
        let projected: serde_json::Value = serde_json::from_slice(&projected).unwrap();
        assert_eq!(projected["summary"]["total_errors"], 2);
        assert_eq!(projected["summary"]["total_corrections"], 1);
        assert!(projected["error_fix_pairs"].as_array().unwrap().is_empty());

        snatch_cmd()
            .env("SNATCH_CLAUDE_DIR", claude.path())
            .env("CODEX_HOME", codex.path())
            .args(["lessons", &format!("codex:{THREAD}"), "--all"])
            .assert()
            .failure()
            .stderr(predicate::str::contains("cannot be combined"));
    }

    #[test]
    fn timeline_renders_normalized_codex_turns() {
        let claude = setup_fixture_dir();
        let codex = slice_home();
        let output = snatch_cmd()
            .env("SNATCH_CLAUDE_DIR", claude.path())
            .env("CODEX_HOME", codex.path())
            .args(["-o", "json", "timeline", &format!("codex:{THREAD}")])
            .assert()
            .success()
            .get_output()
            .stdout
            .clone();
        let value: serde_json::Value = serde_json::from_slice(&output).unwrap();
        assert_eq!(value["timeline"][0]["user_prompt"], "run the tests");
        assert_eq!(value["timeline"][0]["assistant_summary"], "All green.");
        assert_eq!(value["compaction_events"][0]["kind"], "full");
        assert_eq!(
            value["compaction_events"][0]["replacement_history_items"],
            0
        );
        assert_eq!(value["compaction_events"][0]["window"]["number"], 2);
        assert_eq!(value["compaction_events"][0]["window"]["id"], "w2");
        assert_eq!(
            value["compaction_events"][0]["window"]["legacy_numeric_id"],
            false
        );
    }

    #[test]
    fn chunks_lists_semantic_codex_boundaries() {
        let claude = setup_fixture_dir();
        let codex = slice_home();
        let out = snatch_cmd()
            .env("SNATCH_CLAUDE_DIR", claude.path())
            .env("CODEX_HOME", codex.path())
            .args(["-o", "json", "chunks", &format!("codex:{THREAD}")])
            .assert()
            .success()
            .get_output()
            .stdout
            .clone();
        let value: serde_json::Value = serde_json::from_slice(&out).unwrap();
        assert_eq!(value["provider"], "codex");
        assert_eq!(value["qualified_id"], format!("codex:{THREAD}"));
        assert_eq!(value["total_chunks"], 1);
        assert_eq!(value["chunks"][0]["prompt"], "run the tests");

        snatch_cmd()
            .env("SNATCH_CLAUDE_DIR", claude.path())
            .env("CODEX_HOME", codex.path())
            .args(["chunks", &format!("codex:{THREAD}"), "--no-chain"])
            .assert()
            .failure()
            .stderr(predicate::str::contains("not supported with"));
    }

    #[test]
    fn timeline_reports_one_turn_for_one_human_prompt_amid_harness_context() {
        // Round-22 blocker 3: developer/environment context must not count
        // as human turns.
        let tmp = TempDir::new().unwrap();
        let day = tmp.path().join("sessions/2026/07/16");
        std::fs::create_dir_all(&day).unwrap();
        let lines = [
            serde_json::json!({"timestamp": "2026-07-16T10:00:00.000Z", "type": "session_meta",
                "payload": {"id": THREAD, "cwd": "/tmp/p", "cli_version": "0.9"}}),
            serde_json::json!({"timestamp": "2026-07-16T10:00:01.000Z", "type": "response_item",
                "payload": {"type": "message", "role": "developer",
                            "content": [{"type": "input_text", "text": "<permissions instructions>"}]}}),
            serde_json::json!({"timestamp": "2026-07-16T10:00:01.100Z", "type": "response_item",
                "payload": {"type": "message", "role": "user",
                            "content": [{"type": "input_text", "text": "<environment_context>"}]}}),
            serde_json::json!({"timestamp": "2026-07-16T10:00:02.000Z", "type": "response_item",
                "payload": {"type": "message", "role": "user",
                            "content": [{"type": "input_text", "text": "do the thing"}]}}),
            serde_json::json!({"timestamp": "2026-07-16T10:00:02.500Z", "type": "event_msg",
                "payload": {"type": "user_message", "message": "do the thing"}}),
            serde_json::json!({"timestamp": "2026-07-16T10:00:03.000Z", "type": "response_item",
                "payload": {"type": "message", "role": "assistant",
                            "content": [{"type": "output_text", "text": "done"}]}}),
        ];
        let content = lines
            .iter()
            .map(|l| l.to_string())
            .collect::<Vec<_>>()
            .join("\n")
            + "\n";
        std::fs::write(
            day.join(format!("rollout-2026-07-16T10-00-00-{THREAD}.jsonl")),
            content,
        )
        .unwrap();

        let claude = setup_fixture_dir();
        let out = snatch_cmd()
            .env("SNATCH_CLAUDE_DIR", claude.path())
            .env("CODEX_HOME", tmp.path())
            .args(["timeline", &format!("codex:{THREAD}")])
            .assert()
            .success()
            .get_output()
            .stdout
            .clone();
        let text = String::from_utf8_lossy(&out);
        assert!(text.contains("(1 turns)"), "got: {text}");
        assert!(text.contains("User: do the thing"), "got: {text}");
        assert!(
            !text.contains("<permissions") && !text.contains("<environment_context>"),
            "harness context must not appear as a turn: {text}"
        );
    }

    #[test]
    fn midturn_steering_renders_once_without_splitting_the_turn() {
        // B3 steering census: one of the two unique unmatched user_message
        // events in the 226-session corpus occurs between assistant
        // emissions in one native turn window. It must remain in that turn
        // AND render once in native human-message order.
        let tmp = TempDir::new().unwrap();
        let day = tmp.path().join("sessions/2026/07/16");
        std::fs::create_dir_all(&day).unwrap();
        let lines = [
            serde_json::json!({"timestamp": "2026-07-16T10:00:00.000Z", "type": "session_meta",
                "payload": {"id": THREAD, "cwd": "/tmp/p", "cli_version": "0.9"}}),
            serde_json::json!({"timestamp": "2026-07-16T10:00:01.000Z", "type": "turn_context",
                "payload": {"turn_id": "t-1", "model": "gpt-test"}}),
            serde_json::json!({"timestamp": "2026-07-16T10:00:02.000Z", "type": "response_item",
                "payload": {"type": "message", "role": "user",
                            "content": [{"type": "input_text", "text": "start the task"}]}}),
            serde_json::json!({"timestamp": "2026-07-16T10:00:02.500Z", "type": "event_msg",
                "payload": {"type": "user_message", "message": "start the task"}}),
            serde_json::json!({"timestamp": "2026-07-16T10:00:03.000Z", "type": "response_item",
                "payload": {"type": "message", "role": "assistant",
                            "content": [{"type": "output_text", "text": "working"}]}}),
            // Steering: user event with NO response twin, mid-window.
            serde_json::json!({"timestamp": "2026-07-16T10:00:04.000Z", "type": "event_msg",
                "payload": {"type": "user_message", "message": "also check the docs",
                            "images": [], "local_images": [], "text_elements": []}}),
            serde_json::json!({"timestamp": "2026-07-16T10:00:05.000Z", "type": "response_item",
                "payload": {"type": "message", "role": "assistant",
                            "content": [{"type": "output_text", "text": "done, docs checked"}]}}),
        ];
        let content = lines
            .iter()
            .map(|l| l.to_string())
            .collect::<Vec<_>>()
            .join("\n")
            + "\n";
        std::fs::write(
            day.join(format!("rollout-2026-07-16T10-00-00-{THREAD}.jsonl")),
            content,
        )
        .unwrap();

        let claude = setup_fixture_dir();
        let out = snatch_cmd()
            .env("SNATCH_CLAUDE_DIR", claude.path())
            .env("CODEX_HOME", tmp.path())
            .args(["timeline", &format!("codex:{THREAD}")])
            .assert()
            .success()
            .get_output()
            .stdout
            .clone();
        let text = String::from_utf8_lossy(&out);
        assert!(
            text.contains("(1 turns)"),
            "steering split the turn: {text}"
        );
        assert!(text.contains("User: start the task"), "got: {text}");
        assert_eq!(
            text.matches("Steering: also check the docs").count(),
            1,
            "steering prompt must render exactly once: {text}"
        );
        assert!(
            text.contains("done, docs checked"),
            "final answer belongs to the same turn: {text}"
        );
        let boundary = text.find("User: start the task").unwrap();
        let steering = text.find("Steering: also check the docs").unwrap();
        let answer = text.find("Assistant: done, docs checked").unwrap();
        assert!(
            boundary < steering && steering < answer,
            "human emissions and final answer must retain native order: {text}"
        );

        let json = snatch_cmd()
            .env("SNATCH_CLAUDE_DIR", claude.path())
            .env("CODEX_HOME", tmp.path())
            .args(["-o", "json", "timeline", &format!("codex:{THREAD}")])
            .assert()
            .success()
            .get_output()
            .stdout
            .clone();
        let value: serde_json::Value = serde_json::from_slice(&json).unwrap();
        assert_eq!(
            value["timeline"][0]["steering_prompts"],
            serde_json::json!(["also check the docs"])
        );

        let chunk = snatch_cmd()
            .env("SNATCH_CLAUDE_DIR", claude.path())
            .env("CODEX_HOME", tmp.path())
            .args([
                "messages",
                &format!("codex:{THREAD}"),
                "--chunk",
                "0",
                "--detail",
                "conversation",
            ])
            .assert()
            .success()
            .get_output()
            .stdout
            .clone();
        let chunk = String::from_utf8_lossy(&chunk);
        assert!(chunk.contains("start the task"), "got: {chunk}");
        assert!(chunk.contains("also check the docs"), "got: {chunk}");
        assert!(chunk.contains("done, docs checked"), "got: {chunk}");

        let overview = snatch_cmd()
            .env("SNATCH_CLAUDE_DIR", claude.path())
            .env("CODEX_HOME", tmp.path())
            .args([
                "messages",
                &format!("codex:{THREAD}"),
                "--detail",
                "overview",
            ])
            .assert()
            .success()
            .get_output()
            .stdout
            .clone();
        let overview = String::from_utf8_lossy(&overview);
        assert!(overview.contains("start the task"), "got: {overview}");
        assert!(
            !overview.contains("also check the docs"),
            "midturn steering is a chunk member, not a boundary: {overview}"
        );
    }

    #[test]
    fn messages_refuses_claude_machinery_flags_on_codex_sessions() {
        let claude = setup_fixture_dir();
        let codex = slice_home();
        for extra in [&["--no-chain"][..], &["--subagent-transcripts"]] {
            let mut args = vec!["messages", "codex:0198cccc"];
            args.extend_from_slice(extra);
            snatch_cmd()
                .env("SNATCH_CLAUDE_DIR", claude.path())
                .env("CODEX_HOME", codex.path())
                .args(&args)
                .assert()
                .failure()
                .stderr(predicate::str::contains("not supported with"));
        }
    }
}

/// Round-23 blocker 1: provider-routed CLAUDE sessions must not lose
/// prompts or collapse the timeline — the Claude adapter declares no
/// semantic coverage, so surfaces keep classic heuristics for it.
#[test]
fn provider_routed_claude_messages_and_timeline_match_classic() {
    let tmp = setup_fixture_dir();
    let qualified = format!("claude-code:{SESSION_ID}");

    let classic = snatch_cmd()
        .env("SNATCH_CLAUDE_DIR", tmp.path())
        .args(["messages", SESSION_ID, "--detail", "conversation"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let routed = snatch_cmd()
        .env("SNATCH_CLAUDE_DIR", tmp.path())
        .args(["messages", &qualified, "--detail", "conversation"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let classic_text = String::from_utf8_lossy(&classic);
    let routed_text = String::from_utf8_lossy(&routed);
    assert!(
        classic_text.contains("Hello, Claude!"),
        "classic shows the prompt: {classic_text}"
    );
    assert!(
        routed_text.contains("Hello, Claude!"),
        "provider route must retain the user prompt: {routed_text}"
    );

    let classic_tl = snatch_cmd()
        .env("SNATCH_CLAUDE_DIR", tmp.path())
        .args(["timeline", SESSION_ID])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let routed_tl = snatch_cmd()
        .env("SNATCH_CLAUDE_DIR", tmp.path())
        .args(["timeline", &qualified])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let classic_turns = String::from_utf8_lossy(&classic_tl);
    let routed_turns = String::from_utf8_lossy(&routed_tl);
    let count = |t: &str| {
        t.lines()
            .find(|l| l.contains("turns)"))
            .map(|l| l.to_string())
    };
    assert_eq!(
        count(&classic_turns).map(|l| l.split('(').nth(1).map(str::to_string)),
        count(&routed_turns).map(|l| l.split('(').nth(1).map(str::to_string)),
        "turn counts must match: classic={classic_turns} routed={routed_turns}"
    );
    assert!(
        routed_turns.contains("User: Hello, Claude!"),
        "provider-routed timeline must keep the human turn: {routed_turns}"
    );

    let classic_json = snatch_cmd()
        .env("SNATCH_CLAUDE_DIR", tmp.path())
        .args(["-o", "json", "timeline", SESSION_ID])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let classic_json: serde_json::Value = serde_json::from_slice(&classic_json).unwrap();
    assert!(
        classic_json.get("compaction_events").is_none(),
        "provider-only compaction metadata must not change flagless Claude JSON"
    );

    let classic_export = snatch_cmd()
        .env("SNATCH_CLAUDE_DIR", tmp.path())
        .args(["export", SESSION_ID, "-f", "markdown"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let routed_export = snatch_cmd()
        .env("SNATCH_CLAUDE_DIR", tmp.path())
        .args(["export", &qualified, "-f", "markdown"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    assert_eq!(
        routed_export, classic_export,
        "provider routing must not alter Claude normalized export output"
    );
}

#[test]
fn provider_routed_claude_lessons_keep_classic_correction_semantics() {
    let tmp = TempDir::new().unwrap();
    let project_dir = tmp
        .path()
        .join("projects")
        .join(encode_project_path(PROJECT_PATH));
    std::fs::create_dir_all(&project_dir).unwrap();
    let sid = "bbbbbbbb-1111-2222-3333-444444444444";
    let lines = [
        serde_json::json!({"type":"assistant","uuid":"a1","parentUuid":null,
            "timestamp":"2026-07-17T00:00:00Z","sessionId":sid,"version":"1",
            "message":{"id":"m1","type":"message","role":"assistant",
                "model":"claude-sonnet-5","content":[{"type":"text","text":"I will rewrite it."}]}}),
        serde_json::json!({"type":"user","uuid":"u1","parentUuid":"a1",
            "timestamp":"2026-07-17T00:00:01Z","sessionId":sid,"version":"1",
            "message":{"role":"user","content":"No, use the builder instead"}}),
    ];
    std::fs::write(
        project_dir.join(format!("{sid}.jsonl")),
        lines
            .iter()
            .map(serde_json::Value::to_string)
            .collect::<Vec<_>>()
            .join("\n")
            + "\n",
    )
    .unwrap();

    let run = |reference: &str| {
        snatch_cmd()
            .env("SNATCH_CLAUDE_DIR", tmp.path())
            .args(["-o", "json", "lessons", reference])
            .assert()
            .success()
            .get_output()
            .stdout
            .clone()
    };
    let classic: serde_json::Value = serde_json::from_slice(&run(sid)).unwrap();
    let routed: serde_json::Value =
        serde_json::from_slice(&run(&format!("claude-code:{sid}"))).unwrap();
    assert_eq!(classic["summary"]["total_corrections"], 1);
    assert_eq!(routed["summary"]["total_corrections"], 1);
    assert_eq!(
        classic["user_corrections"], routed["user_corrections"],
        "provider routing must not switch Claude onto absent semantic annotations"
    );
}
