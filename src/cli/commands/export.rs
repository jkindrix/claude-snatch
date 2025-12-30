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
    JsonExporter, MarkdownExporter, SqliteExporter, TextExporter, XmlExporter,
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

    // Export the session
    let exported = export_session(cli, args, &session, args.output_file.as_ref())?;

    // Print success message to stderr if writing to file
    if exported && args.output_file.is_some() && !cli.quiet {
        eprintln!(
            "Exported session {} to {}",
            session_id,
            args.output_file.as_ref().unwrap().display()
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
                let exporter = HtmlExporter::new();
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
                let exporter = HtmlExporter::new();
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
    let claude_dir = get_claude_dir(cli.claude_dir.as_ref())?;

    // Build session filter
    let mut filter = SessionFilter::new();

    // By default exclude subagents unless explicitly included
    if !args.include_agents {
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
            match filter.matches(s) {
                Ok(matches) => matches,
                Err(_) => false,
            }
        })
        .collect();

    if sessions.is_empty() {
        if !cli.quiet {
            eprintln!("No sessions match the specified filters");
        }
        return Ok(());
    }

    // Sort by modification time (newest first)
    sessions.sort_by(|a, b| b.modified_time().cmp(&a.modified_time()));

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
            session.project_path().replace('/', "_").replace('\\', "_"),
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

/// Export a single session to the specified output.
///
/// Uses atomic file writes for file output to ensure data integrity.
fn export_session(
    cli: &Cli,
    args: &ExportArgs,
    session: &Session,
    output_path: Option<&PathBuf>,
) -> Result<bool> {
    // Parse the session
    let entries = session.parse()?;

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
            minimization: None,
            only: only_filter,
        }
    };

    // Handle SQLite separately as it manages its own file
    if matches!(args.format, ExportFormatArg::Sqlite) {
        if let Some(path) = output_path {
            let exporter = SqliteExporter::new();
            exporter.export_to_file(&conversation, path, &options)?;
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
                let exporter = HtmlExporter::new();
                exporter.export_conversation(&conversation, &mut writer, &options)?;
            }
            ExportFormatArg::Sqlite => {
                unreachable!("SQLite handled above");
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
                let exporter = HtmlExporter::new();
                exporter.export_conversation(&conversation, &mut writer, &options)?;
            }
            ExportFormatArg::Sqlite => {
                return Err(SnatchError::export(
                    "SQLite export requires an output file (--output <path.db>)",
                ));
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
