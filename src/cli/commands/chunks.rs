//! Chunks command implementation.
//!
//! Lists the prompt-boundary chunks of a session: each boundary prompt and
//! everything it produced, with entry/tool counts and any abandoned branches.
//! Semantic providers keep midturn steering inside the active chunk.
//! Chunk indices feed the `--chunk` selector on `snatch messages` and the
//! `chunk` parameter of the MCP `get_session_messages` tool.

use crate::analysis::chunking::{chunk_conversation, chunk_conversation_semantic};
use crate::analysis::extraction::truncate_text;
use crate::cli::{ChunksArgs, Cli, OutputFormat};
use crate::error::{Result, SnatchError};
use crate::reconstruction::Conversation;

use super::get_claude_dir;

/// JSON output types.
#[derive(serde::Serialize)]
struct ChunksOutput {
    session_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    provider: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    qualified_id: Option<String>,
    project_path: String,
    /// Root file id of the resume chain, when chain members were merged.
    #[serde(skip_serializing_if = "Option::is_none")]
    chain_id: Option<String>,
    total_chunks: usize,
    /// Tree entries before the first human prompt (hook injections etc.).
    preamble_entries: usize,
    chunks: Vec<ChunkOutput>,
}

#[derive(serde::Serialize)]
struct ChunkOutput {
    index: usize,
    prompt: String,
    /// "user" (typed at a turn boundary) or "queued" (mid-turn steering).
    prompt_source: &'static str,
    prompt_uuid: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    start_ts: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    end_ts: Option<String>,
    /// Main-thread + attached entries (branch entries not counted).
    entries: usize,
    /// Off-main-thread members (late async results, progress leaves).
    attached: usize,
    tool_calls: usize,
    /// Failed tool results (is_error) among member entries.
    errors: usize,
    /// Abandoned branches (e.g. rewind forks) hanging off this chunk.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    branches: Vec<BranchOutput>,
}

#[derive(serde::Serialize)]
struct BranchOutput {
    root_uuid: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    prompt: Option<String>,
    entries: usize,
}

/// Compact wall-clock span for the listing ("45s", "9m", "1h 12m").
fn fmt_span(
    start: Option<chrono::DateTime<chrono::Utc>>,
    end: Option<chrono::DateTime<chrono::Utc>>,
) -> String {
    let (Some(s), Some(e)) = (start, end) else {
        return "?".to_string();
    };
    let secs = (e - s).num_seconds().max(0);
    match secs {
        0..=59 => format!("{secs}s"),
        60..=3599 => format!("{}m", secs / 60),
        _ => format!("{}h {}m", secs / 3600, (secs % 3600) / 60),
    }
}

