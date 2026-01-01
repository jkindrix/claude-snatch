//! Quickstart guide for new users.
//!
//! Provides interactive examples and common workflows to help new users
//! get started with claude-snatch quickly.

use crate::cli::{Cli, OutputFormat, QuickstartArgs, QuickstartTopic};
use crate::error::Result;

/// Run the quickstart command.
pub fn run(cli: &Cli, args: &QuickstartArgs) -> Result<()> {
    match cli.effective_output() {
        OutputFormat::Json => show_json(args),
        _ => show_text(args),
    }
}

fn show_json(args: &QuickstartArgs) -> Result<()> {
    use serde_json::json;

    let topics = match args.topic {
        QuickstartTopic::All => vec![
            QuickstartTopic::Overview,
            QuickstartTopic::Explore,
            QuickstartTopic::Export,
            QuickstartTopic::Search,
            QuickstartTopic::Stats,
            QuickstartTopic::Tui,
            QuickstartTopic::Workflows,
        ],
        topic => vec![topic],
    };

    let content: Vec<_> = topics
        .iter()
        .map(|t| {
            json!({
                "topic": format!("{:?}", t).to_lowercase(),
                "title": topic_title(*t),
                "examples": topic_examples(*t, args.verbose),
            })
        })
        .collect();

    println!("{}", serde_json::to_string_pretty(&content)?);
    Ok(())
}

fn show_text(args: &QuickstartArgs) -> Result<()> {
    match args.topic {
        QuickstartTopic::Overview => show_overview(args.verbose),
        QuickstartTopic::Explore => show_explore(args.verbose),
        QuickstartTopic::Export => show_export(args.verbose),
        QuickstartTopic::Search => show_search(args.verbose),
        QuickstartTopic::Stats => show_stats(args.verbose),
        QuickstartTopic::Tui => show_tui(args.verbose),
        QuickstartTopic::Workflows => show_workflows(args.verbose),
        QuickstartTopic::All => {
            show_overview(args.verbose);
            println!("\n{}\n", "=".repeat(60));
            show_explore(args.verbose);
            println!("\n{}\n", "=".repeat(60));
            show_export(args.verbose);
            println!("\n{}\n", "=".repeat(60));
            show_search(args.verbose);
            println!("\n{}\n", "=".repeat(60));
            show_stats(args.verbose);
            println!("\n{}\n", "=".repeat(60));
            show_tui(args.verbose);
            println!("\n{}\n", "=".repeat(60));
            show_workflows(args.verbose);
        }
    }
    Ok(())
}

fn topic_title(topic: QuickstartTopic) -> &'static str {
    match topic {
        QuickstartTopic::Overview => "Getting Started in 5 Minutes",
        QuickstartTopic::Explore => "Exploring Sessions and Projects",
        QuickstartTopic::Export => "Exporting Conversations",
        QuickstartTopic::Search => "Searching Sessions",
        QuickstartTopic::Stats => "Usage Statistics",
        QuickstartTopic::Tui => "Interactive TUI Browser",
        QuickstartTopic::Workflows => "Common Workflows",
        QuickstartTopic::All => "Complete Guide",
    }
}

fn topic_examples(topic: QuickstartTopic, verbose: bool) -> Vec<&'static str> {
    match topic {
        QuickstartTopic::Overview => {
            if verbose {
                vec![
                    "snatch list",
                    "snatch list --project /path/to/project",
                    "snatch export <session-id>",
                    "snatch export <session-id> -f json -O output.json",
                    "snatch stats --global",
                ]
            } else {
                vec!["snatch list", "snatch export <session-id>", "snatch stats"]
            }
        }
        QuickstartTopic::Explore => vec![
            "snatch list",
            "snatch list projects",
            "snatch info <session-id>",
        ],
        QuickstartTopic::Export => vec![
            "snatch export <session-id>",
            "snatch export <session-id> -f json",
            "snatch export <session-id> -f html --toc",
        ],
        QuickstartTopic::Search => vec![
            "snatch search \"pattern\"",
            "snatch search \"error\" -i",
            "snatch index search \"query\"",
        ],
        QuickstartTopic::Stats => vec![
            "snatch stats",
            "snatch stats --global --costs",
            "snatch stats --blocks",
        ],
        QuickstartTopic::Tui => vec!["snatch tui", "snatch tui --session <id>"],
        QuickstartTopic::Workflows => vec![
            "snatch prompts --all > prompts.txt",
            "snatch standup --period 24h",
            "snatch export --all -f sqlite -O archive.db",
        ],
        QuickstartTopic::All => vec![],
    }
}

