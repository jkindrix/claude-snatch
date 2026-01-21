//! Diff command implementation.
//!
//! Compares two sessions or exported files to show differences.
//! Supports both line-based diff and semantic diff (by message structure).

use std::fs;
use std::path::PathBuf;

use std::collections::HashSet;

use crate::cli::{Cli, DiffArgs, OutputFormat};
use crate::error::{Result, SnatchError};
use crate::export::JsonlDiff;
use crate::model::LogEntry;
use crate::parser::JsonlParser;
use crate::reconstruction::Conversation;

use super::get_claude_dir;

/// Build the set of message types to include from args.
/// Returns None if no filtering should be applied (include all types).
fn build_type_filter(args: &DiffArgs) -> Option<HashSet<String>> {
    let mut types = HashSet::new();

    // --prompts is shorthand for --type user
    if args.prompts {
        types.insert("user".to_string());
    }

    // Add any explicitly specified types
    for t in &args.message_type {
        types.insert(t.to_lowercase());
    }

    // If no types specified, don't filter
    if types.is_empty() {
        None
    } else {
        Some(types)
    }
}

/// Filter log entries by message type.
fn filter_entries_by_type(entries: Vec<LogEntry>, type_filter: &Option<HashSet<String>>) -> Vec<LogEntry> {
    match type_filter {
        None => entries, // No filter, return all
        Some(types) => entries
            .into_iter()
            .filter(|entry| {
                let msg_type = entry.message_type().to_lowercase();
                types.contains(&msg_type)
            })
            .collect(),
    }
}

/// Run the diff command.
pub fn run(cli: &Cli, args: &DiffArgs) -> Result<()> {
    // Resolve the two targets
    let (first_path, second_path) = resolve_targets(cli, args)?;

    // Default to semantic diff (more useful for JSONL sessions)
    // Use line-based only if explicitly requested
    if args.line_based {
        // Line-based diff: compare raw JSONL content
        run_line_diff(cli, args, &first_path, &second_path)
    } else {
        // Semantic diff: compare by message structure (default)
        run_semantic_diff(cli, args, &first_path, &second_path)
    }
}

/// Run line-based diff comparison.
fn run_line_diff(cli: &Cli, args: &DiffArgs, first_path: &PathBuf, second_path: &PathBuf) -> Result<()> {
    // Read and compare the files
    let first_content = fs::read_to_string(first_path)
        .map_err(|e| SnatchError::io(format!("Failed to read first file: {}", first_path.display()), e))?;
    let second_content = fs::read_to_string(second_path)
        .map_err(|e| SnatchError::io(format!("Failed to read second file: {}", second_path.display()), e))?;

    // Perform the diff
    let diff = JsonlDiff::compare(&first_content, &second_content);

    // Output based on format
    match cli.effective_output() {
        OutputFormat::Json => print_line_diff_json(&diff, first_path, second_path)?,
        _ => print_line_diff_text(&diff, first_path, second_path, args)?,
    }

    // Exit with appropriate code
    if !diff.is_identical() && args.exit_code {
        std::process::exit(1);
    }

    Ok(())
}

/// Run semantic diff comparison.
fn run_semantic_diff(cli: &Cli, args: &DiffArgs, first_path: &PathBuf, second_path: &PathBuf) -> Result<()> {
    // Parse both files as JSONL
    let mut parser = JsonlParser::new().with_lenient(true);

    let first_entries = parser.parse_file(first_path)?;
    let second_entries = parser.parse_file(second_path)?;

    // Apply message type filter if specified
    let type_filter = build_type_filter(args);
    let first_entries = filter_entries_by_type(first_entries, &type_filter);
    let second_entries = filter_entries_by_type(second_entries, &type_filter);

    // Build conversation trees
    let first_conv = Conversation::from_entries(first_entries)?;
    let second_conv = Conversation::from_entries(second_entries)?;

    // Perform semantic comparison
    let diff = compare_conversations(&first_conv, &second_conv);

    // Output based on format
    match cli.effective_output() {
        OutputFormat::Json => print_semantic_diff_json(&diff, first_path, second_path, &type_filter)?,
        _ => print_semantic_diff_text(&diff, first_path, second_path, args, &type_filter)?,
    }

    // Exit with appropriate code
    if !diff.is_identical() && args.exit_code {
        std::process::exit(1);
    }

    Ok(())
}

