//! JSONL parsing for Claude Code session logs.
//!
//! This module provides high-performance parsing of JSONL files with:
//! - Streaming support for large files
//! - Graceful error recovery for malformed lines
//! - Schema version detection
//! - Partial line handling for active sessions

mod streaming;

pub use streaming::*;

use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::Path;

use crate::error::{Result, SnatchError};
use crate::model::{LogEntry, SchemaVersion};

/// JSONL parser for Claude Code session logs.
#[derive(Debug)]
pub struct JsonlParser {
    /// Detected schema version.
    schema_version: Option<SchemaVersion>,
    /// Whether to preserve original JSON for lossless export.
    preserve_raw: bool,
    /// Whether to skip malformed lines instead of failing.
    lenient: bool,
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
    pub fn parse_file(&mut self, path: impl AsRef<Path>) -> Result<Vec<LogEntry>> {
        let path = path.as_ref();
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

        let reader = BufReader::new(file);
        self.parse_reader(reader)
    }

    /// Parse JSONL from a reader.
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
                        continue;
                    }
                    return Err(e);
                }
            }
        }

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

/// Truncate a string for preview display.
fn truncate_preview(s: &str, max_len: usize) -> String {
    if s.len() <= max_len {
        s.to_string()
    } else {
        format!("{}...", &s[..max_len])
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
