//! claude-snatch: High-performance CLI/TUI tool for extracting Claude Code conversation logs.
//!
//! This crate provides comprehensive extraction, analysis, and export capabilities
//! for Claude Code JSONL session logs with maximum data fidelity.
//!
//! # Features
//!
//! - **Maximum Fidelity**: Extract all 77+ documented JSONL data elements
//! - **Beyond-JSONL**: Access 21+ supplementary data sources
//! - **Rust Performance**: Native speed, 10-100x faster than alternatives
//! - **Lossless Round-Trip**: Preserve unknown fields for forward compatibility
//! - **Dual Interface**: Both CLI (scriptable) and TUI (interactive) modes
//!
//! # Quick Start (High-Level API)
//!
//! For simple use cases, use the [`api`] module:
//!
//! ```rust,no_run
//! use claude_snatch::api::{SnatchClient, ExportFormat};
//!
//! fn main() -> claude_snatch::Result<()> {
//!     // Create a client that auto-discovers Claude Code data
//!     let client = SnatchClient::discover()?;
//!
//!     // List recent sessions
//!     for session in client.recent_sessions(10)? {
//!         println!("Session: {} ({} messages)", session.id, session.message_count);
//!     }
//!
//!     // Export a session to markdown
//!     let markdown = client.export_session("session_id", ExportFormat::Markdown)?;
//!     println!("{}", markdown);
//!
//!     Ok(())
//! }
//! ```
//!
//! # Architecture
//!
//! The crate is organized into the following modules:
//!
//! - [`api`]: High-level programmatic API for common operations
//! - [`model`]: Core data structures for all message types and content blocks
//! - [`parser`]: JSONL parsing with streaming support and error recovery
//! - [`discovery`]: Session and project discovery across platforms
//! - [`reconstruction`]: Conversation tree building and linking
//! - [`analytics`]: Statistics calculation and usage tracking
//! - [`export`]: Output format generation (Markdown, JSON, etc.)
//! - [`extraction`]: Beyond-JSONL data extraction (settings, CLAUDE.md, MCP, etc.)
//! - [`cli`]: Command-line interface
//! - [`tui`]: Terminal user interface
//! - [`config`]: Configuration management
//! - [`error`]: Error types and handling
//!
//! # Low-Level Example
//!
//! For more control, use the internal modules directly:
//!
//! ```rust,no_run
//! use claude_snatch::{discovery::ClaudeDirectory, parser::JsonlParser};
//!
//! fn main() -> Result<(), Box<dyn std::error::Error>> {
//!     // Discover Claude Code data directory
//!     let claude_dir = ClaudeDirectory::discover()?;
//!
//!     // List all projects
//!     for project in claude_dir.projects()? {
//!         println!("Project: {}", project.path().display());
//!     }
//!
//!     Ok(())
//! }
//! ```

#![doc(html_root_url = "https://docs.rs/claude-snatch/0.1.0")]
// Use deny instead of forbid to allow targeted unsafe in mmap feature.
// When mmap feature is enabled, streaming.rs uses memmap2 which requires unsafe.
#![deny(unsafe_code)]
#![warn(missing_docs)]
#![warn(rustdoc::missing_crate_level_docs)]

pub mod analytics;
pub mod api;
pub mod async_io;
pub mod cache;
pub mod cli;
pub mod config;
pub mod discovery;
pub mod error;
pub mod export;
pub mod extraction;
pub mod git;
pub mod index;
pub mod model;
pub mod parser;
pub mod reconstruction;
pub mod tags;
pub mod tui;
pub mod util;

// Re-export commonly used types at the crate root
pub use error::{Result, SnatchError};
pub use model::{LogEntry, SchemaVersion};

/// Library version.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Library name.
pub const NAME: &str = env!("CARGO_PKG_NAME");

/// Default Claude Code data directory name.
pub const CLAUDE_DIR_NAME: &str = ".claude";

/// Projects subdirectory name.
pub const PROJECTS_DIR_NAME: &str = "projects";

/// File history subdirectory name.
pub const FILE_HISTORY_DIR_NAME: &str = "filehistory";

/// Prelude module for convenient imports.
pub mod prelude {

    pub use crate::api::{SnatchClient, ExportFormat, ExportOptionsBuilder};
    pub use crate::error::{Result, SnatchError};
    pub use crate::model::{
        AssistantMessage, ContentBlock, LogEntry, SchemaVersion, UserMessage,
    };
    pub use crate::parser::JsonlParser;
}
