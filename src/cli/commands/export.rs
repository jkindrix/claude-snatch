//! Export command implementation.
//!
//! Exports conversations to various formats (Markdown, JSON, etc.).

use std::collections::HashSet;
use std::io::{self, Write};
use std::path::PathBuf;

use indicatif::{ProgressBar, ProgressStyle};

use crate::cli::{Cli, ContentFilter, ExportArgs, ExportFormatArg};
use crate::discovery::{Session, SessionFilter};
use crate::error::{Result, SnatchError};
use crate::export::{
    conversation_to_jsonl, ContentType, CsvExporter, ExportOptions, Exporter, HtmlExporter,
    JsonExporter, MarkdownExporter, OtelExporter, SessionMeta, SqliteExporter, TextExporter,
    XmlExporter,
};
use crate::model::{ContentBlock, LogEntry};
use crate::reconstruction::Conversation;
use crate::util::{detect_sensitive, AtomicFile, RedactionConfig, SensitiveDataType};

use super::{get_claude_dir, parse_date_filter};

/// Convert CLI ContentFilter to export ContentType.
fn content_filter_to_type(filter: ContentFilter) -> ContentType {
    match filter {
        ContentFilter::User => ContentType::User,
        ContentFilter::Prompts => ContentType::Prompts,
        ContentFilter::Assistant => ContentType::Assistant,
        ContentFilter::Thinking => ContentType::Thinking,
        ContentFilter::ToolUse => ContentType::ToolUse,
        ContentFilter::ToolResults => ContentType::ToolResults,
        ContentFilter::System => ContentType::System,
        ContentFilter::Summary => ContentType::Summary,
    }
}

/// Convert CLI --only filters to a HashSet of ContentTypes.
fn build_only_filter(filters: &[ContentFilter]) -> HashSet<ContentType> {
    filters.iter().map(|f| content_filter_to_type(*f)).collect()
}

/// Run the export command.
pub fn run(cli: &Cli, args: &ExportArgs) -> Result<()> {
    // Always validate date filters early to catch typos/errors immediately
    if let Some(ref since) = args.since {
        parse_date_filter(since)?; // Validate, but result is used later if needed
    }
    if let Some(ref until) = args.until {
        parse_date_filter(until)?; // Validate, but result is used later if needed
    }

    // Validate gist-specific arguments
    if args.gist {
        // Check that gh CLI is available
        if !is_gh_cli_available() {
            return Err(SnatchError::ConfigError {
                message: "GitHub CLI (gh) is not installed or not in PATH. \
                    Install from https://cli.github.com/ and run 'gh auth login'".to_string(),
            });
        }

        // Gist doesn't support SQLite format
        if matches!(args.format, ExportFormatArg::Sqlite) {
            return Err(SnatchError::ConfigError {
                message: "--gist is not compatible with SQLite format".to_string(),
            });
        }

        // Gist doesn't support --all (too many gists would be created)
        if args.all {
            return Err(SnatchError::ConfigError {
                message: "--gist is not compatible with --all. Export a single session.".to_string(),
            });
        }

        // Gist and clipboard are mutually exclusive
        if args.clipboard {
            return Err(SnatchError::ConfigError {
                message: "--gist and --clipboard are mutually exclusive".to_string(),
            });
        }
    }

    // Validate clipboard-specific arguments
    if args.clipboard {
        // Clipboard doesn't support --all
        if args.all {
            return Err(SnatchError::ConfigError {
                message: "--clipboard is not compatible with --all. Export a single session.".to_string(),
            });
        }

        // Clipboard doesn't support SQLite format
        if matches!(args.format, ExportFormatArg::Sqlite) {
            return Err(SnatchError::ConfigError {
                message: "--clipboard is not compatible with SQLite format".to_string(),
            });
        }
    }

    // Validate arguments
    if args.all {
        // Export all sessions with optional filtering
        export_all_sessions(cli, args)
    } else if let Some(ref session_id) = args.session {
        // Warn if date filters are used with single-session export (they don't apply)
        if (args.since.is_some() || args.until.is_some()) && !cli.quiet {
            eprintln!(
                "Note: --since/--until filters are only applied with --all; single session export ignores them"
            );
        }
        // Export a single session
        export_single_session(cli, args, session_id)
    } else {
        Err(SnatchError::ConfigError {
            message: "Either specify a session ID or use --all to export all sessions".to_string(),
        })
    }
}

/// Export a single session.
fn export_single_session(cli: &Cli, args: &ExportArgs, session_id: &str) -> Result<()> {
    let claude_dir = get_claude_dir(cli.claude_dir.as_ref())?;

    // Find the session
    let session = claude_dir
        .find_session(session_id)?
        .ok_or_else(|| SnatchError::SessionNotFound {
            session_id: session_id.to_string(),
        })?;

    // Handle combined agents export
    if args.combine_agents {
        return export_combined_agents(cli, args, &session);
    }

    // Handle gist upload
    if args.gist {
        return export_session_to_gist(cli, args, &session);
    }

    // Export the session
    let exported = export_session(cli, args, &session, args.output_file.as_ref())?;

    // Print success message to stderr if writing to file
    if let (true, Some(output_file)) = (exported && !cli.quiet, args.output_file.as_ref()) {
        eprintln!(
            "Exported session {} to {}",
            session_id,
            output_file.display()
        );
    }

    Ok(())
}

