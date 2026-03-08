//! Session file discovery and metadata.
//!
//! A session corresponds to a single JSONL file containing conversation history.
//! Sessions can be main conversations or subagent sessions (agent-*.jsonl).

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use chrono::{DateTime, Utc};
use tracing::{debug, instrument, trace};

use crate::cache::global_cache;
use crate::error::{Result, SnatchError};
use crate::model::{LogEntry, SchemaVersion, SystemSubtype};
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
    /// Parent session ID for subagent sessions (extracted from directory structure).
    /// When a subagent session lives at `<parent-uuid>/subagents/agent-*.jsonl`,
    /// this field contains the parent UUID.
    parent_session_id: Option<String>,
}

impl Session {
    /// Create a Session from its file path.
    #[instrument(skip(path, project_path), fields(path = %path.as_ref().display()))]
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

        trace!(
            session_id = %file_info.session_id,
            is_subagent = file_info.is_subagent,
            file_size = metadata.len(),
            "Session loaded"
        );

        // Extract parent session ID from directory structure.
        // Subagent files live at <parent-uuid>/subagents/agent-*.jsonl
        let parent_session_id = if file_info.is_subagent {
            path.parent() // subagents/
                .and_then(|p| p.parent()) // <parent-uuid>/
                .and_then(|p| p.file_name())
                .and_then(|n| n.to_str())
                .filter(|name| {
                    // Validate it looks like a UUID (8-4-4-4-12 format)
                    name.len() == 36 && name.chars().filter(|&c| c == '-').count() == 4
                })
                .map(String::from)
        } else {
            None
        };

