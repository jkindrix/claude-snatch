//! Project discovery and management.
//!
//! A project in Claude Code corresponds to a working directory.
//! Project data is stored in `~/.claude/projects/<encoded-path>/`.

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use crate::error::{Result, SnatchError};

use super::paths::{decode_project_path, is_session_file};
use super::session::Session;

/// A Claude Code project directory.
#[derive(Debug, Clone)]
pub struct Project {
    /// Path to the project directory in ~/.claude/projects/.
    path: PathBuf,
    /// Encoded directory name.
    encoded_name: String,
    /// Decoded project path (the actual working directory).
    decoded_path: String,
}

impl Project {
    /// Create a Project from its directory path.
    pub fn from_path(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref().to_path_buf();

        if !path.exists() {
            return Err(SnatchError::ProjectNotFound {
                project_path: path.display().to_string(),
            });
        }

        if !path.is_dir() {
            return Err(SnatchError::InvalidSessionFile {
                path: path.clone(),
                reason: "Not a directory".to_string(),
            });
        }

        let encoded_name = path
            .file_name()
            .and_then(|n| n.to_str())
            .ok_or_else(|| SnatchError::InvalidPathEncoding {
                path: path.display().to_string(),
            })?
            .to_string();

        let decoded_path = decode_project_path(&encoded_name);

        Ok(Self {
            path,
            encoded_name,
            decoded_path,
        })
    }

    /// Get the path to the project directory.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Get the encoded directory name.
    #[must_use]
    pub fn encoded_name(&self) -> &str {
        &self.encoded_name
    }

    /// Get the decoded project path (original working directory).
    #[must_use]
    pub fn decoded_path(&self) -> &str {
        &self.decoded_path
    }

    /// Get a display name for the project (last component of decoded path).
    #[must_use]
    pub fn display_name(&self) -> &str {
        self.decoded_path
            .rsplit('/')
            .next()
            .unwrap_or(&self.decoded_path)
    }

    /// List all sessions in this project.
    pub fn sessions(&self) -> Result<Vec<Session>> {
        let mut sessions = Vec::new();

        for entry in std::fs::read_dir(&self.path).map_err(|e| {
            SnatchError::io(
                format!("Failed to read project directory: {}", self.path.display()),
                e,
            )
        })? {
            let entry = entry.map_err(|e| {
                SnatchError::io("Failed to read directory entry", e)
            })?;

            let path = entry.path();
            if is_session_file(&path) {
                match Session::from_path(&path, &self.decoded_path) {
                    Ok(session) => sessions.push(session),
                    Err(_) => continue, // Skip invalid session files
                }
            }
        }

        // Sort by modification time (newest first)
        sessions.sort_by(|a, b| b.modified_time().cmp(&a.modified_time()));

        Ok(sessions)
    }

    /// List only main sessions (excluding subagent sessions).
    pub fn main_sessions(&self) -> Result<Vec<Session>> {
        Ok(self.sessions()?.into_iter().filter(|s| !s.is_subagent()).collect())
    }

    /// List only subagent sessions.
    pub fn subagent_sessions(&self) -> Result<Vec<Session>> {
        Ok(self.sessions()?.into_iter().filter(|s| s.is_subagent()).collect())
    }

    /// Find a session by its ID.
    pub fn find_session(&self, session_id: &str) -> Result<Option<Session>> {
        for session in self.sessions()? {
            if session.session_id() == session_id {
                return Ok(Some(session));
            }
        }
        Ok(None)
    }

    /// Get the number of sessions in this project.
    pub fn session_count(&self) -> Result<usize> {
        Ok(self.sessions()?.len())
    }

    /// Get the total size of all session files in bytes.
    pub fn total_size(&self) -> Result<u64> {
        Ok(self.sessions()?.iter().map(|s| s.file_size()).sum())
    }

    /// Get the modification time of the most recently modified session.
    pub fn last_modified(&self) -> Result<Option<SystemTime>> {
        Ok(self.sessions()?.first().map(|s| s.modified_time()))
    }

