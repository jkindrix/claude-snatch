//! Project lessons command implementation.
//!
//! Aggregates error→fix pairs and user corrections across all sessions
//! for a project. Deduplicates similar errors, ranks by frequency.

use crate::analysis::project_lessons::{aggregate_project_lessons, ProjectLessonsParams};
use crate::cli::{Cli, OutputFormat, ProjectLessonsArgs};
use crate::error::Result;

use super::helpers::{self, short_id, SessionCollectParams};

/// Run the project-lessons command.
pub fn run(cli: &Cli, args: &ProjectLessonsArgs) -> Result<()> {
    let sessions = helpers::collect_sessions(cli, &SessionCollectParams {
        session: None,
        project: Some(&args.project),
        since: args.since.as_deref(),
        until: args.until.as_deref(),
        recent: None,
        no_subagents: args.no_subagents,
    })?;

    let params = ProjectLessonsParams {
        category: args.category.clone().unwrap_or_else(|| "all".to_string()),
        limit: args.limit,
        min_occurrences: args.min_occurrences,
    };

    let result = aggregate_project_lessons(&sessions, &params, cli.max_file_size);

    match cli.effective_output() {
        OutputFormat::Json => {
            let output = serde_json::json!({
                "project": args.project,
                "sessions_analyzed": result.summary.sessions_analyzed,
                "total_errors": result.summary.total_errors,
                "total_corrections": result.summary.total_corrections,
                "top_failure_modes": result.summary.top_failure_modes,
                "recurring_errors": result.recurring_errors.iter().map(|e| {
                    serde_json::json!({
                        "tool_name": e.tool_name,
                        "error_pattern": e.error_pattern,
                        "count": e.count,
                        "sessions": e.sessions,
                        "last_seen": e.last_seen,
                        "example_resolution": e.example_resolution,
                    })
                }).collect::<Vec<_>>(),
                "recurring_corrections": result.recurring_corrections.iter().map(|c| {
                    serde_json::json!({
                        "pattern": c.pattern,
                        "count": c.count,
                        "sessions": c.sessions,
                        "examples": c.examples,
                    })
                }).collect::<Vec<_>>(),
            });
            println!("{}", serde_json::to_string_pretty(&output)?);
        }
        _ => {
            if result.recurring_errors.is_empty() && result.recurring_corrections.is_empty() {
                println!(
                    "No recurring lessons found across {} sessions.",
                    result.summary.sessions_analyzed
                );
                return Ok(());
            }

            println!(
                "Project lessons across {} sessions ({} errors, {} corrections)\n",
                result.summary.sessions_analyzed,
                result.summary.total_errors,
                result.summary.total_corrections,
            );

            if !result.recurring_errors.is_empty() {
                println!("Recurring Errors:");
                println!("{}", "-".repeat(60));
                for (i, err) in result.recurring_errors.iter().enumerate() {
                    let session_list: Vec<&str> = err.sessions.iter()
                        .take(3)
                        .map(|s| short_id(s))
                        .collect();
                    println!(
                        "  {}. [{}] {} ({}x, sessions: {})",
                        i + 1,
                        err.tool_name,
                        err.error_pattern,
                        err.count,
                        session_list.join(", "),
                    );
                    if let Some(ref resolution) = err.example_resolution {
                        println!("     Fix: {resolution}");
                    }
                    println!();
                }
            }

            if !result.recurring_corrections.is_empty() {
                println!("Recurring Corrections:");
                println!("{}", "-".repeat(60));
                for (i, corr) in result.recurring_corrections.iter().enumerate() {
                    println!("  {}. {} ({}x)", i + 1, corr.pattern, corr.count);
                    println!();
                }
            }

            if !result.summary.top_failure_modes.is_empty() {
                println!("Top Failure Modes:");
                for (tool, count) in &result.summary.top_failure_modes {
                    println!("  {tool}: {count} error(s)");
                }
            }
        }
    }

    Ok(())
}