fn show_overview(verbose: bool) {
    println!(
        r#"
CLAUDE-SNATCH: Getting Started in 5 Minutes
============================================

Snatch extracts and analyzes your Claude Code conversations with maximum
data fidelity. Here's how to get started:

STEP 1: List Your Sessions
--------------------------
  snatch list

  This shows your most recent Claude Code sessions. You'll see:
  - Session ID (short 8-char prefix you can use in other commands)
  - Project path
  - Last modified time
  - Message count

STEP 2: Export a Conversation
-----------------------------
  snatch export <session-id>

  Replace <session-id> with one from the list (e.g., "abc12345").
  By default, exports to Markdown on stdout.

  Common variations:
    snatch export abc12345 -O conversation.md    # Save to file
    snatch export abc12345 -f json               # Export as JSON
    snatch export abc12345 -f html --toc         # HTML with sidebar

STEP 3: View Statistics
-----------------------
  snatch stats --global

  See your total token usage and estimated costs across all sessions.

QUICK REFERENCE
---------------
  snatch list              List sessions
  snatch export <id>       Export a conversation
  snatch search "query"    Search across sessions
  snatch stats             Usage statistics
  snatch tui               Interactive browser
  snatch info <id>         Session details
  snatch quickstart <topic> More help on specific topics

AVAILABLE TOPICS
----------------
  snatch quickstart explore     Finding sessions and projects
  snatch quickstart export      All export formats and options
  snatch quickstart search      Search and filtering
  snatch quickstart stats       Understanding usage data
  snatch quickstart tui         Interactive TUI guide
  snatch quickstart workflows   Common recipes and integrations
  snatch quickstart all         Show everything"#
    );

    if verbose {
        println!(
            r#"

ADDITIONAL TIPS
---------------
- Use short IDs: "abc12345" instead of full UUIDs
- Most commands support -o json for machine-readable output
- Add --help to any command for full documentation
- Use snatch pick for fuzzy session selection"#
        );
    }
}

fn show_explore(verbose: bool) {
    println!(
        r#"
EXPLORING SESSIONS AND PROJECTS
===============================

LIST SESSIONS
-------------
  snatch list                     # Recent sessions (default: 50)
  snatch list -n 100              # Show more results
  snatch list -n 0                # Show all (no limit)
  snatch list --active            # Only active sessions
  snatch list --since 1week       # Modified in last week

LIST PROJECTS
-------------
  snatch list projects            # Show all projects

FILTER BY PROJECT
-----------------
  snatch list -p /path/to/project # Filter by project path
  snatch list -p myproject        # Substring match works too

SESSION DETAILS
---------------
  snatch info <session-id>        # Full session information
  snatch info <session-id> --tree # Show conversation structure
  snatch info <session-id> --raw  # Raw JSONL entries"#
    );

    if verbose {
        println!(
            r#"

SORT OPTIONS
------------
  snatch list -s modified         # By modification time (default)
  snatch list -s oldest           # Oldest first
  snatch list -s size             # By file size
  snatch list -s name             # Alphabetically

OUTPUT FORMATS
--------------
  snatch list -o json             # JSON output
  snatch list -o tsv              # Tab-separated (for scripts)
  snatch list -o compact          # Single-line per entry"#
        );
    }
}

