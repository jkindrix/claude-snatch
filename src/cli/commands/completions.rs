//! Dynamic completions command implementation.
//!
//! Provides runtime completions for session IDs, project names, and other dynamic values.

use std::io::{self, Write};

use crate::cli::Cli;
use crate::error::Result;

use super::get_claude_dir;

/// Write a line to stdout, silently ignoring broken pipe errors.
/// This is common when shell completions are truncated.
fn write_completion(line: &str) -> bool {
    let stdout = io::stdout();
    let mut handle = stdout.lock();
    if writeln!(handle, "{}", line).is_err() {
        return false; // Stop on broken pipe
    }
    true
}

/// Completion type requested.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompletionType {
    /// Complete session IDs.
    Sessions,
    /// Complete project names/paths.
    Projects,
    /// Complete tool names.
    Tools,
    /// Complete output formats.
    Formats,
    /// Complete model names.
    Models,
}

impl std::str::FromStr for CompletionType {
    type Err = String;

    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "sessions" | "session" | "s" => Ok(Self::Sessions),
            "projects" | "project" | "p" => Ok(Self::Projects),
            "tools" | "tool" | "t" => Ok(Self::Tools),
            "formats" | "format" | "f" => Ok(Self::Formats),
            "models" | "model" | "m" => Ok(Self::Models),
            _ => Err(format!("unknown completion type: {}", s)),
        }
    }
}

/// Arguments for dynamic completions.
#[derive(Debug, Clone)]
pub struct DynamicCompletionsArgs {
    /// Type of completion to generate.
    pub completion_type: CompletionType,
    /// Optional prefix to filter completions.
    pub prefix: Option<String>,
    /// Maximum number of completions to return.
    pub limit: Option<usize>,
}

/// Generate dynamic completions.
pub fn run(cli: &Cli, args: &DynamicCompletionsArgs) -> Result<()> {
    match args.completion_type {
        CompletionType::Sessions => complete_sessions(cli, args.prefix.as_deref(), args.limit),
        CompletionType::Projects => complete_projects(cli, args.prefix.as_deref(), args.limit),
        CompletionType::Tools => complete_tools(cli, args.prefix.as_deref(), args.limit),
        CompletionType::Formats => complete_formats(args.prefix.as_deref()),
        CompletionType::Models => complete_models(args.prefix.as_deref()),
    }
}

/// Complete session IDs.
fn complete_sessions(cli: &Cli, prefix: Option<&str>, limit: Option<usize>) -> Result<()> {
    let claude_dir = get_claude_dir(cli.claude_dir.as_ref())?;
    let sessions = claude_dir.all_sessions()?;

    let limit = limit.unwrap_or(50);
    let mut count = 0;

    for session in sessions {
        let session_id = session.session_id().to_string();

        // Filter by prefix if provided
        if let Some(prefix) = prefix {
            if !session_id.starts_with(prefix) {
                continue;
            }
        }

        // Include project info for context
        let project = session.project_path();
        let short_project = if project.len() > 30 {
            format!("...{}", &project[project.len() - 27..])
        } else {
            project.to_string()
        };

        // Output session ID with description (tab-separated for shell completions)
        if !write_completion(&format!("{}\t{}", session_id, short_project)) {
            break; // Stop on broken pipe
        }

        count += 1;
        if count >= limit {
            break;
        }
    }

    Ok(())
}

/// Complete project names/paths.
fn complete_projects(cli: &Cli, prefix: Option<&str>, limit: Option<usize>) -> Result<()> {
    let claude_dir = get_claude_dir(cli.claude_dir.as_ref())?;
    let projects = claude_dir.projects()?;

    let limit = limit.unwrap_or(50);
    let mut count = 0;

    for project in projects {
        let path = project.decoded_path();

        // Filter by prefix if provided
        if let Some(prefix) = prefix {
            if !path.to_lowercase().contains(&prefix.to_lowercase()) {
                continue;
            }
        }

        // Get session count for context
        let session_count = project.sessions().map(|s| s.len()).unwrap_or(0);
        let desc = format!("{} sessions", session_count);

        if !write_completion(&format!("{}\t{}", path, desc)) {
            break; // Stop on broken pipe
        }

        count += 1;
        if count >= limit {
            break;
        }
    }

    Ok(())
}

