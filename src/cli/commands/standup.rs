//! Standup command implementation.
//!
//! Generates a summary report of recent Claude Code activity,
//! suitable for daily standups or progress reports.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fmt::Write as FmtWrite;
use std::sync::Arc;
use std::time::SystemTime;

use chrono::{DateTime, Duration, Utc};

use crate::analytics::SessionAnalytics;
use crate::cli::{Cli, OutputFormat, StandupArgs, StandupFormat};
use crate::discovery::{format_count, format_number};
use crate::error::{Result, SnatchError};
use crate::model::{ContentBlock, LogEntry, ToolUse};
use crate::provider::{
    ActivityKind, FileChangeKind, FileChangeOutcome, FileChangeProjection, LogicalSessionKey,
    ParsedSession, ProviderPricing, ToolKind,
};
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

#[derive(Debug, serde::Serialize)]
struct ProviderStandupReport {
    period: String,
    #[serde(with = "chrono::serde::ts_seconds")]
    period_start: DateTime<Utc>,
    #[serde(with = "chrono::serde::ts_seconds")]
    period_end: DateTime<Utc>,
    period_basis: &'static str,
    providers: Vec<String>,
    projects: Vec<ProviderProjectSummary>,
    total_sessions: usize,
    session_descriptors_analyzed: usize,
    date_filter_fallback_descriptors: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    usage: Option<ProviderStandupUsage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    files: Option<ProviderFilesSummary>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tools: Option<ToolsSummary>,
    skipped_providers: Vec<ProviderStandupSkip>,
    warnings: Vec<String>,
    coverage_note: &'static str,
}

#[derive(Debug, serde::Serialize)]
struct ProviderProjectSummary {
    project_key: String,
    path: String,
    sessions: usize,
    accomplishments: Vec<String>,
}

#[derive(Debug, serde::Serialize)]
struct ProviderStandupUsage {
    work_tokens: u64,
    total_processed_tokens: u64,
    input_uncached_tokens: u64,
    cache_creation_tokens: u64,
    cache_read_tokens: u64,
    output_tokens: u64,
    estimated_cost: Option<f64>,
    pricing_coverage: &'static str,
    unpriced_providers: Vec<String>,
    unpriced_models: Vec<String>,
}

#[derive(Debug, serde::Serialize)]
struct ProviderFilesSummary {
    files_created: usize,
    files_modified: usize,
    files_deleted: usize,
    recognized_files_read: usize,
    unique_files: Vec<String>,
}

#[derive(Debug, serde::Serialize)]
struct ProviderStandupSkip {
    provider: String,
    reason: String,
}

#[derive(Default)]
struct ProviderProjectActivity {
    path: String,
    sessions: BTreeSet<LogicalSessionKey>,
    created: BTreeSet<String>,
    modified: BTreeSet<String>,
    deleted: BTreeSet<String>,
    reads: BTreeSet<String>,
    tests_run: usize,
    commits_made: usize,
    searches_done: usize,
}

#[derive(Default)]
struct ProviderStandupAggregate {
    projects: BTreeMap<String, ProviderProjectActivity>,
    project_roots: BTreeMap<String, BTreeSet<String>>,
    logical_sessions: BTreeSet<LogicalSessionKey>,
    descriptors_analyzed: usize,
    input_tokens: u64,
    cache_creation_tokens: u64,
    cache_read_tokens: u64,
    output_tokens: u64,
    estimated_cost: f64,
    has_estimated_cost: bool,
    unpriced_providers: BTreeSet<String>,
    unpriced_models: BTreeSet<String>,
    tool_counts: BTreeMap<String, usize>,
    files: crate::file_index::ProviderFileIndexBuilder,
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

