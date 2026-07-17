//! Lessons command implementation.
//!
//! Extracts error→fix pairs and user corrections from a session,
//! targeting compaction failure modes F2 (negative result amnesia)
//! and F4 (operational gotcha amnesia).
//!
//! Supports single-session mode (session_id) or cross-session mode
//! (--project or --all) for aggregated lesson extraction.

use std::collections::HashMap;

use crate::analysis::lessons::{
    extract_lessons_from_conversation, LessonCategory, LessonOptions, LessonResult,
};
use crate::cli::{Cli, LessonsArgs, OutputFormat};
use crate::error::{Result, SnatchError};
use crate::reconstruction::Conversation;

use super::get_claude_dir;

/// JSON output types for serialization.
#[derive(serde::Serialize)]
struct LessonsOutput {
    session_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    provider: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    qualified_id: Option<String>,
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
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    providers: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    project_keys: Vec<String>,
    sessions_scanned: usize,
    sessions_with_lessons: usize,
    summary: LessonsOutputSummary,
    error_fix_pairs: Vec<ErrorFixOutput>,
    user_corrections: Vec<CorrectionOutput>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    skipped_providers: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    warnings: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    activity_basis: Option<String>,
}

/// Run the lessons command.
pub fn run(cli: &Cli, args: &LessonsArgs) -> Result<()> {
    // Determine mode: single-session vs cross-session
    if args.project.is_some() || args.all {
        let qualified_request = args.session_id.as_ref().is_some_and(|session_id| {
            super::helpers::provider_registry(cli).looks_qualified(session_id)
        });
        if !args.provider.is_empty() {
            if qualified_request {
                return Err(SnatchError::InvalidArgument {
                    name: "session_id".to_string(),
                    reason: "a session id cannot be combined with a project/all provider union"
                        .to_string(),
                });
            }
            return run_provider_cross_session(cli, args);
        }
        if qualified_request {
            return Err(SnatchError::InvalidArgument {
                name: "session_id".to_string(),
                reason: "a qualified session id cannot be combined with --project/--all"
                    .to_string(),
            });
        }
        return run_cross_session(cli, args);
    }

    let session_id = args
        .session_id
        .as_ref()
        .ok_or_else(|| SnatchError::InvalidArgument {
            name: "session_id".to_string(),
            reason: "session ID is required unless --project or --all is specified".to_string(),
        })?;

    let registry = super::helpers::provider_registry(cli);
    let provider_route = !args.provider.is_empty() || registry.looks_qualified(session_id);
    let (conversation, display_id, provider, qualified_id, semantic_annotations) =
        if provider_route {
            let resolution = registry.resolve_with_default_policy(&args.provider, session_id)?;
            let semantic_annotations = resolution.provider.capabilities().semantic_annotations;
            let parsed = crate::provider::registry::cached_parsed_session(
                crate::cache::global_cache(),
                resolution.provider,
                &resolution.key,
            )?;
            (
                Conversation::from_parsed_session(parsed)?,
                resolution.key.to_string(),
                Some(resolution.key.provider.to_string()),
                Some(resolution.key.to_string()),
                semantic_annotations,
            )
        } else {
            let claude_dir = get_claude_dir(cli.claude_dir.as_ref())?;
            let session = claude_dir.find_session(session_id)?.ok_or_else(|| {
                SnatchError::SessionNotFound {
                    session_id: session_id.clone(),
                }
            })?;
            let entries = session.parse_with_options(cli.max_file_size)?;
            (
                Conversation::from_entries(entries)?,
                session_id.clone(),
                None,
                None,
                false,
            )
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

    let result = extract_lessons_from_conversation(&conversation, &opts, semantic_annotations);

    match cli.effective_output() {
        OutputFormat::Json => {
            let output = LessonsOutput {
                session_id: display_id.clone(),
                provider,
                qualified_id,
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
                println!("No lessons found in session {}.", display_id);
                return Ok(());
            }

            println!(
                "Lessons for session {} ({} errors, {} corrections)\n",
                display_id, result.summary.total_errors, result.summary.total_corrections,
            );

            print_text_lessons(&result);
        }
    }

    Ok(())
}

/// Run cross-session lessons extraction.
fn run_provider_cross_session(cli: &Cli, args: &LessonsArgs) -> Result<()> {
    use crate::provider::project::{history_units, new_activity_entries};
    use crate::provider::registry::{cached_parsed_session, ProviderSelection};

    let selection = ProviderSelection::from_flags(&args.provider).map_err(|reason| {
        SnatchError::InvalidArgument {
            name: "--provider".to_string(),
            reason,
        }
    })?;
    let atomic = matches!(selection, ProviderSelection::Explicit(_));
    let registry = super::helpers::provider_registry(cli);
    let mut collected = registry.collect_project_union(&selection)?;

    if let Some(filter) = &args.project {
        collected.projects.retain(|project| project.matches(filter));
    }
    let project_keys: Vec<String> = collected
        .projects
        .iter()
        .map(|project| project.identity.to_string())
        .collect();
    let providers: std::collections::BTreeSet<String> = collected
        .projects
        .iter()
        .flat_map(|project| project.providers.iter().map(ToString::to_string))
        .collect();

    let category = args
        .category
        .as_deref()
        .map(LessonCategory::from_str_loose)
        .unwrap_or(LessonCategory::All);
    let opts = LessonOptions {
        category,
        // Cross-session limit is global, not multiplied by session count.
        limit: usize::MAX,
        ..LessonOptions::default()
    };

    let mut all_error_pairs = Vec::new();
    let mut all_corrections = Vec::new();
    let mut tool_error_counts: HashMap<String, usize> = HashMap::new();
    let mut sessions_scanned = 0_usize;
    let mut sessions_with_lessons = 0_usize;
    let mut total_errors = 0_usize;
    let mut total_corrections = 0_usize;
    let mut warnings: Vec<String> = collected
        .context_warnings
        .iter()
        .map(|warning| format!("{}: project metadata unavailable", warning.key))
        .collect();

    for project in &collected.projects {
        for unit in history_units(project, &collected.lineage) {
            let mut result = None;
            let mut parse_error = None;
            if unit.members.len() == 1 {
                let key = &unit.members[0];
                let provider = match registry.get(&key.provider) {
                    Ok(provider) => provider,
                    Err(error) => {
                        continue_or_record_failure(
                            atomic,
                            &unit.root,
                            &mut warnings,
                            &error.to_string(),
                        )?;
                        continue;
                    }
                };
                match cached_parsed_session(crate::cache::global_cache(), provider, key) {
                    Ok(parsed) => match Conversation::from_parsed_session(parsed) {
                        Ok(conversation) => {
                            result = Some(extract_lessons_from_conversation(
                                &conversation,
                                &opts,
                                provider.capabilities().semantic_annotations,
                            ));
                        }
                        Err(error) => parse_error = Some(error.to_string()),
                    },
                    Err(error) => parse_error = Some(error.to_string()),
                }
            } else {
                let mut entries = Vec::new();
                for key in &unit.members {
                    let provider = match registry.get(&key.provider) {
                        Ok(provider) => provider,
                        Err(error) => {
                            parse_error = Some(error.to_string());
                            break;
                        }
                    };
                    match cached_parsed_session(crate::cache::global_cache(), provider, key) {
                        Ok(parsed) => entries.extend(new_activity_entries(&parsed)),
                        Err(error) => {
                            parse_error = Some(error.to_string());
                            break;
                        }
                    }
                }
                if parse_error.is_none() {
                    match Conversation::from_entries(entries) {
                        Ok(conversation) => {
                            // Current continuation provider is Claude and
                            // deliberately uses classic heuristics. A future
                            // semantic continuation provider must gain a
                            // complete-bundle merger before this branch may
                            // claim semantic coverage.
                            result = Some(extract_lessons_from_conversation(
                                &conversation,
                                &opts,
                                false,
                            ));
                        }
                        Err(error) => parse_error = Some(error.to_string()),
                    }
                }
            }
            if let Some(error) = parse_error {
                if atomic {
                    return Err(SnatchError::InvalidArgument {
                        name: unit.root.to_string(),
                        reason: format!("selected session could not be parsed: {error}"),
                    });
                }
                warnings.push(format!("{}: parse failed", unit.root));
                continue;
            }
            let Some(result) = result else { continue };
            sessions_scanned += 1;
            total_errors = total_errors.saturating_add(result.summary.total_errors);
            total_corrections = total_corrections.saturating_add(result.summary.total_corrections);
            if result.error_fix_pairs.is_empty() && result.user_corrections.is_empty() {
                continue;
            }
            sessions_with_lessons += 1;
            for (tool, count) in &result.summary.most_error_prone_tools {
                *tool_error_counts.entry(tool.clone()).or_default() += count;
            }
            let qualified = unit.root.to_string();
            all_error_pairs.extend(
                result
                    .error_fix_pairs
                    .into_iter()
                    .map(|pair| ErrorFixOutput {
                        session_id: Some(qualified.clone()),
                        timestamp: pair.timestamp,
                        tool_name: pair.tool_name,
                        input_summary: pair.input_summary,
                        error_preview: pair.error_preview,
                        resolution_summary: pair.resolution_summary,
                        resolution_tools: pair.resolution_tools,
                    }),
            );
            all_corrections.extend(result.user_corrections.into_iter().map(|correction| {
                CorrectionOutput {
                    session_id: Some(qualified.clone()),
                    timestamp: correction.timestamp,
                    user_text: correction.user_text,
                    prior_assistant_summary: correction.prior_assistant_summary,
                }
            }));
        }
    }

    all_error_pairs.sort_by(|a, b| a.timestamp.cmp(&b.timestamp));
    all_corrections.sort_by(|a, b| a.timestamp.cmp(&b.timestamp));
    all_error_pairs.truncate(args.limit);
    all_corrections.truncate(args.limit);
    let mut most_error_prone: Vec<(String, usize)> = tool_error_counts.into_iter().collect();
    most_error_prone.sort_by_key(|entry| std::cmp::Reverse(entry.1));
    let skipped_providers = collected
        .skipped
        .iter()
        .map(|(provider, _)| format!("{provider}: unavailable"))
        .collect();

    if cli.effective_output() == OutputFormat::Json {
        let output = AggregatedLessonsOutput {
            providers: providers.into_iter().collect(),
            project_keys,
            sessions_scanned,
            sessions_with_lessons,
            summary: LessonsOutputSummary {
                total_errors,
                total_corrections,
                most_error_prone_tools: most_error_prone,
            },
            error_fix_pairs: all_error_pairs,
            user_corrections: all_corrections,
            skipped_providers,
            warnings,
            activity_basis: Some("new-activity-only".into()),
        };
        println!("{}", serde_json::to_string_pretty(&output)?);
        return Ok(());
    }

    if all_error_pairs.is_empty() && all_corrections.is_empty() {
        println!("No lessons found across {sessions_scanned} sessions.");
        for skipped in skipped_providers {
            eprintln!("warning: {skipped}");
        }
        for warning in warnings {
            eprintln!("warning: {warning}");
        }
        return Ok(());
    }
    println!(
        "Lessons across {sessions_scanned} sessions ({sessions_with_lessons} with lessons, \
         {total_errors} errors, {total_corrections} corrections)\n"
    );
    if !all_error_pairs.is_empty() {
        println!("Error -> Fix Pairs:\n{}", "-".repeat(60));
        for (index, pair) in all_error_pairs.iter().enumerate() {
            println!(
                "  {}. [{}] [{}] {}",
                index + 1,
                pair.session_id.as_deref().unwrap_or("?"),
                pair.tool_name,
                pair.error_preview
            );
            if let Some(resolution) = &pair.resolution_summary {
                println!("     Fix: {resolution}");
            }
            println!();
        }
    }
    if !all_corrections.is_empty() {
        println!("User Corrections:\n{}", "-".repeat(60));
        for (index, correction) in all_corrections.iter().enumerate() {
            println!(
                "  {}. [{}] {}",
                index + 1,
                correction.session_id.as_deref().unwrap_or("?"),
                correction.user_text
            );
        }
    }
    if !most_error_prone.is_empty() {
        println!("Most Error-Prone Tools:");
        for (tool, count) in most_error_prone {
            println!("  {tool}: {count} error(s)");
        }
    }
    for skipped in skipped_providers {
        eprintln!("warning: {skipped}");
    }
    for warning in warnings {
        eprintln!("warning: {warning}");
    }
    Ok(())
}

fn continue_or_record_failure(
    atomic: bool,
    key: &crate::provider::LogicalSessionKey,
    warnings: &mut Vec<String>,
    reason: &str,
) -> Result<()> {
    if atomic {
        return Err(SnatchError::InvalidArgument {
            name: key.to_string(),
            reason: format!("selected session could not be parsed: {reason}"),
        });
    }
    warnings.push(format!("{key}: parse failed"));
    Ok(())
}

/// Run classic Claude-only cross-session lessons extraction.
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

    let show_progress = sessions.len() > 10 && std::io::stderr().is_terminal() && !cli.quiet;
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
        let result = extract_lessons_from_conversation(&conversation, &opts, false);

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
    most_error_prone.sort_by_key(|b| std::cmp::Reverse(b.1));

    match cli.effective_output() {
        OutputFormat::Json => {
            let output = AggregatedLessonsOutput {
                providers: Vec::new(),
                project_keys: Vec::new(),
                sessions_scanned: sessions.len(),
                sessions_with_lessons,
                summary: LessonsOutputSummary {
                    total_errors: all_error_pairs.len(),
                    total_corrections: all_corrections.len(),
                    most_error_prone_tools: most_error_prone,
                },
                error_fix_pairs: all_error_pairs,
                user_corrections: all_corrections,
                skipped_providers: Vec::new(),
                warnings: Vec::new(),
                activity_basis: None,
            };
            println!("{}", serde_json::to_string_pretty(&output)?);
        }
        _ => {
            if all_error_pairs.is_empty() && all_corrections.is_empty() {
                println!("No lessons found across {} sessions.", sessions.len());
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
                    println!(
                        "  {}. [{}] [{}] {}",
                        i + 1,
                        sid,
                        pair.tool_name,
                        pair.error_preview
                    );
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
