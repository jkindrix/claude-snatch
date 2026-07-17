//! Messages command implementation.
//!
//! Reads conversation messages from a session at different detail levels,
//! mirroring the MCP `get_session_messages` tool for CLI use.

use std::collections::HashMap;

use crate::analysis::chunking::{chunk_conversation, entries_for_chunk_range, parse_chunk_spec};
use crate::analysis::extraction::{
    boundary_prompt_text, failed_tool_use_ids, has_tool_errors, is_prompt_boundary,
    queued_human_prompt, render_attachment_content,
};
use crate::analysis::extraction::{
    extract_assistant_summary, extract_error_preview, extract_result_preview,
    extract_thinking_text, extract_tool_input_summary, extract_tool_names,
    extract_user_prompt_text, get_model, has_thinking, is_human_prompt, truncate_text,
};
use crate::analysis::subagents::{match_subagents, SubagentMatch, SubagentMatches};
use crate::cli::{Cli, MessagesArgs, OutputFormat};
use crate::error::{Result, SnatchError};
use crate::model::message::LogEntry;
use crate::reconstruction::Conversation;

use super::get_claude_dir;

/// JSON output types.
#[derive(serde::Serialize)]
struct MessagesOutput {
    session_id: String,
    project_path: String,
    /// Root file id of the resume chain, when chain members were merged.
    #[serde(skip_serializing_if = "Option::is_none")]
    chain_id: Option<String>,
    /// All member file ids (chain order), when chain members were merged.
    #[serde(skip_serializing_if = "Option::is_none")]
    chain_members: Option<Vec<String>>,
    /// Number of files merged, when this session is part of a chain.
    #[serde(skip_serializing_if = "Option::is_none")]
    chain_member_count: Option<usize>,
    total_messages: usize,
    returned: usize,
    offset: usize,
    messages: Vec<MessageOutput>,
    /// Subagents present on disk but not joinable to a specific spawn call.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    unmatched_subagents: Vec<UnmatchedSubagentOutput>,
}

#[derive(serde::Serialize)]
struct UnmatchedSubagentOutput {
    session_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    message_count: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    result_preview: Option<String>,
}

#[derive(serde::Serialize)]
struct MessageOutput {
    index: usize,
    msg_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    timestamp: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    git_branch: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_calls: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_details: Option<Vec<ToolDetailOutput>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    has_thinking: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    thinking_preview: Option<String>,
}

#[derive(serde::Serialize)]
struct ToolDetailOutput {
    tool_name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    file_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    command: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pattern: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    subagent_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    prompt: Option<String>,
    /// For Agent/Task calls: the spawned subagent's session id, when matched.
    #[serde(skip_serializing_if = "Option::is_none")]
    subagent_session_id: Option<String>,
    /// Preview of the subagent's final assistant message (its result), truncated.
    #[serde(skip_serializing_if = "Option::is_none")]
    subagent_result_preview: Option<String>,
    /// Full subagent transcript, present only with --subagent-transcripts.
    #[serde(skip_serializing_if = "Option::is_none")]
    subagent_transcript: Option<Vec<MessageOutput>>,
    /// Whether the matched tool result was an error (absent when success).
    #[serde(skip_serializing_if = "Option::is_none")]
    had_error: Option<bool>,
    /// Truncated preview of a failed tool result's error text.
    #[serde(skip_serializing_if = "Option::is_none")]
    error_preview: Option<String>,
    /// Truncated preview of a successful tool result's output (absent on
    /// error; for Agent/Task calls subagent_result_preview is used instead).
    #[serde(skip_serializing_if = "Option::is_none")]
    result_preview: Option<String>,
}

/// Whether thinking blocks should be rendered for a given detail level.
///
/// `--detail full` implies thinking ("full means full"), so it is always on
/// at that level. Other levels stay gated by the `--include-thinking` flag.
fn effective_include_thinking(flag: bool, detail: &str) -> bool {
    flag || detail == "full"
}

