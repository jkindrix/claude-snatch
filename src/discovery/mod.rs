//! Session and project discovery for Claude Code data.
//!
//! This module provides cross-platform discovery of Claude Code data directories,
//! projects, and sessions. It handles:
//! - Auto-discovery of ~/.claude directory
//! - Platform-specific path handling (Linux, macOS, Windows, WSL)
//! - Project path encoding/decoding
//! - Session enumeration and metadata extraction

mod hierarchy;
mod paths;
mod project;
mod session;
pub mod streaming;

pub use hierarchy::*;
pub use paths::*;
pub use project::*;
pub use session::*;
pub use streaming::{detect_session_state, SessionState};

use std::path::{Path, PathBuf};

use crate::error::{Result, SnatchError};
use crate::{FILE_HISTORY_DIR_NAME, PROJECTS_DIR_NAME};

/// Claude Code data directory manager.
#[derive(Debug, Clone)]
pub struct ClaudeDirectory {
    /// Root path to the .claude directory.
    root: PathBuf,
    /// Projects subdirectory.
    projects_dir: PathBuf,
    /// File history subdirectory.
    file_history_dir: PathBuf,
}

impl ClaudeDirectory {
    /// Create from an explicit path.
    pub fn from_path(path: impl AsRef<Path>) -> Result<Self> {
        let root = path.as_ref().to_path_buf();

        if !root.exists() {
            return Err(SnatchError::ClaudeDirectoryNotFound {
                expected_path: root,
            });
        }

        let projects_dir = root.join(PROJECTS_DIR_NAME);
        let file_history_dir = root.join(FILE_HISTORY_DIR_NAME);

        Ok(Self {
            root,
            projects_dir,
            file_history_dir,
        })
    }

    /// Auto-discover the Claude Code data directory.
    pub fn discover() -> Result<Self> {
        let path = discover_claude_directory()?;
        Self::from_path(path)
    }

    /// Get the root path.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Get the projects directory path.
    #[must_use]
    pub fn projects_dir(&self) -> &Path {
        &self.projects_dir
    }

    /// Get the file history directory path.
    #[must_use]
    pub fn file_history_dir(&self) -> &Path {
        &self.file_history_dir
    }

    /// Check if the projects directory exists.
    #[must_use]
    pub fn has_projects(&self) -> bool {
        self.projects_dir.exists()
    }

    /// Check if the file history directory exists.
    #[must_use]
    pub fn has_file_history(&self) -> bool {
        self.file_history_dir.exists()
    }

    /// List all projects.
    pub fn projects(&self) -> Result<Vec<Project>> {
        if !self.projects_dir.exists() {
            return Ok(Vec::new());
        }

        let mut projects = Vec::new();

        for entry in std::fs::read_dir(&self.projects_dir).map_err(|e| {
            SnatchError::io(
                format!("Failed to read projects directory: {}", self.projects_dir.display()),
                e,
            )
        })? {
            let entry = entry.map_err(|e| {
                SnatchError::io("Failed to read directory entry", e)
            })?;

            let path = entry.path();
            if path.is_dir() {
                match Project::from_path(&path) {
                    Ok(project) => projects.push(project),
                    Err(_) => continue, // Skip invalid project directories
                }
            }
        }

        // Sort by decoded path
        projects.sort_by(|a, b| a.decoded_path().cmp(&b.decoded_path()));

        Ok(projects)
    }

    /// Find a project by its decoded path.
    pub fn find_project(&self, decoded_path: &str) -> Result<Option<Project>> {
        let encoded = encode_project_path(decoded_path);
        let project_dir = self.projects_dir.join(&encoded);

        if project_dir.exists() {
            Ok(Some(Project::from_path(&project_dir)?))
        } else {
            Ok(None)
        }
    }

    /// Find a project by encoded directory name.
    pub fn find_project_by_encoded(&self, encoded_name: &str) -> Result<Option<Project>> {
        let project_dir = self.projects_dir.join(encoded_name);

        if project_dir.exists() {
            Ok(Some(Project::from_path(&project_dir)?))
        } else {
            Ok(None)
        }
    }

