//! Info command implementation.
//!
//! Displays detailed information about sessions and projects.

use std::collections::{HashMap, HashSet};

use crate::analytics::SessionAnalytics;
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
        let matched = super::helpers::filter_projects(projects, target);
        if let Some(project) = matched.into_iter().next() {
            return show_project_info(cli, args, &project);
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
            // Compute analytics from conversation (the expensive part)
            let analytics = match session.parse_with_options(cli.max_file_size) {
                Ok(entries) => {
                    let conversation = Conversation::from_entries(entries)?;
                    Some(SessionAnalytics::from_conversation(&conversation))
                }
                Err(_) => None,
            };

            let (user_messages, assistant_messages, primary_model, tools_summary,
                 estimated_cost, input_tokens, output_tokens,
                 files_modified, files_created, lines_added, lines_removed) =
                if let Some(ref a) = analytics {
                    // Collect tool counts into a HashMap
                    let tools: HashMap<String, usize> = a.tool_counts.iter()
                        .map(|(k, v)| (k.clone(), *v))
                        .collect();

                    // Collect file paths: edited files vs created files
                    let edited: Vec<String> = a.file_stats.files.keys()
                        .cloned()
                        .collect();
                    // We don't have separate created-vs-edited file lists in FileModificationStats,
                    // but we have the counts. Use the file list as "files_modified".

                    (
                        Some(a.message_counts.user),
                        Some(a.message_counts.assistant),
                        a.primary_model().map(|s| s.to_string()),
                        if tools.is_empty() { None } else { Some(tools) },
                        a.usage.estimated_cost,
                        Some(a.usage.usage.input_tokens + a.usage.usage.cache_read_input_tokens.unwrap_or(0)),
                        Some(a.usage.usage.output_tokens),
                        if edited.is_empty() { None } else { Some(edited) },
                        None::<Vec<String>>, // files_created not separately tracked at path level
                        Some(a.file_stats.total_lines_added),
                        Some(a.file_stats.total_lines_removed),
                    )
                } else {
                    (None, None, None, None, None, None, None, None, None, None, None)
                };

            println!("{}", serde_json::to_string_pretty(&SessionInfoOutput {
                session_id: summary.session_id.clone(),
                project_path: summary.project_path.clone(),
                is_subagent: summary.is_subagent,
                parent_session_id: summary.parent_session_id.clone(),
                file_size: summary.file_size,
                file_size_human: summary.file_size_human.clone(),
                entry_count: summary.entry_count,
                message_count: summary.message_count,
                compaction_count: summary.compaction_count,
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
                user_messages,
                assistant_messages,
                primary_model,
                tools_summary,
                estimated_cost,
                input_tokens,
                output_tokens,
                files_modified,
                files_created,
                lines_added,
                lines_removed,
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
            if let Some(ref parent) = summary.parent_session_id {
                println!("Parent:       {}", parent);
            }
            println!("Status:       {:?}", summary.state);
            if summary.compaction_count > 0 {
                println!("Compactions:  {}", summary.compaction_count);
            }
            println!();

            if args.paths {
                println!("File Path:    {}", session.path().display());
                println!();
            }

            println!("File Size:    {}", summary.file_size_human);
            println!("Entries:      {}", summary.entry_count);
            println!("Messages:     {}", summary.message_count);

            // Show tool result artifacts info
            let (tr_count, tr_size) = session.tool_result_stats();
            if tr_count > 0 {
                println!("Tool Results: {} files ({})", tr_count, crate::discovery::format_size(tr_size));
            }
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
    #[serde(skip_serializing_if = "Option::is_none")]
    parent_session_id: Option<String>,
    file_size: u64,
    file_size_human: String,
    entry_count: usize,
    message_count: usize,
    compaction_count: usize,
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

    // ── Analytics fields (computed on demand) ──

    /// User message count.
    #[serde(skip_serializing_if = "Option::is_none")]
    user_messages: Option<usize>,
    /// Assistant message count.
    #[serde(skip_serializing_if = "Option::is_none")]
    assistant_messages: Option<usize>,
    /// Primary model used.
    #[serde(skip_serializing_if = "Option::is_none")]
    primary_model: Option<String>,
    /// Tool usage counts (tool_name -> invocation_count).
    #[serde(skip_serializing_if = "Option::is_none")]
    tools_summary: Option<HashMap<String, usize>>,
    /// Estimated cost in USD.
    #[serde(skip_serializing_if = "Option::is_none")]
    estimated_cost: Option<f64>,
    /// Total input tokens.
    #[serde(skip_serializing_if = "Option::is_none")]
    input_tokens: Option<u64>,
    /// Total output tokens.
    #[serde(skip_serializing_if = "Option::is_none")]
    output_tokens: Option<u64>,
    /// Files modified during the session.
    #[serde(skip_serializing_if = "Option::is_none")]
    files_modified: Option<Vec<String>>,
    /// Files created during the session.
    #[serde(skip_serializing_if = "Option::is_none")]
    files_created: Option<Vec<String>>,
    /// Total lines added.
    #[serde(skip_serializing_if = "Option::is_none")]
    lines_added: Option<usize>,
    /// Total lines removed.
    #[serde(skip_serializing_if = "Option::is_none")]
    lines_removed: Option<usize>,
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
            parent_session_id: None,
            file_size: 1024,
            file_size_human: "1 KB".to_string(),
            entry_count: 50,
            message_count: 25,
            compaction_count: 2,
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
            user_messages: Some(12),
            assistant_messages: Some(13),
            primary_model: Some("claude-sonnet-4-20250514".to_string()),
            tools_summary: Some(HashMap::from([("Read".to_string(), 5), ("Edit".to_string(), 3)])),
            estimated_cost: Some(0.42),
            input_tokens: Some(50000),
            output_tokens: Some(10000),
            files_modified: Some(vec!["src/main.rs".to_string()]),
            files_created: None,
            lines_added: Some(100),
            lines_removed: Some(20),
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
        assert!(json.contains("\"user_messages\":12"));
        assert!(json.contains("\"primary_model\":\"claude-sonnet-4-20250514\""));
        assert!(json.contains("\"estimated_cost\":0.42"));
        assert!(json.contains("\"lines_added\":100"));
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
            parent_session_id: Some("parent-uuid".to_string()),
            file_size: 0,
            file_size_human: "0 B".to_string(),
            entry_count: 0,
            message_count: 0,
            compaction_count: 0,
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
            user_messages: None,
            assistant_messages: None,
            primary_model: None,
            tools_summary: None,
            estimated_cost: None,
            input_tokens: None,
            output_tokens: None,
            files_modified: None,
            files_created: None,
            lines_added: None,
            lines_removed: None,
        };

        let json = serde_json::to_string(&output).unwrap();
        assert!(json.contains("\"is_subagent\":true"));
        assert!(json.contains("\"start_time\":null"));
        assert!(json.contains("\"version\":null"));
        // Optional fields should be skipped when empty/false/None
        assert!(!json.contains("\"name\""));
        assert!(!json.contains("\"tags\""));
        assert!(!json.contains("\"bookmarked\""));
        assert!(!json.contains("\"outcome\""));
        assert!(!json.contains("\"user_messages\""));
        assert!(!json.contains("\"tools_summary\""));
        assert!(!json.contains("\"estimated_cost\""));
        assert!(!json.contains("\"files_modified\""));
    }
}
