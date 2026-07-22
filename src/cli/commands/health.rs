//! Project health command implementation.
//!
//! Shows hotspot files, rework files, decision stability, and per-session
//! error/tool counts.

use std::collections::BTreeSet;

use crate::analysis::project_health::{
    analyze_project_health, ProjectHealthParams, ProviderProjectHealthAccumulator,
};
use crate::cli::{Cli, HealthArgs, OutputFormat};
use crate::error::{Result, SnatchError};

use super::helpers::{self, short_id, SessionCollectParams};

/// Run the health command.
pub fn run(cli: &Cli, args: &HealthArgs) -> Result<()> {
    if !args.provider.is_empty() {
        return run_provider(cli, args);
    }
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
                "total_tool_failures": result.total_errors,
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
                        "tool_failure_count": s.error_count,
                        "tool_count": s.tool_count,
                    })
                }).collect::<Vec<_>>(),
            });
            println!("{}", serde_json::to_string_pretty(&output)?);
        }
        _ => {
            println!(
                "Project health: {} sessions, {} tool failures, {} tool calls\n",
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
                        "  [{}] {} — {} tool failures, {} tools",
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

fn run_provider(cli: &Cli, args: &HealthArgs) -> Result<()> {
    use crate::provider::project::context_overlaps_time_range;
    use crate::provider::registry::ProviderSelection;

    let selection = ProviderSelection::from_flags(&args.provider).map_err(|reason| {
        SnatchError::InvalidArgument {
            name: "--provider".to_string(),
            reason,
        }
    })?;
    let atomic = matches!(selection, ProviderSelection::Explicit(_));
    let since = args
        .since
        .as_deref()
        .map(super::parse_date_filter)
        .transpose()?;
    let until = args
        .until
        .as_deref()
        .map(super::parse_date_filter)
        .transpose()?;
    let registry = helpers::provider_registry(cli);
    let mut providers: BTreeSet<_> = registry
        .select(&selection)?
        .providers
        .into_iter()
        .map(|provider| provider.id().to_string())
        .collect();
    let mut accumulator = ProviderProjectHealthAccumulator::default();
    let mut date_fallbacks = BTreeSet::new();
    let mut analysis_errors = Vec::new();

    let report = registry.visit_filtered_parsed_project_sessions(
        &selection,
        crate::cache::global_cache(),
        Some(&args.project),
        !args.no_subagents,
        |_, session| {
            let (include, fallback) = context_overlaps_time_range(&session.context, since, until);
            if include && fallback {
                date_fallbacks.insert(session.descriptor.key.clone());
            }
            include
        },
        |project, session, logical_root, parsed| {
            let semantic_annotations = registry
                .get(&session.descriptor.key.provider)
                .expect("visited session came from a registered provider")
                .capabilities()
                .semantic_annotations;
            let mut roots = project.cwd_variants.clone();
            if let Some(path) = &project.display_path {
                roots.push(path.clone());
            }
            if let Err(error) =
                accumulator.add_session(&roots, logical_root, parsed, semantic_annotations)
            {
                analysis_errors.push((session.descriptor.key.clone(), error.to_string()));
            }
        },
    )?;
    if atomic {
        if let Some((key, reason)) = analysis_errors.first() {
            return Err(SnatchError::InvalidArgument {
                name: key.to_string(),
                reason: format!("selected session could not be analyzed: {reason}"),
            });
        }
    }
    for (provider, _) in &report.skipped {
        providers.remove(&provider.to_string());
    }

    let claude_project = helpers::resolve_single_project(cli, &args.project).ok();
    let decision_store = claude_project
        .as_ref()
        .and_then(|project| crate::decisions::load_decisions(project.path()).ok());
    let registry_path = claude_project
        .as_ref()
        .map(|project| project.path().display().to_string());
    let params = ProjectHealthParams {
        max_hotspots: args.max_hotspots,
    };
    let result = accumulator.finish(decision_store.as_ref(), &params);
    let health = &result.health;
    let mut warnings = report.warnings;
    warnings.extend(
        analysis_errors
            .into_iter()
            .map(|(key, _)| format!("{key}: session could not be analyzed")),
    );
    if !date_fallbacks.is_empty() {
        warnings.push(format!(
            "{} session descriptors used conservative source-time evidence for date filtering",
            date_fallbacks.len()
        ));
    }
    warnings.sort();
    warnings.dedup();
    let skipped: Vec<_> = report
        .skipped
        .iter()
        .map(|(provider, reason)| {
            serde_json::json!({"provider": provider.to_string(), "reason": reason})
        })
        .collect();

    match cli.effective_output() {
        OutputFormat::Json => {
            let output = serde_json::json!({
                "project": args.project,
                "providers": providers,
                "sessions_analyzed": health.sessions_analyzed,
                "session_descriptors_analyzed": result.session_descriptors_analyzed,
                "date_filter_fallback_descriptors": date_fallbacks.len(),
                "total_tool_failures": health.total_errors,
                "confirmed_tool_failures": result.confirmed_failures,
                "inferred_failure_signals": result.inferred_failures,
                "total_tool_calls": health.total_tool_calls,
                "hotspot_files": health.hotspot_files.iter().map(|file| serde_json::json!({
                    "path": file.path,
                    "edit_count": file.edit_count,
                    "session_count": file.session_count,
                })).collect::<Vec<_>>(),
                "rework_files": health.rework_files.iter().map(|file| serde_json::json!({
                    "path": file.path,
                    "version_count": file.version_count,
                    "session_count": file.session_count,
                })).collect::<Vec<_>>(),
                "decision_churn": health.decision_churn.as_ref().map(|churn| serde_json::json!({
                    "total": churn.total_decisions,
                    "confirmed": churn.confirmed_count,
                    "superseded": churn.superseded_count,
                    "abandoned": churn.abandoned_count,
                    "proposed": churn.proposed_count,
                })),
                "registry_coverage": {
                    "scope": "claude_project_registry",
                    "available": decision_store.is_some(),
                    "project_path": registry_path,
                },
                "session_stats": health.session_stats.iter().map(|session| serde_json::json!({
                    "session_id": session.session_id,
                    "timestamp": session.timestamp,
                    "tool_failure_count": session.error_count,
                    "tool_count": session.tool_count,
                })).collect::<Vec<_>>(),
                "skipped_providers": skipped,
                "warnings": warnings,
                "coverage_note": "File churn uses source-backed applied patch/snapshot evidence; arbitrary shell writes are not inferred.",
            });
            println!("{}", serde_json::to_string_pretty(&output)?);
        }
        _ => {
            println!(
                "Project health: {} logical sessions ({} descriptors), {} confirmed tool failures, {} inferred signals, {} tool calls\n",
                health.sessions_analyzed,
                result.session_descriptors_analyzed,
                result.confirmed_failures,
                result.inferred_failures,
                health.total_tool_calls,
            );
            println!("File churn is limited to source-backed applied patches/snapshots; shell writes are not inferred.\n");
            if !health.hotspot_files.is_empty() {
                println!("Hotspot Files (most source-backed edits):");
                println!("{}", "-".repeat(60));
                for file in &health.hotspot_files {
                    println!(
                        "  {} — {} edits across {} logical sessions",
                        file.path, file.edit_count, file.session_count
                    );
                }
                println!();
            }
            if !health.rework_files.is_empty() {
                println!("Rework Files (edited across multiple logical sessions):");
                println!("{}", "-".repeat(60));
                for file in &health.rework_files {
                    println!(
                        "  {} — {} versions across {} logical sessions",
                        file.path, file.version_count, file.session_count
                    );
                }
                println!();
            }
            if let Some(churn) = &health.decision_churn {
                println!("Decision Stability (Claude project registry):");
                println!("{}", "-".repeat(60));
                println!(
                    "  {} total | {} confirmed | {} superseded | {} abandoned | {} proposed\n",
                    churn.total_decisions,
                    churn.confirmed_count,
                    churn.superseded_count,
                    churn.abandoned_count,
                    churn.proposed_count,
                );
            } else {
                println!(
                    "Decision stability: unavailable (Claude project registry not resolved)\n"
                );
            }
            if !health.session_stats.is_empty() {
                println!("Per-Logical-Session Stats:");
                println!("{}", "-".repeat(60));
                for session in &health.session_stats {
                    println!(
                        "  [{}] {} — {} tool failures, {} tools",
                        session.session_id,
                        session.timestamp.as_deref().unwrap_or("?"),
                        session.error_count,
                        session.tool_count,
                    );
                }
            }
            for warning in &warnings {
                eprintln!("warning: {warning}");
            }
            for (provider, _) in &report.skipped {
                eprintln!("warning: provider '{provider}' unavailable");
            }
        }
    }
    Ok(())
}
