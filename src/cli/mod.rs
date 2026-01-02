//! Command-line interface for claude-snatch.
//!
//! Provides scriptable CLI access to Claude Code session data with
//! five core commands:
//! - `list`: List projects and sessions
//! - `export`: Export conversations in various formats
//! - `search`: Search across sessions
//! - `stats`: Show usage statistics
//! - `info`: Display session/project information

mod commands;

pub use commands::*;

use clap::{ArgAction, CommandFactory, Parser, Subcommand, ValueEnum};
use clap_complete::{generate, Shell};
use std::io::{self, IsTerminal};
use std::path::PathBuf;

use crate::cache::init_global_cache;
use crate::config::Config;
use crate::error::Result;
#[cfg(feature = "mcp")]
use crate::error::SnatchError;
use crate::export::ExportFormat;

/// Claude Code conversation extractor with maximum data fidelity.
#[derive(Debug, Parser)]
#[command(name = "snatch")]
#[command(author, version, about, long_about = None)]
#[command(propagate_version = true)]
#[command(disable_help_flag = true)]
pub struct Cli {
    /// Subcommand to run (defaults to TUI in interactive terminals).
    #[command(subcommand)]
    pub command: Option<Commands>,

    /// Print short help (use --help for full details).
    #[arg(short = 'h', global = true, action = ArgAction::HelpShort)]
    short_help: (),

    /// Print full help with all options.
    #[arg(long = "help", global = true, action = ArgAction::HelpLong)]
    long_help: (),

    /// Path to Claude directory (default: ~/.claude).
    #[arg(short = 'd', long, global = true, env = "SNATCH_CLAUDE_DIR", hide_short_help = true)]
    pub claude_dir: Option<PathBuf>,

    /// Output format for structured data.
    #[arg(short = 'o', long, global = true, default_value = "text", env = "SNATCH_OUTPUT")]
    pub output: OutputFormat,

    /// Enable verbose output.
    #[arg(short = 'v', long, global = true, env = "SNATCH_VERBOSE")]
    pub verbose: bool,

    /// Suppress non-essential output.
    #[arg(short = 'q', long, global = true, env = "SNATCH_QUIET")]
    pub quiet: bool,

    /// Enable colored output (auto-detected by default).
    #[arg(long, global = true, env = "SNATCH_COLOR", hide_short_help = true)]
    pub color: Option<bool>,

    /// Output as JSON (shorthand for -o json).
    #[arg(long, global = true, env = "SNATCH_JSON")]
    pub json: bool,

    /// Log level (error, warn, info, debug, trace).
    #[arg(long, global = true, default_value = "warn", env = "SNATCH_LOG_LEVEL", hide_short_help = true)]
    pub log_level: LogLevel,

    /// Log format (text, json, compact, pretty).
    #[arg(long, global = true, default_value = "text", env = "SNATCH_LOG_FORMAT", hide_short_help = true)]
    pub log_format: LogFormat,

    /// Log output file (default: stderr).
    #[arg(long, global = true, env = "SNATCH_LOG_FILE", hide_short_help = true)]
    pub log_file: Option<std::path::PathBuf>,

    /// Number of threads for parallel processing (default: number of CPUs).
    #[arg(short = 'j', long, global = true, env = "SNATCH_THREADS", hide_short_help = true)]
    pub threads: Option<usize>,

    /// Path to custom configuration file.
    #[arg(long, global = true, env = "SNATCH_CONFIG", hide_short_help = true)]
    pub config: Option<PathBuf>,

    /// Maximum file size to parse in bytes (default: 100MB).
    /// Use 0 for unlimited. Prevents memory exhaustion on large files.
    #[arg(long, global = true, env = "SNATCH_MAX_FILE_SIZE", value_name = "BYTES", hide_short_help = true)]
    pub max_file_size: Option<u64>,
}

/// Log level options.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, ValueEnum)]
pub enum LogLevel {
    /// Only errors.
    Error,
    /// Errors and warnings.
    #[default]
    Warn,
    /// Errors, warnings, and informational messages.
    Info,
    /// All of the above plus debug messages.
    Debug,
    /// All messages including trace-level details.
    Trace,
}

/// Log format options.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, ValueEnum)]
pub enum LogFormat {
    /// Human-readable text format.
    #[default]
    Text,
    /// Structured JSON format for machine consumption.
    Json,
    /// Compact single-line format.
    Compact,
    /// Pretty format with full details.
    Pretty,
}

impl LogLevel {
    /// Convert to tracing filter level.
    #[must_use]
    pub fn to_filter_string(self) -> &'static str {
        match self {
            Self::Error => "error",
            Self::Warn => "warn",
            Self::Info => "info",
            Self::Debug => "debug",
            Self::Trace => "trace",
        }
    }
}

impl Cli {
    /// Get effective output format.
    #[must_use]
    pub fn effective_output(&self) -> OutputFormat {
        if self.json {
            OutputFormat::Json
        } else {
            self.output
        }
    }

    /// Check if colored output should be used.
    ///
    /// Returns true if:
    /// - `--color=true` was explicitly set, OR
    /// - `--color` was not set and stdout is a terminal
    #[must_use]
    pub fn effective_color(&self) -> bool {
        match self.color {
            Some(true) => true,
            Some(false) => false,
            None => std::io::stdout().is_terminal(),
        }
    }
}

/// CLI subcommands.
#[derive(Debug, Subcommand)]
pub enum Commands {
    /// List projects and sessions.
    #[command(alias = "ls")]
    List(ListArgs),

    /// Export conversations to various formats.
    #[command(alias = "x")]
    Export(ExportArgs),

    /// Search across sessions.
    #[command(alias = "s", alias = "find")]
    Search(SearchArgs),

    /// Show usage statistics.
    #[command(alias = "stat")]
    Stats(StatsArgs),

    /// Display detailed information.
    #[command(alias = "i", alias = "show")]
    Info(InfoArgs),

    /// Launch interactive TUI.
    #[command(alias = "ui")]
    Tui(TuiArgs),

    /// Validate session files.
    Validate(ValidateArgs),

    /// Watch for session changes.
    Watch(WatchArgs),

    /// Compare two sessions or files.
    #[command(alias = "d")]
    Diff(DiffArgs),

    /// View and modify configuration.
    #[command(alias = "cfg")]
    Config(ConfigArgs),

    /// Extract Beyond-JSONL data (settings, CLAUDE.md, MCP, etc.).
    #[command(alias = "ext")]
    Extract(ExtractArgs),

    /// Manage the session cache.
    Cache(CacheArgs),

    /// Manage the full-text search index.
    #[command(alias = "idx")]
    Index(IndexArgs),

    /// Generate shell completions.
    Completions(CompletionsArgs),

