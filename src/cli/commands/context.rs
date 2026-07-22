//! Context command implementation.
//!
//! Contextual zoom around a specific event in a session. Shows the
//! surrounding conversation context for a message UUID or timestamp.

use crate::analysis::event_context::{
    find_event_context, find_semantic_event_context, EventContextParams, SemanticContextTurn,
};
use crate::cli::{Cli, ContextArgs, OutputFormat};
use crate::error::{Result, SnatchError};
use crate::reconstruction::Conversation;

use super::get_claude_dir;

struct ContextSource {
    provider: String,
    qualified_id: String,
    native_id: String,
    semantic_annotations: bool,
}

/// Run the context command.
pub fn run(cli: &Cli, args: &ContextArgs) -> Result<()> {
    if args.message_id.is_none() && args.timestamp.is_none() {
        return Err(SnatchError::InvalidArgument {
            name: "message_id or timestamp".to_string(),
            reason: "either --message-id or --timestamp is required".to_string(),
        });
    }

    let registry = (!args.provider.is_empty() || args.session_id.contains(':'))
        .then(|| super::helpers::provider_registry(cli));
    let provider_route = !args.provider.is_empty()
        || registry
            .as_ref()
            .is_some_and(|registry| registry.looks_qualified(&args.session_id));
    let (conversation, source) = if provider_route {
        let ContextArgs {
            session_id: _,
            provider: _,
            message_id: _,
            timestamp: _,
            context_window: _,
        } = args;
        let registry =
            registry.expect("provider flags or qualified reference constructed registry");
        let resolution = registry.resolve_with_default_policy(&args.provider, &args.session_id)?;
        let parsed = crate::provider::registry::cached_parsed_session(
            crate::cache::global_cache(),
            resolution.provider,
            &resolution.key,
        )?;
        let source = ContextSource {
            provider: resolution.key.provider.to_string(),
            qualified_id: resolution.key.to_string(),
            native_id: resolution.key.native_id.clone(),
            semantic_annotations: resolution.provider.capabilities().semantic_annotations,
        };
        (Conversation::from_parsed_session(parsed)?, Some(source))
    } else {
        let claude_dir = get_claude_dir(cli.claude_dir.as_ref())?;
        let session = claude_dir.find_session(&args.session_id)?.ok_or_else(|| {
            SnatchError::SessionNotFound {
                session_id: args.session_id.clone(),
            }
        })?;
        let entries = session.parse_with_options(cli.max_file_size)?;
        (Conversation::from_entries(entries)?, None)
    };

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

    let main_entries = conversation.main_thread_entries();
    let result = match if source
        .as_ref()
        .is_some_and(|source| source.semantic_annotations)
    {
        find_semantic_event_context(&conversation, &params)
    } else {
        find_event_context(&main_entries, &params)
    } {
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

            let mut output = serde_json::json!({
                "session_id": source
                    .as_ref()
                    .map_or(args.session_id.as_str(), |source| source.native_id.as_str()),
                "target_index": result.target_index,
                "target": to_json(&result.target),
                "before": result.before.iter().map(to_json).collect::<Vec<_>>(),
                "after": result.after.iter().map(to_json).collect::<Vec<_>>(),
                "related_files": result.related_files,
                "error_count": result.error_count,
            });
            let object = output
                .as_object_mut()
                .expect("event-context JSON root is an object");
            if let Some(source) = &source {
                object.insert("provider".into(), source.provider.clone().into());
                object.insert("qualified_id".into(), source.qualified_id.clone().into());
            }
            if let Some(count) = result.confirmed_failure_count {
                object.insert("confirmed_failure_count".into(), count.into());
            }
            if let Some(count) = result.inferred_failure_count {
                object.insert("inferred_failure_count".into(), count.into());
            }
            if let Some(window) = &result.semantic_window {
                object.insert("semantic_window".into(), serde_json::to_value(window)?);
            }
            println!("{}", serde_json::to_string_pretty(&output)?);
        }
        _ => {
            println!("Context around message [{}]:\n", result.target_index);
            if let Some(window) = &result.semantic_window {
                for turn in &window.before {
                    print_semantic_turn(turn, "  ");
                }
                print_semantic_turn(&window.focus, "◆ ");
                println!("Target event:");
                print_turn(&result.target, "▸ ");
                for turn in &window.after {
                    print_semantic_turn(turn, "  ");
                }
            } else {
                for turn in &result.before {
                    print_turn(turn, "  ");
                }
                print_turn(&result.target, "▸ ");
                for turn in &result.after {
                    print_turn(turn, "  ");
                }
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

fn print_semantic_turn(turn: &SemanticContextTurn, prefix: &str) {
    let native = turn.turn_id.as_deref().unwrap_or("derived");
    println!(
        "{prefix}Turn {} ({native}, entries {}-{}, {} events):",
        turn.index, turn.start_entry_index, turn.end_entry_index, turn.event_count
    );
    if let Some(prompt) = &turn.user_prompt {
        println!("{prefix}    user: {prompt}");
    }
    for steering in &turn.steering_prompts {
        println!("{prefix}    steering: {steering}");
    }
    if let Some(response) = &turn.assistant_response {
        println!("{prefix}    assistant: {response}");
    }
    if !turn.tools.is_empty() {
        println!("{prefix}    tools: {}", turn.tools.join(", "));
    }
    if turn.confirmed_failure_count > 0 || turn.inferred_failure_count > 0 {
        println!(
            "{prefix}    failures: {} confirmed, {} inferred",
            turn.confirmed_failure_count, turn.inferred_failure_count
        );
    }
    println!();
}

fn print_turn(turn: &crate::analysis::event_context::ContextTurn, prefix: &str) {
    let ts = turn
        .timestamp
        .map(|t| t.format("%H:%M:%S").to_string())
        .unwrap_or_default();
    let error_mark = if turn.had_errors { " ✗" } else { "" };

    println!(
        "{prefix}[{}] {ts} {}{}:",
        turn.index, turn.message_type, error_mark
    );
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