/// Export a session to a GitHub Gist.
fn export_session_to_gist(cli: &Cli, args: &ExportArgs, session: &Session) -> Result<()> {
    // Parse the session
    let entries = session.parse_with_options(cli.max_file_size)?;

    if entries.is_empty() {
        return Err(SnatchError::ConfigError {
            message: "Session has no entries to export".to_string(),
        });
    }

    // Build conversation tree
    let conversation = Conversation::from_entries(entries)?;

    // Check for PII if requested
    if args.warn_pii {
        check_for_pii(&conversation, cli.quiet);
    }

    // Build export options
    let redaction = args.redact.map(|level| level.into());
    let only_filter = build_only_filter(&args.only);
    let options = if args.lossless {
        let mut opts = ExportOptions::full();
        opts.redaction = redaction;
        opts.redaction_preview = args.redact_preview;
        opts.only = only_filter;
        opts
    } else {
        ExportOptions {
            include_thinking: args.thinking,
            include_tool_use: args.tool_use,
            include_tool_results: args.tool_results,
            include_system: args.system,
            include_timestamps: args.timestamps,
            relative_timestamps: false,
            include_usage: args.usage,
            include_metadata: args.metadata,
            include_images: true,
            max_depth: None,
            truncate_at: None,
            include_branches: !args.main_thread,
            main_thread_only: args.main_thread,
            redaction,
            redaction_preview: args.redact_preview,
            minimization: None,
            only: only_filter,
        }
    };

    // Export to string
    let content = export_to_string(
        &conversation,
        args.format,
        &options,
        args.pretty,
        args.main_thread,
        args.toc,
        args.dark,
    )?;

    // Generate filename
    let ext = get_format_extension(args.format);
    let short_id = &session.session_id()[..8.min(session.session_id().len())];
    let filename = format!("session-{short_id}.{ext}");

    // Generate description
    let description = args.gist_description.as_deref().unwrap_or_else(|| {
        // Default description would be generated, but we can't return a reference
        // to a temporary, so we use a static default
        "Claude Code session export"
    });

    // Upload to gist
    if !cli.quiet {
        eprintln!("Uploading to GitHub Gist...");
    }

    let gist_url = upload_to_gist(&content, &filename, Some(description), args.gist_public)?;

    // Print the gist URL
    if cli.json {
        println!(r#"{{"url": "{}"}}"#, gist_url);
    } else {
        println!("{gist_url}");
    }

    if !cli.quiet {
        eprintln!(
            "✓ Created {} gist with {} entries",
            if args.gist_public { "public" } else { "secret" },
            conversation.len()
        );
    }

    Ok(())
}

/// Export a session combined with its subagent transcripts.
fn export_combined_agents(cli: &Cli, args: &ExportArgs, session: &Session) -> Result<()> {
    use crate::discovery::{HierarchyBuilder, collect_hierarchy_entries};

    // Get the project for this session
    let claude_dir = get_claude_dir(cli.claude_dir.as_ref())?;
    let projects = claude_dir.projects()?;

    // Find the project containing this session
    let project = projects.iter().find(|p| {
        p.decoded_path() == session.project_path()
    }).ok_or_else(|| SnatchError::ProjectNotFound {
        project_path: session.project_path().to_string(),
    })?;

    // Build hierarchy for the project
    let hierarchy = HierarchyBuilder::new().build_for_project(project)?;

    // Find the node for this session
    let node = find_node_by_session_id(&hierarchy, session.session_id())
        .ok_or_else(|| SnatchError::SessionNotFound {
            session_id: session.session_id().to_string(),
        })?;

    // Collect all entries from the hierarchy
    let labeled_entries = collect_hierarchy_entries(node)?;

    if labeled_entries.is_empty() {
        if !cli.quiet {
            eprintln!("No entries found in session hierarchy");
        }
        return Ok(());
    }

    // Extract just the entries
    let entries: Vec<_> = labeled_entries.into_iter().map(|(_, e)| e).collect();

    // Build conversation from combined entries
    let conversation = Conversation::from_entries(entries)?;

    // Check for PII if requested
    if args.warn_pii {
        check_for_pii(&conversation, cli.quiet);
    }

    // Build export options
    let redaction = args.redact.map(|level| level.into());
    let only_filter = build_only_filter(&args.only);
    let options = if args.lossless {
        let mut opts = ExportOptions::full();
        opts.redaction = redaction;
        opts.redaction_preview = args.redact_preview;
        opts.only = only_filter;
        opts
    } else {
        ExportOptions {
            include_thinking: args.thinking,
            include_tool_use: args.tool_use,
            include_tool_results: args.tool_results,
            include_system: args.system,
            include_timestamps: args.timestamps,
            relative_timestamps: false,
            include_usage: args.usage,
            include_metadata: args.metadata,
            include_images: true,
            max_depth: None,
            truncate_at: None,
            include_branches: !args.main_thread,
            main_thread_only: args.main_thread,
            redaction,
            redaction_preview: args.redact_preview,
            minimization: None,
            only: only_filter,
        }
    };

    // Handle SQLite separately as it manages its own file
    if matches!(args.format, ExportFormatArg::Sqlite) {
        if let Some(output_path) = &args.output_file {
            let exporter = SqliteExporter::new();
            exporter.export_to_file(&conversation, output_path, &options)?;
            if !cli.quiet {
                let total_sessions = node.total_sessions();
                eprintln!(
                    "Exported combined session {} ({} sessions) to {}",
                    session.session_id(),
                    total_sessions,
                    output_path.display()
                );
            }
            return Ok(());
        } else {
            return Err(SnatchError::ConfigError {
                message: "SQLite export requires an output file path".to_string(),
            });
        }
    }

    // For file output, use atomic writes
    if let Some(output_path) = &args.output_file {
        let mut atomic = AtomicFile::create(output_path)?;
        let mut output = std::io::BufWriter::new(atomic.writer());

        match args.format {
            ExportFormatArg::Markdown | ExportFormatArg::Md => {
                let exporter = MarkdownExporter::new();
                exporter.export_conversation(&conversation, &mut output, &options)?;
            }
            ExportFormatArg::Json => {
                let exporter = JsonExporter::new();
                exporter.export_conversation(&conversation, &mut output, &options)?;
            }
            ExportFormatArg::JsonPretty => {
                let exporter = JsonExporter::new().pretty(true);
                exporter.export_conversation(&conversation, &mut output, &options)?;
            }
            ExportFormatArg::Jsonl => {
                conversation_to_jsonl(&conversation, &mut output, args.main_thread)?;
            }
            ExportFormatArg::Html => {
                let exporter = HtmlExporter::new()
                    .with_toc(args.toc)
                    .dark_theme(args.dark);
                exporter.export_conversation(&conversation, &mut output, &options)?;
            }
            ExportFormatArg::Text => {
                let exporter = TextExporter::new();
                exporter.export_conversation(&conversation, &mut output, &options)?;
            }
            ExportFormatArg::Csv => {
                let exporter = CsvExporter::new();
                exporter.export_conversation(&conversation, &mut output, &options)?;
            }
            ExportFormatArg::Xml => {
                let exporter = XmlExporter::new();
                exporter.export_conversation(&conversation, &mut output, &options)?;
            }
            ExportFormatArg::Sqlite => {
                unreachable!("SQLite handled above");
            }
            ExportFormatArg::Otel => {
                let exporter = OtelExporter::new();
                exporter.export_conversation(&conversation, &mut output, &options)?;
            }
        }

        output.flush()?;
        drop(output);
        atomic.finish()?;

        if !cli.quiet {
            let total_sessions = node.total_sessions();
            eprintln!(
                "Exported combined session {} ({} sessions) to {}",
                session.session_id(),
                total_sessions,
                output_path.display()
            );
        }
    } else {
        // Write to stdout (no atomic write needed)
        let mut output: Box<dyn Write> = Box::new(io::stdout());

        match args.format {
            ExportFormatArg::Markdown | ExportFormatArg::Md => {
                let exporter = MarkdownExporter::new();
                exporter.export_conversation(&conversation, &mut output, &options)?;
            }
            ExportFormatArg::Json => {
                let exporter = JsonExporter::new();
                exporter.export_conversation(&conversation, &mut output, &options)?;
            }
            ExportFormatArg::JsonPretty => {
                let exporter = JsonExporter::new().pretty(true);
                exporter.export_conversation(&conversation, &mut output, &options)?;
            }
            ExportFormatArg::Jsonl => {
                conversation_to_jsonl(&conversation, &mut output, args.main_thread)?;
            }
            ExportFormatArg::Html => {
                let exporter = HtmlExporter::new()
                    .with_toc(args.toc)
                    .dark_theme(args.dark);
                exporter.export_conversation(&conversation, &mut output, &options)?;
            }
            ExportFormatArg::Text => {
                let exporter = TextExporter::new();
                exporter.export_conversation(&conversation, &mut output, &options)?;
            }
            ExportFormatArg::Csv => {
                let exporter = CsvExporter::new();
                exporter.export_conversation(&conversation, &mut output, &options)?;
            }
            ExportFormatArg::Xml => {
                let exporter = XmlExporter::new();
                exporter.export_conversation(&conversation, &mut output, &options)?;
            }
            ExportFormatArg::Sqlite => {
                return Err(SnatchError::ConfigError {
                    message: "SQLite export requires an output file path".to_string(),
                });
            }
            ExportFormatArg::Otel => {
                let exporter = OtelExporter::new();
                exporter.export_conversation(&conversation, &mut output, &options)?;
            }
        }
    }

    Ok(())
}

/// Find a node by session ID in the hierarchy.
fn find_node_by_session_id<'a>(
    hierarchy: &'a [crate::discovery::AgentNode],
    session_id: &str,
) -> Option<&'a crate::discovery::AgentNode> {
    for node in hierarchy {
        if node.session.session_id() == session_id {
            return Some(node);
        }
        // Search children
        if let Some(found) = find_node_in_children(node, session_id) {
            return Some(found);
        }
    }
    None
}

