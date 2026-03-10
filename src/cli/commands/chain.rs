//! Chain command implementation.
//!
//! Lists session chains (multi-file logical sessions) for a project.

use std::io::Write;

use crate::cli::{Cli, OutputFormat};
use crate::discovery::chain::detect_chains;
use crate::error::Result;
use crate::util::pager::PagerWriter;

use super::get_claude_dir;

/// Arguments for the chain command.
#[derive(Debug, Clone, clap::Args)]
pub struct ChainArgs {
    /// Filter by project (substring match on path).
    #[arg(short, long)]
    pub project: Option<String>,
}

/// Run the chain command.
pub fn run(cli: &Cli, args: &ChainArgs) -> Result<()> {
    let claude_dir = get_claude_dir(cli.claude_dir.as_ref())?;
    let mut writer = PagerWriter::new(false);

    let projects = claude_dir.projects()?;
    let filtered: Vec<_> = if let Some(ref filter) = args.project {
        projects.into_iter().filter(|p| p.best_path().contains(filter)).collect()
    } else {
        projects
    };

    let mut total_chains = 0;

    for project in &filtered {
        let sessions = project.main_sessions()?;
        if sessions.is_empty() {
            continue;
        }

        let chains = detect_chains(
            sessions.iter().map(|s| (s.session_id(), s.path()))
        );

        if chains.is_empty() {
            continue;
        }

        // Sort chains by start time (newest first)
        let mut sorted_chains: Vec<_> = chains.values().collect();
        sorted_chains.sort_by(|a, b| b.started().cmp(&a.started()));

        match cli.effective_output() {
            OutputFormat::Json => {
                let output: Vec<serde_json::Value> = sorted_chains.iter().map(|c| {
                    serde_json::json!({
                        "root_id": c.root_id,
                        "slug": c.slug,
                        "members": c.file_ids(),
                        "length": c.len(),
                        "started": c.started().map(|t| t.to_rfc3339()),
                        "project": project.best_path(),
                    })
                }).collect();
                writeln!(writer, "{}", serde_json::to_string_pretty(&output)?)?;
            }
            _ => {
                writeln!(writer, "Project: {}", project.best_path())?;
                writeln!(writer, "Chains: {}", sorted_chains.len())?;
                writeln!(writer)?;

                for chain in &sorted_chains {
                    let slug_display = chain.slug.as_deref().unwrap_or("(no slug)");
                    let started = chain.started()
                        .map(|t| t.format("%Y-%m-%d %H:%M UTC").to_string())
                        .unwrap_or_else(|| "unknown".to_string());

                    writeln!(writer, "  {} [{}] ({} files, started {})",
                        &chain.root_id[..8.min(chain.root_id.len())],
                        slug_display,
                        chain.len(),
                        started,
                    )?;

                    for (i, member) in chain.members.iter().enumerate() {
                        let marker = if i == 0 { "root" } else { "cont" };
                        let ts = member.started
                            .map(|t| t.format("%H:%M").to_string())
                            .unwrap_or_else(|| "??:??".to_string());
                        writeln!(writer, "    {}. {} ({}, {})",
                            i + 1,
                            &member.file_id[..8.min(member.file_id.len())],
                            marker,
                            ts,
                        )?;
                    }
                    writeln!(writer)?;
                }
            }
        }

        total_chains += sorted_chains.len();
    }

    if total_chains == 0 {
        writeln!(writer, "No session chains found.")?;
    }

    writer.finish()?;
    Ok(())
}
