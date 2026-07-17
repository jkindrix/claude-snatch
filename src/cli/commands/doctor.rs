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
    if !args.provider.is_empty() {
        return provider_diagnostics(cli, args);
    }

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

/// Provider diagnostics (`doctor --provider ...`): each selected provider's
/// own drift/health report through the provider-neutral hook. Providers
/// without dedicated diagnostics say so (the classic scan covers Claude).
/// Reports contain aggregate vocabulary only — providers cap and escape
/// native strings during collection, and no session ids or file paths are
/// emitted (round-16/17 security guardrail).
fn provider_diagnostics(cli: &Cli, args: &DoctorArgs) -> Result<()> {
    use crate::provider::registry::ProviderSelection;

    // COMPLETE argument classification: destructured WITHOUT `..` so a new
    // DoctorArgs field must be classified here to compile (round-19).
    // --all is universal (provider diagnostics always scan the full
    // corpus); project/date/subagent filters are refused.
    let DoctorArgs {
        project,
        since,
        all: _,
        subagents,
        provider: _,
    } = args;
    helpers::refuse_unsupported_flags(
        "doctor --provider (full-corpus provider diagnostics)",
        &[
            ("project", project.is_some()),
            ("--since", since.is_some()),
            ("--subagents", *subagents),
        ],
    )?;

    let selection = ProviderSelection::from_flags(&args.provider).map_err(|reason| {
        crate::error::SnatchError::InvalidArgument {
            name: "--provider".to_string(),
            reason,
        }
    })?;
    let registry = helpers::provider_registry(cli);
    // Thin renderer: runtime-failure semantics (atomic vs partial, zero-
    // success error) are enforced centrally in the registry (round-19).
    // Unavailability and failure details are withheld here — doctor output
    // promises no filesystem paths; `snatch providers` carries details.
    let collected = registry.collect_selected_diagnostics(&selection)?;

    let mut reports = serde_json::Map::new();
    for (id, value) in collected.items {
        let value = value.unwrap_or_else(|| {
            serde_json::json!({
                "note": "no dedicated provider diagnostics; the classic `snatch doctor` scan covers this provider"
            })
        });
        reports.insert(id.to_string(), value);
    }
    for (id, _) in &collected.skipped {
        reports.insert(
            id.to_string(),
            serde_json::json!({
                "unavailable": "provider unavailable or diagnostics failed (details withheld; run `snatch providers`)"
            }),
        );
    }

    if cli.effective_output() == OutputFormat::Json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::Value::Object(reports))?
        );
        return Ok(());
    }

    for (id, value) in &reports {
        println!("{id}");
        if let Some(reason) = value.get("unavailable").and_then(|v| v.as_str()) {
            println!("  unavailable: {reason}");
            continue;
        }
        if let Some(err) = value.get("error").and_then(|v| v.as_str()) {
            println!("  {err}");
            continue;
        }
        if let Some(note) = value.get("note").and_then(|v| v.as_str()) {
            println!("  {note}");
            continue;
        }
        // Compact counter summary; full detail via -o json.
        for (label, key) in [
            ("sessions scanned", "sessions"),
            ("legacy sessions", "legacy_sessions"),
            ("unreadable sessions", "unreadable_sessions"),
            ("records", "records"),
            ("unparseable records", "unparseable"),
            ("active tails (transient)", "active_tails"),
            ("schema-checked records", "field_schema_checked_records"),
            (
                "missing payload discriminators",
                "missing_payload_discriminators",
            ),
            ("vocabulary keys dropped at cap", "vocabulary_keys_dropped"),
            ("vocabulary keys truncated", "vocabulary_keys_truncated"),
        ] {
            if let Some(n) = value.get(key).and_then(serde_json::Value::as_u64) {
                println!("  {label}: {n}");
            }
        }
        for (label, key) in [
            ("unknown envelope types", "unknown_envelope_types"),
            ("unknown response_item types", "unknown_response_item_types"),
            ("unknown event_msg types", "unknown_event_msg_types"),
            ("unknown field paths", "unknown_field_paths"),
            ("unbaselined payload variants", "unbaselined_payload_types"),
        ] {
            if let Some(map) = value.get(key).and_then(|v| v.as_object()) {
                let total: u64 = map.values().filter_map(serde_json::Value::as_u64).sum();
                println!("  {label}: {} distinct ({total} records)", map.len());
            }
        }
        if let Some(months) = value.get("reasoning_by_month").and_then(|v| v.as_object()) {
            if !months.is_empty() {
                println!("  reasoning summary availability by month (with/total):");
                for (month, pair) in months {
                    if let Some(arr) = pair.as_array() {
                        let total = arr.first().and_then(serde_json::Value::as_u64).unwrap_or(0);
                        let with = arr.get(1).and_then(serde_json::Value::as_u64).unwrap_or(0);
                        println!("    {month}: {with}/{total}");
                    }
                }
            }
        }
    }
    Ok(())
}
