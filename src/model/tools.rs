//! Tool-specific metadata structures for Claude Code JSONL logs.
//!
//! This module defines the `toolUseResult` structures for all 24+ Claude Code tools.
//! Each tool produces different metadata when executed.

use indexmap::IndexMap;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Tool use result - tool-specific execution metadata.
/// Stored in the `toolUseResult` field of user messages.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ToolUseResult {
    /// Glob tool result.
    Glob(GlobResult),
    /// Grep tool result.
    Grep(GrepResult),
    /// Read tool result.
    Read(ReadResult),
    /// Bash tool result.
    Bash(BashResult),
    /// WebFetch tool result.
    WebFetch(WebFetchResult),
    /// WebSearch tool result.
    WebSearch(WebSearchResult),
    /// Edit tool result.
    Edit(EditResult),
    /// MultiEdit tool result.
    MultiEdit(MultiEditResult),
    /// LS tool result.
    Ls(LsResult),
    /// Write tool result.
    Write(WriteResult),
    /// Task tool result.
    Task(TaskResult),
    /// TaskOutput tool result.
    TaskOutput(TaskOutputResult),
    /// KillShell tool result.
    KillShell(KillShellResult),
    /// NotebookEdit tool result.
    NotebookEdit(NotebookEditResult),
    /// NotebookRead tool result.
    NotebookRead(NotebookReadResult),
    /// TodoRead tool result.
    TodoRead(TodoReadResult),
    /// TodoWrite tool result.
    TodoWrite(TodoWriteResult),
    /// AskUserQuestion tool result.
    AskUserQuestion(AskUserQuestionResult),
    /// LSP tool result.
    Lsp(LspResult),
    /// EnterPlanMode tool result.
    EnterPlanMode(EnterPlanModeResult),
    /// ExitPlanMode tool result.
    ExitPlanMode(ExitPlanModeResult),
    /// Skill tool result.
    Skill(SkillResult),
    /// ListMcpResources tool result.
    ListMcpResources(ListMcpResourcesResult),
    /// ReadMcpResource tool result.
    ReadMcpResource(ReadMcpResourceResult),
    /// Generic/unknown tool result.
    Generic(Value),
}

/// Glob tool result.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GlobResult {
    /// Matched file paths.
    pub filenames: Vec<String>,
    /// Execution duration in milliseconds.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
    /// Number of files matched.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub num_files: Option<usize>,
    /// Whether output was truncated.
    #[serde(default)]
    pub truncated: bool,
    /// Unknown fields.
    #[serde(flatten)]
    pub extra: IndexMap<String, Value>,
}

/// Grep tool result.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GrepResult {
    /// Output mode used.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mode: Option<GrepMode>,
    /// Matched file paths.
    #[serde(default)]
    pub filenames: Vec<String>,
    /// Number of files with matches.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub num_files: Option<usize>,
    /// Unknown fields.
    #[serde(flatten)]
    pub extra: IndexMap<String, Value>,
}

/// Grep output mode.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GrepMode {
    /// Show only file paths.
    FilesWithMatches,
    /// Show matching content.
    Content,
    /// Show match counts.
    Count,
}

/// Read tool result.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReadResult {
    /// Content type.
    #[serde(rename = "type")]
    pub result_type: String,
    /// File information.
    pub file: ReadFileInfo,
    /// Unknown fields.
    #[serde(flatten)]
    pub extra: IndexMap<String, Value>,
}

/// Read file information.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReadFileInfo {
    /// File path.
    pub file_path: String,
    /// File content.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    /// Number of lines read.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub num_lines: Option<usize>,
    /// Starting line number.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub start_line: Option<usize>,
    /// Total lines in file.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total_lines: Option<usize>,
    /// Unknown fields.
    #[serde(flatten)]
    pub extra: IndexMap<String, Value>,
}

/// Bash tool result.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BashResult {
    /// Standard output.
    #[serde(default)]
    pub stdout: String,
    /// Standard error.
    #[serde(default)]
    pub stderr: String,
    /// Whether command was interrupted.
    #[serde(default)]
    pub interrupted: bool,
    /// Whether output contains image data.
    #[serde(default)]
    pub is_image: bool,
    /// Command exit code.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    /// Unknown fields.
    #[serde(flatten)]
    pub extra: IndexMap<String, Value>,
}

impl BashResult {
    /// Check if command succeeded (exit code 0).
    #[must_use]
    pub fn succeeded(&self) -> bool {
        self.exit_code == Some(0)
    }

    /// Check if command was interrupted or timed out.
    #[must_use]
    pub fn was_interrupted(&self) -> bool {
        self.interrupted
    }
}

