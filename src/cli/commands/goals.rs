//! Goals command implementation.
//!
//! Manage persistent goals for a project. Goals survive compaction
//! and sessions, enabling long-term intent tracking.

use crate::cli::{Cli, GoalsArgs, OutputFormat};
use crate::error::{Result, SnatchError};
use crate::goals::{load_goals, save_goals, GoalStatus};

use super::get_claude_dir;

/// JSON output for goals list.
#[derive(serde::Serialize)]
struct GoalsListOutput {
    project_path: String,
    goals: Vec<GoalOutput>,
}

/// JSON output for a single goal.
#[derive(serde::Serialize)]
struct GoalOutput {
    id: u64,
    text: String,
    status: String,
    created_at: String,
    updated_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    progress: Option<String>,
}

/// JSON output for goal mutations.
#[derive(serde::Serialize)]
struct GoalMutationOutput {
    operation: String,
    project_path: String,
    message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    goal: Option<GoalOutput>,
}

/// Run the goals command.
pub fn run(cli: &Cli, args: &GoalsArgs) -> Result<()> {
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
            let store = load_goals(project_dir)?;

            match cli.effective_output() {
                OutputFormat::Json => {
                    let output = GoalsListOutput {
                        project_path,
                        goals: store
                            .goals
                            .iter()
                            .map(|g| GoalOutput {
                                id: g.id,
                                text: g.text.clone(),
                                status: g.status.to_string(),
                                created_at: g.created_at.to_rfc3339(),
                                updated_at: g.updated_at.to_rfc3339(),
                                progress: g.progress.clone(),
                            })
                            .collect(),
                    };
                    println!("{}", serde_json::to_string_pretty(&output)?);
                }
                _ => {
                    if store.goals.is_empty() {
                        println!("No goals for {project_path}.");
                        return Ok(());
                    }
                    println!("Goals for {project_path}:\n");
                    for goal in &store.goals {
                        let status_marker = match goal.status {
                            GoalStatus::Open => " ",
                            GoalStatus::InProgress => ">",
                            GoalStatus::Done => "x",
                            GoalStatus::Abandoned => "-",
                        };
                        print!("  [{status_marker}] #{}: {}", goal.id, goal.text);
                        if let Some(ref p) = goal.progress {
                            print!(" ({p})");
                        }
                        println!();
                    }
                    let active = store.active_goals().len();
                    println!("\n{} goal(s), {} active", store.goals.len(), active);
                }
            }
        }

        "add" => {
            let text = args.text.as_deref().ok_or_else(|| SnatchError::InvalidArgument {
                name: "text".into(),
                reason: "--text is required for add operation".into(),
            })?;

            let mut store = load_goals(project_dir)?;
            let id = store.add_goal(text.to_string(), args.progress.clone());
            save_goals(project_dir, &store)?;

            match cli.effective_output() {
                OutputFormat::Json => {
                    let goal = store.goals.iter().find(|g| g.id == id).unwrap();
                    let output = GoalMutationOutput {
                        operation: "add".into(),
                        project_path,
                        message: format!("Added goal #{id}"),
                        goal: Some(GoalOutput {
                            id: goal.id,
                            text: goal.text.clone(),
                            status: goal.status.to_string(),
                            created_at: goal.created_at.to_rfc3339(),
                            updated_at: goal.updated_at.to_rfc3339(),
                            progress: goal.progress.clone(),
                        }),
                    };
                    println!("{}", serde_json::to_string_pretty(&output)?);
                }
                _ => println!("Added goal #{id}: {text}"),
            }
        }

        "update" => {
            let id = args.id.ok_or_else(|| SnatchError::InvalidArgument {
                name: "id".into(),
                reason: "--id is required for update operation".into(),
            })?;

            let status = match args.status.as_deref() {
                Some(s) => Some(GoalStatus::parse(s).ok_or_else(|| SnatchError::InvalidArgument {
                    name: "status".into(),
                    reason: format!(
                        "Invalid status '{s}'. Use: open, in_progress, done, abandoned"
                    ),
                })?),
                None => None,
            };

            if status.is_none() && args.progress.is_none() {
                return Err(SnatchError::InvalidArgument {
                    name: "update".into(),
                    reason: "At least one of --status or --progress is required".into(),
                });
            }

            let mut store = load_goals(project_dir)?;
            if !store.update_goal(id, status, args.progress.clone()) {
                return Err(SnatchError::InvalidArgument {
                    name: "id".into(),
                    reason: format!("Goal #{id} not found"),
                });
            }
            save_goals(project_dir, &store)?;

            match cli.effective_output() {
                OutputFormat::Json => {
                    let goal = store.goals.iter().find(|g| g.id == id).unwrap();
                    let output = GoalMutationOutput {
                        operation: "update".into(),
                        project_path,
                        message: format!("Updated goal #{id}"),
                        goal: Some(GoalOutput {
                            id: goal.id,
                            text: goal.text.clone(),
                            status: goal.status.to_string(),
                            created_at: goal.created_at.to_rfc3339(),
                            updated_at: goal.updated_at.to_rfc3339(),
                            progress: goal.progress.clone(),
                        }),
                    };
                    println!("{}", serde_json::to_string_pretty(&output)?);
                }
                _ => {
                    let goal = store.goals.iter().find(|g| g.id == id).unwrap();
                    println!("Updated goal #{id}: [{}] {}", goal.status, goal.text);
                }
            }
        }

        "remove" => {
            let id = args.id.ok_or_else(|| SnatchError::InvalidArgument {
                name: "id".into(),
                reason: "--id is required for remove operation".into(),
            })?;

            let mut store = load_goals(project_dir)?;
            if !store.remove_goal(id) {
                return Err(SnatchError::InvalidArgument {
                    name: "id".into(),
                    reason: format!("Goal #{id} not found"),
                });
            }
            save_goals(project_dir, &store)?;

            match cli.effective_output() {
                OutputFormat::Json => {
                    let output = GoalMutationOutput {
                        operation: "remove".into(),
                        project_path,
                        message: format!("Removed goal #{id}"),
                        goal: None,
                    };
                    println!("{}", serde_json::to_string_pretty(&output)?);
                }
                _ => println!("Removed goal #{id}"),
            }
        }

        other => {
            return Err(SnatchError::InvalidArgument {
                name: "operation".into(),
                reason: format!("Unknown operation '{other}'. Use: list, add, update, remove"),
            });
        }
    }

    Ok(())
}
