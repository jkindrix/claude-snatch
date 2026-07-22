//! File evolution command implementation.
//!
//! Explains why a file changed over time by combining file modification
//! history with conversation context and thinking blocks.

use crate::analysis::file_evolution::{
    analyze_file_evolution, analyze_provider_file_evolution, FileEvolutionParams,
    ProviderChangeEvent,
};
use crate::cli::{Cli, FileEvolutionArgs, OutputFormat};
use crate::error::Result;

use super::helpers::{self, short_id, SessionCollectParams};

/// Run the file-evolution command.
pub fn run(cli: &Cli, args: &FileEvolutionArgs) -> Result<()> {
    if !args.provider.is_empty() {
        return run_provider(cli, args);
    }
    let sessions = helpers::collect_sessions(
        cli,
        &SessionCollectParams {
            session: None,
            project: Some(&args.project),
            since: args.since.as_deref(),
            until: args.until.as_deref(),
            recent: None,
            no_subagents: args.no_subagents,
        },
    )?;

    let params = FileEvolutionParams {
        file_pattern: args.file_pattern.clone(),
        limit: args.limit,
        max_text_len: 500,
        include_thinking: args.include_thinking,
        context_window: args.context_window,
    };

    let results = analyze_file_evolution(&sessions, &params, cli.max_file_size);

    if results.is_empty() {
        println!(
            "No modifications found for files matching '{}'",
            args.file_pattern
        );
        return Ok(());
    }

    match cli.effective_output() {
        OutputFormat::Json => {
            let output: Vec<_> = results
                .iter()
                .map(|r| {
                    serde_json::json!({
                        "file_path": r.file_path,
                        "total_changes": r.total_changes,
                        "sessions_involved": r.sessions_involved,
                        "changes": r.changes.iter().map(|c| {
                            serde_json::json!({
                                "timestamp": c.timestamp.to_rfc3339(),
                                "session_id": c.session_id,
                                "message_id": c.message_id,
                                "version": c.version,
                                "user_prompt": c.user_prompt,
                                "assistant_response": c.assistant_response,
                                "thinking": c.thinking,
                                "tools_used": c.tools_used,
                                "had_errors": c.had_errors,
                            })
                        }).collect::<Vec<_>>(),
                    })
                })
                .collect();
            println!("{}", serde_json::to_string_pretty(&output)?);
        }
        _ => {
            for result in &results {
                println!(
                    "File: {} ({} changes across {} sessions)\n",
                    result.file_path, result.total_changes, result.sessions_involved,
                );

                for (i, change) in result.changes.iter().enumerate() {
                    let ts = change.timestamp.format("%Y-%m-%d %H:%M");
                    let err_marker = if change.had_errors { " [ERROR]" } else { "" };
                    println!(
                        "  {}. [{}] {} v{}{}",
                        i + 1,
                        short_id(&change.session_id),
                        ts,
                        change.version,
                        err_marker,
                    );

                    if let Some(ref prompt) = change.user_prompt {
                        let preview: String = prompt
                            .lines()
                            .next()
                            .unwrap_or("")
                            .chars()
                            .take(100)
                            .collect();
                        println!("     User: {}", preview);
                    }

                    if let Some(ref response) = change.assistant_response {
                        let preview: String = response
                            .lines()
                            .next()
                            .unwrap_or("")
                            .chars()
                            .take(100)
                            .collect();
                        println!("     Assistant: {}", preview);
                    }

                    if let Some(ref thinking) = change.thinking {
                        let preview: String = thinking
                            .lines()
                            .next()
                            .unwrap_or("")
                            .chars()
                            .take(100)
                            .collect();
                        println!("     Thinking: {}", preview);
                    }

                    if !change.tools_used.is_empty() {
                        println!("     Tools: {}", change.tools_used.join(", "));
                    }

                    println!();
                }
            }
        }
    }

    Ok(())
}

fn event_json(event: &ProviderChangeEvent) -> serde_json::Value {
    serde_json::json!({
        "timestamp": event.timestamp.map(|timestamp| timestamp.to_rfc3339()),
        "provider": event.provider,
        "qualified_id": event.qualified_id,
        "session_id": event.session_id,
        "project_path": event.project_path,
        "entry_id": event.entry_id,
        "operation_id": event.operation_id,
        "version": event.version,
        "kind": event.kind.as_str(),
        "move_path": event.move_path,
        "evidence": event.evidence.as_str(),
        "outcome": event.outcome.as_str(),
        "coverage": event.coverage,
        "record_ordinal": event.record_ordinal,
        "outcome_record_ordinal": event.outcome_record_ordinal,
        "user_prompt": event.user_prompt,
        "assistant_response": event.assistant_response,
        "thinking": event.thinking,
        "tools_used": event.tools_used,
        "had_errors": event.had_errors,
    })
}

