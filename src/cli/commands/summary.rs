//! Summary command implementation.
//!
//! Shows a quick overview of Claude Code usage.

use std::collections::{BTreeMap, BTreeSet};

use chrono::{Duration, Utc};
use rayon::prelude::*;

use crate::analytics::{ProjectAnalytics, SessionAnalytics};
use crate::cli::{Cli, OutputFormat, SummaryArgs};
use crate::discovery::{format_count, format_number};
use crate::error::{Result, SnatchError};
use crate::provider::{
    project::SessionProjectContext, registry::ProviderSelection, LogicalSessionKey, ProviderPricing,
};
use crate::reconstruction::Conversation;
use crate::util::truncate_path;

use super::get_claude_dir;

/// Parse a period string into a Duration.
fn parse_period(period: &str) -> Result<Duration> {
    let s_lower = period.to_lowercase();

    // Extract numeric part and unit
    let numeric_end = s_lower
        .char_indices()
        .find(|(_, c)| !c.is_ascii_digit())
        .map(|(i, _)| i)
        .unwrap_or(s_lower.len());

    if numeric_end == 0 {
        return Err(SnatchError::InvalidArgument {
            name: "period".to_string(),
            reason: format!(
                "Invalid period '{}'. Use format like '24h', '1d', '7d', '1w'",
                period
            ),
        });
    }

    let amount: i64 = s_lower[..numeric_end]
        .parse()
        .map_err(|_| SnatchError::InvalidArgument {
            name: "period".to_string(),
            reason: format!("Invalid number in period: {}", &s_lower[..numeric_end]),
        })?;

    let unit = &s_lower[numeric_end..];
    match unit {
        "h" | "hr" | "hrs" | "hour" | "hours" => Ok(Duration::hours(amount)),
        "d" | "day" | "days" => Ok(Duration::days(amount)),
        "w" | "wk" | "wks" | "week" | "weeks" => Ok(Duration::weeks(amount)),
        "m" | "mo" | "month" | "months" => Ok(Duration::days(amount * 30)),
        _ => Err(SnatchError::InvalidArgument {
            name: "period".to_string(),
            reason: format!(
                "Unknown time unit '{}'. Use h (hours), d (days), w (weeks), m (months)",
                unit
            ),
        }),
    }
}

/// Summary output for JSON serialization.
#[derive(Debug, serde::Serialize)]
struct SummaryOutput {
    period: String,
    projects: usize,
    sessions: usize,
    total_tokens: u64,
    input_tokens: u64,
    output_tokens: u64,
    cache_read_tokens: u64,
    cache_creation_tokens: u64,
    total_processed_tokens: u64,
    messages: usize,
    tool_invocations: usize,
    estimated_cost: Option<f64>,
    top_projects: Vec<ProjectInfo>,
}

/// Project info for summary.
#[derive(Debug, Clone, serde::Serialize)]
struct ProjectInfo {
    path: String,
    sessions: usize,
}

#[derive(Debug, Clone, serde::Serialize)]
struct ProviderSummarySkip {
    provider: String,
    reason: String,
}

#[derive(Debug, serde::Serialize)]
struct ProviderSummaryOutput {
    period: String,
    period_basis: &'static str,
    projects: usize,
    sessions: usize,
    session_descriptors_analyzed: usize,
    activity_time_fallback_sessions: usize,
    total_tokens: u64,
    input_tokens: u64,
    output_tokens: u64,
    cache_read_tokens: u64,
    cache_creation_tokens: u64,
    total_processed_tokens: u64,
    messages: usize,
    tool_invocations: usize,
    estimated_cost: Option<f64>,
    pricing_coverage: &'static str,
    unpriced_providers: Vec<String>,
    unpriced_models: Vec<String>,
    top_projects: Vec<ProviderProjectInfo>,
    skipped_providers: Vec<ProviderSummarySkip>,
    warnings: Vec<String>,
}

#[derive(Debug, Clone, serde::Serialize)]
struct ProviderProjectInfo {
    project_key: String,
    path: String,
    sessions: usize,
}

