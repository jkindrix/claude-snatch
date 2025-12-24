//! Session file discovery and metadata.
//!
//! A session corresponds to a single JSONL file containing conversation history.
//! Sessions can be main conversations or subagent sessions (agent-*.jsonl).

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use chrono::{DateTime, Utc};

use crate::cache::global_cache;
use crate::error::{Result, SnatchError};
use crate::model::{LogEntry, SchemaVersion};
use crate::parser::{JsonlParser, StreamingParser};

use super::paths::parse_session_filename;
use super::streaming::{detect_session_state, SessionState};

/// A Claude Code session file.
#[derive(Debug, Clone)]
pub struct Session {
    /// Path to the JSONL file.
    path: PathBuf,
    /// Session ID (filename without extension).
    session_id: String,
    /// Whether this is a subagent session.
    is_subagent: bool,
    /// Agent hash if subagent.
    agent_hash: Option<String>,
    /// File size in bytes.
    file_size: u64,
    /// Last modification time.
    modified_time: SystemTime,
    /// Parent project path (decoded).
    project_path: String,
}

impl Session {
    /// Create a Session from its file path.
    pub fn from_path(path: impl AsRef<Path>, project_path: &str) -> Result<Self> {
        let path = path.as_ref().to_path_buf();

        if !path.exists() {
            return Err(SnatchError::FileNotFound { path });
        }

        let filename = path
            .file_name()
            .and_then(|n| n.to_str())
            .ok_or_else(|| SnatchError::InvalidSessionFile {
                path: path.clone(),
                reason: "Invalid filename".to_string(),
            })?;

        let file_info = parse_session_filename(filename).ok_or_else(|| {
            SnatchError::InvalidSessionFile {
                path: path.clone(),
                reason: "Not a valid session filename".to_string(),
            }
        })?;

        let metadata = std::fs::metadata(&path).map_err(|e| {
            SnatchError::io(format!("Failed to read metadata for {}", path.display()), e)
        })?;

        let modified_time = metadata.modified().map_err(|e| {
            SnatchError::io(format!("Failed to get mtime for {}", path.display()), e)
        })?;

        Ok(Self {
            path,
            session_id: file_info.session_id,
            is_subagent: file_info.is_subagent,
            agent_hash: file_info.agent_hash,
            file_size: metadata.len(),
            modified_time,
            project_path: project_path.to_string(),
        })
    }

    /// Get the path to the session file.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Get the session ID.
    #[must_use]
    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    /// Check if this is a subagent session.
    #[must_use]
    pub fn is_subagent(&self) -> bool {
        self.is_subagent
    }

    /// Get the agent hash if this is a subagent session.
    #[must_use]
    pub fn agent_hash(&self) -> Option<&str> {
        self.agent_hash.as_deref()
    }

    /// Get the file size in bytes.
    #[must_use]
    pub fn file_size(&self) -> u64 {
        self.file_size
    }

    /// Get the human-readable file size.
    #[must_use]
    pub fn file_size_human(&self) -> String {
        super::format_size(self.file_size)
    }

    /// Get the last modification time.
    #[must_use]
    pub fn modified_time(&self) -> SystemTime {
        self.modified_time
    }

    /// Get the modification time as a DateTime.
    #[must_use]
    pub fn modified_datetime(&self) -> DateTime<Utc> {
        DateTime::from(self.modified_time)
    }

    /// Get the parent project path.
    #[must_use]
    pub fn project_path(&self) -> &str {
        &self.project_path
    }

    /// Detect if this session is currently active.
    pub fn state(&self) -> Result<SessionState> {
        detect_session_state(&self.path)
    }

    /// Check if this session is possibly active.
    pub fn is_active(&self) -> Result<bool> {
        Ok(self.state()? != SessionState::Inactive)
    }

    /// Parse all entries from this session.
    pub fn parse(&self) -> Result<Vec<LogEntry>> {
        let mut parser = JsonlParser::new().with_lenient(true);
        parser.parse_file(&self.path)
    }

    /// Parse all entries with caching support.
    ///
    /// Uses the global cache to avoid re-parsing unchanged files.
    pub fn parse_cached(&self) -> Result<std::sync::Arc<Vec<LogEntry>>> {
        global_cache().get_or_parse(&self.path, || self.parse())
    }

