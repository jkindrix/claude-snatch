//! Git repository correlation for Claude Code sessions.
//!
//! This module provides functionality to correlate session data with
//! git repository information, including commits made during sessions
//! and file modification history.

use std::path::Path;
use std::process::Command;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::error::{Result, SnatchError};

/// Git repository information.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GitRepoInfo {
    /// Repository root directory.
    pub root: String,
    /// Current branch name.
    pub branch: Option<String>,
    /// Remote URL (origin).
    pub remote_url: Option<String>,
    /// Latest commit hash.
    pub head_commit: Option<String>,
    /// Repository name (derived from path or remote).
    pub name: String,
}

/// Git commit information.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GitCommit {
    /// Commit hash (full).
    pub hash: String,
    /// Short hash (7 chars).
    pub short_hash: String,
    /// Commit message (first line).
    pub message: String,
    /// Author name.
    pub author: String,
    /// Author email.
    pub author_email: String,
    /// Commit timestamp.
    pub timestamp: DateTime<Utc>,
    /// Files changed in this commit.
    pub files_changed: Vec<String>,
}

/// Git correlation result for a session.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GitCorrelation {
    /// Repository information.
    pub repo: Option<GitRepoInfo>,
    /// Commits made during the session timeframe.
    pub commits_during_session: Vec<GitCommit>,
    /// Files modified both in session and commits.
    pub correlated_files: Vec<String>,
    /// Branch at session start.
    pub session_branch: Option<String>,
}

impl GitCorrelation {
    /// Create an empty correlation (no git info available).
    pub fn empty() -> Self {
        Self {
            repo: None,
            commits_during_session: Vec::new(),
            correlated_files: Vec::new(),
            session_branch: None,
        }
    }

    /// Check if any git information was found.
    pub fn has_data(&self) -> bool {
        self.repo.is_some() || !self.commits_during_session.is_empty()
    }
}

/// Check if a directory is inside a git repository.
pub fn is_git_repo(path: &Path) -> bool {
    if !path.exists() {
        return false;
    }

    let output = Command::new("git")
        .args(["rev-parse", "--git-dir"])
        .current_dir(path)
        .output();

    matches!(output, Ok(o) if o.status.success())
}

/// Get the root directory of the git repository.
pub fn get_repo_root(path: &Path) -> Option<String> {
    if !path.exists() {
        return None;
    }

    let output = Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .current_dir(path)
        .output()
        .ok()?;

    if output.status.success() {
        Some(String::from_utf8_lossy(&output.stdout).trim().to_string())
    } else {
        None
    }
}

/// Get current branch name.
pub fn get_current_branch(path: &Path) -> Option<String> {
    let output = Command::new("git")
        .args(["rev-parse", "--abbrev-ref", "HEAD"])
        .current_dir(path)
        .output()
        .ok()?;

    if output.status.success() {
        let branch = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if branch != "HEAD" {
            Some(branch)
        } else {
            None
        }
    } else {
        None
    }
}

/// Get remote URL for origin.
pub fn get_remote_url(path: &Path) -> Option<String> {
    let output = Command::new("git")
        .args(["remote", "get-url", "origin"])
        .current_dir(path)
        .output()
        .ok()?;

    if output.status.success() {
        Some(String::from_utf8_lossy(&output.stdout).trim().to_string())
    } else {
        None
    }
}

/// Get the HEAD commit hash.
pub fn get_head_commit(path: &Path) -> Option<String> {
    let output = Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(path)
        .output()
        .ok()?;

    if output.status.success() {
        Some(String::from_utf8_lossy(&output.stdout).trim().to_string())
    } else {
        None
    }
}

/// Get repository information for a directory.
pub fn get_repo_info(path: &Path) -> Option<GitRepoInfo> {
    if !is_git_repo(path) {
        return None;
    }

    let root = get_repo_root(path)?;
    let branch = get_current_branch(path);
    let remote_url = get_remote_url(path);
    let head_commit = get_head_commit(path);

    // Derive name from remote URL or directory name
    let name = if let Some(ref url) = remote_url {
        extract_repo_name_from_url(url)
    } else {
        Path::new(&root)
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("unknown")
            .to_string()
    };

    Some(GitRepoInfo {
        root,
        branch,
        remote_url,
        head_commit,
        name,
    })
}

