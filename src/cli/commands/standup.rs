//! Standup command implementation.
//!
//! Generates a summary report of recent Claude Code activity,
//! suitable for daily standups or progress reports.

use std::collections::HashMap;
use std::fmt::Write as FmtWrite;
use std::time::SystemTime;

use chrono::{DateTime, Duration, Utc};

use crate::analytics::SessionAnalytics;
use crate::cli::{Cli, StandupArgs, StandupFormat};
use crate::discovery::{format_count, format_number};
use crate::error::{Result, SnatchError};
use crate::model::{ContentBlock, LogEntry, ToolUse};
use crate::reconstruction::Conversation;

use super::get_claude_dir;

/// Standup report data structure.
#[derive(Debug, Clone, serde::Serialize)]
struct StandupReport {
    /// Time period covered by the report.
    period: String,
    /// Start of the period.
    #[serde(with = "chrono::serde::ts_seconds")]
    period_start: DateTime<Utc>,
    /// End of the period (now).
    #[serde(with = "chrono::serde::ts_seconds")]
    period_end: DateTime<Utc>,
    /// Projects worked on with session counts.
    projects: Vec<ProjectSummary>,
    /// Total sessions across all projects.
    total_sessions: usize,
    /// Token usage summary.
    #[serde(skip_serializing_if = "Option::is_none")]
    usage: Option<UsageSummary>,
    /// File modifications summary.
    #[serde(skip_serializing_if = "Option::is_none")]
    files: Option<FilesSummary>,
    /// Tool usage breakdown.
    #[serde(skip_serializing_if = "Option::is_none")]
    tools: Option<ToolsSummary>,
}

/// Summary for a single project.
#[derive(Debug, Clone, serde::Serialize)]
struct ProjectSummary {
    /// Project path (decoded).
    path: String,
    /// Number of sessions in this project.
    sessions: usize,
    /// Key accomplishments extracted from tool usage.
    accomplishments: Vec<String>,
}

/// Token usage summary.
#[derive(Debug, Clone, serde::Serialize)]
struct UsageSummary {
    total_tokens: u64,
    input_tokens: u64,
    output_tokens: u64,
    estimated_cost: Option<f64>,
}

/// File modifications summary.
#[derive(Debug, Clone, serde::Serialize)]
struct FilesSummary {
    files_created: usize,
    files_modified: usize,
    files_read: usize,
    unique_files: Vec<String>,
}

/// Tool usage breakdown.
#[derive(Debug, Clone, serde::Serialize)]
struct ToolsSummary {
    total_invocations: usize,
    by_tool: Vec<(String, usize)>,
}

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

    let unit = if numeric_end < s_lower.len() {
        &s_lower[numeric_end..]
    } else {
        "d" // Default to days
    };

    let duration = match unit {
        "h" | "hour" | "hours" => Duration::hours(amount),
        "d" | "day" | "days" => Duration::days(amount),
        "w" | "week" | "weeks" => Duration::weeks(amount),
        "m" | "month" | "months" => Duration::days(amount * 30),
        _ => {
            return Err(SnatchError::InvalidArgument {
                name: "period".to_string(),
                reason: format!(
                    "Unknown time unit '{}'. Use h/d/w (hours/days/weeks)",
                    unit
                ),
            })
        }
    };

    Ok(duration)
}

