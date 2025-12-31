//! Prompts command implementation.
//!
//! Extract user prompts from Claude Code sessions with minimal friction.
//! This command provides a streamlined way to extract just the human-typed
//! prompts without tool results, system messages, or other noise.

use std::fs::File;
use std::io::{self, BufWriter, Write};

use serde::Serialize;

use crate::cli::{Cli, OutputFormat, PromptsArgs};
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

    let mut prompts = extract_prompts_from_session(&session, args, cli.max_file_size)?;

    // Apply limit if specified
    let total_before_limit = prompts.len();
    if let Some(limit) = args.limit {
        prompts.truncate(limit);
    }

    // Check if JSON output is requested
    let use_json = matches!(cli.effective_output(), OutputFormat::Json);

    if let Some(ref path) = args.output_file {
        let file = File::create(path).map_err(|e| {
            SnatchError::io(format!("Failed to create output file: {}", path.display()), e)
        })?;
        let mut writer = BufWriter::new(file);
        if use_json {
            write_prompts_json(&mut writer, &prompts, total_before_limit, None)?;
        } else {
            write_prompts(&mut writer, &prompts, args, None)?;
        }
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
        if use_json {
            write_prompts_json(&mut writer, &prompts, total_before_limit, None)?;
        } else {
            write_prompts(&mut writer, &prompts, args, None)?;
        }
    }

    Ok(())
}

/// Extract prompts from multiple sessions.
fn extract_multiple_sessions(cli: &Cli, args: &PromptsArgs) -> Result<()> {
    let claude_dir = get_claude_dir(cli.claude_dir.as_ref())?;

    // Build session filter
    let mut filter = SessionFilter::new();

    if !args.subagents {
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
            filter.matches(s).unwrap_or_default()
        })
        .collect();

    if sessions.is_empty() {
        if !cli.quiet {
            eprintln!("No sessions match the specified filters");
        }
        return Ok(());
    }

    // Sort by modification time (oldest first for chronological order)
    sessions.sort_by_key(|s| s.modified_time());

    // Check if JSON output is requested
    let use_json = matches!(cli.effective_output(), OutputFormat::Json);

    // Collect all prompts (needed for JSON output and limit application)
    let mut all_prompts: Vec<Prompt> = Vec::new();
    let mut session_count = 0;

    for session in &sessions {
        let prompts = match extract_prompts_from_session(session, args, cli.max_file_size) {
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

        session_count += 1;

        // Add session metadata to prompts if separators are enabled
        let prompts_with_meta: Vec<Prompt> = prompts
            .into_iter()
            .map(|mut p| {
                if args.separators || use_json {
                    p.session_id = Some(session.session_id().to_string());
                    p.project_path = Some(session.project_path().to_string());
                }
                p
            })
            .collect();

        all_prompts.extend(prompts_with_meta);

        // Check if we've reached the limit
        if let Some(limit) = args.limit {
            if all_prompts.len() >= limit {
                all_prompts.truncate(limit);
                break;
            }
        }
    }

    let total_before_limit = all_prompts.len();

    // Write output
    let mut writer: Box<dyn Write> = if let Some(ref path) = args.output_file {
        let file = File::create(path).map_err(|e| {
            SnatchError::io(format!("Failed to create output file: {}", path.display()), e)
        })?;
        Box::new(BufWriter::new(file))
    } else {
        Box::new(io::stdout())
    };

    if use_json {
        write_prompts_json(&mut writer, &all_prompts, total_before_limit, Some(session_count))?;
    } else {
        // Group prompts by session for text output if separators are enabled
        if args.separators {
            let mut current_session: Option<String> = None;
            let mut session_prompts: Vec<Prompt> = Vec::new();

            for prompt in &all_prompts {
                let prompt_session = prompt.session_id.clone();
                if current_session.as_ref() != prompt_session.as_ref() {
                    // Write previous session's prompts
                    if !session_prompts.is_empty() {
                        let info = current_session.as_ref().map(|sid| SessionInfo {
                            session_id: sid.clone(),
                            project_path: session_prompts[0]
                                .project_path
                                .clone()
                                .unwrap_or_default(),
                        });
                        write_prompts(&mut writer, &session_prompts, args, info.as_ref())?;
                    }
                    session_prompts.clear();
                    current_session = prompt_session;
                }
                session_prompts.push(prompt.clone());
            }

            // Write final session's prompts
            if !session_prompts.is_empty() {
                let info = current_session.as_ref().map(|sid| SessionInfo {
                    session_id: sid.clone(),
                    project_path: session_prompts[0]
                        .project_path
                        .clone()
                        .unwrap_or_default(),
                });
                write_prompts(&mut writer, &session_prompts, args, info.as_ref())?;
            }
        } else {
            write_prompts(&mut writer, &all_prompts, args, None)?;
        }
    }

    // Finalize atomic file if writing to file
    if let Some(ref path) = args.output_file {
        drop(writer);
        if !cli.quiet {
            eprintln!(
                "Extracted {} prompts from {} sessions to {}",
                all_prompts.len(),
                session_count,
                path.display()
            );
        }
    }

    Ok(())
}

/// Session information for separators.
#[derive(Debug, Clone, Serialize)]
struct SessionInfo {
    session_id: String,
    project_path: String,
}

/// Extracted prompt with metadata.
#[derive(Debug, Clone, Serialize)]
struct Prompt {
    text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    timestamp: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    session_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    project_path: Option<String>,
}

/// Collection of prompts for JSON output.
#[derive(Debug, Clone, Serialize)]
struct PromptsOutput {
    prompts: Vec<Prompt>,
    total_count: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    session_count: Option<usize>,
}

/// Extract prompts from a session.
fn extract_prompts_from_session(session: &Session, args: &PromptsArgs, max_file_size: Option<u64>) -> Result<Vec<Prompt>> {
    let entries = session.parse_with_options(max_file_size)?;
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
                    session_id: None,    // Will be filled in by caller if needed
                    project_path: None,  // Will be filled in by caller if needed
                });
            }
        }
    }

    Ok(prompts)
}

/// Write prompts as JSON to output.
fn write_prompts_json<W: Write>(
    writer: &mut W,
    prompts: &[Prompt],
    total_count: usize,
    session_count: Option<usize>,
) -> Result<()> {
    let output = PromptsOutput {
        prompts: prompts.to_vec(),
        total_count,
        session_count,
    };
    serde_json::to_writer_pretty(&mut *writer, &output)?;
    writeln!(writer)?;
    Ok(())
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
            session_id: None,
            project_path: None,
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