/// Whether an entry produces any output in text mode at the given detail
/// level. Mirrors the skip rules of the render arms in `run` below — keep the
/// two in sync, or the "showing X-Y" header lies about the rows that follow.
fn renders_at_detail(
    entry: &LogEntry,
    detail: &str,
    include_thinking: bool,
    thinking_max_len: usize,
    truncate_len: usize,
) -> bool {
    let thinking_renders =
        || include_thinking && extract_thinking_text(entry, thinking_max_len).is_some();
    match detail {
        "overview" => boundary_prompt_text(entry).is_some(),
        "conversation" => {
            let has_content = match entry {
                LogEntry::User(_) => extract_user_prompt_text(entry).is_some(),
                LogEntry::Assistant(_) => extract_assistant_summary(entry, truncate_len).is_some(),
                LogEntry::Attachment(_) => queued_human_prompt(entry).is_some(),
                _ => false,
            };
            has_content || thinking_renders()
        }
        _ => {
            // "standard" / "full"
            let has_content = match entry {
                LogEntry::User(_) => extract_user_prompt_text(entry).is_some(),
                LogEntry::Assistant(_) => extract_assistant_summary(entry, truncate_len).is_some(),
                LogEntry::System(sys) => sys.content.is_some(),
                LogEntry::Attachment(_) => render_attachment_content(entry, truncate_len).is_some(),
                _ => false,
            };
            let has_tools = if detail == "standard" {
                !extract_tool_names(entry).is_empty()
            } else if let LogEntry::Assistant(a) = entry {
                !a.message.tool_uses().is_empty()
            } else {
                false
            };
            has_content || has_tools || thinking_renders()
        }
    }
}

/// Run the messages command.
pub fn run(cli: &Cli, args: &MessagesArgs) -> Result<()> {
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

    let project_path = session.project_path().to_string();
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
    if !cli.quiet {
        if let Some(notice) = conversation.duplicate_notice() {
            eprintln!("{notice}");
        }
    }

    let ctx = RenderContext {
        display_id: args.session_id.clone(),
        project_path,
        chain,
        unparsed,
        session: Some(&session),
    };
    render(cli, args, &conversation, &ctx)
}

/// Provider-routed messages: the session resolves through the registry and
/// the conversation is built from the COMPLETE ParsedSession bundle
/// (round-21 constraint 5), then rendered by the same pipeline as Claude
/// sessions — normalized entries are model-shaped.
fn run_provider(cli: &Cli, args: &MessagesArgs) -> Result<()> {
    // COMPLETE argument classification (destructured without `..`):
    // generic view options apply to any provider; Claude-specific machinery
    // (resume chains, subagent transcripts, prompt-boundary chunks — Phase
    // C owns codex chunking) is refused, never ignored.
    let MessagesArgs {
        session_id: _,
        provider: _,
        detail: _,
        message_type: _,
        limit: _,
        offset: _,
        reverse: _,
        include_thinking: _,
        after: _,
        before: _,
        subagent_transcripts,
        no_chain,
        chunk,
        errors_only: _,
        max_text_len: _,
    } = args;
    super::helpers::refuse_unsupported_flags(
        "provider-routed messages",
        &[
            ("--subagent-transcripts", *subagent_transcripts),
            ("--no-chain", *no_chain),
            ("--chunk", chunk.is_some()),
        ],
    )?;

    let registry = super::helpers::provider_registry(cli);
    let resolution = registry.resolve_with_default_policy(&args.provider, &args.session_id)?;
    let parsed = crate::provider::registry::cached_parsed_session(
        crate::cache::global_cache(),
        resolution.provider,
        &resolution.key,
    )?;
    let unparsed = parsed.diagnostics.unparseable;
    let project_path = parsed
        .entries
        .iter()
        .find_map(|e| match &e.entry {
            LogEntry::User(m) => m.cwd.clone(),
            LogEntry::Assistant(m) => m.cwd.clone(),
            _ => None,
        })
        .unwrap_or_else(|| "unknown".to_string());
    let conversation = Conversation::from_parsed_session(parsed)?;
    let ctx = RenderContext {
        display_id: resolution.key.to_string(),
        project_path,
        chain: None,
        unparsed,
        session: None,
    };
    render(cli, args, &conversation, &ctx)
}

