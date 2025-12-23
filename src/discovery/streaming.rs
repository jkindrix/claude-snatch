//! Streaming utilities for session file handling.
//!
//! This module re-exports streaming functionality from the parser module
//! and provides discovery-specific streaming utilities.

pub use crate::parser::{
    detect_session_state, has_incomplete_line, open_stream, ActiveSessionParser,
    ProgressCallback, ProgressStreamingParser, SessionState, StreamingIterator,
    StreamingParser, StreamingProgress,
};

use std::path::Path;

use crate::error::Result;

/// Open a session file for streaming with automatic state detection.
pub fn open_session_stream(
    path: impl AsRef<Path>,
) -> Result<(StreamingParser<std::io::BufReader<std::fs::File>>, SessionState)> {
    let path = path.as_ref();
    let state = detect_session_state(path)?;
    let parser = open_stream(path)?;
    Ok((parser, state))
}
