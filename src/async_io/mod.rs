//! Async I/O operations for non-blocking file handling.
//!
//! This module provides async versions of common I/O operations using tokio.
//! It enables non-blocking file reading, writing, and directory operations
//! for improved TUI responsiveness and parallel processing.

use std::path::{Path, PathBuf};

use tokio::fs;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

use crate::error::{Result, SnatchError};
use crate::model::LogEntry;
use crate::parser::JsonlParser;
use crate::reconstruction::Conversation;

/// Async file reader for JSONL files.
pub struct AsyncJsonlReader {
    path: PathBuf,
}

impl AsyncJsonlReader {
    /// Create a new async JSONL reader.
    pub fn new(path: impl AsRef<Path>) -> Self {
        Self {
            path: path.as_ref().to_path_buf(),
        }
    }

    /// Read and parse the JSONL file asynchronously.
    pub async fn parse(&self) -> Result<Vec<LogEntry>> {
        let contents = fs::read_to_string(&self.path).await.map_err(|e| {
            SnatchError::io(format!("Failed to read file: {}", self.path.display()), e)
        })?;

        // Parse synchronously (CPU-bound operation)
        let mut parser = JsonlParser::new();
        parser.parse_str(&contents)
    }

    /// Read and parse into a conversation.
    pub async fn parse_conversation(&self) -> Result<Conversation> {
        let entries = self.parse().await?;
        Conversation::from_entries(entries)
    }

    /// Stream lines from the file asynchronously.
    pub async fn stream_lines(&self) -> Result<impl futures::Stream<Item = Result<String>>> {
        let file = fs::File::open(&self.path).await.map_err(|e| {
            SnatchError::io(format!("Failed to open file: {}", self.path.display()), e)
        })?;

        let reader = BufReader::new(file);
        let lines = reader.lines();

        Ok(futures::stream::unfold(lines, |mut lines| async move {
            match lines.next_line().await {
                Ok(Some(line)) => Some((Ok(line), lines)),
                Ok(None) => None,
                Err(e) => Some((Err(SnatchError::io("Failed to read line", e)), lines)),
            }
        }))
    }
}

/// Async file writer with atomic semantics.
pub struct AsyncWriter {
    path: PathBuf,
    temp_path: PathBuf,
}

impl AsyncWriter {
    /// Create a new async writer.
    pub fn new(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref().to_path_buf();
        let temp_path = path.with_extension("tmp");
        Ok(Self { path, temp_path })
    }

    /// Write content to the file atomically.
    pub async fn write(&self, content: &str) -> Result<()> {
        // Ensure parent directory exists
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent).await.map_err(|e| {
                SnatchError::io(format!("Failed to create directory: {}", parent.display()), e)
            })?;
        }

        // Write to temp file
        let mut file = fs::File::create(&self.temp_path).await.map_err(|e| {
            SnatchError::io(format!("Failed to create temp file: {}", self.temp_path.display()), e)
        })?;

        file.write_all(content.as_bytes()).await.map_err(|e| {
            SnatchError::io("Failed to write content", e)
        })?;

        file.flush().await.map_err(|e| {
            SnatchError::io("Failed to flush file", e)
        })?;

        // Atomic rename
        fs::rename(&self.temp_path, &self.path).await.map_err(|e| {
            SnatchError::io(format!("Failed to rename temp file to: {}", self.path.display()), e)
        })?;

        Ok(())
    }

    /// Write bytes to the file atomically.
    pub async fn write_bytes(&self, content: &[u8]) -> Result<()> {
        // Ensure parent directory exists
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent).await.map_err(|e| {
                SnatchError::io(format!("Failed to create directory: {}", parent.display()), e)
            })?;
        }

        // Write to temp file
        let mut file = fs::File::create(&self.temp_path).await.map_err(|e| {
            SnatchError::io(format!("Failed to create temp file: {}", self.temp_path.display()), e)
        })?;

        file.write_all(content).await.map_err(|e| {
            SnatchError::io("Failed to write content", e)
        })?;

        file.flush().await.map_err(|e| {
            SnatchError::io("Failed to flush file", e)
        })?;

        // Atomic rename
        fs::rename(&self.temp_path, &self.path).await.map_err(|e| {
            SnatchError::io(format!("Failed to rename temp file to: {}", self.path.display()), e)
        })?;

        Ok(())
    }
}

/// Read a file asynchronously.
pub async fn read_file(path: impl AsRef<Path>) -> Result<String> {
    let path = path.as_ref();
    fs::read_to_string(path).await.map_err(|e| {
        SnatchError::io(format!("Failed to read file: {}", path.display()), e)
    })
}

/// Read a file as bytes asynchronously.
pub async fn read_file_bytes(path: impl AsRef<Path>) -> Result<Vec<u8>> {
    let path = path.as_ref();
    fs::read(path).await.map_err(|e| {
        SnatchError::io(format!("Failed to read file: {}", path.display()), e)
    })
}

/// Write a file asynchronously with atomic semantics.
pub async fn write_file(path: impl AsRef<Path>, content: &str) -> Result<()> {
    let writer = AsyncWriter::new(path)?;
    writer.write(content).await
}