/// Shared acquisition-independent rendering context.
struct RenderContext<'a> {
    display_id: String,
    project_path: String,
    chain: Option<super::helpers::ChainMeta>,
    unparsed: usize,
    session: Option<&'a crate::discovery::Session>,
}

fn render(
    cli: &Cli,
    args: &MessagesArgs,
    conversation: &Conversation,
    ctx: &RenderContext<'_>,
) -> Result<()> {
    // Human-prompt predicate: provider sessions carry adapter-asserted
    // PromptSemantics in the retained bundle — the Claude-shaped
    // is_human_prompt heuristic would label harness-injected context
    // (permissions, environment, developer instructions) as human
    // (round-22 blocker 3). Claude sessions keep the classic heuristic.
    let semantic = conversation.provider_bundle().is_some();
    let human = |e: &LogEntry| -> bool {
        if semantic {
            matches!(e, LogEntry::User(_))
                && e.uuid()
                    .and_then(|u| conversation.semantics_for_uuid(u))
                    .and_then(|s| s.prompt)
                    .is_some_and(|p| {
                        matches!(p.authorship, crate::provider::PromptAuthorship::Human)
                    })
        } else {
            is_human_prompt(e)
        }
    };
    let detail = args.detail.as_str();
    let msg_type_filter = args.message_type.as_str();
    // 0 means unlimited, matching `list -n 0` (a literal take(0) returned
    // nothing — the worst possible reading of "no limit"). Chunk requests
    // default to unlimited: a chunk is the retrieval unit, and silently
    // cutting it at 50 betrays that; an explicit --limit still paginates.
    let default_limit = if args.chunk.is_some() { 0 } else { 50 };
    let limit = match args.limit.unwrap_or(default_limit) {
        0 => usize::MAX,
        n => n,
    };
    let offset = args.offset;
    // "full" means full: it implies thinking regardless of the --include-thinking
    // flag. Other detail levels stay gated by the flag.
    let include_thinking = effective_include_thinking(args.include_thinking, detail);

    let thinking_max_len = match detail {
        "overview" => 500,
        "conversation" | "standard" => 1000,
        _ => 2000,
    };

    let truncate_len = args.max_text_len.unwrap_or(match detail {
        "overview" => 200,
        "conversation" | "standard" => 500,
        _ => 1000,
    });

    // Match Agent/Task calls to spawned subagents (only "full" detail renders tool
    // details). Uses the unfiltered thread for spawn-order joining.
    //
    // Known limitation: subagent discovery uses only the resolved file's
    // `subagents/` directory. For a merged chain, subagents spawned by *other*
    // chain members may live under those members' directories and are not
    // scanned here yet, so their Agent/Task calls may remain unlinked (the
    // subagent may not be discovered at all, not merely reported as unmatched).
    let subagent_matches: SubagentMatches = match (detail, ctx.session) {
        ("full", Some(session)) => match_subagents(
            session,
            &conversation.main_thread_entries(),
            cli.max_file_size,
        ),
        _ => SubagentMatches::default(),
    };

    let mut main_entries: Vec<&LogEntry> = conversation.main_thread_entries();

    // Restrict to prompt-boundary chunk(s) when --chunk is given. Chunk
    // membership is tree-based, so late async results belong to the chunk
    // that spawned them (appended after its main-thread members).
    if let Some(ref spec) = args.chunk {
        let chunking = chunk_conversation(&conversation);
        let (start, end) = parse_chunk_spec(spec, chunking.len())
            .map_err(|message| SnatchError::ConfigError { message })?;
        main_entries = entries_for_chunk_range(&conversation, &chunking, start, end);
        if !cli.quiet {
            let branches: usize = chunking.chunks[start..=end]
                .iter()
                .map(|c| c.branches.len())
                .sum();
            let range = if start == end {
                format!("chunk {start}")
            } else {
                format!("chunks {start}-{end}")
            };
            let branch_note = if branches > 0 {
                format!(" ({branches} abandoned branch(es) not shown — see `snatch chunks`)")
            } else {
                String::new()
            };
            eprintln!(
                "ℹ Showing {range} of {} ({} entries){branch_note}",
                chunking.len(),
                main_entries.len(),
            );
        }
    }

    // Error drill-down: keep failed tool results AND the assistant entries
    // that issued the failing calls (the result carries the error text, the
    // call carries the command — an audit needs both).
    if args.errors_only {
        let failed = failed_tool_use_ids(&main_entries);
        main_entries.retain(|e| match e {
            LogEntry::User(_) => has_tool_errors(std::slice::from_ref(e)),
            LogEntry::Assistant(a) => a.message.tool_uses().iter().any(|t| failed.contains(&t.id)),
            _ => false,
        });
    }

    // Filter by message type
    match msg_type_filter {
        "user" => main_entries.retain(|e| human(e)),
        "assistant" => main_entries.retain(|e| matches!(e, LogEntry::Assistant(_))),
        "system" => main_entries.retain(|e| matches!(e, LogEntry::System(_))),
        _ => {}
    }

    // Filter by timestamp window
    if args.after.is_some() || args.before.is_some() {
        use chrono::{DateTime, Utc};
        let after = if let Some(ref ts) = args.after {
            let systime = super::parse_date_filter(ts)?;
            Some(DateTime::<Utc>::from(systime))
        } else {
            None
        };
        let before = if let Some(ref ts) = args.before {
            let systime = super::parse_date_filter(ts)?;
            Some(DateTime::<Utc>::from(systime))
        } else {
            None
        };
        main_entries.retain(|e| {
            if let Some(ts) = e.timestamp() {
                if let Some(ref a) = after {
                    if ts < *a {
                        return false;
                    }
                }
                if let Some(ref b) = before {
                    if ts > *b {
                        return false;
                    }
                }
                true
            } else {
                true
            }
        });
    }

    // Pre-filter by detail level. Overview uses the chunker's boundary
    // predicate (typed prompts + queued steering prompts) so its indices
    // always match chunk indices.
    match detail {
        "overview" => {
            if semantic {
                main_entries.retain(|e| human(e));
            } else {
                main_entries.retain(|e| is_prompt_boundary(e));
            }
        }
        "conversation" => {
            main_entries.retain(|e| match e {
                LogEntry::User(_) => human(e),
                LogEntry::Assistant(_) => extract_assistant_summary(e, 1).is_some(),
                // Queued steering prompts are dialogue, not tool noise.
                LogEntry::Attachment(_) => queued_human_prompt(e).is_some(),
                _ => false,
            });
        }
        _ => {}
    }

    let total_messages = main_entries.len();

    // Join tool results onto their calls at full detail (parity with the MCP
    // server): had_error plus error/result previews per tool_use id. Scans
    // every node — not just the main thread — so late async results join too.
    let tool_result_previews: HashMap<String, (bool, Option<String>)> = if detail == "full" {
        let mut map = HashMap::new();
        for node in conversation.nodes().values() {
            if let LogEntry::User(user) = &node.entry {
                for result in user.message.tool_results() {
                    let is_err = result.is_error == Some(true);
                    let preview = if is_err {
                        extract_error_preview(result, 300)
                    } else {
                        extract_result_preview(result, 500)
                    };
                    map.insert(result.tool_use_id.clone(), (is_err, preview));
                }
            }
        }
        map
    } else {
        HashMap::new()
    };

    // Build indexed pairs
    let mut indexed: Vec<(usize, &LogEntry)> = main_entries.into_iter().enumerate().collect();

    if args.reverse {
        indexed.reverse();
    }

    // Paginate
    let paginated: Vec<(usize, &LogEntry)> = indexed.into_iter().skip(offset).take(limit).collect();

    match cli.effective_output() {
        OutputFormat::Json => {
            let messages: Vec<MessageOutput> = paginated
                .iter()
                .filter_map(|(orig_idx, entry)| {
                    build_message_output(
                        *orig_idx,
                        entry,
                        detail,
                        truncate_len,
                        include_thinking,
                        thinking_max_len,
                        &subagent_matches.matched,
                        args.subagent_transcripts,
                        cli.max_file_size,
                        &tool_result_previews,
                    )
                })
                .collect();

            let output = MessagesOutput {
                session_id: ctx.display_id.clone(),
                project_path: ctx.project_path.clone(),
                chain_id: ctx.chain.as_ref().map(|c| c.root_id.clone()),
                chain_members: ctx.chain.as_ref().map(|c| c.members.clone()),
                chain_member_count: ctx.chain.as_ref().map(|c| c.members.len()),
                total_messages,
                returned: messages.len(),
                offset,
                messages,
                unmatched_subagents: subagent_matches
                    .unmatched
                    .iter()
                    .map(|m| UnmatchedSubagentOutput {
                        session_id: m.session_id.clone(),
                        message_count: m.message_count,
                        result_preview: m.result_preview.clone(),
                    })
                    .collect(),
            };
            println!("{}", serde_json::to_string_pretty(&output)?);
        }
        _ => {
            // Text output
            if paginated.is_empty() {
                println!("No messages found.");
                return Ok(());
            }

            // Honest header: pagination slices raw entries, but some entries
            // produce no output at this detail level (bare tool results,
            // metadata). Count the renderable ones up front so "showing X-Y"
            // never overstates what follows.
            let rendered_count = paginated
                .iter()
                .filter(|(_, e)| {
                    renders_at_detail(e, detail, include_thinking, thinking_max_len, truncate_len)
                })
                .count();
            let skipped = paginated.len() - rendered_count;
            let skip_note = if skipped > 0 {
                format!("; {rendered_count} rendered, {skipped} with no content at this detail")
            } else {
                String::new()
            };
            println!(
                "Session {} ({} messages, showing {}-{}{skip_note})\n",
                &ctx.display_id[..8.min(ctx.display_id.len())],
                total_messages,
                offset + 1,
                (offset + paginated.len()).min(total_messages),
            );
            if ctx.unparsed > 0 {
                println!(
                    "⚠ {} line{} could not be parsed (dropped from this view)\n",
                    ctx.unparsed,
                    if ctx.unparsed == 1 { "" } else { "s" }
                );
            }

            for (orig_idx, entry) in &paginated {
                let msg_type = entry.message_type();
                let timestamp = entry
                    .timestamp()
                    .map(|t| t.format("%H:%M:%S").to_string())
                    .unwrap_or_default();

                match detail {
                    "overview" => {
                        if let Some(text) = boundary_prompt_text(entry) {
                            let marker = if matches!(entry, LogEntry::Attachment(_)) {
                                "user (queued)"
                            } else {
                                "user"
                            };
                            println!(
                                "[{orig_idx}] {timestamp} {marker}: {}",
                                truncate_text(&text, truncate_len)
                            );
                        }
                    }
                    "conversation" => {
                        let content = match entry {
                            LogEntry::User(_) => extract_user_prompt_text(entry)
                                .map(|t| truncate_text(&t, truncate_len)),
                            LogEntry::Assistant(_) => {
                                extract_assistant_summary(entry, truncate_len)
                            }
                            LogEntry::Attachment(_) => queued_human_prompt(entry)
                                .map(|t| format!("(queued) {}", truncate_text(t, truncate_len))),
                            _ => None,
                        };
                        if let Some(text) = content {
                            println!("[{orig_idx}] {timestamp} {msg_type}: {text}");
                        }
                        if include_thinking {
                            if let Some(thinking) = extract_thinking_text(entry, thinking_max_len) {
                                println!("    thinking: {}", truncate_text(&thinking, 200));
                            }
                        }
                    }
                    "standard" => {
                        let content = match entry {
                            LogEntry::User(_) => extract_user_prompt_text(entry)
                                .map(|t| truncate_text(&t, truncate_len)),
                            LogEntry::Assistant(_) => {
                                extract_assistant_summary(entry, truncate_len)
                            }
                            LogEntry::System(sys) => sys.content.clone(),
                            LogEntry::Attachment(_) => {
                                render_attachment_content(entry, truncate_len)
                            }
                            _ => None,
                        };
                        let tools = extract_tool_names(entry);
                        let thinking = if include_thinking {
                            extract_thinking_text(entry, thinking_max_len)
                        } else {
                            None
                        };

                        // Skip entries with nothing to show in text mode
                        if content.is_none() && tools.is_empty() && thinking.is_none() {
                            continue;
                        }

                        let model_str = get_model(entry)
                            .map(|m| format!(" ({m})"))
                            .unwrap_or_default();
                        println!("[{orig_idx}] {timestamp} {msg_type}{model_str}:");
                        if let Some(text) = content {
                            println!("    {text}");
                        }
                        if !tools.is_empty() {
                            println!("    tools: {}", tools.join(", "));
                        }
                        if let Some(thinking) = thinking {
                            println!("    thinking: {}", truncate_text(&thinking, 200));
                        }
                        println!();
                    }
                    _ => {
                        // "full"
                        let content = match entry {
                            LogEntry::User(_) => extract_user_prompt_text(entry)
                                .map(|t| truncate_text(&t, truncate_len)),
                            LogEntry::Assistant(_) => {
                                extract_assistant_summary(entry, truncate_len)
                            }
                            LogEntry::System(sys) => sys.content.clone(),
                            LogEntry::Attachment(_) => {
                                render_attachment_content(entry, truncate_len)
                            }
                            _ => None,
                        };
                        let tool_uses: Vec<_> = if let LogEntry::Assistant(a) = entry {
                            a.message.tool_uses()
                        } else {
                            vec![]
                        };
                        let thinking = if include_thinking {
                            extract_thinking_text(entry, thinking_max_len)
                        } else {
                            None
                        };

                        // Skip entries with nothing to show in text mode
                        if content.is_none() && tool_uses.is_empty() && thinking.is_none() {
                            continue;
                        }

                        let model_str = get_model(entry)
                            .map(|m| format!(" ({m})"))
                            .unwrap_or_default();
                        println!("[{orig_idx}] {timestamp} {msg_type}{model_str}:");
                        if let Some(text) = content {
                            println!("    {text}");
                        }
                        for t in &tool_uses {
                            let summary = extract_tool_input_summary(&t.name, &t.input);
                            let detail_str: Vec<String> =
                                summary.iter().map(|(k, v)| format!("{k}={v}")).collect();
                            println!("    > {} {}", t.name, detail_str.join(" "));

                            // Surface the error text with its failing call so
                            // an error audit sees command + error together.
                            if let Some((true, Some(err))) = tool_result_previews.get(&t.id) {
                                println!("      ✗ {err}");
                            }

                            // Attach the spawned subagent's work to its Agent/Task call.
                            if let Some(m) = subagent_matches.matched.get(&t.id) {
                                let msgs = m
                                    .message_count
                                    .map(|n| format!(" ({n} msgs)"))
                                    .unwrap_or_default();
                                println!("      -> subagent {}{}", m.session_id, msgs);
                                if let Some(preview) = &m.result_preview {
                                    println!("         result: {}", truncate_text(preview, 200));
                                }
                                if args.subagent_transcripts {
                                    for sub in render_subagent_transcript_cli(
                                        &m.path,
                                        include_thinking,
                                        cli.max_file_size,
                                    ) {
                                        let c = sub.content.unwrap_or_default();
                                        println!(
                                            "         [{}] {}: {}",
                                            sub.index,
                                            sub.msg_type,
                                            truncate_text(&c, 200)
                                        );
                                    }
                                }
                            }
                        }
                        if let Some(thinking) = thinking {
                            println!("    thinking: {}", truncate_text(&thinking, 300));
                        }
                        println!();
                    }
                }
            }

            // Subagents present on disk but not joinable to a specific spawn call.
            // Emitting a marker keeps a present subagent from vanishing silently.
            for m in &subagent_matches.unmatched {
                let msgs = m
                    .message_count
                    .map(|n| format!("{n} msgs"))
                    .unwrap_or_else(|| "? msgs".to_string());
                println!("[subagent {}: {}, unlinked]", m.session_id, msgs);
                if let Some(preview) = &m.result_preview {
                    println!("    result: {}", truncate_text(preview, 200));
                }
                println!();
            }
        }
    }

    Ok(())
}