/// Extract repository name from a git remote URL.
fn extract_repo_name_from_url(url: &str) -> String {
    // Handle various URL formats:
    // git@github.com:user/repo.git
    // https://github.com/user/repo.git
    // https://github.com/user/repo
    let url = url.trim_end_matches(".git");

    if let Some(pos) = url.rfind('/') {
        url[pos + 1..].to_string()
    } else if let Some(pos) = url.rfind(':') {
        // SSH format
        let after_colon = &url[pos + 1..];
        if let Some(slash) = after_colon.rfind('/') {
            after_colon[slash + 1..].to_string()
        } else {
            after_colon.to_string()
        }
    } else {
        url.to_string()
    }
}

/// Get commits in a time range.
pub fn get_commits_in_range(
    path: &Path,
    start: DateTime<Utc>,
    end: DateTime<Utc>,
) -> Result<Vec<GitCommit>> {
    if !is_git_repo(path) {
        return Ok(Vec::new());
    }

    let start_str = start.format("%Y-%m-%d %H:%M:%S").to_string();
    let end_str = end.format("%Y-%m-%d %H:%M:%S").to_string();

    // Get commits with detailed info
    // Format: hash|short|message|author|email|timestamp
    let output = Command::new("git")
        .args([
            "log",
            "--after",
            &start_str,
            "--before",
            &end_str,
            "--format=%H|%h|%s|%an|%ae|%aI",
            "--name-only",
        ])
        .current_dir(path)
        .output()
        .map_err(|e| SnatchError::io("Failed to run git log", e))?;

    if !output.status.success() {
        return Ok(Vec::new());
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    parse_git_log_output(&stdout)
}

/// Parse git log output into commits.
fn parse_git_log_output(output: &str) -> Result<Vec<GitCommit>> {
    let mut commits = Vec::new();
    let mut current_commit: Option<GitCommit> = None;
    let mut current_files: Vec<String> = Vec::new();

    for line in output.lines() {
        if line.is_empty() {
            continue;
        }

        if line.contains('|') && line.split('|').count() >= 6 {
            // This is a commit line
            // Save previous commit if any
            if let Some(mut commit) = current_commit.take() {
                commit.files_changed = std::mem::take(&mut current_files);
                commits.push(commit);
            }

            let parts: Vec<&str> = line.split('|').collect();
            if parts.len() >= 6 {
                let timestamp = chrono::DateTime::parse_from_rfc3339(parts[5])
                    .map(|dt| dt.with_timezone(&Utc))
                    .unwrap_or_else(|_| Utc::now());

                current_commit = Some(GitCommit {
                    hash: parts[0].to_string(),
                    short_hash: parts[1].to_string(),
                    message: parts[2].to_string(),
                    author: parts[3].to_string(),
                    author_email: parts[4].to_string(),
                    timestamp,
                    files_changed: Vec::new(),
                });
            }
        } else if current_commit.is_some() {
            // This is a file name
            current_files.push(line.to_string());
        }
    }

    // Don't forget the last commit
    if let Some(mut commit) = current_commit {
        commit.files_changed = current_files;
        commits.push(commit);
    }

    Ok(commits)
}

/// Correlate a session with git information.
pub fn correlate_session(
    cwd: Option<&str>,
    start_time: Option<DateTime<Utc>>,
    end_time: Option<DateTime<Utc>>,
    session_branch: Option<&str>,
    modified_files: &[String],
) -> GitCorrelation {
    let path = match cwd {
        Some(p) => Path::new(p),
        None => return GitCorrelation::empty(),
    };

    if !path.exists() || !is_git_repo(path) {
        return GitCorrelation::empty();
    }

    let repo = get_repo_info(path);

    // Get commits during session
    let commits_during_session = match (start_time, end_time) {
        (Some(start), Some(end)) => get_commits_in_range(path, start, end).unwrap_or_default(),
        _ => Vec::new(),
    };

    // Find files that appear in both session modifications and commits
    let mut correlated_files: Vec<String> = Vec::new();
    let commit_files: std::collections::HashSet<&str> = commits_during_session
        .iter()
        .flat_map(|c| c.files_changed.iter().map(|s| s.as_str()))
        .collect();

    for file in modified_files {
        // Extract just the filename for comparison
        let filename = Path::new(file)
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or(file);

        if commit_files.iter().any(|cf| cf.ends_with(filename)) {
            correlated_files.push(file.clone());
        }
    }

    GitCorrelation {
        repo,
        commits_during_session,
        correlated_files,
        session_branch: session_branch.map(String::from),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_repo_name_https() {
        assert_eq!(
            extract_repo_name_from_url("https://github.com/user/repo.git"),
            "repo"
        );
        assert_eq!(
            extract_repo_name_from_url("https://github.com/user/repo"),
            "repo"
        );
    }

    #[test]
    fn test_extract_repo_name_ssh() {
        assert_eq!(
            extract_repo_name_from_url("git@github.com:user/repo.git"),
            "repo"
        );
    }

    #[test]
    fn test_git_correlation_empty() {
        let correlation = GitCorrelation::empty();
        assert!(!correlation.has_data());
        assert!(correlation.repo.is_none());
        assert!(correlation.commits_during_session.is_empty());
    }

    #[test]
    fn test_is_git_repo_nonexistent() {
        assert!(!is_git_repo(Path::new("/nonexistent/path")));
    }

    #[test]
    fn test_correlate_session_no_cwd() {
        let result = correlate_session(None, None, None, None, &[]);
        assert!(!result.has_data());
    }

    #[test]
    fn test_correlate_session_nonexistent_path() {
        let result = correlate_session(Some("/nonexistent/path"), None, None, None, &[]);
        assert!(!result.has_data());
    }

    #[test]
    fn test_git_repo_info_struct() {
        let info = GitRepoInfo {
            root: "/path/to/repo".to_string(),
            branch: Some("main".to_string()),
            remote_url: Some("https://github.com/user/repo.git".to_string()),
            head_commit: Some("abc123def456".to_string()),
            name: "repo".to_string(),
        };

        assert_eq!(info.root, "/path/to/repo");
        assert_eq!(info.branch, Some("main".to_string()));
        assert_eq!(info.name, "repo");
    }

    #[test]
    fn test_git_commit_struct() {
        let commit = GitCommit {
            hash: "abc123def456789".to_string(),
            short_hash: "abc123d".to_string(),
            message: "Initial commit".to_string(),
            author: "Test User".to_string(),
            author_email: "test@example.com".to_string(),
            timestamp: Utc::now(),
            files_changed: vec!["file1.rs".to_string(), "file2.rs".to_string()],
        };

        assert_eq!(commit.message, "Initial commit");
        assert_eq!(commit.files_changed.len(), 2);
    }

    #[test]
    fn test_git_correlation_has_data_with_repo() {
        let correlation = GitCorrelation {
            repo: Some(GitRepoInfo {
                root: "/test".to_string(),
                branch: None,
                remote_url: None,
                head_commit: None,
                name: "test".to_string(),
            }),
            commits_during_session: Vec::new(),
            correlated_files: Vec::new(),
            session_branch: None,
        };

        assert!(correlation.has_data());
    }

    #[test]
    fn test_git_correlation_has_data_with_commits() {
        let correlation = GitCorrelation {
            repo: None,
            commits_during_session: vec![GitCommit {
                hash: "abc".to_string(),
                short_hash: "abc".to_string(),
                message: "test".to_string(),
                author: "test".to_string(),
                author_email: "test@test.com".to_string(),
                timestamp: Utc::now(),
                files_changed: vec![],
            }],
            correlated_files: Vec::new(),
            session_branch: None,
        };

        assert!(correlation.has_data());
    }

    #[test]
    fn test_extract_repo_name_various_formats() {
        // Various URL formats
        assert_eq!(
            extract_repo_name_from_url("https://gitlab.com/group/subgroup/repo.git"),
            "repo"
        );
        assert_eq!(
            extract_repo_name_from_url("git@bitbucket.org:team/repo.git"),
            "repo"
        );
        assert_eq!(
            extract_repo_name_from_url("file:///path/to/repo.git"),
            "repo"
        );
    }

    #[test]
    fn test_extract_repo_name_edge_cases() {
        // Edge cases
        assert_eq!(extract_repo_name_from_url("repo"), "repo");
        assert_eq!(extract_repo_name_from_url(""), "");
        assert_eq!(extract_repo_name_from_url("/"), "");
    }

    #[test]
    fn test_get_repo_root_nonexistent() {
        assert!(get_repo_root(Path::new("/definitely/nonexistent/path")).is_none());
    }

    #[test]
    fn test_correlate_with_session_branch() {
        let result = correlate_session(
            Some("/nonexistent"),
            None,
            None,
            Some("feature/test"),
            &[],
        );
        // Path doesn't exist, so no data
        assert!(!result.has_data());
    }

    #[test]
    fn test_correlate_with_modified_files() {
        let files = vec!["src/main.rs".to_string(), "README.md".to_string()];
        let result = correlate_session(
            Some("/nonexistent"),
            None,
            None,
            None,
            &files,
        );
        assert!(!result.has_data());
    }

    // Note: Tests that require an actual git repo are marked as integration tests
    // and should be run in a real git repository environment
}
