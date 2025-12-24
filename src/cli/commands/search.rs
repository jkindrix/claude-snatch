//! Search command implementation.
//!
//! Searches across sessions for text patterns with optional filters.

use std::collections::HashSet;

use regex::{Regex, RegexBuilder};

use crate::cli::{Cli, OutputFormat, SearchArgs};
use crate::error::{Result, SnatchError};
use crate::model::{ContentBlock, LogEntry};

use super::get_claude_dir;

/// Check if an entry matches the search filters.
fn matches_filters(entry: &LogEntry, args: &SearchArgs) -> bool {
    // Check message type filter
    if let Some(ref type_filter) = args.message_type {
        if !matches_message_type(entry, type_filter) {
            return false;
        }
    }

    // Check model filter
    if let Some(ref model_filter) = args.model {
        if !matches_model(entry, model_filter) {
            return false;
        }
    }

    // Check tool name filter
    if let Some(ref tool_filter) = args.tool_name {
        if !contains_tool(entry, tool_filter) {
            return false;
        }
    }

    // Check error filter
    if args.errors && !is_error_message(entry) {
        return false;
    }

    true
}

/// Check if an entry matches the model filter.
fn matches_model(entry: &LogEntry, model_filter: &str) -> bool {
    let model_filter_lower = model_filter.to_lowercase();
    match entry {
        LogEntry::Assistant(msg) => {
            // Check if assistant message has model info in the message
            msg.message.model.to_lowercase().contains(&model_filter_lower)
        }
        _ => true, // Non-assistant messages don't have model info, so don't filter them out
    }
}

/// Check if an entry contains a specific tool use.
fn contains_tool(entry: &LogEntry, tool_filter: &str) -> bool {
    let tool_filter_lower = tool_filter.to_lowercase();
    match entry {
        LogEntry::Assistant(msg) => {
            for block in &msg.message.content {
                if let ContentBlock::ToolUse(tool) = block {
                    if tool.name.to_lowercase().contains(&tool_filter_lower) {
                        return true;
                    }
                }
            }
            false
        }
        LogEntry::User(_) => {
            // Tool results don't have the tool name directly, so we can't filter by tool name here
            // This would require tracking the parent tool_use to get the name
            false
        }
        _ => false,
    }
}

/// Check if an entry is an error message.
fn is_error_message(entry: &LogEntry) -> bool {
    use crate::model::message::SystemSubtype;

    match entry {
        LogEntry::Assistant(msg) => msg.is_api_error_message.unwrap_or(false),
        LogEntry::System(msg) => {
            // Check for api_error subtype
            matches!(msg.subtype, Some(SystemSubtype::ApiError))
        }
        _ => false,
    }
}

/// Run the search command.
pub fn run(cli: &Cli, args: &SearchArgs) -> Result<()> {
    let claude_dir = get_claude_dir(cli.claude_dir.as_ref())?;

    // Build regex
    let regex = RegexBuilder::new(&args.pattern)
        .case_insensitive(args.ignore_case)
        .build()
        .map_err(|e| SnatchError::InvalidArgument {
            name: "pattern".to_string(),
            reason: e.to_string(),
        })?;

    // Collect sessions to search
    let sessions = if let Some(session_id) = &args.session {
        let session = claude_dir
            .find_session(session_id)?
            .ok_or_else(|| SnatchError::SessionNotFound {
                session_id: session_id.clone(),
            })?;
        vec![session]
    } else if let Some(project_filter) = &args.project {
        let projects = claude_dir.projects()?;
        let mut sessions = Vec::new();
        for project in projects {
            if project.decoded_path().contains(project_filter) {
                sessions.extend(project.sessions()?);
            }
        }
        sessions
    } else {
        claude_dir.all_sessions()?
    };

    let mut total_matches = 0;
    let mut all_results = Vec::new();
    let mut sessions_with_matches: HashSet<String> = HashSet::new();
    let mut match_counts: std::collections::HashMap<String, usize> = std::collections::HashMap::new();

    // Search each session
    for session in sessions {
        let entries = match session.parse() {
            Ok(e) => e,
            Err(_) => continue, // Skip unparseable sessions
        };

        let mut session_match_count = 0;

        for entry in &entries {
            // Apply all filters
            if !matches_filters(entry, args) {
                continue;
            }

            let matches = search_entry(entry, &regex, args);

            if !matches.is_empty() {
                sessions_with_matches.insert(session.session_id().to_string());

                for m in matches {
                    total_matches += 1;
                    session_match_count += 1;

                    // For files_only or count mode, we don't need to store full results
                    if !args.files_only && !args.count {
                        let result = SearchResult {
                            session_id: session.session_id().to_string(),
                            project: session.project_path().to_string(),
                            uuid: entry.uuid().unwrap_or("").to_string(),
                            entry_type: entry.message_type().to_string(),
                            location: m.location,
                            line: m.line,
                            context_before: m.context_before,
                            matched_text: m.matched_text,
                            context_after: m.context_after,
                        };

                        all_results.push(result);
                    }

                    if let Some(limit) = args.limit {
                        if total_matches >= limit {
                            break;
                        }
                    }
                }
            }
        }

        if session_match_count > 0 {
            match_counts.insert(session.session_id().to_string(), session_match_count);
        }

        if let Some(limit) = args.limit {
            if total_matches >= limit {
                break;
            }
        }
    }

    // Output results based on mode
    if args.files_only {
        output_files_only(cli, &sessions_with_matches)?;
    } else if args.count {
        output_count(cli, &match_counts, total_matches)?;
    } else {
        output_full_results(cli, args, &all_results, total_matches)?;
    }

    Ok(())
}

