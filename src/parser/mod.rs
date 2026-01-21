//! JSONL parsing for Claude Code session logs.
//!
//! This module provides high-performance parsing of JSONL files with:
//! - Streaming support for large files
//! - Graceful error recovery for malformed lines
//! - Schema version detection
//! - Partial line handling for active sessions
//!
//! # Example
//!
//! ```rust,no_run
//! use claude_snatch::parser::JsonlParser;
//!
//! // Parse from file
//! let mut parser = JsonlParser::new()
//!     .with_lenient(true);  // Skip malformed lines
//!
//! let entries = parser.parse_file("session.jsonl")?;
//! println!("Parsed {} entries", entries.len());
//!
//! // Check parsing statistics
//! let stats = parser.stats();
//! println!("Success rate: {:.1}%", stats.success_rate());
//! # Ok::<(), claude_snatch::SnatchError>(())
//! ```
//!
//! # Parsing Modes
//!
//! - **Lenient mode** (default): Skips malformed lines, logs errors
//! - **Strict mode**: Fails on first parse error
//!
//! ```rust
//! use claude_snatch::parser::JsonlParser;
//!
//! // Strict mode for validation
//! let mut strict_parser = JsonlParser::new().with_lenient(false);
//!
//! // Lenient mode for robust parsing
//! let mut lenient_parser = JsonlParser::new().with_lenient(true);
//! ```

mod streaming;

use tracing::{debug, instrument, trace, warn};

pub use streaming::*;

use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::Path;

use crate::error::{Result, SnatchError};
use crate::model::{LogEntry, SchemaVersion};

/// Default maximum file size (unlimited).
///
/// Previously defaulted to 100MB, but this caused friction for users who expect
/// all their data to be included. Use `--max-file-size N` to set a limit in bytes.
pub const DEFAULT_MAX_FILE_SIZE: u64 = 0;

/// JSONL parser for Claude Code session logs.
#[derive(Debug)]
pub struct JsonlParser {
    /// Detected schema version.
    schema_version: Option<SchemaVersion>,
    /// Whether to preserve original JSON for lossless export.
    preserve_raw: bool,
    /// Whether to skip malformed lines instead of failing.
    lenient: bool,
    /// Maximum file size in bytes (0 = unlimited).
    max_file_size: u64,
    /// Statistics about parsing.
    stats: ParseStats,
}

/// Statistics about parsing operations.
#[derive(Debug, Clone, Default)]
pub struct ParseStats {
    /// Total lines processed.
    pub lines_processed: usize,
    /// Successfully parsed entries.
    pub entries_parsed: usize,
    /// Malformed/skipped lines.
    pub lines_skipped: usize,
    /// Empty lines.
    pub empty_lines: usize,
    /// Detected schema version.
    pub schema_version: Option<SchemaVersion>,
    /// Parsing errors encountered.
    pub errors: Vec<ParseError>,
}

impl ParseStats {
    /// Calculate success rate as percentage.
    #[must_use]
    pub fn success_rate(&self) -> f64 {
        if self.lines_processed == 0 {
            return 100.0;
        }
        let valid = self.lines_processed - self.lines_skipped - self.empty_lines;
        if valid == 0 {
            return 0.0;
        }
        (self.entries_parsed as f64 / valid as f64) * 100.0
    }
}

/// A parsing error with context.
#[derive(Debug, Clone)]
pub struct ParseError {
    /// Line number where error occurred.
    pub line: usize,
    /// Error message.
    pub message: String,
    /// Original line content (truncated).
    pub content_preview: String,
}

impl JsonlParser {
    /// Create a new parser with default settings.
    #[must_use]
    pub fn new() -> Self {
        Self {
            schema_version: None,
            preserve_raw: false,
            lenient: true,
            max_file_size: DEFAULT_MAX_FILE_SIZE,
            stats: ParseStats::default(),
        }
    }

    /// Enable raw JSON preservation for lossless export.
    #[must_use]
    pub fn with_raw_preservation(mut self, preserve: bool) -> Self {
        self.preserve_raw = preserve;
        self
    }

