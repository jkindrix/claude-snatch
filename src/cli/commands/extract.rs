//! Extract command implementation.
//!
//! Extracts Beyond-JSONL data from Claude Code directories,
//! including settings, CLAUDE.md, MCP configs, custom commands,
//! rules, output styles, and file history.

use std::path::PathBuf;

use crate::cli::{Cli, ExtractArgs, OutputFormat};
use crate::error::Result;
use crate::extraction::BeyondJsonlData;

use super::get_claude_dir;

/// Run the extract command.
pub fn run(cli: &Cli, args: &ExtractArgs) -> Result<()> {
    let claude_dir = get_claude_dir(cli.claude_dir.as_ref())?;

    // Extract data based on scope
    let data = if let Some(ref project_path) = args.project {
        let path = PathBuf::from(project_path);
        BeyondJsonlData::extract_for_project(&claude_dir, &path)?
    } else {
        BeyondJsonlData::extract_global(&claude_dir)?
    };

    // Check if any data was found
    if !data.has_data() && !cli.quiet {
        eprintln!("No Beyond-JSONL data found.");
        return Ok(());
    }

    // Output based on format
    match cli.effective_output() {
        OutputFormat::Json => print_json_output(&data, args.pretty)?,
        _ => print_text_output(&data, args)?,
    }

    Ok(())
}

/// Print output in JSON format.
fn print_json_output(data: &BeyondJsonlData, pretty: bool) -> Result<()> {
    let json = if pretty {
        serde_json::to_string_pretty(data)?
    } else {
        serde_json::to_string(data)?
    };
    println!("{json}");
    Ok(())
}

