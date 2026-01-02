//! Summary command implementation.
//!
//! Shows a quick overview of Claude Code usage without launching the full TUI.

use chrono::{Duration, Utc};
use rayon::prelude::*;

use crate::analytics::{ProjectAnalytics, SessionAnalytics};
use crate::cli::{Cli, OutputFormat, SummaryArgs};
use crate::discovery::{format_count, format_number};
use crate::error::{Result, SnatchError};
use crate::reconstruction::Conversation;

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

    let amount: i64 = s_lower[..numeric_end].parse().map_err(|_| {
        SnatchError::InvalidArgument {
            name: "period".to_string(),
            reason: format!("Invalid number in period: {}", &s_lower[..numeric_end]),
        }
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
    messages: usize,
    tool_invocations: usize,
    estimated_cost: Option<f64>,
    top_projects: Vec<ProjectInfo>,
}

/// Project info for summary.
#[derive(Debug, serde::Serialize)]
struct ProjectInfo {
    path: String,
    sessions: usize,
}

/// Run the summary command.
pub fn run(cli: &Cli, args: &SummaryArgs) -> Result<()> {
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
    let mut project_counts: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    for session in &recent_sessions {
        let project = session.project_path().to_string();
        *project_counts.entry(project).or_insert(0) += 1;
    }

    // Get top projects
    let mut top_projects: Vec<_> = project_counts.into_iter().collect();
    top_projects.sort_by(|a, b| b.1.cmp(&a.1));
    let top_projects: Vec<ProjectInfo> = top_projects
        .into_iter()
        .take(5)
        .map(|(path, sessions)| ProjectInfo { path, sessions })
        .collect();

    // Compute aggregate analytics
    let session_analytics: Vec<SessionAnalytics> = recent_sessions
        .par_iter()
        .filter_map(|session| {
            session.parse_with_options(cli.max_file_size).ok().and_then(|entries| {
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

    let num_projects = top_projects.len();
    let num_sessions = recent_sessions.len();

    match cli.effective_output() {
        OutputFormat::Json => {
            let output = SummaryOutput {
                period: args.period.clone(),
                projects: num_projects,
                sessions: num_sessions,
                total_tokens: combined.total_usage.usage.total_tokens(),
                input_tokens: combined.total_usage.usage.total_input_tokens(),
                output_tokens: combined.total_usage.usage.output_tokens,
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
            println!("total_tokens\t{}", combined.total_usage.usage.total_tokens());
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
                format_number(combined.total_usage.usage.total_tokens()),
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
                format_number(combined.total_usage.usage.total_tokens())
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
                    // Truncate long paths
                    let display_path = if project.path.len() > 40 {
                        format!("...{}", &project.path[project.path.len() - 37..])
                    } else {
                        project.path.clone()
                    };
                    println!("  {:40} {} sessions", display_path, project.sessions);
                }
                println!();
            }

            // Quick tips
            println!("Quick Commands");
            println!("--------------");
            println!("  snatch recent          Show recent sessions");
            println!("  snatch stats --global  Detailed statistics");
            println!("  snatch tui             Interactive browser");
        }
    }

    Ok(())
}
