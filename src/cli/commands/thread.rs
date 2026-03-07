//! Thread command implementation.
//!
//! Cross-session topic threading: searches all sessions for a pattern,
//! then presents matches with surrounding conversation context, ordered
//! chronologically across sessions.

use std::collections::HashSet;
use std::io::IsTerminal;
use std::time::SystemTime;

use chrono::{DateTime, Utc};
use indicatif::{ProgressBar, ProgressStyle};
use regex::RegexBuilder;

use crate::cli::{Cli, ThreadArgs};
use crate::discovery::Session;
use crate::error::{Result, SnatchError};
use crate::model::{ContentBlock, LogEntry};

use super::get_claude_dir;

/// A threaded exchange: a match with its surrounding conversation context.
#[derive(Debug)]
struct ThreadedExchange {
    /// Timestamp of the matched entry.
    timestamp: DateTime<Utc>,
    /// Session ID where the match occurred.
    session_id: String,
    /// Short session ID (first 8 chars).
    short_id: String,
    /// Project path.
    project: String,
    /// UUID of the matched entry (for dedup).
    entry_uuid: String,
    /// The user message before (or the matched user message itself).
    user_text: Option<String>,
    /// The assistant response (or the matched assistant message itself).
    assistant_text: Option<String>,
    /// Thinking block text if requested.
    thinking_text: Option<String>,
    /// Which part matched: "user", "assistant", or "thinking".
    match_location: String,
    /// Number of pattern matches in this exchange.
    match_count: usize,
}

/// Extract visible text from a LogEntry.
fn extract_entry_text(entry: &LogEntry) -> Option<String> {
    match entry {
        LogEntry::User(user) => {
            let text = match &user.message {
                crate::model::UserContent::Simple(s) => s.content.clone(),
                crate::model::UserContent::Blocks(b) => b
                    .content
                    .iter()
                    .filter_map(|c| {
                        if let ContentBlock::Text(t) = c {
                            Some(t.text.as_str())
                        } else {
                            None
                        }
                    })
                    .collect::<Vec<_>>()
                    .join("\n"),
            };
            if text.trim().is_empty() {
                None
            } else {
                Some(text)
            }
        }
        LogEntry::Assistant(assistant) => {
            let texts: Vec<&str> = assistant
                .message
                .content
                .iter()
                .filter_map(|block| {
                    if let ContentBlock::Text(t) = block {
                        Some(t.text.as_str())
                    } else {
                        None
                    }
                })
                .collect();
            let joined = texts.join("\n");
            if joined.trim().is_empty() {
                None
            } else {
                Some(joined)
            }
        }
        _ => None,
    }
}

/// Extract thinking text from an assistant entry.
fn extract_thinking_text(entry: &LogEntry) -> Option<String> {
    if let LogEntry::Assistant(assistant) = entry {
        let texts: Vec<&str> = assistant
            .message
            .content
            .iter()
            .filter_map(|block| {
                if let ContentBlock::Thinking(t) = block {
                    Some(t.thinking.as_str())
                } else {
                    None
                }
            })
            .collect();
        let joined = texts.join("\n");
        if joined.trim().is_empty() {
            None
        } else {
            Some(joined)
        }
    } else {
        None
    }
}

/// Collect sessions matching thread args (mirrors search's collect_sessions).
fn collect_sessions(cli: &Cli, args: &ThreadArgs) -> Result<Vec<Session>> {
    let claude_dir = get_claude_dir(cli.claude_dir.as_ref())?;

    let mut sessions = if let Some(session_id) = &args.session {
        let session = claude_dir
            .find_session(session_id)?
            .ok_or_else(|| SnatchError::SessionNotFound {
                session_id: session_id.clone(),
            })?;
        vec![session]
    } else if let Some(project_filter) = &args.project {
        let projects = claude_dir.projects()?;
        let mut sess = Vec::new();
        for project in projects {
            if project.decoded_path().contains(project_filter) {
                sess.extend(project.sessions()?);
            }
        }
        sess
    } else {
        claude_dir.all_sessions()?
    };

    // Date filters
    let since_time: Option<SystemTime> = if let Some(ref since) = args.since {
        Some(super::parse_date_filter(since)?)
    } else {
        None
    };
    let until_time: Option<SystemTime> = if let Some(ref until) = args.until {
        Some(super::parse_date_filter(until)?)
    } else {
        None
    };
    if since_time.is_some() || until_time.is_some() {
        sessions.retain(|s| {
            let modified = s.modified_time();
            if let Some(since) = since_time {
                if modified < since {
                    return false;
                }
            }
            if let Some(until) = until_time {
                if modified > until {
                    return false;
                }
            }
            true
        });
    }

    if let Some(n) = args.recent {
        sessions.sort_by(|a, b| b.modified_time().cmp(&a.modified_time()));
        sessions.truncate(n);
    }

    if args.no_subagents {
        sessions.retain(|s| !s.is_subagent());
    }

    Ok(sessions)
}

