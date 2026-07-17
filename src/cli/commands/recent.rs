//! Recent command implementation.
//!
//! A shorthand for `list -n 5` to quickly show recent sessions.

use chrono::{DateTime, Local, Utc};

use crate::cli::{Cli, OutputFormat, RecentArgs};
use crate::discovery::Session;
use crate::error::Result;
use crate::tags::TagStore;
use crate::util::truncate_path;

use super::get_claude_dir;

/// Session info for JSON output.
#[derive(Debug, serde::Serialize)]
struct SessionInfo {
    id: String,
    project: String,
    #[serde(with = "chrono::serde::ts_seconds")]
    modified: DateTime<Utc>,
    size_bytes: u64,
    entry_count: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    bookmarked: Option<bool>,
}

impl SessionInfo {
    fn from_session(session: &Session, tag_store: &TagStore) -> Self {
        let id = session.session_id().to_string();
        let tags = tag_store.get(&id);
        let entry_count = session.quick_metadata_cached().ok().map(|m| m.entry_count);
        Self {
            id: id.clone(),
            project: session.display_project_path(),
            modified: session.modified_datetime(),
            size_bytes: session.file_size(),
            entry_count,
            name: tags.and_then(|t| t.name.clone()),
            bookmarked: tags.map(|t| t.bookmarked),
        }
    }
}

/// Run the recent command.
///
/// By default, resume chains are collapsed into one logical-conversation row
/// keyed by the chain root. `--no-chain` restores the flat per-file view.
pub fn run(cli: &Cli, args: &RecentArgs) -> Result<()> {
    let claude_dir = get_claude_dir(cli.claude_dir.as_ref())?;
    let tag_store = TagStore::load()?;

    // Get all sessions
    let mut sessions = claude_dir.all_sessions()?;

    // Filter by project if specified
    if let Some(project_filter) = &args.project {
        let projects = claude_dir.projects()?;
        let matched = super::helpers::filter_projects(projects, project_filter);
        let matched_paths: Vec<String> = matched
            .iter()
            .map(|p| p.decoded_path().to_string())
            .collect();
        sessions.retain(|s| {
            matched_paths
                .iter()
                .any(|mp| s.project_path().contains(mp.as_str()))
        });
    }

    if !args.no_chain {
        return run_collapsed(cli, args, sessions, &tag_store);
    }

    // Sessions are already sorted by modification time (most recent first)
    // Take the requested count
    sessions.truncate(args.count);

    if sessions.is_empty() {
        if !cli.quiet {
            println!("No recent sessions found.");
        }
        return Ok(());
    }

    match cli.effective_output() {
        OutputFormat::Json => {
            let output: Vec<_> = sessions
                .iter()
                .map(|s| SessionInfo::from_session(s, &tag_store))
                .collect();
            println!("{}", serde_json::to_string_pretty(&output)?);
        }
        OutputFormat::Tsv => {
            println!("id\tproject\tmodified\tsize\tname");
            for session in &sessions {
                let id = session.session_id();
                let tags = tag_store.get(id);
                let name = tags
                    .and_then(|t| t.name.as_ref())
                    .map(|n| n.as_str())
                    .unwrap_or("");
                let project = session.display_project_path();
                let modified = session
                    .modified_datetime()
                    .with_timezone(&Local)
                    .format("%Y-%m-%d %H:%M")
                    .to_string();
                println!(
                    "{}\t{}\t{}\t{}\t{}",
                    &id[..8.min(id.len())],
                    project,
                    modified,
                    session.file_size(),
                    name
                );
            }
        }
        OutputFormat::Compact => {
            for session in &sessions {
                let id = session.session_id();
                let short_id = &id[..8.min(id.len())];
                let project = session.display_project_path();
                let display_project = truncate_path(&project, 40);
                println!("{} {}", short_id, display_project);
            }
        }
        OutputFormat::Text => {
            println!("Recent Sessions");
            println!("{}", "=".repeat(60));
            println!();

            for session in &sessions {
                print_session_line(session, &tag_store)?;
            }

            println!();
            println!("Tip: Use 'snatch info <id>' for details or 'snatch pick' to browse.");
        }
    }

    Ok(())
}

/// Logical-conversation row for JSON output (chains collapsed).
#[derive(Debug, serde::Serialize)]
struct LogicalSessionInfo {
    id: String,
    latest_session_id: String,
    chain_member_count: usize,
    chain_members: Vec<String>,
    project: String,
    #[serde(with = "chrono::serde::ts_seconds")]
    modified: DateTime<Utc>,
    size_bytes: u64,
    entry_count: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    bookmarked: Option<bool>,
}

impl LogicalSessionInfo {
    fn from_row(row: &super::helpers::LogicalSession, tag_store: &TagStore) -> Self {
        let root = row.root();
        let tags = tag_store.get(&row.root_id);
        // Sum entry counts across members for the logical conversation.
        let mut entry_count = None;
        for s in &row.members {
            if let Ok(m) = s.quick_metadata_cached() {
                entry_count = Some(entry_count.unwrap_or(0) + m.entry_count);
            }
        }
        Self {
            id: row.root_id.clone(),
            latest_session_id: row.latest_session_id().to_string(),
            chain_member_count: row.member_count(),
            chain_members: row.member_ids(),
            project: root.display_project_path(),
            modified: DateTime::<Utc>::from(row.latest_modified()),
            size_bytes: row.total_size(),
            entry_count,
            name: tags.and_then(|t| t.name.clone()),
            bookmarked: tags.map(|t| t.bookmarked),
        }
    }
}

