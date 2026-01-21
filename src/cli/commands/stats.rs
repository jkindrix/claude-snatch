//! Stats command implementation.
//!
//! Displays usage statistics for sessions and projects.

use std::collections::BTreeMap;

use chrono::{DateTime, Datelike, Duration, TimeZone, Timelike, Utc};
use rayon::prelude::*;

use crate::analytics::history::{CostDataPoint, CostHistory};
use crate::analytics::{ProjectAnalytics, SessionAnalytics};
use crate::cli::{Cli, OutputFormat, StatsArgs};
use crate::config::Config;
use crate::discovery::{format_count, format_number, Session};
use crate::error::{Result, SnatchError};
use crate::model::{ContentBlock, LogEntry};
use crate::reconstruction::Conversation;
use crate::util::{sparkline_u64, sparkline_with_range};

use super::get_claude_dir;

/// The duration of a billing window (5 hours).
const BILLING_WINDOW_HOURS: i64 = 5;

/// Represents a 5-hour billing window block.
#[derive(Debug, Clone, Default, serde::Serialize)]
struct BillingBlock {
    /// Block start time (UTC).
    #[serde(with = "chrono::serde::ts_seconds")]
    start: DateTime<Utc>,
    /// Block end time (UTC).
    #[serde(with = "chrono::serde::ts_seconds")]
    end: DateTime<Utc>,
    /// Status: "active", "completed", or "gap".
    status: String,
    /// Input tokens in this block.
    input_tokens: u64,
    /// Output tokens in this block.
    output_tokens: u64,
    /// Cache read tokens in this block.
    cache_read_tokens: u64,
    /// Cache creation tokens in this block.
    cache_creation_tokens: u64,
    /// Total tokens in this block.
    total_tokens: u64,
    /// Estimated cost for this block.
    #[serde(skip_serializing_if = "Option::is_none")]
    estimated_cost: Option<f64>,
    /// Message count in this block.
    message_count: usize,
    /// Tool invocations in this block.
    tool_invocations: usize,
}

impl BillingBlock {
    /// Create a new billing block for the given time range.
    fn new(start: DateTime<Utc>, end: DateTime<Utc>) -> Self {
        let now = Utc::now();
        let status = if end > now {
            "active".to_string()
        } else {
            "completed".to_string()
        };
        Self {
            start,
            end,
            status,
            ..Default::default()
        }
    }

    /// Create a gap block (no activity).
    #[allow(dead_code)]
    fn gap(start: DateTime<Utc>, end: DateTime<Utc>) -> Self {
        Self {
            start,
            end,
            status: "gap".to_string(),
            ..Default::default()
        }
    }

    /// Get the remaining time for an active block.
    fn remaining(&self) -> Option<Duration> {
        if self.status == "active" {
            let now = Utc::now();
            if self.end > now {
                return Some(self.end - now);
            }
        }
        None
    }
}

/// Calculate the billing block start time for a given timestamp.
/// Blocks start at midnight, 5am, 10am, 3pm, 8pm UTC.
fn block_start_for(timestamp: DateTime<Utc>) -> DateTime<Utc> {
    let hour = timestamp.hour() as i64;
    let block_hour = (hour / BILLING_WINDOW_HOURS) * BILLING_WINDOW_HOURS;
    Utc.with_ymd_and_hms(
        timestamp.year(),
        timestamp.month(),
        timestamp.day(),
        block_hour as u32,
        0,
        0,
    )
    .single()
    .unwrap_or(timestamp)
}

/// Calculate the billing block end time for a given timestamp.
fn block_end_for(timestamp: DateTime<Utc>) -> DateTime<Utc> {
    block_start_for(timestamp) + Duration::hours(BILLING_WINDOW_HOURS)
}

/// Aggregate entries into billing blocks.
fn aggregate_billing_blocks(sessions: &[Session], max_file_size: Option<u64>) -> Vec<BillingBlock> {
    // Use BTreeMap to keep blocks sorted by start time
    let mut blocks: BTreeMap<DateTime<Utc>, BillingBlock> = BTreeMap::new();

    for session in sessions {
        let entries = match session.parse_with_options(max_file_size) {
            Ok(e) => e,
            Err(_) => continue,
        };

        for entry in entries {
            let timestamp = match entry.timestamp() {
                Some(ts) => ts,
                None => continue,
            };

            let block_start = block_start_for(timestamp);
            let block_end = block_end_for(timestamp);

            let block = blocks
                .entry(block_start)
                .or_insert_with(|| BillingBlock::new(block_start, block_end));

            // Count messages
            block.message_count += 1;

            // Extract usage and tool invocations based on entry type
            if let LogEntry::Assistant(assistant) = &entry {
                // Count output tokens from assistant messages
                if let Some(usage) = &assistant.message.usage {
                    block.input_tokens += usage.input_tokens;
                    block.output_tokens += usage.output_tokens;
                    block.cache_read_tokens += usage.cache_read_input_tokens.unwrap_or(0);
                    block.cache_creation_tokens += usage.cache_creation_input_tokens.unwrap_or(0);
                }

                // Count tool invocations
                for content in &assistant.message.content {
                    if matches!(content, ContentBlock::ToolUse(_)) {
                        block.tool_invocations += 1;
                    }
                }
            }
        }
    }

    // Calculate total tokens and estimated cost for each block
    for block in blocks.values_mut() {
        block.total_tokens = block.input_tokens + block.output_tokens;

        // Simple cost estimation (Claude 3.5 Sonnet pricing as baseline)
        // Input: $3/MTok, Output: $15/MTok, Cache Read: $0.30/MTok, Cache Write: $3.75/MTok
        let input_cost = (block.input_tokens as f64 / 1_000_000.0) * 3.0;
        let output_cost = (block.output_tokens as f64 / 1_000_000.0) * 15.0;
        let cache_read_cost = (block.cache_read_tokens as f64 / 1_000_000.0) * 0.30;
        let cache_write_cost = (block.cache_creation_tokens as f64 / 1_000_000.0) * 3.75;
        let total_cost = input_cost + output_cost + cache_read_cost + cache_write_cost;
        if total_cost > 0.0 {
            block.estimated_cost = Some(total_cost);
        }
    }

    blocks.into_values().collect()
}

/// Output blocks for JSON serialization.
#[derive(Debug, serde::Serialize)]
struct BlocksOutput {
    blocks: Vec<BillingBlock>,
    total_blocks: usize,
    active_block: Option<BillingBlock>,
    summary: BlocksSummary,
}

