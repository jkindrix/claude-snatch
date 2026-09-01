//! Authoritative project-path resolution from recorded session `cwd`.
//!
//! Claude Code encodes a project's working directory into its storage
//! directory name by mapping every path-special character to `-`:
//!
//! ```text
//! /home/user/my_app  ->  -home-user-my-app
//! /mnt/c/_dev/A.B     ->  -mnt-c--dev-A-B
//! ```
//!
//! The mapping is lossy, so the directory name alone cannot say whether a `-`
//! stands for `/`, `_`, `.`, or a literal `-`. [`decode_project_path`] guesses
//! by probing the filesystem, which is both slow and — for the common case of a
//! project directory that has since been deleted or moved — unable to answer at
//! all, because every candidate it tests is equally absent.
//!
//! The session logs record the answer directly: entries carry a `cwd` field
//! written at the time the session ran. This module reads that field and
//! **verifies** it by re-encoding: a candidate is accepted only when
//! `claude_encode_project_path(cwd) == <directory name>`. That check is exact,
//! not heuristic, because the encoding is a pure per-character map. So a
//! resolution here is a proof; anything unproven falls back to guessing.
//!
//! Verification also settles the case of a session whose `cwd` changes
//! mid-file: the candidate that re-encodes to this directory wins, whatever its
//! position in the log.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};

use once_cell::sync::Lazy;
use parking_lot::RwLock;
use tracing::debug;

use super::paths::{claude_encode_project_path, decode_project_path};

/// Maximum JSONL lines inspected per session file.
///
/// `cwd` normally appears within the first handful of entries; the tail of the
/// window covers logs that open with summary or meta records.
const MAX_LINES_PER_FILE: usize = 64;

/// Maximum bytes read per session file.
///
/// A single entry can legitimately reach a few hundred KB, so this bounds the
/// scan by volume rather than trusting the line count alone.
const MAX_BYTES_PER_FILE: u64 = 4 * 1024 * 1024;

/// Top-level session files tried before descending into subdirectories.
const MAX_FILES_SHALLOW: usize = 5;

/// Maximum session files inspected during the nested rescue walk.
const MAX_FILES_NESTED: usize = 32;

/// Memo of verified resolutions, keyed by encoded directory name.
///
/// Only verified results are stored. They can never go stale: the key is the
/// directory name and the value re-encodes to exactly that name, so the entry
/// stays true for as long as the directory exists under that name. Unverified
/// fallbacks are deliberately *not* memoized — caching a guess made before a
/// brand-new session had written its first entry would pin the wrong path for
/// the life of the process.
static VERIFIED: Lazy<RwLock<HashMap<String, String>>> = Lazy::new(|| RwLock::new(HashMap::new()));

/// Resolve the working directory a project's storage directory stands for.
///
/// Returns the `cwd` recorded in the project's own session logs when one
/// re-encodes to `encoded_name`, and otherwise falls back to
/// [`decode_project_path`]'s filesystem-probing guess.
pub fn resolve_project_path(project_dir: &Path, encoded_name: &str) -> String {
    if let Some(hit) = VERIFIED.read().get(encoded_name) {
        return hit.clone();
    }

    if let Some(path) = recorded_cwd(project_dir, encoded_name) {
        VERIFIED
            .write()
            .insert(encoded_name.to_string(), path.clone());
        return path;
    }

    debug!(
        project = encoded_name,
        "No verifiable cwd in session logs; falling back to path guessing"
    );
    decode_project_path(encoded_name)
}

/// Find a recorded `cwd` that re-encodes to `encoded_name`.
fn recorded_cwd(project_dir: &Path, encoded_name: &str) -> Option<String> {
    let mut shallow = session_files_in(project_dir);
    shallow.sort();

    for file in shallow.iter().take(MAX_FILES_SHALLOW) {
        if let Some(cwd) = scan_file(file, encoded_name) {
            return Some(cwd);
        }
    }

    // Rescue pass: sessions can live under <session-uuid>/ subdirectories (and
    // subagent logs under <session-uuid>/subagents/), so a project whose
    // top-level files are absent or truncated is still recoverable.
    let mut nested = Vec::new();
    collect_nested(project_dir, &mut nested, 0);
    nested.sort();

    for file in nested.iter().take(MAX_FILES_NESTED) {
        if let Some(cwd) = scan_file(file, encoded_name) {
            return Some(cwd);
        }
    }

    None
}

/// List non-empty `.jsonl` files directly inside a directory.
fn session_files_in(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };

    entries
        .flatten()
        .filter_map(|entry| {
            let path = entry.path();
            if path.extension().is_some_and(|ext| ext == "jsonl")
                && entry.metadata().map(|m| m.len() > 0).unwrap_or(false)
            {
                Some(path)
            } else {
                None
            }
        })
        .collect()
}

/// Collect `.jsonl` files from subdirectories, bounded in depth and count.
fn collect_nested(dir: &Path, out: &mut Vec<PathBuf>, depth: usize) {
    // <session-uuid>/subagents/<agent>.jsonl is the deepest real layout.
    if depth > 2 || out.len() >= MAX_FILES_NESTED {
        return;
    }

    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };

    for entry in entries.flatten() {
        if out.len() >= MAX_FILES_NESTED {
            return;
        }
        let path = entry.path();
        if entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
            out.extend(session_files_in(&path));
            collect_nested(&path, out, depth + 1);
        }
    }
}

