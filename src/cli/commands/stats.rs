//! Stats command implementation.
//!
//! Displays usage statistics for sessions and projects.

use rayon::prelude::*;

use crate::analytics::{ProjectAnalytics, SessionAnalytics};
use crate::cli::{Cli, OutputFormat, StatsArgs};
use crate::discovery::{format_count, format_number, Session};
use crate::error::{Result, SnatchError};
use crate::reconstruction::Conversation;

use super::get_claude_dir;

/// Format a model name for display, adding explanatory notes for special values.
fn format_model_name(model: &str) -> String {
    match model {
        "<synthetic>" => "(synthetic - internal/cached response)".to_string(),
        "" => "(unknown model)".to_string(),
        m => m.to_string(),
    }
}

/// Compute statistics in parallel across multiple sessions.
fn compute_stats_parallel(sessions: &[Session]) -> ProjectAnalytics {
    // Process sessions in parallel and collect individual analytics
    let session_analytics: Vec<SessionAnalytics> = sessions
        .par_iter()
        .filter_map(|session| {
            session.parse().ok().and_then(|entries| {
                Conversation::from_entries(entries)
                    .ok()
                    .map(|conv| SessionAnalytics::from_conversation(&conv))
            })
        })
        .collect();

    // Merge all analytics into a single ProjectAnalytics
    let mut combined = ProjectAnalytics::default();
    for analytics in session_analytics {
        combined.add_session(&analytics);
    }
    combined.calculate_cost();
    combined
}

/// Run the stats command.
pub fn run(cli: &Cli, args: &StatsArgs) -> Result<()> {
    let claude_dir = get_claude_dir(cli.claude_dir.as_ref())?;

    if let Some(session_id) = &args.session {
        // Stats for specific session
        let session = claude_dir
            .find_session(session_id)?
            .ok_or_else(|| SnatchError::SessionNotFound {
                session_id: session_id.clone(),
            })?;

        let entries = session.parse()?;
        let conversation = Conversation::from_entries(entries)?;
        let analytics = SessionAnalytics::from_conversation(&conversation);

        output_session_stats(cli, args, &analytics)?;
    } else if let Some(project_filter) = &args.project {
        // Stats for specific project
        let projects = claude_dir.projects()?;
        let project = projects
            .iter()
            .find(|p| p.decoded_path().contains(project_filter))
            .ok_or_else(|| SnatchError::ProjectNotFound {
                project_path: project_filter.clone(),
            })?;

        let sessions = project.sessions()?;
        let project_analytics = compute_stats_parallel(&sessions);

        output_project_stats(cli, args, &project_analytics, project.decoded_path())?;
    } else if args.global {
        // Global stats across all sessions - parallel processing
        let all_sessions = claude_dir.all_sessions()?;
        let global_analytics = compute_stats_parallel(&all_sessions);

        output_global_stats(cli, args, &global_analytics)?;
    } else {
        // Default: show summary of all projects
        output_overview(cli, args, &claude_dir)?;
    }

    Ok(())
}