/// Check if an entry matches the message type filter.
fn matches_message_type(entry: &LogEntry, type_filter: &str) -> bool {
    let entry_type = entry.message_type().to_lowercase();
    match type_filter {
        "user" => entry_type == "user",
        "assistant" => entry_type == "assistant",
        "system" => entry_type == "system",
        "summary" => entry_type == "summary",
        _ => entry_type.contains(type_filter),
    }
}

/// Output only session IDs with matches.
fn output_files_only(cli: &Cli, sessions: &HashSet<String>) -> Result<()> {
    match cli.effective_output() {
        OutputFormat::Json => {
            let sessions_vec: Vec<&String> = sessions.iter().collect();
            println!("{}", serde_json::to_string_pretty(&sessions_vec)?);
        }
        _ => {
            for session_id in sessions {
                println!("{}", session_id);
            }
        }
    }
    Ok(())
}

/// Output match counts.
fn output_count(
    cli: &Cli,
    match_counts: &std::collections::HashMap<String, usize>,
    total: usize,
) -> Result<()> {
    match cli.effective_output() {
        OutputFormat::Json => {
            let output = serde_json::json!({
                "total": total,
                "by_session": match_counts,
            });
            println!("{}", serde_json::to_string_pretty(&output)?);
        }
        _ => {
            if match_counts.len() == 1 {
                // Single session - just show count
                println!("{}", total);
            } else {
                // Multiple sessions - show per-session counts
                let mut counts: Vec<(&String, &usize)> = match_counts.iter().collect();
                counts.sort_by(|a, b| b.1.cmp(a.1));

                for (session_id, count) in counts {
                    println!("{}:{}", &session_id[..8.min(session_id.len())], count);
                }
                println!();
                println!("Total: {}", total);
            }
        }
    }
    Ok(())
}

/// Output full search results.
fn output_full_results(
    cli: &Cli,
    args: &SearchArgs,
    all_results: &[SearchResult],
    total_matches: usize,
) -> Result<()> {
    match cli.effective_output() {
        OutputFormat::Json => {
            println!("{}", serde_json::to_string_pretty(&all_results)?);
        }
        OutputFormat::Tsv => {
            println!("session\tproject\tuuid\ttype\tlocation\tline");
            for result in all_results {
                println!(
                    "{}\t{}\t{}\t{}\t{}\t{}",
                    &result.session_id[..8.min(result.session_id.len())],
                    result.project,
                    &result.uuid[..8.min(result.uuid.len())],
                    result.entry_type,
                    result.location,
                    result.line.replace('\t', " ")
                );
            }
        }
        OutputFormat::Compact => {
            for result in all_results {
                println!("{}:{}: {}",
                    &result.session_id[..8.min(result.session_id.len())],
                    result.location,
                    result.matched_text
                );
            }
        }
        OutputFormat::Text => {
            if all_results.is_empty() {
                println!("No matches found.");
                return Ok(());
            }

            println!("Found {} matches:", total_matches);
            println!();

            let mut current_session = String::new();

            for result in all_results {
                if result.session_id != current_session {
                    current_session = result.session_id.clone();
                    println!("Session: {} ({})",
                        &result.session_id[..8.min(result.session_id.len())],
                        result.project
                    );
                }

                println!();
                println!("  [{} in {}]", result.entry_type, result.location);

                if args.context > 0 && !result.context_before.is_empty() {
                    for line in result.context_before.lines() {
                        println!("  | {}", line);
                    }
                }

                println!("  > {}", result.matched_text);

                if args.context > 0 && !result.context_after.is_empty() {
                    for line in result.context_after.lines() {
                        println!("  | {}", line);
                    }
                }
            }
        }
    }

    Ok(())
}

