//! File recovery command implementation.
//!
//! Recovers files from Write/Edit operations recorded in Claude Code session logs.
//! Supports reconstructing final file state by applying Edit operations in sequence.

use std::collections::HashMap;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use globset::{Glob, GlobMatcher};
use serde::Serialize;

use crate::cli::{Cli, OutputFormat, RecoverArgs};
use crate::error::{Result, SnatchError};
use crate::model::{ContentBlock, LogEntry};
use crate::reconstruction::Conversation;

use super::get_claude_dir;

/// A file operation extracted from the session.
#[derive(Debug, Clone)]
pub struct FileOperation {
    /// The file path.
    pub file_path: String,
    /// Operation type.
    pub op_type: FileOpType,
    /// Content for Write operations.
    pub content: Option<String>,
    /// Old string for Edit operations.
    pub old_string: Option<String>,
    /// New string for Edit operations.
    pub new_string: Option<String>,
    /// Replace all flag for Edit operations.
    pub replace_all: bool,
    /// Timestamp of the operation.
    pub timestamp: DateTime<Utc>,
    /// Index in the session (for ordering).
    pub index: usize,
}

/// Type of file operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileOpType {
    /// File was written (created or overwritten).
    Write,
    /// File was edited (partial replacement).
    Edit,
}

impl std::fmt::Display for FileOpType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Write => write!(f, "Write"),
            Self::Edit => write!(f, "Edit"),
        }
    }
}

/// Recovered file information.
#[derive(Debug, Clone, Serialize)]
pub struct RecoveredFile {
    /// Original absolute path from the session.
    pub original_path: String,
    /// Relative path for output.
    pub output_path: String,
    /// File content.
    #[serde(skip)]
    pub content: String,
    /// Number of Write operations.
    pub write_count: usize,
    /// Number of Edit operations applied.
    pub edit_count: usize,
    /// Whether reconstruction was complete (all edits applied successfully).
    pub complete: bool,
    /// Any warnings during reconstruction.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
    /// Content size in bytes.
    pub size_bytes: usize,
    /// Number of lines.
    pub line_count: usize,
}

/// Run the recover command.
pub fn run(cli: &Cli, args: &RecoverArgs) -> Result<()> {
    let claude_dir = get_claude_dir(cli.claude_dir.as_ref())?;

    // Find the session
    let sessions = claude_dir.all_sessions()?;
    let session = sessions
        .iter()
        .find(|s| {
            s.session_id().starts_with(&args.session) || s.session_id() == args.session
        })
        .ok_or_else(|| SnatchError::SessionNotFound {
            session_id: args.session.clone(),
        })?;

    // Parse the session
    let entries = session.parse_with_options(cli.max_file_size)?;
    let conversation = Conversation::from_entries(entries)?;

    // Extract file operations
    let operations = extract_file_operations(&conversation, args.main_thread);

    if operations.is_empty() {
        if !args.quiet {
            eprintln!("No file operations found in session.");
        }
        return Ok(());
    }

    // Group operations by file path
    let mut files_map: HashMap<String, Vec<FileOperation>> = HashMap::new();
    for op in operations {
        files_map.entry(op.file_path.clone()).or_default().push(op);
    }

    // Sort operations within each file by index
    for ops in files_map.values_mut() {
        ops.sort_by_key(|op| op.index);
    }

    // Filter by file pattern if specified
    let file_pattern: Option<GlobMatcher> = args.file.as_ref().and_then(|p| {
        Glob::new(p).ok().map(|g| g.compile_matcher())
    });

    // Recover files
    let mut recovered_files: Vec<RecoveredFile> = Vec::new();

    for (file_path, ops) in &files_map {
        // Apply file pattern filter
        if let Some(ref pattern) = file_pattern {
            let file_name = Path::new(file_path)
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or(file_path);
            if !pattern.is_match(file_name) && !pattern.is_match(file_path) {
                continue;
            }
        }

        let recovered = recover_file(file_path, ops, args)?;
        recovered_files.push(recovered);
    }

    if recovered_files.is_empty() {
        if !args.quiet {
            eprintln!("No files matched the filter criteria.");
        }
        return Ok(());
    }

    // Sort by output path for consistent output
    recovered_files.sort_by(|a, b| a.output_path.cmp(&b.output_path));

    // Output based on format
    match cli.effective_output() {
        OutputFormat::Json => {
            let json = if cli.verbose {
                serde_json::to_string_pretty(&recovered_files)?
            } else {
                serde_json::to_string(&recovered_files)?
            };
            println!("{json}");
        }
        OutputFormat::Tsv => {
            println!("path\tsize\tlines\twrites\tedits\tcomplete");
            for f in &recovered_files {
                println!(
                    "{}\t{}\t{}\t{}\t{}\t{}",
                    f.output_path, f.size_bytes, f.line_count, f.write_count, f.edit_count, f.complete
                );
            }
        }
        OutputFormat::Text | OutputFormat::Compact => {
            if args.preview {
                // Preview mode - just show what would be recovered
                println!("Files to recover:");
                println!();
                for f in &recovered_files {
                    let status = if f.complete { "✓" } else { "⚠" };
                    println!(
                        "  {} {} ({} bytes, {} lines, {} writes, {} edits)",
                        status, f.output_path, f.size_bytes, f.line_count, f.write_count, f.edit_count
                    );
                    if args.verbose && !f.warnings.is_empty() {
                        for warn in &f.warnings {
                            println!("    Warning: {warn}");
                        }
                    }
                }
                println!();
                println!(
                    "Total: {} files ({} bytes)",
                    recovered_files.len(),
                    recovered_files.iter().map(|f| f.size_bytes).sum::<usize>()
                );
            } else {
                // Actually write files
                write_recovered_files(&recovered_files, args)?;
            }
        }
    }

    Ok(())
}

