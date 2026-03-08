//! Lessons command implementation.
//!
//! Extracts error→fix pairs and user corrections from a session,
//! targeting compaction failure modes F2 (negative result amnesia)
//! and F4 (operational gotcha amnesia).
//!
//! Supports single-session mode (session_id) or cross-session mode
//! (--project or --all) for aggregated lesson extraction.

use std::collections::HashMap;

use crate::analysis::lessons::{extract_lessons, LessonCategory, LessonOptions, LessonResult};
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
    session_id: Option<String>,
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
    session_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    timestamp: Option<String>,
    user_text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    prior_assistant_summary: Option<String>,
}

/// Aggregated output for cross-session lessons.
#[derive(serde::Serialize)]
struct AggregatedLessonsOutput {
    sessions_scanned: usize,
    sessions_with_lessons: usize,
    summary: LessonsOutputSummary,
    error_fix_pairs: Vec<ErrorFixOutput>,
    user_corrections: Vec<CorrectionOutput>,
}

/// Run the lessons command.
pub fn run(cli: &Cli, args: &LessonsArgs) -> Result<()> {
    // Determine mode: single-session vs cross-session
    if args.project.is_some() || args.all {
        return run_cross_session(cli, args);
    }

    let session_id = args.session_id.as_ref().ok_or_else(|| SnatchError::InvalidArgument {
        name: "session_id".to_string(),
        reason: "session ID is required unless --project or --all is specified".to_string(),
    })?;

    let claude_dir = get_claude_dir(cli.claude_dir.as_ref())?;
    let session = claude_dir
        .find_session(session_id)?
        .ok_or_else(|| SnatchError::SessionNotFound {
            session_id: session_id.clone(),
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
                session_id: session_id.clone(),
                summary: LessonsOutputSummary {
                    total_errors: result.summary.total_errors,
                    total_corrections: result.summary.total_corrections,
                    most_error_prone_tools: result.summary.most_error_prone_tools,
                },
                error_fix_pairs: result
                    .error_fix_pairs
                    .into_iter()
                    .map(|p| ErrorFixOutput {
                        session_id: None,
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
                        session_id: None,
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
                println!("No lessons found in session {}.", session_id);
                return Ok(());
            }

            println!(
                "Lessons for session {} ({} errors, {} corrections)\n",
                session_id,
                result.summary.total_errors,
                result.summary.total_corrections,
            );

            print_text_lessons(&result);
        }
    }

    Ok(())
}

/// Run cross-session lessons extraction.
fn run_cross_session(cli: &Cli, args: &LessonsArgs) -> Result<()> {
    let claude_dir = get_claude_dir(cli.claude_dir.as_ref())?;

    let sessions = if let Some(ref project_filter) = args.project {
        let projects = claude_dir.projects()?;
        let matched = super::helpers::filter_projects(projects, project_filter);
        let mut sess = Vec::new();
        for project in matched {
            sess.extend(project.sessions()?);
        }
        sess
    } else {
        claude_dir.all_sessions()?
    };

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

    let mut all_error_pairs = Vec::new();
    let mut all_corrections = Vec::new();
    let mut tool_error_counts: HashMap<String, usize> = HashMap::new();
    let mut sessions_with_lessons = 0;

    let show_progress =
        sessions.len() > 10 && std::io::stderr().is_terminal() && !cli.quiet;
    let progress = if show_progress {
        use indicatif::{ProgressBar, ProgressStyle};
        let pb = ProgressBar::new(sessions.len() as u64);
        pb.set_style(
            ProgressStyle::default_bar()
                .template(
                    "{spinner:.cyan} [{bar:40.cyan/dim}] {pos}/{len} sessions ({eta} remaining)",
                )
                .unwrap()
                .progress_chars("█▓░"),
        );
        Some(pb)
    } else {
        None
    };

    use std::io::IsTerminal;

    for session in &sessions {
        if let Some(ref pb) = progress {
            pb.inc(1);
        }
        let entries = match session.parse_with_options(cli.max_file_size) {
            Ok(e) => e,
            Err(_) => continue,
        };
        let conversation = match Conversation::from_entries(entries) {
            Ok(c) => c,
            Err(_) => continue,
        };
        let all_entries = conversation.chronological_entries();
        let entry_refs: Vec<&_> = all_entries.iter().map(|e| *e).collect();

        let result = extract_lessons(&entry_refs, &opts);

        if result.error_fix_pairs.is_empty() && result.user_corrections.is_empty() {
            continue;
        }
        sessions_with_lessons += 1;

        let sid = session.session_id().to_string();

        // Accumulate tool error counts
        for (tool, count) in &result.summary.most_error_prone_tools {
            *tool_error_counts.entry(tool.clone()).or_insert(0) += count;
        }

        for p in result.error_fix_pairs {
            all_error_pairs.push(ErrorFixOutput {
                session_id: Some(sid.clone()),
                timestamp: p.timestamp,
                tool_name: p.tool_name,
                input_summary: p.input_summary,
                error_preview: p.error_preview,
                resolution_summary: p.resolution_summary,
                resolution_tools: p.resolution_tools,
            });
        }
        for c in result.user_corrections {
            all_corrections.push(CorrectionOutput {
                session_id: Some(sid.clone()),
                timestamp: c.timestamp,
                user_text: c.user_text,
                prior_assistant_summary: c.prior_assistant_summary,
            });
        }
    }

    if let Some(pb) = progress {
        pb.finish_and_clear();
    }

    let mut most_error_prone: Vec<(String, usize)> = tool_error_counts.into_iter().collect();
    most_error_prone.sort_by(|a, b| b.1.cmp(&a.1));

    match cli.effective_output() {
        OutputFormat::Json => {
            let output = AggregatedLessonsOutput {
                sessions_scanned: sessions.len(),
                sessions_with_lessons,
                summary: LessonsOutputSummary {
                    total_errors: all_error_pairs.len(),
                    total_corrections: all_corrections.len(),
                    most_error_prone_tools: most_error_prone,
                },
                error_fix_pairs: all_error_pairs,
                user_corrections: all_corrections,
            };
            println!("{}", serde_json::to_string_pretty(&output)?);
        }
        _ => {
            if all_error_pairs.is_empty() && all_corrections.is_empty() {
                println!(
                    "No lessons found across {} sessions.",
                    sessions.len()
                );
                return Ok(());
            }

            println!(
                "Lessons across {} sessions ({} with lessons, {} errors, {} corrections)\n",
                sessions.len(),
                sessions_with_lessons,
                all_error_pairs.len(),
                all_corrections.len(),
            );

            if !all_error_pairs.is_empty() {
                println!("Error -> Fix Pairs:");
                println!("{}", "-".repeat(60));
                for (i, pair) in all_error_pairs.iter().enumerate() {
                    let sid = pair
                        .session_id
                        .as_deref()
                        .map(|s| &s[..8.min(s.len())])
                        .unwrap_or("?");
                    println!("  {}. [{}] [{}] {}", i + 1, sid, pair.tool_name, pair.error_preview);
                    if let Some(ref resolution) = pair.resolution_summary {
                        println!("     Fix: {resolution}");
                    }
                    if !pair.resolution_tools.is_empty() {
                        println!("     Tools: {}", pair.resolution_tools.join(", "));
                    }
                    println!();
                }
            }

            if !all_corrections.is_empty() {
                println!("User Corrections:");
                println!("{}", "-".repeat(60));
                for (i, correction) in all_corrections.iter().enumerate() {
                    let sid = correction
                        .session_id
                        .as_deref()
                        .map(|s| &s[..8.min(s.len())])
                        .unwrap_or("?");
                    println!("  {}. [{}] {}", i + 1, sid, correction.user_text);
                    if let Some(ref prior) = correction.prior_assistant_summary {
                        println!("     After: {prior}");
                    }
                    println!();
                }
            }

            if !most_error_prone.is_empty() {
                println!("Most Error-Prone Tools:");
                for (tool, count) in &most_error_prone {
                    println!("  {tool}: {count} error(s)");
                }
            }
        }
    }

    Ok(())
}

/// Print lessons in text format (shared between single and cross-session modes).
fn print_text_lessons(result: &LessonResult) {
    if !result.error_fix_pairs.is_empty() {
        println!("Error -> Fix Pairs:");
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