        Ok(Self {
            path,
            session_id: file_info.session_id,
            is_subagent: file_info.is_subagent,
            agent_hash: file_info.agent_hash,
            file_size: metadata.len(),
            modified_time,
            project_path: project_path.to_string(),
            parent_session_id,
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

    /// Check if this is a subagent session (based on filename).
    ///
    /// For a more accurate check that also considers content (parentUuid),
    /// use `effective_is_subagent()`.
    #[must_use]
    pub fn is_subagent(&self) -> bool {
        self.is_subagent
    }

    /// Check if this is a subagent session, considering both filename and content.
    ///
    /// Returns true if the filename indicates a subagent OR if the JSONL content
    /// has a non-null parentUuid on the first entry (handles UUID-named subagents).
    /// Falls back to filename-based classification if metadata isn't cached.
    #[must_use]
    pub fn effective_is_subagent(&self) -> bool {
        if self.is_subagent {
            return true;
        }
        // Check content-based classification from cached metadata
        global_cache()
            .get_metadata(&self.path)
            .map(|m| m.content_is_subagent)
            .unwrap_or(false)
    }

    /// Get the agent hash if this is a subagent session.
    #[must_use]
    pub fn agent_hash(&self) -> Option<&str> {
        self.agent_hash.as_deref()
    }

    /// Get the parent session ID for subagent sessions.
    ///
    /// Extracted from the directory structure: `<parent-uuid>/subagents/agent-*.jsonl`.
    #[must_use]
    pub fn parent_session_id(&self) -> Option<&str> {
        self.parent_session_id.as_deref()
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
    #[instrument(skip(self), fields(session_id = %self.session_id))]
    pub fn parse(&self) -> Result<Vec<LogEntry>> {
        debug!("Parsing session");
        self.parse_with_options(None)
    }

    /// Parse all entries with custom max file size.
    ///
    /// # Arguments
    /// * `max_file_size` - Maximum file size in bytes. Use 0 for unlimited.
    ///                     If None, uses the default limit.
    #[instrument(skip(self), fields(session_id = %self.session_id))]
    pub fn parse_with_options(&self, max_file_size: Option<u64>) -> Result<Vec<LogEntry>> {
        let mut parser = JsonlParser::new().with_lenient(true);
        if let Some(max_size) = max_file_size {
            parser = parser.with_max_file_size(max_size);
        }
        let entries = parser.parse_file(&self.path)?;
        debug!(entries = entries.len(), "Session parsed");
        Ok(entries)
    }

    /// Parse all entries with caching support.
    ///
    /// Uses the global cache to avoid re-parsing unchanged files.
    #[instrument(skip(self), fields(session_id = %self.session_id))]
    pub fn parse_cached(&self) -> Result<std::sync::Arc<Vec<LogEntry>>> {
        trace!("Checking cache for session");
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

        // Find the first entry with a timestamp (Summary entries don't have timestamps)
        let start_time = entries.iter().find_map(|e| e.timestamp());
        // Find the last entry with a timestamp
        let end_time = entries.iter().rev().find_map(|e| e.timestamp());
        // Get version from first entry that has one
        let version = entries.iter().find_map(|e| e.version().map(String::from));
        let schema_version = version
            .as_deref()
            .map(SchemaVersion::from_version_string);

        // Extract the working directory from the first entry that has it
        // This is the authoritative project path from the JSONL file
        let extracted_cwd = entries.iter().find_map(|e| e.cwd().map(String::from));

        // Extract git branch from the first entry that has it
        let git_branch = entries.iter().find_map(|e| e.git_branch().map(String::from));

        // Check if content indicates this is a subagent (parentUuid on first entry)
        let content_is_subagent = entries.first()
            .and_then(|e| e.parent_uuid())
            .is_some();

        // Count message types and compaction events
        let mut user_count = 0;
        let mut assistant_count = 0;
        let mut system_count = 0;
        let mut other_count = 0;
        let mut compaction_count = 0;

        for entry in &entries {
            match entry {
                LogEntry::System(sys) => {
                    system_count += 1;
                    if sys.subtype == Some(SystemSubtype::CompactBoundary) {
                        compaction_count += 1;
                    }
                }
                _ => match entry.message_type() {
                    "user" => user_count += 1,
                    "assistant" => assistant_count += 1,
                    _ => other_count += 1,
                },
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
            extracted_cwd,
            git_branch,
            compaction_count,
            content_is_subagent,
            // Left at 0 to avoid expensive filesystem scans during metadata computation.
            // Use Session::tool_result_stats() directly when needed.
            tool_result_count: 0,
            tool_result_size: 0,
        })
    }

    /// Get session summary suitable for display.
    pub fn summary(&self) -> Result<SessionSummary> {
        let meta = self.quick_metadata()?;
        let state = self.state()?;

        Ok(SessionSummary {
            session_id: self.session_id.clone(),
            is_subagent: self.is_subagent,
            parent_session_id: self.parent_session_id.clone(),
            project_path: self.project_path.clone(),
            extracted_cwd: meta.extracted_cwd.clone(),
            git_branch: meta.git_branch.clone(),
            file_size: self.file_size,
            file_size_human: self.file_size_human(),
            entry_count: meta.entry_count,
            message_count: meta.user_count + meta.assistant_count,
            compaction_count: meta.compaction_count,
            start_time: meta.start_time,
            end_time: meta.end_time,
            duration: meta.duration(),
            state,
            version: meta.version,
        })
    }

    /// Get the tool-results directory for this session, if it exists.
    ///
    /// For main sessions at `<project>/<uuid>.jsonl`, looks at `<project>/<uuid>/tool-results/`.
    /// For subagent sessions at `<project>/<parent>/subagents/agent-*.jsonl`,
    /// looks at `<project>/<parent>/tool-results/`.
    #[must_use]
    pub fn tool_results_dir(&self) -> Option<PathBuf> {
        let parent = self.path.parent()?;
        let dir = if self.is_subagent {
            // subagents/ -> <parent-uuid>/ -> tool-results/
            parent.parent()?.join("tool-results")
        } else {
            // <project>/ -> <uuid>/ -> tool-results/
            let stem = self.path.file_stem()?.to_str()?;
            parent.join(stem).join("tool-results")
        };
        if dir.is_dir() { Some(dir) } else { None }
    }

    /// Count external tool result files and their total size.
    pub fn tool_result_stats(&self) -> (usize, u64) {
        let Some(dir) = self.tool_results_dir() else {
            return (0, 0);
        };
        let mut count = 0usize;
        let mut size = 0u64;
        if let Ok(entries) = std::fs::read_dir(&dir) {
            for entry in entries.flatten() {
                if entry.path().extension().and_then(|e| e.to_str()) == Some("txt") {
                    count += 1;
                    size += entry.metadata().map(|m| m.len()).unwrap_or(0);
                }
            }
        }
        (count, size)
    }

    /// Get the authoritative project path.
    ///
    /// This method extracts the `cwd` from the JSONL file if available,
    /// which is the actual working directory. Falls back to the decoded
    /// directory name if the JSONL doesn't contain a `cwd` field.
    pub fn authoritative_project_path(&self) -> Result<String> {
        let meta = self.quick_metadata_cached()?;
        Ok(meta.extracted_cwd.unwrap_or_else(|| self.project_path.clone()))
    }
}

/// Quick metadata extracted from a session without full parsing.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
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
    /// Working directory extracted from JSONL (authoritative project path).
    /// This is the actual `cwd` field from the first message that has it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub extracted_cwd: Option<String>,
    /// Git branch extracted from JSONL.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub git_branch: Option<String>,
    /// Number of compaction events in this session.
    pub compaction_count: usize,
    /// Whether this session is a subagent based on content analysis.
    /// True when the first entry has a non-null parentUuid (even if filename
    /// looks like a main session UUID).
    pub content_is_subagent: bool,
    /// Number of external tool result files (in tool-results/ directory).
    #[serde(default)]
    pub tool_result_count: usize,
    /// Total size of external tool result files in bytes.
    #[serde(default)]
    pub tool_result_size: u64,
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
    /// Parent session ID for subagent sessions.
    pub parent_session_id: Option<String>,
    /// Parent project path (decoded from directory name, may be inaccurate).
    pub project_path: String,
    /// Authoritative project path extracted from JSONL `cwd` field.
    /// This is the actual working directory and should be preferred over `project_path`.
    pub extracted_cwd: Option<String>,
    /// Git branch extracted from JSONL.
    pub git_branch: Option<String>,
    /// File size in bytes.
    pub file_size: u64,
    /// Human-readable file size.
    pub file_size_human: String,
    /// Total JSONL entry count.
    pub entry_count: usize,
    /// User + Assistant message count.
    pub message_count: usize,
    /// Number of compaction events in this session.
    pub compaction_count: usize,
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
    /// Only include subagent sessions (excludes main sessions).
    pub subagents_only: bool,
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

    /// Only include subagent sessions.
    #[must_use]
    pub fn subagents_only(mut self) -> Self {
        self.subagents_only = true;
        self.include_subagents = true;
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
        // Use effective_is_subagent which considers both filename and content
        let is_sub = session.effective_is_subagent();

        // Check subagent filter
        if !self.include_subagents && is_sub {
            return Ok(false);
        }

        // Check subagents-only filter
        if self.subagents_only && !is_sub {
            return Ok(false);
        }

        // Check time range using content-based timestamps (not file mtime).
        // File mtime changes on compaction, so a January session compacted in
        // March would have a March mtime. Content timestamps are authoritative.
        if self.modified_after.is_some() || self.modified_before.is_some() {
            let (start, end) = match session.quick_metadata_cached() {
                Ok(meta) => {
                    let start = meta.start_time
                        .map(|t| SystemTime::from(t))
                        .unwrap_or_else(|| session.modified_time());
                    let end = meta.end_time
                        .map(|t| SystemTime::from(t))
                        .unwrap_or_else(|| session.modified_time());
                    (start, end)
                }
                Err(_) => (session.modified_time(), session.modified_time()),
            };
            if let Some(after) = self.modified_after {
                if end < after {
                    return Ok(false);
                }
            }
            if let Some(before) = self.modified_before {
                if start > before {
                    return Ok(false);
                }
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
            parent_session_id: None,
            project_path: "/test".to_string(),
            extracted_cwd: Some("/actual/test/path".to_string()),
            git_branch: Some("main".to_string()),
            file_size: 1000,
            file_size_human: "1 KB".to_string(),
            entry_count: 10,
            message_count: 5,
            compaction_count: 0,
            start_time: None,
            end_time: None,
            duration: None,
            state: SessionState::Inactive,
            version: Some("2.0.74".to_string()),
        };

        assert_eq!(summary.short_id(), "40afc8a7");
    }

    #[test]
    fn test_session_summary_prefers_extracted_cwd() {
        let summary = SessionSummary {
            session_id: "test".to_string(),
            is_subagent: false,
            parent_session_id: None,
            project_path: "/decoded/path".to_string(),
            extracted_cwd: Some("/actual/path".to_string()),
            git_branch: None,
            file_size: 0,
            file_size_human: "0 B".to_string(),
            entry_count: 0,
            message_count: 0,
            compaction_count: 0,
            start_time: None,
            end_time: None,
            duration: None,
            state: SessionState::Inactive,
            version: None,
        };

        // The extracted_cwd should be preferred over project_path
        assert_eq!(summary.extracted_cwd, Some("/actual/path".to_string()));
        assert_eq!(summary.project_path, "/decoded/path");
    }
}
