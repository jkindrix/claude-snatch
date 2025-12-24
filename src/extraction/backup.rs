//! File backup/history extraction (BJ-001).
//!
//! Provides access to file backups stored in ~/.claude/filehistory/

use crate::error::Result;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

/// Summary of file history/backup data.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileHistorySummary {
    /// Total number of backup files.
    pub backup_count: usize,

    /// Total size of all backups in bytes.
    pub total_size_bytes: u64,

    /// Unique files backed up.
    pub unique_files: usize,

    /// Oldest backup timestamp.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub oldest_backup: Option<String>,

    /// Newest backup timestamp.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub newest_backup: Option<String>,

    /// Path to file history directory.
    pub directory_path: PathBuf,

    /// Backup file details (limited for performance).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub files: Vec<BackupFileInfo>,
}

impl FileHistorySummary {
    /// Create a summary from a file history directory.
    pub fn from_dir(dir: &Path) -> Result<Self> {
        if !dir.exists() {
            return Ok(Self {
                backup_count: 0,
                total_size_bytes: 0,
                unique_files: 0,
                oldest_backup: None,
                newest_backup: None,
                directory_path: dir.to_path_buf(),
                files: Vec::new(),
            });
        }

        let mut files = Vec::new();
        let mut total_size: u64 = 0;
        let mut oldest: Option<SystemTime> = None;
        let mut newest: Option<SystemTime> = None;
        let mut unique_sources: HashMap<String, usize> = HashMap::new();

        // Scan directory for backup files
        for entry in std::fs::read_dir(dir)? {
            let entry = entry?;
            let path = entry.path();

            if path.is_file() {
                let metadata = std::fs::metadata(&path)?;
                let size = metadata.len();
                total_size += size;

                let modified = metadata.modified().ok();

                // Track oldest/newest
                if let Some(mtime) = modified {
                    oldest = oldest.map(|o| o.min(mtime)).or(Some(mtime));
                    newest = newest.map(|n| n.max(mtime)).or(Some(mtime));
                }

                // Parse filename to extract source path
                if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
                    let source_path = decode_backup_filename(name);
                    if let Some(src) = &source_path {
                        *unique_sources.entry(src.clone()).or_insert(0) += 1;
                    }

                    files.push(BackupFileInfo {
                        backup_path: path,
                        source_path,
                        size_bytes: size,
                        modified_time: modified.and_then(|t| {
                            t.duration_since(SystemTime::UNIX_EPOCH)
                                .ok()
                                .map(|d| d.as_secs())
                        }),
                    });
                }
            }
        }

        // Limit files list for performance
        files.sort_by(|a, b| b.modified_time.cmp(&a.modified_time));
        if files.len() > 100 {
            files.truncate(100);
        }

        Ok(Self {
            backup_count: files.len(),
            total_size_bytes: total_size,
            unique_files: unique_sources.len(),
            oldest_backup: oldest.map(format_system_time),
            newest_backup: newest.map(format_system_time),
            directory_path: dir.to_path_buf(),
            files,
        })
    }

    /// Get human-readable total size.
    #[must_use]
    pub fn total_size_human(&self) -> String {
        format_size(self.total_size_bytes)
    }

    /// Check if there are any backups.
    #[must_use]
    pub fn has_backups(&self) -> bool {
        self.backup_count > 0
    }
}

/// Information about a single backup file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BackupFileInfo {
    /// Path to the backup file.
    pub backup_path: PathBuf,

    /// Original source file path (decoded from filename).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_path: Option<String>,

    /// File size in bytes.
    pub size_bytes: u64,

    /// Modification timestamp (Unix epoch seconds).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub modified_time: Option<u64>,
}

impl BackupFileInfo {
    /// Read the backup file contents.
    pub fn read_contents(&self) -> Result<String> {
        std::fs::read_to_string(&self.backup_path).map_err(|e| {
            crate::error::SnatchError::io(
                format!("Failed to read backup: {}", self.backup_path.display()),
                e,
            )
        })
    }

