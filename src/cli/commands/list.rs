//! List command implementation.
//!
//! Lists projects and sessions with various filtering and sorting options.

use std::io::Write;

use crate::cli::{Cli, ListArgs, ListTarget, OutputFormat, SortOrder};
use crate::discovery::{Project, Session, SessionFilter};
use crate::error::Result;
use crate::util::pager::PagerWriter;

use super::get_claude_dir;

/// Run the list command.
pub fn run(cli: &Cli, args: &ListArgs) -> Result<()> {
    let claude_dir = get_claude_dir(cli.claude_dir.as_ref())?;

    // Create a writer that optionally uses a pager
    let mut writer = PagerWriter::new(args.pager);

    match args.target {
        ListTarget::Projects => list_projects(cli, args, &claude_dir, &mut writer)?,
        ListTarget::Sessions => list_sessions(cli, args, &claude_dir, &mut writer)?,
        ListTarget::All => {
            list_projects(cli, args, &claude_dir, &mut writer)?;
            writeln!(writer)?;
            list_sessions(cli, args, &claude_dir, &mut writer)?;
        }
    }

    // Flush through pager if needed
    writer.finish()?;
    Ok(())
}

/// List projects.
fn list_projects<W: Write>(
    cli: &Cli,
    args: &ListArgs,
    claude_dir: &crate::discovery::ClaudeDirectory,
    writer: &mut W,
) -> Result<()> {
    let mut projects = claude_dir.projects()?;

    // Filter by project path if specified
    if let Some(filter) = &args.project {
        projects.retain(|p| p.decoded_path().contains(filter));
    }

    // Sort projects
    match args.sort {
        SortOrder::Modified => {
            // Already sorted by default
        }
        SortOrder::Oldest => {
            projects.reverse();
        }
        SortOrder::Name => {
            projects.sort_by(|a, b| a.decoded_path().cmp(b.decoded_path()));
        }
        SortOrder::Size => {
            // Sort by total session size
            projects.sort_by(|a, b| {
                let size_a: u64 = a.sessions().map(|s| s.iter().map(|ss| ss.file_size()).sum()).unwrap_or(0);
                let size_b: u64 = b.sessions().map(|s| s.iter().map(|ss| ss.file_size()).sum()).unwrap_or(0);
                size_b.cmp(&size_a)
            });
        }
    }

    // Apply limit
    if let Some(limit) = args.limit {
        projects.truncate(limit);
    }

    // Output
    match cli.effective_output() {
        OutputFormat::Json => {
            let output: Vec<_> = projects.iter().map(|p| ProjectInfo::from(p)).collect();
            writeln!(writer, "{}", serde_json::to_string_pretty(&output)?)?;
        }
        OutputFormat::Tsv => {
            writeln!(writer, "path\tencoded\tsession_count")?;
            for project in &projects {
                let session_count = project.sessions().map(|s| s.len()).unwrap_or(0);
                writeln!(writer, "{}\t{}\t{}", project.decoded_path(), project.encoded_name(), session_count)?;
            }
        }
        OutputFormat::Compact => {
            for project in &projects {
                writeln!(writer, "{}", project.decoded_path())?;
            }
        }
        OutputFormat::Text => {
            if projects.is_empty() {
                writeln!(writer, "No projects found.")?;
                return Ok(());
            }

            writeln!(writer, "Projects ({} found):", projects.len())?;
            writeln!(writer)?;

            for project in &projects {
                let session_count = project.sessions().map(|s| s.len()).unwrap_or(0);
                writeln!(writer, "  {} ({} sessions)", project.decoded_path(), session_count)?;

                if args.sizes {
                    let total_size: u64 = project
                        .sessions()
                        .map(|s| s.iter().map(|ss| ss.file_size()).sum())
                        .unwrap_or(0);
                    writeln!(writer, "    Size: {}", crate::discovery::format_size(total_size))?;
                }
            }
        }
    }

    Ok(())
}