/// Run the chunks command.
pub fn run(cli: &Cli, args: &ChunksArgs) -> Result<()> {
    let registry = super::helpers::provider_registry(cli);
    let provider_route = !args.provider.is_empty() || registry.looks_qualified(&args.session_id);
    let (
        display_id,
        session_id,
        provider,
        qualified_id,
        project_path,
        chain,
        unparsed,
        semantic,
        conversation,
    ) = if provider_route {
        super::helpers::refuse_unsupported_flags(
            "provider-routed chunks",
            &[("--no-chain", args.no_chain)],
        )?;
        let resolution = registry.resolve_with_default_policy(&args.provider, &args.session_id)?;
        let parsed = crate::provider::registry::cached_parsed_session(
            crate::cache::global_cache(),
            resolution.provider,
            &resolution.key,
        )?;
        let project_path = parsed
            .entries
            .iter()
            .find_map(|entry| entry.entry.cwd().map(String::from))
            .unwrap_or_else(|| "unknown".to_string());
        let unparsed = parsed.diagnostics.unparseable;
        let semantic = resolution.provider.capabilities().semantic_annotations;
        let native_id = resolution.key.native_id.clone();
        let qualified = resolution.key.to_string();
        (
            qualified.clone(),
            native_id,
            Some(resolution.key.provider.to_string()),
            Some(qualified),
            project_path,
            None,
            unparsed,
            semantic,
            Conversation::from_parsed_session(parsed)?,
        )
    } else {
        let claude_dir = get_claude_dir(cli.claude_dir.as_ref())?;
        let session = claude_dir.find_session(&args.session_id)?.ok_or_else(|| {
            SnatchError::SessionNotFound {
                session_id: args.session_id.clone(),
            }
        })?;
        let project_path = session.project_path().to_string();
        let chain_aware = !args.no_chain;
        let (entries, unparsed, chain) = super::helpers::resolve_chain_entries(
            &claude_dir,
            &session,
            chain_aware,
            cli.max_file_size,
        )?;
        (
            args.session_id.clone(),
            args.session_id.clone(),
            None,
            None,
            project_path,
            chain,
            unparsed,
            false,
            Conversation::from_entries(entries)?,
        )
    };
    if let Some(ref chain) = chain {
        if !cli.quiet {
            eprintln!(
                "ℹ Showing full resume chain: {} files (root {}). Use --no-chain to restrict.",
                chain.members.len(),
                chain.root_id
            );
        }
    }
    if !cli.quiet {
        if let Some(notice) = conversation.duplicate_notice() {
            eprintln!("{notice}");
        }
    }

    let chunking = if semantic {
        chunk_conversation_semantic(&conversation)
    } else {
        chunk_conversation(&conversation)
    };

    match cli.effective_output() {
        OutputFormat::Json => {
            let output = ChunksOutput {
                session_id,
                provider,
                qualified_id,
                project_path,
                chain_id: chain.as_ref().map(|c| c.root_id.clone()),
                total_chunks: chunking.len(),
                preamble_entries: chunking.preamble_uuids.len(),
                chunks: chunking
                    .chunks
                    .iter()
                    .map(|c| ChunkOutput {
                        index: c.index,
                        prompt: truncate_text(&c.prompt_text, 200),
                        prompt_source: c.prompt_source.as_str(),
                        prompt_uuid: c.prompt_uuid.clone(),
                        start_ts: c.start_ts.map(|t| t.to_rfc3339()),
                        end_ts: c.end_ts.map(|t| t.to_rfc3339()),
                        entries: c.entry_count(),
                        attached: c.attached_uuids.len(),
                        tool_calls: c.tool_call_count,
                        errors: c.error_count,
                        branches: c
                            .branches
                            .iter()
                            .map(|b| BranchOutput {
                                root_uuid: b.root_uuid.clone(),
                                prompt: b.prompt_text.as_deref().map(|p| truncate_text(p, 100)),
                                entries: b.uuids.len(),
                            })
                            .collect(),
                    })
                    .collect(),
            };
            println!("{}", serde_json::to_string_pretty(&output)?);
        }
        _ => {
            if chunking.is_empty() {
                println!(
                    "No chunks: session has no human prompts ({} preamble entries).",
                    chunking.preamble_uuids.len()
                );
                return Ok(());
            }

            println!(
                "Session {} — {} chunks, {} preamble entries\n",
                &display_id[..8.min(display_id.len())],
                chunking.len(),
                chunking.preamble_uuids.len(),
            );
            if unparsed > 0 {
                println!(
                    "⚠ {unparsed} line{} could not be parsed (dropped from this view)\n",
                    if unparsed == 1 { "" } else { "s" }
                );
            }

            for chunk in &chunking.chunks {
                let start = chunk
                    .start_ts
                    .map(|t| t.format("%m-%d %H:%M").to_string())
                    .unwrap_or_else(|| "?".to_string());
                let attached = if chunk.attached_uuids.is_empty() {
                    String::new()
                } else {
                    format!(" (+{} attached)", chunk.attached_uuids.len())
                };
                let errors = if chunk.error_count == 0 {
                    String::new()
                } else {
                    format!(" ⚠{} errors", chunk.error_count)
                };
                let queued_marker = match chunk.prompt_source {
                    crate::analysis::chunking::PromptSource::Queued => "(queued) ",
                    crate::analysis::chunking::PromptSource::User => "",
                };
                println!(
                    "[{:3}] {start} {:>7}  {:4} entries  {:3} tools{errors}{attached}  {queued_marker}{}",
                    chunk.index,
                    fmt_span(chunk.start_ts, chunk.end_ts),
                    chunk.entry_count(),
                    chunk.tool_call_count,
                    truncate_text(&chunk.prompt_text.replace('\n', " "), 80),
                );
                for branch in &chunk.branches {
                    let label = branch
                        .prompt_text
                        .as_deref()
                        .map(|p| format!("\"{}\"", truncate_text(&p.replace('\n', " "), 60)))
                        .unwrap_or_else(|| "(no prompt)".to_string());
                    println!(
                        "      └ abandoned branch, {} entries: {label}",
                        branch.uuids.len()
                    );
                }
            }
            println!(
                "\nRetrieve a chunk with: snatch messages {} --chunk <N> (or a range like 2-5)",
                display_id,
            );
        }
    }

    Ok(())
}
