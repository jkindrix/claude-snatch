//! Diff command implementation.
//!
//! Compares two sessions or exported files to show differences.
//! Supports both line-aligned JSONL diff and ordered semantic-payload diff.

use std::collections::BTreeSet;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use crate::cli::{Cli, DiffArgs, OutputFormat};
use crate::discovery::ClaudeDirectory;
use crate::error::{Result, SnatchError};
use crate::export::JsonlDiff;
use crate::model::LogEntry;
use crate::parser::JsonlParser;
use crate::provider::registry::{cached_parsed_session, ProviderRegistry};
use crate::provider::{EntrySemantics, PromptAuthorship, ToolKind};
use crate::reconstruction::Conversation;
use serde_json::Value;
use similar::{Algorithm, ChangeTag};

use super::get_claude_dir;

/// Build the set of message types to include from args.
/// Returns None if no filtering should be applied (include all types).
fn build_type_filter(args: &DiffArgs) -> Option<BTreeSet<String>> {
    let mut types = BTreeSet::new();

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

#[derive(Debug, Clone)]
struct DiffSource {
    display: String,
    provider: Option<String>,
    qualified_id: Option<String>,
}

impl DiffSource {
    fn classic(path: &Path) -> Self {
        Self {
            display: path.display().to_string(),
            provider: None,
            qualified_id: None,
        }
    }

    fn provider(key: &crate::provider::LogicalSessionKey) -> Self {
        Self {
            display: key.to_string(),
            provider: Some(key.provider.to_string()),
            qualified_id: Some(key.to_string()),
        }
    }
}

struct SemanticTarget {
    source: DiffSource,
    conversation: Conversation,
}

struct LineTarget {
    source: DiffSource,
    content: String,
}

/// Run the diff command.
pub fn run(cli: &Cli, args: &DiffArgs) -> Result<()> {
    let registry =
        (!args.provider.is_empty() || args.first.contains(':') || args.second.contains(':'))
            .then(|| super::helpers::provider_registry(cli));
    let first_provider = target_uses_provider(registry.as_ref(), &args.provider, &args.first);
    let second_provider = target_uses_provider(registry.as_ref(), &args.provider, &args.second);
    let claude_dir = if first_provider && second_provider {
        None
    } else {
        Some(get_claude_dir(cli.claude_dir.as_ref())?)
    };

    // Default to semantic diff (more useful for JSONL sessions)
    // Use line-based only if explicitly requested
    if args.line_based {
        let first = load_line_target(
            cli,
            args,
            &args.first,
            first_provider,
            registry.as_ref(),
            claude_dir.as_ref(),
        )?;
        let second = load_line_target(
            cli,
            args,
            &args.second,
            second_provider,
            registry.as_ref(),
            claude_dir.as_ref(),
        )?;
        run_line_diff(cli, args, &first, &second)
    } else {
        let first = load_semantic_target(
            cli,
            args,
            &args.first,
            first_provider,
            registry.as_ref(),
            claude_dir.as_ref(),
        )?;
        let second = load_semantic_target(
            cli,
            args,
            &args.second,
            second_provider,
            registry.as_ref(),
            claude_dir.as_ref(),
        )?;
        run_semantic_diff(cli, args, &first, &second)
    }
}

fn target_uses_provider(
    registry: Option<&ProviderRegistry>,
    provider_flags: &[String],
    reference: &str,
) -> bool {
    !provider_flags.is_empty()
        || registry.is_some_and(|registry| registry.looks_qualified(reference))
}

fn resolve_classic_path(claude_dir: &ClaudeDirectory, reference: &str) -> Result<PathBuf> {
    if let Some(session) = claude_dir.find_session(reference)? {
        return Ok(session.path().to_path_buf());
    }
    let path = PathBuf::from(reference);
    if path.exists() {
        Ok(path)
    } else {
        Err(SnatchError::FileNotFound { path })
    }
}

fn provider_registry(registry: Option<&ProviderRegistry>) -> Result<&ProviderRegistry> {
    registry.ok_or_else(|| SnatchError::InvalidArgument {
        name: "provider".to_string(),
        reason: "provider routing was requested but no registry was constructed".to_string(),
    })
}

fn load_semantic_target(
    cli: &Cli,
    args: &DiffArgs,
    reference: &str,
    use_provider: bool,
    registry: Option<&ProviderRegistry>,
    claude_dir: Option<&ClaudeDirectory>,
) -> Result<SemanticTarget> {
    if use_provider {
        let resolution =
            provider_registry(registry)?.resolve_with_default_policy(&args.provider, reference)?;
        let parsed = cached_parsed_session(
            crate::cache::global_cache(),
            resolution.provider,
            &resolution.key,
        )?;
        return Ok(SemanticTarget {
            source: DiffSource::provider(&resolution.key),
            conversation: Conversation::from_parsed_session(parsed)?,
        });
    }

    let path = resolve_classic_path(
        claude_dir.expect("classic target construction requires a Claude directory"),
        reference,
    )?;
    let parser = JsonlParser::new().with_lenient(true);
    let mut parser = match cli.max_file_size {
        Some(limit) => parser.with_max_file_size(limit),
        None => parser,
    };
    let entries = parser.parse_file(&path)?;
    Ok(SemanticTarget {
        source: DiffSource::classic(&path),
        conversation: Conversation::from_entries(entries)?,
    })
}

fn load_line_target(
    cli: &Cli,
    args: &DiffArgs,
    reference: &str,
    use_provider: bool,
    registry: Option<&ProviderRegistry>,
    claude_dir: Option<&ClaudeDirectory>,
) -> Result<LineTarget> {
    if use_provider {
        let resolution =
            provider_registry(registry)?.resolve_with_default_policy(&args.provider, reference)?;
        if !resolution.provider.capabilities().raw_jsonl {
            return Err(crate::provider::ProviderError::Unsupported {
                capability: "raw-jsonl line comparison",
            }
            .into());
        }
        let mut bytes = BoundedBytes::new(cli.max_file_size.filter(|limit| *limit != 0));
        resolution
            .provider
            .write_raw_jsonl(&resolution.key, &mut bytes)?;
        let content =
            String::from_utf8(bytes.into_inner()).map_err(|_| SnatchError::InvalidArgument {
                name: reference.to_string(),
                reason: "raw JSONL comparison requires valid UTF-8 records".to_string(),
            })?;
        return Ok(LineTarget {
            source: DiffSource::provider(&resolution.key),
            content,
        });
    }

    let path = resolve_classic_path(
        claude_dir.expect("classic target construction requires a Claude directory"),
        reference,
    )?;
    if let Some(limit) = cli.max_file_size.filter(|limit| *limit != 0) {
        let size = fs::metadata(&path)
            .map_err(|e| SnatchError::io(format!("Failed to inspect file: {}", path.display()), e))?
            .len();
        if size > limit {
            return Err(SnatchError::InvalidArgument {
                name: reference.to_string(),
                reason: format!("file is {size} bytes, exceeding the {limit}-byte limit"),
            });
        }
    }
    let content = fs::read_to_string(&path)
        .map_err(|e| SnatchError::io(format!("Failed to read file: {}", path.display()), e))?;
    Ok(LineTarget {
        source: DiffSource::classic(&path),
        content,
    })
}

struct BoundedBytes {
    bytes: Vec<u8>,
    max: Option<u64>,
}

impl BoundedBytes {
    fn new(max: Option<u64>) -> Self {
        Self {
            bytes: Vec::new(),
            max,
        }
    }

    fn into_inner(self) -> Vec<u8> {
        self.bytes
    }
}

impl Write for BoundedBytes {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        if self.max.is_some_and(|max| {
            u64::try_from(self.bytes.len())
                .unwrap_or(u64::MAX)
                .saturating_add(u64::try_from(buf.len()).unwrap_or(u64::MAX))
                > max
        }) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "raw JSONL exceeds the configured size limit",
            ));
        }
        self.bytes.extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// Run line-based diff comparison.