fn show_export(verbose: bool) {
    println!(
        r#"
EXPORTING CONVERSATIONS
=======================

BASIC EXPORT
------------
  snatch export <session-id>              # Markdown to stdout
  snatch export <session-id> -O out.md    # Save to file

FORMATS
-------
  snatch export <id> -f markdown   # Markdown (default)
  snatch export <id> -f json       # Structured JSON
  snatch export <id> -f html       # Standalone HTML page
  snatch export <id> -f text       # Plain text
  snatch export <id> -f csv        # Tabular CSV
  snatch export <id> -f xml        # XML markup
  snatch export <id> -f sqlite     # SQLite database
  snatch export <id> -f jsonl      # Original JSONL format

CONTENT OPTIONS
---------------
  snatch export <id> --no-thinking       # Exclude thinking blocks
  snatch export <id> --no-tool-use       # Exclude tool invocations
  snatch export <id> --no-tool-results   # Exclude tool outputs
  snatch export <id> --only user         # Only user messages
  snatch export <id> --only prompts      # Only human-typed prompts

BULK EXPORT
-----------
  snatch export --all                    # Export all sessions
  snatch export --all -p /my/project     # All from specific project
  snatch export --all --since 1week      # Recent sessions only"#
    );

    if verbose {
        println!(
            r#"

SPECIAL OPTIONS
---------------
  snatch export <id> --lossless          # Preserve ALL data
  snatch export <id> --combine-agents    # Include subagent sessions
  snatch export <id> --main-thread       # Only main conversation thread
  snatch export <id> --pretty            # Pretty-print JSON
  snatch export <id> --metadata          # Include UUIDs, etc.

HTML OPTIONS
------------
  snatch export <id> -f html --toc       # Table of contents sidebar
  snatch export <id> -f html --dark      # Dark theme

SECURITY
--------
  snatch export <id> --redact security   # Redact API keys, passwords
  snatch export <id> --redact all        # Also redact emails, IPs
  snatch export <id> --warn-pii          # Warn about PII (no redaction)
  snatch export <id> --redact security --redact-preview
                                         # Preview what would be redacted

SHARING
-------
  snatch export <id> --gist              # Upload to GitHub Gist
  snatch export <id> --clipboard         # Copy to clipboard"#
        );
    }
}

fn show_search(verbose: bool) {
    println!(
        r#"
SEARCHING SESSIONS
==================

BASIC SEARCH
------------
  snatch search "pattern"             # Search in all sessions
  snatch search "error" -i            # Case-insensitive
  snatch search "function.*async"     # Regex supported

FILTER SEARCH
-------------
  snatch search "bug" -p /my/project  # Specific project
  snatch search "fix" -s abc12345     # Specific session
  snatch search "api" -t user         # Only user messages
  snatch search "model" -m opus       # By model used

SEARCH OPTIONS
--------------
  snatch search "term" -C 5           # Show 5 context lines
  snatch search "term" -n 10          # Limit to 10 results
  snatch search "term" -l             # Show session IDs only
  snatch search "term" -c             # Count matches only

FUZZY SEARCH
------------
  snatch search "implmentation" -f    # Fuzzy matching (typo-tolerant)
  snatch search "auth" -f --fuzzy-threshold 70  # Adjust sensitivity"#
    );

    if verbose {
        println!(
            r#"

INDEXED SEARCH (Faster)
-----------------------
  snatch index build                  # Build search index first
  snatch index search "query"         # Fast full-text search
  snatch index status                 # Check index status

ADVANCED FILTERS
----------------
  snatch search "term" --thinking     # Also search thinking blocks
  snatch search "term" --tools        # Also search tool outputs
  snatch search "term" -a             # Search everywhere
  snatch search "term" --errors       # Only messages with errors
  snatch search "term" --tool-name Bash  # Filter by tool name"#
        );
    }
}

fn show_stats(verbose: bool) {
    println!(
        r#"
USAGE STATISTICS
================

BASIC STATS
-----------
  snatch stats                        # Stats for recent sessions
  snatch stats --global               # Global totals
  snatch stats -s <session-id>        # Specific session
  snatch stats -p /my/project         # Specific project

DETAILED VIEWS
--------------
  snatch stats --tools                # Tool usage breakdown
  snatch stats --models               # Model usage breakdown
  snatch stats --costs                # Cost estimates
  snatch stats -a                     # All of the above

BILLING BLOCKS
--------------
  snatch stats --blocks               # 5-hour billing windows
  snatch stats --blocks --token-limit 500000  # With usage gauge"#
    );

    if verbose {
        println!(
            r#"

VISUALIZATIONS
--------------
  snatch stats --sparkline            # Usage trend sparklines
  snatch stats --timeline             # Activity timeline
  snatch stats --graph                # Token usage graph

COST TRACKING
-------------
  snatch stats --record               # Record current stats to history
  snatch stats --history              # View historical costs
  snatch stats --history --days 7     # Last 7 days
  snatch stats --weekly               # Weekly aggregation
  snatch stats --monthly              # Monthly aggregation

EXPORT
------
  snatch stats --csv                  # Export as CSV
  snatch stats -o json                # JSON output"#
        );
    }
}

