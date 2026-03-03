//! Timeline command implementation.
//!
//! Shows a turn-by-turn narrative of a session, with tool-only turns
//! collapsed for readability. Mirrors the MCP `get_session_timeline` tool.

use crate::analysis::timeline::{build_timeline, TimelineOptions};
use crate::cli::{Cli, OutputFormat, TimelineArgs};
use crate::error::{Result, SnatchError};
use crate::reconstruction::Conversation;

use super::get_claude_dir;

/// JSON output types for serialization.
#[derive(serde::Serialize)]
struct TimelineOutput {
    session_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    start_time: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    end_time: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    duration: Option<String>,
    total_turns: usize,
    timeline: Vec<TimelineTurnOutput>,
}

#[derive(serde::Serialize)]
struct TimelineTurnOutput {
    index: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    timestamp: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    user_prompt: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    assistant_summary: Option<String>,
    tools_used: Vec<String>,
    files_touched: Vec<String>,
    had_errors: bool,
}

/// Run the timeline command.
pub fn run(cli: &Cli, args: &TimelineArgs) -> Result<()> {
    let claude_dir = get_claude_dir(cli.claude_dir.as_ref())?;

    let session = claude_dir
        .find_session(&args.session_id)?
        .ok_or_else(|| SnatchError::SessionNotFound {
            session_id: args.session_id.clone(),
        })?;

    let entries = session.parse_with_options(cli.max_file_size)?;
    let conversation = Conversation::from_entries(entries)?;
    let turns = conversation.turns();

    let analytics = crate::analytics::SessionAnalytics::from_conversation(&conversation);
    let start_time = analytics.start_time.map(|t| t.to_rfc3339());
    let end_time = analytics.end_time.map(|t| t.to_rfc3339());
    let duration = analytics.duration_string();

    let total_turns = turns.len();

    let opts = TimelineOptions {
        limit: args.limit,
        ..Default::default()
    };

    let timeline = build_timeline(&turns, &opts);

    match cli.effective_output() {
        OutputFormat::Json => {
            let output = TimelineOutput {
                session_id: args.session_id.clone(),
                start_time,
                end_time,
                duration,
                total_turns,
                timeline: timeline
                    .into_iter()
                    .map(|t| TimelineTurnOutput {
                        index: t.index,
                        timestamp: t.timestamp,
                        user_prompt: t.user_prompt,
                        assistant_summary: t.assistant_summary,
                        tools_used: t.tools_used,
                        files_touched: t.files_touched,
                        had_errors: t.had_errors,
                    })
                    .collect(),
            };
            println!("{}", serde_json::to_string_pretty(&output)?);
        }
        _ => {
            // Text output
            if timeline.is_empty() {
                println!("No turns found in session {}.", args.session_id);
                return Ok(());
            }

            println!(
                "Timeline for session {} ({} turns)\n",
                args.session_id, total_turns,
            );

            for turn in &timeline {
                let marker = if turn.had_errors { "!" } else { " " };

                if let Some(ref prompt) = turn.user_prompt {
                    println!("{marker} [{:>3}] User: {prompt}", turn.index);
                }

                if let Some(ref summary) = turn.assistant_summary {
                    println!("{marker} [{:>3}] Assistant: {summary}", turn.index);
                }

                if !turn.tools_used.is_empty() {
                    println!(
                        "        Tools: {}",
                        turn.tools_used.join(", ")
                    );
                }

                if !turn.files_touched.is_empty() {
                    println!(
                        "        Files: {}",
                        turn.files_touched.join(", ")
                    );
                }

                println!();
            }
        }
    }

    Ok(())
}