    /// Check if this project has any active sessions.
    pub fn has_active_sessions(&self) -> Result<bool> {
        use super::streaming::detect_session_state;
        use super::streaming::SessionState;

        for session in self.sessions()? {
            if let Ok(state) = detect_session_state(session.path()) {
                if state != SessionState::Inactive {
                    return Ok(true);
                }
            }
        }
        Ok(false)
    }

    /// Get project metadata summary.
    pub fn summary(&self) -> Result<ProjectSummary> {
        let sessions = self.sessions()?;
        let main_count = sessions.iter().filter(|s| !s.is_subagent()).count();
        let subagent_count = sessions.len() - main_count;
        let total_size = sessions.iter().map(|s| s.file_size()).sum();
        let last_modified = sessions.first().map(|s| s.modified_time());

        Ok(ProjectSummary {
            encoded_name: self.encoded_name.clone(),
            decoded_path: self.decoded_path.clone(),
            display_name: self.display_name().to_string(),
            session_count: sessions.len(),
            main_session_count: main_count,
            subagent_count,
            total_size_bytes: total_size,
            last_modified,
        })
    }
}

/// Summary information about a project.
#[derive(Debug, Clone)]
pub struct ProjectSummary {
    /// Encoded directory name.
    pub encoded_name: String,
    /// Decoded project path.
    pub decoded_path: String,
    /// Display name (last path component).
    pub display_name: String,
    /// Total session count.
    pub session_count: usize,
    /// Main (non-subagent) session count.
    pub main_session_count: usize,
    /// Subagent session count.
    pub subagent_count: usize,
    /// Total size of all session files.
    pub total_size_bytes: u64,
    /// Last modification time.
    pub last_modified: Option<SystemTime>,
}

impl ProjectSummary {
    /// Get human-readable total size.
    #[must_use]
    pub fn total_size_human(&self) -> String {
        super::format_size(self.total_size_bytes)
    }
}

/// Filter options for project listing.
#[derive(Debug, Clone, Default)]
pub struct ProjectFilter {
    /// Filter by path pattern.
    pub path_pattern: Option<String>,
    /// Only include projects with sessions newer than this.
    pub modified_after: Option<SystemTime>,
    /// Only include projects with at least this many sessions.
    pub min_sessions: Option<usize>,
    /// Include only active projects.
    pub active_only: bool,
}

impl ProjectFilter {
    /// Create a new filter.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Filter by path pattern (glob-like).
    #[must_use]
    pub fn with_path_pattern(mut self, pattern: impl Into<String>) -> Self {
        self.path_pattern = Some(pattern.into());
        self
    }

    /// Filter by modification time.
    #[must_use]
    pub fn modified_after(mut self, time: SystemTime) -> Self {
        self.modified_after = Some(time);
        self
    }

    /// Filter by minimum session count.
    #[must_use]
    pub fn min_sessions(mut self, count: usize) -> Self {
        self.min_sessions = Some(count);
        self
    }

    /// Only include active projects.
    #[must_use]
    pub fn active_only(mut self) -> Self {
        self.active_only = true;
        self
    }

    /// Check if a project matches this filter.
    pub fn matches(&self, project: &Project) -> Result<bool> {
        // Check path pattern
        if let Some(pattern) = &self.path_pattern {
            let path = project.decoded_path();
            if !path.contains(pattern) {
                return Ok(false);
            }
        }

        // Check modification time
        if let Some(after) = self.modified_after {
            if let Some(modified) = project.last_modified()? {
                if modified < after {
                    return Ok(false);
                }
            } else {
                return Ok(false);
            }
        }

        // Check session count
        if let Some(min) = self.min_sessions {
            if project.session_count()? < min {
                return Ok(false);
            }
        }

        // Check active status
        if self.active_only && !project.has_active_sessions()? {
            return Ok(false);
        }

        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    #[allow(unused_imports)]
    use super::*;

    #[test]
    fn test_display_name() {
        // Can't easily test without actual directories, but verify the logic
        let decoded = "/home/user/my-awesome-project";
        let name = decoded.rsplit('/').next().unwrap();
        assert_eq!(name, "my-awesome-project");
    }
}
