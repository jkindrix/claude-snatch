//! Info command implementation.
//!
//! Displays detailed information about sessions and projects.

use std::collections::{HashMap, HashSet};

use crate::cli::{Cli, InfoArgs, OutputFormat};
use crate::error::{Result, SnatchError};
use crate::model::{ContentBlock, LogEntry};
use crate::reconstruction::Conversation;
use crate::tags::TagStore;

use super::get_claude_dir;

/// Run the info command.
pub fn run(cli: &Cli, args: &InfoArgs) -> Result<()> {
    let claude_dir = get_claude_dir(cli.claude_dir.as_ref())?;

    if let Some(target) = &args.target {
        // Try to find as session first
        if let Some(session) = claude_dir.find_session(target)? {
            return show_session_info(cli, args, &session);
        }

        // Try to find as project
        if let Some(project) = claude_dir.find_project(target)? {
            return show_project_info(cli, args, &project);
        }

        // Search projects by substring
        let projects = claude_dir.projects()?;
        for project in projects {
            if project.decoded_path().contains(target) {
                return show_project_info(cli, args, &project);
            }
        }

        return Err(SnatchError::SessionNotFound {
            session_id: target.clone(),
        });
    }

    // No target - show directory info
    show_directory_info(cli, args, &claude_dir)
}

/// Show session information.
fn show_session_info(
    cli: &Cli,
    args: &InfoArgs,
    session: &crate::discovery::Session,
) -> Result<()> {
    let summary = session.summary()?;

    // Load tags/bookmarks for this session
    let tag_store = TagStore::load().unwrap_or_default();
    let session_meta = tag_store.get(&summary.session_id);

    match cli.effective_output() {
        OutputFormat::Json => {
            println!("{}", serde_json::to_string_pretty(&SessionInfoOutput {
                session_id: summary.session_id.clone(),
                project_path: summary.project_path.clone(),
                is_subagent: summary.is_subagent,
                file_size: summary.file_size,
                file_size_human: summary.file_size_human.clone(),
                entry_count: summary.entry_count,
                message_count: summary.message_count,
                start_time: summary.start_time.map(|t| t.to_rfc3339()),
                end_time: summary.end_time.map(|t| t.to_rfc3339()),
                duration_human: summary.duration_human(),
                state: format!("{:?}", summary.state),
                version: summary.version.clone(),
                path: session.path().to_string_lossy().to_string(),
                name: session_meta.and_then(|m| m.name.clone()),
                tags: session_meta.map(|m| m.tags.clone()).unwrap_or_default(),
                bookmarked: session_meta.is_some_and(|m| m.bookmarked),
                outcome: session_meta.and_then(|m| m.outcome.map(|o| o.to_string())),
            })?);
        }
        OutputFormat::Tsv => {
            println!("field\tvalue");
            println!("session_id\t{}", summary.session_id);
            println!("project\t{}", summary.project_path);
            println!("subagent\t{}", summary.is_subagent);
            println!("entries\t{}", summary.entry_count);
            println!("size\t{}", summary.file_size);
            if let Some(meta) = session_meta {
                if let Some(name) = &meta.name {
                    println!("name\t{}", name);
                }
                if !meta.tags.is_empty() {
                    println!("tags\t{}", meta.tags.join(","));
                }
                if meta.bookmarked {
                    println!("bookmarked\ttrue");
                }
                if let Some(outcome) = &meta.outcome {
                    println!("outcome\t{}", outcome);
                }
            }
        }
        OutputFormat::Compact => {
            println!("{}:{}:{}",
                summary.short_id(),
                summary.entry_count,
                summary.file_size_human
            );
        }
        OutputFormat::Text => {
            println!("Session Information");
            println!("===================");
            println!();
            println!("Session ID:   {}", summary.session_id);
            println!("Project:      {}", summary.project_path);
            println!("Type:         {}", if summary.is_subagent { "Subagent" } else { "Main" });
            println!("Status:       {:?}", summary.state);
            println!();

            if args.paths {
                println!("File Path:    {}", session.path().display());
                println!();
            }

            println!("File Size:    {}", summary.file_size_human);
            println!("Entries:      {}", summary.entry_count);
            println!("Messages:     {}", summary.message_count);
            println!();

            if let Some(start) = &summary.start_time {
                println!("Started:      {}", start.format("%Y-%m-%d %H:%M:%S UTC"));
            }
            if let Some(end) = &summary.end_time {
                // Use "Last Activity" for active sessions, "Ended" for inactive
                let label = if summary.state.is_active() {
                    "Last Activity"
                } else {
                    "Ended"
                };
                println!("{label}:{}{}",
                    if label.len() < 8 { "        " } else { "  " },
                    end.format("%Y-%m-%d %H:%M:%S UTC")
                );
            }
            if let Some(duration) = summary.duration_human() {
                // Only show duration for inactive sessions (completed)
                if !summary.state.is_active() {
                    println!("Duration:     {duration}");
                }
            }
            println!();

            if let Some(version) = &summary.version {
                println!("Version:      {version}");
            }

            // Show tags, bookmarks, name, and outcome if present
            if let Some(meta) = session_meta {
                let has_metadata = meta.name.is_some() || !meta.tags.is_empty() || meta.bookmarked || meta.outcome.is_some();
                if has_metadata {
                    println!();
                    if let Some(name) = &meta.name {
                        println!("Name:         {name}");
                    }
                    if meta.bookmarked {
                        println!("Bookmarked:   Yes");
                    }
                    if let Some(outcome) = &meta.outcome {
                        println!("Outcome:      {outcome}");
                    }
                    if !meta.tags.is_empty() {
                        println!("Tags:         {}", meta.tags.join(", "));
                    }
                }
            }

            // Show tree structure if requested
            if args.tree {
                println!();
                show_tree_structure(session, cli.max_file_size)?;
            }

            // Show specific entry if requested
            if let Some(uuid) = &args.entry {
                println!();
                show_entry(session, uuid, cli.max_file_size)?;
            }

            // Show raw entries if requested
            if args.raw {
                println!();
                println!("Raw Entries:");
                println!("------------");
                let entries = session.parse_with_options(cli.max_file_size)?;
                for (i, entry) in entries.iter().enumerate() {
                    println!("[{}] {}: {}",
                        i,
                        entry.message_type(),
                        entry.uuid().unwrap_or("no-uuid")
                    );
                }
            }

            // Show message preview if requested
            if let Some(n) = args.messages {
                println!();
                println!("Message Preview (first {n} messages):");
                println!("-------------------------------");
                show_message_preview(session, n, cli.max_file_size)?;
            }

            // Show files touched if requested
            if args.files {
                println!();
                println!("Files Touched:");
                println!("--------------");
                show_files_touched(session, cli.max_file_size)?;
            }
        }
    }

    Ok(())
}