/// Resolve the two targets to compare.
fn resolve_targets(cli: &Cli, args: &DiffArgs) -> Result<(PathBuf, PathBuf)> {
    let claude_dir = get_claude_dir(cli.claude_dir.as_ref())?;

    // First target
    let first_path = if let Some(session) = claude_dir.find_session(&args.first)? {
        session.path().to_path_buf()
    } else {
        let path = PathBuf::from(&args.first);
        if !path.exists() {
            return Err(SnatchError::FileNotFound { path });
        }
        path
    };

    // Second target
    let second_path = if let Some(session) = claude_dir.find_session(&args.second)? {
        session.path().to_path_buf()
    } else {
        let path = PathBuf::from(&args.second);
        if !path.exists() {
            return Err(SnatchError::FileNotFound { path });
        }
        path
    };

    Ok((first_path, second_path))
}

/// Print line-based diff output in text format.
fn print_line_diff_text(diff: &JsonlDiff, first: &PathBuf, second: &PathBuf, args: &DiffArgs) -> Result<()> {
    if diff.is_identical() {
        println!("Files are identical.");
        return Ok(());
    }

    println!("Comparing (line-based):");
    println!("  A: {}", first.display());
    println!("  B: {}", second.display());
    println!();

    // Summary
    println!("Summary:");
    println!("  Matching lines:  {}", diff.matching);
    println!("  Only in A:       {}", diff.only_in_first.len());
    println!("  Only in B:       {}", diff.only_in_second.len());
    println!("  Different:       {}", diff.different.len());
    println!();

    // Details if requested
    if !args.summary_only {
        // Lines only in first
        if !diff.only_in_first.is_empty() {
            println!("Lines only in A (first file):");
            for line_num in &diff.only_in_first {
                println!("  Line {line_num}");
            }
            println!();
        }

        // Lines only in second
        if !diff.only_in_second.is_empty() {
            println!("Lines only in B (second file):");
            for line_num in &diff.only_in_second {
                println!("  Line {line_num}");
            }
            println!();
        }

        // Different lines
        if !diff.different.is_empty() {
            println!("Lines that differ:");
            for (line_num, first_line, second_line) in &diff.different {
                println!("  Line {line_num}:");
                if !args.no_content {
                    let first_preview = truncate_line(first_line, 80);
                    let second_preview = truncate_line(second_line, 80);
                    println!("    A: {first_preview}");
                    println!("    B: {second_preview}");
                }
            }
        }
    }

    Ok(())
}

/// Print line-based diff output in JSON format.
fn print_line_diff_json(diff: &JsonlDiff, first: &PathBuf, second: &PathBuf) -> Result<()> {
    let output = serde_json::json!({
        "mode": "line-based",
        "identical": diff.is_identical(),
        "first": first.to_string_lossy(),
        "second": second.to_string_lossy(),
        "summary": {
            "matching": diff.matching,
            "only_in_first": diff.only_in_first.len(),
            "only_in_second": diff.only_in_second.len(),
            "different": diff.different.len(),
        },
        "details": {
            "only_in_first": diff.only_in_first,
            "only_in_second": diff.only_in_second,
            "different_lines": diff.different.iter()
                .map(|(n, a, b)| serde_json::json!({
                    "line": n,
                    "first": a,
                    "second": b,
                }))
                .collect::<Vec<_>>(),
        }
    });

    println!("{}", serde_json::to_string_pretty(&output)?);
    Ok(())
}

/// Print semantic diff output in text format.
fn print_semantic_diff_text(diff: &ConversationDiff, first: &PathBuf, second: &PathBuf, args: &DiffArgs, type_filter: &Option<HashSet<String>>) -> Result<()> {
    if diff.is_identical() {
        if let Some(types) = type_filter {
            let types_str: Vec<&str> = types.iter().map(String::as_str).collect();
            println!("Conversations are semantically identical (filtered to: {}).", types_str.join(", "));
        } else {
            println!("Conversations are semantically identical.");
        }
        return Ok(());
    }

    println!("Comparing (semantic):");
    println!("  A: {}", first.display());
    println!("  B: {}", second.display());
    if let Some(types) = type_filter {
        let types_str: Vec<&str> = types.iter().map(String::as_str).collect();
        println!("  Filter: {} messages only", types_str.join(", "));
    }
    println!();

    // Summary
    println!("Summary:");
    println!("  Messages in A:      {}", diff.first_message_count);
    println!("  Messages in B:      {}", diff.second_message_count);
    println!("  Common messages:    {}", diff.common_messages);
    println!("  Added in B:         {}", diff.added_messages);
    println!("  Removed from A:     {}", diff.removed_messages);
    println!();

    // Detailed breakdown if requested
    if !args.summary_only {
        // Message type breakdown
        println!("Message Type Breakdown:");
        println!("  A: {} user, {} assistant", diff.first_user_count, diff.first_assistant_count);
        println!("  B: {} user, {} assistant", diff.second_user_count, diff.second_assistant_count);
        println!();

        // Branch analysis
        if diff.first_branch_count > 0 || diff.second_branch_count > 0 {
            println!("Branch Analysis:");
            println!("  A branches: {}", diff.first_branch_count);
            println!("  B branches: {}", diff.second_branch_count);
            println!();
        }
    }

    Ok(())
}

