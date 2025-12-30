//! Streaming JSONL parser for large files.
//!
//! This module provides memory-efficient streaming parsing for large JSONL files,
//! processing entries one at a time without loading the entire file into memory.

// Allow unsafe for mmap feature - memory mapping requires unsafe due to potential
// undefined behavior if the file is modified while mapped.
#![cfg_attr(feature = "mmap", allow(unsafe_code))]

use std::fs::File;
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom};
use std::path::Path;
use std::time::{Duration, SystemTime};

use crate::error::{Result, SnatchError};
use crate::model::LogEntry;

/// Streaming parser for processing large JSONL files efficiently.
pub struct StreamingParser<R> {
    reader: R,
    line_num: usize,
    bytes_read: u64,
    lenient: bool,
    buffer: String,
}

impl<R: BufRead> StreamingParser<R> {
    /// Create a new streaming parser.
    pub fn new(reader: R) -> Self {
        Self {
            reader,
            line_num: 0,
            bytes_read: 0,
            lenient: true,
            buffer: String::with_capacity(4096),
        }
    }

    /// Set lenient mode.
    #[must_use]
    pub fn lenient(mut self, lenient: bool) -> Self {
        self.lenient = lenient;
        self
    }

    /// Get current line number.
    #[must_use]
    pub const fn line_num(&self) -> usize {
        self.line_num
    }

    /// Get bytes read so far.
    #[must_use]
    pub const fn bytes_read(&self) -> u64 {
        self.bytes_read
    }