/// Scan one session file for a `cwd` that re-encodes to `encoded_name`.
fn scan_file(path: &Path, encoded_name: &str) -> Option<String> {
    let file = std::fs::File::open(path).ok()?;
    let mut reader = BufReader::new(file).take(MAX_BYTES_PER_FILE);
    let mut line = String::new();

    for _ in 0..MAX_LINES_PER_FILE {
        line.clear();
        match reader.read_line(&mut line) {
            Ok(0) | Err(_) => break,
            Ok(_) => {}
        }

        // Cheap reject before paying for a JSON parse.
        if !line.contains("\"cwd\"") {
            continue;
        }

        let Ok(entry) = serde_json::from_str::<CwdOnly>(&line) else {
            continue;
        };
        let Some(cwd) = entry.cwd else {
            continue;
        };

        // The verification that makes this a proof rather than a guess.
        if claude_encode_project_path(&cwd) == encoded_name {
            return Some(cwd);
        }
    }

    None
}

/// Minimal projection of a log entry: only the field this module needs.
#[derive(serde::Deserialize)]
struct CwdOnly {
    #[serde(default)]
    cwd: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a project directory holding one session file with the given lines.
    fn project_with(dir: &Path, encoded: &str, lines: &[&str]) -> PathBuf {
        let project = dir.join(encoded);
        std::fs::create_dir_all(&project).unwrap();
        std::fs::write(
            project.join("11111111-2222-3333-4444-555555555555.jsonl"),
            lines.join("\n"),
        )
        .unwrap();
        project
    }

    #[test]
    fn resolves_underscore_path_that_guessing_gets_wrong() {
        let tmp = tempfile::tempdir().unwrap();
        let encoded = "-home-user-cma-central-db-design";
        let project = project_with(
            tmp.path(),
            encoded,
            &[r#"{"type":"user","cwd":"/home/user/cma_central_db_design"}"#],
        );

        assert_eq!(
            resolve_project_path(&project, encoded),
            "/home/user/cma_central_db_design"
        );
        // The guess this replaces splits every hyphen into a separator.
        assert_eq!(
            decode_project_path(encoded),
            "/home/user/cma/central/db/design"
        );
    }

    #[test]
    fn resolves_dotted_windows_path() {
        let tmp = tempfile::tempdir().unwrap();
        let encoded = "-mnt-c--dev-CMA-Central";
        let project = project_with(
            tmp.path(),
            encoded,
            &[r#"{"type":"user","cwd":"/mnt/c/_dev/CMA.Central"}"#],
        );

        assert_eq!(
            resolve_project_path(&project, encoded),
            "/mnt/c/_dev/CMA.Central"
        );
    }

    #[test]
    fn rejects_cwd_that_does_not_re_encode() {
        let tmp = tempfile::tempdir().unwrap();
        let encoded = "-home-user-alpha";
        // A cwd from an unrelated directory must never be accepted.
        let project = project_with(
            tmp.path(),
            encoded,
            &[r#"{"type":"user","cwd":"/home/user/beta"}"#],
        );

        assert_eq!(
            resolve_project_path(&project, encoded),
            decode_project_path(encoded)
        );
    }

    #[test]
    fn picks_the_matching_cwd_when_it_changes_mid_session() {
        let tmp = tempfile::tempdir().unwrap();
        let encoded = "-home-user-proj-sub";
        let project = project_with(
            tmp.path(),
            encoded,
            &[
                r#"{"type":"user","cwd":"/home/user/proj"}"#,
                r#"{"type":"user","cwd":"/home/user/proj/sub"}"#,
            ],
        );

        // The first cwd re-encodes to "-home-user-proj", not this directory.
        assert_eq!(
            resolve_project_path(&project, encoded),
            "/home/user/proj/sub"
        );
    }

    #[test]
    fn skips_unparsable_lines_and_keeps_scanning() {
        let tmp = tempfile::tempdir().unwrap();
        let encoded = "-home-user-gamma";
        let project = project_with(
            tmp.path(),
            encoded,
            &[
                "not json at all",
                r#"{"type":"summary","summary":"no cwd here"}"#,
                r#"{"type":"user","cwd":"/home/user/gamma"}"#,
            ],
        );

        assert_eq!(resolve_project_path(&project, encoded), "/home/user/gamma");
    }

    #[test]
    fn falls_back_when_project_has_no_session_files() {
        let tmp = tempfile::tempdir().unwrap();
        let encoded = "-home-user-empty-project";
        let project = tmp.path().join(encoded);
        std::fs::create_dir_all(&project).unwrap();

        assert_eq!(
            resolve_project_path(&project, encoded),
            decode_project_path(encoded)
        );
    }

    #[test]
    fn finds_cwd_in_nested_session_directory() {
        let tmp = tempfile::tempdir().unwrap();
        let encoded = "-home-user-nested-proj";
        let project = tmp.path().join(encoded);
        let nested = project.join("aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee");
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::write(
            nested.join("session.jsonl"),
            r#"{"type":"user","cwd":"/home/user/nested-proj"}"#,
        )
        .unwrap();

        assert_eq!(
            resolve_project_path(&project, encoded),
            "/home/user/nested-proj"
        );
    }

    #[test]
    fn ignores_empty_session_files() {
        let tmp = tempfile::tempdir().unwrap();
        let encoded = "-home-user-delta";
        let project = tmp.path().join(encoded);
        std::fs::create_dir_all(&project).unwrap();
        // Sorts before the real file, and must not stop the scan.
        std::fs::write(
            project.join("00000000-0000-0000-0000-000000000000.jsonl"),
            "",
        )
        .unwrap();
        std::fs::write(
            project.join("11111111-1111-1111-1111-111111111111.jsonl"),
            r#"{"type":"user","cwd":"/home/user/delta"}"#,
        )
        .unwrap();

        assert_eq!(resolve_project_path(&project, encoded), "/home/user/delta");
    }
}
