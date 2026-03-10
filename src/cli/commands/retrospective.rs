//! Project retrospective command implementation.
//!
//! Composite analysis combining health, lessons, and decisions into
//! a single project review.

use crate::analysis::retrospective::{analyze_retrospective, RetrospectiveParams};
use crate::cli::{Cli, RetrospectiveArgs, OutputFormat};
use crate::error::Result;

use super::helpers::{self, short_id, SessionCollectParams};

/// Run the retrospective command.
pub fn run(cli: &Cli, args: &RetrospectiveArgs) -> Result<()> {
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

    let params = RetrospectiveParams {
        max_files: args.max_files,
        max_errors: args.max_errors,
        max_corrections: args.max_corrections,
        min_occurrences: args.min_occurrences,
    };

    let result = analyze_retrospective(
        &sessions,
        decision_store.as_ref(),
        &params,
        cli.max_file_size,
    );

    match cli.effective_output() {
        OutputFormat::Json => {
            let output = serde_json::json!({
                "project": args.project,
                "summary": {
                    "sessions_analyzed": result.summary.sessions_analyzed,
                    "total_errors": result.summary.total_errors,
                    "total_tool_calls": result.summary.total_tool_calls,
                    "total_corrections": result.summary.total_corrections,
                    "error_rate": result.summary.error_rate,
                    "top_failure_modes": result.summary.top_failure_modes.iter().map(|(t, c)| {
                        serde_json::json!({"tool": t, "count": c})
                    }).collect::<Vec<_>>(),
                },
                "hotspot_files": result.hotspot_files.iter().map(|f| {
                    serde_json::json!({"path": f.path, "edit_count": f.edit_count, "session_count": f.session_count})
                }).collect::<Vec<_>>(),
                "rework_files": result.rework_files.iter().map(|f| {
                    serde_json::json!({"path": f.path, "version_count": f.version_count, "session_count": f.session_count})
                }).collect::<Vec<_>>(),
                "recurring_errors": result.recurring_errors.iter().map(|e| {
                    serde_json::json!({
                        "tool": e.tool_name, "pattern": e.error_pattern,
                        "count": e.count, "sessions": e.sessions,
                        "last_seen": e.last_seen, "resolution": e.example_resolution,
                    })
                }).collect::<Vec<_>>(),
                "recurring_corrections": result.recurring_corrections.iter().map(|c| {
                    serde_json::json!({"pattern": c.pattern, "count": c.count, "sessions": c.sessions})
                }).collect::<Vec<_>>(),
                "decisions": result.decisions.iter().map(|d| {
                    serde_json::json!({
                        "id": d.id, "title": d.title, "status": d.status,
                        "confidence": d.confidence, "tags": d.tags,
                    })
                }).collect::<Vec<_>>(),
            });
            println!("{}", serde_json::to_string_pretty(&output)?);
        }
        _ => {
            // Header
            println!(
                "Project Retrospective: {} sessions, {} errors, {} tool calls, {:.1}% error rate\n",
                result.summary.sessions_analyzed,
                result.summary.total_errors,
                result.summary.total_tool_calls,
                result.summary.error_rate * 100.0,
            );

            // Top failure modes
            if !result.summary.top_failure_modes.is_empty() {
                print!("Top failure modes: ");
                let modes: Vec<String> = result.summary.top_failure_modes.iter()
                    .take(5)
                    .map(|(t, c)| format!("{} ({}x)", t, c))
                    .collect();
                println!("{}\n", modes.join(", "));
            }

            // Decisions
            if !result.decisions.is_empty() {
                println!("Decisions:");
                println!("{}", "-".repeat(60));
                for d in &result.decisions {
                    let tags = if d.tags.is_empty() {
                        String::new()
                    } else {
                        format!(" [{}]", d.tags.join(", "))
                    };
                    println!("  #{} {} ({}, {:.0}%){}", d.id, d.title, d.status, d.confidence * 100.0, tags);
                }
                println!();
            }

            // Recurring errors
            if !result.recurring_errors.is_empty() {
                println!("Recurring Errors:");
                println!("{}", "-".repeat(60));
                for (i, e) in result.recurring_errors.iter().enumerate() {
                    let preview: String = e.error_pattern.lines().next().unwrap_or("").chars().take(80).collect();
                    println!("  {}. [{}] {} ({}x)", i + 1, e.tool_name, preview, e.count);
                    if let Some(ref fix) = e.example_resolution {
                        let fix_preview: String = fix.chars().take(80).collect();
                        println!("     Fix: {}", fix_preview);
                    }
                }
                println!();
            }

            // Hotspot files
            if !result.hotspot_files.is_empty() {
                println!("Hotspot Files:");
                println!("{}", "-".repeat(60));
                for f in &result.hotspot_files {
                    println!("  {} — {} edits, {} sessions", f.path, f.edit_count, f.session_count);
                }
                println!();
            }

            // Rework files
            if !result.rework_files.is_empty() {
                println!("Rework Files:");
                println!("{}", "-".repeat(60));
                for f in &result.rework_files {
                    println!("  {} — {} versions, {} sessions", f.path, f.version_count, f.session_count);
                }
                println!();
            }

            // Per-session stats (compact)
            if !result.session_stats.is_empty() {
                println!("Session Activity:");
                println!("{}", "-".repeat(60));
                for s in &result.session_stats {
                    if s.error_count > 0 || s.tool_count > 0 {
                        let ts = s.timestamp.as_deref().unwrap_or("?");
                        println!(
                            "  [{}] {} — {} errors, {} tools",
                            short_id(&s.session_id), ts, s.error_count, s.tool_count,
                        );
                    }
                }
            }
        }
    }

    Ok(())
}
