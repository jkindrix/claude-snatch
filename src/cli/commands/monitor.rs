//! Active-monitoring command (goal #8, design `.tmp/issues/0023`).
//!
//! On-demand, read-only "what should I watch out for?" view: ranked
//! cross-session recurring-error insights. (SessionStart auto-injection was
//! declined — decision #26 — so there is no cooldown/inject surface.)

use crate::analysis::monitor::{insights_from, rank, Insight, MonitorParams};
use crate::cli::{Cli, MonitorArgs, OutputFormat};
use crate::error::Result;

use super::helpers::{self, SessionCollectParams};

/// Run the monitor command.
pub fn run(cli: &Cli, args: &MonitorArgs) -> Result<()> {
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
    let top = rank(all, args.top);

    match cli.effective_output() {
        OutputFormat::Json => print_json(&top)?,
        _ => print_human(&args.project, &top),
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
