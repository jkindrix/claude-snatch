//! Export command implementation.
//!
//! Exports conversations to various formats (Markdown, JSON, etc.).

use std::io::{self, Write};
use std::path::PathBuf;
use std::time::SystemTime;

use chrono::{Duration, NaiveDate, Utc};
use indicatif::{ProgressBar, ProgressStyle};

use crate::cli::{Cli, ExportArgs, ExportFormatArg};
use crate::discovery::{Session, SessionFilter};
use crate::error::{Result, SnatchError};
use crate::export::{
    conversation_to_jsonl, CsvExporter, ExportOptions, Exporter, HtmlExporter, JsonExporter,
    MarkdownExporter, SqliteExporter, TextExporter, XmlExporter,
};
use crate::reconstruction::Conversation;

use super::get_claude_dir;

/// Run the export command.
pub fn run(cli: &Cli, args: &ExportArgs) -> Result<()> {
    // Validate arguments
    if args.all {
        // Export all sessions with optional filtering
        export_all_sessions(cli, args)
    } else if let Some(ref session_id) = args.session {
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
    let exported = export_session(cli, args, &session, args.output.as_ref())?;

    // Print success message to stderr if writing to file
    if exported && args.output.is_some() && !cli.quiet {
        eprintln!(
            "Exported session {} to {}",
            session_id,
            args.output.as_ref().unwrap().display()
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

    // Build export options
    let options = if args.lossless {
        ExportOptions::full()
    } else {
        ExportOptions {
            include_thinking: args.thinking,
            include_tool_use: args.tool_use,
            include_tool_results: args.tool_results,
            include_system: args.system,
            include_timestamps: args.timestamps,
            include_usage: args.usage,
            include_metadata: args.metadata,
            include_images: true,
            max_depth: None,
            truncate_at: None,
            include_branches: !args.main_thread,
            main_thread_only: args.main_thread,
        }
    };

    // Get output writer
    let mut output: Box<dyn Write> = if let Some(output_path) = &args.output {
        Box::new(std::fs::File::create(output_path)?)
    } else {
        Box::new(io::stdout())
    };

    // Export based on format
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
            if let Some(output_path) = &args.output {
                let exporter = SqliteExporter::new();
                exporter.export_to_file(&conversation, output_path, &options)?;
            } else {
                return Err(SnatchError::ConfigError {
                    message: "SQLite export requires an output file path".to_string(),
                });
            }
        }
    }

    // Print success message
    if args.output.is_some() && !cli.quiet {
        let total_sessions = node.total_sessions();
        eprintln!(
            "Exported combined session {} ({} sessions) to {}",
            session.session_id(),
            total_sessions,
            args.output.as_ref().unwrap().display()
        );
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
    let output_dir = if let Some(ref output) = args.output {
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

    // Build export options - lossless mode overrides individual settings
    let options = if args.lossless {
        ExportOptions::full()
    } else {
        ExportOptions {
            include_thinking: args.thinking,
            include_tool_use: args.tool_use,
            include_tool_results: args.tool_results,
            include_system: args.system,
            include_timestamps: args.timestamps,
            include_usage: args.usage,
            include_metadata: args.metadata,
            include_images: true,
            max_depth: None,
            truncate_at: None,
            include_branches: !args.main_thread,
            main_thread_only: args.main_thread,
        }
    };

    // Get output writer
    let mut writer: Box<dyn Write> = match output_path {
        Some(path) => {
            let file = std::fs::File::create(path).map_err(|e| {
                SnatchError::io(format!("Failed to create output file: {}", path.display()), e)
            })?;
            Box::new(std::io::BufWriter::new(file))
        }
        None => Box::new(io::stdout().lock()),
    };

    // Export based on format
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
            // Export in original JSONL format
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
            // SQLite requires a file path
            if let Some(path) = output_path {
                drop(writer); // Close the file we opened
                std::fs::remove_file(path).ok(); // Remove empty file
                let exporter = SqliteExporter::new();
                exporter.export_to_file(&conversation, path, &options)?;

                // Print success message for SQLite
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
    }

    writer.flush()?;

    // Print success message to stderr if writing to file (single session mode only)
    if output_path.is_some() && args.session.is_some() && !cli.quiet {
        eprintln!(
            "Exported {} entries to {}",
            conversation.len(),
            output_path.unwrap().display()
        );
    }

    Ok(true)
}

/// Parse a date filter string.
///
/// Supports:
/// - ISO date: `2024-12-24`
/// - Relative: `1day`, `2weeks`, `3months`
fn parse_date_filter(s: &str) -> Result<SystemTime> {
    // Try parsing as ISO date first
    if let Ok(date) = NaiveDate::parse_from_str(s, "%Y-%m-%d") {
        let datetime = date.and_hms_opt(0, 0, 0).unwrap();
        let utc = chrono::TimeZone::from_utc_datetime(&Utc, &datetime);
        return Ok(SystemTime::from(utc));
    }

    // Try parsing as relative duration
    let s_lower = s.to_lowercase();

    // Extract numeric part and unit
    let numeric_end = s_lower
        .char_indices()
        .find(|(_, c)| !c.is_ascii_digit())
        .map(|(i, _)| i)
        .unwrap_or(s_lower.len());

    if numeric_end == 0 || numeric_end == s_lower.len() {
        return Err(SnatchError::ConfigError {
            message: format!(
                "Invalid date filter '{}'. Use YYYY-MM-DD or relative like '1week', '2days'",
                s
            ),
        });
    }

    let amount: i64 = s_lower[..numeric_end].parse().map_err(|_| {
        SnatchError::ConfigError {
            message: format!("Invalid number in date filter: {}", &s_lower[..numeric_end]),
        }
    })?;

    let unit = &s_lower[numeric_end..];
    let duration = match unit {
        "d" | "day" | "days" => Duration::days(amount),
        "w" | "week" | "weeks" => Duration::weeks(amount),
        "m" | "month" | "months" => Duration::days(amount * 30), // Approximate
        "y" | "year" | "years" => Duration::days(amount * 365),  // Approximate
        "h" | "hour" | "hours" => Duration::hours(amount),
        _ => {
            return Err(SnatchError::ConfigError {
                message: format!(
                    "Unknown time unit '{}'. Use days, weeks, months, or years",
                    unit
                ),
            })
        }
    };

    let target = Utc::now() - duration;
    Ok(SystemTime::from(target))
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