/// Find a node by session ID in children.
fn find_node_in_children<'a>(
    node: &'a crate::discovery::AgentNode,
    session_id: &str,
) -> Option<&'a crate::discovery::AgentNode> {
    for child in &node.children {
        if child.session.session_id() == session_id {
            return Some(child);
        }
        if let Some(found) = find_node_in_children(child, session_id) {
            return Some(found);
        }
    }
    None
}

/// Export all sessions matching filters.
fn export_all_sessions(cli: &Cli, args: &ExportArgs) -> Result<()> {
    // Special handling for SQLite: export all sessions to a single database
    if matches!(args.format, ExportFormatArg::Sqlite) {
        return export_all_sessions_sqlite(cli, args);
    }

    let claude_dir = get_claude_dir(cli.claude_dir.as_ref())?;

    // Build session filter
    let mut filter = SessionFilter::new();

    // By default exclude subagents unless explicitly included
    if !args.subagents {
        filter = filter.main_only();
    }

    // Parse date filters
    if let Some(ref since) = args.since {
        let since_time = parse_date_filter(since)?;
        filter.modified_after = Some(since_time);
    }

    if let Some(ref until) = args.until {
        let until_time = parse_date_filter(until)?;
        filter.modified_before = Some(until_time);
    }

    // Get all sessions
    let all_sessions = claude_dir.all_sessions()?;

    // Filter sessions
    let mut sessions: Vec<&Session> = all_sessions
        .iter()
        .filter(|s| {
            // Apply project filter
            if let Some(ref project) = args.project {
                if !s.project_path().contains(project) {
                    return false;
                }
            }

            // Apply session filter
            filter.matches(s).unwrap_or_default()
        })
        .collect();

    if sessions.is_empty() {
        if !cli.quiet {
            eprintln!("No sessions match the specified filters");
        }
        return Ok(());
    }

    // Sort by modification time (newest first)
    sessions.sort_by_key(|s| std::cmp::Reverse(s.modified_time()));

    // Determine output directory
    let output_dir = if let Some(ref output) = args.output_file {
        // If output is a directory, use it; otherwise use its parent
        if output.is_dir() {
            output.clone()
        } else {
            output.parent().map(PathBuf::from).unwrap_or_else(|| PathBuf::from("."))
        }
    } else {
        // Default to current directory for multi-session export
        PathBuf::from(".")
    };

    // Ensure output directory exists
    if !output_dir.exists() {
        std::fs::create_dir_all(&output_dir).map_err(|e| {
            SnatchError::io(format!("Failed to create output directory: {}", output_dir.display()), e)
        })?;
    }

    let extension = get_format_extension(args.format);
    let mut exported_count = 0;
    let mut error_count = 0;
    let mut skipped_count = 0;

    // Create progress bar if requested
    let progress = if args.progress && !cli.quiet {
        let pb = ProgressBar::new(sessions.len() as u64);
        pb.set_style(
            ProgressStyle::default_bar()
                .template("{spinner:.green} [{elapsed_precise}] [{bar:40.cyan/blue}] {pos}/{len} ({eta})")
                .expect("Invalid progress bar template")
                .progress_chars("#>-"),
        );
        Some(pb)
    } else {
        None
    };

    for session in &sessions {
        // Generate output filename
        let filename = format!(
            "{}_{}.{}",
            session.project_path().replace(['/', '\\'], "_"),
            session.session_id(),
            extension
        );
        let output_path = output_dir.join(&filename);

        // Check if file exists and handle overwrite
        if output_path.exists() && !args.overwrite {
            skipped_count += 1;
            if cli.verbose {
                eprintln!("Skipped (exists): {}", output_path.display());
            }
            if let Some(ref pb) = progress {
                pb.inc(1);
            }
            continue;
        }

        match export_session(cli, args, session, Some(&output_path)) {
            Ok(true) => {
                exported_count += 1;
                if cli.verbose {
                    eprintln!("Exported: {}", output_path.display());
                }
            }
            Ok(false) => {}
            Err(e) => {
                error_count += 1;
                if !cli.quiet {
                    eprintln!("Failed to export {}: {}", session.session_id(), e);
                }
            }
        }

        if let Some(ref pb) = progress {
            pb.inc(1);
        }
    }

    if let Some(pb) = progress {
        pb.finish_with_message("Export complete");
    }

    // Print summary
    if !cli.quiet {
        let mut suffix = String::new();
        if skipped_count > 0 {
            suffix.push_str(&format!(" ({} skipped)", skipped_count));
        }
        if error_count > 0 {
            suffix.push_str(&format!(" ({} errors)", error_count));
        }
        eprintln!(
            "Exported {} of {} sessions to {}{}",
            exported_count,
            sessions.len(),
            output_dir.display(),
            suffix
        );
    }

    Ok(())
}

