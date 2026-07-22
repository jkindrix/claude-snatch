//! Priority suggestion command implementation.
//!
//! Ranks what to work on next based on recurring errors, file churn,
//! open goals, and unresolved decisions.

use std::collections::BTreeSet;

use crate::analysis::priorities::{
    suggest_priorities, suggest_provider_priorities, PriorityParams,
};
use crate::analysis::project_health::{ProjectHealthParams, ProviderProjectHealthAccumulator};
use crate::cli::{Cli, OutputFormat, PrioritiesArgs};
use crate::error::{Result, SnatchError};

use super::helpers::{self, SessionCollectParams};

/// Run the priorities command.
pub fn run(cli: &Cli, args: &PrioritiesArgs) -> Result<()> {
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
                "total_tool_failures": result.total_errors,
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
                "Priorities for '{}': {} sessions, {} tool failures, {} open goals, {} proposed decisions\n",
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

fn run_provider(cli: &Cli, args: &PrioritiesArgs) -> Result<()> {
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
    let goal_store = claude_project
        .as_ref()
        .and_then(|project| crate::goals::load_goals(project.path()).ok());
    let registry_path = claude_project
        .as_ref()
        .map(|project| project.path().display().to_string());
    let params = PriorityParams {
        max_priorities: args.max_priorities,
        ..Default::default()
    };
    let health_params = ProjectHealthParams {
        max_hotspots: params.max_files,
    };
    let analysis = accumulator.finish(decision_store.as_ref(), &health_params);
    let result = suggest_provider_priorities(
        analysis,
        decision_store.as_ref(),
        goal_store.as_ref(),
        &params,
    );
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

    match cli.effective_output() {
        OutputFormat::Json => {
            let output = serde_json::json!({
                "project": args.project,
                "providers": providers,
                "sessions_analyzed": result.sessions_analyzed,
                "session_descriptors_analyzed": result.session_descriptors_analyzed,
                "date_filter_fallback_descriptors": date_fallbacks.len(),
                "total_tool_failures": result.total_errors,
                "confirmed_tool_failures": result.confirmed_failures,
                "inferred_failure_signals": result.inferred_failures,
                "open_goals": result.open_goals,
                "proposed_decisions": result.proposed_decisions,
                "registry_coverage": {
                    "scope": "claude_project_registry",
                    "goals_available": result.open_goals.is_some(),
                    "decisions_available": result.proposed_decisions.is_some(),
                    "project_path": registry_path,
                },
                "priorities": result.priorities.iter().map(|priority| serde_json::json!({
                    "rank": priority.rank,
                    "category": priority.category,
                    "summary": priority.summary,
                    "score": priority.score,
                    "sources": priority.sources.iter().map(ToString::to_string).collect::<Vec<_>>(),
                })).collect::<Vec<_>>(),
                "skipped_providers": report.skipped.iter().map(|(provider, reason)| serde_json::json!({
                    "provider": provider.to_string(), "reason": reason,
                })).collect::<Vec<_>>(),
                "warnings": warnings,
                "coverage_note": "Reliability uses classified tool outcomes; churn uses source-backed applied patch/snapshot evidence. Registry priorities remain Claude-project scoped.",
            });
            println!("{}", serde_json::to_string_pretty(&output)?);
        }
        _ => {
            let goals = result
                .open_goals
                .map_or_else(|| "unavailable".to_string(), |count| count.to_string());
            let decisions = result
                .proposed_decisions
                .map_or_else(|| "unavailable".to_string(), |count| count.to_string());
            println!(
                "Priorities for '{}': {} logical sessions ({} descriptors), {} confirmed failures, {} inferred signals, {} open goals, {} proposed decisions\n",
                args.project,
                result.sessions_analyzed,
                result.session_descriptors_analyzed,
                result.confirmed_failures,
                result.inferred_failures,
                goals,
                decisions,
            );
            for item in &result.priorities {
                println!(
                    "  {}. [{}] {} (score: {:.1})",
                    item.rank,
                    item.category.to_uppercase(),
                    item.summary,
                    item.score
                );
                for source in &item.sources {
                    println!("     evidence: {source}");
                }
            }
            if result.open_goals.is_none() || result.proposed_decisions.is_none() {
                println!(
                    "\nRegistry priorities unavailable: Claude project registry not resolved."
                );
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