/// Print output in text format.
fn print_text_output(data: &BeyondJsonlData, args: &ExtractArgs) -> Result<()> {
    println!("Beyond-JSONL Data Extraction");
    println!("=============================");
    println!();
    println!("Data sources found: {}", data.data_source_count());
    println!();

    // Global settings (BJ-002)
    if let Some(ref settings) = data.global_settings {
        if args.all || args.settings {
            println!("[Global Settings]");
            if let Some(ref model) = &settings.model {
                if let Some(ref default) = model.default {
                    println!("  Default Model: {default}");
                }
                if let Some(temp) = model.temperature {
                    println!("  Temperature: {temp}");
                }
            }
            if let Some(ref api) = &settings.api {
                if let Some(ref endpoint) = api.endpoint {
                    println!("  API Endpoint: {endpoint}");
                }
            }
            if settings.has_permissions() {
                println!("  Has permissions: {} rules", settings.permissions.len());
            }
            println!();
        }
    }

    // Project settings (BJ-003)
    if let Some(ref settings) = data.project_settings {
        if args.all || args.settings {
            println!("[Project Settings]");
            if let Some(ref model) = &settings.model {
                if let Some(ref default) = model.default {
                    println!("  Default Model: {default}");
                }
            }
            println!();
        }
    }

    // Global CLAUDE.md (BJ-004)
    if let Some(ref claude_md) = data.global_claude_md {
        if args.all || args.claude_md {
            println!("[Global CLAUDE.md]");
            let preview = if claude_md.len() > 200 {
                format!("{}...", &claude_md[..200])
            } else {
                claude_md.clone()
            };
            for line in preview.lines().take(5) {
                println!("  {line}");
            }
            if claude_md.lines().count() > 5 {
                println!("  ... ({} more lines)", claude_md.lines().count() - 5);
            }
            println!();
        }
    }

    // Project CLAUDE.md (BJ-005)
    if let Some(ref claude_md) = data.project_claude_md {
        if args.all || args.claude_md {
            println!("[Project CLAUDE.md]");
            let preview = if claude_md.len() > 200 {
                format!("{}...", &claude_md[..200])
            } else {
                claude_md.clone()
            };
            for line in preview.lines().take(5) {
                println!("  {line}");
            }
            if claude_md.lines().count() > 5 {
                println!("  ... ({} more lines)", claude_md.lines().count() - 5);
            }
            println!();
        }
    }

    // MCP config (BJ-006)
    if let Some(ref mcp) = data.mcp_config {
        if args.all || args.mcp {
            println!("[MCP Configuration]");
            println!("  Servers: {}", mcp.mcp_servers.len());
            for (name, server) in &mcp.mcp_servers {
                let cmd = server.command.as_deref().unwrap_or("(no command)");
                println!("    - {name}: {cmd}");
            }
            println!();
        }
    }

    // Global commands (BJ-007)
    if !data.global_commands.is_empty() && (args.all || args.commands) {
        println!("[Global Commands]");
        for cmd in &data.global_commands {
            println!("  - /{}: {}", cmd.name, cmd.description.as_deref().unwrap_or(""));
        }
        println!();
    }

    // Project commands (BJ-008)
    if !data.project_commands.is_empty() && (args.all || args.commands) {
        println!("[Project Commands]");
        for cmd in &data.project_commands {
            println!("  - /{}: {}", cmd.name, cmd.description.as_deref().unwrap_or(""));
        }
        println!();
    }

    // Credentials (BJ-009)
    if let Some(ref creds) = data.credentials_present {
        if args.all {
            println!("[Credentials]");
            println!("  File exists: {}", creds.file_exists);
            println!("  Anthropic key present: {}", creds.anthropic_key_present);
            println!();
        }
    }

    // Hooks (BJ-011)
    if !data.hooks.is_empty() && (args.all || args.hooks) {
        println!("[Hooks]");
        for hook in &data.hooks {
            let event_name = hook.event.as_deref()
                .or(hook.matcher.as_deref())
                .unwrap_or("unknown");
            let cmd = hook.command.as_deref().unwrap_or("(no command)");
            println!("  - {event_name}: {cmd}");
        }
        println!();
    }

    // Session retention (BJ-014)
    if let Some(ref retention) = data.session_retention {
        if args.all {
            println!("[Session Retention]");
            if let Some(max) = retention.max_sessions {
                println!("  Max sessions: {max}");
            }
            if let Some(days) = retention.max_age_days {
                println!("  Max age: {days} days");
            }
            println!("  Auto cleanup: {}", retention.auto_cleanup);
            println!();
        }
    }

    // Sandbox (BJ-015)
    if let Some(ref sandbox) = data.sandbox_config {
        if args.all {
            println!("[Sandbox Configuration]");
            println!("  Enabled: {}", sandbox.enabled);
            if !sandbox.allowed_directories.is_empty() {
                println!("  Allowed directories: {}", sandbox.allowed_directories.len());
            }
            if !sandbox.blocked_commands.is_empty() {
                println!("  Blocked commands: {}", sandbox.blocked_commands.len());
            }
            println!();
        }
    }

    // Global rules (BJ-017)
    if !data.global_rules.is_empty() && (args.all || args.rules) {
        println!("[Global Rules]");
        for rule in &data.global_rules {
            println!("  - {}: {}", rule.name, rule.description.as_deref().unwrap_or(""));
        }
        println!();
    }

    // Project rules (BJ-018)
    if !data.project_rules.is_empty() && (args.all || args.rules) {
        println!("[Project Rules]");
        for rule in &data.project_rules {
            println!("  - {}: {}", rule.name, rule.description.as_deref().unwrap_or(""));
        }
        println!();
    }

    // Output styles (BJ-021)
    if !data.output_styles.is_empty() && args.all {
        println!("[Output Styles]");
        for style in &data.output_styles {
            println!("  - {}", style.name);
        }
        println!();
    }

    // File history (BJ-001)
    if let Some(ref history) = data.file_history_summary {
        if args.all || args.file_history {
            println!("[File History]");
            println!("  Total backups: {}", history.backup_count);
            println!("  Total size: {} bytes", history.total_size_bytes);
            println!("  Unique files: {}", history.unique_files);
            if let Some(ref oldest) = history.oldest_backup {
                println!("  Oldest backup: {oldest}");
            }
            if let Some(ref newest) = history.newest_backup {
                println!("  Newest backup: {newest}");
            }
            println!();
        }
    }

    Ok(())
}