/// Search result.
#[derive(Debug, serde::Serialize)]
struct SearchResult {
    session_id: String,
    project: String,
    uuid: String,
    entry_type: String,
    location: String,
    line: String,
    context_before: String,
    matched_text: String,
    context_after: String,
}

/// A match within an entry.
struct Match {
    location: String,
    line: String,
    context_before: String,
    matched_text: String,
    context_after: String,
}

/// Search an entry for matches.
fn search_entry(entry: &LogEntry, regex: &Regex, args: &SearchArgs) -> Vec<Match> {
    let mut matches = Vec::new();

    match entry {
        LogEntry::User(user) => {
            // Search user content
            let text = match &user.message {
                crate::model::UserContent::Simple(s) => s.content.clone(),
                crate::model::UserContent::Blocks(b) => {
                    b.content.iter().filter_map(|c| {
                        match c {
                            ContentBlock::Text(t) => Some(t.text.as_str()),
                            _ => None,
                        }
                    }).collect::<Vec<_>>().join("\n")
                }
            };

            if regex.is_match(&text) {
                matches.extend(find_matches(&text, regex, "user message", args.context));
            }
        }
        LogEntry::Assistant(assistant) => {
            for block in &assistant.message.content {
                match block {
                    ContentBlock::Text(text) => {
                        if regex.is_match(&text.text) {
                            matches.extend(find_matches(&text.text, regex, "assistant text", args.context));
                        }
                    }
                    ContentBlock::Thinking(thinking) if args.thinking || args.all => {
                        if regex.is_match(&thinking.thinking) {
                            matches.extend(find_matches(&thinking.thinking, regex, "thinking", args.context));
                        }
                    }
                    ContentBlock::ToolUse(tool) if args.tools || args.all => {
                        let input_str = serde_json::to_string(&tool.input).unwrap_or_default();
                        if regex.is_match(&input_str) {
                            matches.extend(find_matches(&input_str, regex, &format!("tool:{}", tool.name), args.context));
                        }
                    }
                    ContentBlock::ToolResult(result) if args.tools || args.all => {
                        if let Some(content) = &result.content {
                            if let crate::model::content::ToolResultContent::String(text) = content {
                                if regex.is_match(text) {
                                    matches.extend(find_matches(text, regex, "tool result", args.context));
                                }
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
        LogEntry::System(system) => {
            if let Some(content) = &system.content {
                if regex.is_match(content) {
                    matches.extend(find_matches(content, regex, "system", args.context));
                }
            }
        }
        LogEntry::Summary(summary) => {
            if regex.is_match(&summary.summary) {
                matches.extend(find_matches(&summary.summary, regex, "summary", args.context));
            }
        }
        _ => {}
    }

    matches
}

/// Find matches with context in text.
fn find_matches(text: &str, regex: &Regex, location: &str, context_lines: usize) -> Vec<Match> {
    let lines: Vec<&str> = text.lines().collect();
    let mut matches = Vec::new();
    let mut seen_lines = std::collections::HashSet::new();

    for (line_num, line) in lines.iter().enumerate() {
        if regex.is_match(line) && !seen_lines.contains(&line_num) {
            seen_lines.insert(line_num);

            // Get context
            let start = line_num.saturating_sub(context_lines);
            let end = (line_num + context_lines + 1).min(lines.len());

            let context_before = lines[start..line_num].join("\n");
            let context_after = lines[(line_num + 1)..end].join("\n");

            // Extract matched portion
            let matched_text = if let Some(m) = regex.find(line) {
                m.as_str().to_string()
            } else {
                line.to_string()
            };

            matches.push(Match {
                location: location.to_string(),
                line: (*line).to_string(),
                context_before,
                matched_text,
                context_after,
            });
        }
    }

    matches
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_find_matches_single_line() {
        let regex = Regex::new("hello").unwrap();
        let matches = find_matches("hello world", &regex, "test", 0);
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].matched_text, "hello");
    }

    #[test]
    fn test_find_matches_with_context() {
        let regex = Regex::new("target").unwrap();
        let text = "line 1\nline 2\ntarget line\nline 4\nline 5";
        let matches = find_matches(text, &regex, "test", 1);
        assert_eq!(matches.len(), 1);
        assert!(matches[0].context_before.contains("line 2"));
        assert!(matches[0].context_after.contains("line 4"));
    }

    #[test]
    fn test_find_matches_no_match() {
        let regex = Regex::new("notfound").unwrap();
        let matches = find_matches("hello world", &regex, "test", 0);
        assert!(matches.is_empty());
    }
}
