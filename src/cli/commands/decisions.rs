//! Decisions command implementation.
//!
//! Manage a persistent decision registry for a project. Decisions survive
//! compaction and sessions, enabling design decision tracking.

use crate::cli::{Cli, DecisionsArgs, OutputFormat};
use crate::decisions::{load_decisions, save_decisions, DecisionStatus};
use crate::error::{Result, SnatchError};

use super::get_claude_dir;
use super::helpers::{
    extract_text, has_options_pattern, has_tool_calls, is_affirmative, main_thread_entries,
};

/// Stop words to exclude from title matching in score.
const STOP_WORDS: &[&str] = &[
    "the", "this", "that", "with", "from", "into", "have", "will",
    "been", "were", "they", "them", "their", "what", "when", "where",
    "which", "there", "then", "than", "also", "just", "more", "some",
    "each", "does", "should", "would", "could", "about", "other",
    "take", "make", "like", "over", "only", "very", "after", "before",
];

/// Check if text contains enough significant title keywords.
fn title_matches_text(title_keywords: &[&str], text_lower: &str) -> bool {
    if title_keywords.len() < 2 {
        // Too few keywords to match reliably
        return false;
    }
    title_keywords.iter().all(|w| text_lower.contains(w))
}

/// JSON output for a single decision.
#[derive(serde::Serialize)]
struct DecisionOutput {
    id: u64,
    title: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    description: Option<String>,
    status: String,
    confidence: f64,
    created_at: String,
    updated_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    session_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    superseded_by: Option<u64>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    tags: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    references: Vec<String>,
}

fn to_output(d: &crate::decisions::Decision) -> DecisionOutput {
    DecisionOutput {
        id: d.id,
        title: d.title.clone(),
        description: d.description.clone(),
        status: d.status.to_string(),
        confidence: d.confidence,
        created_at: d.created_at.to_rfc3339(),
        updated_at: d.updated_at.to_rfc3339(),
        session_id: d.session_id.clone(),
        superseded_by: d.superseded_by,
        tags: d.tags.clone(),
        references: d.references.clone(),
    }
}