    /// Get human-readable file size.
    #[must_use]
    pub fn size_human(&self) -> String {
        format_size(self.size_bytes)
    }

    /// Get the backup filename.
    #[must_use]
    pub fn filename(&self) -> Option<&str> {
        self.backup_path.file_name().and_then(|n| n.to_str())
    }
}

/// File backup entry for correlation with JSONL events.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileBackupEntry {
    /// Original file path.
    pub file_path: String,

    /// Backup reference/ID.
    pub backup_ref: String,

    /// Session ID where backup was created.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,

    /// Timestamp of backup.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timestamp: Option<String>,

    /// File contents (if loaded).
    #[serde(skip)]
    pub contents: Option<String>,
}

impl FileBackupEntry {
    /// Load backup contents from file history directory.
    pub fn load_contents(&mut self, file_history_dir: &Path) -> Result<()> {
        // Try to find the backup file by reference
        let backup_path = file_history_dir.join(&self.backup_ref);
        if backup_path.exists() {
            self.contents = Some(std::fs::read_to_string(&backup_path)?);
        }
        Ok(())
    }
}

/// Decode backup filename to extract original source path.
fn decode_backup_filename(filename: &str) -> Option<String> {
    // Backup filenames are typically encoded with path separators replaced
    // Format varies by Claude Code version, so we try common patterns

    // Remove hash suffix if present (e.g., "path-to-file-abc123.bak")
    let name = filename.strip_suffix(".bak").unwrap_or(filename);

    // Try to find hash separator (last occurrence of hyphen followed by hex)
    if let Some(idx) = name.rfind('-') {
        let suffix = &name[idx + 1..];
        if suffix.chars().all(|c| c.is_ascii_hexdigit()) && suffix.len() >= 6 {
            let path_part = &name[..idx];
            // Replace encoded separators
            let decoded = path_part
                .replace("--", "\0DOUBLE\0") // Preserve double-dash
                .replace('-', "/")
                .replace("\0DOUBLE\0", "-");
            return Some(decoded);
        }
    }

    // Fallback: just replace dashes with slashes
    Some(name.replace('-', "/"))
}

/// Format SystemTime as ISO 8601 string.
fn format_system_time(time: SystemTime) -> String {
    let duration = time
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default();
    let secs = duration.as_secs();

    // Simple ISO 8601 formatting without external dependencies
    let days_since_epoch = secs / 86400;
    let time_of_day = secs % 86400;

    let hours = time_of_day / 3600;
    let minutes = (time_of_day % 3600) / 60;
    let seconds = time_of_day % 60;

    // Calculate date (simplified - assumes 365.25 days/year average)
    let years_approx = days_since_epoch / 365;
    let year = 1970 + years_approx;

    format!(
        "{year:04}-01-01T{hours:02}:{minutes:02}:{seconds:02}Z"
    )
}

/// File version history for a specific source file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileVersionHistory {
    /// Original source file path.
    pub source_path: String,

    /// All versions of this file in chronological order.
    pub versions: Vec<FileVersion>,
}

/// A single version of a file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileVersion {
    /// Backup file path.
    pub backup_path: PathBuf,

    /// Version number (1 = oldest).
    pub version: usize,

    /// File size in bytes.
    pub size_bytes: u64,

    /// Modification timestamp (Unix epoch seconds).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub modified_time: Option<u64>,

    /// ISO 8601 timestamp string.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timestamp: Option<String>,
}

impl FileVersion {
    /// Read the version's content.
    pub fn read_contents(&self) -> Result<String> {
        std::fs::read_to_string(&self.backup_path).map_err(|e| {
            crate::error::SnatchError::io(
                format!("Failed to read backup: {}", self.backup_path.display()),
                e,
            )
        })
    }

