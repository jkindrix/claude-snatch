//! Utility functions for common operations.
//!
//! This module provides shared utilities used across the crate:
//! - Atomic file operations for data safety
//! - Path utilities

use std::io::{self, Write};
use std::path::Path;

use tempfile::NamedTempFile;

use crate::error::{Result, SnatchError};

/// Atomically write content to a file.
///
/// This function ensures data integrity by:
/// 1. Writing to a temporary file in the same directory
/// 2. Syncing the data to disk
/// 3. Atomically renaming the temp file to the target path
///
/// If any step fails, the original file (if it exists) remains unchanged.
///
/// # Arguments
///
/// * `path` - The target file path
/// * `content` - The content to write as bytes
///
/// # Errors
///
/// Returns an error if:
/// - The parent directory cannot be determined or doesn't exist
/// - The temporary file cannot be created
/// - Writing to the temporary file fails
/// - The atomic rename (persist) operation fails
///
/// # Example
///
/// ```rust,no_run
/// use claude_snatch::util::atomic_write;
///
/// atomic_write("config.toml", b"key = \"value\"").unwrap();
/// ```
pub fn atomic_write(path: impl AsRef<Path>, content: &[u8]) -> Result<()> {
    let path = path.as_ref();

    // Get the parent directory for creating the temp file
    let parent = path.parent().ok_or_else(|| SnatchError::IoError {
        context: format!("Cannot determine parent directory for: {}", path.display()),
        source: io::Error::new(io::ErrorKind::InvalidInput, "No parent directory"),
    })?;

    // Ensure parent directory exists
    if !parent.exists() {
        std::fs::create_dir_all(parent).map_err(|e| {
            SnatchError::io(
                format!("Failed to create directory: {}", parent.display()),
                e,
            )
        })?;
    }

    // Create temp file in the same directory (ensures same filesystem for atomic rename)
    let mut temp_file = NamedTempFile::new_in(parent).map_err(|e| {
        SnatchError::io(
            format!("Failed to create temporary file in: {}", parent.display()),
            e,
        )
    })?;

    // Write content to temp file
    temp_file.write_all(content).map_err(|e| {
        SnatchError::io(
            format!("Failed to write to temporary file for: {}", path.display()),
            e,
        )
    })?;

    // Sync to disk before rename
    temp_file.flush().map_err(|e| {
        SnatchError::io(
            format!("Failed to flush temporary file for: {}", path.display()),
            e,
        )
    })?;

    // Atomically rename temp file to target
    temp_file.persist(path).map_err(|e| {
        SnatchError::io(
            format!("Failed to atomically write file: {}", path.display()),
            e.error,
        )
    })?;

    Ok(())
}

/// Atomically write content to a file using a writer function.
///
/// This is useful when you need to write using a `Write` trait object
/// rather than providing bytes directly.
///
/// # Arguments
///
/// * `path` - The target file path
/// * `write_fn` - A function that writes content to the provided writer
///
/// # Errors
///
/// Returns an error if any file operation fails.
///
/// # Example
///
/// ```rust,no_run
/// use claude_snatch::util::atomic_write_with;
/// use std::io::Write;
///
/// atomic_write_with("output.txt", |writer| {
///     writeln!(writer, "Hello, world!")
/// }).unwrap();
/// ```
pub fn atomic_write_with<F>(path: impl AsRef<Path>, write_fn: F) -> Result<()>
where
    F: FnOnce(&mut dyn Write) -> io::Result<()>,
{
    let path = path.as_ref();

    // Get the parent directory
    let parent = path.parent().ok_or_else(|| SnatchError::IoError {
        context: format!("Cannot determine parent directory for: {}", path.display()),
        source: io::Error::new(io::ErrorKind::InvalidInput, "No parent directory"),
    })?;

    // Ensure parent directory exists
    if !parent.exists() {
        std::fs::create_dir_all(parent).map_err(|e| {
            SnatchError::io(
                format!("Failed to create directory: {}", parent.display()),
                e,
            )
        })?;
    }

    // Create temp file in the same directory
    let mut temp_file = NamedTempFile::new_in(parent).map_err(|e| {
        SnatchError::io(
            format!("Failed to create temporary file in: {}", parent.display()),
            e,
        )
    })?;

    // Let the caller write content
    write_fn(&mut temp_file).map_err(|e| {
        SnatchError::io(
            format!("Failed to write content for: {}", path.display()),
            e,
        )
    })?;

    // Sync to disk
    temp_file.flush().map_err(|e| {
        SnatchError::io(
            format!("Failed to flush temporary file for: {}", path.display()),
            e,
        )
    })?;

    // Atomically rename
    temp_file.persist(path).map_err(|e| {
        SnatchError::io(
            format!("Failed to atomically write file: {}", path.display()),
            e.error,
        )
    })?;

    Ok(())
}

