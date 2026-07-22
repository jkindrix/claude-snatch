//! File history command implementation.
//!
//! Shows which sessions modified a given file path.

use std::io::Write;

use crate::cli::{Cli, OutputFormat};
use crate::error::Result;
use crate::file_index::FileIndex;
use crate::file_index::{ProviderFileIndexBuilder, ProviderFileModification};
use crate::util::pager::PagerWriter;

use super::get_claude_dir;

/// Arguments for the file-history command.
#[derive(Debug, Clone, clap::Args)]
pub struct FileHistoryArgs {
    /// File path to look up (substring match).
    pub path: String,

    /// Filter by project (substring match on path).
    #[arg(short, long)]
    pub project: Option<String>,

    /// Maximum results to show.
    #[arg(short, long, default_value = "50")]
    pub limit: usize,

    /// Source providers to search (repeatable; "all" = every installed
    /// provider). Omit for the classic Claude-only route.
    #[arg(long = "provider", value_name = "PROVIDER")]
    pub provider: Vec<String>,
}

/// Run the file-history command.
pub fn run(cli: &Cli, args: &FileHistoryArgs) -> Result<()> {
    if !args.provider.is_empty() {
        return run_provider(cli, args);
    }
    let claude_dir = get_claude_dir(cli.claude_dir.as_ref())?;
    let mut writer = PagerWriter::new(false);

    // Collect sessions to index
    let projects = claude_dir.projects()?;
    let mut sessions = Vec::new();
    for project in &projects {
        if let Some(ref filter) = args.project {
            if !project.best_path().contains(filter) {
                continue;
            }
        }
        if let Ok(s) = project.sessions() {
            sessions.extend(s);
        }
    }

    let index = FileIndex::from_sessions(&sessions, cli.max_file_size);

    // Search for matching files
    let mut matches = index.search(&args.path);
    matches.sort_by_key(|(path, _)| path.to_string());

    if matches.is_empty() {
        writeln!(
            writer,
            "No sessions found that modified files matching '{}'.",
            args.path
        )?;
        writer.finish()?;
        return Ok(());
    }

    match cli.effective_output() {
        OutputFormat::Json => {
            let output: Vec<serde_json::Value> = matches
                .iter()
                .flat_map(|(path, mods)| {
                    mods.iter().take(args.limit).map(move |m| {
                        serde_json::json!({
                            "file_path": path,
                            "session_id": m.session_id,
                            "project_path": m.project_path,
                            "message_id": m.message_id,
                            "timestamp": m.timestamp.to_rfc3339(),
                            "version": m.version,
                        })
                    })
                })
                .take(args.limit)
                .collect();
            writeln!(writer, "{}", serde_json::to_string_pretty(&output)?)?;
        }
        _ => {
            let total_files = matches.len();
            let total_mods: usize = matches.iter().map(|(_, m)| m.len()).sum();
            writeln!(
                writer,
                "Files matching '{}': {} ({} modifications)",
                args.path, total_files, total_mods
            )?;
            writeln!(writer)?;

            let mut shown = 0;
            for (path, mods) in &matches {
                if shown >= args.limit {
                    break;
                }
                writeln!(writer, "  {path}")?;
                for m in mods.iter().take(args.limit - shown) {
                    let ts = m.timestamp.format("%Y-%m-%d %H:%M UTC");
                    writeln!(
                        writer,
                        "    {} v{} ({}) [{}]",
                        &m.session_id[..8.min(m.session_id.len())],
                        m.version,
                        ts,
                        &m.message_id[..8.min(m.message_id.len())],
                    )?;
                    shown += 1;
                }
                writeln!(writer)?;
            }
        }
    }

    writer.finish()?;
    Ok(())
}

fn provider_change_json(file_path: &str, change: &ProviderFileModification) -> serde_json::Value {
    serde_json::json!({
        "file_path": file_path,
        "move_path": change.move_path,
        "provider": change.session.provider.to_string(),
        "qualified_id": change.session.to_string(),
        "session_id": change.session.native_id,
        "project_path": change.project_path,
        "entry_id": change.entry_id.to_string(),
        "operation_id": change.operation_id,
        "timestamp": change.timestamp.map(|timestamp| timestamp.to_rfc3339()),
        "version": change.version,
        "kind": change.kind.as_str(),
        "coverage": change.coverage,
        "evidence": change.evidence.as_str(),
        "outcome": change.outcome.as_str(),
        "record_ordinal": change.record.ordinal,
        "outcome_record_ordinal": change.outcome_record.as_ref().map(|record| record.ordinal),
    })
}