/// Output session statistics.
fn output_session_stats(cli: &Cli, args: &StatsArgs, analytics: &SessionAnalytics) -> Result<()> {
    let summary = analytics.summary_report();

    match cli.effective_output() {
        OutputFormat::Json => {
            println!("{}", serde_json::to_string_pretty(&StatsOutput::from_session(analytics))?);
        }
        OutputFormat::Tsv => {
            println!("metric\tvalue");
            println!("total_tokens\t{}", summary.total_tokens);
            println!("input_tokens\t{}", summary.input_tokens);
            println!("output_tokens\t{}", summary.output_tokens);
            println!("messages\t{}", summary.total_messages);
            println!("tool_invocations\t{}", summary.tool_invocations);
            println!("cache_hit_rate\t{:.2}", summary.cache_hit_rate);
            if let Some(cost) = summary.estimated_cost {
                println!("estimated_cost\t{cost:.4}");
            }
        }
        OutputFormat::Compact => {
            println!(
                "tokens:{} msgs:{} tools:{} cost:{}",
                summary.total_tokens,
                summary.total_messages,
                summary.tool_invocations,
                summary.cost_string()
            );
        }
        OutputFormat::Text => {
            println!("Session Statistics");
            println!("==================");
            println!();

            // Duration
            if let Some(duration) = analytics.duration_string() {
                println!("Duration: {duration}");
            }

            // Model
            if let Some(model) = analytics.primary_model() {
                println!("Primary Model: {model}");
            }
            println!();

            // Token usage
            println!("Token Usage:");
            println!("  Input:  {:>14} tokens", format_number(summary.input_tokens));
            println!("  Output: {:>14} tokens", format_number(summary.output_tokens));
            println!("  Total:  {:>14} tokens", format_number(summary.total_tokens));
            println!("  Cache Hit Rate: {:.1}%", summary.cache_hit_rate);
            println!();

            // Messages
            println!("Messages:");
            println!("  User:      {:>10}", format_count(summary.user_messages));
            println!("  Assistant: {:>10}", format_count(summary.assistant_messages));
            println!("  Total:     {:>10}", format_count(summary.total_messages));
            println!();

            // Tools
            if summary.tool_invocations > 0 || args.tools || args.all {
                println!("Tool Usage:");
                println!("  Total Invocations: {}", format_count(summary.tool_invocations));
                println!("  Unique Tools:      {}", format_count(summary.unique_tools));

                if args.tools || args.all {
                    println!();
                    println!("  Top Tools:");
                    for (tool, count) in analytics.top_tools(10) {
                        println!("    {tool}: {}", format_count(count));
                    }
                }
                println!();
            }

            // Thinking
            if summary.thinking_blocks > 0 {
                println!("Thinking:");
                println!("  Blocks: {}", format_count(summary.thinking_blocks));
                println!(
                    "  Avg Block Length: {} chars",
                    format_count(analytics.thinking_stats.average_length())
                );
                println!();
            }

            // Cost
            println!("Estimated Cost: {}", summary.cost_string());

            // Errors
            if summary.error_count > 0 {
                println!();
                println!("Errors: {}", format_count(summary.error_count));
                let breakdown = analytics.error_breakdown();
                if !breakdown.is_empty() {
                    for (error_type, count) in breakdown {
                        // Format error type for display (e.g., "tool_error" -> "Tool errors")
                        let label = match error_type {
                            "tool_error" => "Tool errors",
                            "api_error" => "API errors",
                            other => other,
                        };
                        println!("  {label}: {}", format_count(count));
                    }
                }
            }
        }
    }

    Ok(())
}

