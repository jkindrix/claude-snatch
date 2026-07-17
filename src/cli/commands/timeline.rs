//! Timeline command implementation.
//!
//! Shows a turn-by-turn narrative of a session, with tool-only turns
//! collapsed for readability. Mirrors the MCP `get_session_timeline` tool.

use crate::analysis::subagents::{match_subagents, SubagentMatches};
use crate::analysis::timeline::{build_timeline, TimelineOptions};
use crate::cli::{Cli, OutputFormat, TimelineArgs};
use crate::error::{Result, SnatchError};
use crate::reconstruction::Conversation;

use super::get_claude_dir;

/// JSON output types for serialization.
#[derive(serde::Serialize)]
struct TimelineOutput {
    session_id: String,
    /// Root file id of the resume chain, when chain members were merged.
    #[serde(skip_serializing_if = "Option::is_none")]
    chain_id: Option<String>,
    /// All member file ids (chain order), when chain members were merged.
    #[serde(skip_serializing_if = "Option::is_none")]
    chain_members: Option<Vec<String>>,
    /// Number of files merged, when this session is part of a chain.
    #[serde(skip_serializing_if = "Option::is_none")]
    chain_member_count: Option<usize>,
    /// Lines the lenient parser skipped (mirrors the text-mode warning).
    unparsed_count: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    start_time: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    end_time: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    span: Option<String>,
    total_turns: usize,
    timeline: Vec<TimelineTurnOutput>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    subagents: Vec<SubagentOutput>,
}

#[derive(serde::Serialize)]
struct SubagentOutput {
    session_id: String,
    /// "linked" when joined to a spawn call, "unlinked" otherwise.
    link: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    message_count: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    result_preview: Option<String>,
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
    if !args.provider.is_empty()
        || (args.session_id.contains(':')
            && super::helpers::provider_registry(cli).looks_qualified(&args.session_id))
    {
        return run_provider(cli, args);
    }

    let claude_dir = get_claude_dir(cli.claude_dir.as_ref())?;

    let session =
        claude_dir
            .find_session(&args.session_id)?
            .ok_or_else(|| SnatchError::SessionNotFound {
                session_id: args.session_id.clone(),
            })?;

    let chain_aware = !args.no_chain;
    let (entries, unparsed, chain) = super::helpers::resolve_chain_entries(
        &claude_dir,
        &session,
        chain_aware,
        cli.max_file_size,
    )?;
    if let Some(ref chain) = chain {
        if !cli.quiet {
            eprintln!(
                "ℹ Showing full resume chain: {} files (root {}). Use --no-chain to restrict.",
                chain.members.len(),
                chain.root_id
            );
        }
    }
    let conversation = Conversation::from_entries(entries)?;
    let ctx = TimelineContext {
        display_id: args.session_id.clone(),
        chain,
        unparsed,
        session: Some(&session),
    };
    render(cli, args, &conversation, &ctx)
}

/// Provider-routed timeline: resolves through the registry and builds the
/// conversation from the COMPLETE ParsedSession bundle (round-21
/// constraint 5); the normalized entries drive the same turn/timeline
/// machinery as Claude sessions.
fn run_provider(cli: &Cli, args: &TimelineArgs) -> Result<()> {
    // COMPLETE argument classification (destructured without `..`):
    // --no-chain is Claude resume-chain machinery, refused.
    let TimelineArgs {
        session_id: _,
        provider: _,
        limit: _,
        no_chain,
    } = args;
    super::helpers::refuse_unsupported_flags(
        "provider-routed timeline",
        &[("--no-chain", *no_chain)],
    )?;

    let registry = super::helpers::provider_registry(cli);
    let resolution = registry.resolve_with_default_policy(&args.provider, &args.session_id)?;
    let parsed = crate::provider::registry::cached_parsed_session(
        crate::cache::global_cache(),
        resolution.provider,
        &resolution.key,
    )?;
    let unparsed = parsed.diagnostics.unparseable;
    let conversation = Conversation::from_parsed_session(parsed)?;
    let ctx = TimelineContext {
        display_id: resolution.key.to_string(),
        chain: None,
        unparsed,
        session: None,
    };
    render(cli, args, &conversation, &ctx)
}

/// Shared acquisition-independent rendering context.
struct TimelineContext<'a> {
    display_id: String,
    chain: Option<super::helpers::ChainMeta>,
    unparsed: usize,
    session: Option<&'a crate::discovery::Session>,
}