/// Export all sessions to a single SQLite database.
fn export_all_sessions_sqlite(cli: &Cli, args: &ExportArgs) -> Result<()> {
    use rusqlite::Connection;

    let claude_dir = get_claude_dir(cli.claude_dir.as_ref())?;

    // SQLite export requires an output file
    let output_path = args.output_file.as_ref().ok_or_else(|| {
        SnatchError::export("SQLite export requires an output file (--output <path.db>)".to_string())
    })?;

    // Build session filter
    let mut filter = SessionFilter::new();

    if !args.subagents {
        filter = filter.main_only();
    }

    if let Some(ref since) = args.since {
        let since_time = parse_date_filter(since)?;
        filter.modified_after = Some(since_time);
    }

    if let Some(ref until) = args.until {
        let until_time = parse_date_filter(until)?;
        filter.modified_before = Some(until_time);
    }

    // Get all sessions
    let all_sessions = claude_dir.all_sessions()?;

    // Filter sessions
    let mut sessions: Vec<&Session> = all_sessions
        .iter()
        .filter(|s| {
            if let Some(ref project) = args.project {
                if !s.project_path().contains(project) {
                    return false;
                }
            }
            filter.matches(s).unwrap_or_default()
        })
        .collect();

    if sessions.is_empty() {
        if !cli.quiet {
            eprintln!("No sessions match the specified filters");
        }
        return Ok(());
    }

    // Sort by modification time (oldest first for database insertion order)
    sessions.sort_by_key(|s| s.modified_time());

    // Remove existing file if present
    if output_path.exists() {
        std::fs::remove_file(output_path).map_err(|e| {
            SnatchError::io(
                format!("Failed to remove existing database: {}", output_path.display()),
                e,
            )
        })?;
    }

    // Create database connection
    let conn = Connection::open(output_path).map_err(|e| {
        SnatchError::export(format!("Failed to create SQLite database: {}", e))
    })?;

    // Create exporter and schema
    let exporter = SqliteExporter::new()
        .with_fts(true)
        .with_foreign_keys(true)
        .with_usage(true);

    // Build export options
    let options = ExportOptions {
        include_thinking: args.thinking,
        include_tool_use: true,
        include_tool_results: true,
        include_system: true,
        include_usage: true,
        include_timestamps: true,
        include_metadata: true,
        main_thread_only: false,
        ..Default::default()
    };

    // Create progress bar
    let progress = if args.progress && !cli.quiet {
        let pb = ProgressBar::new(sessions.len() as u64);
        pb.set_style(
            ProgressStyle::default_bar()
                .template("{spinner:.green} [{elapsed_precise}] [{bar:40.cyan/blue}] {pos}/{len} ({eta})")
                .expect("Invalid progress bar template")
                .progress_chars("#>-"),
        );
        Some(pb)
    } else {
        None
    };

    let mut exported_count = 0;
    let mut error_count = 0;

    for session in &sessions {
        // Parse session with custom max file size if specified
        match session.parse_with_options(cli.max_file_size) {
            Ok(entries) if !entries.is_empty() => {
                match Conversation::from_entries(entries) {
                    Ok(conversation) => {
                        // Build session metadata
                        let meta = SessionMeta {
                            project_path: Some(session.project_path().to_string()),
                            is_subagent: session.is_subagent(),
                            agent_hash: session.agent_hash().map(String::from),
                            file_size: Some(session.file_size()),
                            git_branch: None,
                            git_commit: None,
                        };
                        match exporter.export_to_connection_with_meta(&conversation, &conn, &options, Some(&meta)) {
                            Ok(()) => {
                                exported_count += 1;
                                if cli.verbose {
                                    eprintln!("Exported: {}", session.session_id());
                                }
                            }
                            Err(e) => {
                                error_count += 1;
                                if !cli.quiet {
                                    eprintln!("Failed to export {}: {}", session.session_id(), e);
                                }
                            }
                        }
                    }
                    Err(e) => {
                        error_count += 1;
                        if !cli.quiet {
                            eprintln!("Failed to build conversation for {}: {}", session.session_id(), e);
                        }
                    }
                }
            }
            Ok(_) => {
                // Empty session, skip
            }
            Err(e) => {
                error_count += 1;
                if !cli.quiet {
                    eprintln!("Failed to parse {}: {}", session.session_id(), e);
                }
            }
        }

        if let Some(ref pb) = progress {
            pb.inc(1);
        }
    }

    if let Some(pb) = progress {
        pb.finish_with_message("Export complete");
    }

    // Print summary
    if !cli.quiet {
        let mut suffix = String::new();
        if error_count > 0 {
            suffix.push_str(&format!(" ({} errors)", error_count));
        }
        eprintln!(
            "Exported {} sessions to {}{}",
            exported_count,
            output_path.display(),
            suffix
        );
    }

    Ok(())
}