/// Output project statistics.
fn output_project_stats(
    cli: &Cli,
    args: &StatsArgs,
    analytics: &ProjectAnalytics,
    project_path: &str,
) -> Result<()> {
    match cli.effective_output() {
        OutputFormat::Json => {
            println!("{}", serde_json::to_string_pretty(&StatsOutput::from_project(analytics, project_path))?);
        }
        OutputFormat::Tsv => {
            println!("metric\tvalue");
            println!("sessions\t{}", analytics.session_count);
            println!("total_tokens\t{}", analytics.total_usage.usage.total_tokens());
            println!("tool_invocations\t{}", analytics.message_counts.tool_uses);
        }
        OutputFormat::Compact => {
            println!(
                "sessions:{} tokens:{} tools:{}",
                analytics.session_count,
                analytics.total_usage.usage.total_tokens(),
                analytics.message_counts.tool_uses
            );
        }
        OutputFormat::Text => {
            println!("Project Statistics: {project_path}");
            println!("{}", "=".repeat(20 + project_path.len()));
            println!();
            println!("Sessions: {}", format_count(analytics.session_count));
            println!();

            // Duration
            let total_secs = analytics.total_duration.num_seconds();
            if total_secs > 0 {
                println!("Total Duration: {}h {}m",
                    total_secs / 3600,
                    (total_secs % 3600) / 60
                );
                println!();
            }

            // Token usage
            println!("Token Usage:");
            println!("  Total: {} tokens", format_number(analytics.total_usage.usage.total_tokens()));
            println!();

            // Messages
            println!("Messages:");
            println!("  User:      {}", format_count(analytics.message_counts.user));
            println!("  Assistant: {}", format_count(analytics.message_counts.assistant));
            println!();

            // Tools
            if args.tools || args.all {
                println!("Tool Usage:");
                let mut tools: Vec<_> = analytics.tool_counts.iter().collect();
                tools.sort_by(|a, b| b.1.cmp(a.1));
                for (tool, count) in tools.iter().take(10) {
                    println!("  {tool}: {}", format_count(**count));
                }
                println!();
            }

            // Models
            if args.models || args.all {
                println!("Model Usage:");
                for (model, count) in &analytics.model_usage {
                    let display_name = format_model_name(model);
                    println!("  {display_name}: {} uses", format_number(*count));
                }
                println!();
            }

            // Cost breakdown
            if args.costs || args.all {
                println!("Cost Breakdown by Model:");
                let mut total_cost = 0.0;
                for (model, usage) in &analytics.total_usage.by_model {
                    if let Some(pricing) = crate::model::ModelPricing::for_model(model) {
                        let cost = pricing.calculate_cost(usage);
                        total_cost += cost.total_cost;
                        if cost.total_cost > 0.0 {
                            let display_name = format_model_name(model);
                            println!("  {display_name}:");
                            println!("    Input:       ${:.4}", cost.input_cost);
                            println!("    Output:      ${:.4}", cost.output_cost);
                            println!("    Cache Write: ${:.4}", cost.cache_write_cost);
                            println!("    Cache Read:  ${:.4}", cost.cache_read_cost);
                            println!("    Subtotal:    ${:.4}", cost.total_cost);
                        }
                    }
                }
                println!();
                println!("Estimated Total Cost: ${total_cost:.2}");
            } else if let Some(cost) = analytics.total_usage.estimated_cost {
                // Just show total cost without breakdown
                println!("Estimated Cost: ${cost:.2}");
            }
        }
    }

    Ok(())
}