    /// Read the next entry.
    pub fn next_entry(&mut self) -> Option<Result<LogEntry>> {
        loop {
            self.buffer.clear();

            match self.reader.read_line(&mut self.buffer) {
                Ok(0) => return None, // EOF
                Ok(n) => {
                    self.line_num += 1;
                    self.bytes_read += n as u64;

                    let trimmed = self.buffer.trim();
                    if trimmed.is_empty() {
                        continue;
                    }

                    match serde_json::from_str::<LogEntry>(trimmed) {
                        Ok(entry) => return Some(Ok(entry)),
                        Err(e) => {
                            if self.lenient {
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
                Err(e) => {
                    if self.lenient {
                        self.line_num += 1;
                        continue;
                    }
                    return Some(Err(SnatchError::io(
                        format!("Failed to read line {}", self.line_num + 1),
                        e,
                    )));
                }
            }
        }
    }

    /// Iterate over all entries.
    pub fn entries(self) -> StreamingIterator<R> {
        StreamingIterator { parser: self }
    }
}

/// Iterator adapter for streaming parser.
pub struct StreamingIterator<R> {
    parser: StreamingParser<R>,
}

impl<R: BufRead> Iterator for StreamingIterator<R> {
    type Item = Result<LogEntry>;

    fn next(&mut self) -> Option<Self::Item> {
        self.parser.next_entry()
    }
}

/// Open a file for streaming parsing.
pub fn open_stream(path: impl AsRef<Path>) -> Result<StreamingParser<BufReader<File>>> {
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

    Ok(StreamingParser::new(BufReader::with_capacity(64 * 1024, file)))
}

/// Session file state detection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionState {
    /// File hasn't been modified recently.
    Inactive,
    /// File was modified recently (within 60 seconds).
    RecentlyActive,
    /// File was modified very recently (within 5 seconds).
    PossiblyActive,
}

impl SessionState {
    /// Get human-readable description.
    #[must_use]
    pub const fn description(&self) -> &'static str {
        match self {
            Self::Inactive => "inactive",
            Self::RecentlyActive => "recently active",
            Self::PossiblyActive => "possibly active",
        }
    }

    /// Check if this state indicates the session is active.
    #[must_use]
    pub const fn is_active(&self) -> bool {
        matches!(self, Self::RecentlyActive | Self::PossiblyActive)
    }
}

/// Check if a session file is currently active.
pub fn detect_session_state(path: impl AsRef<Path>) -> Result<SessionState> {
    let path = path.as_ref();
    let metadata = std::fs::metadata(path).map_err(|e| {
        SnatchError::io(format!("Failed to read metadata for {}", path.display()), e)
    })?;

    let modified = metadata.modified().map_err(|e| {
        SnatchError::io(format!("Failed to get mtime for {}", path.display()), e)
    })?;

    let now = SystemTime::now();
    let age = now.duration_since(modified).unwrap_or(Duration::MAX);

    if age < Duration::from_secs(5) {
        Ok(SessionState::PossiblyActive)
    } else if age < Duration::from_secs(60) {
        Ok(SessionState::RecentlyActive)
    } else {
        Ok(SessionState::Inactive)
    }
}

/// Check if the last line in a file is incomplete (no newline terminator).
pub fn has_incomplete_line(path: impl AsRef<Path>) -> Result<bool> {
    let path = path.as_ref();
    let mut file = File::open(path).map_err(|e| {
        SnatchError::io(format!("Failed to open {}", path.display()), e)
    })?;

    let metadata = file.metadata().map_err(|e| {
        SnatchError::io(format!("Failed to read metadata for {}", path.display()), e)
    })?;

    if metadata.len() == 0 {
        return Ok(false);
    }

    // Seek to last byte
    file.seek(SeekFrom::End(-1)).map_err(|e| {
        SnatchError::io(format!("Failed to seek in {}", path.display()), e)
    })?;

    let mut buf = [0u8; 1];
    file.read_exact(&mut buf).map_err(|e| {
        SnatchError::io(format!("Failed to read last byte from {}", path.display()), e)
    })?;

    Ok(buf[0] != b'\n')
}

/// Parser for active/in-progress session files.
pub struct ActiveSessionParser<R> {
    inner: StreamingParser<R>,
    wait_for_complete: bool,
    poll_interval: Duration,
}

impl<R: BufRead> ActiveSessionParser<R> {
    /// Create a new active session parser.
    pub fn new(reader: R) -> Self {
        Self {
            inner: StreamingParser::new(reader),
            wait_for_complete: false,
            poll_interval: Duration::from_millis(100),
        }
    }

    /// Enable waiting for incomplete lines to complete.
    #[must_use]
    pub fn wait_for_complete(mut self, wait: bool) -> Self {
        self.wait_for_complete = wait;
        self
    }

    /// Set the poll interval when waiting.
    #[must_use]
    pub fn poll_interval(mut self, interval: Duration) -> Self {
        self.poll_interval = interval;
        self
    }

    /// Get the underlying streaming parser.
    #[must_use]
    pub fn into_inner(self) -> StreamingParser<R> {
        self.inner
    }
}

/// Progress tracking for streaming operations.
#[derive(Debug, Clone, Default)]
pub struct StreamingProgress {
    /// Total bytes in source file.
    pub total_bytes: u64,
    /// Bytes processed so far.
    pub bytes_processed: u64,
    /// Lines processed.
    pub lines_processed: usize,
    /// Entries parsed successfully.
    pub entries_parsed: usize,
    /// Errors encountered.
    pub errors: usize,
}

impl StreamingProgress {
    /// Calculate progress percentage.
    #[must_use]
    pub fn percentage(&self) -> f64 {
        if self.total_bytes == 0 {
            return 100.0;
        }
        (self.bytes_processed as f64 / self.total_bytes as f64) * 100.0
    }

    /// Estimate remaining bytes.
    #[must_use]
    pub fn remaining_bytes(&self) -> u64 {
        self.total_bytes.saturating_sub(self.bytes_processed)
    }
}

/// Callback for streaming progress updates.
pub type ProgressCallback = Box<dyn Fn(&StreamingProgress) + Send + Sync>;

/// Streaming parser with progress callbacks.
pub struct ProgressStreamingParser<R> {
    inner: StreamingParser<R>,
    progress: StreamingProgress,
    callback: Option<ProgressCallback>,
    callback_interval: usize,
    entries_since_callback: usize,
}

impl<R: BufRead> ProgressStreamingParser<R> {
    /// Create a new progress-tracking parser.
    pub fn new(reader: R, total_bytes: u64) -> Self {
        Self {
            inner: StreamingParser::new(reader),
            progress: StreamingProgress {
                total_bytes,
                ..Default::default()
            },
            callback: None,
            callback_interval: 100,
            entries_since_callback: 0,
        }
    }

    /// Set progress callback.
    pub fn on_progress(mut self, callback: impl Fn(&StreamingProgress) + Send + Sync + 'static) -> Self {
        self.callback = Some(Box::new(callback));
        self
    }

    /// Set callback interval (entries between callbacks).
    #[must_use]
    pub fn callback_interval(mut self, interval: usize) -> Self {
        self.callback_interval = interval;
        self
    }

    /// Get current progress.
    #[must_use]
    pub fn progress(&self) -> &StreamingProgress {
        &self.progress
    }

    /// Read next entry with progress tracking.
    pub fn next_entry(&mut self) -> Option<Result<LogEntry>> {
        let result = self.inner.next_entry();

        // Update progress
        self.progress.bytes_processed = self.inner.bytes_read();
        self.progress.lines_processed = self.inner.line_num();

        match &result {
            Some(Ok(_)) => {
                self.progress.entries_parsed += 1;
                self.entries_since_callback += 1;
            }
            Some(Err(_)) => {
                self.progress.errors += 1;
            }
            None => {}
        }

        // Call progress callback if appropriate
        if let Some(callback) = &self.callback {
            if self.entries_since_callback >= self.callback_interval || result.is_none() {
                callback(&self.progress);
                self.entries_since_callback = 0;
            }
        }

        result
    }
}

/// Memory-mapped parser for very large files.
///
/// This parser uses memory-mapped I/O for efficient zero-copy parsing
/// of large JSONL files. The file contents are mapped directly into
/// memory without copying, reducing memory allocations and improving
/// performance for files larger than available RAM.
#[cfg(feature = "mmap")]
pub struct MmapParser {
    /// Memory-mapped file contents.
    mmap: memmap2::Mmap,
    /// Current byte offset in the file.
    offset: usize,
    /// Current line number (1-indexed).
    line_num: usize,
    /// Whether to skip malformed lines.
    lenient: bool,
    /// Parsing errors encountered.
    errors: Vec<crate::parser::ParseError>,
}

#[cfg(feature = "mmap")]
impl MmapParser {
    /// Create a new memory-mapped parser from a file.
    ///
    /// # Safety
    /// The file must not be modified while the parser is in use.
    pub fn new(file: &std::fs::File) -> std::io::Result<Self> {
        // SAFETY: We document that the file must not be modified.
        // The unsafe block is required by memmap2 because memory-mapped
        // files can have undefined behavior if the underlying file is
        // modified while mapped.
        let mmap = unsafe { memmap2::Mmap::map(file)? };
        Ok(Self {
            mmap,
            offset: 0,
            line_num: 0,
            lenient: true,
            errors: Vec::new(),
        })
    }

    /// Create from a file path.
    pub fn from_path(path: impl AsRef<std::path::Path>) -> crate::error::Result<Self> {
        let file = std::fs::File::open(path.as_ref()).map_err(|e| {
            crate::error::SnatchError::io(
                format!("Failed to open file: {}", path.as_ref().display()),
                e,
            )
        })?;
        Self::new(&file).map_err(|e| {
            crate::error::SnatchError::io(
                format!("Failed to memory-map file: {}", path.as_ref().display()),
                e,
            )
        })
    }

    /// Set lenient mode.
    #[must_use]
    pub fn lenient(mut self, lenient: bool) -> Self {
        self.lenient = lenient;
        self
    }

    /// Get current line number.
    #[must_use]
    pub fn line_num(&self) -> usize {
        self.line_num
    }

    /// Get parsing errors.
    #[must_use]
    pub fn errors(&self) -> &[crate::parser::ParseError] {
        &self.errors
    }

    /// Get remaining bytes to parse.
    #[must_use]
    pub fn remaining(&self) -> usize {
        self.mmap.len().saturating_sub(self.offset)
    }

    /// Get the total file size.
    #[must_use]
    pub fn file_size(&self) -> usize {
        self.mmap.len()
    }

    /// Get the bytes as a slice (zero-copy access).
    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        &self.mmap[..]
    }

    /// Parse the next entry from the memory-mapped file.
    ///
    /// Returns None when the end of file is reached.
    pub fn next_entry(&mut self) -> Option<crate::error::Result<crate::model::LogEntry>> {
        loop {
            if self.offset >= self.mmap.len() {
                return None;
            }

            // Find the next newline
            let remaining = &self.mmap[self.offset..];
            let line_end = remaining.iter().position(|&b| b == b'\n');

            let line_bytes = match line_end {
                Some(pos) => {
                    let line = &remaining[..pos];
                    self.offset += pos + 1;
                    line
                }
                None => {
                    // Last line without newline
                    let line = remaining;
                    self.offset = self.mmap.len();
                    line
                }
            };

            self.line_num += 1;

            // Skip empty lines
            let line_str = match std::str::from_utf8(line_bytes) {
                Ok(s) => s.trim(),
                Err(e) => {
                    if self.lenient {
                        self.errors.push(crate::parser::ParseError {
                            line: self.line_num,
                            message: format!("Invalid UTF-8: {e}"),
                            content_preview: String::new(),
                        });
                        continue;
                    }
                    return Some(Err(crate::error::SnatchError::DataIntegrityError {
                        message: format!("Invalid UTF-8 at line {}: {e}", self.line_num),
                    }));
                }
            };

            if line_str.is_empty() {
                continue;
            }

            // Parse JSON (zero-copy: line_str borrows from mmap)
            match serde_json::from_str::<crate::model::LogEntry>(line_str) {
                Ok(entry) => return Some(Ok(entry)),
                Err(e) => {
                    if self.lenient {
                        self.errors.push(crate::parser::ParseError {
                            line: self.line_num,
                            message: e.to_string(),
                            content_preview: line_str.chars().take(100).collect(),
                        });
                        continue;
                    }
                    return Some(Err(crate::error::SnatchError::parse_with_source(
                        self.line_num,
                        e.to_string(),
                        e,
                    )));
                }
            }
        }
    }

    /// Parse all entries from the file.
    pub fn parse_all(&mut self) -> crate::error::Result<Vec<crate::model::LogEntry>> {
        let mut entries = Vec::new();
        while let Some(result) = self.next_entry() {
            entries.push(result?);
        }
        Ok(entries)
    }

    /// Create an iterator over entries.
    pub fn entries(self) -> MmapParserIterator {
        MmapParserIterator { parser: self }
    }
}

/// Iterator over entries from a memory-mapped parser.
#[cfg(feature = "mmap")]
pub struct MmapParserIterator {
    parser: MmapParser,
}

#[cfg(feature = "mmap")]
impl Iterator for MmapParserIterator {
    type Item = crate::error::Result<crate::model::LogEntry>;