/// Extract file operations from the conversation.
fn extract_file_operations(conversation: &Conversation, main_thread_only: bool) -> Vec<FileOperation> {
    let mut operations = Vec::new();
    let mut index = 0;

    let entries = if main_thread_only {
        conversation.main_thread_entries()
    } else {
        conversation.chronological_entries()
    };

    for entry in entries {
        if let LogEntry::Assistant(asst_msg) = entry {
            let timestamp = asst_msg.timestamp;

            for block in &asst_msg.message.content {
                if let ContentBlock::ToolUse(tool_use) = block {
                    let input = &tool_use.input;

                    match tool_use.name.as_str() {
                        "Write" => {
                            if let (Some(path), Some(content)) = (
                                input.get("file_path").and_then(|v| v.as_str()),
                                input.get("content").and_then(|v| v.as_str()),
                            ) {
                                operations.push(FileOperation {
                                    file_path: path.to_string(),
                                    op_type: FileOpType::Write,
                                    content: Some(content.to_string()),
                                    old_string: None,
                                    new_string: None,
                                    replace_all: false,
                                    timestamp,
                                    index,
                                });
                                index += 1;
                            }
                        }
                        "Edit" => {
                            if let (Some(path), Some(old), Some(new)) = (
                                input.get("file_path").and_then(|v| v.as_str()),
                                input.get("old_string").and_then(|v| v.as_str()),
                                input.get("new_string").and_then(|v| v.as_str()),
                            ) {
                                let replace_all = input
                                    .get("replace_all")
                                    .and_then(|v| v.as_bool())
                                    .unwrap_or(false);

                                operations.push(FileOperation {
                                    file_path: path.to_string(),
                                    op_type: FileOpType::Edit,
                                    content: None,
                                    old_string: Some(old.to_string()),
                                    new_string: Some(new.to_string()),
                                    replace_all,
                                    timestamp,
                                    index,
                                });
                                index += 1;
                            }
                        }
                        _ => {}
                    }
                }
            }
        }
    }

    operations
}

/// Recover a single file from its operations.
fn recover_file(
    file_path: &str,
    operations: &[FileOperation],
    args: &RecoverArgs,
) -> Result<RecoveredFile> {
    let mut content = String::new();
    let mut write_count = 0;
    let mut edit_count = 0;
    let mut complete = true;
    let mut warnings = Vec::new();

    // Find all Write operations and use the last one as the base
    let writes: Vec<_> = operations
        .iter()
        .filter(|op| op.op_type == FileOpType::Write)
        .collect();

    let edits: Vec<_> = operations
        .iter()
        .filter(|op| op.op_type == FileOpType::Edit)
        .collect();

    if writes.is_empty() && !edits.is_empty() {
        // File was only edited, not written in this session
        // We can't recover without the original content
        warnings.push("File was edited but not written in this session; cannot recover without original content".to_string());
        complete = false;
    } else if !writes.is_empty() {
        // Get the last Write as the base content
        let last_write = writes.last().unwrap();
        content = last_write.content.clone().unwrap_or_default();
        write_count = writes.len();

        // Apply edits if requested
        if args.apply_edits {
            // Only apply edits that come AFTER the last write
            let last_write_index = last_write.index;

            for edit in &edits {
                if edit.index > last_write_index {
                    let old_str = edit.old_string.as_deref().unwrap_or("");
                    let new_str = edit.new_string.as_deref().unwrap_or("");

                    if edit.replace_all {
                        if content.contains(old_str) {
                            content = content.replace(old_str, new_str);
                            edit_count += 1;
                        } else {
                            warnings.push(format!(
                                "Edit pattern not found (replace_all): '{}'",
                                truncate_for_display(old_str, 50)
                            ));
                            complete = false;
                        }
                    } else {
                        // Replace only first occurrence
                        if let Some(pos) = content.find(old_str) {
                            content = format!(
                                "{}{}{}",
                                &content[..pos],
                                new_str,
                                &content[pos + old_str.len()..]
                            );
                            edit_count += 1;
                        } else {
                            warnings.push(format!(
                                "Edit pattern not found: '{}'",
                                truncate_for_display(old_str, 50)
                            ));
                            complete = false;
                        }
                    }
                }
            }
        }
    }

    // Calculate output path
    let output_path = calculate_output_path(file_path, args);

    Ok(RecoveredFile {
        original_path: file_path.to_string(),
        output_path,
        size_bytes: content.len(),
        line_count: content.lines().count(),
        content,
        write_count,
        edit_count,
        complete,
        warnings,
    })
}