/// Output global statistics.
fn output_global_stats(cli: &Cli, args: &StatsArgs, analytics: &ProjectAnalytics) -> Result<()> {
    match cli.effective_output() {
        OutputFormat::Json => {
            println!("{}", serde_json::to_string_pretty(&StatsOutput::from_project(analytics, "global"))?);
        }
        OutputFormat::Tsv => {
            println!("metric\tvalue");
            println!("sessions\t{}", analytics.session_count);
            println!("total_tokens\t{}", analytics.total_usage.usage.total_tokens());
            if let Some(cost) = analytics.total_usage.estimated_cost {
                println!("estimated_cost\t{cost:.4}");
            }
        }
        OutputFormat::Compact => {
            let cost = analytics.total_usage.estimated_cost.map(|c| format!("${c:.2}")).unwrap_or_else(|| "N/A".to_string());
            println!("sessions:{} tokens:{} cost:{}",
                analytics.session_count,
                analytics.total_usage.usage.total_tokens(),
                cost
            );
        }
        OutputFormat::Text => {
            println!("Global Statistics");
            println!("=================");
            println!();
            println!("Total Sessions: {}", format_count(analytics.session_count));
            println!();

            // Duration
            let total_secs = analytics.total_duration.num_seconds();
            if total_secs > 0 {
                println!("Total Duration: {}h {}m",
                    total_secs / 3600,
                    (total_secs % 3600) / 60
                );
                println!();
            }

            // Token usage
            let usage = &analytics.total_usage.usage;
            println!("Token Usage:");
            println!("  Input:  {} tokens", format_number(usage.total_input_tokens()));
            println!("  Output: {} tokens", format_number(usage.output_tokens));
            println!("  Total:  {} tokens", format_number(usage.total_tokens()));
            println!();

            // Messages
            println!("Messages:");
            println!("  User:      {}", format_count(analytics.message_counts.user));
            println!("  Assistant: {}", format_count(analytics.message_counts.assistant));
            println!("  Tool Uses: {}", format_count(analytics.message_counts.tool_uses));
            println!();

            // Top tools
            if args.tools || args.all {
                println!("Top Tools:");
                let mut tools: Vec<_> = analytics.tool_counts.iter().collect();
                tools.sort_by(|a, b| b.1.cmp(a.1));
                for (tool, count) in tools.iter().take(10) {
                    println!("  {tool}: {}", format_count(**count));
                }
                println!();
            }

            // Models
            if args.models || args.all {
                println!("Model Usage:");
                for (model, count) in &analytics.model_usage {
                    let display_name = format_model_name(model);
                    println!("  {display_name}: {} uses", format_number(*count));
                }
                println!();
            }

            // Cost breakdown
            if args.costs || args.all {
                println!("Cost Breakdown by Model:");
                let mut total_cost = 0.0;
                for (model, usage) in &analytics.total_usage.by_model {
                    if let Some(pricing) = crate::model::ModelPricing::for_model(model) {
                        let cost = pricing.calculate_cost(usage);
                        total_cost += cost.total_cost;
                        if cost.total_cost > 0.0 {
                            let display_name = format_model_name(model);
                            println!("  {display_name}:");
                            println!("    Input:       ${:.4}", cost.input_cost);
                            println!("    Output:      ${:.4}", cost.output_cost);
                            println!("    Cache Write: ${:.4}", cost.cache_write_cost);
                            println!("    Cache Read:  ${:.4}", cost.cache_read_cost);
                            println!("    Subtotal:    ${:.4}", cost.total_cost);
                        }
                    }
                }
                println!();
                println!("Estimated Total Cost: ${total_cost:.2}");
            } else if let Some(cost) = analytics.total_usage.estimated_cost {
                // Just show total cost without breakdown
                println!("Estimated Total Cost: ${cost:.2}");
            }
        }
    }

    Ok(())
}

/// Output overview of all projects.
fn output_overview(
    cli: &Cli,
    _args: &StatsArgs,
    claude_dir: &crate::discovery::ClaudeDirectory,
) -> Result<()> {
    let stats = claude_dir.statistics()?;

    match cli.effective_output() {
        OutputFormat::Json => {
            println!("{}", serde_json::to_string_pretty(&OverviewOutput {
                project_count: stats.project_count,
                session_count: stats.session_count,
                subagent_count: stats.subagent_count,
                total_size_bytes: stats.total_size_bytes,
                total_size_human: stats.total_size_human(),
            })?);
        }
        OutputFormat::Tsv => {
            println!("metric\tvalue");
            println!("projects\t{}", stats.project_count);
            println!("sessions\t{}", stats.session_count);
            println!("subagents\t{}", stats.subagent_count);
            println!("total_size\t{}", stats.total_size_bytes);
        }
        OutputFormat::Compact => {
            println!("projects:{} sessions:{} size:{}",
                stats.project_count,
                stats.session_count,
                stats.total_size_human()
            );
        }
        OutputFormat::Text => {
            println!("Claude Code Overview");
            println!("====================");
            println!();
            println!("Projects:        {}", format_count(stats.project_count));
            println!("Sessions:        {}", format_count(stats.session_count));
            println!("Subagents:       {}", format_count(stats.subagent_count));
            println!("Total Size:      {}", stats.total_size_human());

            if stats.has_file_history {
                println!("Backup Files:    {}", format_count(stats.backup_file_count));
            }

            println!();
            println!("Use 'snatch stats --global' for detailed token and cost statistics.");
        }
    }

    Ok(())
}

