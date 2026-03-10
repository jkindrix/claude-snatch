//! File evolution command implementation.
//!
//! Explains why a file changed over time by combining file modification
//! history with conversation context and thinking blocks.

use crate::analysis::file_evolution::{analyze_file_evolution, FileEvolutionParams};
use crate::cli::{Cli, FileEvolutionArgs, OutputFormat};
use crate::error::Result;

use super::helpers::{self, short_id, SessionCollectParams};

/// Run the file-evolution command.
pub fn run(cli: &Cli, args: &FileEvolutionArgs) -> Result<()> {
    let sessions = helpers::collect_sessions(cli, &SessionCollectParams {
        session: None,
        project: Some(&args.project),
        since: args.since.as_deref(),
        until: args.until.as_deref(),
        recent: None,
        no_subagents: args.no_subagents,
    })?;

    let params = FileEvolutionParams {
        file_pattern: args.file_pattern.clone(),
        limit: args.limit,
        max_text_len: 500,
        include_thinking: args.include_thinking,
        context_window: args.context_window,
    };

    let results = analyze_file_evolution(&sessions, &params, cli.max_file_size);

    if results.is_empty() {
        println!("No modifications found for files matching '{}'", args.file_pattern);
        return Ok(());
    }

    match cli.effective_output() {
        OutputFormat::Json => {
            let output: Vec<_> = results.iter().map(|r| {
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
            }).collect();
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
                        "  {}. [{}] {} v{}{}", i + 1, short_id(&change.session_id), ts, change.version, err_marker,
                    );

                    if let Some(ref prompt) = change.user_prompt {
                        let preview: String = prompt.lines().next().unwrap_or("").chars().take(100).collect();
                        println!("     User: {}", preview);
                    }

                    if let Some(ref response) = change.assistant_response {
                        let preview: String = response.lines().next().unwrap_or("").chars().take(100).collect();
                        println!("     Assistant: {}", preview);
                    }

                    if let Some(ref thinking) = change.thinking {
                        let preview: String = thinking.lines().next().unwrap_or("").chars().take(100).collect();
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