fn render(
    cli: &Cli,
    args: &TimelineArgs,
    conversation: &Conversation,
    ctx: &TimelineContext<'_>,
) -> Result<()> {
    if !cli.quiet {
        if let Some(notice) = conversation.duplicate_notice() {
            eprintln!("{notice}");
        }
    }
    let turns = conversation.turns();

    let analytics = crate::analytics::SessionAnalytics::from_conversation(conversation);
    let start_time = analytics.start_time.map(|t| t.to_rfc3339());
    let end_time = analytics.end_time.map(|t| t.to_rfc3339());
    let span = analytics.duration_string();

    let total_turns = turns.len();

    let opts = TimelineOptions {
        limit: args.limit,
        ..Default::default()
    };

    let timeline = build_timeline(&turns, &opts);

    // Subagent markers: surface work spawned by Agent/Task calls (matching the
    // messages surface). Linked subagents carry the spawn description; present
    // but unjoinable ones are still marked so the work never vanishes silently.
    let matches = match ctx.session {
        Some(session) => match_subagents(
            session,
            &conversation.main_thread_entries(),
            cli.max_file_size,
        ),
        None => SubagentMatches::default(),
    };
    let mut subagents: Vec<SubagentOutput> = matches
        .matched
        .values()
        .map(|m| SubagentOutput {
            session_id: m.session_id.clone(),
            link: "linked",
            description: m.description.clone(),
            message_count: m.message_count,
            result_preview: m.result_preview.clone(),
        })
        .chain(matches.unmatched.iter().map(|m| SubagentOutput {
            session_id: m.session_id.clone(),
            link: "unlinked",
            description: m.description.clone(),
            message_count: m.message_count,
            result_preview: m.result_preview.clone(),
        }))
        .collect();
    subagents.sort_by(|a, b| a.session_id.cmp(&b.session_id));

    match cli.effective_output() {
        OutputFormat::Json => {
            let output = TimelineOutput {
                session_id: ctx.display_id.clone(),
                chain_id: ctx.chain.as_ref().map(|c| c.root_id.clone()),
                chain_members: ctx.chain.as_ref().map(|c| c.members.clone()),
                chain_member_count: ctx.chain.as_ref().map(|c| c.members.len()),
                unparsed_count: ctx.unparsed,
                start_time,
                end_time,
                span,
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
                subagents,
            };
            println!("{}", serde_json::to_string_pretty(&output)?);
        }
        _ => {
            // Text output
            if timeline.is_empty() {
                println!("No turns found in session {}.", ctx.display_id);
                return Ok(());
            }

            println!(
                "Timeline for session {} ({} turns)\n",
                ctx.display_id, total_turns,
            );
            if ctx.unparsed > 0 {
                println!(
                    "⚠ {} line{} could not be parsed (dropped from this view)\n",
                    ctx.unparsed,
                    if ctx.unparsed == 1 { "" } else { "s" }
                );
            }

            for turn in &timeline {
                let marker = if turn.had_errors { "!" } else { " " };

                if let Some(ref prompt) = turn.user_prompt {
                    println!("{marker} [{:>3}] User: {prompt}", turn.index);
                }

                if let Some(ref summary) = turn.assistant_summary {
                    println!("{marker} [{:>3}] Assistant: {summary}", turn.index);
                }

                if !turn.tools_used.is_empty() {
                    println!("        Tools: {}", turn.tools_used.join(", "));
                }

                if !turn.files_touched.is_empty() {
                    println!("        Files: {}", turn.files_touched.join(", "));
                }

                println!();
            }

            if !subagents.is_empty() {
                println!("Subagents:");
                for s in &subagents {
                    let msgs = s
                        .message_count
                        .map(|n| format!("{n} msgs"))
                        .unwrap_or_else(|| "? msgs".to_string());
                    let desc = s
                        .description
                        .as_deref()
                        .map(|d| format!(" {d}"))
                        .unwrap_or_default();
                    println!(
                        "  [subagent {}: {}, {}]{}",
                        s.session_id, msgs, s.link, desc
                    );
                    if let Some(preview) = &s.result_preview {
                        println!("      result: {}", crate::util::truncate_text(preview, 200));
                    }
                }
                println!();
            }
        }
    }

    Ok(())
}
