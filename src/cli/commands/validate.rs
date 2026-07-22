//! Validate command implementation.
//!
//! Validates session files for schema compliance and data integrity.

use crate::cli::{Cli, OutputFormat, ValidateArgs};
use crate::error::{Result, SnatchError};
use crate::model::{LogEntry, SchemaVersion};
use crate::parser::JsonlParser;
use crate::provider::registry::ProviderSelection;
use crate::provider::{IngestionDiagnostics, ParsedSession, SourceProvider};
use crate::reconstruction::Conversation;

use super::get_claude_dir;

/// Run the validate command.
pub fn run(cli: &Cli, args: &ValidateArgs) -> Result<()> {
    if !args.provider.is_empty() {
        let registry = super::helpers::provider_registry(cli);
        return run_provider(cli, args, &registry);
    }
    if let Some(session) = args
        .session
        .as_deref()
        .filter(|session| session.contains(':'))
    {
        let registry = super::helpers::provider_registry(cli);
        if registry.looks_qualified(session) {
            return run_provider(cli, args, &registry);
        }
    }

    let claude_dir = get_claude_dir(cli.claude_dir.as_ref())?;

    let sessions = if args.all {
        claude_dir.all_sessions()?
    } else if let Some(session_id) = &args.session {
        let session =
            claude_dir
                .find_session(session_id)?
                .ok_or_else(|| SnatchError::SessionNotFound {
                    session_id: session_id.clone(),
                })?;
        vec![session]
    } else {
        // Validate most recent sessions by default
        let mut sessions = claude_dir.all_sessions()?;
        sessions.truncate(10);
        sessions
    };

    let mut all_results = Vec::new();
    let mut total_errors = 0;
    let mut total_warnings = 0;

    for session in &sessions {
        let result = validate_session(session, args)?;
        total_errors += result.errors.len();
        total_warnings += result.warnings.len();
        all_results.push(result);
    }

    // Output results
    match cli.effective_output() {
        OutputFormat::Json => {
            println!(
                "{}",
                serde_json::to_string_pretty(&ValidationReport {
                    sessions_validated: all_results.len(),
                    total_errors,
                    total_warnings,
                    results: all_results,
                })?
            );
        }
        OutputFormat::Tsv => {
            println!("session\terrors\twarnings\tvalid");
            for result in &all_results {
                println!(
                    "{}\t{}\t{}\t{}",
                    &result.session_id[..8.min(result.session_id.len())],
                    result.errors.len(),
                    result.warnings.len(),
                    result.is_valid
                );
            }
        }
        OutputFormat::Compact => {
            for result in &all_results {
                let status = if result.is_valid { "OK" } else { "FAIL" };
                println!(
                    "{}:{} ({}E/{}W)",
                    &result.session_id[..8.min(result.session_id.len())],
                    status,
                    result.errors.len(),
                    result.warnings.len()
                );
            }
        }
        OutputFormat::Text => {
            println!("Validation Results");
            println!("==================");
            println!();
            println!("Sessions Validated: {}", all_results.len());
            println!("Total Errors:       {total_errors}");
            println!("Total Warnings:     {total_warnings}");
            println!();

            for result in &all_results {
                let status = if result.is_valid { "✓" } else { "✗" };
                println!(
                    "{} {} ({} entries, {}E/{}W)",
                    status,
                    &result.session_id[..8.min(result.session_id.len())],
                    result.entry_count,
                    result.errors.len(),
                    result.warnings.len()
                );

                if !result.errors.is_empty() {
                    for error in &result.errors {
                        println!("    ERROR: {error}");
                    }
                }

                if !result.warnings.is_empty() && !cli.quiet {
                    for warning in &result.warnings {
                        println!("    WARN:  {warning}");
                    }
                }
            }

            println!();
            if total_errors == 0 {
                println!("All sessions validated successfully.");
            } else {
                println!("Validation completed with {} errors.", total_errors);
            }
        }
    }

    if total_errors > 0 {
        std::process::exit(1);
    }

    Ok(())
}