    /// Set lenient mode (skip malformed lines instead of failing).
    #[must_use]
    pub fn with_lenient(mut self, lenient: bool) -> Self {
        self.lenient = lenient;
        self
    }

    /// Set maximum file size in bytes (0 = unlimited).
    ///
    /// Prevents memory exhaustion from malicious or corrupted large files.
    /// Default is 500 MB.
    #[must_use]
    pub fn with_max_file_size(mut self, max_bytes: u64) -> Self {
        self.max_file_size = max_bytes;
        self
    }

    /// Get parse statistics.
    #[must_use]
    pub fn stats(&self) -> &ParseStats {
        &self.stats
    }

    /// Get detected schema version.
    #[must_use]
    pub fn schema_version(&self) -> Option<&SchemaVersion> {
        self.schema_version.as_ref()
    }

    /// Parse a JSONL file from a path.
    #[instrument(skip(self), fields(path = %path.as_ref().display()))]
    pub fn parse_file(&mut self, path: impl AsRef<Path>) -> Result<Vec<LogEntry>> {
        let path = path.as_ref();
        debug!("Opening file for parsing");

        let file = File::open(path).map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                SnatchError::FileNotFound {
                    path: path.to_path_buf(),
                }
            } else if e.kind() == std::io::ErrorKind::PermissionDenied {
                SnatchError::PermissionDenied {
                    path: path.to_path_buf(),
                }
            } else {
                SnatchError::io(format!("Failed to open {}", path.display()), e)
            }
        })?;

        // Check file size limit to prevent memory exhaustion
        if self.max_file_size > 0 {
            let metadata = file.metadata().map_err(|e| {
                SnatchError::io(format!("Failed to get metadata for {}", path.display()), e)
            })?;
            let file_size = metadata.len();
            trace!(file_size, max_size = self.max_file_size, "Checking file size limit");

            if file_size > self.max_file_size {
                // Log at debug level since the error message already conveys this information
                debug!(
                    file_size,
                    max_size = self.max_file_size,
                    "File exceeds size limit, skipping"
                );
                return Err(SnatchError::validation(format!(
                    "File size ({}) exceeds maximum ({}). Use --max-file-size 0 for unlimited.",
                    format_bytes(file_size),
                    format_bytes(self.max_file_size)
                )));
            }
        }

        let reader = BufReader::new(file);
        self.parse_reader(reader)
    }

    /// Parse JSONL from a reader.
    #[instrument(skip(self, reader), level = "debug")]
    pub fn parse_reader<R: BufRead>(&mut self, reader: R) -> Result<Vec<LogEntry>> {
        let mut entries = Vec::new();
        self.stats = ParseStats::default();

        for (line_num, line_result) in reader.lines().enumerate() {
            let line_num = line_num + 1; // 1-indexed
            self.stats.lines_processed += 1;

            let line = match line_result {
                Ok(l) => l,
                Err(e) => {
                    if self.lenient {
                        self.stats.lines_skipped += 1;
                        self.stats.errors.push(ParseError {
                            line: line_num,
                            message: format!("I/O error: {e}"),
                            content_preview: String::new(),
                        });
                        warn!(line = line_num, error = %e, "I/O error reading line, skipping");
                        continue;
                    }
                    return Err(SnatchError::io(format!("Failed to read line {line_num}"), e));
                }
            };

            // Skip empty lines
            let trimmed = line.trim();
            if trimmed.is_empty() {
                self.stats.empty_lines += 1;
                continue;
            }

            // Parse the JSON line
            match self.parse_line(trimmed, line_num) {
                Ok(entry) => {
                    // Detect schema version from first entry with version field
                    if self.schema_version.is_none() {
                        if let Some(version) = entry.version() {
                            self.schema_version = Some(SchemaVersion::from_version_string(version));
                            self.stats.schema_version = self.schema_version.clone();
                            debug!(version, "Detected schema version");
                        }
                    }

                    self.stats.entries_parsed += 1;
                    entries.push(entry);
                }
                Err(e) => {
                    if self.lenient {
                        self.stats.lines_skipped += 1;
                        self.stats.errors.push(ParseError {
                            line: line_num,
                            message: e.to_string(),
                            content_preview: truncate_preview(trimmed, 100),
                        });
                        trace!(line = line_num, error = %e, "Parse error, skipping line");
                        continue;
                    }
                    return Err(e);
                }
            }
        }

        debug!(
            entries = entries.len(),
            lines = self.stats.lines_processed,
            skipped = self.stats.lines_skipped,
            "Parsing complete"
        );
        Ok(entries)
    }

    /// Parse a single JSON line.
    fn parse_line(&self, line: &str, line_num: usize) -> Result<LogEntry> {
        serde_json::from_str(line).map_err(|e| SnatchError::parse_with_source(line_num, e.to_string(), e))
    }

    /// Parse JSONL from a string.
    pub fn parse_str(&mut self, content: &str) -> Result<Vec<LogEntry>> {
        self.parse_reader(content.as_bytes())
    }

    /// Parse a single entry from a JSON string.
    pub fn parse_entry(json: &str) -> Result<LogEntry> {
        serde_json::from_str(json).map_err(|e| SnatchError::parse_with_source(0, e.to_string(), e))
    }
}