/// Complete tool names.
fn complete_tools(cli: &Cli, prefix: Option<&str>, limit: Option<usize>) -> Result<()> {
    let claude_dir = get_claude_dir(cli.claude_dir.as_ref())?;
    let sessions = claude_dir.all_sessions()?;

    // Collect unique tool names
    let mut tools = std::collections::HashSet::new();
    let limit = limit.unwrap_or(100);

    'outer: for session in sessions {
        if let Ok(entries) = session.parse() {
            for entry in entries {
                if let crate::model::LogEntry::Assistant(msg) = entry {
                    for block in &msg.message.content {
                        if let crate::model::ContentBlock::ToolUse(tool) = block {
                            let name = &tool.name;

                            // Filter by prefix if provided
                            if let Some(prefix) = prefix {
                                if !name.to_lowercase().starts_with(&prefix.to_lowercase()) {
                                    continue;
                                }
                            }

                            tools.insert(name.clone());

                            if tools.len() >= limit {
                                break 'outer;
                            }
                        }
                    }
                }
            }
        }
    }

    // Sort and output
    let mut tools: Vec<_> = tools.into_iter().collect();
    tools.sort();

    for tool in tools {
        if !write_completion(&format!("{}\tClaude Code tool", tool)) {
            break; // Stop on broken pipe
        }
    }

    Ok(())
}

/// Complete output formats.
fn complete_formats(prefix: Option<&str>) -> Result<()> {
    let formats = [
        ("json", "JSON format"),
        ("text", "Plain text format"),
        ("compact", "Compact format"),
        ("tsv", "Tab-separated values"),
    ];

    for (format, desc) in formats {
        if let Some(prefix) = prefix {
            if !format.starts_with(&prefix.to_lowercase()) {
                continue;
            }
        }
        if !write_completion(&format!("{}\t{}", format, desc)) {
            break; // Stop on broken pipe
        }
    }

    Ok(())
}

/// Complete model names.
fn complete_models(prefix: Option<&str>) -> Result<()> {
    let models = [
        ("claude-3-opus", "Most capable model"),
        ("claude-3-sonnet", "Balanced model"),
        ("claude-3-haiku", "Fastest model"),
        ("claude-3.5-sonnet", "Latest Sonnet"),
        ("claude-3.5-haiku", "Latest Haiku"),
    ];

    for (model, desc) in models {
        if let Some(prefix) = prefix {
            if !model.to_lowercase().contains(&prefix.to_lowercase()) {
                continue;
            }
        }
        if !write_completion(&format!("{}\t{}", model, desc)) {
            break; // Stop on broken pipe
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_completion_type_from_str() {
        assert_eq!(
            "sessions".parse::<CompletionType>().unwrap(),
            CompletionType::Sessions
        );
        assert_eq!(
            "s".parse::<CompletionType>().unwrap(),
            CompletionType::Sessions
        );
        assert_eq!(
            "projects".parse::<CompletionType>().unwrap(),
            CompletionType::Projects
        );
        assert_eq!(
            "tools".parse::<CompletionType>().unwrap(),
            CompletionType::Tools
        );
        assert_eq!(
            "formats".parse::<CompletionType>().unwrap(),
            CompletionType::Formats
        );
        assert_eq!(
            "models".parse::<CompletionType>().unwrap(),
            CompletionType::Models
        );
    }

    #[test]
    fn test_completion_type_invalid() {
        assert!("invalid".parse::<CompletionType>().is_err());
    }

    #[test]
    fn test_complete_formats() {
        // Just verify it runs without error
        complete_formats(None).unwrap();
        complete_formats(Some("j")).unwrap();
    }

    #[test]
    fn test_complete_models() {
        // Just verify it runs without error
        complete_models(None).unwrap();
        complete_models(Some("sonnet")).unwrap();
    }
}