/// Show tree structure of a session.
fn show_tree_structure(session: &crate::discovery::Session, max_file_size: Option<u64>) -> Result<()> {
    let entries = session.parse_with_options(max_file_size)?;
    let conversation = Conversation::from_entries(entries)?;
    let stats = conversation.statistics();

    println!("Tree Structure:");
    println!("  Nodes:        {}", stats.total_nodes);
    println!("  Max Depth:    {}", stats.max_depth);
    println!("  Main Thread:  {} entries", stats.main_thread_length);
    println!("  Branches:     {}", stats.branch_count);
    println!("  Tool Uses:    {}", stats.tool_uses);
    println!("  Tool Results: {}", stats.tool_results);
    println!(
        "  Balanced:     {} (tool calls match results)",
        if stats.tools_balanced() { "yes" } else { "no" }
    );

    if stats.branch_count > 0 {
        println!();
        println!("Branch Points:");
        for bp in conversation.branch_points() {
            if let Some(node) = conversation.get_node(bp) {
                println!("  {} ({} children at depth {})",
                    &bp[..8.min(bp.len())],
                    node.children.len(),
                    node.depth
                );
            }
        }
    }

    Ok(())
}

/// Show a specific entry.
fn show_entry(session: &crate::discovery::Session, uuid: &str, max_file_size: Option<u64>) -> Result<()> {
    let entries = session.parse_with_options(max_file_size)?;

    for entry in &entries {
        if entry.uuid() == Some(uuid) || entry.uuid().map(|u| u.starts_with(uuid)).unwrap_or(false) {
            println!("Entry: {}", entry.uuid().unwrap_or("unknown"));
            println!("Type: {}", entry.message_type());
            println!();
            println!("{}", serde_json::to_string_pretty(&entry)?);
            return Ok(());
        }
    }

    println!("Entry not found: {uuid}");
    Ok(())
}

