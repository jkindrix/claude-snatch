//! Export command implementation.
//!
//! Exports conversations to various formats (Markdown, JSON, etc.).

use std::io::{self, Write};

use crate::cli::{Cli, ExportArgs, ExportFormatArg};
use crate::error::{Result, SnatchError};
use crate::export::{
    conversation_to_jsonl, ExportOptions, Exporter, JsonExporter,
    MarkdownExporter,
};
use crate::reconstruction::Conversation;

use super::get_claude_dir;

/// Run the export command.
pub fn run(cli: &Cli, args: &ExportArgs) -> Result<()> {
    let claude_dir = get_claude_dir(cli.claude_dir.as_ref())?;

    // Find the session
    let session = claude_dir
        .find_session(&args.session)?
        .ok_or_else(|| SnatchError::SessionNotFound {
            session_id: args.session.clone(),
        })?;

    // Parse the session
    let entries = session.parse()?;

    // Build conversation tree
    let conversation = Conversation::from_entries(entries)?;

    // Build export options
    let options = ExportOptions {
        include_thinking: args.thinking,
        include_tool_use: args.tool_use,
        include_tool_results: args.tool_results,
        include_system: args.system,
        include_timestamps: args.timestamps,
        include_usage: args.usage,
        include_metadata: args.metadata,
        include_images: true,
        max_depth: None,
        truncate_at: None,
        include_branches: !args.main_thread,
        main_thread_only: args.main_thread,
    };

    // Get output writer
    let mut writer: Box<dyn Write> = match &args.output {
        Some(path) => {
            let file = std::fs::File::create(path).map_err(|e| {
                SnatchError::io(format!("Failed to create output file: {}", path.display()), e)
            })?;
            Box::new(std::io::BufWriter::new(file))
        }
        None => Box::new(io::stdout().lock()),
    };

    // Export based on format
    match args.format {
        ExportFormatArg::Markdown | ExportFormatArg::Md => {
            let exporter = MarkdownExporter::new();
            exporter.export_conversation(&conversation, &mut writer, &options)?;
        }
        ExportFormatArg::Json => {
            let exporter = JsonExporter::new().pretty(args.pretty);
            exporter.export_conversation(&conversation, &mut writer, &options)?;
        }
        ExportFormatArg::JsonPretty => {
            let exporter = JsonExporter::new().pretty(true);
            exporter.export_conversation(&conversation, &mut writer, &options)?;
        }
        ExportFormatArg::Text => {
            let exporter = MarkdownExporter::new().plain_text(true);
            exporter.export_conversation(&conversation, &mut writer, &options)?;
        }
        ExportFormatArg::Jsonl => {
            // Export in original JSONL format
            conversation_to_jsonl(&conversation, &mut writer, options.main_thread_only)?;
        }
    }

    writer.flush()?;

    // Print success message to stderr if writing to file
    if args.output.is_some() && !cli.quiet {
        eprintln!(
            "Exported {} entries to {}",
            conversation.len(),
            args.output.as_ref().unwrap().display()
        );
    }

    Ok(())
}
