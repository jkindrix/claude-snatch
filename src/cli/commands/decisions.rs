//! Decisions command implementation.
//!
//! Manage a persistent decision registry for a project. Decisions survive
//! compaction and sessions, enabling design decision tracking.

use crate::cli::{Cli, DecisionsArgs, OutputFormat};
use crate::decisions::{load_decisions, save_decisions, DecisionStatus};
use crate::error::{Result, SnatchError};

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

        other => {
            return Err(SnatchError::InvalidArgument {
                name: "operation".into(),
                reason: format!("Unknown operation '{other}'. Use: list, add, update, remove, supersede"),
            });
        }
    }

    Ok(())
}
