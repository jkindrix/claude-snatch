//! Prompts command implementation.
//!
//! Extract user prompts from Claude Code sessions with minimal friction.
//! This command provides a streamlined way to extract just the human-typed
//! prompts without tool results, system messages, or other noise.

use std::fs::File;
use std::io::{self, BufWriter, Write};

use crate::cli::{Cli, PromptsArgs};
use crate::discovery::{Session, SessionFilter};
use crate::error::{Result, SnatchError};
use crate::model::LogEntry;

use super::{get_claude_dir, parse_date_filter};

/// Run the prompts command.
pub fn run(cli: &Cli, args: &PromptsArgs) -> Result<()> {
    // Validate arguments
    if args.session.is_none() && !args.all && args.project.is_none() {
        return Err(SnatchError::InvalidArgument {
            name: "session".to_string(),
            reason: "Specify a session ID, use --all, or use -p/--project to filter".to_string(),
        });
    }

    // If a specific session is provided
    if let Some(ref session_id) = args.session {
        return extract_single_session(cli, args, session_id);
    }

    // Otherwise, extract from multiple sessions
    extract_multiple_sessions(cli, args)
}

/// Extract prompts from a single session.
fn extract_single_session(cli: &Cli, args: &PromptsArgs, session_id: &str) -> Result<()> {
    let claude_dir = get_claude_dir(cli.claude_dir.as_ref())?;

    let session = claude_dir
        .find_session(session_id)?
        .ok_or_else(|| SnatchError::SessionNotFound {
            session_id: session_id.to_string(),
        })?;

    let prompts = extract_prompts_from_session(&session, args)?;

    if let Some(ref path) = args.output_file {
        let file = File::create(path).map_err(|e| {
            SnatchError::io(format!("Failed to create output file: {}", path.display()), e)
        })?;
        let mut writer = BufWriter::new(file);
        write_prompts(&mut writer, &prompts, args, None)?;
        writer.flush()?;
        if !cli.quiet {
            eprintln!(
                "Extracted {} prompts to {}",
                prompts.len(),
                path.display()
            );
        }
    } else {
        let mut writer = io::stdout();
        write_prompts(&mut writer, &prompts, args, None)?;
    }

    Ok(())
}

/// Extract prompts from multiple sessions.
fn extract_multiple_sessions(cli: &Cli, args: &PromptsArgs) -> Result<()> {
    let claude_dir = get_claude_dir(cli.claude_dir.as_ref())?;

    // Build session filter
    let mut filter = SessionFilter::new();

    if !args.include_agents {
        filter = filter.main_only();
    }

    if let Some(ref since) = args.since {
        let since_time = parse_date_filter(since)?;
        filter.modified_after = Some(since_time);
    }

    if let Some(ref until) = args.until {
        let until_time = parse_date_filter(until)?;
        filter.modified_before = Some(until_time);
    }

    // Get all sessions
    let all_sessions = claude_dir.all_sessions()?;

    // Filter sessions
    let mut sessions: Vec<&Session> = all_sessions
        .iter()
        .filter(|s| {
            if let Some(ref project) = args.project {
                if !s.project_path().contains(project) {
                    return false;
                }
            }
            match filter.matches(s) {
                Ok(matches) => matches,
                Err(_) => false,
            }
        })
        .collect();

    if sessions.is_empty() {
        if !cli.quiet {
            eprintln!("No sessions match the specified filters");
        }
        return Ok(());
    }

    // Sort by modification time (oldest first for chronological order)
    sessions.sort_by(|a, b| a.modified_time().cmp(&b.modified_time()));

    let mut writer: Box<dyn Write> = if let Some(ref path) = args.output_file {
        let file = File::create(path).map_err(|e| {
            SnatchError::io(format!("Failed to create output file: {}", path.display()), e)
        })?;
        Box::new(BufWriter::new(file))
    } else {
        Box::new(io::stdout())
    };

    let mut total_prompts = 0;

    for session in &sessions {
        let prompts = match extract_prompts_from_session(session, args) {
            Ok(p) => p,
            Err(e) => {
                if !cli.quiet {
                    eprintln!("Warning: Failed to extract from {}: {}", session.session_id(), e);
                }
                continue;
            }
        };

        if prompts.is_empty() {
            continue;
        }

        let session_info = if args.separators {
            Some(SessionInfo {
                session_id: session.session_id().to_string(),
                project_path: session.project_path().to_string(),
            })
        } else {
            None
        };

        write_prompts(&mut writer, &prompts, args, session_info.as_ref())?;

        total_prompts += prompts.len();
    }

    // Finalize atomic file if writing to file
    if let Some(ref path) = args.output_file {
        drop(writer);
        if !cli.quiet {
            eprintln!(
                "Extracted {} prompts from {} sessions to {}",
                total_prompts,
                sessions.len(),
                path.display()
            );
        }
    }

    Ok(())
}