    /// Create a streaming parser for this session.
    pub fn stream(&self) -> Result<StreamingParser<std::io::BufReader<std::fs::File>>> {
        super::streaming::open_stream(&self.path)
    }

    /// Get quick metadata without parsing the entire file.
    pub fn quick_metadata(&self) -> Result<QuickSessionMetadata> {
        self.compute_metadata()
    }

    /// Get quick metadata with caching support.
    pub fn quick_metadata_cached(&self) -> Result<QuickSessionMetadata> {
        // Try cache first
        if let Some(cached) = global_cache().get_metadata(&self.path) {
            return Ok(cached);
        }

        // Compute and cache
        let metadata = self.compute_metadata()?;
        global_cache().cache_metadata(&self.path, metadata.clone());
        Ok(metadata)
    }

    /// Compute metadata from parsed entries.
    fn compute_metadata(&self) -> Result<QuickSessionMetadata> {
        // Read just the first and last few lines
        let mut parser = JsonlParser::new().with_lenient(true);
        let entries = parser.parse_file(&self.path)?;

        let first_entry = entries.first();
        let last_entry = entries.last();

        let start_time = first_entry.and_then(|e| e.timestamp());
        let end_time = last_entry.and_then(|e| e.timestamp());
        let version = first_entry.and_then(|e| e.version().map(String::from));
        let schema_version = version
            .as_deref()
            .map(SchemaVersion::from_version_string);

        // Count message types
        let mut user_count = 0;
        let mut assistant_count = 0;
        let mut system_count = 0;
        let mut other_count = 0;

        for entry in &entries {
            match entry.message_type() {
                "user" => user_count += 1,
                "assistant" => assistant_count += 1,
                "system" => system_count += 1,
                _ => other_count += 1,
            }
        }

        Ok(QuickSessionMetadata {
            session_id: self.session_id.clone(),
            is_subagent: self.is_subagent,
            file_size: self.file_size,
            entry_count: entries.len(),
            user_count,
            assistant_count,
            system_count,
            other_count,
            start_time,
            end_time,
            version,
            schema_version,
        })
    }

    /// Get session summary suitable for display.
    pub fn summary(&self) -> Result<SessionSummary> {
        let meta = self.quick_metadata()?;
        let state = self.state()?;

        Ok(SessionSummary {
            session_id: self.session_id.clone(),
            is_subagent: self.is_subagent,
            project_path: self.project_path.clone(),
            file_size: self.file_size,
            file_size_human: self.file_size_human(),
            entry_count: meta.entry_count,
            message_count: meta.user_count + meta.assistant_count,
            start_time: meta.start_time,
            end_time: meta.end_time,
            duration: meta.duration(),
            state,
            version: meta.version,
        })
    }
}

/// Quick metadata extracted from a session without full parsing.
#[derive(Debug, Clone)]
pub struct QuickSessionMetadata {
    /// Session ID.
    pub session_id: String,
    /// Whether this is a subagent session.
    pub is_subagent: bool,
    /// File size in bytes.
    pub file_size: u64,
    /// Total entry count.
    pub entry_count: usize,
    /// User message count.
    pub user_count: usize,
    /// Assistant message count.
    pub assistant_count: usize,
    /// System message count.
    pub system_count: usize,
    /// Other message count.
    pub other_count: usize,
    /// First timestamp.
    pub start_time: Option<DateTime<Utc>>,
    /// Last timestamp.
    pub end_time: Option<DateTime<Utc>>,
    /// Claude Code version.
    pub version: Option<String>,
    /// Schema version.
    pub schema_version: Option<SchemaVersion>,
}

impl QuickSessionMetadata {
    /// Calculate session duration.
    #[must_use]
    pub fn duration(&self) -> Option<chrono::Duration> {
        match (&self.start_time, &self.end_time) {
            (Some(start), Some(end)) => Some(*end - *start),
            _ => None,
        }
    }

    /// Get duration as human-readable string.
    #[must_use]
    pub fn duration_human(&self) -> Option<String> {
        self.duration().map(|d| {
            let total_secs = d.num_seconds();
            if total_secs < 60 {
                format!("{total_secs}s")
            } else if total_secs < 3600 {
                format!("{}m {}s", total_secs / 60, total_secs % 60)
            } else {
                format!(
                    "{}h {}m",
                    total_secs / 3600,
                    (total_secs % 3600) / 60
                )
            }
        })
    }
}