#[derive(Debug, Clone, Default, serde::Serialize)]
struct ProviderIngestionSummary {
    mapped: usize,
    suppressed: usize,
    unknown: usize,
    recovered: usize,
    unparseable: usize,
}

impl From<&IngestionDiagnostics> for ProviderIngestionSummary {
    fn from(value: &IngestionDiagnostics) -> Self {
        Self {
            mapped: value.mapped,
            suppressed: value.suppressed,
            unknown: value.unknown,
            recovered: value.recovered,
            unparseable: value.unparseable,
        }
    }
}

#[derive(Debug, serde::Serialize)]
struct ProviderValidationResult {
    provider: String,
    qualified_id: String,
    entry_count: usize,
    record_count: usize,
    is_valid: bool,
    source_complete: bool,
    provenance_valid: bool,
    diagnostics: ProviderIngestionSummary,
    errors: Vec<String>,
    warnings: Vec<String>,
}

#[derive(Debug, serde::Serialize)]
struct ProviderValidationSkip {
    provider: String,
    reason: String,
}

#[derive(Debug, serde::Serialize)]
struct ProviderValidationReport {
    sessions_validated: usize,
    total_records: usize,
    total_entries: usize,
    total_errors: usize,
    total_warnings: usize,
    results: Vec<ProviderValidationResult>,
    skipped_providers: Vec<ProviderValidationSkip>,
}

fn run_provider(
    cli: &Cli,
    args: &ValidateArgs,
    registry: &crate::provider::registry::ProviderRegistry,
) -> Result<()> {
    // COMPLETE classification: these legacy checks describe Claude's native
    // schema/tree and must never be silently applied to another provider.
    let ValidateArgs {
        session,
        provider: _,
        all,
        schema,
        unknown_fields,
        relationships,
    } = args;
    super::helpers::refuse_unsupported_flags(
        "validate --provider (source and normalized-provenance validation)",
        &[
            ("--schema (use doctor --provider for native drift)", *schema),
            (
                "--unknown-fields (use doctor --provider for native drift)",
                *unknown_fields,
            ),
            (
                "--relationships (use chain --provider for typed lineage)",
                *relationships,
            ),
        ],
    )?;

    if session.is_some() && *all {
        return Err(SnatchError::InvalidArgument {
            name: "validate".to_string(),
            reason: "a session reference cannot be combined with --all".to_string(),
        });
    }
    if session.is_none() && !*all {
        return Err(SnatchError::InvalidArgument {
            name: "validate --provider".to_string(),
            reason: "requires a session reference or --all; provider inventories do not imply a portable recent order"
                .to_string(),
        });
    }

    let mut results = Vec::new();
    let skipped = if let Some(reference) = session {
        let resolution = registry.resolve_with_default_policy(&args.provider, reference)?;
        let key = resolution.key.clone();
        results.push(validate_provider_parse(
            resolution.provider,
            &key,
            Vec::new(),
            resolution.provider.parse(&key),
        ));
        Vec::new()
    } else {
        let selection = ProviderSelection::from_flags(&args.provider).map_err(|reason| {
            SnatchError::InvalidArgument {
                name: "--provider".to_string(),
                reason,
            }
        })?;
        let collected = registry.collect_selected_sessions(&selection)?;
        for descriptor in collected.items {
            let provider = registry.get(&descriptor.key.provider)?;
            let descriptor_violations = descriptor.validate();
            let key = descriptor.key.clone();
            let parsed = provider.parse_discovered(&descriptor);
            results.push(validate_provider_parse(
                provider,
                &key,
                descriptor_violations,
                parsed,
            ));
        }
        collected.skipped
    };

    let skipped_providers = skipped
        .into_iter()
        .map(|(provider, reason)| ProviderValidationSkip {
            provider: provider.to_string(),
            reason,
        })
        .collect::<Vec<_>>();
    let report = ProviderValidationReport {
        sessions_validated: results.len(),
        total_records: results.iter().map(|result| result.record_count).sum(),
        total_entries: results.iter().map(|result| result.entry_count).sum(),
        total_errors: results.iter().map(|result| result.errors.len()).sum(),
        total_warnings: results
            .iter()
            .map(|result| result.warnings.len())
            .sum::<usize>()
            + skipped_providers.len(),
        results,
        skipped_providers,
    };
    render_provider_report(cli, &report)?;
    if report.total_errors > 0 {
        std::process::exit(1);
    }
    Ok(())
}

