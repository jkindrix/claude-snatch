//! Priority suggestion command implementation.
//!
//! Ranks what to work on next based on recurring errors, file churn,
//! open goals, and unresolved decisions.

use crate::analysis::priorities::{suggest_priorities, PriorityParams};
use crate::cli::{Cli, PrioritiesArgs, OutputFormat};
use crate::error::Result;

use super::helpers::{self, SessionCollectParams};

/// Run the priorities command.
pub fn run(cli: &Cli, args: &PrioritiesArgs) -> Result<()> {
    let sessions = helpers::collect_sessions(cli, &SessionCollectParams {
        session: None,
        project: Some(&args.project),
        since: args.since.as_deref(),
        until: args.until.as_deref(),
        recent: None,
        no_subagents: args.no_subagents,
    })?;

    let project = helpers::resolve_single_project(cli, &args.project).ok();
    let decision_store = project
        .as_ref()
        .and_then(|p| crate::decisions::load_decisions(p.path()).ok());
    let goal_store = project
        .as_ref()
        .and_then(|p| crate::goals::load_goals(p.path()).ok());

    let params = PriorityParams {
        max_priorities: args.max_priorities,
        ..Default::default()
    };

    let result = suggest_priorities(
        &sessions,
        decision_store.as_ref(),
        goal_store.as_ref(),
        &params,
        cli.max_file_size,
    );

    match cli.effective_output() {
        OutputFormat::Json => {
            let output = serde_json::json!({
                "project": args.project,
                "sessions_analyzed": result.sessions_analyzed,
                "total_errors": result.total_errors,
                "open_goals": result.open_goals,
                "proposed_decisions": result.proposed_decisions,
                "priorities": result.priorities.iter().map(|p| {
                    serde_json::json!({
                        "rank": p.rank,
                        "category": p.category,
                        "summary": p.summary,
                        "score": p.score,
                        "sources": p.sources.iter().map(|s| s.to_string()).collect::<Vec<_>>(),
                    })
                }).collect::<Vec<_>>(),
            });
            println!("{}", serde_json::to_string_pretty(&output)?);
        }
        _ => {
            println!(
                "Priorities for '{}': {} sessions, {} errors, {} open goals, {} proposed decisions\n",
                args.project, result.sessions_analyzed, result.total_errors,
                result.open_goals, result.proposed_decisions,
            );

            if result.priorities.is_empty() {
                println!("No priority items found.");
                return Ok(());
            }

            for item in &result.priorities {
                let category_tag = match item.category.as_str() {
                    "reliability" => "[RELIABILITY]",
                    "stability" => "[STABILITY]",
                    "goal" => "[GOAL]",
                    "decision" => "[DECISION]",
                    _ => "[OTHER]",
                };
                println!(
                    "  {}. {} {} (score: {:.1})",
                    item.rank, category_tag, item.summary, item.score,
                );
                for source in &item.sources {
                    println!("     evidence: {}", source);
                }
                println!();
            }
        }
    }

    Ok(())
}
