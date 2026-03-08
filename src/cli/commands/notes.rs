//! Notes command implementation.
//!
//! Manage tactical session notes for a project. Notes capture
//! mid-work state that survives compaction.

use crate::cli::{Cli, NotesArgs, OutputFormat};
use crate::error::{Result, SnatchError};
use crate::notes::{load_notes, save_notes};


/// JSON output for notes list.
#[derive(serde::Serialize)]
struct NotesListOutput {
    project_path: String,
    notes: Vec<NoteOutput>,
}

/// JSON output for a single note.
#[derive(serde::Serialize)]
struct NoteOutput {
    id: u64,
    text: String,
    created_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    session_id: Option<String>,
}

/// JSON output for note mutations.
#[derive(serde::Serialize)]
struct NoteMutationOutput {
    operation: String,
    project_path: String,
    message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    note: Option<NoteOutput>,
}

/// Run the notes command.
pub fn run(cli: &Cli, args: &NotesArgs) -> Result<()> {
    let project_filter = args.project.as_deref().unwrap_or("");
    let project = super::helpers::resolve_single_project(cli, project_filter)?;

    let project_dir = project.path();
    let project_path = project.decoded_path().to_string();

    let operation = args.operation.as_deref().unwrap_or("list");

    match operation {
        "list" => {
            let store = load_notes(project_dir)?;

            match cli.effective_output() {
                OutputFormat::Json => {
                    let output = NotesListOutput {
                        project_path,
                        notes: store
                            .notes
                            .iter()
                            .map(|n| NoteOutput {
                                id: n.id,
                                text: n.text.clone(),
                                created_at: n.created_at.to_rfc3339(),
                                session_id: n.session_id.clone(),
                            })
                            .collect(),
                    };
                    println!("{}", serde_json::to_string_pretty(&output)?);
                }
                _ => {
                    if store.notes.is_empty() {
                        println!("No notes for {project_path}.");
                        return Ok(());
                    }
                    println!("Notes for {project_path}:\n");
                    for note in &store.notes {
                        print!("  #{}: {}", note.id, note.text);
                        if let Some(ref sid) = note.session_id {
                            let short = if sid.len() > 8 { &sid[..8] } else { sid };
                            print!(" [session:{short}]");
                        }
                        println!();
                    }
                    println!("\n{} note(s)", store.notes.len());
                }
            }
        }

        "add" => {
            let text = args.text.as_deref().ok_or_else(|| SnatchError::InvalidArgument {
                name: "text".into(),
                reason: "--text is required for add operation".into(),
            })?;

            let mut store = load_notes(project_dir)?;
            let id = store.add_note(text.to_string(), args.session_id.clone());
            save_notes(project_dir, &store)?;

            match cli.effective_output() {
                OutputFormat::Json => {
                    let note = store.notes.iter().find(|n| n.id == id).unwrap();
                    let output = NoteMutationOutput {
                        operation: "add".into(),
                        project_path,
                        message: format!("Added note #{id}"),
                        note: Some(NoteOutput {
                            id: note.id,
                            text: note.text.clone(),
                            created_at: note.created_at.to_rfc3339(),
                            session_id: note.session_id.clone(),
                        }),
                    };
                    println!("{}", serde_json::to_string_pretty(&output)?);
                }
                _ => println!("Added note #{id}: {text}"),
            }
        }

        "remove" => {
            let id = args.id.ok_or_else(|| SnatchError::InvalidArgument {
                name: "id".into(),
                reason: "--id is required for remove operation".into(),
            })?;

            let mut store = load_notes(project_dir)?;
            if !store.remove_note(id) {
                return Err(SnatchError::InvalidArgument {
                    name: "id".into(),
                    reason: format!("Note #{id} not found"),
                });
            }
            save_notes(project_dir, &store)?;

            match cli.effective_output() {
                OutputFormat::Json => {
                    let output = NoteMutationOutput {
                        operation: "remove".into(),
                        project_path,
                        message: format!("Removed note #{id}"),
                        note: None,
                    };
                    println!("{}", serde_json::to_string_pretty(&output)?);
                }
                _ => println!("Removed note #{id}"),
            }
        }

        "clear" => {
            let mut store = load_notes(project_dir)?;
            let removed = store.clear();
            save_notes(project_dir, &store)?;

            match cli.effective_output() {
                OutputFormat::Json => {
                    let output = NoteMutationOutput {
                        operation: "clear".into(),
                        project_path,
                        message: format!("Cleared {removed} note(s)"),
                        note: None,
                    };
                    println!("{}", serde_json::to_string_pretty(&output)?);
                }
                _ => println!("Cleared {removed} note(s)"),
            }
        }

        other => {
            return Err(SnatchError::InvalidArgument {
                name: "operation".into(),
                reason: format!("Unknown operation '{other}'. Use: list, add, remove, clear"),
            });
        }
    }

    Ok(())
}
