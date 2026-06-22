//! Thread command implementation.
//!
//! Cross-session topic threading: searches all sessions for a pattern,
//! then presents matches with surrounding conversation context, ordered
//! chronologically across sessions.

use std::io::IsTerminal;

use indicatif::{ProgressBar, ProgressStyle};
use regex::RegexBuilder;

use crate::analysis::threading::{thread_topic, ThreadParams, ThreadedExchange};
use crate::cli::{Cli, ThreadArgs};
use crate::error::{Result, SnatchError};

use super::helpers::{self, truncate, SessionCollectParams};

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

    let sessions = helpers::collect_sessions(
        cli,
        &SessionCollectParams {
            session: args.session.as_deref(),
            project: args.project.as_deref(),
            since: args.since.as_deref(),
            until: args.until.as_deref(),
            recent: args.recent,
            no_subagents: args.no_subagents,
        },
    )?;

    let session_count = sessions.len();
    let show_progress = session_count > 10 && std::io::stderr().is_terminal() && !cli.quiet;
    if show_progress {
        let pb = ProgressBar::new(session_count as u64);
        pb.set_style(
            ProgressStyle::default_bar()
                .template("{spinner:.cyan} [{bar:40.cyan/dim}] {pos}/{len} sessions")
                .unwrap()
                .progress_chars("█▓░"),
        );
        // Progress display for user feedback — threading itself handles iteration
        pb.finish_and_clear();
    }

    let limit = if args.no_limit {
        usize::MAX
    } else {
        args.limit
    };

    let params = ThreadParams {
        include_thinking: args.thinking,
        limit,
        max_user_context: args.max_user_context.unwrap_or(args.max_context),
        max_assistant_context: args.max_assistant_context.unwrap_or(args.max_context),
        max_thinking_context: args.max_context,
        role_filter: args.role.clone(),
        decisions_only: args.decisions_only,
    };

    let result = thread_topic(&sessions, &regex, &params, cli.max_file_size);

    if result.exchanges.is_empty() {
        if !cli.quiet {
            println!("No matches found for pattern: {}", args.pattern);
        }
        return Ok(());
    }

    match cli.effective_output() {
        crate::cli::OutputFormat::Json => output_json(&result.exchanges),
        _ => {
            output_text(
                cli,
                &result.exchanges,
                &args.pattern,
                result.session_count,
                params.max_user_context,
                params.max_assistant_context,
                params.max_thinking_context,
            );
            if args.summary && !cli.quiet {
                output_summary(&result.exchanges, result.session_count);
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

    println!(
        "{}",
        serde_json::to_string_pretty(&entries).unwrap_or_default()
    );
}

fn output_text(
    cli: &Cli,
    exchanges: &[ThreadedExchange],
    pattern: &str,
    session_count: usize,
    max_user_context: usize,
    max_assistant_context: usize,
    max_thinking_context: usize,
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
            println!(
                "--- {} [{}] ---",
                date.to_string().split(' ').next().unwrap_or(""),
                exchange.short_id
            );
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
            for line in truncate(text, max_thinking_context).lines() {
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

    if let Some(ref text) = first.assistant_text {
        let snippet = truncate(text, 200);
        let first_line = snippet.lines().next().unwrap_or("");
        println!("\n  First ({}):", first.timestamp.format("%Y-%m-%d %H:%M"));
        println!("    {first_line}");
    }

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