    /// Get all sessions across all projects.
    pub fn all_sessions(&self) -> Result<Vec<Session>> {
        let mut sessions = Vec::new();

        for project in self.projects()? {
            for session in project.sessions()? {
                sessions.push(session);
            }
        }

        // Sort by timestamp (newest first)
        sessions.sort_by(|a, b| b.modified_time().cmp(&a.modified_time()));

        Ok(sessions)
    }

    /// Find a session by UUID across all projects.
    pub fn find_session(&self, session_id: &str) -> Result<Option<Session>> {
        for project in self.projects()? {
            if let Some(session) = project.find_session(session_id)? {
                return Ok(Some(session));
            }
        }
        Ok(None)
    }

    /// Get global settings file path.
    #[must_use]
    pub fn settings_path(&self) -> PathBuf {
        self.root.join("settings.json")
    }

    /// Get CLAUDE.md file path.
    #[must_use]
    pub fn claude_md_path(&self) -> PathBuf {
        self.root.join("CLAUDE.md")
    }

    /// Get MCP configuration file path.
    #[must_use]
    pub fn mcp_config_path(&self) -> PathBuf {
        self.root.join("mcp.json")
    }

    /// Get commands directory path.
    #[must_use]
    pub fn commands_dir(&self) -> PathBuf {
        self.root.join("commands")
    }

    /// Get rules directory path.
    #[must_use]
    pub fn rules_dir(&self) -> PathBuf {
        self.root.join("rules")
    }

    /// Get output styles directory path.
    #[must_use]
    pub fn output_styles_dir(&self) -> PathBuf {
        self.root.join("output-styles")
    }

    /// Get statistics about the Claude directory.
    pub fn statistics(&self) -> Result<DirectoryStatistics> {
        let mut stats = DirectoryStatistics::default();

        for project in self.projects()? {
            stats.project_count += 1;

            for session in project.sessions()? {
                stats.session_count += 1;
                stats.total_size_bytes += session.file_size();

                if session.is_subagent() {
                    stats.subagent_count += 1;
                }
            }
        }

        if self.has_file_history() {
            stats.has_file_history = true;
            // Count backup files
            if let Ok(entries) = std::fs::read_dir(&self.file_history_dir) {
                stats.backup_file_count = entries.count();
            }
        }

        Ok(stats)
    }
}

/// Statistics about the Claude directory.
#[derive(Debug, Clone, Default)]
pub struct DirectoryStatistics {
    /// Number of projects.
    pub project_count: usize,
    /// Number of sessions (including subagents).
    pub session_count: usize,
    /// Number of subagent sessions.
    pub subagent_count: usize,
    /// Total size of all session files in bytes.
    pub total_size_bytes: u64,
    /// Whether file history exists.
    pub has_file_history: bool,
    /// Number of backup files.
    pub backup_file_count: usize,
}

impl DirectoryStatistics {
    /// Get human-readable total size.
    #[must_use]
    pub fn total_size_human(&self) -> String {
        format_size(self.total_size_bytes)
    }
}

/// Format a size in bytes as human-readable string.
#[must_use]
pub fn format_size(bytes: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = KB * 1024;
    const GB: u64 = MB * 1024;

    if bytes >= GB {
        format!("{:.2} GB", bytes as f64 / GB as f64)
    } else if bytes >= MB {
        format!("{:.2} MB", bytes as f64 / MB as f64)
    } else if bytes >= KB {
        format!("{:.2} KB", bytes as f64 / KB as f64)
    } else {
        format!("{bytes} B")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_format_size() {
        assert_eq!(format_size(500), "500 B");
        assert_eq!(format_size(1024), "1.00 KB");
        assert_eq!(format_size(1536), "1.50 KB");
        assert_eq!(format_size(1048576), "1.00 MB");
        assert_eq!(format_size(1073741824), "1.00 GB");
    }
}
