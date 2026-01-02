//! List command implementation.
//!
//! Lists projects and sessions with various filtering and sorting options.

use std::io::Write;
use std::time::SystemTime;

use crate::cli::{Cli, ListArgs, ListTarget, OutputFormat, SortOrder};
use crate::discovery::{Project, Session, SessionFilter};
use crate::error::Result;
use crate::model::LogEntry;
use crate::parser::JsonlParser;
use crate::tags::TagStore;
use crate::util::pager::PagerWriter;

use super::{get_claude_dir, parse_date_filter, parse_size};

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

    // Apply limit (0 means unlimited)
    let total_count = projects.len();
    let truncated = args.limit > 0 && total_count > args.limit;
    if args.limit > 0 {
        projects.truncate(args.limit);
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

            if truncated {
                writeln!(writer, "Projects (showing {} of {}, use -n 0 for all):", projects.len(), total_count)?;
            } else {
                writeln!(writer, "Projects ({} found):", projects.len())?;
            }
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
    // Auto-enable sizes when sorting by size (makes UX intuitive)
    let show_sizes = args.sizes || matches!(args.sort, SortOrder::Size);

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

    // Apply date filters
    let since_time: Option<SystemTime> = if let Some(ref since) = args.since {
        Some(parse_date_filter(since)?)
    } else {
        None
    };

    let until_time: Option<SystemTime> = if let Some(ref until) = args.until {
        Some(parse_date_filter(until)?)
    } else {
        None
    };

    if since_time.is_some() || until_time.is_some() {
        sessions.retain(|s| {
            let modified = s.modified_time();
            if let Some(since) = since_time {
                if modified < since {
                    return false;
                }
            }
            if let Some(until) = until_time {
                if modified > until {
                    return false;
                }
            }
            true
        });
    }

    // Apply size filters
    let min_size: Option<u64> = if let Some(ref size) = args.min_size {
        Some(parse_size(size)?)
    } else {
        None
    };

    let max_size: Option<u64> = if let Some(ref size) = args.max_size {
        Some(parse_size(size)?)
    } else {
        None
    };

    if min_size.is_some() || max_size.is_some() {
        sessions.retain(|s| {
            let size = s.file_size();
            if let Some(min) = min_size {
                if size < min {
                    return false;
                }
            }
            if let Some(max) = max_size {
                if size > max {
                    return false;
                }
            }
            true
        });
    }

    // Load TagStore for metadata-based filtering
    let tag_store = TagStore::load().unwrap_or_default();

    // Apply tag filters
    let tag_filters: Vec<&str> = {
        let mut tags = Vec::new();
        if let Some(ref tag) = args.tag {
            tags.push(tag.as_str());
        }
        if let Some(ref tag_list) = args.tags {
            tags.extend(tag_list.split(',').map(str::trim));
        }
        tags
    };

    if !tag_filters.is_empty() {
        sessions.retain(|s| {
            if let Some(meta) = tag_store.get(s.session_id()) {
                tag_filters.iter().any(|t| meta.tags.iter().any(|mt| mt.contains(t)))
            } else {
                false
            }
        });
    }

    // Apply bookmark filter
    if args.bookmarked {
        sessions.retain(|s| {
            tag_store
                .get(s.session_id())
                .map(|m| m.bookmarked)
                .unwrap_or(false)
        });
    }

    // Apply outcome filter
    if let Some(ref outcome_filter) = args.outcome {
        let outcome_lower = outcome_filter.to_lowercase();
        sessions.retain(|s| {
            if let Some(meta) = tag_store.get(s.session_id()) {
                if let Some(ref outcome) = meta.outcome {
                    let outcome_str = format!("{:?}", outcome).to_lowercase();
                    outcome_str.contains(&outcome_lower)
                } else {
                    false
                }
            } else {
                false
            }
        });
    }

    // Apply name filter
    if let Some(ref name_filter) = args.by_name {
        let name_lower = name_filter.to_lowercase();
        sessions.retain(|s| {
            if let Some(meta) = tag_store.get(s.session_id()) {
                if let Some(ref name) = meta.name {
                    name.to_lowercase().contains(&name_lower)
                } else {
                    false
                }
            } else {
                false
            }
        });
    }

    // Sort sessions
    match args.sort {
        SortOrder::Modified => {
            sessions.sort_by_key(|s| std::cmp::Reverse(s.modified_time()));
        }
        SortOrder::Oldest => {
            sessions.sort_by_key(|s| s.modified_time());
        }
        SortOrder::Size => {
            sessions.sort_by_key(|s| std::cmp::Reverse(s.file_size()));
        }
        SortOrder::Name => {
            sessions.sort_by_key(|s| s.session_id().to_string());
        }
    }

    // Apply limit (0 means unlimited)
    let total_count = sessions.len();
    let truncated = args.limit > 0 && total_count > args.limit;
    if args.limit > 0 {
        sessions.truncate(args.limit);
    }

    // Output
    match cli.effective_output() {
        OutputFormat::Json => {
            let output: Vec<_> = sessions
                .iter()
                .map(|s| SessionInfo::from_session(s, &tag_store, args.context))
                .collect();
            writeln!(writer, "{}", serde_json::to_string_pretty(&output)?)?;
        }
        OutputFormat::Tsv => {
            if args.context {
                writeln!(writer, "session_id\tproject\tsize\tmodified\tsubagent\tname\tcontext")?;
            } else {
                writeln!(writer, "session_id\tproject\tsize\tmodified\tsubagent\tname")?;
            }
            for session in &sessions {
                let id = if args.full_ids {
                    session.session_id().to_string()
                } else {
                    short_id(session.session_id())
                };
                let meta = tag_store.get(session.session_id());
                let name = meta.and_then(|m| m.name.as_deref()).unwrap_or("");
                if args.context {
                    let context = get_session_context(session, 100).unwrap_or_default();
                    // Escape tabs and newlines in context for TSV
                    let context_escaped = context.replace('\t', " ").replace('\n', " ");
                    writeln!(
                        writer,
                        "{}\t{}\t{}\t{}\t{}\t{}\t{}",
                        id,
                        session.project_path(),
                        session.file_size(),
                        session.modified_datetime().format("%Y-%m-%d %H:%M:%S UTC"),
                        session.is_subagent(),
                        name,
                        context_escaped
                    )?;
                } else {
                    writeln!(
                        writer,
                        "{}\t{}\t{}\t{}\t{}\t{}",
                        id,
                        session.project_path(),
                        session.file_size(),
                        session.modified_datetime().format("%Y-%m-%d %H:%M:%S UTC"),
                        session.is_subagent(),
                        name
                    )?;
                }
            }
        }
        OutputFormat::Compact => {
            for session in &sessions {
                let id = if args.full_ids {
                    session.session_id().to_string()
                } else {
                    short_id(session.session_id())
                };
                if args.context {
                    if let Some(context) = get_session_context(session, 60) {
                        writeln!(writer, "{}: {}", id, context)?;
                    } else {
                        writeln!(writer, "{}", id)?;
                    }
                } else {
                    writeln!(writer, "{}", id)?;
                }
            }
        }
        OutputFormat::Text => {
            if sessions.is_empty() {
                writeln!(writer, "No sessions found.")?;
                return Ok(());
            }

            if truncated {
                writeln!(writer, "Sessions (showing {} of {}, use -n 0 for all):", sessions.len(), total_count)?;
            } else {
                writeln!(writer, "Sessions ({} found):", sessions.len())?;
            }
            writeln!(writer)?;

            for session in &sessions {
                let id = if args.full_ids {
                    session.session_id().to_string()
                } else {
                    short_id(session.session_id())
                };

                // Get metadata from TagStore
                let meta = tag_store.get(session.session_id());
                let bookmark_marker = if meta.map(|m| m.bookmarked).unwrap_or(false) {
                    "★ "
                } else {
                    ""
                };

                let subagent_marker = if session.is_subagent() { " [subagent]" } else { "" };

                // Build outcome badge
                let outcome_badge = meta
                    .and_then(|m| m.outcome.as_ref())
                    .map(|o| format!(" [{:?}]", o))
                    .unwrap_or_default();

                // Show custom name if available
                if let Some(name) = meta.and_then(|m| m.name.as_ref()) {
                    write!(writer, "  {}\"{}\" ({}){}{}", bookmark_marker, name, id, subagent_marker, outcome_badge)?;
                } else {
                    write!(writer, "  {}{}{}{}", bookmark_marker, id, subagent_marker, outcome_badge)?;
                }

                if show_sizes {
                    let size_str = session.file_size_human();
                    if session.file_size() == 0 {
                        // Add context for empty sessions
                        write!(writer, " ({} - empty, possibly new or interrupted)", size_str)?;
                    } else {
                        write!(writer, " ({})", size_str)?;
                    }
                }

                writeln!(writer)?;
                writeln!(writer, "    Project: {}", session.project_path())?;
                writeln!(writer, "    Modified: {}", session.modified_datetime().format("%Y-%m-%d %H:%M:%S UTC"))?;

                // Show tags if any
                if let Some(m) = meta {
                    if !m.tags.is_empty() {
                        writeln!(writer, "    Tags: {}", m.tags.join(", "))?;
                    }
                }

                if let Ok(state) = session.state() {
                    if state != crate::discovery::SessionState::Inactive {
                        writeln!(writer, "    Status: {}", state.description())?;
                    }
                }

                // Show context (first user prompt) if requested
                if args.context {
                    if let Some(context) = get_session_context(session, 100) {
                        writeln!(writer, "    Context: \"{}\"", context)?;
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

/// Extract the first user prompt from a session (for context display).
fn get_session_context(session: &Session, max_len: usize) -> Option<String> {
    let mut parser = JsonlParser::new().with_lenient(true);
    let entries = parser.parse_file(session.path()).ok()?;

    // Find the first user message with text content
    for entry in entries {
        if let LogEntry::User(user_msg) = entry {
            // Skip tool results - we want actual human input
            if user_msg.message.has_tool_results() {
                continue;
            }

            // Get text content
            if let Some(text) = user_msg.message.as_text() {
                // Clean up the text - remove excessive whitespace
                let cleaned: String = text
                    .lines()
                    .map(|l| l.trim())
                    .filter(|l| !l.is_empty())
                    .take(3) // Take first 3 non-empty lines
                    .collect::<Vec<_>>()
                    .join(" ");

                if cleaned.is_empty() {
                    continue;
                }

                // Truncate to max length
                if cleaned.len() > max_len {
                    return Some(format!("{}...", &cleaned[..max_len]));
                }
                return Some(cleaned);
            }
        }
    }
    None
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
    #[serde(skip_serializing_if = "Option::is_none")]
    name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    context: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    tags: Vec<String>,
    bookmarked: bool,
}

impl SessionInfo {
    fn from_session(session: &Session, tag_store: &TagStore, include_context: bool) -> Self {
        let meta = tag_store.get(session.session_id());
        let context = if include_context {
            get_session_context(session, 100)
        } else {
            None
        };

        Self {
            session_id: session.session_id().to_string(),
            project_path: session.project_path().to_string(),
            is_subagent: session.is_subagent(),
            file_size: session.file_size(),
            modified: session.modified_datetime().to_rfc3339(),
            name: meta.and_then(|m| m.name.clone()),
            context,
            tags: meta.map(|m| m.tags.clone()).unwrap_or_default(),
            bookmarked: meta.map(|m| m.bookmarked).unwrap_or(false),
        }
    }
}
