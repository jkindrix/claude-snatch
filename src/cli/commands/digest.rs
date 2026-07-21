//! Digest command implementation.
//!
//! Produces a compact summary of a session's key topics, files,
//! tools, and decisions for quick orientation.

use crate::analysis::digest::{build_digest_from_conversation, format_digest, DigestOptions};
use crate::cli::{Cli, DigestArgs, OutputFormat};
use crate::error::{Result, SnatchError};
use crate::reconstruction::Conversation;

use super::get_claude_dir;

/// Run the digest command.
pub fn run(cli: &Cli, args: &DigestArgs) -> Result<()> {
    let registry = super::helpers::provider_registry(cli);
    let provider_route = !args.provider.is_empty() || registry.looks_qualified(&args.session_id);
    let (conversation, display_id, project_path, provider, qualified_id, semantic_annotations) =
        if provider_route {
            let resolution =
                registry.resolve_with_default_policy(&args.provider, &args.session_id)?;
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
            (
                Conversation::from_parsed_session(parsed)?,
                resolution.key.to_string(),
                project_path,
                Some(resolution.key.provider.to_string()),
                Some(resolution.key.to_string()),
                resolution.provider.capabilities().semantic_annotations,
            )
        } else {
            let claude_dir = get_claude_dir(cli.claude_dir.as_ref())?;
            let session = claude_dir.find_session(&args.session_id)?.ok_or_else(|| {
                SnatchError::SessionNotFound {
                    session_id: args.session_id.clone(),
                }
            })?;
            let project_path = session.project_path().to_string();
            let entries = session.parse_with_options(cli.max_file_size)?;
            (
                Conversation::from_entries(entries)?,
                args.session_id.clone(),
                project_path,
                None,
                None,
                false,
            )
        };
    if !cli.quiet {
        if let Some(notice) = conversation.duplicate_notice() {
            eprintln!("{notice}");
        }
    }
    let opts = DigestOptions {
        max_prompts: args.max_prompts,
        ..DigestOptions::default()
    };

    let digest = build_digest_from_conversation(&conversation, &opts, semantic_annotations);

    match cli.effective_output() {
        OutputFormat::Json => {
            let output = serde_json::json!({
                "session_id": display_id,
                "provider": provider,
                "qualified_id": qualified_id,
                "project_path": project_path,
                "key_prompts": digest.key_prompts,
                "recent_prompts": digest.recent_prompts,
                "total_prompts": digest.total_prompts,
                "files_touched": digest.files_touched,
                "top_tools": digest.top_tools,
                "error_count": digest.error_count,
                "confirmed_tool_failures": digest.confirmed_tool_failures,
                "inferred_failure_signals": digest.inferred_failure_signals,
                "compaction_count": digest.compaction_count,
                "thinking_keywords": digest.thinking_keywords,
            });
            println!("{}", serde_json::to_string_pretty(&output)?);
        }
        _ => {
            let formatted = format_digest(&digest, opts.max_chars);
            println!("Session Digest: {}\n", display_id);
            println!("{formatted}");
        }
    }

    Ok(())
}
