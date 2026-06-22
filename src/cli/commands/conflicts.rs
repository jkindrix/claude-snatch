//! Contradiction detection implementation.
//!
//! Finds potentially conflicting decisions across sessions by:
//! 1. Registry-based: comparing decisions that share tags/topics
//! 2. Search-based: finding opposing language about the same topic across sessions

use std::io::IsTerminal;

use indicatif::{ProgressBar, ProgressStyle};

use crate::analysis::conflict_detection::{
    detect_registry_conflicts, detect_search_conflicts, ConflictPair,
};
use crate::cli::{Cli, ConflictsArgs};
use crate::decisions;
use crate::error::Result;

use super::helpers::{self, short_id, truncate, SessionCollectParams};

/// Find the project directory for a given project filter.
fn find_project_dir(cli: &Cli, project_filter: &str) -> Result<Option<std::path::PathBuf>> {
    match super::helpers::resolve_single_project(cli, project_filter) {
        Ok(project) => Ok(Some(project.path().to_path_buf())),
        Err(crate::error::SnatchError::ProjectNotFound { .. }) => Ok(None),
        Err(e) => Err(e),
    }
}

/// Run the conflicts command.
pub fn run(cli: &Cli, args: &ConflictsArgs) -> Result<()> {
    let mut conflicts: Vec<ConflictPair> = Vec::new();

    // === Approach 1: Registry-based detection ===
    if let Some(ref project) = args.project {
        if let Some(project_dir) = find_project_dir(cli, project)? {
            let store = decisions::load_decisions(&project_dir)?;
            detect_registry_conflicts(&store, &args.topic, &mut conflicts);
        }
    }

    // === Approach 2: Search-based detection ===
    if let Some(ref topic) = args.topic {
        let sessions = helpers::collect_sessions(
            cli,
            &SessionCollectParams {
                session: None,
                project: args.project.as_deref(),
                since: args.since.as_deref(),
                until: args.until.as_deref(),
                recent: None,
                no_subagents: args.no_subagents,
            },
        )?;

        let session_count = sessions.len();
        let show_progress = session_count > 10 && std::io::stderr().is_terminal() && !cli.quiet;
        if show_progress {
            let pb = ProgressBar::new(session_count as u64);
            pb.set_style(
                ProgressStyle::default_bar()
                    .template("{spinner:.cyan} [{bar:40.cyan/dim}] {pos}/{len} sessions")
                    .unwrap()
                    .progress_chars("█▓░"),
            );
            pb.finish_and_clear();
        }

        let search_conflicts = detect_search_conflicts(
            &sessions,
            topic,
            args.exclude_session.as_deref(),
            cli.max_file_size,
        )?;
        conflicts.extend(search_conflicts);
    }

    conflicts.retain(|c| c.confidence >= args.min_confidence);

    conflicts.sort_by(|a, b| {
        b.confidence
            .partial_cmp(&a.confidence)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.earlier_time.cmp(&b.earlier_time))
    });

    let limit = if args.no_limit {
        conflicts.len()
    } else {
        args.limit.min(conflicts.len())
    };
    conflicts.truncate(limit);

    if conflicts.is_empty() {
        if !cli.quiet {
            if args.topic.is_some() {
                println!("No conflicts detected for the specified topic.");
            } else {
                println!("No conflicts detected in the decision registry.");
                if args.project.is_some() {
                    println!("Tip: use --topic <pattern> to search for opposing language across sessions.");
                }
            }
        }
        return Ok(());
    }

    match cli.effective_output() {
        crate::cli::OutputFormat::Json => output_json(&conflicts),
        _ => output_text(cli, &conflicts),
    }

    Ok(())
}

fn output_json(conflicts: &[ConflictPair]) {
    let entries: Vec<serde_json::Value> = conflicts
        .iter()
        .map(|c| {
            serde_json::json!({
                "topic": c.topic,
                "detection": format!("{}", c.detection),
                "confidence": c.confidence,
                "earlier": {
                    "timestamp": c.earlier_time.to_rfc3339(),
                    "session_id": c.earlier_session,
                    "text": c.earlier_text,
                },
                "later": {
                    "timestamp": c.later_time.to_rfc3339(),
                    "session_id": c.later_session,
                    "text": c.later_text,
                }
            })
        })
        .collect();

    println!(
        "{}",
        serde_json::to_string_pretty(&entries).unwrap_or_default()
    );
}

fn output_text(cli: &Cli, conflicts: &[ConflictPair]) {
    if !cli.quiet {
        println!(
            "Detected {} potential conflict{}:\n",
            conflicts.len(),
            if conflicts.len() == 1 { "" } else { "s" }
        );
    }

    for (i, conflict) in conflicts.iter().enumerate() {
        let conf_pct = (conflict.confidence * 100.0) as u32;
        let earlier_date = conflict.earlier_time.format("%Y-%m-%d");
        let later_date = conflict.later_time.format("%Y-%m-%d");

        println!(
            "  [{:>3}%] {} | topic: {}",
            conf_pct, conflict.detection, conflict.topic
        );
        println!();
        println!(
            "    EARLIER ({} [{}]):",
            earlier_date,
            short_id(&conflict.earlier_session)
        );
        for line in truncate(&conflict.earlier_text, 300).lines() {
            println!("      {}", line);
        }
        println!();
        println!(
            "    LATER ({} [{}]):",
            later_date,
            short_id(&conflict.later_session)
        );
        for line in truncate(&conflict.later_text, 300).lines() {
            println!("      {}", line);
        }

        if i < conflicts.len() - 1 {
            println!();
            println!("  ─────────────────────────────────────────");
            println!();
        }
    }

    println!();
}