fn validate_provider_parse(
    provider: &dyn SourceProvider,
    expected_key: &crate::provider::LogicalSessionKey,
    descriptor_violations: Vec<String>,
    parsed: std::result::Result<ParsedSession, crate::provider::ProviderError>,
) -> ProviderValidationResult {
    let mut errors = descriptor_violations
        .into_iter()
        .map(|violation| format!("Descriptor: {violation}"))
        .collect::<Vec<_>>();
    let warnings = Vec::new();
    let parsed = match parsed {
        Ok(parsed) => parsed,
        Err(error) => {
            errors.push(format!("Parse error: {error}"));
            return ProviderValidationResult {
                provider: provider.id().to_string(),
                qualified_id: expected_key.to_string(),
                entry_count: 0,
                record_count: 0,
                is_valid: false,
                source_complete: false,
                provenance_valid: false,
                diagnostics: ProviderIngestionSummary::default(),
                errors,
                warnings,
            };
        }
    };

    if parsed.descriptor.key != *expected_key {
        errors.push(format!(
            "Provider returned session {} while validating {expected_key}",
            parsed.descriptor.key
        ));
    }
    let provenance_violations = parsed.validate_provenance();
    let provenance_valid = provenance_violations.is_empty()
        && parsed.descriptor.key == *expected_key
        && errors.is_empty();
    errors.extend(
        provenance_violations
            .into_iter()
            .map(|violation| format!("Provenance: {violation}")),
    );

    let diagnostics = ProviderIngestionSummary::from(&parsed.diagnostics);
    if diagnostics.recovered > 0 {
        errors.push(format!(
            "{} damaged native record(s) were only partially recovered",
            diagnostics.recovered
        ));
    }
    if diagnostics.unparseable > 0 {
        errors.push(format!(
            "{} native record(s) could not be parsed",
            diagnostics.unparseable
        ));
    }
    let source_complete = diagnostics.recovered == 0 && diagnostics.unparseable == 0;

    ProviderValidationResult {
        provider: provider.id().to_string(),
        qualified_id: expected_key.to_string(),
        entry_count: parsed.entries.len(),
        record_count: parsed.record_dispositions.len(),
        is_valid: errors.is_empty(),
        source_complete,
        provenance_valid,
        diagnostics,
        errors,
        warnings,
    }
}

fn render_provider_report(cli: &Cli, report: &ProviderValidationReport) -> Result<()> {
    match cli.effective_output() {
        OutputFormat::Json => println!("{}", serde_json::to_string_pretty(report)?),
        OutputFormat::Tsv => {
            println!(
                "provider\tqualified_id\trecords\tentries\tmapped\tsuppressed\tunknown\trecovered\tunparseable\tprovenance_valid\tsource_complete\tvalid"
            );
            for result in &report.results {
                println!(
                    "{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}",
                    result.provider,
                    result.qualified_id,
                    result.record_count,
                    result.entry_count,
                    result.diagnostics.mapped,
                    result.diagnostics.suppressed,
                    result.diagnostics.unknown,
                    result.diagnostics.recovered,
                    result.diagnostics.unparseable,
                    result.provenance_valid,
                    result.source_complete,
                    result.is_valid,
                );
            }
        }
        OutputFormat::Compact => {
            for result in &report.results {
                println!(
                    "{}:{} ({}R/{}E/{}W)",
                    result.qualified_id,
                    if result.is_valid { "OK" } else { "FAIL" },
                    result.record_count,
                    result.errors.len(),
                    result.warnings.len()
                );
            }
        }
        OutputFormat::Text => {
            println!("Provider Validation Results");
            println!("===========================");
            println!();
            println!("Sessions Validated: {}", report.sessions_validated);
            println!("Native Records:     {}", report.total_records);
            println!("Normalized Entries: {}", report.total_entries);
            println!("Total Errors:       {}", report.total_errors);
            println!("Total Warnings:     {}", report.total_warnings);
            println!();
            for result in &report.results {
                println!(
                    "{} {} ({} records, {} entries, {}E/{}W)",
                    if result.is_valid { "✓" } else { "✗" },
                    result.qualified_id,
                    result.record_count,
                    result.entry_count,
                    result.errors.len(),
                    result.warnings.len()
                );
                for error in &result.errors {
                    println!("    ERROR: {error}");
                }
                if !cli.quiet {
                    for warning in &result.warnings {
                        println!("    WARN:  {warning}");
                    }
                }
            }
            println!();
            if report.total_errors == 0 {
                println!("All parsed sessions passed source and provenance validation.");
            } else {
                println!(
                    "Validation completed with {} error(s).",
                    report.total_errors
                );
            }
        }
    }

    if cli.effective_output() != OutputFormat::Json && !cli.quiet {
        for skipped in &report.skipped_providers {
            eprintln!(
                "Warning: provider '{}' was skipped: {}",
                skipped.provider, skipped.reason
            );
        }
    }
    Ok(())
}