    /// Generate dynamic completions (used by shell completion scripts).
    #[command(hide = true, name = "_complete")]
    DynamicCompletions(DynamicCompletionsArgs),

    /// Clean up old or empty sessions.
    #[command(alias = "clean")]
    Cleanup(CleanupArgs),

    /// Manage session tags and names.
    Tag(TagArgs),

    /// Extract code blocks from sessions.
    Code(CodeArgs),

    /// Extract user prompts from sessions.
    Prompts(PromptsArgs),

    /// Generate a standup report of recent activity.
    #[command(alias = "daily")]
    Standup(StandupArgs),

    /// Interactively pick a session using fuzzy search.
    #[command(alias = "browse")]
    Pick(PickArgs),

    /// Quick start guide for new users.
    #[command(alias = "guide", alias = "examples")]
    Quickstart(QuickstartArgs),

    /// Show a quick summary of Claude Code usage.
    Summary(SummaryArgs),

    /// List the most recent sessions (shorthand for list -n 5).
    Recent(RecentArgs),

    /// Start MCP (Model Context Protocol) server mode.
    /// Requires the 'mcp' feature to be enabled.
    #[cfg(feature = "mcp")]
    #[command(alias = "mcp")]
    ServeMcp(ServeMcpArgs),
}

/// Arguments for the completions command.
#[derive(Debug, Clone, clap::Args)]
pub struct CompletionsArgs {
    /// Shell to generate completions for.
    #[arg(value_enum)]
    pub shell: CompletionShell,
}

/// Supported shells for completion generation.
#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum CompletionShell {
    /// Bash shell.
    Bash,
    /// Zsh shell.
    Zsh,
    /// Fish shell.
    Fish,
    /// PowerShell.
    Powershell,
    /// Elvish shell.
    Elvish,
}

impl From<CompletionShell> for Shell {
    fn from(shell: CompletionShell) -> Self {
        match shell {
            CompletionShell::Bash => Shell::Bash,
            CompletionShell::Zsh => Shell::Zsh,
            CompletionShell::Fish => Shell::Fish,
            CompletionShell::Powershell => Shell::PowerShell,
            CompletionShell::Elvish => Shell::Elvish,
        }
    }
}

/// Supported completion types.
#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum DynamicCompletionType {
    /// Session IDs.
    Sessions,
    /// Project paths.
    Projects,
    /// Tool names.
    Tools,
    /// Output formats.
    Formats,
    /// Model names.
    Models,
}

/// Arguments for dynamic completions (hidden command).
#[derive(Debug, Clone, clap::Args)]
pub struct DynamicCompletionsArgs {
    /// Type of completion to generate.
    #[arg(value_enum)]
    pub completion_type: DynamicCompletionType,

    /// Optional prefix to filter completions.
    #[arg(short = 'p', long)]
    pub prefix: Option<String>,

    /// Maximum number of completions to return.
    #[arg(short = 'l', long, default_value = "50")]
    pub limit: usize,
}

/// Generate shell completions and print to stdout.
pub fn generate_completions(shell: CompletionShell) {
    let mut cmd = Cli::command();
    let shell: Shell = shell.into();
    generate(shell, &mut cmd, "snatch", &mut io::stdout());
}

/// Output format for CLI.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, ValueEnum)]
pub enum OutputFormat {
    /// Human-readable text.
    #[default]
    Text,
    /// JSON output.
    Json,
    /// Tab-separated values.
    Tsv,
    /// Compact single-line output.
    Compact,
}

/// Arguments for the list command.
#[derive(Debug, Parser)]
pub struct ListArgs {
    /// What to list.
    #[arg(default_value = "sessions")]
    pub target: ListTarget,

    /// Filter by project path (substring match).
    #[arg(short = 'p', long)]
    pub project: Option<String>,

    /// Include subagent sessions.
    #[arg(long)]
    pub subagents: bool,

    /// Only show active sessions.
    #[arg(long)]
    pub active: bool,

    /// Sort order.
    #[arg(short = 's', long, default_value = "modified")]
    pub sort: SortOrder,

    /// Limit number of results (default: 50, use 0 for unlimited).
    #[arg(short = 'n', long, default_value = "50")]
    pub limit: usize,

    /// Show full UUIDs instead of short IDs.
    #[arg(long)]
    pub full_ids: bool,

    /// Show file sizes.
    #[arg(long)]
    pub sizes: bool,

    /// Pipe output through a pager (less/more).
    #[arg(long)]
    pub pager: bool,

    /// Filter sessions modified since this date (YYYY-MM-DD or relative like "1week", "3days").
    #[arg(long)]
    pub since: Option<String>,

    /// Filter sessions modified until this date (YYYY-MM-DD or relative like "1week", "3days").
    #[arg(long)]
    pub until: Option<String>,

    /// Filter sessions by tag.
    #[arg(long)]
    pub tag: Option<String>,

    /// Filter sessions by multiple tags (comma-separated).
    #[arg(long)]
    pub tags: Option<String>,

    /// Show only bookmarked sessions.
    #[arg(long)]
    pub bookmarked: bool,

    /// Filter sessions by outcome (success, partial, failed, abandoned).
    #[arg(long)]
    pub outcome: Option<String>,

    /// Filter sessions by custom name (substring match).
    #[arg(long)]
    pub by_name: Option<String>,

    /// Minimum file size filter (e.g., "1KB", "1MB").
    #[arg(long)]
    pub min_size: Option<String>,

    /// Maximum file size filter (e.g., "10MB", "100KB").
    #[arg(long)]
    pub max_size: Option<String>,

    /// Show session context (first user prompt preview).
    #[arg(short = 'c', long)]
    pub context: bool,

    /// Hide projects with 0 sessions (only applies to 'list projects').
    #[arg(long)]
    pub hide_empty: bool,
}

/// What to list.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, ValueEnum)]
pub enum ListTarget {
    /// List projects.
    Projects,
    /// List sessions.
    #[default]
    Sessions,
    /// List all (projects and sessions).
    All,
}

/// Sort order for listings.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, ValueEnum)]
pub enum SortOrder {
    /// Sort by modification time (newest first).
    #[default]
    Modified,
    /// Sort by modification time (oldest first).
    Oldest,
    /// Sort by size (largest first).
    Size,
    /// Sort by name/path alphabetically.
    Name,
}

/// Content types that can be filtered in exports.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, ValueEnum)]
pub enum ContentFilter {
    /// User messages/prompts (includes tool results within user entries).
    User,
    /// Only human-typed prompts (excludes tool results within user entries).
    #[value(alias = "human")]
    Prompts,
    /// Assistant responses (text content).
    Assistant,
    /// Thinking/reasoning blocks.
    Thinking,
    /// Tool invocations.
    #[value(alias = "tools")]
    ToolUse,
    /// Tool results/outputs.
    ToolResults,
    /// System messages.
    System,
    /// Summary entries.
    Summary,
    /// Code blocks only (extracted from assistant responses).
    Code,
}