fn run_provider(cli: &Cli, args: &FileEvolutionArgs) -> Result<()> {
    use crate::file_index::parsed_session_time_range;
    use crate::provider::registry::{ParsedProjectSession, ProviderSelection};

    let selection = ProviderSelection::from_flags(&args.provider)
        .map_err(crate::provider::ProviderError::Other)?;
    let registry = helpers::provider_registry(cli);
    let since = args
        .since
        .as_deref()
        .map(super::parse_date_filter)
        .transpose()?
        .map(chrono::DateTime::<chrono::Utc>::from);
    let until = args
        .until
        .as_deref()
        .map(super::parse_date_filter)
        .transpose()?
        .map(chrono::DateTime::<chrono::Utc>::from);
    let mut sessions = Vec::new();
    let collected = registry.visit_matching_file_change_sessions(
        &selection,
        Some(&args.project),
        !args.no_subagents,
        &args.file_pattern,
        |project_path, parsed| {
            let in_range = parsed_session_time_range(&parsed).map_or(
                since.is_none() && until.is_none(),
                |(start, end)| {
                    since.is_none_or(|bound| end >= bound)
                        && until.is_none_or(|bound| start <= bound)
                },
            );
            if in_range {
                sessions.push(ParsedProjectSession {
                    project_path: project_path.to_string(),
                    parsed,
                });
            }
        },
    )?;

    let params = FileEvolutionParams {
        file_pattern: args.file_pattern.clone(),
        limit: args.limit,
        max_text_len: 500,
        include_thinking: args.include_thinking,
        context_window: args.context_window,
    };
    let views: Vec<_> = sessions
        .iter()
        .map(|session| (session.project_path.as_str(), session.parsed.as_ref()))
        .collect();
    let results = analyze_provider_file_evolution(&views, &params);

    match cli.effective_output() {
        OutputFormat::Json => {
            let output = serde_json::json!({
                "project_path": args.project,
                "file_pattern": args.file_pattern,
                "files": results.iter().map(|result| serde_json::json!({
                    "file_path": result.file_path,
                    "total_changes": result.total_changes,
                    "total_attempts": result.total_attempts,
                    "sessions_involved": result.sessions_involved,
                    "changes": result.changes.iter().map(event_json).collect::<Vec<_>>(),
                    "attempts": result.attempts.iter().map(event_json).collect::<Vec<_>>(),
                })).collect::<Vec<_>>(),
                "skipped_providers": collected.skipped.iter().map(|(provider, _)| format!("{provider}: unavailable")).collect::<Vec<_>>(),
                "warnings": collected.warnings,
                "coverage_note": "Structured patch/snapshot evidence only; arbitrary shell writes are not inferred.",
            });
            println!("{}", serde_json::to_string_pretty(&output)?);
        }
        _ if results.is_empty() => println!(
            "No source-backed file-change evidence found for files matching '{}'",
            args.file_pattern
        ),
        _ => {
            for result in &results {
                println!(
                    "File: {} ({} applied changes, {} non-applied attempts across {} sessions)\n",
                    result.file_path,
                    result.total_changes,
                    result.total_attempts,
                    result.sessions_involved,
                );
                for (index, change) in result.changes.iter().enumerate() {
                    let timestamp = change
                        .timestamp
                        .map(|value| value.format("%Y-%m-%d %H:%M").to_string())
                        .unwrap_or_else(|| "time unavailable".into());
                    let moved = change
                        .move_path
                        .as_deref()
                        .map(|path| format!(" -> {path}"))
                        .unwrap_or_default();
                    println!(
                        "  {}. [{}] {} {}{} [{}; {}]",
                        index + 1,
                        short_id(&change.session_id),
                        timestamp,
                        change.kind.as_str(),
                        moved,
                        change.evidence.as_str(),
                        change.coverage,
                    );
                    if let Some(prompt) = &change.user_prompt {
                        println!("     User: {}", prompt.lines().next().unwrap_or(""));
                    }
                    if let Some(response) = &change.assistant_response {
                        println!("     Assistant: {}", response.lines().next().unwrap_or(""));
                    }
                }
                for attempt in &result.attempts {
                    println!(
                        "  attempt [{}] {} [{}; {}]",
                        short_id(&attempt.session_id),
                        attempt.kind.as_str(),
                        attempt.outcome.as_str(),
                        attempt.evidence.as_str(),
                    );
                }
            }
            println!(
                "Evidence is bounded to structured patches/snapshots; shell writes are not inferred."
            );
            for (provider, _) in &collected.skipped {
                println!("warning: {provider} unavailable");
            }
            for warning in &collected.warnings {
                println!("warning: {warning}");
            }
        }
    }
    Ok(())
}