/// Session information for separators.
struct SessionInfo {
    session_id: String,
    project_path: String,
}

/// Extracted prompt with metadata.
struct Prompt {
    text: String,
    timestamp: Option<String>,
}

/// Extract prompts from a session.
fn extract_prompts_from_session(session: &Session, args: &PromptsArgs) -> Result<Vec<Prompt>> {
    let entries = session.parse()?;
    let mut prompts = Vec::new();

    for entry in entries {
        if let LogEntry::User(user) = entry {
            // Get text content from user message
            let text = match &user.message {
                crate::model::message::UserContent::Simple(simple) => {
                    simple.content.clone()
                }
                crate::model::message::UserContent::Blocks(blocks) => {
                    // Extract text blocks only (skip tool results)
                    blocks
                        .content
                        .iter()
                        .filter_map(|block| {
                            if let crate::model::ContentBlock::Text(t) = block {
                                Some(t.text.as_str())
                            } else {
                                None
                            }
                        })
                        .collect::<Vec<_>>()
                        .join("\n")
                }
            };

            // Apply minimum length filter
            let trimmed = text.trim();
            if trimmed.len() >= args.min_length {
                let timestamp = if args.timestamps {
                    Some(user.timestamp.format("%Y-%m-%d %H:%M:%S UTC").to_string())
                } else {
                    None
                };

                prompts.push(Prompt {
                    text: trimmed.to_string(),
                    timestamp,
                });
            }
        }
    }

    Ok(prompts)
}

/// Write prompts to output.
fn write_prompts<W: Write>(
    writer: &mut W,
    prompts: &[Prompt],
    args: &PromptsArgs,
    session_info: Option<&SessionInfo>,
) -> Result<()> {
    // Write session separator if requested
    if let Some(info) = session_info {
        writeln!(writer)?;
        writeln!(writer, "# Session: {} ({})", &info.session_id[..8.min(info.session_id.len())], info.project_path)?;
        writeln!(writer)?;
    }

    for (i, prompt) in prompts.iter().enumerate() {
        if args.numbered {
            write!(writer, "{}. ", i + 1)?;
        }

        if let Some(ref ts) = prompt.timestamp {
            writeln!(writer, "[{}]", ts)?;
        }

        writeln!(writer, "{}", prompt.text)?;

        // Add blank line between prompts for readability
        if i < prompts.len() - 1 {
            writeln!(writer)?;
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_prompt_struct() {
        let prompt = Prompt {
            text: "Hello world".to_string(),
            timestamp: Some("2025-12-30 12:00:00 UTC".to_string()),
        };
        assert_eq!(prompt.text, "Hello world");
        assert!(prompt.timestamp.is_some());
    }

    #[test]
    fn test_session_info_struct() {
        let info = SessionInfo {
            session_id: "abc12345-1234-5678-9abc-def012345678".to_string(),
            project_path: "/home/user/project".to_string(),
        };
        assert!(info.session_id.starts_with("abc12345"));
    }
}