    let amount: i64 = s_lower[..numeric_end]
        .parse()
        .map_err(|_| SnatchError::InvalidArgument {
            name: "period".to_string(),
            reason: format!("Invalid number in period: {}", &s_lower[..numeric_end]),
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
                reason: format!("Unknown time unit '{}'. Use h/d/w (hours/days/weeks)", unit),
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
                    if cmd.contains("test")
                        || cmd.contains("cargo test")
                        || cmd.contains("pytest")
                        || cmd.contains("npm test")
                    {
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

fn fallback_tool_kind(tool: &ToolUse) -> ToolKind {
    match tool.name.as_str() {
        "Bash" => ToolKind::Shell,
        "Read" => ToolKind::FileRead,
        "Write" | "Edit" | "MultiEdit" => ToolKind::FileWrite,
        "Grep" | "Glob" => ToolKind::Search,
        "WebSearch" | "WebFetch" => ToolKind::Web,
        "Agent" | "Task" => ToolKind::Subagent,
        name if name.starts_with("mcp__") => ToolKind::Mcp,
        name => ToolKind::Other(name.to_string()),
    }
}

fn tool_kind_for(
    parsed: &ParsedSession,
    entry: &crate::provider::EntryId,
    tool: &ToolUse,
) -> ToolKind {
    parsed
        .semantics
        .get(entry)
        .and_then(|semantics| semantics.tools.get(&tool.id))
        .map(|semantics| semantics.kind.clone())
        .unwrap_or_else(|| fallback_tool_kind(tool))
}

fn tool_kind_label(kind: &ToolKind, native_name: &str) -> String {
    match kind {
        ToolKind::Shell => "shell".to_string(),
        ToolKind::FileRead => "file-read".to_string(),
        ToolKind::FileWrite => "file-write".to_string(),
        ToolKind::Search => "search".to_string(),
        ToolKind::Web => "web".to_string(),
        ToolKind::Subagent => "subagent".to_string(),
        ToolKind::Mcp => "mcp".to_string(),
        ToolKind::Orchestration => "orchestration".to_string(),
        ToolKind::Other(label) if !label.is_empty() => label.clone(),
        ToolKind::Other(_) => native_name.to_string(),
    }
}

fn tool_command_text(tool: &ToolUse) -> Option<String> {
    for field in ["command", "cmd"] {
        let Some(value) = tool.input.get(field) else {
            continue;
        };
        if let Some(text) = value.as_str() {
            return Some(text.to_string());
        }
        if let Some(parts) = value.as_array() {
            let parts: Vec<_> = parts.iter().filter_map(serde_json::Value::as_str).collect();
            if !parts.is_empty() {
                return Some(parts.join(" "));
            }
        }
    }
    None
}

fn tool_file_path(tool: &ToolUse) -> Option<&str> {
    ["file_path", "path"]
        .into_iter()
        .find_map(|field| tool.input.get(field).and_then(serde_json::Value::as_str))
}

fn short_file_names(paths: &BTreeSet<String>) -> Vec<String> {
    paths
        .iter()
        .filter_map(|path| {
            std::path::Path::new(path)
                .file_name()
                .map(|name| name.to_string_lossy().to_string())
        })
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

fn summarize_file_accomplishment(verb: &str, paths: &BTreeSet<String>) -> Option<String> {
    if paths.is_empty() {
        return None;
    }
    let names = short_file_names(paths);
    if names.len() <= 3 {
        Some(format!("{verb} {}", names.join(", ")))
    } else {
        Some(format!("{verb} {} files", paths.len()))
    }
}

fn provider_project_accomplishments(project: &ProviderProjectActivity) -> Vec<String> {
    let mut accomplishments = Vec::new();
    if let Some(value) = summarize_file_accomplishment("Created", &project.created) {
        accomplishments.push(value);
    }
    if let Some(value) = summarize_file_accomplishment("Modified", &project.modified) {
        accomplishments.push(value);
    }
    if let Some(value) = summarize_file_accomplishment("Deleted", &project.deleted) {
        accomplishments.push(value);
    }
    if project.tests_run > 0 {
        accomplishments.push(format!("Ran {} test suite(s)", project.tests_run));
    }
    if project.commits_made > 0 {
        accomplishments.push(format!("Made {} commit(s)", project.commits_made));
    }
    if project.searches_done > 5 {
        accomplishments.push("Code exploration and research".to_string());
    }
    accomplishments
}

fn provider_files_summary(
    projects: &BTreeMap<String, ProviderProjectActivity>,
) -> ProviderFilesSummary {
    let mut files_created = 0_usize;
    let mut files_modified = 0_usize;
    let mut files_deleted = 0_usize;
    let mut recognized_files_read = 0_usize;
    let mut unique_files = BTreeSet::new();
    for project in projects.values() {
        files_created = files_created.saturating_add(project.created.len());
        files_modified = files_modified.saturating_add(project.modified.len());
        files_deleted = files_deleted.saturating_add(project.deleted.len());
        recognized_files_read = recognized_files_read.saturating_add(project.reads.len());
        unique_files.extend(project.created.iter().cloned());
        unique_files.extend(project.modified.iter().cloned());
        unique_files.extend(project.deleted.iter().cloned());
        unique_files.extend(project.reads.iter().cloned());
    }
    ProviderFilesSummary {
        files_created,
        files_modified,
        files_deleted,
        recognized_files_read,
        unique_files: short_file_names(&unique_files)
            .into_iter()
            .take(10)
            .collect(),
    }
}

impl ProviderStandupAggregate {
    fn add_session(
        &mut self,
        project_key: &str,
        project_path: &str,
        project_roots: &[String],
        logical_root: &LogicalSessionKey,
        parsed: Arc<ParsedSession>,
        pricing: ProviderPricing,
    ) -> Result<()> {
        let entries = crate::provider::project::new_activity_entries(&parsed);
        let conversation = Conversation::from_entries(entries)?;
        let usage = crate::analysis::usage::provider_usage_summary(&conversation, pricing);
        self.input_tokens = self
            .input_tokens
            .saturating_add(usage.canonical.input_tokens);
        self.cache_creation_tokens = self
            .cache_creation_tokens
            .saturating_add(usage.canonical.cache_creation_tokens);
        self.cache_read_tokens = self
            .cache_read_tokens
            .saturating_add(usage.canonical.cache_read_tokens);
        self.output_tokens = self
            .output_tokens
            .saturating_add(usage.canonical.output_tokens);
        if let Some(cost) = usage.pricing.estimated_cost {
            self.estimated_cost += cost;
            self.has_estimated_cost = true;
        }
        if pricing == ProviderPricing::Unpriced && usage.canonical.total_processed_tokens > 0 {
            self.unpriced_providers
                .insert(parsed.descriptor.key.provider.to_string());
        }
        self.unpriced_models.extend(usage.pricing.unpriced_models);

        self.logical_sessions.insert(logical_root.clone());
        self.project_roots
            .entry(project_key.to_string())
            .or_default()
            .extend(project_roots.iter().cloned());
        let project = self
            .projects
            .entry(project_key.to_string())
            .or_insert_with(|| ProviderProjectActivity {
                path: project_path.to_string(),
                ..Default::default()
            });
        project.sessions.insert(logical_root.clone());

        for entry in &parsed.entries {
            if parsed
                .semantics
                .get(&entry.id)
                .is_some_and(|semantics| semantics.activity == ActivityKind::InheritedHistory)
            {
                continue;
            }
            let LogEntry::Assistant(assistant) = &entry.entry else {
                continue;
            };
            for tool in assistant.message.tool_uses() {
                let kind = tool_kind_for(&parsed, &entry.id, tool);
                *self
                    .tool_counts
                    .entry(tool_kind_label(&kind, &tool.name))
                    .or_default() += 1;
                match kind {
                    ToolKind::Shell => {
                        if let Some(command) = tool_command_text(tool) {
                            if command.contains("test")
                                || command.contains("pytest")
                                || command.contains("cargo nextest")
                            {
                                project.tests_run = project.tests_run.saturating_add(1);
                            }
                            if command.contains("git commit") {
                                project.commits_made = project.commits_made.saturating_add(1);
                            }
                        }
                    }
                    ToolKind::FileRead => {
                        project.searches_done = project.searches_done.saturating_add(1);
                        if let Some(path) = tool_file_path(tool) {
                            project.reads.insert(path.to_string());
                        }
                    }
                    ToolKind::Search => {
                        project.searches_done = project.searches_done.saturating_add(1);
                    }
                    _ => {}
                }
            }
        }

        let projection = FileChangeProjection {
            changes: parsed.file_changes.clone(),
            inherited_owners: parsed
                .semantics
                .iter()
                .filter(|(_, semantics)| semantics.activity == ActivityKind::InheritedHistory)
                .map(|(entry, _)| entry.clone())
                .collect(),
            owner_timestamps: parsed
                .entries
                .iter()
                .filter_map(|entry| {
                    entry
                        .entry
                        .timestamp()
                        .map(|timestamp| (entry.id.clone(), timestamp))
                })
                .collect(),
        };
        self.files.add_projection_for_logical_session(
            project_key,
            &parsed.descriptor.key,
            logical_root,
            &projection,
        );
        self.descriptors_analyzed = self.descriptors_analyzed.saturating_add(1);
        Ok(())
    }
}

/// Run the standup command.
pub fn run(cli: &Cli, args: &StandupArgs) -> Result<()> {
    if !args.provider.is_empty() {
        return run_provider(cli, args);
    }
    let claude_dir = get_claude_dir(cli.claude_dir.as_ref())?;

    // Parse period
    let duration = parse_period(&args.period)?;
    let period_start = Utc::now() - duration;
    let period_end = Utc::now();
    let cutoff = SystemTime::from(period_start);

    // Get all projects (filtered if specified)
    let projects = {
        let all = claude_dir.projects()?;
        if let Some(ref filter) = args.project {
            super::helpers::filter_projects(all, filter)
        } else {
            all
        }
    };

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
                                    if let Some(path) =
                                        tool_use.input.get("file_path").and_then(|v| v.as_str())
                                    {
                                        let action = match tool_use.name.as_str() {
                                            "Write" => FileAction::Created,
                                            "Edit" => FileAction::Modified,
                                            "Read" => FileAction::Read,
                                            _ => continue,
                                        };
                                        // Upgrade action if already tracked
                                        let entry =
                                            all_files.entry(path.to_string()).or_insert(action);
                                        if *entry == FileAction::Read && action != FileAction::Read
                                        {
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
    project_summaries.sort_by_key(|b| std::cmp::Reverse(b.sessions));

    // Build usage summary
    let usage = if (args.usage || args.all) && total_tokens > 0 {
        Some(UsageSummary {
            total_tokens,
            input_tokens: total_input,
            output_tokens: total_output,
            estimated_cost: if total_cost > 0.0 {
                Some(total_cost)
            } else {
                None
            },
        })
    } else {
        None
    };

    // Build files summary
    let files = if (args.files || args.all) && !all_files.is_empty() {
        let files_created = all_files
            .values()
            .filter(|&&a| a == FileAction::Created)
            .count();
        let files_modified = all_files
            .values()
            .filter(|&&a| a == FileAction::Modified)
            .count();
        let files_read = all_files
            .values()
            .filter(|&&a| a == FileAction::Read)
            .count();

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
        by_tool.sort_by_key(|b| std::cmp::Reverse(b.1));
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

    // Format and output (global --json overrides --format)
    let effective_format = if matches!(cli.effective_output(), OutputFormat::Json) {
        StandupFormat::Json
    } else {
        args.format
    };
    let output = format_report(&report, effective_format)?;

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

fn run_provider(cli: &Cli, args: &StandupArgs) -> Result<()> {
    use crate::provider::project::context_overlaps_time_range;
    use crate::provider::registry::ProviderSelection;

    let duration = parse_period(&args.period)?;
    let period_end = Utc::now();
    let period_start = period_end - duration;
    let since = Some(SystemTime::from(period_start));
    let selection = ProviderSelection::from_flags(&args.provider).map_err(|reason| {
        SnatchError::InvalidArgument {
            name: "--provider".to_string(),
            reason,
        }
    })?;
    let atomic = matches!(selection, ProviderSelection::Explicit(_));
    let registry = super::helpers::provider_registry(cli);
    let mut providers: BTreeSet<_> = registry
        .select(&selection)?
        .providers
        .into_iter()
        .map(|provider| provider.id().to_string())
        .collect();
    let mut aggregate = ProviderStandupAggregate::default();
    let mut date_fallbacks = BTreeSet::new();
    let mut selected_descriptors = 0_usize;
    let mut analysis_errors = Vec::new();
    let report = registry.visit_filtered_parsed_project_sessions(
        &selection,
        crate::cache::global_cache(),
        args.project.as_deref(),
        false,
        |_, session| {
            let (include, fallback) = context_overlaps_time_range(&session.context, since, None);
            if include {
                selected_descriptors = selected_descriptors.saturating_add(1);
                if fallback {
                    date_fallbacks.insert(session.descriptor.key.clone());
                }
            }
            include
        },
        |project, session, logical_root, parsed| {
            let provider = registry
                .get(&session.descriptor.key.provider)
                .expect("visited session came from a registered provider");
            let project_key = project.identity.to_string();
            let project_path = project
                .display_path
                .clone()
                .unwrap_or_else(|| project_key.clone());
            let mut roots = project.cwd_variants.clone();
            if let Some(path) = &project.display_path {
                roots.push(path.clone());
            }
            if let Err(error) = aggregate.add_session(
                &project_key,
                &project_path,
                &roots,
                logical_root,
                parsed,
                provider.capabilities().pricing,
            ) {
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
    if selected_descriptors > 0 && aggregate.descriptors_analyzed == 0 {
        return Err(SnatchError::ConfigError {
            message: "no selected session could be analyzed".to_string(),
        });
    }
    for (provider, _) in &report.skipped {
        providers.remove(&provider.to_string());
    }

    let file_index = std::mem::take(&mut aggregate.files).build();
    for (path, changes) in file_index.entries {
        for change in changes {
            if change.outcome != FileChangeOutcome::Applied {
                continue;
            }
            let Some(roots) = aggregate.project_roots.get(&change.project_path) else {
                continue;
            };
            if !crate::analysis::project_health::is_provider_project_file(&path, roots) {
                continue;
            }
            let display_path = change.move_path.as_deref().unwrap_or(&path).to_string();
            let Some(project) = aggregate.projects.get_mut(&change.project_path) else {
                continue;
            };
            match change.kind {
                FileChangeKind::Add => {
                    project.created.insert(display_path);
                }
                FileChangeKind::Delete => {
                    project.deleted.insert(display_path);
                }
                FileChangeKind::Update => {
                    project.modified.insert(display_path);
                }
            }
        }
    }
    for (project_key, project) in &mut aggregate.projects {
        let Some(roots) = aggregate.project_roots.get(project_key) else {
            project.reads.clear();
            continue;
        };
        project
            .reads
            .retain(|path| crate::analysis::project_health::is_provider_project_file(path, roots));
    }

    let mut projects: Vec<_> = aggregate
        .projects
        .iter()
        .map(|(project_key, project)| ProviderProjectSummary {
            project_key: project_key.clone(),
            path: project.path.clone(),
            sessions: project.sessions.len(),
            accomplishments: provider_project_accomplishments(project),
        })
        .collect();
    projects.sort_by(|a, b| {
        b.sessions
            .cmp(&a.sessions)
            .then_with(|| a.project_key.cmp(&b.project_key))
    });

    let processed_tokens = aggregate
        .input_tokens
        .saturating_add(aggregate.cache_creation_tokens)
        .saturating_add(aggregate.cache_read_tokens)
        .saturating_add(aggregate.output_tokens);
    let work_tokens = aggregate
        .input_tokens
        .saturating_add(aggregate.cache_creation_tokens)
        .saturating_add(aggregate.output_tokens);
    let has_unpriced =
        !aggregate.unpriced_providers.is_empty() || !aggregate.unpriced_models.is_empty();
    let pricing_coverage = if processed_tokens == 0 {
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
    let usage = ((args.usage || args.all) && processed_tokens > 0).then(|| ProviderStandupUsage {
        work_tokens,
        total_processed_tokens: processed_tokens,
        input_uncached_tokens: aggregate.input_tokens,
        cache_creation_tokens: aggregate.cache_creation_tokens,
        cache_read_tokens: aggregate.cache_read_tokens,
        output_tokens: aggregate.output_tokens,
        estimated_cost: aggregate
            .has_estimated_cost
            .then_some(aggregate.estimated_cost),
        pricing_coverage,
        unpriced_providers: aggregate.unpriced_providers.iter().cloned().collect(),
        unpriced_models: aggregate.unpriced_models.iter().cloned().collect(),
    });

    let files = if args.files || args.all {
        Some(provider_files_summary(&aggregate.projects))
    } else {
        None
    };
    let tools = if args.tools || args.all {
        let total_invocations = aggregate.tool_counts.values().sum();
        let mut by_tool: Vec<_> = aggregate.tool_counts.into_iter().collect();
        by_tool.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        by_tool.truncate(10);
        Some(ToolsSummary {
            total_invocations,
            by_tool,
        })
    } else {
        None
    };
    let mut warnings = report.warnings;
    warnings.extend(
        analysis_errors
            .into_iter()
            .map(|(key, _)| format!("{key}: session could not be analyzed")),
    );
    if !date_fallbacks.is_empty() {
        warnings.push(format!(
            "{} session descriptors used conservative source-time evidence for period filtering",
            date_fallbacks.len()
        ));
    }
    warnings.sort();
    warnings.dedup();
    let skipped_providers: Vec<_> = report
        .skipped
        .iter()
        .map(|(provider, reason)| ProviderStandupSkip {
            provider: provider.to_string(),
            reason: reason.clone(),
        })
        .collect();
    let report = ProviderStandupReport {
        period: args.period.clone(),
        period_start,
        period_end,
        period_basis: "logical sessions whose whole source artifact overlaps the period",
        providers: providers.into_iter().collect(),
        projects,
        total_sessions: aggregate.logical_sessions.len(),
        session_descriptors_analyzed: aggregate.descriptors_analyzed,
        date_filter_fallback_descriptors: date_fallbacks.len(),
        usage,
        files,
        tools,
        skipped_providers,
        warnings,
        coverage_note: "Accomplishments and files use normalized new-work tool calls plus source-backed applied file changes; arbitrary shell writes are not inferred, and recognized reads are not a complete I/O audit.",
    };
    let effective_format = if matches!(cli.effective_output(), OutputFormat::Json) {
        StandupFormat::Json
    } else {
        args.format
    };
    let output = format_provider_report(&report, effective_format)?;
    if args.clipboard {
        match arboard::Clipboard::new() {
            Ok(mut clipboard) => {
                if let Err(error) = clipboard.set_text(&output) {
                    eprintln!("Warning: Failed to copy to clipboard: {error}");
                } else if !cli.quiet {
                    eprintln!("Copied to clipboard.");
                }
            }
            Err(error) => eprintln!("Warning: Failed to access clipboard: {error}"),
        }
    }
    println!("{output}");
    for skipped in &report.skipped_providers {
        eprintln!(
            "warning: provider '{}' skipped: {}",
            skipped.provider, skipped.reason
        );
    }
    for warning in &report.warnings {
        eprintln!("warning: {warning}");
    }
    Ok(())
}

/// Format the report based on the requested format.
fn format_report(report: &StandupReport, format: StandupFormat) -> Result<String> {
    match format {
        StandupFormat::Json => Ok(serde_json::to_string_pretty(report)?),
        StandupFormat::Text => format_text(report),
        StandupFormat::Markdown => format_markdown(report),
    }
}

fn format_provider_report(report: &ProviderStandupReport, format: StandupFormat) -> Result<String> {
    match format {
        StandupFormat::Json => Ok(serde_json::to_string_pretty(report)?),
        StandupFormat::Text => format_provider_text(report),
        StandupFormat::Markdown => format_provider_markdown(report),
    }
}

fn format_provider_text(report: &ProviderStandupReport) -> Result<String> {
    let mut output = String::new();
    writeln!(output, "Standup Report ({})", report.period).ok();
    writeln!(output, "{}", "=".repeat(40)).ok();
    writeln!(output, "Period basis: {}", report.period_basis).ok();
    writeln!(output, "Providers: {}", report.providers.join(", ")).ok();
    writeln!(
        output,
        "Sessions: {} logical ({} source descriptors)",
        format_count(report.total_sessions),
        format_count(report.session_descriptors_analyzed)
    )
    .ok();
    writeln!(
        output,
        "Projects: {}\n",
        format_count(report.projects.len())
    )
    .ok();
    if report.projects.is_empty() {
        writeln!(output, "No activity in the last {}.", report.period).ok();
        return Ok(output);
    }
    writeln!(output, "Projects Worked On:").ok();
    writeln!(output, "-------------------").ok();
    for project in &report.projects {
        let name = project.path.split('/').next_back().unwrap_or(&project.path);
        writeln!(output, "  {} ({} sessions)", name, project.sessions).ok();
        for accomplishment in &project.accomplishments {
            writeln!(output, "    - {accomplishment}").ok();
        }
    }
    writeln!(output).ok();
    if let Some(usage) = &report.usage {
        writeln!(output, "Token Usage:").ok();
        writeln!(output, "------------").ok();
        writeln!(
            output,
            "  Work:      {} tokens",
            format_number(usage.work_tokens)
        )
        .ok();
        writeln!(
            output,
            "  Processed: {} tokens",
            format_number(usage.total_processed_tokens)
        )
        .ok();
        writeln!(
            output,
            "  Input:     {} uncached + {} cache creation + {} cache read",
            format_number(usage.input_uncached_tokens),
            format_number(usage.cache_creation_tokens),
            format_number(usage.cache_read_tokens)
        )
        .ok();
        writeln!(
            output,
            "  Output:    {} tokens",
            format_number(usage.output_tokens)
        )
        .ok();
        match usage.estimated_cost {
            Some(cost) => writeln!(
                output,
                "  Estimated cost: ${cost:.2} ({})",
                usage.pricing_coverage
            )
            .ok(),
            None => writeln!(output, "  Estimated cost: N/A ({})", usage.pricing_coverage).ok(),
        };
        writeln!(output).ok();
    }
    if let Some(files) = &report.files {
        writeln!(output, "File Evidence:").ok();
        writeln!(output, "--------------").ok();
        writeln!(output, "  Created:         {}", files.files_created).ok();
        writeln!(output, "  Modified:        {}", files.files_modified).ok();
        writeln!(output, "  Deleted:         {}", files.files_deleted).ok();
        writeln!(
            output,
            "  Recognized reads: {}",
            files.recognized_files_read
        )
        .ok();
        if !files.unique_files.is_empty() {
            writeln!(output, "  Files: {}", files.unique_files.join(", ")).ok();
        }
        writeln!(output).ok();
    }
    if let Some(tools) = &report.tools {
        writeln!(
            output,
            "Canonical Tool Usage ({} total):",
            format_count(tools.total_invocations)
        )
        .ok();
        writeln!(output, "------------------------------").ok();
        for (tool, count) in &tools.by_tool {
            writeln!(output, "  {tool}: {}", format_count(*count)).ok();
        }
        writeln!(output).ok();
    }
    writeln!(output, "Coverage: {}", report.coverage_note).ok();
    Ok(output)
}

fn format_provider_markdown(report: &ProviderStandupReport) -> Result<String> {
    let mut output = String::new();
    writeln!(output, "## Standup Report ({})\n", report.period).ok();
    writeln!(output, "_Period basis: {}_\n", report.period_basis).ok();
    writeln!(
        output,
        "**Sessions:** {} logical / {} source | **Projects:** {} | **Providers:** {}\n",
        format_count(report.total_sessions),
        format_count(report.session_descriptors_analyzed),
        format_count(report.projects.len()),
        report.providers.join(", ")
    )
    .ok();
    if report.projects.is_empty() {
        writeln!(output, "*No activity in the last {}.*", report.period).ok();
        return Ok(output);
    }
    writeln!(output, "### What I worked on\n").ok();
    for project in &report.projects {
        let name = project.path.split('/').next_back().unwrap_or(&project.path);
        writeln!(output, "- **{}** ({} sessions)", name, project.sessions).ok();
        for accomplishment in &project.accomplishments {
            writeln!(output, "  - {accomplishment}").ok();
        }
    }
    writeln!(output).ok();
    if let Some(usage) = &report.usage {
        let cost = usage
            .estimated_cost
            .map(|cost| format!("${cost:.2}"))
            .unwrap_or_else(|| "N/A".to_string());
        writeln!(
            output,
            "**Tokens:** {} work / {} processed | **Estimated cost:** {} ({})\n",
            format_number(usage.work_tokens),
            format_number(usage.total_processed_tokens),
            cost,
            usage.pricing_coverage
        )
        .ok();
    }
    if let Some(files) = &report.files {
        writeln!(
            output,
            "**Source-backed files:** {} created, {} modified, {} deleted; {} recognized reads\n",
            files.files_created,
            files.files_modified,
            files.files_deleted,
            files.recognized_files_read
        )
        .ok();
    }
    if let Some(tools) = &report.tools {
        let top_tools: Vec<_> = tools
            .by_tool
            .iter()
            .take(5)
            .map(|(tool, count)| format!("{tool}:{count}"))
            .collect();
        writeln!(output, "**Canonical tools:** {}\n", top_tools.join(" | ")).ok();
    }
    writeln!(output, "_Coverage: {}_", report.coverage_note).ok();
    Ok(output)
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
        let name = project.path.split('/').next_back().unwrap_or(&project.path);
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
        writeln!(
            output,
            "  Total:  {} tokens",
            format_number(usage.total_tokens)
        )
        .ok();
        writeln!(
            output,
            "  Input:  {} tokens",
            format_number(usage.input_tokens)
        )
        .ok();
        writeln!(
            output,
            "  Output: {} tokens",
            format_number(usage.output_tokens)
        )
        .ok();
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
        writeln!(
            output,
            "Tool Usage ({} total):",
            format_count(tools.total_invocations)
        )
        .ok();
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

    writeln!(
        output,
        "**Sessions:** {} | **Projects:** {}\n",
        format_count(report.total_sessions),
        format_count(report.projects.len())
    )
    .ok();

    // Projects
    writeln!(output, "### What I worked on\n").ok();
    for project in &report.projects {
        let name = project.path.split('/').next_back().unwrap_or(&project.path);
        writeln!(output, "- **{}** ({} sessions)", name, project.sessions).ok();
        for accomplishment in &project.accomplishments {
            writeln!(output, "  - {accomplishment}").ok();
        }
    }
    writeln!(output).ok();

    // Usage
    if let Some(ref usage) = report.usage {
        write!(
            output,
            "**Tokens:** {} total",
            format_number(usage.total_tokens)
        )
        .ok();
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
        let top_tools: Vec<_> = tools
            .by_tool
            .iter()
            .take(5)
            .map(|(t, c)| format!("{t}:{c}"))
            .collect();
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

    #[test]
    fn provider_file_counts_keep_project_identity() {
        let project = || {
            let mut value = ProviderProjectActivity::default();
            value.modified.insert("src/lib.rs".to_string());
            value
        };
        let projects = BTreeMap::from([
            ("cwd:/work/one".to_string(), project()),
            ("cwd:/work/two".to_string(), project()),
        ]);
        let summary = provider_files_summary(&projects);
        assert_eq!(summary.files_modified, 2);
        assert_eq!(summary.unique_files, ["lib.rs"]);
    }
}