/// Write bytes to a file asynchronously with atomic semantics.
pub async fn write_file_bytes(path: impl AsRef<Path>, content: &[u8]) -> Result<()> {
    let writer = AsyncWriter::new(path)?;
    writer.write_bytes(content).await
}

/// Check if a file exists asynchronously.
pub async fn file_exists(path: impl AsRef<Path>) -> bool {
    fs::metadata(path.as_ref()).await.is_ok()
}

/// Get file metadata asynchronously.
pub async fn file_metadata(path: impl AsRef<Path>) -> Result<std::fs::Metadata> {
    let path = path.as_ref();
    fs::metadata(path).await.map_err(|e| {
        SnatchError::io(format!("Failed to get metadata: {}", path.display()), e)
    })
}

/// Create a directory and all parent directories asynchronously.
pub async fn create_dir_all(path: impl AsRef<Path>) -> Result<()> {
    let path = path.as_ref();
    fs::create_dir_all(path).await.map_err(|e| {
        SnatchError::io(format!("Failed to create directory: {}", path.display()), e)
    })
}

/// Remove a file asynchronously.
pub async fn remove_file(path: impl AsRef<Path>) -> Result<()> {
    let path = path.as_ref();
    fs::remove_file(path).await.map_err(|e| {
        SnatchError::io(format!("Failed to remove file: {}", path.display()), e)
    })
}

/// Read a directory asynchronously.
pub async fn read_dir(path: impl AsRef<Path>) -> Result<Vec<PathBuf>> {
    let path = path.as_ref();
    let mut entries = fs::read_dir(path).await.map_err(|e| {
        SnatchError::io(format!("Failed to read directory: {}", path.display()), e)
    })?;

    let mut paths = Vec::new();
    while let Some(entry) = entries.next_entry().await.map_err(|e| {
        SnatchError::io(format!("Failed to read directory entry: {}", path.display()), e)
    })? {
        paths.push(entry.path());
    }

    Ok(paths)
}

/// Parse multiple JSONL files in parallel.
pub async fn parse_files_parallel(paths: Vec<PathBuf>) -> Vec<Result<Vec<LogEntry>>> {
    use futures::future::join_all;

    let futures = paths.into_iter().map(|path| async move {
        let reader = AsyncJsonlReader::new(path);
        reader.parse().await
    });

    join_all(futures).await
}

/// Export a conversation asynchronously.
pub async fn export_conversation_async<W: std::io::Write + Send + 'static>(
    conversation: Conversation,
    mut writer: W,
    format: crate::export::ExportFormat,
    options: crate::export::ExportOptions,
) -> Result<()> {
    use crate::export::{
        CsvExporter, ExportFormat, Exporter, HtmlExporter, JsonExporter,
        MarkdownExporter, TextExporter, XmlExporter,
    };

    // Run the export in a blocking task
    tokio::task::spawn_blocking(move || {
        match format {
            ExportFormat::Markdown => {
                let exporter = MarkdownExporter::new();
                exporter.export_conversation(&conversation, &mut writer, &options)
            }
            ExportFormat::Json | ExportFormat::JsonPretty => {
                let exporter = JsonExporter::new()
                    .pretty(matches!(format, ExportFormat::JsonPretty));
                exporter.export_conversation(&conversation, &mut writer, &options)
            }
            ExportFormat::Html => {
                let exporter = HtmlExporter::new();
                exporter.export_conversation(&conversation, &mut writer, &options)
            }
            ExportFormat::Text => {
                let exporter = TextExporter::new();
                exporter.export_conversation(&conversation, &mut writer, &options)
            }
            ExportFormat::Csv => {
                let exporter = CsvExporter::new();
                exporter.export_conversation(&conversation, &mut writer, &options)
            }
            ExportFormat::Xml => {
                let exporter = XmlExporter::new();
                exporter.export_conversation(&conversation, &mut writer, &options)
            }
            ExportFormat::Sqlite => {
                Err(SnatchError::unsupported("SQLite async export - use synchronous export"))
            }
        }
    })
    .await
    .map_err(|e| SnatchError::io("Export task failed", std::io::Error::new(std::io::ErrorKind::Other, e)))?
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[tokio::test]
    async fn test_async_write_read() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("test.txt");

        write_file(&path, "Hello, async!").await.unwrap();
        let content = read_file(&path).await.unwrap();

        assert_eq!(content, "Hello, async!");
    }

    #[tokio::test]
    async fn test_async_write_bytes() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("test.bin");

        write_file_bytes(&path, b"binary data").await.unwrap();
        let content = read_file_bytes(&path).await.unwrap();

        assert_eq!(content, b"binary data");
    }

    #[tokio::test]
    async fn test_file_exists() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("test.txt");

        assert!(!file_exists(&path).await);

        write_file(&path, "test").await.unwrap();

        assert!(file_exists(&path).await);
    }

    #[tokio::test]
    async fn test_create_dir_all() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("a/b/c");

        create_dir_all(&path).await.unwrap();

        assert!(path.exists());
    }

    #[tokio::test]
    async fn test_read_dir() {
        let dir = tempdir().unwrap();

        write_file(dir.path().join("file1.txt"), "1").await.unwrap();
        write_file(dir.path().join("file2.txt"), "2").await.unwrap();

        let paths = read_dir(dir.path()).await.unwrap();

        assert_eq!(paths.len(), 2);
    }
}