/// Render a subagent's main thread as message outputs (standard detail: user and
/// assistant text plus tool names; tool details are not expanded recursively).
fn render_subagent_transcript_cli(
    path: &std::path::Path,
    include_thinking: bool,
    max_file_size: Option<u64>,
) -> Vec<MessageOutput> {
    let entries = crate::discovery::Session::from_path(path, "")
        .ok()
        .and_then(|s| s.parse_with_options(max_file_size).ok())
        .unwrap_or_default();
    let Ok(conversation) = Conversation::from_entries(entries) else {
        return vec![];
    };
    conversation
        .main_thread_entries()
        .iter()
        .enumerate()
        .map(|(i, entry)| {
            let content = match entry {
                LogEntry::User(_) => {
                    extract_user_prompt_text(entry).map(|t| truncate_text(&t, 500))
                }
                LogEntry::Assistant(_) => extract_assistant_summary(entry, 500),
                LogEntry::System(sys) => sys.content.clone(),
                _ => None,
            };
            let tool_names = extract_tool_names(entry);
            let thinking = if include_thinking {
                extract_thinking_text(entry, 1000)
            } else {
                None
            };
            MessageOutput {
                index: i,
                msg_type: entry.message_type().to_string(),
                timestamp: entry.timestamp().map(|t| t.to_rfc3339()),
                content,
                git_branch: entry.git_branch().map(|s| s.to_string()),
                model: get_model(entry),
                tool_calls: if tool_names.is_empty() {
                    None
                } else {
                    Some(tool_names)
                },
                tool_details: None,
                has_thinking: if has_thinking(entry) {
                    Some(true)
                } else {
                    None
                },
                thinking_preview: thinking,
            }
        })
        .collect()
}

