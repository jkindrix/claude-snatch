//! Active-monitoring command (goal #8, design `.tmp/issues/0023`).
//!
//! Surfaces ranked cross-session insights (recurring errors). Two modes:
//! - default: a full read-only "what's going on" view (cooldown ignored);
//! - `--inject`: the proactive surface for the SessionStart hook — applies the
//!   cooldown, prints a compact block, and is **silent when nothing passes**.
//!   `--mark-shown` then records what was surfaced so it won't nag next time.

use std::path::PathBuf;

use chrono::Utc;

use crate::analysis::monitor::{insights_from, rank, Insight, MonitorParams};
use crate::analysis::monitor_state::MonitorState;
use crate::cli::{Cli, MonitorArgs, OutputFormat};
use crate::error::Result;

use super::helpers::{self, SessionCollectParams};

/// Resolve the project directory (for the cooldown state).
fn find_project_dir(cli: &Cli, project_filter: &str) -> Result<Option<PathBuf>> {
    match helpers::resolve_single_project(cli, project_filter) {
        Ok(project) => Ok(Some(project.path().to_path_buf())),
        Err(crate::error::SnatchError::ProjectNotFound { .. }) => Ok(None),
        Err(e) => Err(e),
    }
}

/// Run the monitor command.
pub fn run(cli: &Cli, args: &MonitorArgs) -> Result<()> {
    let project_dir = find_project_dir(cli, &args.project)?;

    // The hook (`--inject`) bounds the window via `--since` to stay fast; the
    // full scan is for the explicit, on-demand invocation.
    let sessions = helpers::collect_sessions(
        cli,
        &SessionCollectParams {
            session: None,
            project: Some(&args.project),
            since: args.since.as_deref(),
            until: None,
            recent: None,
            no_subagents: args.no_subagents,
        },
    )?;

    let params = MonitorParams {
        min_occurrences: args.min_occurrences,
    };
    let all = insights_from(&sessions, &params, cli.max_file_size);

    let now = Utc::now();
    let mut state = project_dir
        .as_ref()
        .map(|dir| MonitorState::load(dir))
        .unwrap_or_default();

    // Cooldown applies only on the proactive `--inject` surface.
    let candidates: Vec<Insight> = if args.inject {
        all.into_iter()
            .filter(|i| state.should_surface(i, now, args.cooldown_days))
            .collect()
    } else {
        all
    };
    let top = rank(candidates, args.top);

    match cli.effective_output() {
        OutputFormat::Json => print_json(&top)?,
        _ if args.inject => print_inject(&top), // silent when empty
        _ => print_human(&args.project, &top),
    }

    // Persist only what the proactive surface actually showed.
    if args.inject && args.mark_shown {
        if let Some(dir) = &project_dir {
            for i in &top {
                state.mark_shown(i, now);
            }
            if let Err(e) = state.save(dir) {
                if !cli.quiet {
                    eprintln!("warning: failed to persist monitor state: {e}");
                }
            }
        }
    }

    Ok(())
}

fn print_json(insights: &[Insight]) -> Result<()> {
    let arr: Vec<_> = insights
        .iter()
        .map(|i| {
            serde_json::json!({
                "kind": i.kind.as_str(),
                "title": i.title,
                "evidence": i.evidence,
                "severity": i.severity,
                "fingerprint": i.fingerprint,
            })
        })
        .collect();
    println!("{}", serde_json::to_string_pretty(&arr)?);
    Ok(())
}

/// Compact hook output. Prints nothing when empty — the not-nagging guarantee.
fn print_inject(insights: &[Insight]) {
    if insights.is_empty() {
        return;
    }
    println!("### ⚠ Active monitor (snatch)");
    println!();
    for i in insights {
        println!("- **{}** — {}", i.title, i.evidence);
    }
}

fn print_human(project: &str, insights: &[Insight]) {
    if insights.is_empty() {
        println!("No monitor insights for '{project}'.");
        return;
    }
    println!("Monitor insights for '{}' ({}):", project, insights.len());
    println!("{}", "-".repeat(60));
    for (n, i) in insights.iter().enumerate() {
        println!("  {}. [{}] {}", n + 1, i.severity, i.title);
        println!("     {}", i.evidence);
    }
}