/// Summary of billing blocks.
#[derive(Debug, serde::Serialize)]
struct BlocksSummary {
    total_tokens: u64,
    total_input_tokens: u64,
    total_output_tokens: u64,
    total_messages: usize,
    total_tool_invocations: usize,
    estimated_cost: Option<f64>,
    highest_block_tokens: u64,
    /// Sparkline visualization of token usage across blocks.
    #[serde(skip_serializing_if = "Option::is_none")]
    token_sparkline: Option<String>,
    /// Sparkline visualization of message counts across blocks.
    #[serde(skip_serializing_if = "Option::is_none")]
    message_sparkline: Option<String>,
}

/// Format a model name for display, adding explanatory notes for special values.
fn format_model_name(model: &str) -> String {
    match model {
        "<synthetic>" => "(synthetic - internal/cached response)".to_string(),
        "" => "(unknown model)".to_string(),
        m => m.to_string(),
    }
}

/// Compute statistics in parallel across multiple sessions.
fn compute_stats_parallel(sessions: &[Session], max_file_size: Option<u64>) -> ProjectAnalytics {
    // Process sessions in parallel and collect individual analytics
    let session_analytics: Vec<SessionAnalytics> = sessions
        .par_iter()
        .filter_map(|session| {
            session.parse_with_options(max_file_size).ok().and_then(|entries| {
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

    // Handle cost history clear
    if args.clear_history {
        let mut history = CostHistory::load()?;
        history.clear();
        history.save()?;
        if !cli.quiet {
            println!("Cost history cleared.");
        }
        return Ok(());
    }

    // Handle cost history CSV export
    if args.csv {
        let history = CostHistory::load()?;
        if history.is_empty() {
            if !cli.quiet {
                println!("No cost history recorded. Use 'snatch stats --record' to record data.");
            }
            return Ok(());
        }
        println!("{}", history.to_csv());
        return Ok(());
    }

    // Handle record to history
    if args.record {
        let all_sessions = claude_dir.all_sessions()?;
        let global_analytics = compute_stats_parallel(&all_sessions, cli.max_file_size);

        let today = Utc::now().date_naive();
        let mut history = CostHistory::load()?;

        let point = CostDataPoint::with_values(
            today,
            global_analytics.total_usage.usage.total_tokens(),
            global_analytics.total_usage.usage.total_input_tokens(),
            global_analytics.total_usage.usage.output_tokens,
            global_analytics.total_usage.usage.cache_read_input_tokens.unwrap_or(0),
            global_analytics.total_usage.estimated_cost.unwrap_or(0.0),
            global_analytics.session_count,
            global_analytics.message_counts.total(),
        );

        history.record(point);
        history.save()?;

        if !cli.quiet {
            println!(
                "Recorded today's stats: {} tokens, ${:.4} estimated cost",
                format_number(global_analytics.total_usage.usage.total_tokens()),
                global_analytics.total_usage.estimated_cost.unwrap_or(0.0)
            );
        }
        return Ok(());
    }

    // Handle cost history display
    if args.history || args.weekly || args.monthly {
        return output_cost_history(cli, args);
    }

    // Handle timeline visualization
    if args.timeline {
        let sessions = claude_dir.all_sessions()?;
        return output_timeline(cli, args, &sessions);
    }

    // Handle token usage graph
    if args.graph {
        let sessions = claude_dir.all_sessions()?;
        return output_token_graph(cli, args, &sessions);
    }

    // Handle billing blocks mode
    if args.blocks {
        let sessions: Vec<Session> = if let Some(session_id) = &args.session {
            // Blocks for specific session
            let session = claude_dir
                .find_session(session_id)?
                .ok_or_else(|| SnatchError::SessionNotFound {
                    session_id: session_id.clone(),
                })?;
            vec![session]
        } else if let Some(project_filters) = &args.project {
            // Blocks for specific project(s)
            let projects = claude_dir.projects()?;
            let matching_projects: Vec<_> = projects
                .iter()
                .filter(|p| {
                    let path = p.decoded_path();
                    project_filters.iter().any(|filter| path.contains(filter))
                })
                .collect();

            if matching_projects.is_empty() {
                return Err(SnatchError::ProjectNotFound {
                    project_path: project_filters.join(", "),
                });
            }

            let mut all_sessions = Vec::new();
            for project in matching_projects {
                all_sessions.extend(project.sessions()?);
            }
            all_sessions
        } else {
            // Global blocks across all sessions
            claude_dir.all_sessions()?
        };

        return output_blocks_stats(cli, args, &sessions);
    }

    if let Some(session_id) = &args.session {
        // Stats for specific session
        let session = claude_dir
            .find_session(session_id)?
            .ok_or_else(|| SnatchError::SessionNotFound {
                session_id: session_id.clone(),
            })?;

        let entries = session.parse_with_options(cli.max_file_size)?;
        let conversation = Conversation::from_entries(entries)?;
        let analytics = SessionAnalytics::from_conversation(&conversation);

        output_session_stats(cli, args, &analytics)?;
    } else if let Some(project_filters) = &args.project {
        // Stats for specific project(s)
        let projects = claude_dir.projects()?;

        // Find all projects matching any of the filters
        let matching_projects: Vec<_> = projects
            .iter()
            .filter(|p| {
                let path = p.decoded_path();
                project_filters.iter().any(|filter| path.contains(filter))
            })
            .collect();

        if matching_projects.is_empty() {
            return Err(SnatchError::ProjectNotFound {
                project_path: project_filters.join(", "),
            });
        }

        // Single project - use original behavior
        if matching_projects.len() == 1 {
            let project = matching_projects[0];
            let sessions = project.sessions()?;
            let project_analytics = compute_stats_parallel(&sessions, cli.max_file_size);
            output_project_stats(cli, args, &project_analytics, project.decoded_path())?;
        } else {
            // Multiple projects - aggregate and show breakdown
            let mut all_sessions: Vec<Session> = Vec::new();
            let mut project_names: Vec<String> = Vec::new();

            for project in &matching_projects {
                all_sessions.extend(project.sessions()?);
                project_names.push(project.decoded_path().to_string());
            }

            let aggregate_analytics = compute_stats_parallel(&all_sessions, cli.max_file_size);
            output_multi_project_stats(cli, args, &aggregate_analytics, &project_names)?;
        }
    } else if args.global || args.models || args.costs || args.all {
        // Global stats across all sessions - parallel processing
        // Also show global stats when --models, --costs, or --all is specified without a scope,
        // since these flags require computing full statistics to be useful.
        let all_sessions = claude_dir.all_sessions()?;
        let global_analytics = compute_stats_parallel(&all_sessions, cli.max_file_size);

        output_global_stats(cli, args, &global_analytics)?;

        // Show budget status if configured
        output_budget_status(cli)?;
    } else {
        // Default: show summary of all projects
        output_overview(cli, args, &claude_dir)?;

        // Show budget status if configured
        output_budget_status(cli)?;
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

/// Output statistics for multiple projects (aggregated).
fn output_multi_project_stats(
    cli: &Cli,
    args: &StatsArgs,
    analytics: &ProjectAnalytics,
    project_names: &[String],
) -> Result<()> {
    let projects_label = format!("{} projects", project_names.len());

    match cli.effective_output() {
        OutputFormat::Json => {
            // Include project list in JSON output
            let mut output = serde_json::to_value(StatsOutput::from_project(analytics, &projects_label))?;
            if let Some(obj) = output.as_object_mut() {
                obj.insert("projects".to_string(), serde_json::json!(project_names));
            }
            println!("{}", serde_json::to_string_pretty(&output)?);
        }
        OutputFormat::Tsv => {
            println!("metric\tvalue");
            println!("project_count\t{}", project_names.len());
            println!("sessions\t{}", analytics.session_count);
            println!("total_tokens\t{}", analytics.total_usage.usage.total_tokens());
            println!("tool_invocations\t{}", analytics.message_counts.tool_uses);
            if let Some(cost) = analytics.total_usage.estimated_cost {
                println!("estimated_cost\t{cost:.4}");
            }
        }
        OutputFormat::Compact => {
            let cost = analytics.total_usage.estimated_cost.map(|c| format!("${c:.2}")).unwrap_or_else(|| "N/A".to_string());
            println!(
                "projects:{} sessions:{} tokens:{} cost:{}",
                project_names.len(),
                analytics.session_count,
                analytics.total_usage.usage.total_tokens(),
                cost
            );
        }
        OutputFormat::Text => {
            println!("Aggregate Statistics ({} projects)", project_names.len());
            println!("{}", "=".repeat(35));
            println!();

            // List projects
            println!("Projects:");
            for name in project_names {
                // Truncate long paths for display
                let display_name = if name.len() > 60 {
                    format!("...{}", &name[name.len().saturating_sub(57)..])
                } else {
                    name.clone()
                };
                println!("  - {display_name}");
            }
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

/// Output billing blocks statistics.
fn output_blocks_stats(cli: &Cli, args: &StatsArgs, sessions: &[Session]) -> Result<()> {
    let blocks = aggregate_billing_blocks(sessions, cli.max_file_size);

    if blocks.is_empty() {
        if !cli.quiet {
            println!("No billing blocks found.");
        }
        return Ok(());
    }

    // Find the active block and calculate summary
    let active_block = blocks.iter().find(|b| b.status == "active").cloned();
    let total_tokens: u64 = blocks.iter().map(|b| b.total_tokens).sum();
    let total_input_tokens: u64 = blocks.iter().map(|b| b.input_tokens).sum();
    let total_output_tokens: u64 = blocks.iter().map(|b| b.output_tokens).sum();
    let total_messages: usize = blocks.iter().map(|b| b.message_count).sum();
    let total_tool_invocations: usize = blocks.iter().map(|b| b.tool_invocations).sum();
    let total_cost: f64 = blocks.iter().filter_map(|b| b.estimated_cost).sum();
    let highest_block_tokens = blocks.iter().map(|b| b.total_tokens).max().unwrap_or(0);

    // Parse token limit for display
    let token_limit: Option<u64> = args.token_limit.as_ref().and_then(|s| {
        if s.to_lowercase() == "max" {
            Some(highest_block_tokens)
        } else {
            s.parse().ok()
        }
    });

    match cli.effective_output() {
        OutputFormat::Json => {
            // Generate sparklines for JSON output when flag is set and there's data
            let (token_sparkline, message_sparkline) = if args.sparkline && blocks.len() > 1 {
                let token_values: Vec<u64> = blocks.iter().map(|b| b.total_tokens).collect();
                let msg_values: Vec<u64> = blocks.iter().map(|b| b.message_count as u64).collect();
                (
                    Some(sparkline_u64(&token_values)),
                    Some(sparkline_u64(&msg_values)),
                )
            } else {
                (None, None)
            };

            let output = BlocksOutput {
                blocks: blocks.clone(),
                total_blocks: blocks.len(),
                active_block,
                summary: BlocksSummary {
                    total_tokens,
                    total_input_tokens,
                    total_output_tokens,
                    total_messages,
                    total_tool_invocations,
                    estimated_cost: if total_cost > 0.0 { Some(total_cost) } else { None },
                    highest_block_tokens,
                    token_sparkline,
                    message_sparkline,
                },
            };
            println!("{}", serde_json::to_string_pretty(&output)?);
        }
        OutputFormat::Tsv => {
            println!("block_start\tblock_end\tstatus\ttokens\tinput\toutput\tmessages\ttools\tcost");
            for block in &blocks {
                println!(
                    "{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}",
                    block.start.format("%Y-%m-%d %H:%M"),
                    block.end.format("%Y-%m-%d %H:%M"),
                    block.status,
                    block.total_tokens,
                    block.input_tokens,
                    block.output_tokens,
                    block.message_count,
                    block.tool_invocations,
                    block.estimated_cost.map(|c| format!("{c:.4}")).unwrap_or_default()
                );
            }
        }
        OutputFormat::Compact => {
            println!(
                "blocks:{} tokens:{} cost:${:.2}",
                blocks.len(),
                total_tokens,
                total_cost
            );
            if let Some(ref active) = active_block {
                if let Some(remaining) = active.remaining() {
                    let mins = remaining.num_minutes();
                    println!("active: {}h {}m remaining", mins / 60, mins % 60);
                }
            }
        }
        OutputFormat::Text => {
            println!("5-Hour Billing Windows");
            println!("======================");
            println!();

            // Show active block prominently if it exists
            if let Some(ref active) = active_block {
                println!("🟢 ACTIVE BLOCK");
                println!("  Period:    {} - {} UTC",
                    active.start.format("%Y-%m-%d %H:%M"),
                    active.end.format("%H:%M")
                );
                if let Some(remaining) = active.remaining() {
                    let hours = remaining.num_hours();
                    let mins = remaining.num_minutes() % 60;
                    println!("  Remaining: {}h {}m", hours, mins);
                }
                println!("  Tokens:    {} (in: {}, out: {})",
                    format_number(active.total_tokens),
                    format_number(active.input_tokens),
                    format_number(active.output_tokens)
                );

                // Show progress bar if token limit specified
                if let Some(limit) = token_limit {
                    let pct = (active.total_tokens as f64 / limit as f64 * 100.0).min(100.0);
                    let bar_width = 40;
                    let filled = ((pct / 100.0) * bar_width as f64) as usize;
                    let empty = bar_width - filled;
                    println!("  Usage:     [{}{}] {:.1}% of {}",
                        "█".repeat(filled),
                        "░".repeat(empty),
                        pct,
                        format_number(limit)
                    );
                }

                if let Some(cost) = active.estimated_cost {
                    println!("  Cost:      ${cost:.4}");
                }
                println!();
            }

            // Summary
            println!("Summary");
            println!("-------");
            println!("  Total Blocks:      {}", format_count(blocks.len()));
            println!("  Total Tokens:      {}", format_number(total_tokens));
            println!("  Highest Block:     {} tokens", format_number(highest_block_tokens));
            println!("  Total Messages:    {}", format_count(total_messages));
            println!("  Tool Invocations:  {}", format_count(total_tool_invocations));
            if total_cost > 0.0 {
                println!("  Estimated Cost:    ${total_cost:.4}");
            }

            // Sparkline visualizations
            if args.sparkline && blocks.len() > 1 {
                println!();
                println!("Usage Trends");
                println!("------------");

                // Token usage trend across blocks
                let token_values: Vec<u64> = blocks.iter().map(|b| b.total_tokens).collect();
                let token_spark = sparkline_with_range(
                    &token_values.iter().map(|&v| v as f64).collect::<Vec<_>>(),
                    None,
                );
                println!("  Tokens:    {token_spark}");

                // Message count trend
                let msg_values: Vec<u64> = blocks.iter().map(|b| b.message_count as u64).collect();
                let msg_spark = sparkline_u64(&msg_values);
                println!("  Messages:  {msg_spark}");

                // Tool invocations trend
                let tool_values: Vec<u64> = blocks.iter().map(|b| b.tool_invocations as u64).collect();
                if tool_values.iter().any(|&v| v > 0) {
                    let tool_spark = sparkline_u64(&tool_values);
                    println!("  Tools:     {tool_spark}");
                }

                // Cost trend if available
                let cost_values: Vec<f64> = blocks.iter().map(|b| b.estimated_cost.unwrap_or(0.0)).collect();
                if cost_values.iter().any(|&v| v > 0.0) {
                    let cost_spark = sparkline_with_range(&cost_values, None);
                    println!("  Cost:      {cost_spark}");
                }
            }
            println!();

            // Recent blocks (last 5)
            let recent_blocks: Vec<_> = blocks.iter().rev().take(5).collect();
            if !recent_blocks.is_empty() {
                println!("Recent Blocks");
                println!("-------------");
                for block in recent_blocks {
                    let status_icon = match block.status.as_str() {
                        "active" => "🟢",
                        "completed" => "✓",
                        "gap" => "○",
                        _ => " ",
                    };
                    println!(
                        "  {} {} - {}: {} tokens, {} msgs",
                        status_icon,
                        block.start.format("%m/%d %H:%M"),
                        block.end.format("%H:%M"),
                        format_number(block.total_tokens),
                        block.message_count
                    );
                }
            }
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

/// Output for cost history JSON serialization.
#[derive(Debug, serde::Serialize)]
struct CostHistoryOutput {
    period: String,
    days: i64,
    stats: crate::analytics::history::CostPeriodStats,
    #[serde(skip_serializing_if = "Option::is_none")]
    daily_sparkline: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    weekly: Option<Vec<WeeklyCostOutput>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    monthly: Option<Vec<MonthlyCostOutput>>,
}

/// Weekly cost output for JSON.
#[derive(Debug, serde::Serialize)]
struct WeeklyCostOutput {
    period: String,
    cost: f64,
}

/// Monthly cost output for JSON.
#[derive(Debug, serde::Serialize)]
struct MonthlyCostOutput {
    month: String,
    cost: f64,
}

/// Output cost history data.
fn output_cost_history(cli: &Cli, args: &StatsArgs) -> Result<()> {
    let history = CostHistory::load()?;

    if history.is_empty() {
        if !cli.quiet {
            println!("No cost history recorded.");
            println!();
            println!("To start tracking costs, run:");
            println!("  snatch stats --record");
            println!();
            println!("You can automate daily recording with a cron job or scheduler.");
        }
        return Ok(());
    }

    // Determine which view to show
    if args.weekly {
        return output_weekly_costs(cli, args, &history);
    }

    if args.monthly {
        return output_monthly_costs(cli, args, &history);
    }

    // Default: show daily history for the last N days
    let stats = history.stats_last_days(args.days);

    match cli.effective_output() {
        OutputFormat::Json => {
            let daily_sparkline = if args.sparkline && !stats.daily_costs.is_empty() {
                Some(sparkline_with_range(&stats.daily_costs, None))
            } else {
                None
            };

            let output = CostHistoryOutput {
                period: format!("last_{}_days", args.days),
                days: args.days,
                stats: stats.clone(),
                daily_sparkline,
                weekly: None,
                monthly: None,
            };
            println!("{}", serde_json::to_string_pretty(&output)?);
        }
        OutputFormat::Tsv => {
            println!("date\ttokens\tinput_tokens\toutput_tokens\tcache_read\tcost\tsessions\tmessages");
            let end = Utc::now().date_naive();
            let start = end - Duration::days(args.days - 1);

            let mut current = start;
            while current <= end {
                if let Some(point) = history.get(current) {
                    println!(
                        "{}\t{}\t{}\t{}\t{}\t{:.6}\t{}\t{}",
                        current,
                        point.tokens,
                        point.input_tokens,
                        point.output_tokens,
                        point.cache_read_tokens,
                        point.cost,
                        point.session_count,
                        point.message_count,
                    );
                }
                current += Duration::days(1);
            }
        }
        OutputFormat::Compact => {
            let trend = if stats.trend_direction > 0.0 {
                "↑"
            } else if stats.trend_direction < 0.0 {
                "↓"
            } else {
                "→"
            };
            println!(
                "days:{} cost:${:.2} avg:${:.2} trend:{}",
                stats.active_days,
                stats.total_cost,
                stats.avg_daily_cost,
                trend
            );
        }
        OutputFormat::Text => {
            println!("Cost History (Last {} Days)", args.days);
            println!("{}", "=".repeat(30));
            println!();

            // Summary stats
            println!("Summary");
            println!("-------");
            println!("  Active Days:     {:>8}", stats.active_days);
            println!("  Total Cost:      ${:>7.2}", stats.total_cost);
            println!("  Avg Daily Cost:  ${:>7.2}", stats.avg_daily_cost);
            println!("  Max Daily Cost:  ${:>7.2}", stats.max_daily_cost);
            println!("  Min Daily Cost:  ${:>7.2}", stats.min_daily_cost);
            println!("  Total Tokens:    {:>8}", format_number(stats.total_tokens));
            println!("  Total Sessions:  {:>8}", format_count(stats.total_sessions));

            // Trend indicator
            let trend_icon = if stats.trend_direction > 0.01 {
                "📈 Increasing"
            } else if stats.trend_direction < -0.01 {
                "📉 Decreasing"
            } else {
                "➡️  Stable"
            };
            println!("  Trend:           {}", trend_icon);
            println!();

            // Sparkline visualization
            if args.sparkline && !stats.daily_costs.is_empty() {
                println!("Daily Cost Trend");
                println!("----------------");
                let spark = sparkline_with_range(&stats.daily_costs, None);
                println!("  {}", spark);
                println!();

                // Token trend
                if !stats.daily_tokens.is_empty() {
                    let token_spark = sparkline_u64(&stats.daily_tokens);
                    println!("Daily Token Trend");
                    println!("-----------------");
                    println!("  {}", token_spark);
                    println!();
                }
            }

            // Show date range
            if let (Some(first), Some(last)) = (history.first_date(), history.last_date()) {
                println!("Data Range: {} to {}", first, last);
                println!("Total Days Recorded: {}", history.len());
            }
        }
    }

    Ok(())
}

/// Output weekly cost aggregation.
fn output_weekly_costs(cli: &Cli, args: &StatsArgs, history: &CostHistory) -> Result<()> {
    let weeks = (args.days / 7).max(4) as usize;
    let weekly_data = history.weekly_costs(weeks);

    if weekly_data.is_empty() {
        if !cli.quiet {
            println!("No weekly cost data available.");
        }
        return Ok(());
    }

    match cli.effective_output() {
        OutputFormat::Json => {
            let weekly: Vec<WeeklyCostOutput> = weekly_data
                .iter()
                .map(|(period, cost)| WeeklyCostOutput {
                    period: period.clone(),
                    cost: *cost,
                })
                .collect();

            let total_cost: f64 = weekly_data.iter().map(|(_, c)| c).sum();
            let output = serde_json::json!({
                "period": "weekly",
                "weeks": weeks,
                "data": weekly,
                "total_cost": total_cost,
            });
            println!("{}", serde_json::to_string_pretty(&output)?);
        }
        OutputFormat::Tsv => {
            println!("period\tcost");
            for (period, cost) in &weekly_data {
                println!("{}\t{:.4}", period, cost);
            }
        }
        OutputFormat::Compact => {
            let costs: Vec<f64> = weekly_data.iter().map(|(_, c)| *c).collect();
            let total: f64 = costs.iter().sum();
            if args.sparkline {
                let spark = sparkline_with_range(&costs, None);
                println!("weeks:{} total:${:.2} {}", weeks, total, spark);
            } else {
                println!("weeks:{} total:${:.2}", weeks, total);
            }
        }
        OutputFormat::Text => {
            println!("Weekly Cost Summary ({} Weeks)", weeks);
            println!("{}", "=".repeat(35));
            println!();

            let total_cost: f64 = weekly_data.iter().map(|(_, c)| c).sum();
            let avg_weekly = total_cost / weekly_data.len() as f64;

            // Find max for bar chart scaling
            let max_cost = weekly_data.iter().map(|(_, c)| *c).fold(0.0_f64, f64::max);

            for (period, cost) in &weekly_data {
                let bar_len = if max_cost > 0.0 {
                    ((cost / max_cost) * 20.0) as usize
                } else {
                    0
                };
                println!(
                    "  {} │ {:>7.2} │ {}",
                    period,
                    cost,
                    "█".repeat(bar_len)
                );
            }

            println!();
            println!("  Total:    ${:.2}", total_cost);
            println!("  Average:  ${:.2}/week", avg_weekly);

            // Sparkline
            if args.sparkline {
                let costs: Vec<f64> = weekly_data.iter().map(|(_, c)| *c).collect();
                let spark = sparkline_with_range(&costs, None);
                println!();
                println!("Trend: {}", spark);
            }
        }
    }

    Ok(())
}

/// Output monthly cost aggregation.
fn output_monthly_costs(cli: &Cli, args: &StatsArgs, history: &CostHistory) -> Result<()> {
    let months = (args.days / 30).max(6) as usize;
    let monthly_data = history.monthly_costs(months);

    if monthly_data.is_empty() {
        if !cli.quiet {
            println!("No monthly cost data available.");
        }
        return Ok(());
    }

    match cli.effective_output() {
        OutputFormat::Json => {
            let monthly: Vec<MonthlyCostOutput> = monthly_data
                .iter()
                .map(|(month, cost)| MonthlyCostOutput {
                    month: month.clone(),
                    cost: *cost,
                })
                .collect();

            let total_cost: f64 = monthly_data.iter().map(|(_, c)| c).sum();
            let output = serde_json::json!({
                "period": "monthly",
                "months": months,
                "data": monthly,
                "total_cost": total_cost,
            });
            println!("{}", serde_json::to_string_pretty(&output)?);
        }
        OutputFormat::Tsv => {
            println!("month\tcost");
            for (month, cost) in &monthly_data {
                println!("{}\t{:.4}", month, cost);
            }
        }
        OutputFormat::Compact => {
            let costs: Vec<f64> = monthly_data.iter().map(|(_, c)| *c).collect();
            let total: f64 = costs.iter().sum();
            if args.sparkline {
                let spark = sparkline_with_range(&costs, None);
                println!("months:{} total:${:.2} {}", months, total, spark);
            } else {
                println!("months:{} total:${:.2}", months, total);
            }
        }
        OutputFormat::Text => {
            println!("Monthly Cost Summary ({} Months)", months);
            println!("{}", "=".repeat(35));
            println!();

            let total_cost: f64 = monthly_data.iter().map(|(_, c)| c).sum();
            let avg_monthly = total_cost / monthly_data.len() as f64;

            // Find max for bar chart scaling
            let max_cost = monthly_data.iter().map(|(_, c)| *c).fold(0.0_f64, f64::max);

            for (month, cost) in &monthly_data {
                let bar_len = if max_cost > 0.0 {
                    ((cost / max_cost) * 25.0) as usize
                } else {
                    0
                };
                println!(
                    "  {} │ ${:>8.2} │ {}",
                    month,
                    cost,
                    "█".repeat(bar_len)
                );
            }

            println!();
            println!("  Total:    ${:.2}", total_cost);
            println!("  Average:  ${:.2}/month", avg_monthly);

            // Sparkline
            if args.sparkline {
                let costs: Vec<f64> = monthly_data.iter().map(|(_, c)| *c).collect();
                let spark = sparkline_with_range(&costs, None);
                println!();
                println!("Trend: {}", spark);
            }
        }
    }

    Ok(())
}

/// Timeline entry for visualization.
#[derive(Debug, Clone, serde::Serialize)]
struct TimelineEntry {
    period: String,
    session_count: usize,
    total_tokens: u64,
    message_count: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    estimated_cost: Option<f64>,
}

/// Timeline output for JSON serialization.
#[derive(Debug, serde::Serialize)]
struct TimelineOutput {
    granularity: String,
    entries: Vec<TimelineEntry>,
    total_sessions: usize,
    total_tokens: u64,
    date_range: (String, String),
}

/// Get timeline entries from sessions.
fn collect_timeline_entries(
    sessions: &[Session],
    granularity: &str,
    max_file_size: Option<u64>,
) -> Vec<TimelineEntry> {
    use std::collections::BTreeMap;

    let mut buckets: BTreeMap<String, TimelineEntry> = BTreeMap::new();

    for session in sessions {
        let entries = match session.parse_with_options(max_file_size) {
            Ok(e) => e,
            Err(_) => continue,
        };

        // Get session start time
        let start_time = entries.iter().find_map(|e| e.timestamp());
        let start_time = match start_time {
            Some(t) => t,
            None => continue,
        };

        // Determine bucket key based on granularity
        let bucket_key = match granularity {
            "hourly" => start_time.format("%Y-%m-%d %H:00").to_string(),
            "weekly" => {
                let week = start_time.iso_week();
                format!("{}-W{:02}", week.year(), week.week())
            }
            _ => start_time.format("%Y-%m-%d").to_string(), // daily default
        };

        // Calculate session metrics
        let conv = match Conversation::from_entries(entries) {
            Ok(c) => c,
            Err(_) => continue,
        };
        let analytics = SessionAnalytics::from_conversation(&conv);

        let entry = buckets.entry(bucket_key.clone()).or_insert_with(|| TimelineEntry {
            period: bucket_key,
            session_count: 0,
            total_tokens: 0,
            message_count: 0,
            estimated_cost: None,
        });

        entry.session_count += 1;
        entry.total_tokens += analytics.usage.usage.total_tokens();
        entry.message_count += analytics.message_counts.total();
        if let Some(cost) = analytics.usage.estimated_cost {
            entry.estimated_cost = Some(entry.estimated_cost.unwrap_or(0.0) + cost);
        }
    }

    buckets.into_values().collect()
}

/// Output activity timeline visualization.
fn output_timeline(cli: &Cli, args: &StatsArgs, sessions: &[Session]) -> Result<()> {
    let entries = collect_timeline_entries(sessions, &args.granularity, cli.max_file_size);

    if entries.is_empty() {
        if !cli.quiet {
            println!("No session activity found.");
        }
        return Ok(());
    }

    let total_sessions: usize = entries.iter().map(|e| e.session_count).sum();
    let total_tokens: u64 = entries.iter().map(|e| e.total_tokens).sum();
    let date_range = (
        entries.first().map(|e| e.period.clone()).unwrap_or_default(),
        entries.last().map(|e| e.period.clone()).unwrap_or_default(),
    );

    match cli.effective_output() {
        OutputFormat::Json => {
            let output = TimelineOutput {
                granularity: args.granularity.clone(),
                entries,
                total_sessions,
                total_tokens,
                date_range,
            };
            println!("{}", serde_json::to_string_pretty(&output)?);
        }
        OutputFormat::Tsv => {
            println!("period\tsessions\ttokens\tmessages\tcost");
            for entry in &entries {
                println!(
                    "{}\t{}\t{}\t{}\t{}",
                    entry.period,
                    entry.session_count,
                    entry.total_tokens,
                    entry.message_count,
                    entry.estimated_cost.map(|c| format!("{:.4}", c)).unwrap_or_default()
                );
            }
        }
        OutputFormat::Compact => {
            // Simple sparkline of activity
            let values: Vec<u64> = entries.iter().map(|e| e.session_count as u64).collect();
            let spark = sparkline_u64(&values);
            println!(
                "periods:{} sessions:{} tokens:{} {}",
                entries.len(),
                total_sessions,
                format_number(total_tokens),
                spark
            );
        }
        OutputFormat::Text => {
            let granularity_label = match args.granularity.as_str() {
                "hourly" => "Hourly",
                "weekly" => "Weekly",
                _ => "Daily",
            };

            println!("{} Activity Timeline", granularity_label);
            println!("{}", "=".repeat(30));
            println!();

            // Summary
            println!("Date Range: {} to {}", date_range.0, date_range.1);
            println!("Total Sessions: {}", format_count(total_sessions));
            println!("Total Tokens: {}", format_number(total_tokens));
            println!();

            // Find max session count for scaling
            let max_sessions = entries.iter().map(|e| e.session_count).max().unwrap_or(1);

            // Activity bars
            println!("Activity Timeline");
            println!("-----------------");

            for entry in &entries {
                let bar_len = ((entry.session_count as f64 / max_sessions as f64) * 30.0) as usize;
                let bar = "█".repeat(bar_len);
                let empty = "░".repeat(30 - bar_len);

                // Use different intensity indicator for high activity
                let intensity = if entry.session_count >= max_sessions {
                    "🔥"
                } else if entry.session_count as f64 >= max_sessions as f64 * 0.7 {
                    "●"
                } else if entry.session_count as f64 >= max_sessions as f64 * 0.3 {
                    "○"
                } else {
                    "·"
                };

                println!(
                    "{} {} │ {}{} │ {} sessions, {} tokens",
                    intensity,
                    entry.period,
                    bar,
                    empty,
                    entry.session_count,
                    format_number(entry.total_tokens)
                );

                // For daily view with many days, add separator every week
                if args.granularity == "daily" && entry.period.ends_with("-01") {
                    println!("  {:─<14}┼{:─<32}┤", "", "");
                }
            }

            // Sparkline summary
            if args.sparkline && entries.len() > 1 {
                println!();
                println!("Session Activity Trend");
                println!("----------------------");
                let session_values: Vec<u64> = entries.iter().map(|e| e.session_count as u64).collect();
                let spark = sparkline_u64(&session_values);
                println!("  Sessions: {}", spark);

                let token_values: Vec<u64> = entries.iter().map(|e| e.total_tokens).collect();
                let token_spark = sparkline_u64(&token_values);
                println!("  Tokens:   {}", token_spark);
            }

            // Heatmap legend
            println!();
            println!("Legend: 🔥 Peak  ● High  ○ Medium  · Low");
        }
    }

    Ok(())
}

/// Token breakdown entry for graph visualization.
#[derive(Debug, Clone, serde::Serialize)]
struct TokenBreakdown {
    period: String,
    input_tokens: u64,
    output_tokens: u64,
    cache_read_tokens: u64,
    cache_write_tokens: u64,
    total_tokens: u64,
}

/// Token graph output for JSON serialization.
#[derive(Debug, serde::Serialize)]
struct TokenGraphOutput {
    granularity: String,
    data: Vec<TokenBreakdown>,
    totals: TokenBreakdown,
}

/// Collect token breakdown data from sessions.
fn collect_token_breakdown(
    sessions: &[Session],
    granularity: &str,
    max_file_size: Option<u64>,
) -> Vec<TokenBreakdown> {
    use std::collections::BTreeMap;

    let mut buckets: BTreeMap<String, TokenBreakdown> = BTreeMap::new();

    for session in sessions {
        let entries = match session.parse_with_options(max_file_size) {
            Ok(e) => e,
            Err(_) => continue,
        };

        // Get session start time
        let start_time = entries.iter().find_map(|e| e.timestamp());
        let start_time = match start_time {
            Some(t) => t,
            None => continue,
        };

        // Determine bucket key based on granularity
        let bucket_key = match granularity {
            "hourly" => start_time.format("%Y-%m-%d %H:00").to_string(),
            "weekly" => {
                let week = start_time.iso_week();
                format!("{}-W{:02}", week.year(), week.week())
            }
            _ => start_time.format("%Y-%m-%d").to_string(), // daily default
        };

        // Calculate session metrics
        let conv = match Conversation::from_entries(entries) {
            Ok(c) => c,
            Err(_) => continue,
        };
        let analytics = SessionAnalytics::from_conversation(&conv);
        let usage = &analytics.usage.usage;

        let entry = buckets.entry(bucket_key.clone()).or_insert_with(|| TokenBreakdown {
            period: bucket_key,
            input_tokens: 0,
            output_tokens: 0,
            cache_read_tokens: 0,
            cache_write_tokens: 0,
            total_tokens: 0,
        });

        entry.input_tokens += usage.input_tokens;
        entry.output_tokens += usage.output_tokens;
        entry.cache_read_tokens += usage.cache_read_input_tokens.unwrap_or(0);
        entry.cache_write_tokens += usage.cache_creation_input_tokens.unwrap_or(0);
        entry.total_tokens += usage.total_tokens();
    }

    buckets.into_values().collect()
}

/// Output token usage graph visualization.
fn output_token_graph(cli: &Cli, args: &StatsArgs, sessions: &[Session]) -> Result<()> {
    let data = collect_token_breakdown(sessions, &args.granularity, cli.max_file_size);

    if data.is_empty() {
        if !cli.quiet {
            println!("No token data found.");
        }
        return Ok(());
    }

    // Calculate totals
    let totals = TokenBreakdown {
        period: "Total".to_string(),
        input_tokens: data.iter().map(|d| d.input_tokens).sum(),
        output_tokens: data.iter().map(|d| d.output_tokens).sum(),
        cache_read_tokens: data.iter().map(|d| d.cache_read_tokens).sum(),
        cache_write_tokens: data.iter().map(|d| d.cache_write_tokens).sum(),
        total_tokens: data.iter().map(|d| d.total_tokens).sum(),
    };

    match cli.effective_output() {
        OutputFormat::Json => {
            let output = TokenGraphOutput {
                granularity: args.granularity.clone(),
                data,
                totals,
            };
            println!("{}", serde_json::to_string_pretty(&output)?);
        }
        OutputFormat::Tsv => {
            println!("period\tinput\toutput\tcache_read\tcache_write\ttotal");
            for entry in &data {
                println!(
                    "{}\t{}\t{}\t{}\t{}\t{}",
                    entry.period,
                    entry.input_tokens,
                    entry.output_tokens,
                    entry.cache_read_tokens,
                    entry.cache_write_tokens,
                    entry.total_tokens
                );
            }
        }
        OutputFormat::Compact => {
            // Sparklines for each token type
            let input_vals: Vec<u64> = data.iter().map(|d| d.input_tokens).collect();
            let output_vals: Vec<u64> = data.iter().map(|d| d.output_tokens).collect();
            println!(
                "periods:{} in:{} out:{} input:{} output:{}",
                data.len(),
                format_number(totals.input_tokens),
                format_number(totals.output_tokens),
                sparkline_u64(&input_vals),
                sparkline_u64(&output_vals)
            );
        }
        OutputFormat::Text => {
            let granularity_label = match args.granularity.as_str() {
                "hourly" => "Hourly",
                "weekly" => "Weekly",
                _ => "Daily",
            };

            println!("{} Token Usage Graph", granularity_label);
            println!("{}", "=".repeat(35));
            println!();

            // Summary
            println!("Token Breakdown Summary");
            println!("-----------------------");
            println!("  Input Tokens:       {:>12}", format_number(totals.input_tokens));
            println!("  Output Tokens:      {:>12}", format_number(totals.output_tokens));
            println!("  Cache Read Tokens:  {:>12}", format_number(totals.cache_read_tokens));
            println!("  Cache Write Tokens: {:>12}", format_number(totals.cache_write_tokens));
            println!("  Total Tokens:       {:>12}", format_number(totals.total_tokens));
            println!();

            // Stacked bar chart
            let max_total = data.iter().map(|d| d.total_tokens).max().unwrap_or(1);
            let bar_width = args.graph_width.min(80).max(20);

            println!("Token Usage by Period");
            println!("{}", "-".repeat(bar_width + 25));

            for entry in &data {
                let scale = entry.total_tokens as f64 / max_total as f64;
                let total_bar_len = (scale * bar_width as f64) as usize;

                // Calculate proportions for stacked bar
                let input_ratio = entry.input_tokens as f64 / entry.total_tokens.max(1) as f64;
                let output_ratio = entry.output_tokens as f64 / entry.total_tokens.max(1) as f64;

                let input_len = (input_ratio * total_bar_len as f64) as usize;
                let output_len = (output_ratio * total_bar_len as f64) as usize;
                // Cache takes the remainder of the bar
                let cache_len = total_bar_len.saturating_sub(input_len + output_len);

                // Build stacked bar: Input (█), Output (▓), Cache (░)
                let input_bar = "█".repeat(input_len);
                let output_bar = "▓".repeat(output_len);
                let cache_bar = "░".repeat(cache_len);

                println!(
                    "  {:>14} │{}{}{}│ {}",
                    entry.period,
                    input_bar,
                    output_bar,
                    cache_bar,
                    format_number(entry.total_tokens)
                );
            }

            println!();
            println!("Legend: █ Input  ▓ Output  ░ Cache");

            // Trend sparklines
            if args.sparkline && data.len() > 1 {
                println!();
                println!("Token Trends (sparkline)");
                println!("------------------------");

                let input_vals: Vec<u64> = data.iter().map(|d| d.input_tokens).collect();
                let output_vals: Vec<u64> = data.iter().map(|d| d.output_tokens).collect();
                let total_vals: Vec<u64> = data.iter().map(|d| d.total_tokens).collect();

                println!("  Input:  {}", sparkline_u64(&input_vals));
                println!("  Output: {}", sparkline_u64(&output_vals));
                println!("  Total:  {}", sparkline_u64(&total_vals));
            }

            // Calculate and show ratios
            println!();
            println!("Token Distribution");
            println!("------------------");
            let input_pct = (totals.input_tokens as f64 / totals.total_tokens.max(1) as f64) * 100.0;
            let output_pct = (totals.output_tokens as f64 / totals.total_tokens.max(1) as f64) * 100.0;
            let cache_pct = ((totals.cache_read_tokens + totals.cache_write_tokens) as f64
                / totals.total_tokens.max(1) as f64) * 100.0;

            println!("  Input:  {:.1}%", input_pct);
            println!("  Output: {:.1}%", output_pct);
            println!("  Cache:  {:.1}%", cache_pct);

            // Input/Output ratio
            if totals.output_tokens > 0 {
                let io_ratio = totals.input_tokens as f64 / totals.output_tokens as f64;
                println!("  I/O Ratio: {:.2}:1", io_ratio);
            }
        }
    }

    Ok(())
}

/// Display budget status if configured.
fn output_budget_status(cli: &Cli) -> Result<()> {
    let config = Config::load().unwrap_or_default();

    if !config.budget.has_limits() || !config.budget.show_in_stats {
        return Ok(());
    }

    // Load cost history to get current spending
    let history = CostHistory::load()?;

    // Get today's cost
    let today = Utc::now().date_naive();
    let today_cost = history.get(today).map(|p| p.cost).unwrap_or(0.0);

    // Get this week's cost
    let week_stats = history.stats_this_week();
    let week_cost = week_stats.total_cost;

    // Get this month's cost
    let month_stats = history.stats_this_month();
    let month_cost = month_stats.total_cost;

    // Check against budgets
    let status = config.budget.check(today_cost, week_cost, month_cost);

    // Only display if there are any alerts or in text mode with limits
    let has_alerts = status.any_exceeded() || status.any_warning();

    match cli.effective_output() {
        OutputFormat::Json => {
            if has_alerts {
                let alerts: Vec<_> = status.alerts().iter().map(|a| {
                    serde_json::json!({
                        "period": a.period,
                        "spent": a.spent,
                        "limit": a.limit,
                        "percent_used": a.percent_used * 100.0,
                        "remaining": a.remaining,
                        "status": a.status_indicator(),
                    })
                }).collect();
                println!("{}", serde_json::to_string_pretty(&serde_json::json!({
                    "budget_alerts": alerts
                }))?);
            }
        }
        OutputFormat::Tsv => {
            if has_alerts {
                println!("period\tspent\tlimit\tpercent_used\tstatus");
                for alert in status.alerts() {
                    println!("{}\t{:.2}\t{:.2}\t{:.1}\t{}",
                        alert.period,
                        alert.spent,
                        alert.limit,
                        alert.percent_used * 100.0,
                        alert.status_indicator()
                    );
                }
            }
        }
        OutputFormat::Compact => {
            for alert in status.alerts() {
                println!("{}:{} ${:.2}/${:.2} ({:.0}%)",
                    alert.status_indicator(),
                    alert.period.to_lowercase(),
                    alert.spent,
                    alert.limit,
                    alert.percent_used * 100.0
                );
            }
        }
        OutputFormat::Text => {
            if has_alerts || config.budget.has_limits() {
                println!();
                println!("Budget Status");
                println!("-------------");

                // Show each configured limit
                let use_color = cli.effective_color();
                if let Some(ref daily) = status.daily {
                    let bar = progress_bar(daily.percent_used, 20);
                    println!("  Daily:   ${:>7.2} / ${:.2} [{bar}] {}",
                        daily.spent, daily.limit, daily.colored_status(use_color));
                }

                if let Some(ref weekly) = status.weekly {
                    let bar = progress_bar(weekly.percent_used, 20);
                    println!("  Weekly:  ${:>7.2} / ${:.2} [{bar}] {}",
                        weekly.spent, weekly.limit, weekly.colored_status(use_color));
                }

                if let Some(ref monthly) = status.monthly {
                    let bar = progress_bar(monthly.percent_used, 20);
                    println!("  Monthly: ${:>7.2} / ${:.2} [{bar}] {}",
                        monthly.spent, monthly.limit, monthly.colored_status(use_color));
                }

                if status.any_exceeded() {
                    eprintln!();
                    eprintln!("  ⚠️  One or more budgets exceeded!");
                } else if status.any_warning() {
                    eprintln!();
                    eprintln!("  ⚠️  Approaching budget limit (>{:.0}% threshold)",
                        config.budget.warning_threshold * 100.0);
                }
            }
        }
    }

    Ok(())
}

/// Create a simple progress bar.
fn progress_bar(percent: f64, width: usize) -> String {
    let filled = ((percent.min(1.0)) * width as f64) as usize;
    let empty = width.saturating_sub(filled);

    if percent > 1.0 {
        // Over budget - show overflow
        format!("{}!", "█".repeat(width))
    } else {
        format!("{}{}", "█".repeat(filled), "░".repeat(empty))
    }
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