/// Build a JSON message output for a single entry.
#[allow(clippy::too_many_arguments)]
fn build_message_output(
    index: usize,
    entry: &LogEntry,
    detail: &str,
    truncate_len: usize,
    include_thinking: bool,
    thinking_max_len: usize,
    subagent_matches: &HashMap<String, SubagentMatch>,
    include_subagent_transcripts: bool,
    max_file_size: Option<u64>,
    tool_result_previews: &HashMap<String, (bool, Option<String>)>,
) -> Option<MessageOutput> {
    let msg_type = entry.message_type().to_string();
    let timestamp = entry.timestamp().map(|t| t.to_rfc3339());
    let git_branch = entry.git_branch().map(|s| s.to_string());

    match detail {
        "overview" => {
            let content = boundary_prompt_text(entry).map(|t| truncate_text(&t, truncate_len));
            Some(MessageOutput {
                index,
                msg_type,
                timestamp,
                content,
                git_branch,
                model: None,
                tool_calls: None,
                tool_details: None,
                has_thinking: None,
                thinking_preview: None,
            })
        }
        "conversation" => {
            let content = match entry {
                LogEntry::User(_) => {
                    extract_user_prompt_text(entry).map(|t| truncate_text(&t, truncate_len))
                }
                LogEntry::Assistant(_) => extract_assistant_summary(entry, truncate_len),
                LogEntry::Attachment(_) => queued_human_prompt(entry)
                    .map(|t| format!("(queued) {}", truncate_text(t, truncate_len))),
                _ => None,
            };
            let thinking = if include_thinking {
                extract_thinking_text(entry, thinking_max_len)
            } else {
                None
            };
            Some(MessageOutput {
                index,
                msg_type,
                timestamp,
                content,
                git_branch,
                model: get_model(entry),
                tool_calls: None,
                tool_details: None,
                has_thinking: if has_thinking(entry) {
                    Some(true)
                } else {
                    None
                },
                thinking_preview: thinking,
            })
        }
        "standard" => {
            let content = match entry {
                LogEntry::User(_) => {
                    extract_user_prompt_text(entry).map(|t| truncate_text(&t, truncate_len))
                }
                LogEntry::Assistant(_) => extract_assistant_summary(entry, truncate_len),
                LogEntry::System(sys) => sys.content.clone(),
                LogEntry::Attachment(_) => render_attachment_content(entry, truncate_len),
                _ => None,
            };
            let tool_names = extract_tool_names(entry);
            let thinking = if include_thinking {
                extract_thinking_text(entry, thinking_max_len)
            } else {
                None
            };
            Some(MessageOutput {
                index,
                msg_type,
                timestamp,
                content,
                git_branch,
                model: get_model(entry),
                tool_calls: if tool_names.is_empty() {
                    None
                } else {
                    Some(tool_names)
                },
                tool_details: None,
                has_thinking: if has_thinking(entry) {
                    Some(true)
                } else {
                    None
                },
                thinking_preview: thinking,
            })
        }
        _ => {
            // "full"
            let content = match entry {
                LogEntry::User(_) => {
                    extract_user_prompt_text(entry).map(|t| truncate_text(&t, truncate_len))
                }
                LogEntry::Assistant(_) => extract_assistant_summary(entry, truncate_len),
                LogEntry::System(sys) => sys.content.clone(),
                LogEntry::Attachment(_) => render_attachment_content(entry, truncate_len),
                _ => None,
            };
            let tool_names = extract_tool_names(entry);
            let tool_details: Vec<ToolDetailOutput> = if let LogEntry::Assistant(a) = entry {
                a.message
                    .tool_uses()
                    .iter()
                    .map(|t| {
                        let summary = extract_tool_input_summary(&t.name, &t.input);
                        let matched = subagent_matches.get(&t.id);
                        let (had_error, preview) = tool_result_previews
                            .get(&t.id)
                            .cloned()
                            .unwrap_or((false, None));
                        ToolDetailOutput {
                            tool_name: t.name.clone(),
                            file_path: summary.get("file_path").cloned(),
                            command: summary.get("command").cloned(),
                            pattern: summary.get("pattern").cloned(),
                            subagent_type: summary.get("subagent_type").cloned(),
                            description: summary.get("description").cloned(),
                            prompt: summary.get("prompt").cloned(),
                            subagent_session_id: matched.map(|m| m.session_id.clone()),
                            subagent_result_preview: matched.and_then(|m| m.result_preview.clone()),
                            subagent_transcript: matched
                                .filter(|_| include_subagent_transcripts)
                                .map(|m| {
                                    render_subagent_transcript_cli(
                                        &m.path,
                                        include_thinking,
                                        max_file_size,
                                    )
                                }),
                            had_error: if had_error { Some(true) } else { None },
                            error_preview: if had_error { preview.clone() } else { None },
                            // Agent/Task calls carry the richer subagent
                            // preview instead.
                            result_preview: if had_error || matched.is_some() {
                                None
                            } else {
                                preview
                            },
                        }
                    })
                    .collect()
            } else {
                vec![]
            };
            let thinking = if include_thinking {
                extract_thinking_text(entry, thinking_max_len)
            } else {
                None
            };
            Some(MessageOutput {
                index,
                msg_type,
                timestamp,
                content,
                git_branch,
                model: get_model(entry),
                tool_calls: if tool_names.is_empty() {
                    None
                } else {
                    Some(tool_names)
                },
                tool_details: if tool_details.is_empty() {
                    None
                } else {
                    Some(tool_details)
                },
                has_thinking: if has_thinking(entry) {
                    Some(true)
                } else {
                    None
                },
                thinking_preview: thinking,
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::effective_include_thinking;

    #[test]
    fn full_detail_implies_thinking_without_flag() {
        // "full means full": thinking is on even when the flag is unset.
        assert!(effective_include_thinking(false, "full"));
    }

    #[test]
    fn non_full_detail_still_gated_by_flag() {
        for detail in ["overview", "conversation", "standard"] {
            assert!(
                !effective_include_thinking(false, detail),
                "{detail} should hide thinking without the flag"
            );
            assert!(
                effective_include_thinking(true, detail),
                "{detail} should show thinking with the flag"
            );
        }
    }

    #[test]
    fn full_detail_with_flag_stays_on_once() {
        // Both full + flag: still a single boolean, so no double-rendering.
        assert!(effective_include_thinking(true, "full"));
    }
}