impl Default for JsonlParser {
    fn default() -> Self {
        Self::new()
    }
}

/// Format bytes in a human-readable format.
fn format_bytes(bytes: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = 1024 * KB;
    const GB: u64 = 1024 * MB;

    if bytes >= GB {
        format!("{:.1} GB", bytes as f64 / GB as f64)
    } else if bytes >= MB {
        format!("{:.1} MB", bytes as f64 / MB as f64)
    } else if bytes >= KB {
        format!("{:.1} KB", bytes as f64 / KB as f64)
    } else {
        format!("{bytes} B")
    }
}

/// Truncate a string for preview display.
///
/// Uses character-aware truncation to avoid panicking on multi-byte UTF-8 characters.
fn truncate_preview(s: &str, max_len: usize) -> String {
    if s.len() <= max_len {
        s.to_string()
    } else {
        // Find a valid character boundary at or before max_len
        let mut end = max_len;
        while end > 0 && !s.is_char_boundary(end) {
            end -= 1;
        }
        format!("{}...", &s[..end])
    }
}

/// A parsed entry with its raw JSON preserved.
#[derive(Debug, Clone)]
pub struct RawLogEntry {
    /// Parsed entry.
    pub entry: LogEntry,
    /// Original JSON string.
    pub raw: String,
    /// Line number in source file.
    pub line_num: usize,
}

impl RawLogEntry {
    /// Create from entry and raw JSON.
    #[must_use]
    pub fn new(entry: LogEntry, raw: String, line_num: usize) -> Self {
        Self { entry, raw, line_num }
    }
}

/// Iterator over log entries from a reader.
pub struct LogEntryIterator<R: BufRead> {
    reader: std::io::Lines<R>,
    line_num: usize,
    lenient: bool,
    errors: Vec<ParseError>,
}

impl<R: BufRead> LogEntryIterator<R> {
    /// Create a new iterator over log entries.
    pub fn new(reader: R, lenient: bool) -> Self {
        Self {
            reader: reader.lines(),
            line_num: 0,
            lenient,
            errors: Vec::new(),
        }
    }

    /// Get parsing errors encountered so far.
    #[must_use]
    pub fn errors(&self) -> &[ParseError] {
        &self.errors
    }

    /// Get current line number.
    #[must_use]
    pub fn line_num(&self) -> usize {
        self.line_num
    }
}

impl<R: BufRead> Iterator for LogEntryIterator<R> {
    type Item = Result<LogEntry>;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            let line_result = self.reader.next()?;
            self.line_num += 1;

