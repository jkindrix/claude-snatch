//! Thread command implementation.
//!
//! Cross-session topic threading: searches all sessions for a pattern,
//! then presents matches with surrounding conversation context, ordered
//! chronologically across sessions.

use std::collections::HashSet;
use std::io::IsTerminal;

use chrono::{DateTime, Utc};
use indicatif::{ProgressBar, ProgressStyle};
use regex::RegexBuilder;

use crate::cli::{Cli, ThreadArgs};
use crate::error::{Result, SnatchError};

use super::helpers::{
    self, extract_text, extract_thinking_text, looks_like_decision, short_id, truncate,
    SessionCollectParams,
};

/// A threaded exchange: a match with its surrounding conversation context.
#[derive(Debug)]
struct ThreadedExchange {
    timestamp: DateTime<Utc>,
    session_id: String,
    short_id: String,
    project: String,
    entry_uuid: String,
    user_text: Option<String>,
    assistant_text: Option<String>,
    thinking_text: Option<String>,
    match_location: String,
    match_count: usize,
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

    let sessions = helpers::collect_sessions(cli, &SessionCollectParams {
        session: args.session.as_deref(),
        project: args.project.as_deref(),
        since: args.since.as_deref(),
        until: args.until.as_deref(),
        recent: args.recent,
        no_subagents: args.no_subagents,
    })?;

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

        let main_entries = helpers::main_thread_entries(&entries);
        let mut seen_uuids: HashSet<String> = HashSet::new();

        for (idx, entry) in main_entries.iter().enumerate() {
            let mut match_location = String::new();

            let entry_text = extract_text(entry);
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

            // Filter by role if specified
            if let Some(ref role) = args.role {
                if entry.message_type() != role.as_str() {
                    continue;
                }
            }

            // Filter to decision-point exchanges only
            if args.decisions_only {
                let is_decision = entry_text.as_ref().map_or(false, |t| looks_like_decision(t));
                if !is_decision {
                    // Also check the paired assistant text (if current entry is user)
                    let paired_assistant = if entry.message_type() == "user" {
                        ((idx + 1)..main_entries.len())
                            .find(|&i| main_entries[i].message_type() == "assistant")
                            .and_then(|i| extract_text(main_entries[i]))
                    } else {
                        None
                    };
                    let paired_is_decision = paired_assistant
                        .as_ref()
                        .map_or(false, |t| looks_like_decision(t));
                    if !paired_is_decision {
                        continue;
                    }
                }
            }

            let uuid = entry.uuid().unwrap_or("").to_string();
            if !uuid.is_empty() && !seen_uuids.insert(uuid.clone()) {
                continue;
            }

            let timestamp = entry.timestamp().unwrap_or_else(Utc::now);

            let user_text = if entry.message_type() == "user" {
                extract_text(entry)
            } else {
                (0..idx).rev()
                    .find(|&i| main_entries[i].message_type() == "user")
                    .and_then(|i| extract_text(main_entries[i]))
            };

            let assistant_text = if entry.message_type() == "assistant" {
                extract_text(entry)
            } else {
                ((idx + 1)..main_entries.len())
                    .find(|&i| main_entries[i].message_type() == "assistant")
                    .and_then(|i| extract_text(main_entries[i]))
            };

            let thinking_text = if args.thinking {
                if entry.message_type() == "assistant" {
                    extract_thinking_text(entry)
                } else {
                    ((idx + 1)..main_entries.len())
                        .find(|&i| main_entries[i].message_type() == "assistant")
                        .and_then(|i| extract_thinking_text(main_entries[i]))
                }
            } else {
                None
            };

            exchanges.push(ThreadedExchange {
                timestamp,
                session_id: session.session_id().to_string(),
                short_id: short_id(session.session_id()).to_string(),
                project: session.project_path().to_string(),
                entry_uuid: uuid,
                user_text,
                assistant_text,
                thinking_text,
                match_location,
                match_count,
            });
        }
    }

    if let Some(pb) = progress {
        pb.finish_and_clear();
    }

    exchanges.sort_by(|a, b| a.timestamp.cmp(&b.timestamp));

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

    let unique_sessions: HashSet<&str> = exchanges.iter().map(|e| e.session_id.as_str()).collect();

    match cli.effective_output() {
        crate::cli::OutputFormat::Json => output_json(&exchanges),
        _ => {
            output_text(
                cli,
                &exchanges,
                &args.pattern,
                unique_sessions.len(),
                args.max_context,
                args.max_user_context.unwrap_or(args.max_context),
                args.max_assistant_context.unwrap_or(args.max_context),
            );
            if args.summary && !cli.quiet {
                output_summary(&exchanges, unique_sessions.len());
            }
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
    max_user_context: usize,
    max_assistant_context: usize,
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
            for line in truncate(text, max_user_context).lines() {
                println!("    {}", line);
            }
        }

        if let Some(ref text) = exchange.assistant_text {
            println!();
            println!("  ASSISTANT:");
            for line in truncate(text, max_assistant_context).lines() {
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

fn output_summary(exchanges: &[ThreadedExchange], session_count: usize) {
    if exchanges.is_empty() {
        return;
    }

    println!("═══════════════════════════════════════════");
    println!("  SUMMARY");
    println!("═══════════════════════════════════════════");

    let first = &exchanges[0];
    let last = &exchanges[exchanges.len() - 1];
    let total_matches: usize = exchanges.iter().map(|e| e.match_count).sum();

    // Time span
    let first_date = first.timestamp.format("%Y-%m-%d");
    let last_date = last.timestamp.format("%Y-%m-%d");
    if first_date.to_string() == last_date.to_string() {
        println!("  Date: {first_date}");
    } else {
        println!("  Span: {first_date} → {last_date}");
    }
    println!(
        "  {} exchange(s) across {} session(s), {} total match(es)",
        exchanges.len(),
        session_count,
        total_matches,
    );

    // First mention context
    if let Some(ref text) = first.assistant_text {
        let snippet = truncate(text, 200);
        let first_line = snippet.lines().next().unwrap_or("");
        println!("\n  First ({}):", first.timestamp.format("%Y-%m-%d %H:%M"));
        println!("    {first_line}");
    }

    // Last mention context (if different from first)
    if exchanges.len() > 1 {
        if let Some(ref text) = last.assistant_text {
            let snippet = truncate(text, 200);
            let first_line = snippet.lines().next().unwrap_or("");
            println!("\n  Latest ({}):", last.timestamp.format("%Y-%m-%d %H:%M"));
            println!("    {first_line}");
        }
    }

    println!();
}
