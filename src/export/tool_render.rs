//! Readable rendering of common tool-call inputs for human export formats.
//!
//! By default exporters render a tool call's `input` as a pretty-printed JSON
//! blob, which turns an `Edit` into escaped `old_string`/`new_string` fields
//! rather than a diff. [`classify`] recognizes the common Claude Code tools by
//! name *and* shape and returns a [`ToolInputView`] the human exporters
//! (markdown / text / html) render with their own primitives.
//!
//! Detection is deliberately defensive: any tool name we don't special-case, or
//! any recognized name whose input doesn't match the expected shape, yields
//! [`ToolInputView::Json`] so the caller falls back to today's JSON rendering.
//! Tool input schemas are Claude-Code-internal and may drift; the JSON fallback
//! keeps a shape change from producing garbage.

use serde_json::{Map, Value};
use similar::TextDiff;

use crate::model::content::ToolUse;

/// A single edit operation (old text → new text).
pub struct EditOp<'a> {
    pub old_string: &'a str,
    pub new_string: &'a str,
}

/// A todo item from a `TodoWrite` call.
pub struct TodoItem<'a> {
    pub content: &'a str,
    pub status: &'a str,
}

impl TodoItem<'_> {
    /// Checklist marker for this item's status.
    #[must_use]
    pub fn checkbox(&self) -> &'static str {
        match self.status {
            "completed" => "[x]",
            "in_progress" => "[~]",
            _ => "[ ]",
        }
    }
}

/// A recognized, readably-renderable view of a tool call's input.
///
/// `Edit` covers both `Edit` (one op) and `MultiEdit` (many). `Json` signals
/// the caller should fall back to pretty-JSON rendering.
pub enum ToolInputView<'a> {
    Edit {
        file_path: &'a str,
        edits: Vec<EditOp<'a>>,
    },
    Bash {
        command: &'a str,
        description: Option<&'a str>,
    },
    Write {
        file_path: &'a str,
        content: &'a str,
    },
    Todos(Vec<TodoItem<'a>>),
    Json,
}

/// Classify a tool call's input into a renderable view (or `Json` fallback).
#[must_use]
pub fn classify(tool_use: &ToolUse) -> ToolInputView<'_> {
    let Some(obj) = tool_use.input.as_object() else {
        return ToolInputView::Json;
    };
    match tool_use.name.as_str() {
        "Edit" => classify_edit(obj),
        "MultiEdit" => classify_multi_edit(obj),
        "Bash" => classify_bash(obj),
        "Write" => classify_write(obj),
        "TodoWrite" => classify_todos(obj),
        _ => ToolInputView::Json,
    }
}

fn str_field<'a>(obj: &'a Map<String, Value>, key: &str) -> Option<&'a str> {
    obj.get(key)?.as_str()
}

fn classify_edit(obj: &Map<String, Value>) -> ToolInputView<'_> {
    match (
        str_field(obj, "file_path"),
        str_field(obj, "old_string"),
        str_field(obj, "new_string"),
    ) {
        (Some(file_path), Some(old_string), Some(new_string)) => ToolInputView::Edit {
            file_path,
            edits: vec![EditOp {
                old_string,
                new_string,
            }],
        },
        _ => ToolInputView::Json,
    }
}

fn classify_multi_edit(obj: &Map<String, Value>) -> ToolInputView<'_> {
    let Some(file_path) = str_field(obj, "file_path") else {
        return ToolInputView::Json;
    };
    let Some(arr) = obj.get("edits").and_then(Value::as_array) else {
        return ToolInputView::Json;
    };
    let mut edits = Vec::with_capacity(arr.len());
    for entry in arr {
        let Some(eo) = entry.as_object() else {
            return ToolInputView::Json;
        };
        match (str_field(eo, "old_string"), str_field(eo, "new_string")) {
            (Some(old_string), Some(new_string)) => edits.push(EditOp {
                old_string,
                new_string,
            }),
            _ => return ToolInputView::Json,
        }
    }
    if edits.is_empty() {
        return ToolInputView::Json;
    }
    ToolInputView::Edit { file_path, edits }
}

fn classify_bash(obj: &Map<String, Value>) -> ToolInputView<'_> {
    match str_field(obj, "command") {
        Some(command) => ToolInputView::Bash {
            command,
            description: str_field(obj, "description"),
        },
        None => ToolInputView::Json,
    }
}

fn classify_write(obj: &Map<String, Value>) -> ToolInputView<'_> {
    match (str_field(obj, "file_path"), str_field(obj, "content")) {
        (Some(file_path), Some(content)) => ToolInputView::Write { file_path, content },
        _ => ToolInputView::Json,
    }
}