/// Export a single session to the specified output.
///
/// Uses atomic file writes for file output to ensure data integrity.
fn export_session(
    cli: &Cli,
    args: &ExportArgs,
    session: &Session,
    output_path: Option<&PathBuf>,
) -> Result<bool> {
    // Parse the session with custom max file size if specified
    let entries = session.parse_with_options(cli.max_file_size)?;

    if entries.is_empty() {
        return Ok(false);
    }

    // Build conversation tree
    let conversation = Conversation::from_entries(entries)?;

    // Check for PII if requested
    if args.warn_pii {
        check_for_pii(&conversation, cli.quiet);
    }

    // Build export options - lossless mode overrides individual settings
    let redaction = args.redact.map(|level| level.into());
    let only_filter = build_only_filter(&args.only);
    let options = if args.lossless {
        let mut opts = ExportOptions::full();
        opts.redaction = redaction;
        opts.redaction_preview = args.redact_preview;
        opts.only = only_filter;
        opts
    } else {
        ExportOptions {
            include_thinking: args.thinking,
            include_tool_use: args.tool_use,
            include_tool_results: args.tool_results,
            include_system: args.system,
            include_timestamps: args.timestamps,
            relative_timestamps: false,
            include_usage: args.usage,
            include_metadata: args.metadata,
            include_images: true,
            max_depth: None,
            truncate_at: None,
            include_branches: !args.main_thread,
            main_thread_only: args.main_thread,
            redaction,
            redaction_preview: args.redact_preview,
            minimization: None,
            only: only_filter,
        }
    };

    // Handle SQLite separately as it manages its own file
    if matches!(args.format, ExportFormatArg::Sqlite) {
        if args.clipboard {
            return Err(SnatchError::ConfigError {
                message: "--clipboard is not compatible with SQLite format".to_string(),
            });
        }
        if let Some(path) = output_path {
            let exporter = SqliteExporter::new();
            // Build session metadata
            let meta = SessionMeta {
                project_path: Some(session.project_path().to_string()),
                is_subagent: session.is_subagent(),
                agent_hash: session.agent_hash().map(String::from),
                file_size: Some(session.file_size()),
                git_branch: None,
                git_commit: None,
            };
            exporter.export_to_file_with_meta(&conversation, path, &options, Some(&meta))?;
            if args.session.is_some() && !cli.quiet {
                eprintln!("Exported {} entries to {}", conversation.len(), path.display());
            }
            return Ok(true);
        } else {
            return Err(SnatchError::export(
                "SQLite export requires an output file (--output <path.db>)",
            ));
        }
    }

    // Handle clipboard export
    if args.clipboard {
        let content = export_to_string(
            &conversation,
            args.format,
            &options,
            args.pretty,
            args.main_thread,
            args.toc,
            args.dark,
        )?;

        match arboard::Clipboard::new() {
            Ok(mut clipboard) => {
                if let Err(e) = clipboard.set_text(&content) {
                    return Err(SnatchError::ExportError {
                        message: format!("Failed to copy to clipboard: {e}"),
                        source: None,
                    });
                }
                if !cli.quiet {
                    eprintln!("Copied {} entries to clipboard.", conversation.len());
                }
            }
            Err(e) => {
                return Err(SnatchError::ExportError {
                    message: format!("Failed to access clipboard: {e}"),
                    source: None,
                });
            }
        }
        return Ok(true);
    }

    // For file output, use atomic writes
    if let Some(path) = output_path {
        let mut atomic = AtomicFile::create(path)?;
        let mut writer = std::io::BufWriter::new(atomic.writer());

        match args.format {
            ExportFormatArg::Markdown | ExportFormatArg::Md => {
                let exporter = MarkdownExporter::new();
                exporter.export_conversation(&conversation, &mut writer, &options)?;
            }
            ExportFormatArg::Json => {
                let exporter = JsonExporter::new().pretty(args.pretty);
                exporter.export_conversation(&conversation, &mut writer, &options)?;
            }
            ExportFormatArg::JsonPretty => {
                let exporter = JsonExporter::new().pretty(true);
                exporter.export_conversation(&conversation, &mut writer, &options)?;
            }
            ExportFormatArg::Text => {
                let exporter = TextExporter::new();
                exporter.export_conversation(&conversation, &mut writer, &options)?;
            }
            ExportFormatArg::Jsonl => {
                conversation_to_jsonl(&conversation, &mut writer, options.main_thread_only)?;
            }
            ExportFormatArg::Csv => {
                let exporter = CsvExporter::new();
                exporter.export_conversation(&conversation, &mut writer, &options)?;
            }
            ExportFormatArg::Xml => {
                let exporter = XmlExporter::new();
                exporter.export_conversation(&conversation, &mut writer, &options)?;
            }
            ExportFormatArg::Html => {
                let exporter = HtmlExporter::new()
                    .with_toc(args.toc)
                    .dark_theme(args.dark);
                exporter.export_conversation(&conversation, &mut writer, &options)?;
            }
            ExportFormatArg::Sqlite => {
                unreachable!("SQLite handled above");
            }
            ExportFormatArg::Otel => {
                let exporter = OtelExporter::new();
                exporter.export_conversation(&conversation, &mut writer, &options)?;
            }
        }

        writer.flush()?;
        drop(writer);
        atomic.finish()?;

        if args.session.is_some() && !cli.quiet {
            eprintln!("Exported {} entries to {}", conversation.len(), path.display());
        }
    } else {
        // Write to stdout (no atomic write needed)
        let mut writer: Box<dyn Write> = Box::new(io::stdout().lock());

        match args.format {
            ExportFormatArg::Markdown | ExportFormatArg::Md => {
                let exporter = MarkdownExporter::new();
                exporter.export_conversation(&conversation, &mut writer, &options)?;
            }
            ExportFormatArg::Json => {
                let exporter = JsonExporter::new().pretty(args.pretty);
                exporter.export_conversation(&conversation, &mut writer, &options)?;
            }
            ExportFormatArg::JsonPretty => {
                let exporter = JsonExporter::new().pretty(true);
                exporter.export_conversation(&conversation, &mut writer, &options)?;
            }
            ExportFormatArg::Text => {
                let exporter = TextExporter::new();
                exporter.export_conversation(&conversation, &mut writer, &options)?;
            }
            ExportFormatArg::Jsonl => {
                conversation_to_jsonl(&conversation, &mut writer, options.main_thread_only)?;
            }
            ExportFormatArg::Csv => {
                let exporter = CsvExporter::new();
                exporter.export_conversation(&conversation, &mut writer, &options)?;
            }
            ExportFormatArg::Xml => {
                let exporter = XmlExporter::new();
                exporter.export_conversation(&conversation, &mut writer, &options)?;
            }
            ExportFormatArg::Html => {
                let exporter = HtmlExporter::new()
                    .with_toc(args.toc)
                    .dark_theme(args.dark);
                exporter.export_conversation(&conversation, &mut writer, &options)?;
            }
            ExportFormatArg::Sqlite => {
                return Err(SnatchError::export(
                    "SQLite export requires an output file (--output <path.db>)",
                ));
            }
            ExportFormatArg::Otel => {
                let exporter = OtelExporter::new();
                exporter.export_conversation(&conversation, &mut writer, &options)?;
            }
        }

        writer.flush()?;
    }

    Ok(true)
}

