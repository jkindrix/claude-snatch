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
        semantic: false,
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
    let semantic = resolution.provider.capabilities().semantic_annotations;
    let conversation = Conversation::from_parsed_session(parsed)?;
    let ctx = TimelineContext {
        display_id: resolution.key.to_string(),
        semantic,
        chain: None,
        unparsed,
        session: None,
    };
    render(cli, args, &conversation, &ctx)
}

/// Shared acquisition-independent rendering context.
struct TimelineContext<'a> {
    display_id: String,
    /// Semantic rendering is keyed on the provider's DECLARED
    /// semantic_annotations capability — never on the mere presence of a
    /// bundle (a coverage-less adapter would lose prompts and collapse
    /// timelines; round-23 blocker 1).
    semantic: bool,
    chain: Option<super::helpers::ChainMeta>,
    unparsed: usize,
    session: Option<&'a crate::discovery::Session>,
}

/// Group a provider conversation into turns using the semantics sidecar:
/// a new turn starts when the entry's `turn_id` changes to a different
/// known id, or at a HUMAN-authored prompt. Harness context preceding the
/// first human prompt forms no turn of its own unless it produced
/// assistant activity.
fn semantic_turns<'a>(
    conversation: &'a Conversation,
) -> Vec<crate::reconstruction::ConversationTurn<'a>> {
    use crate::model::LogEntry;
    use crate::provider::PromptAuthorship;

    let mut turns: Vec<crate::reconstruction::ConversationTurn<'a>> = Vec::new();
    let mut current: Option<crate::reconstruction::ConversationTurn<'a>> = None;
    let mut current_turn_id: Option<String> = None;

    let flush = |turn: Option<crate::reconstruction::ConversationTurn<'a>>,
                 turns: &mut Vec<crate::reconstruction::ConversationTurn<'a>>| {
        if let Some(t) = turn {
            // Keep only turns with substance: a human prompt or assistant
            // activity. Pure harness preambles are visible via messages,
            // not as timeline turns.
            if t.user_message.is_some() || t.assistant_message.is_some() || !t.tool_uses.is_empty()
            {
                turns.push(t);
            }
        }
    };

    for entry in conversation.main_thread_entries() {
        let sem = entry
            .uuid()
            .and_then(|u| conversation.semantics_for_uuid(u));
        let entry_turn = sem.and_then(|s| s.turn_id.clone());
        let prompt = sem.and_then(|s| s.prompt);
        let is_human = matches!(entry, LogEntry::User(_))
            && prompt.is_some_and(|p| matches!(p.authorship, PromptAuthorship::Human));
        // Only a TURN-BOUNDARY human prompt opens a turn; a MidTurn human
        // prompt (steering) stays inside the current one (round-24: the
        // normalizer's PromptDelivery axis must be honored here, not just
        // authorship).
        let is_human_boundary = is_human
            && prompt.is_some_and(|p| {
                matches!(p.delivery, crate::provider::PromptDelivery::TurnBoundary)
            });

        let turn_changed = match (&entry_turn, &current_turn_id) {
            (Some(new), Some(old)) => new != old,
            (Some(_), None) => current.is_some(),
            _ => false,
        };
        if is_human_boundary || turn_changed {
            flush(current.take(), &mut turns);
        }
        if entry_turn.is_some() {
            current_turn_id = entry_turn;
        }

        let turn = current.get_or_insert_with(|| crate::reconstruction::ConversationTurn {
            user_message: None,
            assistant_message: None,
            tool_uses: Vec::new(),
            tool_results: Vec::new(),
        });
        match entry {
            LogEntry::User(user) => {
                if is_human && turn.user_message.is_none() {
                    turn.user_message = Some(entry);
                }
                turn.tool_results.extend(user.message.tool_results());
            }
            LogEntry::Assistant(assistant) => {
                turn.tool_uses.extend(assistant.message.tool_uses());
                // The latest TEXT-bearing emission is the turn's answer
                // (reasoning/tool-only entries never clobber it).
                let has_text = assistant.message.content.iter().any(|b| {
                    matches!(b, crate::model::ContentBlock::Text(t) if !t.text.trim().is_empty())
                });
                if has_text {
                    turn.assistant_message = Some(entry);
                }
            }
            _ => {}
        }
    }
    flush(current, &mut turns);
    turns
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
    // Provider sessions group turns SEMANTICALLY: by the turn_id carrier
    // and Human prompt boundaries from the retained bundle — the classic
    // adjacent user/assistant pairing would count every harness-injected
    // context message as a turn (round-22 blocker 3: a real one-task
    // session reported 77 turns). Claude sessions keep the classic pairing.
    let turns = if ctx.semantic {
        semantic_turns(conversation)
    } else {
        conversation.turns()
    };

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