fn run_line_diff(
    cli: &Cli,
    args: &DiffArgs,
    first: &LineTarget,
    second: &LineTarget,
) -> Result<()> {
    // Perform the diff
    let diff = JsonlDiff::compare(&first.content, &second.content);

    // Output based on format
    match cli.effective_output() {
        OutputFormat::Json => print_line_diff_json(&diff, &first.source, &second.source)?,
        _ => print_line_diff_text(&diff, &first.source, &second.source, args)?,
    }

    // Exit with appropriate code
    if !diff.is_identical() && args.exit_code {
        std::process::exit(1);
    }

    Ok(())
}

/// Run semantic diff comparison.
fn run_semantic_diff(
    cli: &Cli,
    args: &DiffArgs,
    first: &SemanticTarget,
    second: &SemanticTarget,
) -> Result<()> {
    let type_filter = build_type_filter(args);
    let diff = compare_conversations_filtered(
        &first.conversation,
        &second.conversation,
        type_filter.as_ref(),
        args.prompts,
    );

    // Output based on format
    match cli.effective_output() {
        OutputFormat::Json => {
            print_semantic_diff_json(&diff, &first.source, &second.source, type_filter.as_ref())?;
        }
        _ => print_semantic_diff_text(
            &diff,
            &first.source,
            &second.source,
            args,
            type_filter.as_ref(),
        )?,
    }

    // Exit with appropriate code
    if !diff.is_identical() && args.exit_code {
        std::process::exit(1);
    }

    Ok(())
}