/// Extract accomplishments from tool usage.
fn extract_accomplishments(tool_uses: &[ToolUse]) -> Vec<String> {
    let mut accomplishments = Vec::new();
    let mut files_written: HashMap<String, usize> = HashMap::new();
    let mut files_edited: HashMap<String, usize> = HashMap::new();
    let mut tests_run = 0;
    let mut commits_made = 0;
    let mut searches_done = 0;

    for tool in tool_uses {
        match tool.name.as_str() {
            "Write" => {
                if let Some(path) = tool.input.get("file_path").and_then(|v| v.as_str()) {
                    let filename = std::path::Path::new(path)
                        .file_name()
                        .map(|s| s.to_string_lossy().to_string())
                        .unwrap_or_else(|| path.to_string());
                    *files_written.entry(filename).or_insert(0) += 1;
                }
            }
            "Edit" => {
                if let Some(path) = tool.input.get("file_path").and_then(|v| v.as_str()) {
                    let filename = std::path::Path::new(path)
                        .file_name()
                        .map(|s| s.to_string_lossy().to_string())
                        .unwrap_or_else(|| path.to_string());
                    *files_edited.entry(filename).or_insert(0) += 1;
                }
            }
            "Bash" => {
                if let Some(cmd) = tool.input.get("command").and_then(|v| v.as_str()) {
                    if cmd.contains("test") || cmd.contains("cargo test") || cmd.contains("pytest") || cmd.contains("npm test") {
                        tests_run += 1;
                    }
                    if cmd.contains("git commit") {
                        commits_made += 1;
                    }
                }
            }
            "Grep" | "Glob" | "Read" => {
                searches_done += 1;
            }
            _ => {}
        }
    }

    // Generate accomplishment strings
    if !files_written.is_empty() {
        if files_written.len() <= 3 {
            let names: Vec<&str> = files_written.keys().map(|s| s.as_str()).collect();
            accomplishments.push(format!("Created {}", names.join(", ")));
        } else {
            accomplishments.push(format!("Created {} files", files_written.len()));
        }
    }

    if !files_edited.is_empty() {
        if files_edited.len() <= 3 {
            let names: Vec<&str> = files_edited.keys().map(|s| s.as_str()).collect();
            accomplishments.push(format!("Modified {}", names.join(", ")));
        } else {
            accomplishments.push(format!("Modified {} files", files_edited.len()));
        }
    }

    if tests_run > 0 {
        accomplishments.push(format!("Ran {} test suite(s)", tests_run));
    }

    if commits_made > 0 {
        accomplishments.push(format!("Made {} commit(s)", commits_made));
    }

    if searches_done > 5 {
        accomplishments.push("Code exploration and research".to_string());
    }

    accomplishments
}