/// Get the file extension for a format.
fn get_format_extension(format: ExportFormatArg) -> &'static str {
    match format {
        ExportFormatArg::Markdown | ExportFormatArg::Md => "md",
        ExportFormatArg::Json | ExportFormatArg::JsonPretty => "json",
        ExportFormatArg::Text => "txt",
        ExportFormatArg::Jsonl => "jsonl",
        ExportFormatArg::Csv => "csv",
        ExportFormatArg::Xml => "xml",
        ExportFormatArg::Html => "html",
        ExportFormatArg::Sqlite => "db",
        ExportFormatArg::Otel => "otlp.json",
    }
}

/// Check for PII in a conversation and print warnings.
///
/// Returns the set of detected PII types.
fn check_for_pii(conversation: &Conversation, quiet: bool) -> HashSet<SensitiveDataType> {
    let config = RedactionConfig::all();
    let mut detected_types: HashSet<SensitiveDataType> = HashSet::new();
    let mut sample_count = 0;
    const MAX_SAMPLES: usize = 5;

    for entry in conversation.chronological_entries() {
        let texts = extract_entry_texts(entry);

        for text in texts {
            let types = detect_sensitive(text, &config);
            for data_type in types {
                if detected_types.insert(data_type) && !quiet && sample_count < MAX_SAMPLES {
                    // Print the first warning for this type
                    eprintln!(
                        "⚠️  PII Warning: {} detected in exported content",
                        data_type.description()
                    );
                    sample_count += 1;
                }
            }
        }
    }

    if !detected_types.is_empty() && !quiet {
        eprintln!();
        eprintln!("📋 PII Detection Summary:");
        for data_type in &detected_types {
            eprintln!("   • {}", data_type.description());
        }
        eprintln!();
        eprintln!("💡 Tip: Use --redact to automatically redact sensitive data");
        eprintln!();
    }

    detected_types
}