/// Create an atomic file writer that will atomically replace the target file on drop.
///
/// This struct wraps a `NamedTempFile` and provides a `finish()` method to
/// complete the atomic write. If `finish()` is not called, the temporary file
/// is discarded without modifying the target.
///
/// # Example
///
/// ```rust,no_run
/// use claude_snatch::util::AtomicFile;
/// use std::io::Write;
///
/// let mut atomic = AtomicFile::create("output.txt").unwrap();
/// writeln!(atomic.writer(), "Hello, world!").unwrap();
/// atomic.finish().unwrap();
/// ```
pub struct AtomicFile {
    temp_file: NamedTempFile,
    target_path: std::path::PathBuf,
}

impl AtomicFile {
    /// Create a new atomic file writer for the given target path.
    pub fn create(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();

        // Get the parent directory
        let parent = path.parent().ok_or_else(|| SnatchError::IoError {
            context: format!("Cannot determine parent directory for: {}", path.display()),
            source: io::Error::new(io::ErrorKind::InvalidInput, "No parent directory"),
        })?;

        // Ensure parent directory exists
        if !parent.exists() {
            std::fs::create_dir_all(parent).map_err(|e| {
                SnatchError::io(
                    format!("Failed to create directory: {}", parent.display()),
                    e,
                )
            })?;
        }

        // Create temp file in the same directory
        let temp_file = NamedTempFile::new_in(parent).map_err(|e| {
            SnatchError::io(
                format!("Failed to create temporary file in: {}", parent.display()),
                e,
            )
        })?;

        Ok(Self {
            temp_file,
            target_path: path.to_path_buf(),
        })
    }

    /// Get a mutable reference to the underlying writer.
    pub fn writer(&mut self) -> &mut NamedTempFile {
        &mut self.temp_file
    }

    /// Finish the atomic write by syncing and renaming the temp file.
    ///
    /// This consumes the `AtomicFile`. If this method is not called,
    /// the temporary file is discarded without affecting the target.
    pub fn finish(mut self) -> Result<()> {
        // Sync to disk
        self.temp_file.flush().map_err(|e| {
            SnatchError::io(
                format!("Failed to flush file: {}", self.target_path.display()),
                e,
            )
        })?;

        // Atomically rename
        self.temp_file.persist(&self.target_path).map_err(|e| {
            SnatchError::io(
                format!("Failed to atomically write: {}", self.target_path.display()),
                e.error,
            )
        })?;

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;
    use tempfile::tempdir;

    #[test]
    fn test_atomic_write() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("test.txt");

        atomic_write(&path, b"Hello, world!").unwrap();

        let mut content = String::new();
        std::fs::File::open(&path)
            .unwrap()
            .read_to_string(&mut content)
            .unwrap();
        assert_eq!(content, "Hello, world!");
    }

    #[test]
    fn test_atomic_write_creates_parent_dirs() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("nested").join("dir").join("test.txt");

        atomic_write(&path, b"Nested content").unwrap();

        assert!(path.exists());
    }

    #[test]
    fn test_atomic_write_with_closure() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("closure.txt");

        atomic_write_with(&path, |w| {
            writeln!(w, "Line 1")?;
            writeln!(w, "Line 2")
        })
        .unwrap();

        let content = std::fs::read_to_string(&path).unwrap();
        assert_eq!(content, "Line 1\nLine 2\n");
    }

    #[test]
    fn test_atomic_file() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("atomic.txt");

        let mut atomic = AtomicFile::create(&path).unwrap();
        writeln!(atomic.writer(), "Atomic write").unwrap();
        atomic.finish().unwrap();

        let content = std::fs::read_to_string(&path).unwrap();
        assert_eq!(content, "Atomic write\n");
    }

    #[test]
    fn test_atomic_file_abort() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("aborted.txt");

        // Write initial content
        std::fs::write(&path, "Original content").unwrap();

        // Start atomic write but don't finish
        {
            let mut atomic = AtomicFile::create(&path).unwrap();
            writeln!(atomic.writer(), "New content").unwrap();
            // Drop without calling finish()
        }

        // Original content should remain
        let content = std::fs::read_to_string(&path).unwrap();
        assert_eq!(content, "Original content");
    }
}