impl ContentFilter {
    /// Get a human-readable description.
    pub fn description(&self) -> &'static str {
        match self {
            Self::User => "user messages",
            Self::Prompts => "human-typed prompts only",
            Self::Assistant => "assistant responses",
            Self::Thinking => "thinking blocks",
            Self::ToolUse => "tool invocations",
            Self::ToolResults => "tool results",
            Self::System => "system messages",
            Self::Summary => "summary entries",
            Self::Code => "code blocks only",
        }
    }
}

/// Arguments for the export command.
#[derive(Debug, Parser)]
pub struct ExportArgs {
    /// Session ID to export (supports short prefixes like "780893e4").
    /// Optional with --all flag.
    pub session: Option<String>,

    /// Output file path (stdout if not specified).
    #[arg(short = 'O', long = "out")]
    pub output_file: Option<PathBuf>,

    /// Export format.
    #[arg(short = 'f', long, default_value = "markdown", env = "SNATCH_EXPORT_FORMAT")]
    pub format: ExportFormatArg,

    /// Export all sessions.
    #[arg(short = 'a', long)]
    pub all: bool,

    /// Filter by project path (substring match).
    #[arg(short = 'p', long)]
    pub project: Option<String>,

    /// Export sessions modified since this date (YYYY-MM-DD or relative like "1week").
    #[arg(long)]
    pub since: Option<String>,

    /// Export sessions modified until this date (YYYY-MM-DD or relative like "1week").
    #[arg(long)]
    pub until: Option<String>,

    /// Include subagent sessions.
    #[arg(long)]
    pub subagents: bool,

    /// Combine parent session with its subagent transcripts (interleaved by time).
    #[arg(long)]
    pub combine_agents: bool,

    /// Include thinking blocks (enabled by default, use --no-thinking to disable).
    #[arg(long, default_value = "true")]
    pub thinking: bool,

    /// Include tool use blocks (enabled by default, use --no-tool-use to disable).
    #[arg(long, default_value = "true")]
    pub tool_use: bool,

    /// Include tool results (enabled by default, use --no-tool-results to disable).
    #[arg(long, default_value = "true")]
    pub tool_results: bool,

    /// Include system messages (disabled by default).
    #[arg(long)]
    pub system: bool,

    /// Export ONLY specified content types (exclusive filter).
    /// When set, only these content types are included; all others are excluded.
    /// Accepts multiple values: --only user,thinking or --only user --only thinking
    #[arg(long, value_delimiter = ',', value_name = "TYPES")]
    pub only: Vec<ContentFilter>,

    /// Include timestamps.
    #[arg(long, default_value = "true")]
    pub timestamps: bool,

    /// Include usage statistics.
    #[arg(long, default_value = "true")]
    pub usage: bool,

    /// Include metadata (UUIDs, etc.).
    #[arg(long)]
    pub metadata: bool,

    /// Only export main thread (exclude branches). By default all entries are exported.
    #[arg(long)]
    pub main_thread: bool,

    /// Pretty-print JSON output.
    #[arg(long)]
    pub pretty: bool,

    /// Lossless export: preserve all data including unknown fields.
    /// Implies --metadata, --system, --thinking, --tool-use, --tool-results.
    #[arg(long)]
    pub lossless: bool,

    /// Show progress bar for long operations.
    #[arg(long)]
    pub progress: bool,

    /// Overwrite existing output files without prompting.
    #[arg(long)]
    pub overwrite: bool,

    /// Redact sensitive data from output.
    /// Accepts: "security" (API keys, passwords, credentials) or "all" (includes emails, IPs, phones).
    #[arg(long, value_name = "LEVEL")]
    pub redact: Option<RedactionLevel>,

    /// Warn about PII (personally identifiable information) in exported content.
    /// Does not modify output, only displays warnings.
    #[arg(long)]
    pub warn_pii: bool,

    /// Preview what would be redacted without actually removing data.
    /// Shows sensitive data wrapped in [WOULD-REDACT:Type]...[/WOULD-REDACT] markers.
    /// Use with --redact to see exactly what will be hidden before committing.
    #[arg(long)]
    pub redact_preview: bool,

    /// Upload export to GitHub Gist instead of writing to file.
    /// Requires the `gh` CLI to be installed and authenticated.
    #[arg(long)]
    pub gist: bool,

    /// Make the gist public (default is secret/private).
    #[arg(long)]
    pub gist_public: bool,

    /// Description for the gist.
    #[arg(long)]
    pub gist_description: Option<String>,

    /// Include table of contents/navigation sidebar in HTML export.
    #[arg(long)]
    pub toc: bool,

    /// Use dark theme for HTML export (default is light).
    #[arg(long)]
    pub dark: bool,

    /// Copy export to clipboard instead of writing to file/stdout.
    #[arg(long, visible_alias = "copy")]
    pub clipboard: bool,

    /// Use a custom export template by name.
    /// Templates are defined in config at ~/.config/claude-snatch/templates/
    /// Use --template list to see available templates.
    #[arg(long, value_name = "NAME")]
    pub template: Option<String>,

    /// List available export templates.
    #[arg(long)]
    pub list_templates: bool,
}

/// Export format argument.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, ValueEnum)]
pub enum ExportFormatArg {
    /// Markdown format.
    #[default]
    Markdown,
    /// Compact Markdown.
    Md,
    /// JSON format.
    Json,
    /// Pretty JSON.
    JsonPretty,
    /// Plain text.
    Text,
    /// JSONL (original format).
    Jsonl,
    /// CSV tabular format.
    Csv,
    /// XML structured markup.
    Xml,
    /// HTML formatted output.
    Html,
    /// SQLite database.
    Sqlite,
    /// OpenTelemetry (OTLP JSON).
    Otel,
}

/// Redaction level for sensitive data.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, ValueEnum)]
pub enum RedactionLevel {
    /// Redact security-sensitive data: API keys, passwords, credentials, SSN, credit cards.
    #[default]
    Security,
    /// Redact all sensitive data: includes emails, IP addresses, phone numbers.
    All,
}

impl From<RedactionLevel> for crate::util::RedactionConfig {
    fn from(level: RedactionLevel) -> Self {
        match level {
            RedactionLevel::Security => crate::util::RedactionConfig::security(),
            RedactionLevel::All => crate::util::RedactionConfig::all(),
        }
    }
}