/// Summary information about a session for display.
#[derive(Debug, Clone)]
pub struct SessionSummary {
    /// Session ID.
    pub session_id: String,
    /// Whether this is a subagent session.
    pub is_subagent: bool,
    /// Parent project path.
    pub project_path: String,
    /// File size in bytes.
    pub file_size: u64,
    /// Human-readable file size.
    pub file_size_human: String,
    /// Total JSONL entry count.
    pub entry_count: usize,
    /// User + Assistant message count.
    pub message_count: usize,
    /// First timestamp.
    pub start_time: Option<DateTime<Utc>>,
    /// Last timestamp.
    pub end_time: Option<DateTime<Utc>>,
    /// Session duration.
    pub duration: Option<chrono::Duration>,
    /// Current session state.
    pub state: SessionState,
    /// Claude Code version.
    pub version: Option<String>,
}

impl SessionSummary {
    /// Get duration as human-readable string.
    #[must_use]
    pub fn duration_human(&self) -> Option<String> {
        self.duration.map(|d| {
            let total_secs = d.num_seconds();
            if total_secs < 60 {
                format!("{total_secs}s")
            } else if total_secs < 3600 {
                format!("{}m {}s", total_secs / 60, total_secs % 60)
            } else {
                format!(
                    "{}h {}m",
                    total_secs / 3600,
                    (total_secs % 3600) / 60
                )
            }
        })
    }

    /// Get a short display ID.
    #[must_use]
    pub fn short_id(&self) -> &str {
        if self.session_id.len() > 8 {
            &self.session_id[..8]
        } else {
            &self.session_id
        }
    }
}

/// Filter options for session listing.
#[derive(Debug, Clone, Default)]
pub struct SessionFilter {
    /// Include subagent sessions.
    pub include_subagents: bool,
    /// Only include sessions modified after this time.
    pub modified_after: Option<SystemTime>,
    /// Only include sessions modified before this time.
    pub modified_before: Option<SystemTime>,
    /// Minimum file size in bytes.
    pub min_size: Option<u64>,
    /// Maximum file size in bytes.
    pub max_size: Option<u64>,
    /// Only include active sessions.
    pub active_only: bool,
}

impl SessionFilter {
    /// Create a new filter that includes all sessions.
    #[must_use]
    pub fn new() -> Self {
        Self {
            include_subagents: true,
            ..Default::default()
        }
    }

    /// Exclude subagent sessions.
    #[must_use]
    pub fn main_only(mut self) -> Self {
        self.include_subagents = false;
        self
    }

    /// Filter by modification time range.
    #[must_use]
    pub fn modified_between(mut self, after: SystemTime, before: SystemTime) -> Self {
        self.modified_after = Some(after);
        self.modified_before = Some(before);
        self
    }

    /// Filter by minimum size.
    #[must_use]
    pub fn min_size(mut self, size: u64) -> Self {
        self.min_size = Some(size);
        self
    }

    /// Only include active sessions.
    #[must_use]
    pub fn active_only(mut self) -> Self {
        self.active_only = true;
        self
    }

    /// Check if a session matches this filter.
    pub fn matches(&self, session: &Session) -> Result<bool> {
        // Check subagent filter
        if !self.include_subagents && session.is_subagent() {
            return Ok(false);
        }

        // Check modification time
        if let Some(after) = self.modified_after {
            if session.modified_time() < after {
                return Ok(false);
            }
        }

        if let Some(before) = self.modified_before {
            if session.modified_time() > before {
                return Ok(false);
            }
        }

        // Check size
        if let Some(min) = self.min_size {
            if session.file_size() < min {
                return Ok(false);
            }
        }

        if let Some(max) = self.max_size {
            if session.file_size() > max {
                return Ok(false);
            }
        }

        // Check active status
        if self.active_only && !session.is_active()? {
            return Ok(false);
        }

        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_short_id() {
        let summary = SessionSummary {
            session_id: "40afc8a7-3fcb-4d29-b1ee-100b81b8c6c0".to_string(),
            is_subagent: false,
            project_path: "/test".to_string(),
            file_size: 1000,
            file_size_human: "1 KB".to_string(),
            entry_count: 10,
            message_count: 5,
            start_time: None,
            end_time: None,
            duration: None,
            state: SessionState::Inactive,
            version: Some("2.0.74".to_string()),
        };

        assert_eq!(summary.short_id(), "40afc8a7");
    }
}