/// Validate a single session.
fn validate_session(
    session: &crate::discovery::Session,
    args: &ValidateArgs,
) -> Result<ValidationResult> {
    let mut result = ValidationResult {
        session_id: session.session_id().to_string(),
        entry_count: 0,
        is_valid: true,
        errors: Vec::new(),
        warnings: Vec::new(),
        schema_version: None,
        unknown_fields: Vec::new(),
    };

    // Parse the session
    let mut parser = JsonlParser::new().with_lenient(true);
    let entries = match parser.parse_file(session.path()) {
        Ok(entries) => entries,
        Err(e) => {
            result.errors.push(format!("Parse error: {e}"));
            result.is_valid = false;
            return Ok(result);
        }
    };

    result.entry_count = entries.len();

    // Detect schema version
    if args.schema {
        if let Some(first) = entries.first() {
            if let Some(version) = first.version() {
                let schema = SchemaVersion::from_version_string(version);
                result.schema_version = Some(format!("{schema:?}"));
            }
        }
    }

    // Validate relationships
    if args.relationships {
        let conversation = match Conversation::from_entries(entries.clone()) {
            Ok(c) => c,
            Err(e) => {
                result.errors.push(format!("Tree construction error: {e}"));
                result.is_valid = false;
                return Ok(result);
            }
        };

        let stats = conversation.statistics();

        // Check for orphan nodes (should have parents but don't link correctly)
        if conversation.roots().len() > 1 {
            result.warnings.push(format!(
                "Multiple roots detected: {} (may indicate orphan nodes)",
                conversation.roots().len()
            ));
        }

        // Check tool use/result balance
        if !stats.tools_balanced() {
            result.warnings.push(format!(
                "Unbalanced tools: {} uses vs {} results",
                stats.tool_uses, stats.tool_results
            ));
        }

        // Duplicate-UUID diagnostics: content conflicts are real collisions
        // (the dropped entry's data is lost from reconstruction); exact
        // duplicates are benign overlap (e.g. a boundary entry shared by two
        // files in a resume chain).
        let conflicts = conversation.conflicting_duplicates().count();
        let exact = conversation.duplicate_uuids().len() - conflicts;
        if conflicts > 0 {
            result.warnings.push(format!(
                "{conflicts} duplicate-UUID entr{} with differing content dropped (data loss; kept first occurrence)",
                if conflicts == 1 { "y" } else { "ies" }
            ));
        }
        if exact > 0 {
            result.warnings.push(format!(
                "{exact} exact-duplicate UUID entr{} deduplicated (benign overlap)",
                if exact == 1 { "y" } else { "ies" }
            ));
        }
    }

    // Check for unknown fields
    if args.unknown_fields {
        for entry in &entries {
            let unknown = collect_unknown_fields(entry);
            if !unknown.is_empty() {
                result.unknown_fields.extend(unknown);
            }
        }

        if !result.unknown_fields.is_empty() {
            result.warnings.push(format!(
                "Unknown fields detected: {:?}",
                &result.unknown_fields[..result.unknown_fields.len().min(5)]
            ));
        }
    }

    // Basic validation - only check fields for entry types that require them
    for (i, entry) in entries.iter().enumerate() {
        // Only Assistant, User, and System messages require UUIDs
        // Summary, FileHistorySnapshot, QueueOperation, and TurnEnd legitimately lack UUIDs
        let requires_uuid = matches!(
            entry,
            LogEntry::Assistant(_) | LogEntry::User(_) | LogEntry::System(_)
        );

        if requires_uuid && entry.uuid().is_none() {
            result.errors.push(format!(
                "Entry {i} ({}): missing UUID",
                entry.message_type()
            ));
            result.is_valid = false;
        }

        // Only certain entry types have timestamps
        // Summary and FileHistorySnapshot legitimately lack timestamps
        let requires_timestamp = matches!(
            entry,
            LogEntry::Assistant(_)
                | LogEntry::User(_)
                | LogEntry::System(_)
                | LogEntry::QueueOperation(_)
                | LogEntry::TurnEnd(_)
                | LogEntry::Progress(_)
        );

        if requires_timestamp && entry.timestamp().is_none() {
            result.warnings.push(format!(
                "Entry {i} ({}): missing timestamp",
                entry.message_type()
            ));
        }
    }

    Ok(result)
}