/// Truncate text to max_chars, appending "..." if truncated.
fn truncate(text: &str, max_chars: usize) -> String {
    if text.len() <= max_chars {
        text.to_string()
    } else {
        let boundary = text
            .char_indices()
            .nth(max_chars)
            .map(|(i, _)| i)
            .unwrap_or(text.len());
        format!("{}...", &text[..boundary])
    }
}

/// Run the thread command.
pub fn run(cli: &Cli, args: &ThreadArgs) -> Result<()> {
    if args.pattern.trim().is_empty() {
        return Err(SnatchError::InvalidArgument {
            name: "pattern".to_string(),
            reason: "thread pattern cannot be empty".to_string(),
        });
    }

    let regex = RegexBuilder::new(&args.pattern)
        .case_insensitive(args.ignore_case)
        .build()
        .map_err(|e| SnatchError::InvalidArgument {
            name: "pattern".to_string(),
            reason: e.to_string(),
        })?;

    let sessions = collect_sessions(cli, args)?;

    let session_count = sessions.len();
    let show_progress = session_count > 10 && std::io::stderr().is_terminal() && !cli.quiet;
    let progress = if show_progress {
        let pb = ProgressBar::new(session_count as u64);
        pb.set_style(
            ProgressStyle::default_bar()
                .template("{spinner:.cyan} [{bar:40.cyan/dim}] {pos}/{len} sessions")
                .unwrap()
                .progress_chars("█▓░"),
        );
        Some(pb)
    } else {
        None
    };

    let mut exchanges: Vec<ThreadedExchange> = Vec::new();

    for session in &sessions {
        if let Some(ref pb) = progress {
            pb.inc(1);
        }

        let entries = match session.parse_with_options(cli.max_file_size) {
            Ok(e) => e,
            Err(_) => continue,
        };

        // Filter to main thread only (skip sidechain/subagent messages)
        let main_entries: Vec<&LogEntry> = entries
            .iter()
            .filter(|e| !e.is_sidechain())
            .collect();

        // Track which entry UUIDs we've already included (dedup within session)
        let mut seen_uuids: HashSet<String> = HashSet::new();

        for (idx, entry) in main_entries.iter().enumerate() {
            let mut match_location = String::new();

            let user_text_owned;
            let assistant_text_owned;
            let thinking_text_owned;

            let entry_text = extract_entry_text(entry);
            let thinking_text = if args.thinking {
                extract_thinking_text(entry)
            } else {
                None
            };

            let mut match_count = 0;

            if let Some(ref text) = entry_text {
                let count = regex.find_iter(text).count();
                if count > 0 {
                    match_count += count;
                    match_location = entry.message_type().to_string();
                }
            }

            if let Some(ref text) = thinking_text {
                let count = regex.find_iter(text).count();
                if count > 0 {
                    match_count += count;
                    if match_location.is_empty() {
                        match_location = "thinking".to_string();
                    }
                }
            }

            if match_count == 0 {
                continue;
            }

            // Dedup: skip if we've seen this UUID
            let uuid = entry.uuid().unwrap_or("").to_string();
            if !uuid.is_empty() && !seen_uuids.insert(uuid.clone()) {
                continue;
            }

            // Build the exchange context
            let timestamp = entry.timestamp().unwrap_or_else(Utc::now);

            // Find surrounding context
            // If matched entry is user: look for next assistant response
            // If matched entry is assistant: look for previous user message
            user_text_owned = if entry.message_type() == "user" {
                extract_entry_text(entry)
            } else {
                // Look backward for previous user message
                let mut found = None;
                for prev_idx in (0..idx).rev() {
                    if main_entries[prev_idx].message_type() == "user" {
                        found = extract_entry_text(main_entries[prev_idx]);
                        break;
                    }
                }
                found
            };

            assistant_text_owned = if entry.message_type() == "assistant" {
                extract_entry_text(entry)
            } else {
                // Look forward for next assistant message
                let mut found = None;
                for next_idx in (idx + 1)..main_entries.len() {
                    if main_entries[next_idx].message_type() == "assistant" {
                        found = extract_entry_text(main_entries[next_idx]);
                        break;
                    }
                }
                found
            };

            thinking_text_owned = if args.thinking {
                if entry.message_type() == "assistant" {
                    extract_thinking_text(entry)
                } else {
                    // Look forward for next assistant's thinking
                    let mut found = None;
                    for next_idx in (idx + 1)..main_entries.len() {
                        if main_entries[next_idx].message_type() == "assistant" {
                            found = extract_thinking_text(main_entries[next_idx]);
                            break;
                        }
                    }
                    found
                }
            } else {
                None
            };

            let short_id = if session.session_id().len() >= 8 {
                session.session_id()[..8].to_string()
            } else {
                session.session_id().to_string()
            };

            exchanges.push(ThreadedExchange {
                timestamp,
                session_id: session.session_id().to_string(),
                short_id,
                project: session.project_path().to_string(),
                entry_uuid: uuid,
                user_text: user_text_owned,
                assistant_text: assistant_text_owned,
                thinking_text: thinking_text_owned,
                match_location,
                match_count,
            });
        }
    }

    if let Some(pb) = progress {
        pb.finish_and_clear();
    }

    // Sort chronologically
    exchanges.sort_by(|a, b| a.timestamp.cmp(&b.timestamp));

    // Apply limit
    let limit = if args.no_limit {
        exchanges.len()
    } else {
        args.limit.min(exchanges.len())
    };
    exchanges.truncate(limit);

    if exchanges.is_empty() {
        if !cli.quiet {
            println!("No matches found for pattern: {}", args.pattern);
        }
        return Ok(());
    }

    // Count unique sessions
    let unique_sessions: HashSet<&str> = exchanges.iter().map(|e| e.session_id.as_str()).collect();

    match cli.effective_output() {
        crate::cli::OutputFormat::Json => {
            output_json(&exchanges);
        }
        _ => {
            output_text(cli, &exchanges, &args.pattern, unique_sessions.len(), args.max_context);
        }
    }

    Ok(())
}

