//! Context command implementation.
//!
//! Contextual zoom around a specific event in a session. Shows the
//! surrounding conversation context for a message UUID or timestamp.

use crate::analysis::event_context::{find_event_context, EventContextParams};
use crate::cli::{Cli, ContextArgs, OutputFormat};
use crate::error::{Result, SnatchError};
use crate::reconstruction::Conversation;

use super::get_claude_dir;

/// Run the context command.
pub fn run(cli: &Cli, args: &ContextArgs) -> Result<()> {
    if args.message_id.is_none() && args.timestamp.is_none() {
        return Err(SnatchError::InvalidArgument {
            name: "message_id or timestamp".to_string(),
            reason: "either --message-id or --timestamp is required".to_string(),
        });
    }

    let claude_dir = get_claude_dir(cli.claude_dir.as_ref())?;
    let session = claude_dir
        .find_session(&args.session_id)?
        .ok_or_else(|| SnatchError::SessionNotFound {
            session_id: args.session_id.clone(),
        })?;

    let entries = session.parse_with_options(cli.max_file_size)?;
    let conversation = Conversation::from_entries(entries)?;
    let main_entries = conversation.main_thread_entries();
    let entry_refs: Vec<&_> = main_entries.iter().copied().collect();

    let timestamp = if let Some(ref ts) = args.timestamp {
        use crate::cli::commands::parse_date_filter;
        use chrono::{DateTime, Utc};
        // Try as ISO 8601 first
        if let Ok(dt) = DateTime::parse_from_rfc3339(ts) {
            Some(dt.with_timezone(&Utc))
        } else {
            // Try relative duration via parse_date_filter (returns SystemTime)
            let systime = parse_date_filter(ts)?;
            Some(DateTime::<Utc>::from(systime))
        }
    } else {
        None
    };

    let params = EventContextParams {
        message_id: args.message_id.clone(),
        timestamp,
        context_window: args.context_window,
        max_text_len: 500,
    };

    let result = match find_event_context(&entry_refs, &params) {
        Some(r) => r,
        None => {
            println!("Event not found in session.");
            return Ok(());
        }
    };

    match cli.effective_output() {
        OutputFormat::Json => {
            let to_json = |t: &crate::analysis::event_context::ContextTurn| {
                serde_json::json!({
                    "index": t.index,
                    "type": t.message_type,
                    "uuid": t.uuid,
                    "timestamp": t.timestamp.map(|ts| ts.to_rfc3339()),
                    "text": t.text,
                    "tools": t.tools,
                    "had_errors": t.had_errors,
                })
            };

            let output = serde_json::json!({
                "session_id": args.session_id,
                "target_index": result.target_index,
                "target": to_json(&result.target),
                "before": result.before.iter().map(to_json).collect::<Vec<_>>(),
                "after": result.after.iter().map(to_json).collect::<Vec<_>>(),
                "related_files": result.related_files,
                "error_count": result.error_count,
            });
            println!("{}", serde_json::to_string_pretty(&output)?);
        }
        _ => {
            println!("Context around message [{}]:\n", result.target_index);

            for turn in &result.before {
                print_turn(turn, "  ");
            }

            // Target highlighted
            print_turn(&result.target, "▸ ");

            for turn in &result.after {
                print_turn(turn, "  ");
            }

            if !result.related_files.is_empty() {
                println!("\nRelated files:");
                for f in &result.related_files {
                    println!("  {f}");
                }
            }

            if result.error_count > 0 {
                println!("\nErrors in window: {}", result.error_count);
            }
        }
    }

    Ok(())
}

fn print_turn(turn: &crate::analysis::event_context::ContextTurn, prefix: &str) {
    let ts = turn.timestamp
        .map(|t| t.format("%H:%M:%S").to_string())
        .unwrap_or_default();
    let error_mark = if turn.had_errors { " ✗" } else { "" };

    println!("{prefix}[{}] {ts} {}{}:", turn.index, turn.message_type, error_mark);
    if let Some(ref text) = turn.text {
        for line in text.lines().take(5) {
            println!("{prefix}    {line}");
        }
    }
    if !turn.tools.is_empty() {
        println!("{prefix}    tools: {}", turn.tools.join(", "));
    }
    println!();
}