fn show_tui(verbose: bool) {
    println!(
        r#"
INTERACTIVE TUI BROWSER
=======================

LAUNCH
------
  snatch tui                          # Open TUI browser
  snatch tui -p /my/project           # Start with project
  snatch tui -s abc12345              # Open specific session

NAVIGATION
----------
  j/k or Up/Down    Move through list
  h/l or Left/Right Switch panels
  Enter             Select/expand item
  Esc               Go back
  1/2/3             Focus specific panel

SEARCH
------
  /                 Start search
  n/N               Next/previous result
  Enter             Confirm search
  Esc               Cancel search

ACTIONS
-------
  e                 Export current session
  c                 Copy message to clipboard
  C                 Copy code block
  r                 Refresh

DISPLAY
-------
  t                 Toggle thinking blocks
  o                 Toggle tool outputs
  w                 Toggle word wrap
  #                 Toggle line numbers
  T                 Cycle theme
  ?                 Toggle help panel

FILTERS
-------
  f                 Toggle filter panel
  F/B               Cycle message type filter
  E                 Toggle errors-only
  M                 Filter by model
  [/]               Set date range
  X                 Clear all filters"#
    );

    if verbose {
        println!(
            r#"

THEMES
------
  snatch tui --theme dark             # Dark theme (default)
  snatch tui --theme light            # Light theme
  snatch tui --ascii                  # ASCII-only (no Unicode)

EXPORT DIALOG
-------------
  When pressing 'e' to export:
    h/l             Change format
    t               Toggle thinking
    o               Toggle tools
    Enter           Confirm export
    Esc             Cancel"#
        );
    }
}

fn show_workflows(verbose: bool) {
    println!(
        r#"
COMMON WORKFLOWS
================

EXTRACT ALL PROMPTS
-------------------
  snatch prompts --all > all-prompts.txt
  snatch prompts -p /my/project --timestamps

DAILY STANDUP REPORT
--------------------
  snatch standup                      # Last 24 hours activity
  snatch standup --period 1week -a    # Weekly with all details
  snatch standup -f markdown --clipboard  # Copy to clipboard

ARCHIVE SESSIONS
----------------
  snatch export --all -f sqlite -O archive.db    # SQLite archive
  snatch export --all -f json -O archive.json    # JSON archive
  snatch export --all --since 1month -p /proj    # Recent from project

SESSION MANAGEMENT
------------------
  snatch tag add <id> "important"     # Add tag
  snatch tag name <id> "Auth Refactor"  # Name a session
  snatch tag bookmark <id>            # Bookmark for quick access
  snatch tag outcome <id> success     # Mark outcome

FUZZY SESSION PICKER
--------------------
  snatch pick                         # Interactive fuzzy picker
  snatch pick -a info                 # Pick then show info
  snatch pick -a stats                # Pick then show stats"#
    );

    if verbose {
        println!(
            r#"

SHELL INTEGRATION
-----------------
  # Add to .bashrc or .zshrc:
  alias sl='snatch list'
  alias se='snatch export'
  alias ss='snatch search'
  alias st='snatch tui'

  # Quick export function:
  snexport() {{ snatch export "$1" -O "${{2:-conversation.md}}"; }}

CLEANUP
-------
  snatch cleanup --empty --preview    # Preview empty sessions
  snatch cleanup --older-than 3months # Old sessions
  snatch cache stats                  # Check cache usage
  snatch cache invalidate             # Clear stale cache

VALIDATION
----------
  snatch validate --all               # Check all sessions
  snatch validate <id> --schema       # Schema compatibility"#
        );
    }
}
