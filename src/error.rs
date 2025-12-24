//! Error types for claude-snatch.
//!
//! This module provides comprehensive error handling following the thiserror pattern.
//! Error types are designed to be informative, actionable, and suitable for both
//! programmatic handling and user-facing display.

use std::path::PathBuf;

use thiserror::Error;

/// Primary error type for claude-snatch operations.
#[derive(Error, Debug)]
pub enum SnatchError {
    /// JSONL parsing failed.
    #[error("Failed to parse JSONL at line {line}: {message}")]
    ParseError {
        /// Line number where parsing failed.
        line: usize,
        /// Human-readable error message.
        message: String,
        /// Underlying serde_json error, if available.
        #[source]
        source: Option<serde_json::Error>,
    },

    /// File not found.
    #[error("File not found: {path}")]
    FileNotFound {
        /// Path to the missing file.
        path: PathBuf,
    },

    /// Directory not found.
    #[error("Directory not found: {path}")]
    DirectoryNotFound {
        /// Path to the missing directory.
        path: PathBuf,
    },

    /// Permission denied when accessing a file or directory.
    #[error("Permission denied: {path}")]
    PermissionDenied {
        /// Path where access was denied.
        path: PathBuf,
    },

    /// Session not found.
    #[error("Session not found: {session_id}")]
    SessionNotFound {
        /// Session ID that was not found.
        session_id: String,
    },

    /// Project not found.
    #[error("Project not found: {project_path}")]
    ProjectNotFound {
        /// Project path that was not found.
        project_path: String,
    },

    /// Invalid session file format.
    #[error("Invalid session file format: {path}: {reason}")]
    InvalidSessionFile {
        /// Path to the invalid session file.
        path: PathBuf,
        /// Reason why the file is invalid.
        reason: String,
    },

    /// Schema version mismatch.
    #[error("Schema version {found} is not supported (expected {expected})")]
    SchemaVersionMismatch {
        /// Schema version found in the file.
        found: String,
        /// Expected schema version.
        expected: String,
    },

    /// Unknown schema version.
    #[error("Unknown schema version: {version}")]
    UnknownSchemaVersion {
        /// The unknown version string.
        version: String,
    },

    /// Export error.
    #[error("Export failed: {message}")]
    ExportError {
        /// Human-readable error message.
        message: String,
        /// Underlying error, if available.
        #[source]
        source: Option<Box<dyn std::error::Error + Send + Sync>>,
    },

    /// Search error.
    #[error("Search failed: {message}")]
    SearchError {
        /// Human-readable error message.
        message: String,
    },

    /// Configuration error.
    #[error("Configuration error: {message}")]
    ConfigError {
        /// Human-readable error message.
        message: String,
    },

    /// I/O error.
    #[error("I/O error: {context}")]
    IoError {
        /// Context describing the operation that failed.
        context: String,
        /// Underlying I/O error.
        #[source]
        source: std::io::Error,
    },

    /// Serialization error.
    #[error("Serialization error: {context}")]
    SerializationError {
        /// Context describing the operation that failed.
        context: String,
        /// Underlying serde_json error.
        #[source]
        source: serde_json::Error,
    },

    /// Invalid path encoding.
    #[error("Invalid path encoding: {path}")]
    InvalidPathEncoding {
        /// The path with invalid encoding.
        path: String,
    },

    /// TUI error.
    #[error("TUI error: {message}")]
    TuiError {
        /// Human-readable error message.
        message: String,
    },

    /// Interrupted operation.
    #[error("Operation interrupted")]
    Interrupted,

    /// Claude directory not found.
    #[error("Claude Code data directory not found. Expected at: {expected_path}")]
    ClaudeDirectoryNotFound {
        /// Expected path to Claude Code data directory.
        expected_path: PathBuf,
    },

    /// Corrupted file (partial write detected).
    #[error("Corrupted file detected (incomplete write): {path}")]
    CorruptedFile {
        /// Path to the corrupted file.
        path: PathBuf,
    },

    /// Unsupported message type.
    #[error("Unsupported message type: {message_type}")]
    UnsupportedMessageType {
        /// The unsupported message type.
        message_type: String,
    },

    /// Invalid UUID format.
    #[error("Invalid UUID format: {value}")]
    InvalidUuid {
        /// The invalid UUID string.
        value: String,
    },

    /// Tree reconstruction error.
    #[error("Failed to reconstruct conversation tree: {message}")]
    TreeReconstructionError {
        /// Human-readable error message.
        message: String,
    },

    /// Analytics calculation error.
    #[error("Analytics calculation failed: {message}")]
    AnalyticsError {
        /// Human-readable error message.
        message: String,
    },

    /// Timeout error.
    #[error("Operation timed out after {duration_ms}ms")]
    Timeout {
        /// Duration in milliseconds before timeout.
        duration_ms: u64,
    },

    /// Data integrity error.
    #[error("Data integrity error: {message}")]
    DataIntegrityError {
        /// Human-readable error message.
        message: String,
    },

    /// Unsupported operation or feature.
    #[error("Unsupported: {feature}")]
    Unsupported {
        /// Name of the unsupported feature.
        feature: String,
    },