/// Run the recent command with resume chains collapsed into logical rows.
fn run_collapsed(
    cli: &Cli,
    args: &RecentArgs,
    sessions: Vec<Session>,
    tag_store: &TagStore,
) -> Result<()> {
    let mut rows = super::helpers::group_into_logical(sessions);
    rows.sort_by_key(|r| std::cmp::Reverse(r.latest_modified()));
    rows.truncate(args.count);

    if rows.is_empty() {
        if !cli.quiet {
            println!("No recent sessions found.");
        }
        return Ok(());
    }

    match cli.effective_output() {
        OutputFormat::Json => {
            let output: Vec<_> = rows
                .iter()
                .map(|r| LogicalSessionInfo::from_row(r, tag_store))
                .collect();
            println!("{}", serde_json::to_string_pretty(&output)?);
        }
        OutputFormat::Tsv => {
            println!("id\tproject\tmodified\tsize\tname\tmember_count\tlatest_session_id");
            for row in &rows {
                let id = &row.root_id;
                let tags = tag_store.get(id);
                let name = tags
                    .and_then(|t| t.name.as_ref())
                    .map(|n| n.as_str())
                    .unwrap_or("");
                let project = row.root().display_project_path();
                let modified = DateTime::<Utc>::from(row.latest_modified())
                    .with_timezone(&Local)
                    .format("%Y-%m-%d %H:%M")
                    .to_string();
                println!(
                    "{}\t{}\t{}\t{}\t{}\t{}\t{}",
                    &id[..8.min(id.len())],
                    project,
                    modified,
                    row.total_size(),
                    name,
                    row.member_count(),
                    &row.latest_session_id()[..8.min(row.latest_session_id().len())],
                );
            }
        }
        OutputFormat::Compact => {
            for row in &rows {
                let id = &row.root_id;
                let short_id = &id[..8.min(id.len())];
                let project = row.root().display_project_path();
                let display_project = truncate_path(&project, 40);
                if row.is_chain() {
                    println!(
                        "{} {} (chain: {}, latest {})",
                        short_id,
                        display_project,
                        row.member_count(),
                        &row.latest_session_id()[..8.min(row.latest_session_id().len())],
                    );
                } else {
                    println!("{} {}", short_id, display_project);
                }
            }
        }
        OutputFormat::Text => {
            println!("Recent Sessions");
            println!("{}", "=".repeat(60));
            println!();

            for row in &rows {
                print_logical_line(row, tag_store)?;
            }

            println!();
            println!("Tip: Use 'snatch info <id>' for details or 'snatch pick' to browse.");
        }
    }

    Ok(())
}

/// Print a formatted logical-conversation line (chains collapsed).
fn print_logical_line(row: &super::helpers::LogicalSession, tag_store: &TagStore) -> Result<()> {
    let root = row.root();
    let id = &row.root_id;
    let short_id = &id[..8.min(id.len())];
    let tags = tag_store.get(id);

    let mut indicators = String::new();
    if let Some(t) = tags {
        if t.bookmarked {
            indicators.push_str("★ ");
        }
    }

    let name_or_project = tags
        .and_then(|t| t.name.clone())
        .unwrap_or_else(|| root.display_project_path());
    let display_name = truncate_path(&name_or_project, 45);

    let time_str = DateTime::<Utc>::from(row.latest_modified())
        .with_timezone(&Local)
        .format("%Y-%m-%d %H:%M")
        .to_string();

    let info = if row.is_chain() {
        format!("chain: {} files", row.member_count())
    } else {
        row.root()
            .quick_metadata_cached()
            .ok()
            .map(|m| format!("{} entries", m.entry_count))
            .unwrap_or_default()
    };

    println!(
        "  {}{} │ {:45} │ {} │ {}",
        indicators, short_id, display_name, time_str, info
    );

    Ok(())
}

/// Print a formatted session line.
fn print_session_line(session: &Session, tag_store: &TagStore) -> Result<()> {
    let id = session.session_id();
    let short_id = &id[..8.min(id.len())];
    let tags = tag_store.get(id);

    // Build status indicators
    let mut indicators = String::new();
    if let Some(t) = tags {
        if t.bookmarked {
            indicators.push_str("★ ");
        }
    }

    // Session name or project
    let name_or_project = tags
        .and_then(|t| t.name.clone())
        .unwrap_or_else(|| session.display_project_path());

    // Truncate if too long (use consistent 45 char limit for text mode)
    let display_name = truncate_path(&name_or_project, 45);

    // Modification time
    let time_str = session
        .modified_datetime()
        .with_timezone(&Local)
        .format("%Y-%m-%d %H:%M")
        .to_string();

    // Entry count (quick metadata)
    let entry_info = session
        .quick_metadata_cached()
        .ok()
        .map(|m| format!("{} entries", m.entry_count))
        .unwrap_or_default();

    println!(
        "  {}{} │ {:45} │ {} │ {}",
        indicators, short_id, display_name, time_str, entry_info
    );

    Ok(())
}
