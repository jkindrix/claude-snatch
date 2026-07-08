//! Doctor command: schema-drift diagnostics across sessions.
//!
//! Claude Code's on-disk schema drifts under snatch (new entry types,
//! attachment kinds, subtypes; fields emptying out), and the tolerant parser
//! absorbs it silently. `snatch doctor` reports everything unmodeled — with
//! counts and last-seen dates so fossils are distinguishable from live
//! features — plus the known degradation signals.

use crate::analysis::doctor::{Diagnoser, DoctorReport, DriftSighting};
use crate::cli::{Cli, DoctorArgs, OutputFormat};
use crate::error::Result;
use crate::parser::JsonlParser;

use super::helpers::{self, SessionCollectParams};

/// Run the doctor command.
pub fn run(cli: &Cli, args: &DoctorArgs) -> Result<()> {
    // Bound the scan to the recent past unless told otherwise: drift checking
    // cares about what Claude Code writes *now*, and a full-corpus parse is
    // slow. --all removes the bound; --since overrides it.
    let since = match (&args.since, args.all) {
        (Some(s), _) => Some(s.as_str()),
        (None, true) => None,
        (None, false) => Some("30d"),
    };

    let sessions = helpers::collect_sessions(
        cli,
        &SessionCollectParams {
            session: None,
            project: args.project.as_deref(),
            since,
            until: None,
            recent: None,
            no_subagents: !args.subagents,
        },
    )?;

    let mut diagnoser = Diagnoser::new();
    let mut failed = 0usize;
    for session in &sessions {
        let mut parser = JsonlParser::new().with_lenient(true);
        if let Some(max) = cli.max_file_size {
            parser = parser.with_max_file_size(max);
        }
        match parser.parse_file(session.path()) {
            Ok(entries) => diagnoser.diagnose(session.session_id(), &entries, parser.stats()),
            Err(_) => failed += 1,
        }
    }
    let report = diagnoser.finish();

    match cli.effective_output() {
        OutputFormat::Json => println!("{}", serde_json::to_string_pretty(&report)?),
        _ => print_human(&report, failed, since),
    }
    Ok(())
}

fn fmt_sighting(name: &str, s: &DriftSighting) -> String {
    let last_seen = s
        .last_seen
        .map(|t| t.format("%Y-%m-%d").to_string())
        .unwrap_or_else(|| "unknown".to_string());
    format!(
        "  {name:<28} {:>7}x  in {:>3} session(s)  last seen {last_seen}",
        s.count, s.session_count
    )
}

fn print_human(report: &DoctorReport, failed: usize, since: Option<&str>) {
    let scope = since.map_or_else(|| "entire corpus".to_string(), |s| format!("since {s}"));
    println!("Doctor Report ({scope})");
    println!("========================================");
    println!(
        "Scanned: {} sessions, {} entries",
        report.sessions_scanned, report.entries_scanned
    );
    if failed > 0 {
        println!("Unreadable sessions: {failed}");
    }
    println!();

    println!("Parsing:");
    println!(
        "  Unparsed lines: {} (in {} session(s))",
        report.lines_unparsed, report.sessions_with_unparsed
    );
    println!("  Salvaged entries: {}", report.entries_salvaged);
    println!();

    println!("Thinking:");
    if report.thinking_blocks == 0 {
        println!("  No thinking blocks in scope.");
    } else {
        println!(
            "  {} blocks, {} empty ({:.1}%)",
            report.thinking_blocks,
            report.thinking_blocks_empty,
            report.thinking_empty_pct()
        );
        if report.thinking_blocks_empty == report.thinking_blocks {
            println!(
                "  All thinking text is empty — this Claude Code version does not persist it."
            );
        }
    }
    println!();

    println!("Unknown entry types (no LogEntry variant; preserved as Unknown):");
    if report.unknown_entry_types.is_empty() {
        println!("  (none)");
    } else {
        for (name, s) in &report.unknown_entry_types {
            println!("{}", fmt_sighting(name, s));
        }
    }
    println!();

    println!("System subtypes via the Other catch-all:");
    if report.other_system_subtypes.is_empty() {
        println!("  (none)");
    } else {
        for (name, s) in &report.other_system_subtypes {
            println!("{}", fmt_sighting(name, s));
        }
    }
    println!();

    println!("Unknown content blocks:");
    if report.unknown_content_blocks.is_empty() {
        println!("  (none)");
    } else {
        for (name, s) in &report.unknown_content_blocks {
            println!("{}", fmt_sighting(name, s));
        }
    }
    println!();

    println!("Attachment kinds (marker-only unless marked rendered):");
    if report.attachment_kinds.is_empty() {
        println!("  (none)");
    } else {
        for (name, a) in &report.attachment_kinds {
            let tag = if a.rendered_count == a.sighting.count {
                " [rendered]".to_string()
            } else if a.rendered_count > 0 {
                format!(" [renders {}/{}]", a.rendered_count, a.sighting.count)
            } else {
                String::new()
            };
            println!("{}{tag}", fmt_sighting(name, &a.sighting));
        }
    }
    println!();

    println!("Unpriced models (cost estimates show N/A):");
    if report.unpriced_models.is_empty() {
        println!("  (none)");
    } else {
        for (name, s) in &report.unpriced_models {
            println!("{}", fmt_sighting(name, s));
        }
    }
}