/// Run the standup command.
pub fn run(cli: &Cli, args: &StandupArgs) -> Result<()> {
    let claude_dir = get_claude_dir(cli.claude_dir.as_ref())?;

    // Parse period
    let duration = parse_period(&args.period)?;
    let period_start = Utc::now() - duration;
    let period_end = Utc::now();
    let cutoff = SystemTime::from(period_start);

    // Get all projects
    let projects = claude_dir.projects()?;

    // Filter and aggregate data
    let mut project_summaries: Vec<ProjectSummary> = Vec::new();
    let mut total_sessions = 0;
    let mut total_tokens: u64 = 0;
    let mut total_input: u64 = 0;
    let mut total_output: u64 = 0;
    let mut total_cost: f64 = 0.0;
    let mut tool_counts: HashMap<String, usize> = HashMap::new();
    let mut all_files: HashMap<String, FileAction> = HashMap::new();

    #[derive(Clone, Copy, PartialEq, Eq)]
    enum FileAction {
        Created,
        Modified,
        Read,
    }

    for project in projects {
        // Apply project filter if specified
        if let Some(ref filter) = args.project {
            if !project.decoded_path().contains(filter) {
                continue;
            }
        }

        let sessions = project.sessions()?;
        let mut project_sessions = 0;
        let mut project_tool_uses: Vec<ToolUse> = Vec::new();

        for session in sessions {
            // Check if session is within the time period
            let modified = session.modified_time();
            if modified < cutoff {
                continue;
            }

            project_sessions += 1;
            total_sessions += 1;

            // Parse session for detailed analytics
            if let Ok(entries) = session.parse_with_options(cli.max_file_size) {
                if let Ok(conversation) = Conversation::from_entries(entries.clone()) {
                    let analytics = SessionAnalytics::from_conversation(&conversation);
                    let summary = analytics.summary_report();

                    if args.usage || args.all {
                        total_tokens += summary.total_tokens;
                        total_input += summary.input_tokens;
                        total_output += summary.output_tokens;
                        if let Some(cost) = summary.estimated_cost {
                            total_cost += cost;
                        }
                    }

                    if args.tools || args.all {
                        for (tool, count) in analytics.top_tools(50) {
                            *tool_counts.entry(tool.to_string()).or_insert(0) += count;
                        }
                    }
                }

                // Extract tool uses for accomplishments and file tracking
                for entry in entries {
                    if let LogEntry::Assistant(assistant) = entry {
                        for content in &assistant.message.content {
                            if let ContentBlock::ToolUse(tool_use) = content {
                                project_tool_uses.push(tool_use.clone());

                                // Track file operations
                                if args.files || args.all {
                                    if let Some(path) = tool_use.input.get("file_path").and_then(|v| v.as_str()) {
                                        let action = match tool_use.name.as_str() {
                                            "Write" => FileAction::Created,
                                            "Edit" => FileAction::Modified,
                                            "Read" => FileAction::Read,
                                            _ => continue,
                                        };
                                        // Upgrade action if already tracked
                                        let entry = all_files.entry(path.to_string()).or_insert(action);
                                        if *entry == FileAction::Read && action != FileAction::Read {
                                            *entry = action;
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }

        if project_sessions > 0 {
            let accomplishments = extract_accomplishments(&project_tool_uses);
            project_summaries.push(ProjectSummary {
                path: project.decoded_path().to_string(),
                sessions: project_sessions,
                accomplishments,
            });
        }
    }

    // Sort projects by session count (most active first)
    project_summaries.sort_by(|a, b| b.sessions.cmp(&a.sessions));

    // Build usage summary
    let usage = if (args.usage || args.all) && total_tokens > 0 {
        Some(UsageSummary {
            total_tokens,
            input_tokens: total_input,
            output_tokens: total_output,
            estimated_cost: if total_cost > 0.0 { Some(total_cost) } else { None },
        })
    } else {
        None
    };

    // Build files summary
    let files = if (args.files || args.all) && !all_files.is_empty() {
        let files_created = all_files.values().filter(|&&a| a == FileAction::Created).count();
        let files_modified = all_files.values().filter(|&&a| a == FileAction::Modified).count();
        let files_read = all_files.values().filter(|&&a| a == FileAction::Read).count();

        // Get unique file names (not full paths)
        let mut unique_files: Vec<String> = all_files
            .keys()
            .filter_map(|p| {
                std::path::Path::new(p)
                    .file_name()
                    .map(|s| s.to_string_lossy().to_string())
            })
            .collect();
        unique_files.sort();
        unique_files.dedup();
        unique_files.truncate(10); // Limit to top 10

        Some(FilesSummary {
            files_created,
            files_modified,
            files_read,
            unique_files,
        })
    } else {
        None
    };

    // Build tools summary
    let tools = if (args.tools || args.all) && !tool_counts.is_empty() {
        let total_invocations = tool_counts.values().sum();
        let mut by_tool: Vec<(String, usize)> = tool_counts.into_iter().collect();
        by_tool.sort_by(|a, b| b.1.cmp(&a.1));
        by_tool.truncate(10); // Top 10 tools

        Some(ToolsSummary {
            total_invocations,
            by_tool,
        })
    } else {
        None
    };

    // Build the report
    let report = StandupReport {
        period: args.period.clone(),
        period_start,
        period_end,
        projects: project_summaries,
        total_sessions,
        usage,
        files,
        tools,
    };

    // Format and output
    let output = format_report(&report, args.format)?;

    // Handle clipboard
    if args.clipboard {
        match arboard::Clipboard::new() {
            Ok(mut clipboard) => {
                if let Err(e) = clipboard.set_text(&output) {
                    eprintln!("Warning: Failed to copy to clipboard: {e}");
                } else if !cli.quiet {
                    eprintln!("Copied to clipboard.");
                }
            }
            Err(e) => {
                eprintln!("Warning: Failed to access clipboard: {e}");
            }
        }
    }

    println!("{output}");

    Ok(())
}

/// Format the report based on the requested format.
fn format_report(report: &StandupReport, format: StandupFormat) -> Result<String> {
    match format {
        StandupFormat::Json => {
            Ok(serde_json::to_string_pretty(report)?)
        }
        StandupFormat::Text => format_text(report),
        StandupFormat::Markdown => format_markdown(report),
    }
}

/// Format report as plain text.
fn format_text(report: &StandupReport) -> Result<String> {
    let mut output = String::new();

    writeln!(output, "Standup Report ({})", report.period).ok();
    writeln!(output, "{}", "=".repeat(40)).ok();
    writeln!(output).ok();

    if report.projects.is_empty() {
        writeln!(output, "No activity in the last {}.", report.period).ok();
        return Ok(output);
    }

    writeln!(output, "Sessions: {}", format_count(report.total_sessions)).ok();
    writeln!(output, "Projects: {}", format_count(report.projects.len())).ok();
    writeln!(output).ok();

    // Projects
    writeln!(output, "Projects Worked On:").ok();
    writeln!(output, "-------------------").ok();
    for project in &report.projects {
        let name = project.path.split('/').last().unwrap_or(&project.path);
        writeln!(output, "  {} ({} sessions)", name, project.sessions).ok();
        for accomplishment in &project.accomplishments {
            writeln!(output, "    - {accomplishment}").ok();
        }
    }
    writeln!(output).ok();

    // Usage
    if let Some(ref usage) = report.usage {
        writeln!(output, "Token Usage:").ok();
        writeln!(output, "------------").ok();
        writeln!(output, "  Total:  {} tokens", format_number(usage.total_tokens)).ok();
        writeln!(output, "  Input:  {} tokens", format_number(usage.input_tokens)).ok();
        writeln!(output, "  Output: {} tokens", format_number(usage.output_tokens)).ok();
        if let Some(cost) = usage.estimated_cost {
            writeln!(output, "  Cost:   ${cost:.2}").ok();
        }
        writeln!(output).ok();
    }

    // Files
    if let Some(ref files) = report.files {
        writeln!(output, "File Operations:").ok();
        writeln!(output, "----------------").ok();
        writeln!(output, "  Created:  {}", format_count(files.files_created)).ok();
        writeln!(output, "  Modified: {}", format_count(files.files_modified)).ok();
        writeln!(output, "  Read:     {}", format_count(files.files_read)).ok();
        if !files.unique_files.is_empty() {
            writeln!(output, "  Files: {}", files.unique_files.join(", ")).ok();
        }
        writeln!(output).ok();
    }

    // Tools
    if let Some(ref tools) = report.tools {
        writeln!(output, "Tool Usage ({} total):", format_count(tools.total_invocations)).ok();
        writeln!(output, "------------------------").ok();
        for (tool, count) in &tools.by_tool {
            writeln!(output, "  {tool}: {}", format_count(*count)).ok();
        }
        writeln!(output).ok();
    }

    Ok(output)
}

/// Format report as Markdown (for Slack/Teams).
fn format_markdown(report: &StandupReport) -> Result<String> {
    let mut output = String::new();

    writeln!(output, "## Standup Report ({})\n", report.period).ok();

    if report.projects.is_empty() {
        writeln!(output, "*No activity in the last {}.*", report.period).ok();
        return Ok(output);
    }

    writeln!(output, "**Sessions:** {} | **Projects:** {}\n",
        format_count(report.total_sessions),
        format_count(report.projects.len())
    ).ok();

    // Projects
    writeln!(output, "### What I worked on\n").ok();
    for project in &report.projects {
        let name = project.path.split('/').last().unwrap_or(&project.path);
        writeln!(output, "- **{}** ({} sessions)", name, project.sessions).ok();
        for accomplishment in &project.accomplishments {
            writeln!(output, "  - {accomplishment}").ok();
        }
    }
    writeln!(output).ok();

    // Usage
    if let Some(ref usage) = report.usage {
        write!(output, "**Tokens:** {} total", format_number(usage.total_tokens)).ok();
        if let Some(cost) = usage.estimated_cost {
            write!(output, " (${cost:.2})").ok();
        }
        writeln!(output, "\n").ok();
    }

    // Files summary
    if let Some(ref files) = report.files {
        write!(output, "**Files:** ").ok();
        let mut parts = Vec::new();
        if files.files_created > 0 {
            parts.push(format!("{} created", files.files_created));
        }
        if files.files_modified > 0 {
            parts.push(format!("{} modified", files.files_modified));
        }
        writeln!(output, "{}\n", parts.join(", ")).ok();
    }

    // Tools (compact)
    if let Some(ref tools) = report.tools {
        let top_tools: Vec<_> = tools.by_tool.iter().take(5).map(|(t, c)| format!("{t}:{c}")).collect();
        writeln!(output, "**Top tools:** {}\n", top_tools.join(" | ")).ok();
    }

    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_period_hours() {
        let d = parse_period("24h").unwrap();
        assert_eq!(d.num_hours(), 24);
    }

    #[test]
    fn test_parse_period_days() {
        let d = parse_period("7d").unwrap();
        assert_eq!(d.num_days(), 7);
    }

    #[test]
    fn test_parse_period_weeks() {
        let d = parse_period("1w").unwrap();
        assert_eq!(d.num_weeks(), 1);
    }

    #[test]
    fn test_parse_period_default_unit() {
        // Should default to days
        let d = parse_period("3").unwrap();
        assert_eq!(d.num_days(), 3);
    }

    #[test]
    fn test_parse_period_invalid() {
        assert!(parse_period("invalid").is_err());
        assert!(parse_period("").is_err());
    }

    #[test]
    fn test_extract_accomplishments_empty() {
        let accomplishments = extract_accomplishments(&[]);
        assert!(accomplishments.is_empty());
    }
}