fn run_provider(cli: &Cli, args: &FileHistoryArgs) -> Result<()> {
    use crate::provider::registry::ProviderSelection;
    use crate::provider::FileChangeOutcome;

    let selection = ProviderSelection::from_flags(&args.provider)
        .map_err(crate::provider::ProviderError::Other)?;
    let registry = super::helpers::provider_registry(cli);
    let mut builder = ProviderFileIndexBuilder::default();
    let collected = registry.visit_project_file_changes(
        &selection,
        args.project.as_deref(),
        false,
        |project_path, descriptor, projection| {
            builder.add_projection(project_path, &descriptor.key, &projection);
        },
    )?;
    let index = builder.build();
    let matches = index.search_limited(&args.path, args.limit);
    let mut applied = Vec::new();
    let mut attempts = Vec::new();
    for (path, change) in &matches.selected {
        if change.outcome == FileChangeOutcome::Applied {
            applied.push((*path, *change));
        } else {
            attempts.push((*path, *change));
        }
    }

    let mut writer = PagerWriter::new(false);
    match cli.effective_output() {
        OutputFormat::Json => {
            let output = serde_json::json!({
                "path_query": args.path,
                "total_files": matches.total_files,
                "total_modifications": matches.total_modifications,
                "total_attempts": matches.total_attempts,
                "returned": applied.len() + attempts.len(),
                "modifications": applied.iter().map(|(path, change)| provider_change_json(path, change)).collect::<Vec<_>>(),
                "attempts": attempts.iter().map(|(path, change)| provider_change_json(path, change)).collect::<Vec<_>>(),
                "skipped_providers": collected.skipped.iter().map(|(provider, _)| format!("{provider}: unavailable")).collect::<Vec<_>>(),
                "warnings": collected.warnings,
                "coverage_note": "Structured patch/snapshot evidence only; arbitrary shell writes are not inferred.",
            });
            writeln!(writer, "{}", serde_json::to_string_pretty(&output)?)?;
        }
        _ => {
            if matches.total_files == 0 {
                writeln!(
                    writer,
                    "No source-backed file-change evidence matches '{}'.",
                    args.path
                )?;
            } else {
                writeln!(
                    writer,
                    "Files matching '{}': {} ({} applied modifications, {} non-applied attempts)",
                    args.path,
                    matches.total_files,
                    matches.total_modifications,
                    matches.total_attempts
                )?;
                writeln!(writer, "Evidence is bounded to structured patches/snapshots; shell writes are not inferred.")?;
                for (path, change) in &applied {
                    let timestamp = change
                        .timestamp
                        .map(|value| value.format("%Y-%m-%d %H:%M UTC").to_string())
                        .unwrap_or_else(|| "time unavailable".into());
                    let version = change
                        .version
                        .map(|version| format!(" v{version}"))
                        .unwrap_or_default();
                    let moved = change
                        .move_path
                        .as_deref()
                        .map(|destination| format!(" -> {destination}"))
                        .unwrap_or_default();
                    writeln!(
                        writer,
                        "  {path}{moved}: {} {}{} ({timestamp}) [{}]",
                        change.kind.as_str(),
                        change.session,
                        version,
                        change.evidence.as_str(),
                    )?;
                }
                for (path, change) in &attempts {
                    writeln!(
                        writer,
                        "  attempt {path}: {} {} [{}; {}]",
                        change.kind.as_str(),
                        change.session,
                        change.outcome.as_str(),
                        change.evidence.as_str(),
                    )?;
                }
                for (provider, _) in &collected.skipped {
                    writeln!(writer, "  warning: {provider} unavailable")?;
                }
                for warning in &collected.warnings {
                    writeln!(writer, "  warning: {warning}")?;
                }
            }
        }
    }
    writer.finish()?;
    Ok(())
}