/// Print semantic diff output in JSON format.
fn print_semantic_diff_json(diff: &ConversationDiff, first: &PathBuf, second: &PathBuf, type_filter: &Option<HashSet<String>>) -> Result<()> {
    let filter_types: Option<Vec<&String>> = type_filter.as_ref().map(|t| t.iter().collect());

    let output = serde_json::json!({
        "mode": "semantic",
        "identical": diff.is_identical(),
        "first": first.to_string_lossy(),
        "second": second.to_string_lossy(),
        "filter": filter_types,
        "summary": {
            "first_message_count": diff.first_message_count,
            "second_message_count": diff.second_message_count,
            "common_messages": diff.common_messages,
            "added_messages": diff.added_messages,
            "removed_messages": diff.removed_messages,
        },
        "details": {
            "first": {
                "user_messages": diff.first_user_count,
                "assistant_messages": diff.first_assistant_count,
                "branches": diff.first_branch_count,
            },
            "second": {
                "user_messages": diff.second_user_count,
                "assistant_messages": diff.second_assistant_count,
                "branches": diff.second_branch_count,
            }
        }
    });

    println!("{}", serde_json::to_string_pretty(&output)?);
    Ok(())
}

/// Truncate a line for display.
fn truncate_line(line: &str, max_len: usize) -> String {
    if line.len() <= max_len {
        line.to_string()
    } else {
        format!("{}...", &line[..max_len - 3])
    }
}

/// Compare conversations semantically (by message structure, not raw text).
pub fn compare_conversations(first: &Conversation, second: &Conversation) -> ConversationDiff {
    let mut diff = ConversationDiff {
        first_message_count: first.len(),
        second_message_count: second.len(),
        ..Default::default()
    };

    // Compare by UUIDs from nodes
    let first_uuids: std::collections::HashSet<&str> = first.nodes().keys()
        .map(String::as_str)
        .collect();
    let second_uuids: std::collections::HashSet<&str> = second.nodes().keys()
        .map(String::as_str)
        .collect();

    diff.added_messages = second_uuids.difference(&first_uuids).count();
    diff.removed_messages = first_uuids.difference(&second_uuids).count();
    diff.common_messages = first_uuids.intersection(&second_uuids).count();

    // Count message types in first conversation
    for node in first.nodes().values() {
        match node.entry.message_type() {
            "user" => diff.first_user_count += 1,
            "assistant" => diff.first_assistant_count += 1,
            _ => {}
        }
    }

    // Count message types in second conversation
    for node in second.nodes().values() {
        match node.entry.message_type() {
            "user" => diff.second_user_count += 1,
            "assistant" => diff.second_assistant_count += 1,
            _ => {}
        }
    }

    // Count branches
    diff.first_branch_count = first.branch_count();
    diff.second_branch_count = second.branch_count();

    diff
}

/// Semantic conversation diff result.
#[derive(Debug, Default)]
pub struct ConversationDiff {
    /// Message count in first conversation.
    pub first_message_count: usize,
    /// Message count in second conversation.
    pub second_message_count: usize,
    /// Messages added in second.
    pub added_messages: usize,
    /// Messages removed from first.
    pub removed_messages: usize,
    /// Common messages (by UUID).
    pub common_messages: usize,
    /// User message count in first.
    pub first_user_count: usize,
    /// Assistant message count in first.
    pub first_assistant_count: usize,
    /// User message count in second.
    pub second_user_count: usize,
    /// Assistant message count in second.
    pub second_assistant_count: usize,
    /// Branch count in first.
    pub first_branch_count: usize,
    /// Branch count in second.
    pub second_branch_count: usize,
}

impl ConversationDiff {
    /// Check if conversations are semantically identical.
    #[must_use]
    pub fn is_identical(&self) -> bool {
        self.added_messages == 0 && self.removed_messages == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_conversation_diff_identical() {
        let diff = ConversationDiff {
            added_messages: 0,
            removed_messages: 0,
            ..Default::default()
        };
        assert!(diff.is_identical());
    }

    #[test]
    fn test_conversation_diff_different() {
        let diff = ConversationDiff {
            added_messages: 1,
            removed_messages: 0,
            ..Default::default()
        };
        assert!(!diff.is_identical());
    }

    #[test]
    fn test_truncate_line_short() {
        assert_eq!(truncate_line("hello", 10), "hello");
    }

    #[test]
    fn test_truncate_line_long() {
        assert_eq!(truncate_line("hello world this is a long line", 15), "hello world ...");
    }
}
