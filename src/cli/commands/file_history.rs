//! File history command implementation.
//!
//! Shows which sessions modified a given file path.

use std::io::Write;

use crate::cli::{Cli, OutputFormat};
use crate::error::Result;
use crate::file_index::FileIndex;
use crate::util::pager::PagerWriter;

use super::get_claude_dir;

/// Arguments for the file-history command.
#[derive(Debug, Clone, clap::Args)]
pub struct FileHistoryArgs {
    /// File path to look up (substring match).
    pub path: String,

    /// Filter by project (substring match on path).
    #[arg(short, long)]
    pub project: Option<String>,

    /// Maximum results to show.
    #[arg(short, long, default_value = "50")]
    pub limit: usize,
}

/// Run the file-history command.
pub fn run(cli: &Cli, args: &FileHistoryArgs) -> Result<()> {
    let claude_dir = get_claude_dir(cli.claude_dir.as_ref())?;
    let mut writer = PagerWriter::new(false);

    // Collect sessions to index
    let projects = claude_dir.projects()?;
    let mut sessions = Vec::new();
    for project in &projects {
        if let Some(ref filter) = args.project {
            if !project.best_path().contains(filter) {
                continue;
            }
        }
        if let Ok(s) = project.sessions() {
            sessions.extend(s);
        }
    }

    let index = FileIndex::from_sessions(&sessions, cli.max_file_size);

    // Search for matching files
    let mut matches = index.search(&args.path);
    matches.sort_by_key(|(path, _)| path.to_string());

    if matches.is_empty() {
        writeln!(writer, "No sessions found that modified files matching '{}'.", args.path)?;
        writer.finish()?;
        return Ok(());
    }

    match cli.effective_output() {
        OutputFormat::Json => {
            let output: Vec<serde_json::Value> = matches
                .iter()
                .flat_map(|(path, mods)| {
                    mods.iter().take(args.limit).map(move |m| {
                        serde_json::json!({
                            "file_path": path,
                            "session_id": m.session_id,
                            "project_path": m.project_path,
                            "message_id": m.message_id,
                            "timestamp": m.timestamp.to_rfc3339(),
                            "version": m.version,
                        })
                    })
                })
                .take(args.limit)
                .collect();
            writeln!(writer, "{}", serde_json::to_string_pretty(&output)?)?;
        }
        _ => {
            let total_files = matches.len();
            let total_mods: usize = matches.iter().map(|(_, m)| m.len()).sum();
            writeln!(writer, "Files matching '{}': {} ({} modifications)", args.path, total_files, total_mods)?;
            writeln!(writer)?;

            let mut shown = 0;
            for (path, mods) in &matches {
                if shown >= args.limit {
                    break;
                }
                writeln!(writer, "  {path}")?;
                for m in mods.iter().take(args.limit - shown) {
                    let ts = m.timestamp.format("%Y-%m-%d %H:%M UTC");
                    writeln!(
                        writer,
                        "    {} v{} ({}) [{}]",
                        &m.session_id[..8.min(m.session_id.len())],
                        m.version,
                        ts,
                        &m.message_id[..8.min(m.message_id.len())],
                    )?;
                    shown += 1;
                }
                writeln!(writer)?;
            }
        }
    }

    writer.finish()?;
    Ok(())
}
