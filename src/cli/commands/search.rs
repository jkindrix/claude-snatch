//! Search command implementation.
//!
//! Searches across sessions for text patterns.

use regex::{Regex, RegexBuilder};

use crate::cli::{Cli, OutputFormat, SearchArgs};
use crate::error::{Result, SnatchError};
use crate::model::{ContentBlock, LogEntry};

use super::get_claude_dir;

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

    // Search each session
    for session in sessions {
        let entries = match session.parse() {
            Ok(e) => e,
            Err(_) => continue, // Skip unparseable sessions
        };

        for entry in &entries {
            let matches = search_entry(entry, &regex, args);

            if !matches.is_empty() {
                for m in matches {
                    total_matches += 1;

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

                    if let Some(limit) = args.limit {
                        if total_matches >= limit {
                            break;
                        }
                    }
                }
            }
        }

        if let Some(limit) = args.limit {
            if total_matches >= limit {
                break;
            }
        }
    }

    // Output results
    match cli.effective_output() {
        OutputFormat::Json => {
            println!("{}", serde_json::to_string_pretty(&all_results)?);
        }
        OutputFormat::Tsv => {
            println!("session\tproject\tuuid\ttype\tlocation\tline");
            for result in &all_results {
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
            for result in &all_results {
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

            for result in &all_results {
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
