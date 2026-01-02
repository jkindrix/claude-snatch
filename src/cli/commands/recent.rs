//! Recent command implementation.
//!
//! A shorthand for `list -n 5` to quickly show recent sessions.

use chrono::{DateTime, Local, Utc};

use crate::cli::{Cli, OutputFormat, RecentArgs};
use crate::discovery::Session;
use crate::error::Result;
use crate::tags::TagStore;

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
            project: session.project_path().to_string(),
            modified: session.modified_datetime(),
            size_bytes: session.file_size(),
            entry_count,
            name: tags.and_then(|t| t.name.clone()),
            bookmarked: tags.map(|t| t.bookmarked),
        }
    }
}

/// Run the recent command.
pub fn run(cli: &Cli, args: &RecentArgs) -> Result<()> {
    let claude_dir = get_claude_dir(cli.claude_dir.as_ref())?;
    let tag_store = TagStore::load()?;

    // Get all sessions
    let mut sessions = claude_dir.all_sessions()?;

    // Filter by project if specified
    if let Some(project_filter) = &args.project {
        sessions.retain(|s| s.project_path().contains(project_filter));
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
                let name = tags.and_then(|t| t.name.as_ref()).map(|n| n.as_str()).unwrap_or("");
                let project = session.project_path();
                let modified = session.modified_datetime().with_timezone(&Local).format("%Y-%m-%d %H:%M").to_string();
                println!("{}\t{}\t{}\t{}\t{}", &id[..8.min(id.len())], project, modified, session.file_size(), name);
            }
        }
        OutputFormat::Compact => {
            for session in &sessions {
                let id = session.session_id();
                let short_id = &id[..8.min(id.len())];
                let project = session.project_path();
                // Truncate project path
                let display_project = if project.len() > 30 {
                    format!("...{}", &project[project.len() - 27..])
                } else {
                    project.to_string()
                };
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
            println!("Tip: Use 'snatch info <id>' for details or 'snatch tui' to browse.");
        }
    }

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
        .unwrap_or_else(|| session.project_path().to_string());

    // Truncate if too long
    let display_name = if name_or_project.len() > 45 {
        format!("...{}", &name_or_project[name_or_project.len() - 42..])
    } else {
        name_or_project
    };

    // Modification time
    let time_str = session.modified_datetime()
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