impl From<ExportFormatArg> for ExportFormat {
    fn from(arg: ExportFormatArg) -> Self {
        match arg {
            ExportFormatArg::Markdown | ExportFormatArg::Md => ExportFormat::Markdown,
            ExportFormatArg::Json => ExportFormat::Json,
            ExportFormatArg::JsonPretty => ExportFormat::JsonPretty,
            ExportFormatArg::Text | ExportFormatArg::Jsonl => ExportFormat::Text,
            ExportFormatArg::Csv => ExportFormat::Csv,
            ExportFormatArg::Xml => ExportFormat::Xml,
            ExportFormatArg::Html => ExportFormat::Html,
            ExportFormatArg::Sqlite => ExportFormat::Sqlite,
            ExportFormatArg::Otel => ExportFormat::Otel,
        }
    }
}

/// Arguments for the search command.
#[derive(Debug, Parser)]
pub struct SearchArgs {
    /// Search pattern (regex supported).
    pub pattern: String,

    /// Search in specific project.
    #[arg(short = 'p', long)]
    pub project: Option<String>,

    /// Filter by session ID (supports short prefixes like "780893e4").
    #[arg(short = 's', long)]
    pub session: Option<String>,

    /// Case-insensitive search.
    #[arg(short = 'i', long)]
    pub ignore_case: bool,

    /// Also search in thinking blocks (by default only user/assistant text is searched).
    #[arg(long)]
    pub thinking: bool,

    /// Also search in tool outputs (by default only user/assistant text is searched).
    #[arg(long)]
    pub tools: bool,

    /// Search everywhere (user, assistant, thinking, and tools).
    #[arg(short = 'a', long)]
    pub all: bool,

    /// Show context lines.
    #[arg(short = 'C', long, default_value = "2")]
    pub context: usize,

    /// Maximum results.
    #[arg(short = 'n', long)]
    pub limit: Option<usize>,

    /// Only show session IDs containing matches (like grep -l).
    #[arg(short = 'l', long)]
    pub files_only: bool,

    /// Show count of matches only (like grep -c).
    #[arg(short = 'c', long)]
    pub count: bool,

    /// Filter by message type (user, assistant, system, summary).
    #[arg(short = 't', long = "type")]
    pub message_type: Option<String>,

    /// Filter by model used (e.g., "claude-sonnet", "opus").
    #[arg(short = 'm', long)]
    pub model: Option<String>,

    /// Filter by specific tool name (e.g., "Read", "Bash").
    #[arg(long = "tool-name")]
    pub tool_name: Option<String>,

    /// Only show messages with errors.
    #[arg(long)]
    pub errors: bool,

    /// Enable fuzzy matching (like fzf).
    #[arg(short = 'f', long)]
    pub fuzzy: bool,

    /// Minimum fuzzy match score (0-100, default 60).
    #[arg(long, default_value = "60")]
    pub fuzzy_threshold: u8,

    /// Minimum token count for messages.
    #[arg(long)]
    pub min_tokens: Option<u64>,

    /// Maximum token count for messages.
    #[arg(long)]
    pub max_tokens: Option<u64>,

    /// Filter by git branch (partial match).
    #[arg(short = 'b', long = "branch")]
    pub git_branch: Option<String>,

    /// Sort results by relevance score (descending).
    #[arg(long)]
    pub sort: bool,
}

/// Arguments for the stats command.
#[derive(Debug, Parser)]
pub struct StatsArgs {
    /// Session ID to show stats for (supports short prefixes like "780893e4").
    /// Optional - shows global stats if not specified.
    pub session: Option<String>,

    /// Show stats for specific project.
    #[arg(short = 'p', long)]
    pub project: Option<String>,

    /// Show global stats across all sessions.
    #[arg(long)]
    pub global: bool,

    /// Show tool usage breakdown.
    #[arg(long)]
    pub tools: bool,

    /// Show model usage breakdown.
    #[arg(long)]
    pub models: bool,

    /// Show cost breakdown.
    #[arg(long)]
    pub costs: bool,

    /// Show usage grouped by 5-hour billing windows.
    #[arg(short = 'b', long)]
    pub blocks: bool,

    /// Token limit for blocks display (e.g., 500000). Use "max" for highest historical block.
    #[arg(long, value_name = "LIMIT")]
    pub token_limit: Option<String>,

    /// Show all detailed stats.
    #[arg(short = 'a', long)]
    pub all: bool,

    /// Show sparkline visualizations for usage trends.
    #[arg(long)]
    pub sparkline: bool,

    /// Show historical cost tracking data.
    #[arg(long)]
    pub history: bool,

    /// Number of days to show in history (default: 30).
    #[arg(long, default_value = "30", value_name = "DAYS")]
    pub days: i64,

    /// Record current global stats to cost history.
    #[arg(long)]
    pub record: bool,

    /// Show weekly cost aggregation.
    #[arg(long)]
    pub weekly: bool,

    /// Show monthly cost aggregation.
    #[arg(long)]
    pub monthly: bool,

    /// Export cost history as CSV.
    #[arg(long)]
    pub csv: bool,

    /// Clear all cost history data.
    #[arg(long)]
    pub clear_history: bool,

    /// Show activity timeline visualization.
    #[arg(long)]
    pub timeline: bool,

    /// Time granularity for timeline: "hourly", "daily", or "weekly".
    #[arg(long, value_name = "GRANULARITY", default_value = "daily")]
    pub granularity: String,

    /// Show token usage graph visualization.
    #[arg(long)]
    pub graph: bool,

    /// Width of graph visualization (default: 60).
    #[arg(long, value_name = "WIDTH", default_value = "60")]
    pub graph_width: usize,
}

/// Arguments for the info command.
#[derive(Debug, Parser)]
pub struct InfoArgs {
    /// Session ID or project path to show info for.
    /// Session IDs support short prefixes like "780893e4".
    pub target: Option<String>,

    /// Show tree structure.
    #[arg(long)]
    pub tree: bool,

    /// Show raw JSONL entries.
    #[arg(long)]
    pub raw: bool,

    /// Show specific entry by UUID.
    #[arg(long)]
    pub entry: Option<String>,

    /// Show file paths and locations.
    #[arg(long)]
    pub paths: bool,

    /// Preview first N messages from the session.
    #[arg(short = 'm', long)]
    pub messages: Option<usize>,

    /// Show files touched in this session (created, modified, read).
    #[arg(long)]
    pub files: bool,
}

/// Arguments for the TUI command.
#[derive(Debug, Default, Parser)]
pub struct TuiArgs {
    /// Start with specific project.
    #[arg(short = 'p', long)]
    pub project: Option<String>,

    /// Start with specific session (supports short prefixes like "780893e4").
    #[arg(short = 's', long)]
    pub session: Option<String>,

