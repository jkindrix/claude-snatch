//! Project health command implementation.
//!
//! Shows hotspot files, rework files, decision stability, and per-session
//! error/tool counts.

use crate::analysis::project_health::{analyze_project_health, ProjectHealthParams};
use crate::cli::{Cli, HealthArgs, OutputFormat};
use crate::error::Result;

use super::helpers::{self, short_id, SessionCollectParams};

/// Run the health command.
pub fn run(cli: &Cli, args: &HealthArgs) -> Result<()> {
    let sessions = helpers::collect_sessions(
        cli,
        &SessionCollectParams {
            session: None,
            project: Some(&args.project),
            since: args.since.as_deref(),
            until: args.until.as_deref(),
            recent: None,
            no_subagents: args.no_subagents,
        },
    )?;

    // Try to load decision store
    let project = helpers::resolve_single_project(cli, &args.project).ok();
    let decision_store = project
        .as_ref()
        .and_then(|p| crate::decisions::load_decisions(p.path()).ok());

    let params = ProjectHealthParams {
        max_hotspots: args.max_hotspots,
    };

    let result = analyze_project_health(
        &sessions,
        decision_store.as_ref(),
        &params,
        cli.max_file_size,
    );

    match cli.effective_output() {
        OutputFormat::Json => {
            let output = serde_json::json!({
                "project": args.project,
                "sessions_analyzed": result.sessions_analyzed,
                "total_errors": result.total_errors,
                "total_tool_calls": result.total_tool_calls,
                "hotspot_files": result.hotspot_files.iter().map(|f| {
                    serde_json::json!({
                        "path": f.path,
                        "edit_count": f.edit_count,
                        "session_count": f.session_count,
                    })
                }).collect::<Vec<_>>(),
                "rework_files": result.rework_files.iter().map(|f| {
                    serde_json::json!({
                        "path": f.path,
                        "version_count": f.version_count,
                        "session_count": f.session_count,
                    })
                }).collect::<Vec<_>>(),
                "decision_churn": result.decision_churn.as_ref().map(|dc| {
                    serde_json::json!({
                        "total": dc.total_decisions,
                        "confirmed": dc.confirmed_count,
                        "superseded": dc.superseded_count,
                        "abandoned": dc.abandoned_count,
                        "proposed": dc.proposed_count,
                    })
                }),
                "session_stats": result.session_stats.iter().map(|s| {
                    serde_json::json!({
                        "session_id": s.session_id,
                        "timestamp": s.timestamp,
                        "error_count": s.error_count,
                        "tool_count": s.tool_count,
                    })
                }).collect::<Vec<_>>(),
            });
            println!("{}", serde_json::to_string_pretty(&output)?);
        }
        _ => {
            println!(
                "Project health: {} sessions, {} errors, {} tool calls\n",
                result.sessions_analyzed, result.total_errors, result.total_tool_calls,
            );

            if !result.hotspot_files.is_empty() {
                println!("Hotspot Files (most edits):");
                println!("{}", "-".repeat(60));
                for f in &result.hotspot_files {
                    println!(
                        "  {} — {} edits across {} sessions",
                        f.path, f.edit_count, f.session_count,
                    );
                }
                println!();
            }

            if !result.rework_files.is_empty() {
                println!("Rework Files (edited across multiple sessions):");
                println!("{}", "-".repeat(60));
                for f in &result.rework_files {
                    println!(
                        "  {} — {} versions across {} sessions",
                        f.path, f.version_count, f.session_count,
                    );
                }
                println!();
            }

            if let Some(ref dc) = result.decision_churn {
                println!("Decision Stability:");
                println!("{}", "-".repeat(60));
                println!(
                    "  {} total | {} confirmed | {} superseded | {} abandoned | {} proposed",
                    dc.total_decisions,
                    dc.confirmed_count,
                    dc.superseded_count,
                    dc.abandoned_count,
                    dc.proposed_count,
                );
                println!();
            }

            if !result.session_stats.is_empty() {
                println!("Per-Session Stats:");
                println!("{}", "-".repeat(60));
                for s in &result.session_stats {
                    let ts = s.timestamp.as_deref().unwrap_or("?");
                    println!(
                        "  [{}] {} — {} errors, {} tools",
                        short_id(&s.session_id),
                        ts,
                        s.error_count,
                        s.tool_count,
                    );
                }
            }
        }
    }

    Ok(())
}