fn output_json(exchanges: &[ThreadedExchange]) {
    let entries: Vec<serde_json::Value> = exchanges
        .iter()
        .map(|e| {
            let mut obj = serde_json::json!({
                "timestamp": e.timestamp.to_rfc3339(),
                "session_id": e.session_id,
                "entry_uuid": e.entry_uuid,
                "project": e.project,
                "match_location": e.match_location,
                "match_count": e.match_count,
            });
            if let Some(ref text) = e.user_text {
                obj["user_text"] = serde_json::Value::String(text.clone());
            }
            if let Some(ref text) = e.assistant_text {
                obj["assistant_text"] = serde_json::Value::String(text.clone());
            }
            if let Some(ref text) = e.thinking_text {
                obj["thinking_text"] = serde_json::Value::String(text.clone());
            }
            obj
        })
        .collect();

    println!("{}", serde_json::to_string_pretty(&entries).unwrap_or_default());
}

fn output_text(
    cli: &Cli,
    exchanges: &[ThreadedExchange],
    pattern: &str,
    session_count: usize,
    max_context: usize,
) {
    if !cli.quiet {
        println!(
            "Thread: \"{}\" - {} exchanges across {} sessions\n",
            pattern,
            exchanges.len(),
            session_count,
        );
    }

    let mut last_session_id = String::new();

    for (i, exchange) in exchanges.iter().enumerate() {
        let date = exchange.timestamp.format("%Y-%m-%d %H:%M");

        // Print session header when session changes
        if exchange.session_id != last_session_id {
            if i > 0 {
                println!();
            }
            println!("--- {} [{}] ---", date.to_string().split(' ').next().unwrap_or(""), exchange.short_id);
            last_session_id = exchange.session_id.clone();
        }

        println!();
        println!(
            "  {} | {} in {} ({} match{})",
            date,
            exchange.match_location,
            exchange.short_id,
            exchange.match_count,
            if exchange.match_count == 1 { "" } else { "es" }
        );

        if let Some(ref text) = exchange.user_text {
            println!();
            println!("  USER:");
            for line in truncate(text, max_context).lines() {
                println!("    {}", line);
            }
        }

        if let Some(ref text) = exchange.assistant_text {
            println!();
            println!("  ASSISTANT:");
            for line in truncate(text, max_context).lines() {
                println!("    {}", line);
            }
        }

        if let Some(ref text) = exchange.thinking_text {
            println!();
            println!("  THINKING:");
            for line in truncate(text, max_context).lines() {
                println!("    {}", line);
            }
        }

        if i < exchanges.len() - 1 {
            println!();
            println!("  ─────────────────────────────────────────");
        }
    }

    println!();
}