    /// Invalid argument.
    #[error("Invalid argument '{name}': {reason}")]
    InvalidArgument {
        /// Name of the invalid argument.
        name: String,
        /// Reason why the argument is invalid.
        reason: String,
    },

    /// Invalid configuration.
    #[error("Invalid configuration: {message}")]
    InvalidConfig {
        /// Human-readable error message.
        message: String,
    },

    /// Search index error.
    #[error("Index error: {0}")]
    IndexError(
        /// Human-readable error message.
        String,
    ),
}

impl SnatchError {
    /// Create a new parse error.
    #[must_use]
    pub fn parse(line: usize, message: impl Into<String>) -> Self {
        Self::ParseError {
            line,
            message: message.into(),
            source: None,
        }
    }

    /// Create a new parse error with source.
    #[must_use]
    pub fn parse_with_source(line: usize, message: impl Into<String>, source: serde_json::Error) -> Self {
        Self::ParseError {
            line,
            message: message.into(),
            source: Some(source),
        }
    }

    /// Create a new I/O error with context.
    #[must_use]
    pub fn io(context: impl Into<String>, source: std::io::Error) -> Self {
        Self::IoError {
            context: context.into(),
            source,
        }
    }

    /// Create a new export error.
    #[must_use]
    pub fn export(message: impl Into<String>) -> Self {
        Self::ExportError {
            message: message.into(),
            source: None,
        }
    }

    /// Create a new unsupported error.
    #[must_use]
    pub fn unsupported(feature: impl Into<String>) -> Self {
        Self::Unsupported {
            feature: feature.into(),
        }
    }

    /// Get the exit code for this error (CLI-011 requirement).
    #[must_use]
    pub const fn exit_code(&self) -> i32 {
        match self {
            Self::ParseError { .. } => 2,
            Self::FileNotFound { .. } | Self::SessionNotFound { .. } | Self::ProjectNotFound { .. } => 3,
            Self::PermissionDenied { .. } => 4,
            Self::ConfigError { .. } => 5,
            Self::ExportError { .. } => 6,
            Self::SearchError { .. } => 7,
            Self::Interrupted => 130,
            Self::IoError { .. } => 74,
            _ => 1,
        }
    }

    /// Check if this error is recoverable.
    #[must_use]
    pub const fn is_recoverable(&self) -> bool {
        matches!(
            self,
            Self::ParseError { .. }
                | Self::InvalidSessionFile { .. }
                | Self::CorruptedFile { .. }
                | Self::Timeout { .. }
        )
    }
}

/// Result type alias for claude-snatch operations.
pub type Result<T> = std::result::Result<T, SnatchError>;

impl From<std::io::Error> for SnatchError {
    fn from(err: std::io::Error) -> Self {
        Self::IoError {
            context: "I/O operation failed".to_string(),
            source: err,
        }
    }
}

impl From<serde_json::Error> for SnatchError {
    fn from(err: serde_json::Error) -> Self {
        Self::SerializationError {
            context: "JSON operation failed".to_string(),
            source: err,
        }
    }
}

impl From<std::string::FromUtf8Error> for SnatchError {
    fn from(err: std::string::FromUtf8Error) -> Self {
        Self::DataIntegrityError {
            message: format!("Invalid UTF-8: {err}"),
        }
    }
}

/// Exit codes for CLI operations (CLI-011 requirement).
pub mod exit_codes {
    /// Operation completed successfully.
    pub const EXIT_SUCCESS: i32 = 0;
    /// General/unspecified error.
    pub const EXIT_GENERAL_ERROR: i32 = 1;
    /// JSONL parsing failed.
    pub const EXIT_PARSE_ERROR: i32 = 2;
    /// Specified file or session not found.
    pub const EXIT_FILE_NOT_FOUND: i32 = 3;
    /// Insufficient permissions.
    pub const EXIT_PERMISSION_DENIED: i32 = 4;
    /// Invalid configuration.
    pub const EXIT_CONFIG_ERROR: i32 = 5;
    /// Export operation failed.
    pub const EXIT_EXPORT_ERROR: i32 = 6;
    /// Search operation failed.
    pub const EXIT_SEARCH_ERROR: i32 = 7;
    /// Invalid command-line usage (BSD standard).
    pub const EXIT_USAGE_ERROR: i32 = 64;
    /// Input data format error (BSD standard).
    pub const EXIT_DATA_ERROR: i32 = 65;
    /// I/O error (BSD standard).
    pub const EXIT_IO_ERROR: i32 = 74;
    /// Terminated by Ctrl+C (128 + SIGINT).
    pub const EXIT_INTERRUPTED: i32 = 130;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_exit_codes() {
        let parse_err = SnatchError::parse(1, "test");
        assert_eq!(parse_err.exit_code(), 2);

        let not_found = SnatchError::FileNotFound {
            path: PathBuf::from("/test"),
        };
        assert_eq!(not_found.exit_code(), 3);

        let interrupted = SnatchError::Interrupted;
        assert_eq!(interrupted.exit_code(), 130);
    }

    #[test]
    fn test_is_recoverable() {
        let parse_err = SnatchError::parse(1, "test");
        assert!(parse_err.is_recoverable());

        let not_found = SnatchError::FileNotFound {
            path: PathBuf::from("/test"),
        };
        assert!(!not_found.is_recoverable());
    }
}
