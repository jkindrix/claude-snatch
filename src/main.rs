//! claude-snatch: High-performance CLI/TUI for extracting Claude Code conversation logs.
//!
//! This tool provides maximum-fidelity extraction of Claude Code JSONL session logs
//! with support for all 77+ documented data elements, conversation tree reconstruction,
//! and multiple export formats.

use std::process::ExitCode;

use claude_snatch::cli;

fn main() -> ExitCode {
    // Run the CLI (logging is initialized by cli::run based on --log-level and --log-format)
    match cli::run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            // Print error message
            eprintln!("Error: {e}");

            // Print cause chain in debug mode
            if std::env::var("RUST_BACKTRACE").is_ok() {
                if let Some(source) = std::error::Error::source(&e) {
                    eprintln!("Caused by: {source}");
                }
            }

            // Return appropriate exit code
            ExitCode::from(e.exit_code() as u8)
        }
    }
}
