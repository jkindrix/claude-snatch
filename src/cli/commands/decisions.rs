//! Decisions command implementation.
//!
//! Manage a persistent decision registry for a project. Decisions survive
//! compaction and sessions, enabling design decision tracking.

use crate::cli::{Cli, DecisionsArgs, OutputFormat};
use crate::decisions::{load_decisions, save_decisions, DecisionStatus};
use crate::error::{Result, SnatchError};
use crate::model::{ContentBlock, LogEntry};

use super::get_claude_dir;

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
    let claude_dir = get_claude_dir(cli.claude_dir.as_ref())?;

    // Resolve project
    let project_filter = args.project.as_deref().unwrap_or("");
    let projects = claude_dir.projects()?;
    let matches: Vec<_> = projects
        .iter()
        .filter(|p| {
            p.decoded_path().contains(project_filter)
                || p.encoded_name().contains(project_filter)
        })
        .collect();

    let project = match matches.len() {
        0 => {
            return Err(SnatchError::ProjectNotFound {
                project_path: format!("No project matching '{project_filter}'"),
            })
        }
        1 => matches[0],
        n => {
            let names: Vec<_> = matches.iter().map(|p| p.decoded_path()).collect();
            return Err(SnatchError::InvalidArgument {
                name: "project".into(),
                reason: format!(
                    "Ambiguous filter '{project_filter}' matches {n} projects: {}",
                    names.join(", ")
                ),
            });
        }
    };

    let project_dir = project.path();
    let project_path = project.decoded_path().to_string();

    let operation = args.operation.as_deref().unwrap_or("list");

    match operation {
        "list" => {
            let store = load_decisions(project_dir)?;

            // Filter by status if specified
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

            // Filter by tag if specified
            let filtered: Vec<_> = if let Some(ref tag_filter) = args.tag {
                filtered.into_iter().filter(|d| d.tags.iter().any(|t| t.contains(tag_filter.as_str()))).collect()
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
                        println!("  [{status_marker}] #{}: {}{}{}", d.id, d.title, conf, tags);
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

            let tags: Vec<String> = args
                .tag
                .as_deref()
                .map(|t| t.split(',').map(|s| s.trim().to_string()).collect())
                .unwrap_or_default();

            let mut store = load_decisions(project_dir)?;
            let id = store.add_decision(
                title.to_string(),
                args.description.clone(),
                args.session_id.clone(),
                args.confidence,
                tags,
            );

            // Apply status if specified (add defaults to Proposed)
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

            let tags = args
                .tag
                .as_deref()
                .map(|t| t.split(',').map(|s| s.trim().to_string()).collect());

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
                _ => println!("Decision #{id} superseded by #{by}"),
            }
        }

        "score" => {
            let claude_dir = get_claude_dir(cli.claude_dir.as_ref())?;
            let mut store = load_decisions(project_dir)?;

            if store.decisions.is_empty() {
                if !cli.quiet {
                    println!("No decisions to score.");
                }
                return Ok(());
            }

            let mut scored_count = 0u32;
            let mut skipped_count = 0u32;

            for decision in &mut store.decisions {
                let session_id = match &decision.session_id {
                    Some(id) => id.clone(),
                    None => {
                        skipped_count += 1;
                        continue;
                    }
                };

                let session = match claude_dir.find_session(&session_id)? {
                    Some(s) => s,
                    None => {
                        skipped_count += 1;
                        continue;
                    }
                };

                let entries = match session.parse_with_options(cli.max_file_size) {
                    Ok(e) => e,
                    Err(_) => {
                        skipped_count += 1;
                        continue;
                    }
                };

                let main_entries: Vec<&LogEntry> = entries
                    .iter()
                    .filter(|e| !e.is_sidechain())
                    .filter(|e| matches!(e, LogEntry::User(_) | LogEntry::Assistant(_)))
                    .collect();

                let title_lower = decision.title.to_lowercase();
                let mut score: f64 = 0.5; // Base score

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
                        && !title_lower.split_whitespace().all(|w| text_lower.contains(w))
                    {
                        continue;
                    }
                    // Check if next user message is affirmative
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
                            || title_lower.split_whitespace().all(|w| text_lower.contains(w))
                        {
                            // Check next few entries for tool calls
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
                            // Only count if nearby text relates to decision topic
                            if text_lower.contains(&title_lower)
                                || title_lower.split_whitespace().any(|w| text_lower.contains(w))
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
                        "skipped": skipped_count,
                        "decisions": output,
                    }))?);
                }
                _ => {
                    println!("Auto-scored {} decision(s) ({} skipped, no session_id):\n", scored_count, skipped_count);
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

/// Extract visible text from an entry.
fn extract_text(entry: &LogEntry) -> Option<String> {
    match entry {
        LogEntry::User(user) => {
            let text = match &user.message {
                crate::model::UserContent::Simple(s) => s.content.clone(),
                crate::model::UserContent::Blocks(b) => b
                    .content
                    .iter()
                    .filter_map(|c| {
                        if let ContentBlock::Text(t) = c {
                            Some(t.text.as_str())
                        } else {
                            None
                        }
                    })
                    .collect::<Vec<_>>()
                    .join("\n"),
            };
            if text.trim().is_empty() {
                None
            } else {
                Some(text)
            }
        }
        LogEntry::Assistant(assistant) => {
            let texts: Vec<&str> = assistant
                .message
                .content
                .iter()
                .filter_map(|block| {
                    if let ContentBlock::Text(t) = block {
                        Some(t.text.as_str())
                    } else {
                        None
                    }
                })
                .collect();
            let joined = texts.join("\n");
            if joined.trim().is_empty() {
                None
            } else {
                Some(joined)
            }
        }
        _ => None,
    }
}

/// Check if an assistant entry contains tool use calls.
fn has_tool_calls(entry: &LogEntry) -> bool {
    if let LogEntry::Assistant(assistant) = entry {
        assistant
            .message
            .content
            .iter()
            .any(|block| matches!(block, ContentBlock::ToolUse(_)))
    } else {
        false
    }
}

/// Check if user response is a short affirmative (decision confirmation).
fn is_affirmative(text: &str) -> bool {
    let trimmed = text.trim();
    let lower = trimmed.to_lowercase();
    let word_count = trimmed.split_whitespace().count();

    let affirmatives = [
        "yes", "yeah", "yep", "yup", "sure", "ok", "okay", "sounds good",
        "go for it", "do it", "let's do it", "let's go", "perfect",
        "exactly", "agreed", "correct", "right", "absolutely",
        "that works", "makes sense", "go ahead", "proceed",
        "i agree", "i like", "i think so", "definitely",
    ];
    if affirmatives.iter().any(|a| lower.starts_with(a)) {
        return true;
    }

    let choice_patterns = [
        "option ", "approach ", "let's go with", "go with ",
        "i prefer", "i'd prefer", "i'll go with", "let's use",
        "use ", "i choose", "i pick",
    ];
    if choice_patterns.iter().any(|p| lower.starts_with(p)) {
        return true;
    }

    if word_count <= 30 && !trimmed.contains('?') {
        if lower.contains("agree") || lower.contains("go with")
            || lower.contains("let's") || lower.contains("sounds")
            || lower.contains("perfect") || lower.contains("great")
        {
            return true;
        }
    }

    false
}

/// Check if assistant response contains enumeration/options patterns.
fn has_options_pattern(text: &str) -> bool {
    let lower = text.to_lowercase();

    let numbered = regex::Regex::new(r"(?m)^\s*\d+[\.\)]\s+").unwrap();
    if numbered.find_iter(text).count() >= 2 {
        return true;
    }

    if (lower.contains("option a") && lower.contains("option b"))
        || (lower.contains("approach 1") && lower.contains("approach 2"))
        || (lower.contains("option 1") && lower.contains("option 2"))
    {
        return true;
    }

    if (lower.contains("pros:") && lower.contains("cons:"))
        || (lower.contains("advantages") && lower.contains("disadvantages"))
        || lower.contains("trade-off")
        || lower.contains("tradeoff")
    {
        return true;
    }

    let bullets = regex::Regex::new(r"(?m)^\s*[-*]\s+").unwrap();
    if bullets.find_iter(text).count() >= 3
        && (lower.contains("alternatively")
            || lower.contains("or we could")
            || lower.contains("another approach")
            || lower.contains("we could also")
            || lower.contains("versus")
            || lower.contains(" vs "))
    {
        return true;
    }

    false
}