    fn next(&mut self) -> Option<Self::Item> {
        self.parser.next_entry()
    }
}

/// Zero-copy JSON line parser.
///
/// This struct holds a reference to bytes and provides zero-copy
/// parsing of individual JSON lines without allocating strings.
#[derive(Debug)]
pub struct ZeroCopyLine<'a> {
    /// Raw bytes of the line.
    bytes: &'a [u8],
    /// Line number (1-indexed).
    pub line_num: usize,
}

impl<'a> ZeroCopyLine<'a> {
    /// Create from bytes.
    #[must_use]
    pub fn new(bytes: &'a [u8], line_num: usize) -> Self {
        Self { bytes, line_num }
    }

    /// Get the line as a string slice (zero-copy).
    pub fn as_str(&self) -> std::result::Result<&'a str, std::str::Utf8Error> {
        std::str::from_utf8(self.bytes)
    }

    /// Parse into a LogEntry.
    pub fn parse(&self) -> crate::error::Result<crate::model::LogEntry> {
        let s = self.as_str().map_err(|e| {
            crate::error::SnatchError::DataIntegrityError {
                message: format!("Invalid UTF-8 at line {}: {e}", self.line_num),
            }
        })?;

        serde_json::from_str(s.trim()).map_err(|e| {
            crate::error::SnatchError::parse_with_source(self.line_num, e.to_string(), e)
        })
    }

    /// Get raw bytes length.
    #[must_use]
    pub fn len(&self) -> usize {
        self.bytes.len()
    }

    /// Check if empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.bytes.is_empty()
    }
}

