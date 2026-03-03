//! Shared analytical operations for session data.
//!
//! This module contains core analysis logic that is consumed by both
//! the CLI and MCP server. Functions operate on parsed [`LogEntry`] slices
//! and [`Conversation`] structures — no transport-specific types.
//!
//! # Modules
//!
//! - [`extraction`]: Text and metadata extraction from log entries
//! - [`filters`]: Time period parsing and session filtering
//! - [`lessons`]: Error→fix pairs and user correction detection
//! - [`search`]: Multi-scope regex search across conversation entries
//! - [`timeline`]: Turn-by-turn narrative building with tool-only collapse

pub mod extraction;
pub mod filters;
pub mod lessons;
pub mod search;
pub mod timeline;