/// WebFetch tool result.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WebFetchResult {
    /// Execution duration in milliseconds.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
    /// Response size in bytes.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bytes: Option<u64>,
    /// HTTP status code.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub code: Option<u16>,
    /// HTTP status text.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub code_text: Option<String>,
    /// Processed content.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<String>,
    /// Fetched URL.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    /// Unknown fields.
    #[serde(flatten)]
    pub extra: IndexMap<String, Value>,
}

impl WebFetchResult {
    /// Check if fetch succeeded (2xx status).
    #[must_use]
    pub fn succeeded(&self) -> bool {
        self.code.map_or(false, |c| (200..300).contains(&c))
    }
}

/// WebSearch tool result.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WebSearchResult {
    /// Search query.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub query: Option<String>,
    /// Search results.
    #[serde(default)]
    pub results: Vec<Value>,
    /// Search duration in seconds.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration_seconds: Option<f64>,
    /// Unknown fields.
    #[serde(flatten)]
    pub extra: IndexMap<String, Value>,
}

/// Edit tool result.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EditResult {
    /// File path.
    pub file_path: String,
    /// Original text replaced.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub old_string: Option<String>,
    /// Replacement text.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub new_string: Option<String>,
    /// Full original file content.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub original_file: Option<String>,
    /// Unified diff hunks.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub structured_patch: Vec<PatchHunk>,
    /// Unknown fields.
    #[serde(flatten)]
    pub extra: IndexMap<String, Value>,
}

/// A unified diff hunk.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PatchHunk {
    /// Starting line in original file.
    pub old_start: usize,
    /// Line count in original.
    pub old_lines: usize,
    /// Starting line in modified file.
    pub new_start: usize,
    /// Line count in modified.
    pub new_lines: usize,
    /// Diff lines with prefix (' ', '-', '+').
    pub lines: Vec<String>,
    /// Unknown fields.
    #[serde(flatten)]
    pub extra: IndexMap<String, Value>,
}

impl PatchHunk {
    /// Get added lines ('+' prefix).
    #[must_use]
    pub fn added_lines(&self) -> Vec<&str> {
        self.lines
            .iter()
            .filter(|l| l.starts_with('+'))
            .map(|l| l.strip_prefix('+').unwrap_or(l))
            .collect()
    }

    /// Get removed lines ('-' prefix).
    #[must_use]
    pub fn removed_lines(&self) -> Vec<&str> {
        self.lines
            .iter()
            .filter(|l| l.starts_with('-'))
            .map(|l| l.strip_prefix('-').unwrap_or(l))
            .collect()
    }

    /// Get context lines (' ' prefix).
    #[must_use]
    pub fn context_lines(&self) -> Vec<&str> {
        self.lines
            .iter()
            .filter(|l| l.starts_with(' '))
            .map(|l| l.strip_prefix(' ').unwrap_or(l))
            .collect()
    }
}

/// MultiEdit tool result.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MultiEditResult {
    /// File path.
    pub file_path: String,
    /// Number of edits applied.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub edits_applied: Option<usize>,
    /// Individual edit operations.
    #[serde(default)]
    pub edits: Vec<EditOperation>,
    /// Full original file content.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub original_file: Option<String>,
    /// Combined unified diff hunks.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub structured_patch: Vec<PatchHunk>,
    /// Unknown fields.
    #[serde(flatten)]
    pub extra: IndexMap<String, Value>,
}

/// A single edit operation within MultiEdit.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EditOperation {
    /// Original text.
    pub old_string: String,
    /// Replacement text.
    pub new_string: String,
    /// Replace all occurrences.
    #[serde(default)]
    pub replace_all: bool,
    /// Unknown fields.
    #[serde(flatten)]
    pub extra: IndexMap<String, Value>,
}

/// LS tool result.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LsResult {
    /// Directory path listed.
    pub path: String,
    /// Directory entries.
    pub entries: Vec<LsEntry>,
    /// Total entry count.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub num_entries: Option<usize>,
    /// Whether output was truncated.
    #[serde(default)]
    pub truncated: bool,
    /// Unknown fields.
    #[serde(flatten)]
    pub extra: IndexMap<String, Value>,
}

/// A directory entry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LsEntry {
    /// Entry name.
    pub name: String,
    /// Entry type ("file" or "directory").
    #[serde(rename = "type")]
    pub entry_type: String,
    /// Unknown fields.
    #[serde(flatten)]
    pub extra: IndexMap<String, Value>,
}

impl LsEntry {
    /// Check if this is a directory.
    #[must_use]
    pub fn is_directory(&self) -> bool {
        self.entry_type == "directory"
    }