    /// Theme to use.
    #[arg(long, env = "SNATCH_TUI_THEME")]
    pub theme: Option<String>,

    /// Use ASCII-only characters (no Unicode box drawing).
    #[arg(long, env = "SNATCH_ASCII")]
    pub ascii: bool,
}

/// Arguments for the validate command.
#[derive(Debug, Parser)]
pub struct ValidateArgs {
    /// Session ID to validate (supports short prefixes like "780893e4").
    /// Optional with --all flag.
    pub session: Option<String>,

    /// Validate all sessions.
    #[arg(short = 'a', long)]
    pub all: bool,

    /// Check for schema compatibility.
    #[arg(long)]
    pub schema: bool,

    /// Report unknown fields.
    #[arg(long)]
    pub unknown_fields: bool,

    /// Check parent-child relationships.
    #[arg(long)]
    pub relationships: bool,
}

/// Arguments for the watch command.
#[derive(Debug, Parser)]
pub struct WatchArgs {
    /// Session ID to watch (supports short prefixes like "780893e4").
    pub session: Option<String>,

    /// Watch all active sessions.
    #[arg(short = 'a', long)]
    pub all: bool,

    /// Follow mode (like tail -f).
    #[arg(short = 'f', long)]
    pub follow: bool,

    /// Live dashboard mode with real-time stats display.
    #[arg(short = 'l', long)]
    pub live: bool,

    /// Polling interval in milliseconds.
    #[arg(long, default_value = "500")]
    pub interval: u64,
}

/// Arguments for the diff command.
#[derive(Debug, Parser)]
pub struct DiffArgs {
    /// First session ID (supports short prefixes like "780893e4").
    pub first: String,

    /// Second session ID (supports short prefixes like "780893e4").
    pub second: String,

    /// Only show summary, not details.
    #[arg(short = 's', long)]
    pub summary_only: bool,

    /// Don't show content of differing lines (line-based mode only).
    #[arg(long)]
    pub no_content: bool,

    /// Exit with code 1 if files differ.
    #[arg(short = 'e', long)]
    pub exit_code: bool,

    /// Use line-based diff instead of semantic diff.
    /// Line-based diff compares raw JSONL lines; semantic diff compares by message structure.
    #[arg(long)]
    pub line_based: bool,

    /// Show semantic diff (compare by message structure). This is the default.
    #[arg(long, hide = true)]
    pub semantic: bool,
}

/// Arguments for the config command.
#[derive(Debug, Parser)]
pub struct ConfigArgs {
    /// Config action to perform.
    #[command(subcommand)]
    pub action: ConfigAction,
}

/// Config subcommand actions.
#[derive(Debug, Subcommand)]
pub enum ConfigAction {
    /// Show all configuration values.
    #[command(alias = "list")]
    Show,

    /// Get a specific configuration value.
    Get {
        /// Configuration key (e.g., "theme.color").
        key: String,
    },

    /// Set a configuration value.
    Set {
        /// Configuration key (e.g., "theme.color").
        key: String,
        /// Value to set.
        value: String,
    },

    /// Show configuration file path.
    Path,

    /// Initialize configuration file with defaults.
    Init,

    /// Reset configuration to defaults.
    Reset,
}

/// Arguments for the cache command.
#[derive(Debug, Parser)]
pub struct CacheArgs {
    /// Cache action to perform.
    #[command(subcommand)]
    pub action: CacheAction,
}

/// Cache subcommand actions.
#[derive(Debug, Subcommand)]
pub enum CacheAction {
    /// Show cache statistics.
    Stats,

    /// Clear all cached data.
    Clear,

    /// Invalidate stale cache entries.
    Invalidate,

    /// Enable or disable caching.
    Status {
        /// Enable caching.
        #[arg(long, conflicts_with = "disable")]
        enable: bool,

        /// Disable caching.
        #[arg(long, conflicts_with = "enable")]
        disable: bool,
    },
}

/// Arguments for the extract command.
#[derive(Debug, Parser)]
pub struct ExtractArgs {
    /// Extract data for a specific project path.
    #[arg(short = 'p', long)]
    pub project: Option<String>,

    /// Output as pretty-printed JSON.
    #[arg(long)]
    pub pretty: bool,

    /// Show all data sources.
    #[arg(short = 'a', long)]
    pub all: bool,

    /// Include settings data.
    #[arg(long)]
    pub settings: bool,

    /// Include CLAUDE.md content.
    #[arg(long)]
    pub claude_md: bool,

    /// Include MCP configuration.
    #[arg(long)]
    pub mcp: bool,

    /// Include custom commands.
    #[arg(long)]
    pub commands: bool,

    /// Include rules.
    #[arg(long)]
    pub rules: bool,

    /// Include hooks configuration.
    #[arg(long)]
    pub hooks: bool,

    /// Include file history.
    #[arg(long)]
    pub file_history: bool,
}

/// Arguments for the index command.
#[derive(Debug, Parser)]
pub struct IndexArgs {
    /// Index subcommand to run.
    #[command(subcommand)]
    pub command: IndexSubcommand,
}

/// Index subcommand actions.
#[derive(Debug, Subcommand)]
pub enum IndexSubcommand {
    /// Build or update the search index.
    Build(IndexBuildArgs),

    /// Rebuild the index from scratch.
    Rebuild(IndexRebuildArgs),

    /// Show index status.
    Status,

    /// Clear the search index.
    Clear,

    /// Search the index (faster than regex search).
    Search(IndexSearchArgs),
}

/// Arguments for index build command.
#[derive(Debug, Parser)]
pub struct IndexBuildArgs {
    /// Only index sessions from specific project.
    #[arg(short = 'p', long)]
    pub project: Option<String>,
}

/// Arguments for index rebuild command.
#[derive(Debug, Parser)]
pub struct IndexRebuildArgs {
    /// Only rebuild sessions from specific project.
    #[arg(short = 'p', long)]
    pub project: Option<String>,
}

/// Arguments for index search command.
#[derive(Debug, Parser)]
pub struct IndexSearchArgs {
    /// Search query.
    pub query: String,

    /// Filter by message type.
    #[arg(short = 't', long = "type")]
    pub message_type: Option<String>,

    /// Filter by model.
    #[arg(short = 'm', long)]
    pub model: Option<String>,

    /// Filter by session ID (supports short prefixes like "780893e4").
    #[arg(short = 's', long)]
    pub session: Option<String>,

    /// Filter by tool name.
    #[arg(long = "tool-name")]
    pub tool_name: Option<String>,

    /// Include thinking blocks.
    #[arg(long)]
    pub thinking: bool,

    /// Maximum number of results.
    #[arg(short = 'n', long)]
    pub limit: Option<usize>,
}