            let line = match line_result {
                Ok(l) => l,
                Err(e) => {
                    if self.lenient {
                        self.errors.push(ParseError {
                            line: self.line_num,
                            message: format!("I/O error: {e}"),
                            content_preview: String::new(),
                        });
                        continue;
                    }
                    return Some(Err(SnatchError::io(
                        format!("Failed to read line {}", self.line_num),
                        e,
                    )));
                }
            };

            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }

            match serde_json::from_str::<LogEntry>(trimmed) {
                Ok(entry) => return Some(Ok(entry)),
                Err(e) => {
                    if self.lenient {
                        self.errors.push(ParseError {
                            line: self.line_num,
                            message: e.to_string(),
                            content_preview: truncate_preview(trimmed, 100),
                        });
                        continue;
                    }
                    return Some(Err(SnatchError::parse_with_source(
                        self.line_num,
                        e.to_string(),
                        e,
                    )));
                }
            }
        }
    }
}

/// Extension trait for creating iterators from files.
pub trait LogEntryIteratorExt {
    /// Create an iterator over log entries from a file.
    fn log_entries(self, lenient: bool) -> LogEntryIterator<BufReader<File>>;
}

impl LogEntryIteratorExt for File {
    fn log_entries(self, lenient: bool) -> LogEntryIterator<BufReader<File>> {
        LogEntryIterator::new(BufReader::new(self), lenient)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_empty() {
        let mut parser = JsonlParser::new();
        let entries = parser.parse_str("").unwrap();
        assert!(entries.is_empty());
    }

    #[test]
    fn test_parse_single_user_message() {
        let json = r#"{"uuid":"test-uuid","parentUuid":null,"type":"user","timestamp":"2025-12-23T00:00:00Z","sessionId":"session-1","version":"2.0.74","isSidechain":false,"message":{"role":"user","content":"Hello"}}"#;

        let mut parser = JsonlParser::new();
        let entries = parser.parse_str(json).unwrap();

        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].message_type(), "user");
        assert_eq!(entries[0].uuid(), Some("test-uuid"));
    }

    #[test]
    fn test_lenient_parsing() {
        let content = r#"{"uuid":"valid","parentUuid":null,"type":"user","timestamp":"2025-12-23T00:00:00Z","sessionId":"s1","version":"2.0.74","isSidechain":false,"message":{"role":"user","content":"test"}}
invalid json line
{"uuid":"valid2","parentUuid":null,"type":"user","timestamp":"2025-12-23T00:00:00Z","sessionId":"s1","version":"2.0.74","isSidechain":false,"message":{"role":"user","content":"test2"}}"#;

        let mut parser = JsonlParser::new().with_lenient(true);
        let entries = parser.parse_str(content).unwrap();

        assert_eq!(entries.len(), 2);
        assert_eq!(parser.stats().lines_skipped, 1);
    }

    #[test]
    fn test_schema_version_detection() {
        let json = r#"{"uuid":"test","parentUuid":null,"type":"user","timestamp":"2025-12-23T00:00:00Z","sessionId":"s1","version":"2.0.74","isSidechain":false,"message":{"role":"user","content":"test"}}"#;

        let mut parser = JsonlParser::new();
        let _ = parser.parse_str(json).unwrap();

        assert_eq!(
            parser.schema_version(),
            Some(&SchemaVersion::V2Lsp)
        );
    }

    #[test]
    fn test_parse_stats() {
        let content = r#"{"uuid":"1","parentUuid":null,"type":"user","timestamp":"2025-12-23T00:00:00Z","sessionId":"s1","version":"2.0.74","isSidechain":false,"message":{"role":"user","content":"a"}}

{"uuid":"2","parentUuid":null,"type":"user","timestamp":"2025-12-23T00:00:00Z","sessionId":"s1","version":"2.0.74","isSidechain":false,"message":{"role":"user","content":"b"}}
bad
"#;

        let mut parser = JsonlParser::new().with_lenient(true);
        let entries = parser.parse_str(content).unwrap();

        assert_eq!(entries.len(), 2);
        assert_eq!(parser.stats().lines_processed, 4);
        assert_eq!(parser.stats().empty_lines, 1);
        assert_eq!(parser.stats().lines_skipped, 1);
        assert_eq!(parser.stats().entries_parsed, 2);
    }
}