    /// Check if this is a file.
    #[must_use]
    pub fn is_file(&self) -> bool {
        self.entry_type == "file"
    }
}

/// Write tool result.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WriteResult {
    /// File path written.
    pub file_path: String,
    /// Bytes written.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bytes_written: Option<u64>,
    /// Unknown fields.
    #[serde(flatten)]
    pub extra: IndexMap<String, Value>,
}

/// Task tool result.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskResult {
    /// Agent identifier.
    pub agent_id: String,
    /// Task description.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub task_description: Option<String>,
    /// Task status.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    /// Unknown fields.
    #[serde(flatten)]
    pub extra: IndexMap<String, Value>,
}

/// TaskOutput tool result.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskOutputResult {
    /// Task identifier.
    pub task_id: String,
    /// Task status.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    /// Task output.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output: Option<String>,
    /// Unknown fields.
    #[serde(flatten)]
    pub extra: IndexMap<String, Value>,
}

/// KillShell tool result.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct KillShellResult {
    /// Shell identifier.
    pub shell_id: String,
    /// Whether shell was killed.
    pub killed: bool,
    /// Unknown fields.
    #[serde(flatten)]
    pub extra: IndexMap<String, Value>,
}

/// NotebookEdit tool result.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NotebookEditResult {
    /// Notebook file path.
    pub notebook_path: String,
    /// Cell identifier.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cell_id: Option<String>,
    /// Edit mode.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub edit_mode: Option<String>,
    /// Cell type.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cell_type: Option<String>,
    /// Previous cell source.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub previous_source: Option<String>,
    /// New cell source.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub new_source: Option<String>,
    /// Unknown fields.
    #[serde(flatten)]
    pub extra: IndexMap<String, Value>,
}

/// NotebookRead tool result.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NotebookReadResult {
    /// Notebook file path.
    pub notebook_path: String,
    /// Cells in notebook.
    pub cells: Vec<NotebookCell>,
    /// Total cell count.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub num_cells: Option<usize>,
    /// Kernel specification.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kernel_spec: Option<KernelSpec>,
    /// Unknown fields.
    #[serde(flatten)]
    pub extra: IndexMap<String, Value>,
}

/// A notebook cell.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NotebookCell {
    /// Cell identifier.
    pub cell_id: String,
    /// Cell type ("code" or "markdown").
    pub cell_type: String,
    /// Cell source code.
    pub source: String,
    /// Cell outputs (for code cells).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub outputs: Vec<Value>,
    /// Unknown fields.
    #[serde(flatten)]
    pub extra: IndexMap<String, Value>,
}

/// Kernel specification.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KernelSpec {
    /// Kernel name.
    pub name: String,
    /// Programming language.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
    /// Unknown fields.
    #[serde(flatten)]
    pub extra: IndexMap<String, Value>,
}

/// TodoRead tool result.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TodoReadResult {
    /// Current task list items.
    pub todos: Vec<TodoItem>,
    /// Total count.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub count: Option<usize>,
    /// Unknown fields.
    #[serde(flatten)]
    pub extra: IndexMap<String, Value>,
}

/// A todo item.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TodoItem {
    /// Task description.
    pub content: String,
    /// Task status.
    pub status: String,
    /// Active form description.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub active_form: Option<String>,
    /// Task priority.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub priority: Option<String>,
    /// Unknown fields.
    #[serde(flatten)]
    pub extra: IndexMap<String, Value>,
}

/// TodoWrite tool result.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TodoWriteResult {
    /// Number of todos written.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub todos_written: Option<usize>,
    /// Updated todo list.
    #[serde(default)]
    pub todos: Vec<TodoItem>,
    /// Unknown fields.
    #[serde(flatten)]
    pub extra: IndexMap<String, Value>,
}

/// AskUserQuestion tool result.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AskUserQuestionResult {
    /// Questions asked.
    pub questions: Vec<Question>,
    /// User answers.
    #[serde(default)]
    pub answers: IndexMap<String, String>,
    /// Unknown fields.
    #[serde(flatten)]
    pub extra: IndexMap<String, Value>,
}

/// A question for the user.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Question {
    /// Question text.
    pub question: String,
    /// Short header label.
    pub header: String,
    /// Whether multiple selections are allowed.
    #[serde(default)]
    pub multi_select: bool,
    /// Available options.
    pub options: Vec<QuestionOption>,
    /// Unknown fields.
    #[serde(flatten)]
    pub extra: IndexMap<String, Value>,
}

/// A question option.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QuestionOption {
    /// Option label.
    pub label: String,
    /// Option description.
    pub description: String,
    /// Unknown fields.
    #[serde(flatten)]
    pub extra: IndexMap<String, Value>,
}