/// Arguments for the cleanup command.
#[derive(Debug, Parser)]
pub struct CleanupArgs {
    /// Delete empty (0 byte) sessions.
    #[arg(long)]
    pub empty: bool,

    /// Delete sessions older than this date (YYYY-MM-DD or relative like "1week", "3months").
    #[arg(long)]
    pub older_than: Option<String>,

    /// Filter by project path (substring match).
    #[arg(short = 'p', long)]
    pub project: Option<String>,

    /// Include subagent sessions.
    #[arg(long)]
    pub subagents: bool,

    /// Preview what would be deleted without actually deleting.
    #[arg(long, alias = "dry-run")]
    pub preview: bool,

    /// Skip confirmation prompt.
    #[arg(long, short = 'y')]
    pub yes: bool,

    /// Show detailed output of each deleted session.
    #[arg(short = 'v', long)]
    pub verbose: bool,
}

/// Arguments for the tag command.
#[derive(Debug, Parser)]
pub struct TagArgs {
    /// Tag subcommand to run.
    #[command(subcommand)]
    pub action: TagAction,
}

/// Tag subcommand actions.
#[derive(Debug, Subcommand)]
pub enum TagAction {
    /// Add a tag to a session (or multiple sessions with filters).
    Add {
        /// Tag to add.
        tag: String,

        /// Session ID (supports short prefixes). Optional when using filters.
        #[arg(short = 's', long)]
        session: Option<String>,

        /// Tag all sessions modified since this date (e.g., "1week", "3days").
        #[arg(long)]
        since: Option<String>,

        /// Tag all sessions modified until this date.
        #[arg(long)]
        until: Option<String>,

        /// Filter to specific project (substring match).
        #[arg(short = 'p', long)]
        project: Option<String>,

        /// Preview what would be tagged without making changes.
        #[arg(long)]
        preview: bool,
    },

    /// Remove a tag from a session (or multiple sessions with filters).
    Remove {
        /// Tag to remove.
        tag: String,

        /// Session ID (supports short prefixes). Optional when using filters.
        #[arg(short = 's', long)]
        session: Option<String>,

        /// Remove from all sessions modified since this date.
        #[arg(long)]
        since: Option<String>,

        /// Remove from all sessions modified until this date.
        #[arg(long)]
        until: Option<String>,

        /// Filter to specific project (substring match).
        #[arg(short = 'p', long)]
        project: Option<String>,

        /// Preview what would be untagged without making changes.
        #[arg(long)]
        preview: bool,
    },

    /// Set a human-readable name for a session.
    Name {
        /// Session ID (supports short prefixes like "780893e4").
        session: String,
        /// Name to set (empty to clear).
        name: Option<String>,
    },

    /// List all tags or show tags for a session.
    List {
        /// Session ID to show tags for (optional).
        session: Option<String>,
    },

    /// Bookmark a session for quick access.
    Bookmark {
        /// Session ID (supports short prefixes like "780893e4").
        session: String,
    },

    /// Remove bookmark from a session.
    Unbookmark {
        /// Session ID (supports short prefixes like "780893e4").
        session: String,
    },

    /// List all bookmarked sessions.
    Bookmarks,

    /// Show sessions with a specific tag.
    Find {
        /// Tag to search for.
        tag: String,
    },

    /// Set outcome classification for a session.
    Outcome {
        /// Session ID (supports short prefixes like "780893e4").
        session: String,
        /// Outcome: success, partial, failed, abandoned (or clear to remove).
        #[arg(value_parser = parse_outcome)]
        outcome: Option<crate::tags::SessionOutcome>,
    },

    /// List sessions by outcome or show outcome statistics.
    Outcomes {
        /// Filter by specific outcome (success, partial, failed, abandoned).
        #[arg(value_parser = parse_outcome)]
        outcome: Option<crate::tags::SessionOutcome>,
    },

    /// Add a note to a session.
    Note {
        /// Session ID (supports short prefixes like "780893e4").
        session: String,
        /// Note text to add.
        text: String,
        /// Optional label/category for the note (e.g., "todo", "bug", "idea").
        #[arg(short = 'l', long)]
        label: Option<String>,
    },

    /// List notes for a session.
    Notes {
        /// Session ID to show notes for (supports short prefixes like "780893e4").
        session: String,
    },

    /// Remove a note from a session by index.
    Unnote {
        /// Session ID (supports short prefixes like "780893e4").
        session: String,
        /// Index of the note to remove (0-based, as shown in `notes` command).
        index: usize,
    },

    /// Clear all notes from a session.
    ClearNotes {
        /// Session ID (supports short prefixes like "780893e4").
        session: String,
    },

    /// Link two sessions together (marks them as continuations/related).
    Link {
        /// First session ID (supports short prefixes like "780893e4").
        session_a: String,
        /// Second session ID to link to.
        session_b: String,
    },

    /// Unlink two sessions.
    Unlink {
        /// First session ID (supports short prefixes like "780893e4").
        session_a: String,
        /// Second session ID to unlink from.
        session_b: String,
    },

    /// Show linked sessions for a session or list all linked sessions.
    Links {
        /// Session ID to show links for (optional - shows all if not specified).
        session: Option<String>,
    },

    /// Find sessions similar to a given session.
    Similar {
        /// Session ID to find similar sessions for (supports short prefixes).
        session: String,
        /// Maximum number of similar sessions to show.
        #[arg(short = 'n', long, default_value = "10")]
        limit: usize,
        /// Minimum similarity score (0-100, default 20).
        #[arg(long, default_value = "20")]
        threshold: u8,
        /// Weight for tool overlap similarity (0-100).
        #[arg(long, default_value = "30")]
        tool_weight: u8,
        /// Weight for project match similarity (0-100).
        #[arg(long, default_value = "25")]
        project_weight: u8,
        /// Weight for time proximity similarity (0-100).
        #[arg(long, default_value = "15")]
        time_weight: u8,
        /// Weight for tag overlap similarity (0-100).
        #[arg(long, default_value = "20")]
        tag_weight: u8,
        /// Weight for token count similarity (0-100).
        #[arg(long, default_value = "10")]
        token_weight: u8,
    },
}

/// Parse an outcome value from string.
fn parse_outcome(s: &str) -> std::result::Result<crate::tags::SessionOutcome, String> {
    s.parse()
}

/// Arguments for the code command.
#[derive(Debug, Parser)]
pub struct CodeArgs {
    /// Session ID to extract code from (supports short prefixes like "780893e4").
    pub session: String,

    /// Filter by programming language (e.g., "rust", "python", "typescript").
    #[arg(short = 'l', long)]
    pub lang: Option<String>,

