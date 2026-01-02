//! Interactive session picker command.
//!
//! Provides a fuzzy-searchable interface for selecting sessions.

use std::fmt::Write as _;

use dialoguer::{theme::ColorfulTheme, FuzzySelect};

use crate::cli::{Cli, PickAction, PickArgs};
use crate::discovery::SessionFilter;
use crate::error::{Result, SnatchError};
use crate::util::truncate_path;

use super::get_claude_dir;

/// Format a session for display in the picker.
fn format_session_item(
    session: &crate::discovery::Session,
    show_project: bool,
) -> String {
    let mut line = String::new();

    // Session ID (short)
    let short_id = &session.session_id()[..8.min(session.session_id().len())];
    write!(line, "{short_id}").unwrap();

    // Modified time
    if let Ok(modified) = session.modified_time().duration_since(std::time::UNIX_EPOCH) {
        let secs = modified.as_secs();
        let dt = chrono::DateTime::from_timestamp(secs as i64, 0);
        if let Some(dt) = dt {
            write!(line, " | {}", dt.format("%Y-%m-%d %H:%M")).unwrap();
        }
    }

    // Project path (shortened)
    if show_project {
        let project = session.project_path();
        let short_project = truncate_path(project, 40);
        write!(line, " | {short_project}").unwrap();
    }

    // Subagent indicator
    if session.is_subagent() {
        write!(line, " [subagent]").unwrap();
    }

    // File size
    let size = session.file_size();
    let size_str = if size >= 1024 * 1024 {
        format!("{:.1}MB", size as f64 / (1024.0 * 1024.0))
    } else if size >= 1024 {
        format!("{:.1}KB", size as f64 / 1024.0)
    } else {
        format!("{size}B")
    };
    write!(line, " ({size_str})").unwrap();

    line
}

/// Run the pick command.
pub fn run(cli: &Cli, args: &PickArgs) -> Result<()> {
    let claude_dir = get_claude_dir(cli.claude_dir.as_ref())?;

    // Build session filter
    let mut filter = SessionFilter::new();

    // By default exclude subagents unless explicitly included
    if !args.subagents {
        filter = filter.main_only();
    }

    // Get all sessions
    let all_sessions = claude_dir.all_sessions()?;

    // Filter and sort sessions
    let mut sessions: Vec<_> = all_sessions
        .iter()
        .filter(|s| {
            // Apply project filter
            if let Some(ref project) = args.project {
                if !s.project_path().contains(project) {
                    return false;
                }
            }

            // Apply session filter
            filter.matches(s).unwrap_or_default()
        })
        .collect();

    if sessions.is_empty() {
        return Err(SnatchError::ConfigError {
            message: "No sessions found matching the specified filters".to_string(),
        });
    }

    // Sort by modification time (newest first)
    sessions.sort_by_key(|s| std::cmp::Reverse(s.modified_time()));

    // Limit the number of sessions in the picker
    let limit = args.limit.unwrap_or(100);
    if sessions.len() > limit {
        sessions.truncate(limit);
    }

    // Show project paths if there are multiple projects
    let unique_projects: std::collections::HashSet<_> =
        sessions.iter().map(|s| s.project_path()).collect();
    let show_project = unique_projects.len() > 1;

    // Create display items
    let items: Vec<String> = sessions
        .iter()
        .map(|s| format_session_item(s, show_project))
        .collect();

    // Show the fuzzy selector
    let selection = FuzzySelect::with_theme(&ColorfulTheme::default())
        .with_prompt("Select a session (type to filter)")
        .items(&items)
        .default(0)
        .interact_opt()
        .map_err(|e| SnatchError::ConfigError {
            message: format!("Failed to show interactive picker: {e}"),
        })?;

    let Some(idx) = selection else {
        // User cancelled
        if !cli.quiet {
            eprintln!("Selection cancelled.");
        }
        return Ok(());
    };

    let selected_session = sessions[idx];
    let session_id = selected_session.session_id();

    // Perform the requested action
    match args.action {
        PickAction::Export => {
            // Just print the session ID for piping
            println!("{session_id}");
            if !cli.quiet {
                eprintln!("Selected session: {session_id}");
                eprintln!("Tip: Use `snatch export {session_id}` to export this session.");
            }
        }
        PickAction::Info => {
            // Run info command
            let info_args = crate::cli::InfoArgs {
                target: Some(session_id.to_string()),
                tree: false,
                raw: false,
                entry: None,
                paths: false,
                messages: None,
                files: false,
            };
            crate::cli::commands::info::run(cli, &info_args)?;
        }
        PickAction::Stats => {
            // Run stats command
            let stats_args = crate::cli::StatsArgs {
                project: None,
                session: Some(session_id.to_string()),
                global: false,
                tools: true,
                models: false,
                costs: false,
                blocks: false,
                token_limit: None,
                all: false,
                sparkline: false,
                history: false,
                days: 30,
                record: false,
                weekly: false,
                monthly: false,
                csv: false,
                clear_history: false,
                timeline: false,
                granularity: "daily".to_string(),
                graph: false,
                graph_width: 60,
            };
            crate::cli::commands::stats::run(cli, &stats_args)?;
        }
        PickAction::Open => {
            // Print session file path
            println!("{}", selected_session.path().display());
            if !cli.quiet {
                eprintln!("Session file: {}", selected_session.path().display());
            }
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    #[test]
    fn test_format_session_item_short() {
        // Basic test - just ensure formatting doesn't panic
        let formatted = "12345678 | 2024-12-24 10:00 | /project (1.0KB)";
        assert!(formatted.contains("12345678"));
    }
}
