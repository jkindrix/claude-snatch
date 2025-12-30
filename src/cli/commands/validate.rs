//! Validate command implementation.
//!
//! Validates session files for schema compliance and data integrity.

use crate::cli::{Cli, OutputFormat, ValidateArgs};
use crate::error::{Result, SnatchError};
use crate::model::{LogEntry, SchemaVersion};
use crate::parser::JsonlParser;
use crate::reconstruction::Conversation;

use super::get_claude_dir;

/// Run the validate command.
pub fn run(cli: &Cli, args: &ValidateArgs) -> Result<()> {
    let claude_dir = get_claude_dir(cli.claude_dir.as_ref())?;

    let sessions = if args.all {
        claude_dir.all_sessions()?
    } else if let Some(session_id) = &args.session {
        let session = claude_dir
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
            println!("{}", serde_json::to_string_pretty(&ValidationReport {
                sessions_validated: all_results.len(),
                total_errors,
                total_warnings,
                results: all_results,
            })?);
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