/// Show a preview of the first N messages.
fn show_message_preview(session: &crate::discovery::Session, n: usize, max_file_size: Option<u64>) -> Result<()> {
    let entries = session.parse_with_options(max_file_size)?;

    let mut count = 0;
    for entry in &entries {
        if count >= n {
            break;
        }

        match entry {
            LogEntry::User(user_msg) => {
                // Skip tool results
                if user_msg.message.has_tool_results() {
                    continue;
                }

                if let Some(text) = user_msg.message.as_text() {
                    let preview = truncate_preview(text, 200);
                    println!();
                    println!("[{}] User:", count + 1);
                    for line in preview.lines() {
                        println!("  {}", line);
                    }
                    count += 1;
                }
            }
            LogEntry::Assistant(asst_msg) => {
                // Extract text from assistant response
                let mut text_parts = Vec::new();
                for block in &asst_msg.message.content {
                    if let ContentBlock::Text(text_block) = block {
                        text_parts.push(text_block.text.clone());
                    }
                }

                if !text_parts.is_empty() {
                    let combined = text_parts.join("\n");
                    let preview = truncate_preview(&combined, 200);
                    println!();
                    println!("[{}] Assistant:", count + 1);
                    for line in preview.lines() {
                        println!("  {}", line);
                    }
                    count += 1;
                }
            }
            _ => {}
        }
    }

    if count == 0 {
        println!("No messages found.");
    }

    Ok(())
}

/// Truncate text for preview display.
fn truncate_preview(text: &str, max_len: usize) -> String {
    let cleaned: String = text
        .lines()
        .map(|l| l.trim())
        .filter(|l| !l.is_empty())
        .take(5)
        .collect::<Vec<_>>()
        .join("\n");

    if cleaned.len() > max_len {
        format!("{}...", &cleaned[..max_len])
    } else {
        cleaned
    }
}

/// Show files touched during the session.
fn show_files_touched(session: &crate::discovery::Session, max_file_size: Option<u64>) -> Result<()> {
    let entries = session.parse_with_options(max_file_size)?;

    let mut files_read: HashSet<String> = HashSet::new();
    let mut files_written: HashSet<String> = HashSet::new();
    let mut files_created: HashSet<String> = HashSet::new();
    let mut _tool_counts: HashMap<String, usize> = HashMap::new();

    for entry in &entries {
        if let LogEntry::Assistant(asst_msg) = entry {
            for block in &asst_msg.message.content {
                if let ContentBlock::ToolUse(tool_use) = block {
                    let tool_name = tool_use.name.as_str();
                    *_tool_counts.entry(tool_name.to_string()).or_insert(0) += 1;

                    // Extract file paths from tool inputs
                    let input = &tool_use.input;
                    match tool_name {
                        "Read" => {
                            if let Some(path) = input.get("file_path").and_then(serde_json::Value::as_str) {
                                files_read.insert(path.to_string());
                            }
                        }
                        "Write" => {
                            if let Some(path) = input.get("file_path").and_then(serde_json::Value::as_str) {
                                files_created.insert(path.to_string());
                            }
                        }
                        "Edit" => {
                            if let Some(path) = input.get("file_path").and_then(serde_json::Value::as_str) {
                                files_written.insert(path.to_string());
                            }
                        }
                        "Bash" => {
                            // Could potentially extract file paths from bash commands
                            // but it's complex to parse reliably
                        }
                        _ => {}
                    }
                }
            }
        }
    }

    // Display summary
    let total_files = files_read.len() + files_written.len() + files_created.len();
    if total_files == 0 {
        println!("No file operations detected.");
        return Ok(());
    }

    if !files_read.is_empty() {
        println!();
        println!("Read ({}):", files_read.len());
        for path in files_read.iter().take(20) {
            println!("  {}", path);
        }
        if files_read.len() > 20 {
            println!("  ... and {} more", files_read.len() - 20);
        }
    }

    if !files_written.is_empty() {
        println!();
        println!("Modified ({}):", files_written.len());
        for path in files_written.iter().take(20) {
            println!("  {}", path);
        }
        if files_written.len() > 20 {
            println!("  ... and {} more", files_written.len() - 20);
        }
    }

    if !files_created.is_empty() {
        println!();
        println!("Created ({}):", files_created.len());
        for path in files_created.iter().take(20) {
            println!("  {}", path);
        }
        if files_created.len() > 20 {
            println!("  ... and {} more", files_created.len() - 20);
        }
    }

    Ok(())
}

