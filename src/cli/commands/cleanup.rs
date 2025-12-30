//! Cleanup command implementation.
//!
//! Provides safe session cleanup with dry-run support and confirmation prompts.

use std::io::{self, Write};

use crate::cli::{Cli, CleanupArgs, OutputFormat};
use crate::discovery::{Session, SessionFilter, SessionState};
use crate::error::{Result, SnatchError};

use super::{get_claude_dir, parse_date_filter};

/// Run the cleanup command.
pub fn run(cli: &Cli, args: &CleanupArgs) -> Result<()> {
    // Require at least one filter criterion
    if !args.empty && args.older_than.is_none() {
        return Err(SnatchError::InvalidArgument {
            name: "filter".to_string(),
            reason: "At least one filter is required. Use --empty to delete empty sessions \
                     or --older-than to delete old sessions."
                .to_string(),
        });
    }

    let claude_dir = get_claude_dir(cli.claude_dir.as_ref())?;

    // Get all sessions
    let mut sessions: Vec<Session> = if let Some(project_filter) = &args.project {
        let projects = claude_dir.projects()?;
        let mut matched = Vec::new();
        for project in projects {
            if project.decoded_path().contains(project_filter) {
                matched.extend(project.sessions()?);
            }
        }
        matched
    } else {
        claude_dir.all_sessions()?
    };

    // Apply subagent filter
    if !args.subagents {
        let filter = SessionFilter::new().main_only();
        sessions.retain(|s| filter.matches(s).unwrap_or(false));
    }

    // Apply criteria filters
    let older_than_time = if let Some(ref date_str) = args.older_than {
        Some(parse_date_filter(date_str)?)
    } else {
        None
    };

    let mut to_delete: Vec<(Session, String)> = Vec::new();
    let mut skipped_active = 0;

    for session in sessions {
        let mut reasons = Vec::new();

        // Check empty criterion
        if args.empty && session.file_size() == 0 {
            reasons.push("empty (0 bytes)");
        }

        // Check older-than criterion
        if let Some(cutoff) = older_than_time {
            if session.modified_time() < cutoff {
                reasons.push("older than cutoff");
            }
        }

        // If no criteria matched, skip
        if reasons.is_empty() {
            continue;
        }

        // Safety check: never delete active sessions
        if let Ok(state) = session.state() {
            if state != SessionState::Inactive {
                skipped_active += 1;
                continue;
            }
        }

        to_delete.push((session, reasons.join(", ")));
    }

    // Report results
    if to_delete.is_empty() {
        println!("No sessions match the cleanup criteria.");
        if skipped_active > 0 {
            println!(
                "({} active session{} skipped for safety)",
                skipped_active,
                if skipped_active == 1 { "" } else { "s" }
            );
        }
        return Ok(());
    }

    // Calculate total size to be freed
    let total_size: u64 = to_delete.iter().map(|(s, _)| s.file_size()).sum();

    // Output based on format
    match cli.effective_output() {
        OutputFormat::Json => {
            let output: Vec<_> = to_delete
                .iter()
                .map(|(s, reason)| {
                    serde_json::json!({
                        "session_id": s.session_id(),
                        "project": s.project_path(),
                        "file_size": s.file_size(),
                        "reason": reason,
                        "path": s.path().to_string_lossy(),
                    })
                })
                .collect();
            println!("{}", serde_json::to_string_pretty(&output)?);
        }
        OutputFormat::Tsv => {
            println!("session_id\tproject\tsize\treason");
            for (session, reason) in &to_delete {
                println!(
                    "{}\t{}\t{}\t{}",
                    session.session_id(),
                    session.project_path(),
                    session.file_size(),
                    reason
                );
            }
        }
        OutputFormat::Compact => {
            for (session, _) in &to_delete {
                println!("{}", session.session_id());
            }
        }
        OutputFormat::Text => {
            if args.preview {
                println!(
                    "Would delete {} session{} ({} total):",
                    to_delete.len(),
                    if to_delete.len() == 1 { "" } else { "s" },
                    crate::discovery::format_size(total_size)
                );
            } else {
                println!(
                    "Found {} session{} to delete ({} total):",
                    to_delete.len(),
                    if to_delete.len() == 1 { "" } else { "s" },
                    crate::discovery::format_size(total_size)
                );
            }
            println!();

            for (session, reason) in &to_delete {
                let id = short_id(session.session_id());
                let subagent_marker = if session.is_subagent() {
                    " [subagent]"
                } else {
                    ""
                };

                if args.verbose {
                    println!("  {}{}", id, subagent_marker);
                    println!("    Project: {}", session.project_path());
                    println!("    Size: {}", session.file_size_human());
                    println!("    Reason: {}", reason);
                    println!("    Path: {}", session.path().display());
                } else {
                    println!(
                        "  {}{} ({}) - {}",
                        id,
                        subagent_marker,
                        session.file_size_human(),
                        reason
                    );
                }
            }

            if skipped_active > 0 {
                println!();
                println!(
                    "Note: {} active session{} skipped for safety.",
                    skipped_active,
                    if skipped_active == 1 { "" } else { "s" }
                );
            }
        }
    }

    // In preview mode, stop here
    if args.preview {
        return Ok(());
    }

    // Confirmation prompt
    if !args.yes {
        println!();
        print!(
            "Delete {} session{}? This cannot be undone. [y/N] ",
            to_delete.len(),
            if to_delete.len() == 1 { "" } else { "s" }
        );
        io::stdout().flush()?;

        let mut input = String::new();
        io::stdin().read_line(&mut input)?;

        let answer = input.trim().to_lowercase();
        if answer != "y" && answer != "yes" {
            println!("Cleanup cancelled.");
            return Ok(());
        }
    }

    // Perform deletion
    let mut deleted = 0;
    let mut failed = 0;
    let mut freed_bytes: u64 = 0;

    for (session, _) in &to_delete {
        match std::fs::remove_file(session.path()) {
            Ok(()) => {
                deleted += 1;
                freed_bytes += session.file_size();
                if args.verbose {
                    println!("Deleted: {}", session.session_id());
                }
            }
            Err(e) => {
                failed += 1;
                eprintln!(
                    "Failed to delete {}: {}",
                    session.session_id(),
                    e
                );
            }
        }
    }

    // Summary
    println!();
    println!(
        "Deleted {} session{} ({})",
        deleted,
        if deleted == 1 { "" } else { "s" },
        crate::discovery::format_size(freed_bytes)
    );

    if failed > 0 {
        eprintln!(
            "Failed to delete {} session{}",
            failed,
            if failed == 1 { "" } else { "s" }
        );
    }

    Ok(())
}

/// Get short ID (first 8 chars).
fn short_id(id: &str) -> String {
    if id.len() > 8 {
        id[..8].to_string()
    } else {
        id.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_short_id() {
        assert_eq!(short_id("40afc8a7-3fcb-4d29-b1ee-100b81b8c6c0"), "40afc8a7");
        assert_eq!(short_id("short"), "short");
        assert_eq!(short_id("12345678"), "12345678");
    }
}