    /// Only extract code from assistant messages.
    #[arg(long)]
    pub assistant_only: bool,

    /// Only extract code from user messages.
    #[arg(long)]
    pub user_only: bool,

    /// Maximum number of code blocks to extract.
    #[arg(short = 'n', long)]
    pub limit: Option<usize>,

    /// Only extract from main thread (exclude branches).
    #[arg(long)]
    pub main_thread: bool,

    /// Include metadata (source, timestamp, index) with each block.
    #[arg(short = 'm', long)]
    pub metadata: bool,

    /// Concatenate all code blocks into a single output.
    #[arg(short = 'c', long)]
    pub concatenate: bool,

    /// Write each code block to a separate file.
    #[arg(short = 'f', long)]
    pub files: bool,

    /// Output directory for files (with --files). Default: current directory.
    #[arg(short = 'O', long)]
    pub output_dir: Option<std::path::PathBuf>,

    /// Suppress progress messages.
    #[arg(short = 'q', long)]
    pub quiet: bool,
}

/// Arguments for the prompts command.
#[derive(Debug, Parser)]
pub struct PromptsArgs {
    /// Session ID to extract prompts from (supports short prefixes).
    pub session: Option<String>,

    /// Extract prompts from all matching sessions.
    #[arg(long)]
    pub all: bool,

    /// Filter by project path (substring match).
    #[arg(short = 'p', long)]
    pub project: Option<String>,

    /// Only include sessions modified since this date (YYYY-MM-DD or relative like "1week").
    #[arg(long)]
    pub since: Option<String>,

    /// Only include sessions modified before this date.
    #[arg(long)]
    pub until: Option<String>,

    /// Minimum prompt length to include (filters out short/empty prompts).
    #[arg(long, default_value = "10")]
    pub min_length: usize,

    /// Maximum number of prompts to extract.
    #[arg(short = 'n', long)]
    pub limit: Option<usize>,

    /// Include subagent sessions.
    #[arg(long)]
    pub subagents: bool,

    /// Output file path (default: stdout).
    #[arg(short = 'O', long = "file")]
    pub output_file: Option<PathBuf>,

    /// Add session separator comments between sessions.
    #[arg(long)]
    pub separators: bool,

    /// Include timestamps for each prompt.
    #[arg(long)]
    pub timestamps: bool,

    /// Number formatting style (plain text lines, or numbered).
    #[arg(long)]
    pub numbered: bool,
}

/// Arguments for the standup command.
#[derive(Debug, Parser)]
pub struct StandupArgs {
    /// Time period for the report (e.g., "24h", "1d", "7d", "1w").
    /// Default is 24 hours.
    #[arg(long, short = 'p', default_value = "24h")]
    pub period: String,

    /// Filter by project path (substring match).
    #[arg(long)]
    pub project: Option<String>,

    /// Include token usage statistics.
    #[arg(long)]
    pub usage: bool,

    /// Include file modification summary.
    #[arg(long)]
    pub files: bool,

    /// Include tool usage breakdown.
    #[arg(long)]
    pub tools: bool,

    /// Show all details (equivalent to --usage --files --tools).
    #[arg(long)]
    pub all: bool,

    /// Output format: text, json, markdown.
    #[arg(long, short = 'f', value_enum, default_value = "text")]
    pub format: StandupFormat,

    /// Copy output to clipboard.
    #[arg(long)]
    pub clipboard: bool,
}

/// Output formats for standup reports.
#[derive(Debug, Clone, Copy, ValueEnum, Default)]
pub enum StandupFormat {
    /// Plain text with sections.
    #[default]
    Text,
    /// Markdown with bullet points (great for Slack/Teams).
    Markdown,
    /// JSON for programmatic use.
    Json,
}

/// Arguments for the pick command.
#[derive(Debug, Parser)]
pub struct PickArgs {
    /// Filter by project path (substring match).
    #[arg(short = 'p', long)]
    pub project: Option<String>,

    /// Include subagent sessions.
    #[arg(long)]
    pub subagents: bool,

    /// Maximum number of sessions to show in the picker.
    #[arg(short = 'n', long)]
    pub limit: Option<usize>,

    /// Action to perform after selection.
    #[arg(short = 'a', long, default_value = "export")]
    pub action: PickAction,
}

/// Actions available after picking a session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, ValueEnum)]
pub enum PickAction {
    /// Print session ID (for piping to other commands).
    #[default]
    Export,
    /// Show session info.
    Info,
    /// Show session stats.
    Stats,
    /// Print session file path.
    Open,
}

/// Arguments for the quickstart command.
#[derive(Debug, Parser)]
pub struct QuickstartArgs {
    /// Which topic to learn about.
    #[arg(default_value = "overview")]
    pub topic: QuickstartTopic,

    /// Show more detailed examples.
    #[arg(short = 'v', long)]
    pub verbose: bool,
}

/// Topics available in the quickstart guide.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, ValueEnum)]
pub enum QuickstartTopic {
    /// Overview and 5-minute getting started guide.
    #[default]
    Overview,
    /// Exploring sessions and projects.
    Explore,
    /// Exporting conversations in various formats.
    Export,
    /// Searching across sessions.
    Search,
    /// Understanding usage statistics.
    Stats,
    /// Interactive TUI browser.
    Tui,
    /// Common workflow recipes.
    Workflows,
    /// All topics in sequence.
    All,
}

/// Arguments for the summary command.
#[derive(Debug, Parser)]
pub struct SummaryArgs {
    /// Time period for the summary (e.g., "24h", "1d", "7d", "1w").
    /// Default is 24 hours.
    #[arg(long, short = 'p', default_value = "24h")]
    pub period: String,
}

/// Arguments for the recent command.
#[derive(Debug, Parser)]
pub struct RecentArgs {
    /// Number of recent sessions to show.
    #[arg(short = 'n', long, default_value = "5")]
    pub count: usize,

    /// Filter to specific project (substring match).
    #[arg(short = 'p', long)]
    pub project: Option<String>,
}

/// Arguments for the MCP server command.
#[cfg(feature = "mcp")]
#[derive(Debug, Parser)]
pub struct ServeMcpArgs {
    // No additional arguments - uses global claude_dir and max_file_size
}