/// Run the decisions command.
pub fn run(cli: &Cli, args: &DecisionsArgs) -> Result<()> {
    let project_filter = args.project.as_deref().unwrap_or("");
    let project = super::helpers::resolve_single_project(cli, project_filter)?;

    let project_dir = project.path();
    let project_path = project.decoded_path().to_string();

    let operation = args.operation.as_deref().unwrap_or("list");

    match operation {
        "list" => {
            let store = load_decisions(project_dir)?;

            let filtered: Vec<_> = if let Some(ref status_filter) = args.status {
                let status = DecisionStatus::parse(status_filter).ok_or_else(|| {
                    SnatchError::InvalidArgument {
                        name: "status".into(),
                        reason: format!(
                            "Invalid status '{status_filter}'. Use: proposed, confirmed, superseded, abandoned"
                        ),
                    }
                })?;
                store.decisions.iter().filter(|d| d.status == status).collect()
            } else {
                store.decisions.iter().collect()
            };

            let filtered: Vec<_> = if let Some(ref search) = args.search {
                let search_lower = search.to_lowercase();
                filtered.into_iter().filter(|d| {
                    d.title.to_lowercase().contains(&search_lower)
                        || d.description.as_ref().is_some_and(|desc| desc.to_lowercase().contains(&search_lower))
                }).collect()
            } else {
                filtered
            };

            let filtered: Vec<_> = if !args.tag.is_empty() {
                let tag_filters: Vec<String> = args.tag.iter()
                    .flat_map(|t| t.split(',').map(|s| s.trim().to_lowercase()))
                    .collect();
                filtered.into_iter().filter(|d| {
                    tag_filters.iter().any(|tf| d.tags.iter().any(|t| t.to_lowercase().contains(tf.as_str())))
                }).collect()
            } else {
                filtered
            };

            match cli.effective_output() {
                OutputFormat::Json => {
                    let output: Vec<DecisionOutput> = filtered.iter().map(|d| to_output(d)).collect();
                    println!("{}", serde_json::to_string_pretty(&serde_json::json!({
                        "project_path": project_path,
                        "decisions": output,
                    }))?);
                }
                _ => {
                    if filtered.is_empty() {
                        println!("No decisions for {project_path}.");
                        return Ok(());
                    }
                    println!("Decisions for {project_path}:\n");
                    for d in &filtered {
                        let status_marker = match d.status {
                            DecisionStatus::Proposed => "?",
                            DecisionStatus::Confirmed => "!",
                            DecisionStatus::Superseded => "~",
                            DecisionStatus::Abandoned => "-",
                        };
                        let conf = if d.confidence < 1.0 {
                            format!(" ({:.0}%)", d.confidence * 100.0)
                        } else {
                            String::new()
                        };
                        let tags = if d.tags.is_empty() {
                            String::new()
                        } else {
                            format!(" [{}]", d.tags.join(", "))
                        };
                        let session = d.session_id.as_deref()
                            .map(|s| format!(" ({})", &s[..s.len().min(8)]))
                            .unwrap_or_default();
                        println!("  [{status_marker}] #{}: {}{}{}{}", d.id, d.title, conf, tags, session);
                        if let Some(ref desc) = d.description {
                            println!("      {desc}");
                        }
                    }
                    let active = store.active_decisions().len();
                    println!(
                        "\n{} decision(s), {} active",
                        store.decisions.len(),
                        active
                    );
                }
            }
        }

        "add" => {
            let title = args.title.as_deref().ok_or_else(|| SnatchError::InvalidArgument {
                name: "title".into(),
                reason: "--title is required for add operation".into(),
            })?;

            let status = if let Some(ref s) = args.status {
                Some(DecisionStatus::parse(s).ok_or_else(|| SnatchError::InvalidArgument {
                    name: "status".into(),
                    reason: format!(
                        "Invalid status '{s}'. Use: proposed, confirmed, superseded, abandoned"
                    ),
                })?)
            } else {
                None
            };

            let tags: Vec<String> = args.tag.iter()
                .flat_map(|t| t.split(',').map(|s| s.trim().to_string()))
                .filter(|s| !s.is_empty())
                .collect();

            let mut store = load_decisions(project_dir)?;
            let id = store.add_decision(
                title.to_string(),
                args.description.clone(),
                args.session_id.clone(),
                args.confidence,
                tags,
            );

            if let Some(s) = status {
                store.update_decision(id, Some(s), None, None, None);
            }

            save_decisions(project_dir, &store)?;

            match cli.effective_output() {
                OutputFormat::Json => {
                    let decision = store.decisions.iter().find(|d| d.id == id).unwrap();
                    println!("{}", serde_json::to_string_pretty(&serde_json::json!({
                        "operation": "add",
                        "project_path": project_path,
                        "message": format!("Added decision #{id}"),
                        "decision": to_output(decision),
                    }))?);
                }
                _ => println!("Added decision #{id}: {title}"),
            }
        }

        "update" => {
            let id = args.id.ok_or_else(|| SnatchError::InvalidArgument {
                name: "id".into(),
                reason: "--id is required for update operation".into(),
            })?;

            let status = if let Some(ref s) = args.status {
                Some(DecisionStatus::parse(s).ok_or_else(|| SnatchError::InvalidArgument {
                    name: "status".into(),
                    reason: format!(
                        "Invalid status '{s}'. Use: proposed, confirmed, superseded, abandoned"
                    ),
                })?)
            } else {
                None
            };

            let tags: Option<Vec<String>> = if args.tag.is_empty() {
                None
            } else {
                Some(args.tag.iter()
                    .flat_map(|t| t.split(',').map(|s| s.trim().to_string()))
                    .filter(|s| !s.is_empty())
                    .collect())
            };

            if status.is_none() && args.description.is_none() && args.confidence.is_none() && tags.is_none() {
                return Err(SnatchError::InvalidArgument {
                    name: "update".into(),
                    reason: "At least one of --status, --description, --confidence, or --tag is required".into(),
                });
            }

            let mut store = load_decisions(project_dir)?;
            if !store.update_decision(id, status, args.description.clone(), args.confidence, tags) {
                return Err(SnatchError::InvalidArgument {
                    name: "id".into(),
                    reason: format!("Decision #{id} not found"),
                });
            }
            save_decisions(project_dir, &store)?;

            match cli.effective_output() {
                OutputFormat::Json => {
                    let decision = store.decisions.iter().find(|d| d.id == id).unwrap();
                    println!("{}", serde_json::to_string_pretty(&serde_json::json!({
                        "operation": "update",
                        "project_path": project_path,
                        "message": format!("Updated decision #{id}"),
                        "decision": to_output(decision),
                    }))?);
                }
                _ => {
                    let decision = store.decisions.iter().find(|d| d.id == id).unwrap();
                    println!("Updated decision #{id}: [{}] {}", decision.status, decision.title);
                }
            }
        }

        "remove" => {
            let id = args.id.ok_or_else(|| SnatchError::InvalidArgument {
                name: "id".into(),
                reason: "--id is required for remove operation".into(),
            })?;

            let mut store = load_decisions(project_dir)?;
            if !store.remove_decision(id) {
                return Err(SnatchError::InvalidArgument {
                    name: "id".into(),
                    reason: format!("Decision #{id} not found"),
                });
            }
            save_decisions(project_dir, &store)?;

            match cli.effective_output() {
                OutputFormat::Json => {
                    println!("{}", serde_json::to_string_pretty(&serde_json::json!({
                        "operation": "remove",
                        "project_path": project_path,
                        "message": format!("Removed decision #{id}"),
                    }))?);
                }
                _ => println!("Removed decision #{id}"),
            }
        }

        "supersede" => {
            let id = args.id.ok_or_else(|| SnatchError::InvalidArgument {
                name: "id".into(),
                reason: "--id is required for supersede operation".into(),
            })?;
            let by = args.superseded_by.ok_or_else(|| SnatchError::InvalidArgument {
                name: "superseded-by".into(),
                reason: "--superseded-by is required for supersede operation".into(),
            })?;

            let mut store = load_decisions(project_dir)?;
            if !store.supersede_decision(id, by) {
                return Err(SnatchError::InvalidArgument {
                    name: "id".into(),
                    reason: format!("Decision #{id} or #{by} not found"),
                });
            }
            save_decisions(project_dir, &store)?;

            match cli.effective_output() {
                OutputFormat::Json => {
                    let decision = store.decisions.iter().find(|d| d.id == id).unwrap();
                    println!("{}", serde_json::to_string_pretty(&serde_json::json!({
                        "operation": "supersede",
                        "project_path": project_path,
                        "message": format!("Decision #{id} superseded by #{by}"),
                        "decision": to_output(decision),
                    }))?);
                }
                _ => {
                    let old_title = store.decisions.iter().find(|d| d.id == id).map(|d| d.title.as_str()).unwrap_or("?");
                    let new_title = store.decisions.iter().find(|d| d.id == by).map(|d| d.title.as_str()).unwrap_or("?");
                    println!("Decision #{id} '{old_title}' superseded by #{by} '{new_title}'");
                }
            }
        }

        "score" => {
            let mut store = load_decisions(project_dir)?;

            if store.decisions.is_empty() {
                if !cli.quiet {
                    println!("No decisions to score.");
                }
                return Ok(());
            }

            let claude_dir = get_claude_dir(cli.claude_dir.as_ref())?;
            let mut scored_count = 0u32;
            let mut skipped_no_id = 0u32;
            let mut skipped_not_found = 0u32;

            for decision in &mut store.decisions {
                let session_id = match &decision.session_id {
                    Some(id) => id.clone(),
                    None => {
                        skipped_no_id += 1;
                        continue;
                    }
                };

                let session = match claude_dir.find_session(&session_id)? {
                    Some(s) => s,
                    None => {
                        skipped_not_found += 1;
                        continue;
                    }
                };

                let entries = match session.parse_with_options(cli.max_file_size) {
                    Ok(e) => e,
                    Err(_) => {
                        skipped_not_found += 1;
                        continue;
                    }
                };

                let main_entries = main_thread_entries(&entries);
                let title_lower = decision.title.to_lowercase();
                let title_keywords: Vec<&str> = title_lower
                    .split_whitespace()
                    .filter(|w| w.len() > 3 && !STOP_WORDS.contains(w))
                    .collect();
                let mut score: f64 = 0.5;

                // Signal 1: User confirmed (affirmative response near decision text)
                let mut found_confirmation = false;
                for (i, entry) in main_entries.iter().enumerate() {
                    if entry.message_type() != "assistant" {
                        continue;
                    }
                    let text = match extract_text(entry) {
                        Some(t) => t,
                        None => continue,
                    };
                    let text_lower = text.to_lowercase();
                    if !text_lower.contains(&title_lower)
                        && !title_matches_text(&title_keywords, &text_lower)
                    {
                        continue;
                    }
                    if i + 1 < main_entries.len() && main_entries[i + 1].message_type() == "user" {
                        if let Some(user_text) = extract_text(main_entries[i + 1]) {
                            if is_affirmative(&user_text) {
                                found_confirmation = true;
                                break;
                            }
                        }
                    }
                }
                if found_confirmation {
                    score += 0.2;
                }

                // Signal 2: Implementation followed (tool calls near decision topic)
                let mut found_implementation = false;
                for (i, entry) in main_entries.iter().enumerate() {
                    if entry.message_type() != "assistant" {
                        continue;
                    }
                    if let Some(text) = extract_text(entry) {
                        let text_lower = text.to_lowercase();
                        if text_lower.contains(&title_lower)
                            || title_matches_text(&title_keywords, &text_lower)
                        {
                            for j in (i + 1)..main_entries.len().min(i + 4) {
                                if has_tool_calls(main_entries[j]) {
                                    found_implementation = true;
                                    break;
                                }
                            }
                            if found_implementation {
                                break;
                            }
                        }
                    }
                }
                if found_implementation {
                    score += 0.15;
                }

                // Signal 3: Options/tradeoffs discussed (indicates deliberation)
                let mut found_options = false;
                for entry in &main_entries {
                    if entry.message_type() != "assistant" {
                        continue;
                    }
                    if let Some(text) = extract_text(entry) {
                        let text_lower = text.to_lowercase();
                        if (text_lower.contains(&title_lower)
                            || title_lower.split_whitespace().all(|w| text_lower.contains(w)))
                            && has_options_pattern(&text)
                        {
                            found_options = true;
                            break;
                        }
                    }
                }
                if found_options {
                    score += 0.1;
                }

                // Signal 4: User correction found (negative signal)
                let correction_patterns = [
                    "no, ", "no that's wrong", "that's not what i",
                    "i said ", "i meant ", "not what i asked",
                    "wrong ", "incorrect", "don't do that",
                ];
                let mut found_correction = false;
                for entry in &main_entries {
                    if entry.message_type() != "user" {
                        continue;
                    }
                    if let Some(text) = extract_text(entry) {
                        let text_lower = text.to_lowercase();
                        if correction_patterns.iter().any(|p| text_lower.starts_with(p) || text_lower.contains(p)) {
                            if text_lower.contains(&title_lower)
                                || title_matches_text(&title_keywords, &text_lower)
                            {
                                found_correction = true;
                                break;
                            }
                        }
                    }
                }
                if found_correction {
                    score -= 0.2;
                }

                // Signal 5: Already superseded (negative signal from registry)
                if decision.status == DecisionStatus::Superseded {
                    score -= 0.15;
                }

                // Signal 6: Confirmed status (positive signal from registry)
                if decision.status == DecisionStatus::Confirmed {
                    score += 0.1;
                }

                score = score.clamp(0.0, 1.0);
                decision.confidence = score;
                decision.updated_at = chrono::Utc::now();
                scored_count += 1;
            }

            save_decisions(project_dir, &store)?;

            match cli.effective_output() {
                OutputFormat::Json => {
                    let output: Vec<DecisionOutput> = store.decisions.iter().map(|d| to_output(d)).collect();
                    println!("{}", serde_json::to_string_pretty(&serde_json::json!({
                        "operation": "score",
                        "project_path": project_path,
                        "scored": scored_count,
                        "skipped_no_session_id": skipped_no_id,
                        "skipped_session_not_found": skipped_not_found,
                        "decisions": output,
                    }))?);
                }
                _ => {
                    let mut skip_parts = Vec::new();
                    if skipped_no_id > 0 {
                        skip_parts.push(format!("{skipped_no_id} no session_id"));
                    }
                    if skipped_not_found > 0 {
                        skip_parts.push(format!("{skipped_not_found} session not found"));
                    }
                    let skip_msg = if skip_parts.is_empty() {
                        String::new()
                    } else {
                        format!(" (skipped: {})", skip_parts.join(", "))
                    };
                    println!("Auto-scored {} decision(s){}:\n", scored_count, skip_msg);
                    for d in &store.decisions {
                        let status_marker = match d.status {
                            DecisionStatus::Proposed => "?",
                            DecisionStatus::Confirmed => "!",
                            DecisionStatus::Superseded => "~",
                            DecisionStatus::Abandoned => "-",
                        };
                        println!("  [{status_marker}] #{}: {} → {:.0}%", d.id, d.title, d.confidence * 100.0);
                    }
                }
            }
        }

        other => {
            return Err(SnatchError::InvalidArgument {
                name: "operation".into(),
                reason: format!("Unknown operation '{other}'. Use: list, add, update, remove, supersede, score"),
            });
        }
    }

    Ok(())
}