/// Stats output for JSON serialization.
#[derive(Debug, serde::Serialize)]
struct StatsOutput {
    scope: String,
    sessions: Option<usize>,
    total_tokens: u64,
    input_tokens: u64,
    output_tokens: u64,
    messages: usize,
    tool_invocations: usize,
    cache_hit_rate: Option<f64>,
    estimated_cost: Option<f64>,
}

impl StatsOutput {
    fn from_session(analytics: &SessionAnalytics) -> Self {
        let summary = analytics.summary_report();
        Self {
            scope: "session".to_string(),
            sessions: Some(1),
            total_tokens: summary.total_tokens,
            input_tokens: summary.input_tokens,
            output_tokens: summary.output_tokens,
            messages: summary.total_messages,
            tool_invocations: summary.tool_invocations,
            cache_hit_rate: Some(summary.cache_hit_rate),
            estimated_cost: summary.estimated_cost,
        }
    }

    fn from_project(analytics: &ProjectAnalytics, scope: &str) -> Self {
        Self {
            scope: scope.to_string(),
            sessions: Some(analytics.session_count),
            total_tokens: analytics.total_usage.usage.total_tokens(),
            input_tokens: analytics.total_usage.usage.total_input_tokens(),
            output_tokens: analytics.total_usage.usage.output_tokens,
            messages: analytics.message_counts.user + analytics.message_counts.assistant,
            tool_invocations: analytics.message_counts.tool_uses,
            cache_hit_rate: None,
            estimated_cost: analytics.total_usage.estimated_cost,
        }
    }
}

/// Overview output for JSON serialization.
#[derive(Debug, serde::Serialize)]
struct OverviewOutput {
    project_count: usize,
    session_count: usize,
    subagent_count: usize,
    total_size_bytes: u64,
    total_size_human: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::analytics::SessionAnalytics;

    #[test]
    fn test_stats_output_from_session() {
        let analytics = SessionAnalytics::default();
        let output = StatsOutput::from_session(&analytics);

        assert_eq!(output.scope, "session");
        assert_eq!(output.sessions, Some(1));
        assert_eq!(output.total_tokens, 0);
        assert_eq!(output.input_tokens, 0);
        assert_eq!(output.output_tokens, 0);
        assert_eq!(output.messages, 0);
        assert_eq!(output.tool_invocations, 0);
        assert!(output.cache_hit_rate.is_some());
    }

    #[test]
    fn test_stats_output_from_project() {
        let analytics = ProjectAnalytics::default();
        let output = StatsOutput::from_project(&analytics, "test-project");

        assert_eq!(output.scope, "test-project");
        assert_eq!(output.sessions, Some(0));
        assert_eq!(output.total_tokens, 0);
        assert_eq!(output.messages, 0);
        assert_eq!(output.tool_invocations, 0);
        assert!(output.cache_hit_rate.is_none());
    }

    #[test]
    fn test_stats_output_serialization() {
        let output = StatsOutput {
            scope: "session".to_string(),
            sessions: Some(5),
            total_tokens: 1000,
            input_tokens: 600,
            output_tokens: 400,
            messages: 10,
            tool_invocations: 3,
            cache_hit_rate: Some(0.75),
            estimated_cost: Some(0.05),
        };

        let json = serde_json::to_string(&output).unwrap();
        assert!(json.contains("\"scope\":\"session\""));
        assert!(json.contains("\"total_tokens\":1000"));
        assert!(json.contains("\"cache_hit_rate\":0.75"));
    }

    #[test]
    fn test_overview_output_serialization() {
        let output = OverviewOutput {
            project_count: 5,
            session_count: 20,
            subagent_count: 10,
            total_size_bytes: 1024 * 1024,
            total_size_human: "1 MB".to_string(),
        };

        let json = serde_json::to_string(&output).unwrap();
        assert!(json.contains("\"project_count\":5"));
        assert!(json.contains("\"session_count\":20"));
        assert!(json.contains("\"total_size_human\":\"1 MB\""));
    }
}