/// Show project information.
fn show_project_info(
    cli: &Cli,
    args: &InfoArgs,
    project: &crate::discovery::Project,
) -> Result<()> {
    let sessions = project.sessions()?;
    let main_count = sessions.iter().filter(|s| !s.is_subagent()).count();
    let subagent_count = sessions.iter().filter(|s| s.is_subagent()).count();
    let total_size: u64 = sessions.iter().map(|s| s.file_size()).sum();

    match cli.effective_output() {
        OutputFormat::Json => {
            println!("{}", serde_json::to_string_pretty(&ProjectInfoOutput {
                path: project.decoded_path().to_string(),
                encoded_name: project.encoded_name().to_string(),
                session_count: sessions.len(),
                main_sessions: main_count,
                subagent_sessions: subagent_count,
                total_size,
                total_size_human: crate::discovery::format_size(total_size),
            })?);
        }
        OutputFormat::Tsv => {
            println!("field\tvalue");
            println!("path\t{}", project.decoded_path());
            println!("sessions\t{}", sessions.len());
            println!("size\t{}", total_size);
        }
        OutputFormat::Compact => {
            println!("{}:{}:{}", project.decoded_path(), sessions.len(), crate::discovery::format_size(total_size));
        }
        OutputFormat::Text => {
            println!("Project Information");
            println!("===================");
            println!();
            println!("Path:           {}", project.decoded_path());
            println!("Encoded:        {}", project.encoded_name());
            println!();
            println!("Sessions:       {}", sessions.len());
            println!("  Main:         {main_count}");
            println!("  Subagents:    {subagent_count}");
            println!();
            println!("Total Size:     {}", crate::discovery::format_size(total_size));

            if args.paths {
                println!();
                println!("Directory:      {}", project.path().display());
            }

            // List sessions
            println!();
            println!("Sessions:");
            for session in &sessions {
                let subagent = if session.is_subagent() { " [subagent]" } else { "" };
                println!("  {}{} ({})",
                    &session.session_id()[..8.min(session.session_id().len())],
                    subagent,
                    session.file_size_human()
                );
            }
        }
    }

    Ok(())
}

/// Show directory information.
fn show_directory_info(
    cli: &Cli,
    args: &InfoArgs,
    claude_dir: &crate::discovery::ClaudeDirectory,
) -> Result<()> {
    let stats = claude_dir.statistics()?;

    match cli.effective_output() {
        OutputFormat::Json => {
            println!("{}", serde_json::to_string_pretty(&DirectoryInfoOutput {
                root_path: claude_dir.root().to_string_lossy().to_string(),
                project_count: stats.project_count,
                session_count: stats.session_count,
                subagent_count: stats.subagent_count,
                total_size: stats.total_size_bytes,
                total_size_human: stats.total_size_human(),
                has_file_history: stats.has_file_history,
                backup_file_count: stats.backup_file_count,
            })?);
        }
        OutputFormat::Tsv => {
            println!("field\tvalue");
            println!("root\t{}", claude_dir.root().display());
            println!("projects\t{}", stats.project_count);
            println!("sessions\t{}", stats.session_count);
            println!("size\t{}", stats.total_size_bytes);
        }
        OutputFormat::Compact => {
            println!("{}:{}:{}",
                claude_dir.root().display(),
                stats.project_count,
                stats.session_count
            );
        }
        OutputFormat::Text => {
            println!("Claude Code Directory");
            println!("=====================");
            println!();
            println!("Root:           {}", claude_dir.root().display());
            println!();
            println!("Projects:       {}", stats.project_count);
            println!("Sessions:       {}", stats.session_count);
            println!("Subagents:      {}", stats.subagent_count);
            println!("Total Size:     {}", stats.total_size_human());

            if stats.has_file_history {
                println!();
                println!("File History:   Yes ({} backups)", stats.backup_file_count);
            }

            if args.paths {
                println!();
                println!("Paths:");
                println!("  Projects:     {}", claude_dir.projects_dir().display());
                println!("  File History: {}", claude_dir.file_history_dir().display());
                println!("  Settings:     {}", claude_dir.settings_path().display());
                println!("  CLAUDE.md:    {}", claude_dir.claude_md_path().display());
                println!("  MCP Config:   {}", claude_dir.mcp_config_path().display());
            }
        }
    }

    Ok(())
}

/// Session info for JSON output.
#[derive(Debug, serde::Serialize)]
struct SessionInfoOutput {
    session_id: String,
    project_path: String,
    is_subagent: bool,
    file_size: u64,
    file_size_human: String,
    entry_count: usize,
    message_count: usize,
    start_time: Option<String>,
    end_time: Option<String>,
    duration_human: Option<String>,
    state: String,
    version: Option<String>,
    path: String,
    /// Human-readable name for the session.
    #[serde(skip_serializing_if = "Option::is_none")]
    name: Option<String>,
    /// Tags associated with the session.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    tags: Vec<String>,
    /// Whether this session is bookmarked.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    bookmarked: bool,
    /// Outcome classification.
    #[serde(skip_serializing_if = "Option::is_none")]
    outcome: Option<String>,
}