/// Calculate the output path for a file.
fn calculate_output_path(file_path: &str, args: &RecoverArgs) -> String {
    let path = Path::new(file_path);

    // Strip prefix if specified
    let relative_path = if let Some(ref prefix) = args.strip_prefix {
        path.strip_prefix(prefix)
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|_| path.to_path_buf())
    } else {
        // If no prefix specified, try to make it relative by stripping leading /
        let path_str = file_path.trim_start_matches('/');
        PathBuf::from(path_str)
    };

    relative_path.to_string_lossy().to_string()
}

/// Write recovered files to disk.
fn write_recovered_files(files: &[RecoveredFile], args: &RecoverArgs) -> Result<()> {
    let output_dir = &args.output_dir;

    // Create output directory if it doesn't exist
    if !output_dir.exists() {
        fs::create_dir_all(output_dir)?;
    }

    let mut written = 0;
    let mut skipped = 0;
    let mut errors = 0;

    for file in files {
        if file.content.is_empty() && file.write_count == 0 {
            if args.verbose && !args.quiet {
                eprintln!("Skipping {} (no content to recover)", file.output_path);
            }
            skipped += 1;
            continue;
        }

        let full_path = output_dir.join(&file.output_path);

        // Check if file exists
        if full_path.exists() && !args.overwrite {
            if !args.quiet {
                eprintln!(
                    "Skipping {} (already exists, use --overwrite to replace)",
                    full_path.display()
                );
            }
            skipped += 1;
            continue;
        }

        // Create parent directories
        if let Some(parent) = full_path.parent() {
            if !parent.exists() {
                if let Err(e) = fs::create_dir_all(parent) {
                    eprintln!("Error creating directory {}: {}", parent.display(), e);
                    errors += 1;
                    continue;
                }
            }
        }

        // Write the file
        match fs::File::create(&full_path) {
            Ok(mut f) => {
                if let Err(e) = f.write_all(file.content.as_bytes()) {
                    eprintln!("Error writing {}: {}", full_path.display(), e);
                    errors += 1;
                    continue;
                }

                if args.verbose && !args.quiet {
                    let status = if file.complete { "✓" } else { "⚠" };
                    eprintln!(
                        "{} {} ({} bytes, {} writes, {} edits)",
                        status,
                        full_path.display(),
                        file.size_bytes,
                        file.write_count,
                        file.edit_count
                    );
                    for warn in &file.warnings {
                        eprintln!("  Warning: {warn}");
                    }
                }

                written += 1;
            }
            Err(e) => {
                eprintln!("Error creating {}: {}", full_path.display(), e);
                errors += 1;
            }
        }
    }

    if !args.quiet {
        println!();
        println!(
            "Recovery complete: {} written, {} skipped, {} errors",
            written, skipped, errors
        );
        println!("Output directory: {}", output_dir.display());
    }

    Ok(())
}

/// Truncate a string for display.
fn truncate_for_display(s: &str, max_len: usize) -> String {
    if s.len() <= max_len {
        s.to_string()
    } else {
        format!("{}...", &s[..max_len])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_calculate_output_path_no_prefix() {
        let args = RecoverArgs {
            session: "test".to_string(),
            output_dir: PathBuf::from("."),
            file: None,
            apply_edits: false,
            strip_prefix: None,
            preview: false,
            overwrite: false,
            main_thread: false,
            quiet: false,
            verbose: false,
        };

        let result = calculate_output_path("/home/user/project/src/main.rs", &args);
        assert_eq!(result, "home/user/project/src/main.rs");
    }

    #[test]
    fn test_calculate_output_path_with_prefix() {
        let args = RecoverArgs {
            session: "test".to_string(),
            output_dir: PathBuf::from("."),
            file: None,
            apply_edits: false,
            strip_prefix: Some("/home/user/project".to_string()),
            preview: false,
            overwrite: false,
            main_thread: false,
            quiet: false,
            verbose: false,
        };

        let result = calculate_output_path("/home/user/project/src/main.rs", &args);
        assert_eq!(result, "src/main.rs");
    }

    #[test]
    fn test_truncate_for_display() {
        assert_eq!(truncate_for_display("hello", 10), "hello");
        assert_eq!(truncate_for_display("hello world", 5), "hello...");
    }
}