    /// Get human-readable file size.
    #[must_use]
    pub fn size_human(&self) -> String {
        format_size(self.size_bytes)
    }
}

impl FileVersionHistory {
    /// Build version history for a file from the file history directory.
    pub fn from_dir(file_history_dir: &Path, source_path: &str) -> Result<Self> {
        let mut versions = Vec::new();

        if !file_history_dir.exists() {
            return Ok(Self {
                source_path: source_path.to_string(),
                versions,
            });
        }

        // Scan for backup files matching this source path
        for entry in std::fs::read_dir(file_history_dir)? {
            let entry = entry?;
            let path = entry.path();

            if path.is_file() {
                if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
                    if let Some(decoded) = decode_backup_filename(name) {
                        // Check if this backup matches our source path
                        if decoded == source_path || decoded.ends_with(source_path) {
                            let metadata = std::fs::metadata(&path)?;
                            let modified = metadata.modified().ok();

                            versions.push(FileVersion {
                                backup_path: path,
                                version: 0, // Will be set after sorting
                                size_bytes: metadata.len(),
                                modified_time: modified.and_then(|t| {
                                    t.duration_since(SystemTime::UNIX_EPOCH)
                                        .ok()
                                        .map(|d| d.as_secs())
                                }),
                                timestamp: modified.map(format_system_time),
                            });
                        }
                    }
                }
            }
        }

        // Sort by modification time (oldest first) and assign version numbers
        versions.sort_by_key(|v| v.modified_time.unwrap_or(0));
        for (idx, version) in versions.iter_mut().enumerate() {
            version.version = idx + 1;
        }

        Ok(Self {
            source_path: source_path.to_string(),
            versions,
        })
    }

    /// Get the latest version.
    #[must_use]
    pub fn latest(&self) -> Option<&FileVersion> {
        self.versions.last()
    }

    /// Get a specific version by number.
    #[must_use]
    pub fn get_version(&self, version: usize) -> Option<&FileVersion> {
        self.versions.iter().find(|v| v.version == version)
    }

    /// Get number of versions.
    #[must_use]
    pub fn version_count(&self) -> usize {
        self.versions.len()
    }
}

/// Format size in bytes as human-readable string.
fn format_size(bytes: u64) -> String {
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
    use std::io::Write;
    use tempfile::tempdir;

    #[test]
    fn test_file_history_summary_empty() {
        let dir = tempdir().unwrap();
        let summary = FileHistorySummary::from_dir(dir.path()).unwrap();

        assert_eq!(summary.backup_count, 0);
        assert_eq!(summary.total_size_bytes, 0);
        assert!(!summary.has_backups());
    }

    #[test]
    fn test_file_history_summary_with_files() {
        let dir = tempdir().unwrap();

        // Create test backup files
        let backup1 = dir.path().join("home-user-test-file.txt-abc123.bak");
        let mut f = std::fs::File::create(&backup1).unwrap();
        writeln!(f, "Backup content 1").unwrap();

        let backup2 = dir.path().join("home-user-other-file.rs-def456.bak");
        let mut f = std::fs::File::create(&backup2).unwrap();
        writeln!(f, "Backup content 2").unwrap();

        let summary = FileHistorySummary::from_dir(dir.path()).unwrap();

        assert_eq!(summary.backup_count, 2);
        assert!(summary.total_size_bytes > 0);
        assert!(summary.has_backups());
        assert!(summary.unique_files > 0);
    }

    #[test]
    fn test_decode_backup_filename() {
        let decoded = decode_backup_filename("home-user-project-file.rs-abc123.bak");
        assert_eq!(decoded, Some("home/user/project/file.rs".to_string()));
    }

    #[test]
    fn test_format_size() {
        assert_eq!(format_size(500), "500 B");
        assert_eq!(format_size(1024), "1.00 KB");
        assert_eq!(format_size(1536), "1.50 KB");
        assert_eq!(format_size(1048576), "1.00 MB");
    }
}