fn classify_todos(obj: &Map<String, Value>) -> ToolInputView<'_> {
    let Some(arr) = obj.get("todos").and_then(Value::as_array) else {
        return ToolInputView::Json;
    };
    let mut todos = Vec::with_capacity(arr.len());
    for entry in arr {
        let Some(to) = entry.as_object() else {
            return ToolInputView::Json;
        };
        let Some(content) = str_field(to, "content") else {
            return ToolInputView::Json;
        };
        let status = str_field(to, "status").unwrap_or("pending");
        todos.push(TodoItem { content, status });
    }
    if todos.is_empty() {
        return ToolInputView::Json;
    }
    ToolInputView::Todos(todos)
}

/// Render a unified diff body for an edit's old → new text.
///
/// Returns the hunk body (`@@ … @@` markers and `+`/`-`/space lines) without a
/// file header — exporters supply the filename in their own style. The
/// missing-newline hint is suppressed to keep fragment diffs clean.
#[must_use]
pub fn unified_diff(old: &str, new: &str) -> String {
    TextDiff::from_lines(old, new)
        .unified_diff()
        .context_radius(3)
        .missing_newline_hint(false)
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use indexmap::IndexMap;
    use serde_json::json;

    fn tool(name: &str, input: Value) -> ToolUse {
        ToolUse {
            id: "toolu_test".to_string(),
            name: name.to_string(),
            input,
            extra: IndexMap::new(),
        }
    }

    #[test]
    fn edit_with_expected_shape_classifies() {
        let tu = tool(
            "Edit",
            json!({"file_path": "a.rs", "old_string": "x", "new_string": "y"}),
        );
        match classify(&tu) {
            ToolInputView::Edit { file_path, edits } => {
                assert_eq!(file_path, "a.rs");
                assert_eq!(edits.len(), 1);
                assert_eq!(edits[0].old_string, "x");
                assert_eq!(edits[0].new_string, "y");
            }
            _ => panic!("expected Edit"),
        }
    }

    #[test]
    fn edit_missing_field_falls_back_to_json() {
        let tu = tool("Edit", json!({"file_path": "a.rs", "old_string": "x"}));
        assert!(matches!(classify(&tu), ToolInputView::Json));
    }

    #[test]
    fn multi_edit_collects_all_ops() {
        let tu = tool(
            "MultiEdit",
            json!({"file_path": "a.rs", "edits": [
                {"old_string": "a", "new_string": "b"},
                {"old_string": "c", "new_string": "d"},
            ]}),
        );
        match classify(&tu) {
            ToolInputView::Edit { file_path, edits } => {
                assert_eq!(file_path, "a.rs");
                assert_eq!(edits.len(), 2);
                assert_eq!(edits[1].new_string, "d");
            }
            _ => panic!("expected Edit"),
        }
    }

    #[test]
    fn multi_edit_malformed_op_falls_back() {
        let tu = tool(
            "MultiEdit",
            json!({"file_path": "a.rs", "edits": [{"old_string": "a"}]}),
        );
        assert!(matches!(classify(&tu), ToolInputView::Json));
    }

    #[test]
    fn bash_with_and_without_description() {
        let tu = tool("Bash", json!({"command": "ls", "description": "list"}));
        match classify(&tu) {
            ToolInputView::Bash {
                command,
                description,
            } => {
                assert_eq!(command, "ls");
                assert_eq!(description, Some("list"));
            }
            _ => panic!("expected Bash"),
        }
        let tu = tool("Bash", json!({"command": "ls"}));
        match classify(&tu) {
            ToolInputView::Bash { description, .. } => assert_eq!(description, None),
            _ => panic!("expected Bash"),
        }
    }

    #[test]
    fn write_classifies() {
        let tu = tool("Write", json!({"file_path": "a.rs", "content": "hello"}));
        assert!(matches!(classify(&tu), ToolInputView::Write { .. }));
    }

    #[test]
    fn todos_classify_with_status_markers() {
        let tu = tool(
            "TodoWrite",
            json!({"todos": [
                {"content": "one", "status": "completed", "activeForm": "doing one"},
                {"content": "two", "status": "in_progress", "activeForm": "doing two"},
                {"content": "three", "status": "pending", "activeForm": "doing three"},
            ]}),
        );
        match classify(&tu) {
            ToolInputView::Todos(items) => {
                assert_eq!(items.len(), 3);
                assert_eq!(items[0].checkbox(), "[x]");
                assert_eq!(items[1].checkbox(), "[~]");
                assert_eq!(items[2].checkbox(), "[ ]");
            }
            _ => panic!("expected Todos"),
        }
    }

    #[test]
    fn unknown_tool_falls_back_to_json() {
        let tu = tool("SomeOtherTool", json!({"foo": "bar"}));
        assert!(matches!(classify(&tu), ToolInputView::Json));
    }

    #[test]
    fn non_object_input_falls_back_to_json() {
        let tu = tool("Edit", json!("just a string"));
        assert!(matches!(classify(&tu), ToolInputView::Json));
    }

    #[test]
    fn unified_diff_shows_changed_lines() {
        let d = unified_diff("fn main() {\n    old\n}\n", "fn main() {\n    new\n}\n");
        assert!(d.contains("-    old"));
        assert!(d.contains("+    new"));
        assert!(d.contains(" fn main() {"));
    }
}