/// Project info for JSON output.
#[derive(Debug, serde::Serialize)]
struct ProjectInfoOutput {
    path: String,
    encoded_name: String,
    session_count: usize,
    main_sessions: usize,
    subagent_sessions: usize,
    total_size: u64,
    total_size_human: String,
}

/// Directory info for JSON output.
#[derive(Debug, serde::Serialize)]
struct DirectoryInfoOutput {
    root_path: String,
    project_count: usize,
    session_count: usize,
    subagent_count: usize,
    total_size: u64,
    total_size_human: String,
    has_file_history: bool,
    backup_file_count: usize,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_session_info_output_serialization() {
        let output = SessionInfoOutput {
            session_id: "abc123".to_string(),
            project_path: "/home/user/project".to_string(),
            is_subagent: false,
            file_size: 1024,
            file_size_human: "1 KB".to_string(),
            entry_count: 50,
            message_count: 25,
            start_time: Some("2025-01-01T00:00:00Z".to_string()),
            end_time: Some("2025-01-01T01:00:00Z".to_string()),
            duration_human: Some("1 hour".to_string()),
            state: "Complete".to_string(),
            version: Some("2.0.74".to_string()),
            path: "/home/user/.claude/projects/abc123.jsonl".to_string(),
            name: Some("My Session".to_string()),
            tags: vec!["feature".to_string(), "urgent".to_string()],
            bookmarked: true,
            outcome: Some("success".to_string()),
        };

        let json = serde_json::to_string(&output).unwrap();
        assert!(json.contains("\"session_id\":\"abc123\""));
        assert!(json.contains("\"is_subagent\":false"));
        assert!(json.contains("\"entry_count\":50"));
        assert!(json.contains("\"version\":\"2.0.74\""));
        assert!(json.contains("\"name\":\"My Session\""));
        assert!(json.contains("\"tags\":[\"feature\",\"urgent\"]"));
        assert!(json.contains("\"bookmarked\":true"));
        assert!(json.contains("\"outcome\":\"success\""));
    }

    #[test]
    fn test_project_info_output_serialization() {
        let output = ProjectInfoOutput {
            path: "/home/user/project".to_string(),
            encoded_name: "encoded_project".to_string(),
            session_count: 10,
            main_sessions: 8,
            subagent_sessions: 2,
            total_size: 1024 * 1024,
            total_size_human: "1 MB".to_string(),
        };

        let json = serde_json::to_string(&output).unwrap();
        assert!(json.contains("\"path\":\"/home/user/project\""));
        assert!(json.contains("\"session_count\":10"));
        assert!(json.contains("\"main_sessions\":8"));
        assert!(json.contains("\"subagent_sessions\":2"));
    }

    #[test]
    fn test_directory_info_output_serialization() {
        let output = DirectoryInfoOutput {
            root_path: "/home/user/.claude".to_string(),
            project_count: 5,
            session_count: 20,
            subagent_count: 10,
            total_size: 10 * 1024 * 1024,
            total_size_human: "10 MB".to_string(),
            has_file_history: true,
            backup_file_count: 100,
        };

        let json = serde_json::to_string(&output).unwrap();
        assert!(json.contains("\"root_path\":\"/home/user/.claude\""));
        assert!(json.contains("\"project_count\":5"));
        assert!(json.contains("\"has_file_history\":true"));
        assert!(json.contains("\"backup_file_count\":100"));
    }

    #[test]
    fn test_session_info_output_with_nulls() {
        let output = SessionInfoOutput {
            session_id: "test".to_string(),
            project_path: "project".to_string(),
            is_subagent: true,
            file_size: 0,
            file_size_human: "0 B".to_string(),
            entry_count: 0,
            message_count: 0,
            start_time: None,
            end_time: None,
            duration_human: None,
            state: "Unknown".to_string(),
            version: None,
            path: "/test".to_string(),
            name: None,
            tags: vec![],
            bookmarked: false,
            outcome: None,
        };

        let json = serde_json::to_string(&output).unwrap();
        assert!(json.contains("\"is_subagent\":true"));
        assert!(json.contains("\"start_time\":null"));
        assert!(json.contains("\"version\":null"));
        // name, tags, bookmarked, and outcome should be skipped when empty/false/None
        assert!(!json.contains("\"name\""));
        assert!(!json.contains("\"tags\""));
        assert!(!json.contains("\"bookmarked\""));
        assert!(!json.contains("\"outcome\""));
    }
}
