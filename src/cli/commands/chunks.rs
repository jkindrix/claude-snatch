//! Chunks command implementation.
//!
//! Lists the prompt-boundary chunks of a session: each human prompt and
//! everything it produced, with entry/tool counts and any abandoned branches.
//! Chunk indices feed the `--chunk` selector on `snatch messages` and the
//! `chunk` parameter of the MCP `get_session_messages` tool.

use crate::analysis::chunking::chunk_conversation;
use crate::analysis::extraction::truncate_text;
use crate::cli::{ChunksArgs, Cli, OutputFormat};
use crate::error::{Result, SnatchError};
use crate::reconstruction::Conversation;

use super::get_claude_dir;

/// JSON output types.
#[derive(serde::Serialize)]
struct ChunksOutput {
    session_id: String,
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

/// Run the chunks command.
pub fn run(cli: &Cli, args: &ChunksArgs) -> Result<()> {
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

    let chunking = chunk_conversation(&conversation);

    match cli.effective_output() {
        OutputFormat::Json => {
            let output = ChunksOutput {
                session_id: args.session_id.clone(),
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
                &args.session_id[..8.min(args.session_id.len())],
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
                let queued_marker = match chunk.prompt_source {
                    crate::analysis::chunking::PromptSource::Queued => "(queued) ",
                    crate::analysis::chunking::PromptSource::User => "",
                };
                println!(
                    "[{:3}] {start}  {:4} entries  {:3} tools{attached}  {queued_marker}{}",
                    chunk.index,
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
                &args.session_id[..8.min(args.session_id.len())],
            );
        }
    }

    Ok(())
}