/// List sessions.
fn list_sessions<W: Write>(
    cli: &Cli,
    args: &ListArgs,
    claude_dir: &crate::discovery::ClaudeDirectory,
    writer: &mut W,
) -> Result<()> {
    let mut sessions: Vec<Session> = if let Some(project_filter) = &args.project {
        // Get sessions from matching projects
        let projects = claude_dir.projects()?;
        let mut matched_sessions = Vec::new();

        for project in projects {
            if project.decoded_path().contains(project_filter) {
                matched_sessions.extend(project.sessions()?);
            }
        }

        matched_sessions
    } else {
        claude_dir.all_sessions()?
    };

    // Apply filter
    let filter = SessionFilter::new();
    let filter = if args.subagents {
        filter
    } else {
        filter.main_only()
    };
    let filter = if args.active {
        filter.active_only()
    } else {
        filter
    };

    sessions.retain(|s| filter.matches(s).unwrap_or(false));

    // Sort sessions
    match args.sort {
        SortOrder::Modified => {
            sessions.sort_by(|a, b| b.modified_time().cmp(&a.modified_time()));
        }
        SortOrder::Oldest => {
            sessions.sort_by(|a, b| a.modified_time().cmp(&b.modified_time()));
        }
        SortOrder::Size => {
            sessions.sort_by(|a, b| b.file_size().cmp(&a.file_size()));
        }
        SortOrder::Name => {
            sessions.sort_by(|a, b| a.session_id().cmp(b.session_id()));
        }
    }

    // Apply limit
    if let Some(limit) = args.limit {
        sessions.truncate(limit);
    }

    // Output
    match cli.effective_output() {
        OutputFormat::Json => {
            let output: Vec<_> = sessions.iter().map(|s| SessionInfo::from(s)).collect();
            writeln!(writer, "{}", serde_json::to_string_pretty(&output)?)?;
        }
        OutputFormat::Tsv => {
            writeln!(writer, "session_id\tproject\tsize\tmodified\tsubagent")?;
            for session in &sessions {
                let id = if args.full_ids {
                    session.session_id().to_string()
                } else {
                    short_id(session.session_id())
                };
                writeln!(
                    writer,
                    "{}\t{}\t{}\t{}\t{}",
                    id,
                    session.project_path(),
                    session.file_size(),
                    session.modified_datetime().format("%Y-%m-%d %H:%M"),
                    session.is_subagent()
                )?;
            }
        }
        OutputFormat::Compact => {
            for session in &sessions {
                if args.full_ids {
                    writeln!(writer, "{}", session.session_id())?;
                } else {
                    writeln!(writer, "{}", short_id(session.session_id()))?;
                }
            }
        }
        OutputFormat::Text => {
            if sessions.is_empty() {
                writeln!(writer, "No sessions found.")?;
                return Ok(());
            }

            writeln!(writer, "Sessions ({} found):", sessions.len())?;
            writeln!(writer)?;

            for session in &sessions {
                let id = if args.full_ids {
                    session.session_id().to_string()
                } else {
                    short_id(session.session_id())
                };

                let subagent_marker = if session.is_subagent() { " [subagent]" } else { "" };

                write!(writer, "  {}{}", id, subagent_marker)?;

                if args.sizes {
                    write!(writer, " ({})", session.file_size_human())?;
                }

                writeln!(writer)?;
                writeln!(writer, "    Project: {}", session.project_path())?;
                writeln!(writer, "    Modified: {}", session.modified_datetime().format("%Y-%m-%d %H:%M:%S"))?;

                if let Ok(state) = session.state() {
                    if state != crate::discovery::SessionState::Inactive {
                        writeln!(writer, "    Status: {}", state.description())?;
                    }
                }
            }
        }
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

/// Project info for JSON output.
#[derive(Debug, serde::Serialize)]
struct ProjectInfo {
    path: String,
    encoded_name: String,
    session_count: usize,
    total_size: u64,
}

impl From<&Project> for ProjectInfo {
    fn from(project: &Project) -> Self {
        let sessions = project.sessions().unwrap_or_default();
        Self {
            path: project.decoded_path().to_string(),
            encoded_name: project.encoded_name().to_string(),
            session_count: sessions.len(),
            total_size: sessions.iter().map(|s| s.file_size()).sum(),
        }
    }
}

/// Session info for JSON output.
#[derive(Debug, serde::Serialize)]
struct SessionInfo {
    session_id: String,
    project_path: String,
    is_subagent: bool,
    file_size: u64,
    modified: String,
}

impl From<&Session> for SessionInfo {
    fn from(session: &Session) -> Self {
        Self {
            session_id: session.session_id().to_string(),
            project_path: session.project_path().to_string(),
            is_subagent: session.is_subagent(),
            file_size: session.file_size(),
            modified: session.modified_datetime().to_rfc3339(),
        }
    }
}
