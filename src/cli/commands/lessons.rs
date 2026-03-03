//! Lessons command implementation.
//!
//! Extracts error→fix pairs and user corrections from a session,
//! targeting compaction failure modes F2 (negative result amnesia)
//! and F4 (operational gotcha amnesia).

use std::collections::HashMap;

use crate::analysis::lessons::{extract_lessons, LessonCategory, LessonOptions};
use crate::cli::{Cli, LessonsArgs, OutputFormat};
use crate::error::{Result, SnatchError};
use crate::reconstruction::Conversation;

use super::get_claude_dir;

/// JSON output types for serialization.
#[derive(serde::Serialize)]
struct LessonsOutput {
    session_id: String,
    summary: LessonsOutputSummary,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    error_fix_pairs: Vec<ErrorFixOutput>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    user_corrections: Vec<CorrectionOutput>,
}

#[derive(serde::Serialize)]
struct LessonsOutputSummary {
    total_errors: usize,
    total_corrections: usize,
    most_error_prone_tools: Vec<(String, usize)>,
}

#[derive(serde::Serialize)]
struct ErrorFixOutput {
    #[serde(skip_serializing_if = "Option::is_none")]
    timestamp: Option<String>,
    tool_name: String,
    input_summary: HashMap<String, String>,
    error_preview: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    resolution_summary: Option<String>,
    resolution_tools: Vec<String>,
}

#[derive(serde::Serialize)]
struct CorrectionOutput {
    #[serde(skip_serializing_if = "Option::is_none")]
    timestamp: Option<String>,
    user_text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    prior_assistant_summary: Option<String>,
}

/// Run the lessons command.
pub fn run(cli: &Cli, args: &LessonsArgs) -> Result<()> {
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

    let category = args
        .category
        .as_deref()
        .map(LessonCategory::from_str_loose)
        .unwrap_or(LessonCategory::All);

    let opts = LessonOptions {
        category,
        limit: args.limit,
        ..LessonOptions::default()
    };

    let result = extract_lessons(&entry_refs, &opts);

    match cli.effective_output() {
        OutputFormat::Json => {
            let output = LessonsOutput {
                session_id: args.session_id.clone(),
                summary: LessonsOutputSummary {
                    total_errors: result.summary.total_errors,
                    total_corrections: result.summary.total_corrections,
                    most_error_prone_tools: result.summary.most_error_prone_tools,
                },
                error_fix_pairs: result
                    .error_fix_pairs
                    .into_iter()
                    .map(|p| ErrorFixOutput {
                        timestamp: p.timestamp,
                        tool_name: p.tool_name,
                        input_summary: p.input_summary,
                        error_preview: p.error_preview,
                        resolution_summary: p.resolution_summary,
                        resolution_tools: p.resolution_tools,
                    })
                    .collect(),
                user_corrections: result
                    .user_corrections
                    .into_iter()
                    .map(|c| CorrectionOutput {
                        timestamp: c.timestamp,
                        user_text: c.user_text,
                        prior_assistant_summary: c.prior_assistant_summary,
                    })
                    .collect(),
            };
            println!("{}", serde_json::to_string_pretty(&output)?);
        }
        _ => {
            // Text output
            if result.error_fix_pairs.is_empty() && result.user_corrections.is_empty() {
                println!("No lessons found in session {}.", args.session_id);
                return Ok(());
            }

            println!(
                "Lessons for session {} ({} errors, {} corrections)\n",
                args.session_id,
                result.summary.total_errors,
                result.summary.total_corrections,
            );

            if !result.error_fix_pairs.is_empty() {
                println!("Error → Fix Pairs:");
                println!("{}", "-".repeat(60));
                for (i, pair) in result.error_fix_pairs.iter().enumerate() {
                    println!("  {}. [{}] {}", i + 1, pair.tool_name, pair.error_preview);
                    if let Some(ref resolution) = pair.resolution_summary {
                        println!("     Fix: {resolution}");
                    }
                    if !pair.resolution_tools.is_empty() {
                        println!("     Tools: {}", pair.resolution_tools.join(", "));
                    }
                    println!();
                }
            }

            if !result.user_corrections.is_empty() {
                println!("User Corrections:");
                println!("{}", "-".repeat(60));
                for (i, correction) in result.user_corrections.iter().enumerate() {
                    println!("  {}. {}", i + 1, correction.user_text);
                    if let Some(ref prior) = correction.prior_assistant_summary {
                        println!("     After: {prior}");
                    }
                    println!();
                }
            }

            if !result.summary.most_error_prone_tools.is_empty() {
                println!("Most Error-Prone Tools:");
                for (tool, count) in &result.summary.most_error_prone_tools {
                    println!("  {tool}: {count} error(s)");
                }
            }
        }
    }

    Ok(())
}