/// Initialize tracing/logging based on CLI options.
fn init_logging(cli: &Cli) {
    use tracing_subscriber::{
        fmt::{self, format::FmtSpan},
        layer::SubscriberExt,
        util::SubscriberInitExt,
        EnvFilter,
    };

    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new(cli.log_level.to_filter_string()));

    // Build subscriber based on log format
    let result = match cli.log_format {
        LogFormat::Json => {
            // Structured JSON format for machine consumption
            let layer = fmt::layer()
                .json()
                .with_span_events(FmtSpan::CLOSE)
                .with_thread_ids(true)
                .with_file(true)
                .with_line_number(true)
                .with_writer(std::io::stderr);
            tracing_subscriber::registry()
                .with(filter)
                .with(layer)
                .try_init()
        }
        LogFormat::Compact => {
            // Compact single-line format
            let layer = fmt::layer()
                .compact()
                .with_target(false)
                .with_writer(std::io::stderr);
            tracing_subscriber::registry()
                .with(filter)
                .with(layer)
                .try_init()
        }
        LogFormat::Pretty => {
            // Pretty format with full details
            let layer = fmt::layer()
                .pretty()
                .with_thread_ids(true)
                .with_file(true)
                .with_line_number(true)
                .with_writer(std::io::stderr);
            tracing_subscriber::registry()
                .with(filter)
                .with(layer)
                .try_init()
        }
        LogFormat::Text => {
            // Default human-readable text format
            let layer = fmt::layer().with_writer(std::io::stderr);
            tracing_subscriber::registry()
                .with(filter)
                .with(layer)
                .try_init()
        }
    };

    // Silently ignore "already set" errors - this is normal when a subscriber
    // was already configured (e.g., by tests or environment)
    let _ = result;
}

/// Initialize rayon thread pool with custom thread count if specified.
fn init_thread_pool(threads: Option<usize>) {
    if let Some(num_threads) = threads {
        if num_threads > 0 {
            // Configure rayon's global thread pool
            rayon::ThreadPoolBuilder::new()
                .num_threads(num_threads)
                .build_global()
                .ok(); // Ignore error if already initialized
        }
    }
}

/// Run the CLI application.
pub fn run() -> Result<()> {
    let cli = Cli::parse();

    // Initialize thread pool first (before any parallel operations)
    init_thread_pool(cli.threads);

    // Initialize logging
    init_logging(&cli);

    // Initialize the cache from configuration
    let config = match &cli.config {
        Some(path) => Config::load_from(path).unwrap_or_else(|e| {
            eprintln!("Warning: Failed to load config from {}: {}", path.display(), e);
            Config::default()
        }),
        None => Config::load().unwrap_or_default(),
    };
    init_global_cache(&config.cache);

    match &cli.command {
        // No command provided - launch TUI if interactive, otherwise show quick summary
        None => {
            if io::stdout().is_terminal() && io::stdin().is_terminal() {
                let tui_args = TuiArgs::default();
                commands::tui::run(&cli, &tui_args)
            } else {
                commands::summary::run_quick_summary(&cli)
            }
        }
        Some(Commands::List(args)) => commands::list::run(&cli, args),
        Some(Commands::Export(args)) => commands::export::run(&cli, args),
        Some(Commands::Search(args)) => commands::search::run(&cli, args),
        Some(Commands::Stats(args)) => commands::stats::run(&cli, args),
        Some(Commands::Info(args)) => commands::info::run(&cli, args),
        Some(Commands::Tui(args)) => commands::tui::run(&cli, args),
        Some(Commands::Validate(args)) => commands::validate::run(&cli, args),
        Some(Commands::Watch(args)) => commands::watch::run(&cli, args),
        Some(Commands::Diff(args)) => commands::diff::run(&cli, args),
        Some(Commands::Config(args)) => commands::config::run(&cli, args),
        Some(Commands::Extract(args)) => commands::extract::run(&cli, args),
        Some(Commands::Cache(args)) => commands::cache::run(&cli, args),
        Some(Commands::Index(args)) => commands::index::run(&cli, args),
        Some(Commands::Completions(args)) => {
            generate_completions(args.shell);
            Ok(())
        }
        Some(Commands::DynamicCompletions(args)) => {
            // Convert to internal types
            let comp_type = match args.completion_type {
                DynamicCompletionType::Sessions => commands::completions::CompletionType::Sessions,
                DynamicCompletionType::Projects => commands::completions::CompletionType::Projects,
                DynamicCompletionType::Tools => commands::completions::CompletionType::Tools,
                DynamicCompletionType::Formats => commands::completions::CompletionType::Formats,
                DynamicCompletionType::Models => commands::completions::CompletionType::Models,
            };
            let internal_args = commands::completions::DynamicCompletionsArgs {
                completion_type: comp_type,
                prefix: args.prefix.clone(),
                limit: Some(args.limit),
            };
            commands::completions::run(&cli, &internal_args)
        }
        Some(Commands::Cleanup(args)) => commands::cleanup::run(&cli, args),
        Some(Commands::Tag(args)) => commands::tag::run(&cli, args),
        Some(Commands::Code(args)) => commands::code::run(&cli, args),
        Some(Commands::Prompts(args)) => commands::prompts::run(&cli, args),
        Some(Commands::Standup(args)) => commands::standup::run(&cli, args),
        Some(Commands::Pick(args)) => commands::pick::run(&cli, args),
        Some(Commands::Quickstart(args)) => commands::quickstart::run(&cli, args),
        Some(Commands::Summary(args)) => commands::summary::run(&cli, args),
        Some(Commands::Recent(args)) => commands::recent::run(&cli, args),
        #[cfg(feature = "mcp")]
        Some(Commands::ServeMcp(_)) => {
            // Run the MCP server
            let rt = tokio::runtime::Runtime::new().map_err(|e| {
                SnatchError::ExportError {
                    message: format!("Failed to create tokio runtime: {e}"),
                    source: None,
                }
            })?;
            rt.block_on(crate::mcp_server::run_server(cli.claude_dir.clone(), cli.max_file_size))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn verify_cli() {
        Cli::command().debug_assert();
    }

    #[test]
    fn test_export_format_conversion() {
        assert_eq!(
            ExportFormat::from(ExportFormatArg::Markdown),
            ExportFormat::Markdown
        );
        assert_eq!(
            ExportFormat::from(ExportFormatArg::Json),
            ExportFormat::Json
        );
    }

    #[test]
    fn test_log_format_variants() {
        assert_eq!(LogFormat::default(), LogFormat::Text);
        assert!(matches!(LogFormat::Json, LogFormat::Json));
        assert!(matches!(LogFormat::Compact, LogFormat::Compact));
        assert!(matches!(LogFormat::Pretty, LogFormat::Pretty));
    }

    #[test]
    fn test_log_level_to_filter() {
        assert_eq!(LogLevel::Error.to_filter_string(), "error");
        assert_eq!(LogLevel::Warn.to_filter_string(), "warn");
        assert_eq!(LogLevel::Info.to_filter_string(), "info");
        assert_eq!(LogLevel::Debug.to_filter_string(), "debug");
        assert_eq!(LogLevel::Trace.to_filter_string(), "trace");
    }
}
