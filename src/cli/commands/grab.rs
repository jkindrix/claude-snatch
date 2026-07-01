//! `grab` command: one-shot "give this whole session to Claude" export.
//!
//! A thin convenience wrapper over `export` that fixes the sane defaults for the
//! common case — parent session plus its subagent transcripts, in a single file
//! Claude can read — so the user never has to assemble the flag combination by
//! hand. Readable markdown with full tool outputs by default; `--raw` swaps in
//! the byte-faithful JSONL bundle.

use clap::Parser;

use crate::cli::{Cli, ExportArgs, GrabArgs};
use crate::error::Result;

/// Run the grab command by delegating to `export` with fixed defaults.
pub fn run(cli: &Cli, args: &GrabArgs) -> Result<()> {
    // Build the equivalent `export` invocation. The leading element is the
    // binary-name slot that clap ignores.
    let mut argv: Vec<String> = vec!["grab".to_string(), args.session.clone()];

    // Always pull in the subagents; that is the whole point of "grab the session".
    argv.push("--combine-agents".to_string());

    if args.raw {
        // Byte-faithful bundle: parent + subagent transcripts, verbatim.
        argv.push("-f".to_string());
        argv.push("raw-jsonl".to_string());
    } else {
        // Readable markdown (the default format) with externalized tool outputs
        // inlined so nothing is truncated.
        argv.push("--resolve-tool-results".to_string());
    }

    if let Some(out) = &args.output_file {
        argv.push("-O".to_string());
        argv.push(out.to_string_lossy().into_owned());
    }

    let export_args =
        ExportArgs::try_parse_from(argv).map_err(|e| crate::error::SnatchError::ConfigError {
            message: format!("failed to build export arguments for grab: {e}"),
        })?;

    super::export::run(cli, &export_args)
}