/// Collect unknown fields from an entry.
fn collect_unknown_fields(entry: &LogEntry) -> Vec<String> {
    let mut unknown = Vec::new();

    // Serialize and look for extra fields
    if let Ok(value) = serde_json::to_value(entry) {
        if let Some(obj) = value.as_object() {
            if let Some(extra) = obj.get("extra") {
                if let Some(extra_obj) = extra.as_object() {
                    for key in extra_obj.keys() {
                        unknown.push(key.clone());
                    }
                }
            }
        }
    }

    unknown
}

/// Validation result for a session.
#[derive(Debug, serde::Serialize)]
struct ValidationResult {
    session_id: String,
    entry_count: usize,
    is_valid: bool,
    errors: Vec<String>,
    warnings: Vec<String>,
    schema_version: Option<String>,
    unknown_fields: Vec<String>,
}

/// Complete validation report.
#[derive(Debug, serde::Serialize)]
struct ValidationReport {
    sessions_validated: usize,
    total_errors: usize,
    total_warnings: usize,
    results: Vec<ValidationResult>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::fake::{colliding_key, multi_artifact_key, FakeProvider};

    #[test]
    fn generic_validation_rejects_broken_provenance_but_not_preserved_records() {
        let provider = FakeProvider;
        let key = multi_artifact_key();
        let parsed = provider.parse(&key).unwrap();

        let valid = validate_provider_parse(&provider, &key, Vec::new(), Ok(parsed.clone()));
        assert!(valid.is_valid);
        assert!(valid.provenance_valid);
        assert!(valid.source_complete);
        assert_eq!(valid.diagnostics.unknown, 1);
        assert!(valid.warnings.is_empty());

        let mut broken = parsed;
        broken.diagnostics.mapped += 1;
        let invalid = validate_provider_parse(&provider, &key, Vec::new(), Ok(broken));
        assert!(!invalid.is_valid);
        assert!(!invalid.provenance_valid);
        assert!(invalid
            .errors
            .iter()
            .any(|error| error.contains("do not match disposition tallies")));
    }

    #[test]
    fn generic_validation_rejects_a_provider_returning_the_wrong_session() {
        let provider = FakeProvider;
        let parsed = provider.parse(&multi_artifact_key()).unwrap();

        let invalid = validate_provider_parse(&provider, &colliding_key(), Vec::new(), Ok(parsed));
        assert!(!invalid.is_valid);
        assert!(!invalid.provenance_valid);
        assert!(invalid
            .errors
            .iter()
            .any(|error| error.contains("while validating")));
    }
}
