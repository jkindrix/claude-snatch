//! Digest command implementation.
//!
//! Produces a compact summary of a session's key topics, files,
//! tools, and decisions for quick orientation.

use crate::analysis::digest::{build_digest, format_digest, DigestOptions};
use crate::cli::{Cli, DigestArgs, OutputFormat};
use crate::error::{Result, SnatchError};
use crate::reconstruction::Conversation;

use super::get_claude_dir;

/// Run the digest command.
pub fn run(cli: &Cli, args: &DigestArgs) -> Result<()> {
    let claude_dir = get_claude_dir(cli.claude_dir.as_ref())?;

    let session = claude_dir
        .find_session(&args.session_id)?
        .ok_or_else(|| SnatchError::SessionNotFound {
            session_id: args.session_id.clone(),
        })?;

    let entries = session.parse_with_options(cli.max_file_size)?;
    let conversation = Conversation::from_entries(entries)?;
    let all_entries = conversation.chronological_entries();
    let entry_refs: Vec<&_> = all_entries.iter().map(|e| *e).collect();

    let opts = DigestOptions {
        max_prompts: args.max_prompts,
        ..DigestOptions::default()
    };

    let digest = build_digest(&entry_refs, &opts);

    match cli.effective_output() {
        OutputFormat::Json => {
            let formatted = format_digest(&digest, opts.max_chars);
            let output = serde_json::json!({
                "session_id": args.session_id,
                "project_path": session.project_path(),
                "key_prompts": digest.key_prompts,
                "files_touched": digest.files_touched,
                "top_tools": digest.top_tools,
                "error_count": digest.error_count,
                "compaction_count": digest.compaction_count,
                "thinking_keywords": digest.thinking_keywords,
                "formatted": formatted,
            });
            println!("{}", serde_json::to_string_pretty(&output)?);
        }
        _ => {
            let formatted = format_digest(&digest, opts.max_chars);
            println!("Session Digest: {}\n", args.session_id);
            println!("{formatted}");
        }
    }

    Ok(())
}
