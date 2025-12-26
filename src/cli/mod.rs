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

use clap::{CommandFactory, Parser, Subcommand, ValueEnum};
use clap_complete::{generate, Shell};
use std::io;
use std::path::PathBuf;

use crate::cache::init_global_cache;
use crate::config::Config;
use crate::error::Result;
use crate::export::ExportFormat;

/// Claude Code conversation extractor with maximum data fidelity.
#[derive(Debug, Parser)]
#[command(name = "snatch")]
#[command(author, version, about, long_about = None)]
#[command(propagate_version = true)]
pub struct Cli {
    /// Subcommand to run.
    #[command(subcommand)]
    pub command: Commands,

    /// Path to Claude directory (default: ~/.claude).
    #[arg(short = 'd', long, global = true, env = "SNATCH_CLAUDE_DIR")]
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
    #[arg(long, global = true, env = "SNATCH_COLOR")]
    pub color: Option<bool>,

    /// Output as JSON (shorthand for -o json).
    #[arg(long, global = true, env = "SNATCH_JSON")]
    pub json: bool,

    /// Log level (error, warn, info, debug, trace).
    #[arg(long, global = true, default_value = "warn", env = "SNATCH_LOG_LEVEL")]
    pub log_level: LogLevel,

    /// Log format (text, json, compact, pretty).
    #[arg(long, global = true, default_value = "text", env = "SNATCH_LOG_FORMAT")]
    pub log_format: LogFormat,

    /// Log output file (default: stderr).
    #[arg(long, global = true, env = "SNATCH_LOG_FILE")]
    pub log_file: Option<std::path::PathBuf>,

    /// Number of threads for parallel processing (default: number of CPUs).
    #[arg(short = 'j', long, global = true, env = "SNATCH_THREADS")]
    pub threads: Option<usize>,

    /// Path to custom configuration file.
    #[arg(long, global = true, env = "SNATCH_CONFIG")]
    pub config: Option<PathBuf>,
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

    /// Limit number of results.
    #[arg(short = 'n', long)]
    pub limit: Option<usize>,

    /// Show full UUIDs instead of short IDs.
    #[arg(long)]
    pub full_ids: bool,

    /// Show file sizes.
    #[arg(long)]
    pub sizes: bool,

    /// Pipe output through a pager (less/more).
    #[arg(long)]
    pub pager: bool,
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

/// Arguments for the export command.
#[derive(Debug, Parser)]
pub struct ExportArgs {
    /// Session ID or path to export (optional with --all).
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
    pub include_agents: bool,

    /// Combine parent session with its subagent transcripts (interleaved by time).
    #[arg(long)]
    pub combine_agents: bool,

    /// Include thinking blocks.
    #[arg(long, default_value = "true")]
    pub thinking: bool,

    /// Include tool use blocks.
    #[arg(long, default_value = "true")]
    pub tool_use: bool,

    /// Include tool results.
    #[arg(long, default_value = "true")]
    pub tool_results: bool,

    /// Include system messages.
    #[arg(long)]
    pub system: bool,

    /// Include timestamps.
    #[arg(long, default_value = "true")]
    pub timestamps: bool,

    /// Include usage statistics.
    #[arg(long, default_value = "true")]
    pub usage: bool,

    /// Include metadata (UUIDs, etc.).
    #[arg(long)]
    pub metadata: bool,

    /// Only export main thread (exclude branches).
    #[arg(long, default_value = "true")]
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

    /// Search in specific session.
    #[arg(short = 's', long)]
    pub session: Option<String>,

    /// Case-insensitive search.
    #[arg(short = 'i', long)]
    pub ignore_case: bool,

    /// Search in thinking blocks.
    #[arg(long)]
    pub thinking: bool,

    /// Search in tool outputs.
    #[arg(long)]
    pub tools: bool,

    /// Search everywhere (user, assistant, thinking, tools).
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
    /// Show stats for specific project.
    #[arg(short = 'p', long)]
    pub project: Option<String>,

    /// Show stats for specific session.
    #[arg(short = 's', long)]
    pub session: Option<String>,

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

    /// Show all detailed stats.
    #[arg(short = 'a', long)]
    pub all: bool,
}

/// Arguments for the info command.
#[derive(Debug, Parser)]
pub struct InfoArgs {
    /// Session ID or project path to show info for.
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
}

/// Arguments for the TUI command.
#[derive(Debug, Parser)]
pub struct TuiArgs {
    /// Start with specific project.
    #[arg(short = 'p', long)]
    pub project: Option<String>,

    /// Start with specific session.
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
    /// Session ID or path to validate.
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
    /// Session ID to watch.
    pub session: Option<String>,

    /// Watch all active sessions.
    #[arg(short = 'a', long)]
    pub all: bool,

    /// Follow mode (like tail -f).
    #[arg(short = 'f', long)]
    pub follow: bool,

    /// Polling interval in milliseconds.
    #[arg(long, default_value = "500")]
    pub interval: u64,
}

/// Arguments for the diff command.
#[derive(Debug, Parser)]
pub struct DiffArgs {
    /// First session ID or file path.
    pub first: String,

    /// Second session ID or file path.
    pub second: String,

    /// Only show summary, not details.
    #[arg(short = 's', long)]
    pub summary_only: bool,

    /// Don't show content of differing lines.
    #[arg(long)]
    pub no_content: bool,

    /// Exit with code 1 if files differ.
    #[arg(short = 'e', long)]
    pub exit_code: bool,

    /// Show semantic diff (compare by message structure).
    #[arg(long)]
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

    /// Filter by session ID.
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

    if let Err(e) = result {
        eprintln!("Warning: Could not initialize logging: {e}");
    }
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
        Commands::List(args) => commands::list::run(&cli, args),
        Commands::Export(args) => commands::export::run(&cli, args),
        Commands::Search(args) => commands::search::run(&cli, args),
        Commands::Stats(args) => commands::stats::run(&cli, args),
        Commands::Info(args) => commands::info::run(&cli, args),
        Commands::Tui(args) => commands::tui::run(&cli, args),
        Commands::Validate(args) => commands::validate::run(&cli, args),
        Commands::Watch(args) => commands::watch::run(&cli, args),
        Commands::Diff(args) => commands::diff::run(&cli, args),
        Commands::Config(args) => commands::config::run(&cli, args),
        Commands::Extract(args) => commands::extract::run(&cli, args),
        Commands::Cache(args) => commands::cache::run(&cli, args),
        Commands::Index(args) => commands::index::run(&cli, args),
        Commands::Completions(args) => {
            generate_completions(args.shell);
            Ok(())
        }
        Commands::DynamicCompletions(args) => {
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
