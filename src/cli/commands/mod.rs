//! CLI command implementations.
//!
//! Each command is implemented in its own module with a `run` function
//! that handles the command logic.

pub mod cache;
pub mod config;
pub mod diff;
pub mod export;
pub mod extract;
pub mod info;
pub mod list;
pub mod search;
pub mod stats;
pub mod tui;
pub mod validate;
pub mod watch;

use std::path::PathBuf;

use crate::discovery::ClaudeDirectory;
use crate::error::Result;

/// Get the Claude directory from CLI args or discover automatically.
pub fn get_claude_dir(custom_path: Option<&PathBuf>) -> Result<ClaudeDirectory> {
    match custom_path {
        Some(path) => ClaudeDirectory::from_path(path),
        None => ClaudeDirectory::discover(),
    }
}