/// LSP tool result (v2.0.74+).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LspResult {
    /// LSP operation performed.
    pub operation: LspOperation,
    /// Target file path.
    pub file_path: String,
    /// Line number (1-based).
    pub line: usize,
    /// Character offset (1-based).
    pub character: usize,
    /// Operation result.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    /// Unknown fields.
    #[serde(flatten)]
    pub extra: IndexMap<String, Value>,
}

/// LSP operations.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum LspOperation {
    /// Find where a symbol is defined.
    GoToDefinition,
    /// Find all references to a symbol.
    FindReferences,
    /// Get hover information.
    Hover,
    /// Get all symbols in a document.
    DocumentSymbol,
    /// Search for symbols across workspace.
    WorkspaceSymbol,
    /// Find implementations of interface/abstract method.
    GoToImplementation,
    /// Get call hierarchy item at position.
    PrepareCallHierarchy,
    /// Find all callers of function.
    IncomingCalls,
    /// Find all functions called by function.
    OutgoingCalls,
}

/// EnterPlanMode tool result.
/// Used when Claude enters plan mode for designing implementation approaches.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EnterPlanModeResult {
    /// Whether plan mode was entered.
    #[serde(default)]
    pub entered: bool,
    /// Path to the plan file.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub plan_file: Option<String>,
    /// Unknown fields.
    #[serde(flatten)]
    pub extra: IndexMap<String, Value>,
}

/// ExitPlanMode tool result.
/// Used when Claude exits plan mode after completing a plan.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExitPlanModeResult {
    /// Whether plan mode was exited.
    #[serde(default)]
    pub exited: bool,
    /// Whether implementation should proceed.
    #[serde(default)]
    pub proceed: bool,
    /// Unknown fields.
    #[serde(flatten)]
    pub extra: IndexMap<String, Value>,
}

/// Skill tool result.
/// Used when Claude invokes a user-defined skill/command.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SkillResult {
    /// Skill name that was invoked.
    pub skill: String,
    /// Optional arguments passed to the skill.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub args: Option<String>,
    /// Skill execution output.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output: Option<String>,
    /// Skill location (user, project, etc.).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub location: Option<String>,
    /// Unknown fields.
    #[serde(flatten)]
    pub extra: IndexMap<String, Value>,
}

/// ListMcpResources tool result.
/// Used when listing resources from an MCP server.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ListMcpResourcesResult {
    /// MCP server name.
    pub server: String,
    /// Available resources.
    #[serde(default)]
    pub resources: Vec<McpResource>,
    /// Total resource count.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub count: Option<usize>,
    /// Unknown fields.
    #[serde(flatten)]
    pub extra: IndexMap<String, Value>,
}

/// An MCP resource.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpResource {
    /// Resource URI.
    pub uri: String,
    /// Resource name.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Resource description.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// MIME type.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mime_type: Option<String>,
    /// Unknown fields.
    #[serde(flatten)]
    pub extra: IndexMap<String, Value>,
}

/// ReadMcpResource tool result.
/// Used when reading content from an MCP resource.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReadMcpResourceResult {
    /// MCP server name.
    pub server: String,
    /// Resource URI.
    pub uri: String,
    /// Resource content.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    /// Content MIME type.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mime_type: Option<String>,
    /// Content size in bytes.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub size: Option<u64>,
    /// Unknown fields.
    #[serde(flatten)]
    pub extra: IndexMap<String, Value>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_bash_result() {
        let result = BashResult {
            stdout: "success".to_string(),
            stderr: String::new(),
            interrupted: false,
            is_image: false,
            exit_code: Some(0),
            extra: IndexMap::new(),
        };

        assert!(result.succeeded());
        assert!(!result.was_interrupted());
    }

    #[test]
    fn test_patch_hunk() {
        let hunk = PatchHunk {
            old_start: 1,
            old_lines: 3,
            new_start: 1,
            new_lines: 4,
            lines: vec![
                " context".to_string(),
                "-removed".to_string(),
                "+added1".to_string(),
                "+added2".to_string(),
            ],
            extra: IndexMap::new(),
        };

        assert_eq!(hunk.added_lines(), vec!["added1", "added2"]);
        assert_eq!(hunk.removed_lines(), vec!["removed"]);
        assert_eq!(hunk.context_lines(), vec!["context"]);
    }

    #[test]
    fn test_ls_entry() {
        let dir = LsEntry {
            name: "src".to_string(),
            entry_type: "directory".to_string(),
            extra: IndexMap::new(),
        };

        assert!(dir.is_directory());
        assert!(!dir.is_file());
    }
}