#[derive(Default)]
struct ProviderSummaryAggregate {
    analytics: ProjectAnalytics,
    logical_sessions: BTreeSet<LogicalSessionKey>,
    activity_time_fallback_sessions: BTreeSet<LogicalSessionKey>,
    project_sessions: BTreeMap<String, ProviderProjectAggregate>,
    descriptor_count: usize,
    estimated_cost: f64,
    has_estimated_cost: bool,
    unpriced_providers: BTreeSet<String>,
    unpriced_models: BTreeSet<String>,
}

#[derive(Default)]
struct ProviderProjectAggregate {
    path: String,
    sessions: BTreeSet<LogicalSessionKey>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PeriodActivitySelection {
    Excluded,
    Native,
    SourceModifiedFallback,
    Untimed,
}

fn period_activity_selection(
    context: &SessionProjectContext,
    cutoff: chrono::DateTime<Utc>,
) -> PeriodActivitySelection {
    let native_activity = context.ended_at.or(context.started_at);
    if native_activity.is_some_and(|timestamp| timestamp > cutoff) {
        return PeriodActivitySelection::Native;
    }

    // A completed native tail is authoritative. Source mtime may decide only
    // when no native end was cheaply available or the physical tail could be
    // an in-progress/damaged record beyond the last complete native event.
    let source_may_decide = context.ended_at.is_none() || context.native_tail_unresolved;
    if source_may_decide
        && context
            .modified_at
            .is_some_and(|timestamp| timestamp > cutoff)
    {
        return PeriodActivitySelection::SourceModifiedFallback;
    }

    if native_activity.is_none() && context.modified_at.is_none() {
        PeriodActivitySelection::Untimed
    } else {
        PeriodActivitySelection::Excluded
    }
}

/// Run the summary command.
pub fn run(cli: &Cli, args: &SummaryArgs) -> Result<()> {
    if !args.provider.is_empty() {
        return run_provider(cli, args);
    }

    let claude_dir = get_claude_dir(cli.claude_dir.as_ref())?;
    let duration = parse_period(&args.period)?;
    let cutoff = Utc::now() - duration;

    // Get all sessions from the period
    let all_sessions = claude_dir.all_sessions()?;
    let recent_sessions: Vec<_> = all_sessions
        .into_iter()
        .filter(|s| s.modified_datetime() > cutoff)
        .collect();

    if recent_sessions.is_empty() {
        if !cli.quiet {
            println!("No sessions found in the last {}.", args.period);
        }
        return Ok(());
    }

    // Count sessions per project
    let mut project_counts: std::collections::HashMap<String, usize> =
        std::collections::HashMap::new();
    for session in &recent_sessions {
        let project = session.project_path().to_string();
        *project_counts.entry(project).or_insert(0) += 1;
    }

    let num_projects = project_counts.len();

    // Get top projects
    let mut top_projects: Vec<_> = project_counts.into_iter().collect();
    top_projects.sort_by_key(|b| std::cmp::Reverse(b.1));
    let top_projects: Vec<ProjectInfo> = top_projects
        .into_iter()
        .take(5)
        .map(|(path, sessions)| ProjectInfo { path, sessions })
        .collect();

    // Compute aggregate analytics
    let session_analytics: Vec<SessionAnalytics> = recent_sessions
        .par_iter()
        .filter_map(|session| {
            session
                .parse_with_options(cli.max_file_size)
                .ok()
                .and_then(|entries| {
                    Conversation::from_entries(entries)
                        .ok()
                        .map(|conv| SessionAnalytics::from_conversation(&conv))
                })
        })
        .collect();

    let mut combined = ProjectAnalytics::default();
    for analytics in &session_analytics {
        combined.add_session(analytics);
    }
    combined.calculate_cost();

    let num_sessions = recent_sessions.len();

    match cli.effective_output() {
        OutputFormat::Json => {
            let output = SummaryOutput {
                period: args.period.clone(),
                projects: num_projects,
                sessions: num_sessions,
                total_tokens: combined.total_usage.usage.work_tokens(),
                input_tokens: combined.total_usage.usage.input_tokens,
                output_tokens: combined.total_usage.usage.output_tokens,
                cache_read_tokens: combined
                    .total_usage
                    .usage
                    .cache_read_input_tokens
                    .unwrap_or(0),
                cache_creation_tokens: combined
                    .total_usage
                    .usage
                    .cache_creation_input_tokens
                    .unwrap_or(0),
                total_processed_tokens: combined.total_usage.usage.total_tokens(),
                messages: combined.message_counts.user + combined.message_counts.assistant,
                tool_invocations: combined.message_counts.tool_uses,
                estimated_cost: combined.total_usage.estimated_cost,
                top_projects,
            };
            println!("{}", serde_json::to_string_pretty(&output)?);
        }
        OutputFormat::Tsv => {
            println!("metric\tvalue");
            println!("period\t{}", args.period);
            println!("projects\t{num_projects}");
            println!("sessions\t{num_sessions}");
            println!("total_tokens\t{}", combined.total_usage.usage.work_tokens());
            println!("tool_invocations\t{}", combined.message_counts.tool_uses);
            if let Some(cost) = combined.total_usage.estimated_cost {
                println!("estimated_cost\t{cost:.4}");
            }
        }
        OutputFormat::Compact => {
            let cost = combined
                .total_usage
                .estimated_cost
                .map(|c| format!("${c:.2}"))
                .unwrap_or_else(|| "N/A".to_string());
            println!(
                "{}|sessions:{} tokens:{} tools:{} cost:{}",
                args.period,
                num_sessions,
                format_number(combined.total_usage.usage.work_tokens()),
                combined.message_counts.tool_uses,
                cost
            );
        }
        OutputFormat::Text => {
            println!("Claude Code Summary ({})", args.period);
            println!("{}", "=".repeat(35));
            println!();

            // Quick stats
            println!("Activity");
            println!("--------");
            println!("  Projects:  {}", format_count(num_projects));
            println!("  Sessions:  {}", format_count(num_sessions));
            println!();

            // Token usage
            println!("Usage");
            println!("-----");
            println!(
                "  Tokens:    {}",
                format_number(combined.total_usage.usage.work_tokens())
            );
            println!(
                "  Messages:  {}",
                format_count(combined.message_counts.user + combined.message_counts.assistant)
            );
            println!(
                "  Tools:     {}",
                format_count(combined.message_counts.tool_uses)
            );
            if let Some(cost) = combined.total_usage.estimated_cost {
                println!("  Cost:      ${cost:.2}");
            }
            println!();

            // Top projects
            if !top_projects.is_empty() {
                println!("Top Projects");
                println!("------------");
                for project in &top_projects {
                    let display_path = truncate_path(&project.path, 40);
                    println!("  {:40} {} sessions", display_path, project.sessions);
                }
                println!();
            }

            // Quick tips
            println!("Quick Commands");
            println!("--------------");
            println!("  snatch recent          Show recent sessions");
            println!("  snatch stats --global  Detailed statistics");
        }
    }

    Ok(())
}

/// Provider-routed cross-session summary.
///
/// The period selects logical session artifacts using native activity and a
/// conservative source-modification fallback; it does not pretend to clip
/// token accounting inside a long-running artifact at the exact cutoff.
/// Fork-copied history and spawned transcripts are excluded from aggregate
/// work.
fn run_provider(cli: &Cli, args: &SummaryArgs) -> Result<()> {
    let duration = parse_period(&args.period)?;
    let cutoff = Utc::now() - duration;
    let selection = ProviderSelection::from_flags(&args.provider).map_err(|reason| {
        SnatchError::InvalidArgument {
            name: "--provider".to_string(),
            reason,
        }
    })?;
    let atomic = matches!(selection, ProviderSelection::Explicit(_));
    let registry = super::helpers::provider_registry(cli);
    let mut aggregate = ProviderSummaryAggregate::default();
    let mut selected_descriptors = 0_usize;
    let mut untimed = BTreeSet::new();
    let mut analysis_errors = Vec::new();

    let report = registry.visit_filtered_parsed_project_sessions(
        &selection,
        crate::cache::global_cache(),
        None,
        false,
        |_, session| {
            let selection = period_activity_selection(&session.context, cutoff);
            let include = selection != PeriodActivitySelection::Excluded;
            if include {
                selected_descriptors += 1;
                if selection == PeriodActivitySelection::Untimed {
                    untimed.insert(session.descriptor.key.clone());
                }
            }
            include
        },
        |project, session, logical_root, parsed| {
            let provider = match registry.get(&session.descriptor.key.provider) {
                Ok(provider) => provider,
                Err(error) => {
                    analysis_errors.push(format!(
                        "{}: provider context unavailable: {error}",
                        session.descriptor.key
                    ));
                    return;
                }
            };
            let entries = crate::provider::project::new_activity_entries(&parsed);
            let conversation = match Conversation::from_entries(entries) {
                Ok(conversation) => conversation,
                Err(error) => {
                    analysis_errors.push(format!(
                        "{}: projected conversation could not be reconstructed: {error}",
                        session.descriptor.key
                    ));
                    return;
                }
            };
            let analytics = SessionAnalytics::from_conversation(&conversation);
            let pricing_policy = provider.capabilities().pricing;
            let usage =
                crate::analysis::usage::provider_usage_summary(&conversation, pricing_policy);
            aggregate.analytics.add_session(&analytics);
            aggregate.logical_sessions.insert(logical_root.clone());
            if period_activity_selection(&session.context, cutoff)
                == PeriodActivitySelection::SourceModifiedFallback
            {
                aggregate
                    .activity_time_fallback_sessions
                    .insert(logical_root.clone());
            }
            let project_path = project
                .display_path
                .clone()
                .unwrap_or_else(|| project.identity.to_string());
            let project_aggregate = aggregate
                .project_sessions
                .entry(project.identity.to_string())
                .or_insert_with(|| ProviderProjectAggregate {
                    path: project_path,
                    ..Default::default()
                });
            project_aggregate.sessions.insert(logical_root.clone());
            aggregate.descriptor_count += 1;
            if let Some(cost) = usage.pricing.estimated_cost {
                aggregate.estimated_cost += cost;
                aggregate.has_estimated_cost = true;
            }
            if pricing_policy == ProviderPricing::Unpriced
                && usage.canonical.total_processed_tokens > 0
            {
                aggregate
                    .unpriced_providers
                    .insert(session.descriptor.key.provider.to_string());
            }
            aggregate
                .unpriced_models
                .extend(usage.pricing.unpriced_models);
        },
    )?;

    if atomic && !analysis_errors.is_empty() {
        return Err(SnatchError::ConfigError {
            message: analysis_errors.join("; "),
        });
    }
    let mut warnings = report.warnings;
    warnings.extend(analysis_errors);
    warnings.extend(
        untimed
            .into_iter()
            .map(|key| format!("{key}: activity time unavailable; included conservatively")),
    );
    warnings.sort();
    warnings.dedup();
    if selected_descriptors > 0 && aggregate.descriptor_count == 0 {
        return Err(SnatchError::ConfigError {
            message: "no selected session could be analyzed".to_string(),
        });
    }

    let mut top_projects: Vec<_> = aggregate
        .project_sessions
        .iter()
        .map(|(project_key, project)| ProviderProjectInfo {
            project_key: project_key.clone(),
            path: project.path.clone(),
            sessions: project.sessions.len(),
        })
        .collect();
    top_projects.sort_by(|a, b| {
        b.sessions
            .cmp(&a.sessions)
            .then_with(|| a.project_key.cmp(&b.project_key))
    });
    top_projects.truncate(5);

    let usage = &aggregate.analytics.total_usage.usage;
    let total_processed_tokens = usage.total_tokens();
    let has_unpriced =
        !aggregate.unpriced_providers.is_empty() || !aggregate.unpriced_models.is_empty();
    let pricing_coverage = if total_processed_tokens == 0 {
        "not-applicable"
    } else if has_unpriced && aggregate.has_estimated_cost {
        "partial"
    } else if has_unpriced {
        "unavailable"
    } else if aggregate.has_estimated_cost {
        "complete"
    } else {
        "unavailable"
    };
    let estimated_cost = aggregate
        .has_estimated_cost
        .then_some(aggregate.estimated_cost);
    let num_projects = aggregate.project_sessions.len();
    let num_sessions = aggregate.logical_sessions.len();
    let skipped_providers: Vec<_> = report
        .skipped
        .iter()
        .map(|(provider, reason)| ProviderSummarySkip {
            provider: provider.to_string(),
            reason: reason.clone(),
        })
        .collect();
    let unpriced_providers: Vec<_> = aggregate.unpriced_providers.iter().cloned().collect();
    let unpriced_models: Vec<_> = aggregate.unpriced_models.iter().cloned().collect();

    match cli.effective_output() {
        OutputFormat::Json => {
            let output = ProviderSummaryOutput {
                period: args.period.clone(),
                period_basis: "logical sessions with native activity or source modification in period; whole selected artifacts",
                projects: num_projects,
                sessions: num_sessions,
                session_descriptors_analyzed: aggregate.descriptor_count,
                activity_time_fallback_sessions: aggregate
                    .activity_time_fallback_sessions
                    .len(),
                total_tokens: usage.work_tokens(),
                input_tokens: usage.input_tokens,
                output_tokens: usage.output_tokens,
                cache_read_tokens: usage.cache_read_input_tokens.unwrap_or(0),
                cache_creation_tokens: usage.cache_creation_input_tokens.unwrap_or(0),
                total_processed_tokens,
                messages: aggregate.analytics.message_counts.user
                    + aggregate.analytics.message_counts.assistant,
                tool_invocations: aggregate.analytics.message_counts.tool_uses,
                estimated_cost,
                pricing_coverage,
                unpriced_providers: unpriced_providers.clone(),
                unpriced_models: unpriced_models.clone(),
                top_projects: top_projects.clone(),
                skipped_providers: skipped_providers.clone(),
                warnings: warnings.clone(),
            };
            println!("{}", serde_json::to_string_pretty(&output)?);
        }
        OutputFormat::Tsv => {
            println!("metric\tvalue");
            println!("period\t{}", args.period);
            println!("period_basis\tlogical_sessions_native_or_source_activity_whole_artifacts");
            println!("projects\t{num_projects}");
            println!("sessions\t{num_sessions}");
            println!(
                "session_descriptors_analyzed\t{}",
                aggregate.descriptor_count
            );
            println!(
                "activity_time_fallback_sessions\t{}",
                aggregate.activity_time_fallback_sessions.len()
            );
            println!("total_tokens\t{}", usage.work_tokens());
            println!("total_processed_tokens\t{total_processed_tokens}");
            println!(
                "tool_invocations\t{}",
                aggregate.analytics.message_counts.tool_uses
            );
            println!("pricing_coverage\t{pricing_coverage}");
            if let Some(cost) = estimated_cost {
                println!("estimated_cost\t{cost:.4}");
            }
        }
        OutputFormat::Compact => {
            let cost = estimated_cost
                .map(|cost| format!("${cost:.2}"))
                .unwrap_or_else(|| "N/A".to_string());
            println!(
                "{}|sessions:{} tokens:{} tools:{} cost:{} coverage:{}",
                args.period,
                num_sessions,
                format_number(usage.work_tokens()),
                aggregate.analytics.message_counts.tool_uses,
                cost,
                pricing_coverage,
            );
        }
        OutputFormat::Text => {
            if num_sessions == 0 {
                if !cli.quiet {
                    println!("No sessions found in the last {}.", args.period);
                }
            } else {
                println!("Session Summary ({})", args.period);
                println!("{}", "=".repeat(35));
                println!(
                    "Period basis: native or source activity in the period; whole selected artifacts"
                );
                println!();
                println!("Activity");
                println!("--------");
                println!("  Projects:  {}", format_count(num_projects));
                println!("  Sessions:  {}", format_count(num_sessions));
                println!(
                    "  Source sessions analyzed: {}",
                    format_count(aggregate.descriptor_count)
                );
                if !aggregate.activity_time_fallback_sessions.is_empty() {
                    println!(
                        "  Activity-time fallbacks: {}",
                        format_count(aggregate.activity_time_fallback_sessions.len())
                    );
                }
                println!();
                println!("Usage");
                println!("-----");
                println!("  Work tokens:      {}", format_number(usage.work_tokens()));
                println!(
                    "  Processed tokens: {}",
                    format_number(total_processed_tokens)
                );
                println!(
                    "  Messages:         {}",
                    format_count(
                        aggregate.analytics.message_counts.user
                            + aggregate.analytics.message_counts.assistant
                    )
                );
                println!(
                    "  Tools:            {}",
                    format_count(aggregate.analytics.message_counts.tool_uses)
                );
                match estimated_cost {
                    Some(cost) => println!("  Estimated cost:   ${cost:.2} ({pricing_coverage})"),
                    None => println!("  Estimated cost:   N/A ({pricing_coverage})"),
                }
                if !unpriced_providers.is_empty() {
                    println!("  Unpriced providers: {}", unpriced_providers.join(", "));
                }
                if !unpriced_models.is_empty() {
                    println!("  Unpriced models: {}", unpriced_models.join(", "));
                }
                println!();
                if !top_projects.is_empty() {
                    println!("Top Projects");
                    println!("------------");
                    for project in &top_projects {
                        println!(
                            "  {:40} {} sessions",
                            truncate_path(&project.path, 40),
                            project.sessions
                        );
                    }
                }
            }
        }
    }

    for skipped in &skipped_providers {
        eprintln!(
            "warning: provider '{}' skipped: {}",
            skipped.provider, skipped.reason
        );
    }
    for warning in &warnings {
        eprintln!("warning: {warning}");
    }
    Ok(())
}

/// Run a quick summary for bare `snatch` command in non-interactive mode.
///
/// This provides a brief overview without requiring arguments, suitable for
/// piping or scripting contexts.
pub fn run_quick_summary(cli: &Cli) -> Result<()> {
    let claude_dir = get_claude_dir(cli.claude_dir.as_ref())?;

    // Use 24h default period
    let duration = Duration::hours(24);
    let cutoff = Utc::now() - duration;

    // Get all sessions from the period
    let all_sessions = claude_dir.all_sessions()?;
    let total_sessions = all_sessions.len();
    let recent_sessions: Vec<_> = all_sessions
        .into_iter()
        .filter(|s| s.modified_datetime() > cutoff)
        .collect();

    // Count projects
    let mut project_set: std::collections::HashSet<String> = std::collections::HashSet::new();
    for session in &recent_sessions {
        project_set.insert(session.project_path().to_string());
    }

    match cli.effective_output() {
        OutputFormat::Json => {
            let output = serde_json::json!({
                "sessions_24h": recent_sessions.len(),
                "projects_24h": project_set.len(),
                "total_sessions": total_sessions,
            });
            println!("{}", serde_json::to_string(&output)?);
        }
        OutputFormat::Tsv => {
            println!("sessions_24h\tprojects_24h\ttotal_sessions");
            println!(
                "{}\t{}\t{}",
                recent_sessions.len(),
                project_set.len(),
                total_sessions
            );
        }
        OutputFormat::Compact => {
            println!(
                "24h:{} sessions/{} projects | total:{} sessions",
                recent_sessions.len(),
                project_set.len(),
                total_sessions
            );
        }
        OutputFormat::Text => {
            println!(
                "claude-snatch: {} sessions in last 24h ({} projects), {} total",
                recent_sessions.len(),
                project_set.len(),
                format_count(total_sessions)
            );
            println!();
            println!("Run 'snatch --help' for commands.");
        }
    }

    Ok(())
}
