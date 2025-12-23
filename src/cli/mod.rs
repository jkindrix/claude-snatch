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
    #[arg(short = 'd', long, global = true)]
    pub claude_dir: Option<PathBuf>,

    /// Output format for structured data.
    #[arg(short = 'o', long, global = true, default_value = "text")]
    pub output: OutputFormat,

    /// Enable verbose output.
    #[arg(short = 'v', long, global = true)]
    pub verbose: bool,

    /// Suppress non-essential output.
    #[arg(short = 'q', long, global = true)]
    pub quiet: bool,

    /// Enable colored output (auto-detected by default).
    #[arg(long, global = true)]
    pub color: Option<bool>,

    /// Output as JSON (shorthand for -o json).
    #[arg(long, global = true)]
    pub json: bool,
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

    /// Generate shell completions.
    Completions(CompletionsArgs),
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
    /// Session ID or path to export.
    pub session: String,

    /// Output file path (stdout if not specified).
    #[arg(short = 'o', long)]
    pub output: Option<PathBuf>,

    /// Export format.
    #[arg(short = 'f', long, default_value = "markdown")]
    pub format: ExportFormatArg,

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
}

impl From<ExportFormatArg> for ExportFormat {
    fn from(arg: ExportFormatArg) -> Self {
        match arg {
            ExportFormatArg::Markdown | ExportFormatArg::Md => ExportFormat::Markdown,
            ExportFormatArg::Json => ExportFormat::Json,
            ExportFormatArg::JsonPretty => ExportFormat::JsonPretty,
            ExportFormatArg::Text | ExportFormatArg::Jsonl => ExportFormat::Text,
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
    #[arg(long)]
    pub theme: Option<String>,
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

/// Run the CLI application.
pub fn run() -> Result<()> {
    let cli = Cli::parse();

    match &cli.command {
        Commands::List(args) => commands::list::run(&cli, args),
        Commands::Export(args) => commands::export::run(&cli, args),
        Commands::Search(args) => commands::search::run(&cli, args),
        Commands::Stats(args) => commands::stats::run(&cli, args),
        Commands::Info(args) => commands::info::run(&cli, args),
        Commands::Tui(args) => commands::tui::run(&cli, args),
        Commands::Validate(args) => commands::validate::run(&cli, args),
        Commands::Watch(args) => commands::watch::run(&cli, args),
        Commands::Completions(args) => {
            generate_completions(args.shell);
            Ok(())
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
}