/// Print line-based diff output in text format.
fn print_line_diff_text(
    diff: &JsonlDiff,
    first: &DiffSource,
    second: &DiffSource,
    args: &DiffArgs,
) -> Result<()> {
    if diff.is_identical() {
        println!("Files are identical.");
        return Ok(());
    }

    println!("Comparing (line-based):");
    println!("  A: {}", first.display);
    println!("  B: {}", second.display);
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
fn print_line_diff_json(diff: &JsonlDiff, first: &DiffSource, second: &DiffSource) -> Result<()> {
    let mut output = serde_json::json!({
        "mode": "line-based",
        "identical": diff.is_identical(),
        "first": first.display,
        "second": second.display,
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
    add_source_identity(&mut output, first, second);

    println!("{}", serde_json::to_string_pretty(&output)?);
    Ok(())
}

/// Print semantic diff output in text format.
fn print_semantic_diff_text(
    diff: &ConversationDiff,
    first: &DiffSource,
    second: &DiffSource,
    args: &DiffArgs,
    type_filter: Option<&BTreeSet<String>>,
) -> Result<()> {
    if diff.is_identical() {
        if let Some(types) = type_filter {
            let types_str: Vec<&str> = types.iter().map(String::as_str).collect();
            println!(
                "Conversations are semantically identical (filtered to: {}).",
                types_str.join(", ")
            );
        } else {
            println!("Conversations are semantically identical.");
        }
        return Ok(());
    }

    println!("Comparing (semantic):");
    println!("  A: {}", first.display);
    println!("  B: {}", second.display);
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
        println!(
            "  A: {} user, {} assistant",
            diff.first_user_count, diff.first_assistant_count
        );
        println!(
            "  B: {} user, {} assistant",
            diff.second_user_count, diff.second_assistant_count
        );
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
fn print_semantic_diff_json(
    diff: &ConversationDiff,
    first: &DiffSource,
    second: &DiffSource,
    type_filter: Option<&BTreeSet<String>>,
) -> Result<()> {
    let filter_types: Option<Vec<&String>> = type_filter.as_ref().map(|t| t.iter().collect());

    let mut output = serde_json::json!({
        "mode": "semantic",
        "identical": diff.is_identical(),
        "first": first.display,
        "second": second.display,
        "comparison_basis": "ordered_identity_neutral_payloads",
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
    add_source_identity(&mut output, first, second);

    println!("{}", serde_json::to_string_pretty(&output)?);
    Ok(())
}

fn add_source_identity(output: &mut Value, first: &DiffSource, second: &DiffSource) {
    let Some(object) = output.as_object_mut() else {
        return;
    };
    if first.provider.is_some() || second.provider.is_some() {
        object.insert(
            "first_source".to_string(),
            serde_json::json!({
                "provider": first.provider,
                "qualified_id": first.qualified_id,
            }),
        );
        object.insert(
            "second_source".to_string(),
            serde_json::json!({
                "provider": second.provider,
                "qualified_id": second.qualified_id,
            }),
        );
    }
}

/// Truncate a line for display.
fn truncate_line(line: &str, max_len: usize) -> String {
    if line.len() <= max_len {
        line.to_string()
    } else {
        format!("{}...", &line[..max_len - 3])
    }
}

/// Compare conversations semantically by ordered, identity-neutral payloads.
pub fn compare_conversations(first: &Conversation, second: &Conversation) -> ConversationDiff {
    compare_conversations_filtered(first, second, None, false)
}

fn compare_conversations_filtered(
    first: &Conversation,
    second: &Conversation,
    type_filter: Option<&BTreeSet<String>>,
    human_prompts_only: bool,
) -> ConversationDiff {
    let first_entries = comparison_entries(first, type_filter, human_prompts_only);
    let second_entries = comparison_entries(second, type_filter, human_prompts_only);
    let first_fingerprints: Vec<String> = first_entries
        .iter()
        .map(|entry| semantic_fingerprint(entry.entry, entry.semantics))
        .collect();
    let second_fingerprints: Vec<String> = second_entries
        .iter()
        .map(|entry| semantic_fingerprint(entry.entry, entry.semantics))
        .collect();

    let mut diff = ConversationDiff {
        first_message_count: first_entries.len(),
        second_message_count: second_entries.len(),
        ..Default::default()
    };

    for (tag, slice) in
        similar::utils::diff_slices(Algorithm::Myers, &first_fingerprints, &second_fingerprints)
    {
        match tag {
            ChangeTag::Equal => diff.common_messages += slice.len(),
            ChangeTag::Delete => diff.removed_messages += slice.len(),
            ChangeTag::Insert => diff.added_messages += slice.len(),
        }
    }

    // Count message types. Assistant turns dedup streaming chunks (one turn is
    // written as several nodes sharing a message.id), matching get_session_info
    // and CLI info.
    diff.first_user_count = first_entries
        .iter()
        .filter(|entry| entry.entry.message_type() == "user")
        .count();
    diff.second_user_count = second_entries
        .iter()
        .filter(|entry| entry.entry.message_type() == "user")
        .count();
    diff.first_assistant_count = assistant_group_count(&first_entries);
    diff.second_assistant_count = assistant_group_count(&second_entries);

    // Count branches
    diff.first_branch_count = first.branch_count();
    diff.second_branch_count = second.branch_count();

    diff
}

#[derive(Clone, Copy)]
struct ComparisonEntry<'a> {
    entry: &'a LogEntry,
    semantics: Option<&'a EntrySemantics>,
}

fn comparison_entries<'a>(
    conversation: &'a Conversation,
    type_filter: Option<&BTreeSet<String>>,
    human_prompts_only: bool,
) -> Vec<ComparisonEntry<'a>> {
    let bundle = conversation.provider_bundle();
    conversation
        .identified_entries_for_export(false)
        .into_iter()
        .map(|(entry, id)| ComparisonEntry {
            entry,
            semantics: id.and_then(|id| bundle.and_then(|bundle| bundle.semantics.get(id))),
        })
        .filter(|candidate| {
            type_filter
                .is_none_or(|types| types.contains(&candidate.entry.message_type().to_lowercase()))
        })
        .filter(|candidate| {
            if !human_prompts_only || candidate.entry.message_type() != "user" {
                return true;
            }
            candidate.semantics.map_or_else(
                || crate::analysis::extraction::is_human_prompt(candidate.entry),
                |semantics| {
                    semantics
                        .prompt
                        .as_ref()
                        .is_some_and(|prompt| prompt.authorship == PromptAuthorship::Human)
                },
            )
        })
        .collect()
}

fn assistant_group_count(entries: &[ComparisonEntry<'_>]) -> usize {
    entries
        .iter()
        .filter_map(|candidate| match candidate.entry {
            LogEntry::Assistant(message) => Some(message.message.id.as_str()),
            _ => None,
        })
        .collect::<BTreeSet<_>>()
        .len()
}

/// Produce a comparison key that retains the message's semantic payload but
/// excludes transport and provider identity. The normalization is deliberately
/// path-aware: removing every field named `id` recursively would erase ids in
/// tool input supplied by the user.
fn semantic_fingerprint(entry: &LogEntry, semantics: Option<&EntrySemantics>) -> String {
    let mut value = serde_json::to_value(entry).expect("LogEntry always serializes");
    let Some(top) = value.as_object_mut() else {
        return value.to_string();
    };

    for field in [
        "uuid",
        "parentUuid",
        "logicalParentUuid",
        "timestamp",
        "sessionId",
        "version",
        "cwd",
        "gitBranch",
        "userType",
        "isSidechain",
        "isTeammate",
        "agentId",
        "slug",
        "requestId",
    ] {
        top.remove(field);
    }

    match entry {
        LogEntry::Assistant(_) => {
            if let Some(message) = top.get_mut("message").and_then(Value::as_object_mut) {
                for field in [
                    "id",
                    "model",
                    "usage",
                    "stop_reason",
                    "stop_sequence",
                    "container",
                    "context_management",
                ] {
                    message.remove(field);
                }
                if let Some(content) = message.get_mut("content") {
                    normalize_content_blocks(content, semantics);
                }
            }
        }
        LogEntry::User(_) => {
            if let Some(content) = top
                .get_mut("message")
                .and_then(Value::as_object_mut)
                .and_then(|message| message.get_mut("content"))
            {
                normalize_content_blocks(content, semantics);
            }
        }
        LogEntry::System(_) => {
            for field in [
                "toolUseId",
                "checkpointId",
                "targetUuid",
                "retryInMs",
                "retryAttempt",
                "maxRetries",
            ] {
                top.remove(field);
            }
        }
        LogEntry::Summary(_) => {
            top.remove("leafUuid");
        }
        LogEntry::FileHistorySnapshot(_) => {
            top.remove("messageId");
            if let Some(snapshot) = top.get_mut("snapshot").and_then(Value::as_object_mut) {
                snapshot.remove("messageId");
                snapshot.remove("timestamp");
            }
        }
        _ => {}
    }

    sort_json_objects(&mut value);
    serde_json::to_string(&value).expect("normalized JSON value always serializes")
}

fn normalize_content_blocks(content: &mut Value, semantics: Option<&EntrySemantics>) {
    let Some(blocks) = content.as_array_mut() else {
        return;
    };
    for block in blocks {
        let Some(object) = block.as_object_mut() else {
            continue;
        };
        match object.get("type").and_then(Value::as_str) {
            Some("tool_use") => {
                let call_id = object.get("id").and_then(Value::as_str);
                let native_name = object.get("name").and_then(Value::as_str);
                let kind = call_id
                    .and_then(|call_id| semantics?.tools.get(call_id))
                    .map(|tool| tool.kind.clone())
                    .or_else(|| native_name.map(classify_native_tool));
                if let Some(kind) = kind {
                    object.insert("name".to_string(), Value::String(tool_kind_label(&kind)));
                }
                object.remove("id");
            }
            Some("tool_result") => {
                object.remove("tool_use_id");
                object.remove("toolUseId");
            }
            Some("thinking") => {
                object.remove("signature");
            }
            _ => {}
        }
    }
}

fn classify_native_tool(name: &str) -> ToolKind {
    match name {
        "Bash" | "shell" | "local_shell" | "exec_command" | "write_stdin" => ToolKind::Shell,
        "Read" | "read_file" | "view_image" => ToolKind::FileRead,
        "Write" | "Edit" | "MultiEdit" | "NotebookEdit" | "apply_patch" => ToolKind::FileWrite,
        "Glob" | "Grep" | "grep" | "find" | "search" => ToolKind::Search,
        "WebSearch" | "WebFetch" | "web_search" | "browser.search" | "web.run" => ToolKind::Web,
        "Task" | "Agent" => ToolKind::Subagent,
        "exec" | "wait" | "update_plan" => ToolKind::Orchestration,
        _ if name.starts_with("mcp__") || name.starts_with("mcp") || name.contains("__") => {
            ToolKind::Mcp
        }
        _ => ToolKind::Other(name.to_string()),
    }
}

fn tool_kind_label(kind: &ToolKind) -> String {
    match kind {
        ToolKind::Shell => "shell".to_string(),
        ToolKind::FileRead => "file_read".to_string(),
        ToolKind::FileWrite => "file_write".to_string(),
        ToolKind::Search => "search".to_string(),
        ToolKind::Web => "web".to_string(),
        ToolKind::Subagent => "subagent".to_string(),
        ToolKind::Mcp => "mcp".to_string(),
        ToolKind::Orchestration => "orchestration".to_string(),
        ToolKind::Other(native) => format!("other:{native}"),
    }
}

fn sort_json_objects(value: &mut Value) {
    match value {
        Value::Array(items) => {
            for item in items {
                sort_json_objects(item);
            }
        }
        Value::Object(object) => {
            let old = std::mem::take(object);
            let mut entries: Vec<_> = old.into_iter().collect();
            entries.sort_by(|(a, _), (b, _)| a.cmp(b));
            for (key, mut child) in entries {
                sort_json_objects(&mut child);
                object.insert(key, child);
            }
        }
        _ => {}
    }
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
    /// Common messages in the ordered identity-neutral payload diff.
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

    fn user_entry(
        uuid: &str,
        parent: Option<&str>,
        session: &str,
        timestamp: &str,
        text: &str,
    ) -> LogEntry {
        serde_json::from_value(serde_json::json!({
            "type": "user",
            "uuid": uuid,
            "parentUuid": parent,
            "timestamp": timestamp,
            "sessionId": session,
            "version": "2.1.0",
            "message": {"role": "user", "content": text},
        }))
        .unwrap()
    }

    fn conversation(texts: &[&str], identity_prefix: &str) -> Conversation {
        let mut parent = None;
        let entries = texts
            .iter()
            .enumerate()
            .map(|(index, text)| {
                let uuid = format!("{identity_prefix}-{index}");
                let entry = user_entry(
                    &uuid,
                    parent.as_deref(),
                    identity_prefix,
                    &format!("2026-01-01T00:00:{index:02}Z"),
                    text,
                );
                parent = Some(uuid);
                entry
            })
            .collect();
        Conversation::from_entries(entries).unwrap()
    }

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
    fn test_compare_conversations_dedups_streaming_chunks() {
        // A turn split into streaming chunks (shared message.id across nodes)
        // must count as one assistant turn, consistent with get_session_info.
        let chunk = |uuid: &str, parent: &str, msg_id: &str| {
            let json = format!(
                r#"{{"type":"assistant","uuid":"{uuid}","parentUuid":"{parent}","timestamp":"2026-01-01T00:00:00Z","sessionId":"s","version":"2.1.0","isSidechain":false,"message":{{"id":"{msg_id}","type":"message","role":"assistant","model":"m","content":[{{"type":"text","text":"x"}}]}}}}"#
            );
            serde_json::from_str::<crate::model::LogEntry>(&json).unwrap()
        };
        let entries = vec![
            chunk("c1", "root", "msg_A"),
            chunk("c2", "c1", "msg_A"),
            chunk("c3", "c2", "msg_B"),
        ];
        let conv = Conversation::from_entries(entries).unwrap();
        let diff = compare_conversations(&conv, &conv);

        // 3 chunk nodes, 2 distinct turns.
        assert_eq!(diff.first_assistant_count, 2);
        assert_eq!(diff.second_assistant_count, 2);
    }

    #[test]
    fn semantic_diff_ignores_transport_identity() {
        let first = conversation(&["same prompt", "same answer"], "session-a");
        let second = conversation(&["same prompt", "same answer"], "session-b");

        let diff = compare_conversations(&first, &second);
        assert!(diff.is_identical());
        assert_eq!(diff.common_messages, 2);
        assert_eq!(diff.added_messages, 0);
        assert_eq!(diff.removed_messages, 0);
    }

    #[test]
    fn semantic_diff_counts_repeated_emissions_independently() {
        let first = conversation(&["repeat", "repeat", "tail"], "session-a");
        let second = conversation(&["repeat", "tail"], "session-b");

        let diff = compare_conversations(&first, &second);
        assert!(!diff.is_identical());
        assert_eq!(diff.common_messages, 2);
        assert_eq!(diff.removed_messages, 1);
        assert_eq!(diff.added_messages, 0);
    }

    #[test]
    fn semantic_diff_treats_reordering_as_a_change() {
        let first = conversation(&["one", "two"], "session-a");
        let second = conversation(&["two", "one"], "session-b");

        let diff = compare_conversations(&first, &second);
        assert!(!diff.is_identical());
        assert_eq!(diff.common_messages, 1);
        assert_eq!(diff.removed_messages, 1);
        assert_eq!(diff.added_messages, 1);
    }

    #[test]
    fn semantic_diff_includes_uuidless_summary_entries() {
        let summary = |text: &str, leaf: &str| {
            serde_json::from_value::<LogEntry>(serde_json::json!({
                "type": "summary",
                "summary": text,
                "leafUuid": leaf,
            }))
            .unwrap()
        };
        let first = Conversation::from_entries(vec![summary("same summary", "native-a")]).unwrap();
        let second = Conversation::from_entries(vec![summary("same summary", "native-b")]).unwrap();
        assert!(compare_conversations(&first, &second).is_identical());

        let changed =
            Conversation::from_entries(vec![summary("changed summary", "native-c")]).unwrap();
        assert!(!compare_conversations(&first, &changed).is_identical());
    }

    #[test]
    fn semantic_diff_ignores_model_usage_and_native_tool_ids() {
        let assistant = |uuid: &str,
                         session: &str,
                         model: &str,
                         tool_id: &str,
                         tool_name: &str,
                         input_id: &str,
                         tokens: u64| {
            serde_json::from_value::<LogEntry>(serde_json::json!({
                "type": "assistant",
                "uuid": uuid,
                "parentUuid": null,
                "timestamp": "2026-01-01T00:00:00Z",
                "sessionId": session,
                "version": "2.1.0",
                "message": {
                    "id": format!("message-{uuid}"),
                    "type": "message",
                    "role": "assistant",
                    "model": model,
                    "usage": {"input_tokens": tokens, "output_tokens": tokens},
                    "content": [{
                        "type": "tool_use",
                        "id": tool_id,
                        "name": tool_name,
                        "input": {"file_path": "src/lib.rs", "id": input_id}
                    }]
                }
            }))
            .unwrap()
        };
        let first = Conversation::from_entries(vec![assistant(
            "a",
            "session-a",
            "model-a",
            "call-a",
            "Read",
            "user-supplied",
            10,
        )])
        .unwrap();
        let second = Conversation::from_entries(vec![assistant(
            "b",
            "session-b",
            "model-b",
            "call-b",
            "read_file",
            "user-supplied",
            999,
        )])
        .unwrap();

        let diff = compare_conversations(&first, &second);
        assert!(diff.is_identical());
        assert_eq!(diff.common_messages, 1);

        let changed_input = Conversation::from_entries(vec![assistant(
            "c",
            "session-c",
            "model-c",
            "call-c",
            "Read",
            "different-user-input",
            10,
        )])
        .unwrap();
        assert!(!compare_conversations(&first, &changed_input).is_identical());
    }

    #[test]
    fn test_truncate_line_short() {
        assert_eq!(truncate_line("hello", 10), "hello");
    }

    #[test]
    fn test_truncate_line_long() {
        assert_eq!(
            truncate_line("hello world this is a long line", 15),
            "hello world ..."
        );
    }
}