/// Extract text content from a log entry.
fn extract_entry_texts(entry: &LogEntry) -> Vec<&str> {
    let mut texts = Vec::new();

    match entry {
        LogEntry::User(user) => {
            if let Some(text) = user.message.as_text() {
                texts.push(text);
            }
        }
        LogEntry::Assistant(assistant) => {
            for content in &assistant.message.content {
                match content {
                    ContentBlock::Text(t) => texts.push(&t.text),
                    ContentBlock::Thinking(th) => texts.push(&th.thinking),
                    ContentBlock::ToolUse(tool) => {
                        // Check tool input as JSON string
                        if let Some(input_str) = tool.input.as_str() {
                            texts.push(input_str);
                        }
                    }
                    ContentBlock::ToolResult(result) => {
                        if let Some(content_str) = result.content_as_string() {
                            // Can't push borrowed string from owned, so we skip this for now
                            // The text will be detected when it appears elsewhere
                            let _ = content_str;
                        }
                    }
                    _ => {}
                }
            }
        }
        LogEntry::System(system) => {
            if let Some(content) = &system.content {
                texts.push(content);
            }
        }
        _ => {}
    }

    texts
}

// ============================================================================
// GitHub Gist Upload Support
// ============================================================================

/// Check if the GitHub CLI (gh) is available.
fn is_gh_cli_available() -> bool {
    std::process::Command::new("gh")
        .arg("--version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

/// Upload content to a GitHub Gist using the gh CLI.
///
/// Returns the gist URL on success.
fn upload_to_gist(
    content: &str,
    filename: &str,
    description: Option<&str>,
    public: bool,
) -> Result<String> {
    use std::process::{Command, Stdio};

    // Build the gh gist create command
    let mut cmd = Command::new("gh");
    cmd.arg("gist")
        .arg("create")
        .arg("--filename")
        .arg(filename);

    if let Some(desc) = description {
        cmd.arg("--desc").arg(desc);
    }

    if public {
        cmd.arg("--public");
    }

    // Pass content via stdin
    cmd.stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    let mut child = cmd.spawn().map_err(|e| SnatchError::ExportError {
        message: format!("Failed to spawn gh CLI: {e}"),
        source: Some(Box::new(e)),
    })?;

    // Write content to stdin
    if let Some(stdin) = child.stdin.as_mut() {
        stdin.write_all(content.as_bytes()).map_err(|e| SnatchError::ExportError {
            message: format!("Failed to write to gh stdin: {e}"),
            source: Some(Box::new(e)),
        })?;
    }

    let output = child.wait_with_output().map_err(|e| SnatchError::ExportError {
        message: format!("Failed to wait for gh CLI: {e}"),
        source: Some(Box::new(e)),
    })?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(SnatchError::ExportError {
            message: format!("gh gist create failed: {stderr}"),
            source: None,
        });
    }

    // Parse the gist URL from stdout
    let url = String::from_utf8_lossy(&output.stdout).trim().to_string();

    if url.is_empty() {
        return Err(SnatchError::ExportError {
            message: "gh gist create returned empty output".to_string(),
            source: None,
        });
    }

    Ok(url)
}

/// Export a conversation to a string for gist upload.
fn export_to_string(
    conversation: &Conversation,
    format: ExportFormatArg,
    options: &ExportOptions,
    pretty: bool,
    main_thread_only: bool,
    toc: bool,
    dark: bool,
) -> Result<String> {
    let mut buffer = Vec::new();

    match format {
        ExportFormatArg::Markdown | ExportFormatArg::Md => {
            let exporter = MarkdownExporter::new();
            exporter.export_conversation(conversation, &mut buffer, options)?;
        }
        ExportFormatArg::Json => {
            let exporter = JsonExporter::new().pretty(pretty);
            exporter.export_conversation(conversation, &mut buffer, options)?;
        }
        ExportFormatArg::JsonPretty => {
            let exporter = JsonExporter::new().pretty(true);
            exporter.export_conversation(conversation, &mut buffer, options)?;
        }
        ExportFormatArg::Text => {
            let exporter = TextExporter::new();
            exporter.export_conversation(conversation, &mut buffer, options)?;
        }
        ExportFormatArg::Jsonl => {
            conversation_to_jsonl(conversation, &mut buffer, main_thread_only)?;
        }
        ExportFormatArg::Csv => {
            let exporter = CsvExporter::new();
            exporter.export_conversation(conversation, &mut buffer, options)?;
        }
        ExportFormatArg::Xml => {
            let exporter = XmlExporter::new();
            exporter.export_conversation(conversation, &mut buffer, options)?;
        }
        ExportFormatArg::Html => {
            let exporter = HtmlExporter::new()
                .with_toc(toc)
                .dark_theme(dark);
            exporter.export_conversation(conversation, &mut buffer, options)?;
        }
        ExportFormatArg::Sqlite => {
            unreachable!("SQLite cannot be exported to string");
        }
        ExportFormatArg::Otel => {
            let exporter = OtelExporter::new();
            exporter.export_conversation(conversation, &mut buffer, options)?;
        }
    }

    String::from_utf8(buffer).map_err(|e| SnatchError::ExportError {
        message: format!("Invalid UTF-8 in export output: {e}"),
        source: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_date_filter_iso() {
        let result = parse_date_filter("2024-12-24");
        assert!(result.is_ok());
    }

    #[test]
    fn test_parse_date_filter_relative() {
        assert!(parse_date_filter("1day").is_ok());
        assert!(parse_date_filter("2weeks").is_ok());
        assert!(parse_date_filter("3months").is_ok());
        assert!(parse_date_filter("1year").is_ok());
    }

    #[test]
    fn test_parse_date_filter_invalid() {
        assert!(parse_date_filter("invalid").is_err());
        assert!(parse_date_filter("abc123").is_err());
    }

    #[test]
    fn test_get_format_extension() {
        assert_eq!(get_format_extension(ExportFormatArg::Markdown), "md");
        assert_eq!(get_format_extension(ExportFormatArg::Json), "json");
        assert_eq!(get_format_extension(ExportFormatArg::Text), "txt");
        assert_eq!(get_format_extension(ExportFormatArg::Jsonl), "jsonl");
    }
}