/// Iterator that yields zero-copy lines from a byte slice.
#[derive(Debug)]
pub struct ZeroCopyLines<'a> {
    remaining: &'a [u8],
    line_num: usize,
}

impl<'a> ZeroCopyLines<'a> {
    /// Create from a byte slice.
    #[must_use]
    pub fn new(bytes: &'a [u8]) -> Self {
        Self {
            remaining: bytes,
            line_num: 0,
        }
    }
}

impl<'a> Iterator for ZeroCopyLines<'a> {
    type Item = ZeroCopyLine<'a>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.remaining.is_empty() {
            return None;
        }

        self.line_num += 1;

        let line_end = self.remaining.iter().position(|&b| b == b'\n');

        match line_end {
            Some(pos) => {
                let line = &self.remaining[..pos];
                self.remaining = &self.remaining[pos + 1..];
                Some(ZeroCopyLine::new(line, self.line_num))
            }
            None => {
                let line = self.remaining;
                self.remaining = &[];
                Some(ZeroCopyLine::new(line, self.line_num))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn test_streaming_parser() {
        let content = r#"{"uuid":"1","parentUuid":null,"type":"user","timestamp":"2025-12-23T00:00:00Z","sessionId":"s1","version":"2.0.74","isSidechain":false,"message":{"role":"user","content":"hello"}}
{"uuid":"2","parentUuid":"1","type":"user","timestamp":"2025-12-23T00:00:01Z","sessionId":"s1","version":"2.0.74","isSidechain":false,"message":{"role":"user","content":"world"}}"#;

        let reader = BufReader::new(Cursor::new(content));
        let mut parser = StreamingParser::new(reader);

        let entry1 = parser.next_entry().unwrap().unwrap();
        assert_eq!(entry1.uuid(), Some("1"));

        let entry2 = parser.next_entry().unwrap().unwrap();
        assert_eq!(entry2.uuid(), Some("2"));

        assert!(parser.next_entry().is_none());
        assert_eq!(parser.line_num(), 2);
    }

    #[test]
    fn test_streaming_iterator() {
        let content = r#"{"uuid":"a","parentUuid":null,"type":"user","timestamp":"2025-12-23T00:00:00Z","sessionId":"s1","version":"2.0.74","isSidechain":false,"message":{"role":"user","content":"test"}}
invalid
{"uuid":"b","parentUuid":null,"type":"user","timestamp":"2025-12-23T00:00:01Z","sessionId":"s1","version":"2.0.74","isSidechain":false,"message":{"role":"user","content":"test2"}}"#;

        let reader = BufReader::new(Cursor::new(content));
        let parser = StreamingParser::new(reader).lenient(true);

        let entries: Vec<_> = parser.entries().filter_map(|r| r.ok()).collect();
        assert_eq!(entries.len(), 2);
    }

    #[test]
    fn test_session_state() {
        // Can't easily test this without actual files, but verify the enum works
        assert_eq!(SessionState::Inactive.description(), "inactive");
        assert_eq!(SessionState::PossiblyActive.description(), "possibly active");
    }

    #[test]
    fn test_progress_calculation() {
        let progress = StreamingProgress {
            total_bytes: 1000,
            bytes_processed: 250,
            lines_processed: 10,
            entries_parsed: 8,
            errors: 2,
        };

        assert!((progress.percentage() - 25.0).abs() < 0.001);
        assert_eq!(progress.remaining_bytes(), 750);
    }

    #[test]
    fn test_zero_copy_lines() {
        let content = b"line1\nline2\nline3";
        let lines: Vec<_> = ZeroCopyLines::new(content).collect();

        assert_eq!(lines.len(), 3);
        assert_eq!(lines[0].as_str().unwrap(), "line1");
        assert_eq!(lines[0].line_num, 1);
        assert_eq!(lines[1].as_str().unwrap(), "line2");
        assert_eq!(lines[1].line_num, 2);
        assert_eq!(lines[2].as_str().unwrap(), "line3");
        assert_eq!(lines[2].line_num, 3);
    }

    #[test]
    fn test_zero_copy_lines_empty() {
        let content = b"";
        let lines: Vec<_> = ZeroCopyLines::new(content).collect();
        assert!(lines.is_empty());
    }

    #[test]
    fn test_zero_copy_lines_single_line() {
        let content = b"single";
        let lines: Vec<_> = ZeroCopyLines::new(content).collect();
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].as_str().unwrap(), "single");
    }

    #[test]
    fn test_zero_copy_line_len() {
        let line = ZeroCopyLine::new(b"hello world", 1);
        assert_eq!(line.len(), 11);
        assert!(!line.is_empty());
    }

    #[test]
    fn test_zero_copy_line_parse() {
        let json = br#"{"uuid":"test","parentUuid":null,"type":"user","timestamp":"2025-12-23T00:00:00Z","sessionId":"s1","version":"2.0.74","isSidechain":false,"message":{"role":"user","content":"hi"}}"#;
        let line = ZeroCopyLine::new(json, 1);
        let entry = line.parse().unwrap();
        assert_eq!(entry.uuid(), Some("test"));
    }
}
