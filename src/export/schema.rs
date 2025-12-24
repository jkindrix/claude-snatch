//! JSON Schema definitions for export formats.
//!
//! Provides JSON Schema v7 definitions for validating exported data,
//! ensuring schema-compliant output for interoperability.

use jsonschema::Validator;
use once_cell::sync::Lazy;
use serde_json::{json, Value};

use crate::error::{Result, SnatchError};

/// JSON Schema for the conversation export envelope format.
static EXPORT_SCHEMA: Lazy<Value> = Lazy::new(|| {
    json!({
        "$schema": "http://json-schema.org/draft-07/schema#",
        "$id": "https://claude-snatch.dev/schemas/export/v1.0",
        "title": "Claude Snatch Export Format",
        "description": "Schema for claude-snatch conversation export with envelope",
        "type": "object",
        "required": ["version", "exported_at", "exporter", "entries"],
        "properties": {
            "version": {
                "type": "string",
                "description": "Export format version",
                "pattern": "^\\d+\\.\\d+$"
            },
            "exported_at": {
                "type": "string",
                "format": "date-time",
                "description": "ISO 8601 timestamp of export"
            },
            "exporter": {
                "$ref": "#/definitions/exporter_info"
            },
            "metadata": {
                "$ref": "#/definitions/export_metadata"
            },
            "analytics": {
                "$ref": "#/definitions/export_analytics"
            },
            "tree": {
                "$ref": "#/definitions/tree_info"
            },
            "entries": {
                "type": "array",
                "items": {
                    "$ref": "#/definitions/log_entry"
                }
            }
        },
        "definitions": {
            "exporter_info": {
                "type": "object",
                "required": ["name", "version"],
                "properties": {
                    "name": { "type": "string" },
                    "version": { "type": "string" }
                }
            },
            "export_metadata": {
                "type": "object",
                "properties": {
                    "session_id": { "type": ["string", "null"] },
                    "version": { "type": ["string", "null"] },
                    "project_path": { "type": ["string", "null"] }
                }
            },
            "export_analytics": {
                "type": "object",
                "required": [
                    "total_messages", "user_messages", "assistant_messages",
                    "total_tokens", "input_tokens", "output_tokens",
                    "tool_invocations", "thinking_blocks", "cache_hit_rate"
                ],
                "properties": {
                    "total_messages": { "type": "integer", "minimum": 0 },
                    "user_messages": { "type": "integer", "minimum": 0 },
                    "assistant_messages": { "type": "integer", "minimum": 0 },
                    "total_tokens": { "type": "integer", "minimum": 0 },
                    "input_tokens": { "type": "integer", "minimum": 0 },
                    "output_tokens": { "type": "integer", "minimum": 0 },
                    "tool_invocations": { "type": "integer", "minimum": 0 },
                    "thinking_blocks": { "type": "integer", "minimum": 0 },
                    "cache_hit_rate": { "type": "number", "minimum": 0.0, "maximum": 100.0 },
                    "estimated_cost": { "type": ["number", "null"], "minimum": 0.0 },
                    "duration_seconds": { "type": ["integer", "null"] },
                    "primary_model": { "type": ["string", "null"] }
                }
            },
            "tree_info": {
                "type": "object",
                "required": ["total_nodes", "main_thread_length", "max_depth", "branch_count", "roots", "branch_points"],
                "properties": {
                    "total_nodes": { "type": "integer", "minimum": 0 },
                    "main_thread_length": { "type": "integer", "minimum": 0 },
                    "max_depth": { "type": "integer", "minimum": 0 },
                    "branch_count": { "type": "integer", "minimum": 0 },
                    "roots": {
                        "type": "array",
                        "items": { "type": "string" }
                    },
                    "branch_points": {
                        "type": "array",
                        "items": { "type": "string" }
                    }
                }
            },
            "log_entry": {
                "type": "object",
                "required": ["type"],
                "properties": {
                    "type": {
                        "type": "string",
                        "enum": ["user", "assistant", "summary", "system"]
                    },
                    "uuid": { "type": "string" },
                    "parentUuid": { "type": ["string", "null"] },
                    "sessionId": { "type": "string" },
                    "version": { "type": "string" },
                    "timestamp": { "type": "string" },
                    "message": {
                        "$ref": "#/definitions/message"
                    },
                    "usage": {
                        "$ref": "#/definitions/usage"
                    }
                }
            },
            "message": {
                "type": "object",
                "required": ["role"],
                "properties": {
                    "role": {
                        "type": "string",
                        "enum": ["user", "assistant", "system"]
                    },
                    "model": { "type": "string" },
                    "content": {
                        "oneOf": [
                            { "type": "string" },
                            {
                                "type": "array",
                                "items": { "$ref": "#/definitions/content_block" }
                            }
                        ]
                    },
                    "stop_reason": { "type": ["string", "null"] },
                    "stop_sequence": { "type": ["string", "null"] }
                }
            },
            "content_block": {
                "type": "object",
                "required": ["type"],
                "properties": {
                    "type": {
                        "type": "string"
                    }
                },
                "allOf": [
                    {
                        "if": { "properties": { "type": { "const": "text" } } },
                        "then": { "properties": { "text": { "type": "string" } } }
                    },
                    {
                        "if": { "properties": { "type": { "const": "thinking" } } },
                        "then": {
                            "properties": {
                                "thinking": { "type": "string" },
                                "signature": { "type": "string" }
                            }
                        }
                    },
                    {
                        "if": { "properties": { "type": { "const": "tool_use" } } },
                        "then": {
                            "properties": {
                                "id": { "type": "string" },
                                "name": { "type": "string" },
                                "input": { "type": "object" }
                            }
                        }
                    },
                    {
                        "if": { "properties": { "type": { "const": "tool_result" } } },
                        "then": {
                            "properties": {
                                "tool_use_id": { "type": "string" },
                                "content": {},
                                "is_error": { "type": ["boolean", "null"] }
                            }
                        }
                    },
                    {
                        "if": { "properties": { "type": { "const": "image" } } },
                        "then": {
                            "properties": {
                                "source": {
                                    "type": "object",
                                    "properties": {
                                        "type": { "type": "string" },
                                        "media_type": { "type": "string" },
                                        "data": { "type": "string" }
                                    }
                                }
                            }
                        }
                    }
                ]
            },
            "usage": {
                "type": "object",
                "properties": {
                    "input_tokens": { "type": "integer", "minimum": 0 },
                    "output_tokens": { "type": "integer", "minimum": 0 },
                    "cache_creation_input_tokens": { "type": ["integer", "null"], "minimum": 0 },
                    "cache_read_input_tokens": { "type": ["integer", "null"], "minimum": 0 }
                }
            }
        }
    })
});

/// JSON Schema for raw JSONL entries (no envelope).
static ENTRY_SCHEMA: Lazy<Value> = Lazy::new(|| {
    json!({
        "$schema": "http://json-schema.org/draft-07/schema#",
        "$id": "https://claude-snatch.dev/schemas/entry/v1.0",
        "title": "Claude Code Log Entry",
        "description": "Schema for individual Claude Code JSONL log entries",
        "$ref": "#/definitions/log_entry",
        "definitions": EXPORT_SCHEMA.get("definitions").cloned().unwrap_or(json!({}))
    })
});

/// Compiled schema validator for export format.
static EXPORT_VALIDATOR: Lazy<Validator> = Lazy::new(|| {
    jsonschema::draft7::new(&EXPORT_SCHEMA).expect("Invalid export schema")
});

/// Compiled schema validator for log entries.
static ENTRY_VALIDATOR: Lazy<Validator> = Lazy::new(|| {
    jsonschema::draft7::new(&ENTRY_SCHEMA).expect("Invalid entry schema")
});

/// Result of schema validation.
#[derive(Debug, Clone)]
pub struct ValidationResult {
    /// Whether the validation passed.
    pub valid: bool,
    /// List of validation errors.
    pub errors: Vec<String>,
    /// Number of entries validated (for batch validation).
    pub entries_checked: usize,
}

impl ValidationResult {
    /// Create a successful validation result.
    #[must_use]
    pub fn success(entries_checked: usize) -> Self {
        Self {
            valid: true,
            errors: Vec::new(),
            entries_checked,
        }
    }

    /// Create a failed validation result.
    #[must_use]
    pub fn failure(errors: Vec<String>, entries_checked: usize) -> Self {
        Self {
            valid: false,
            errors,
            entries_checked,
        }
    }
}

/// Schema validator for export data.
#[derive(Debug, Clone, Default)]
pub struct SchemaValidator {
    /// Collect all errors or stop at first.
    collect_all_errors: bool,
    /// Maximum errors to collect.
    max_errors: usize,
}

impl SchemaValidator {
    /// Create a new schema validator.
    #[must_use]
    pub fn new() -> Self {
        Self {
            collect_all_errors: true,
            max_errors: 100,
        }
    }

    /// Set whether to collect all errors.
    #[must_use]
    pub fn collect_all_errors(mut self, collect: bool) -> Self {
        self.collect_all_errors = collect;
        self
    }

    /// Set maximum errors to collect.
    #[must_use]
    pub fn max_errors(mut self, max: usize) -> Self {
        self.max_errors = max;
        self
    }

    /// Validate an export envelope.
    pub fn validate_export(&self, data: &Value) -> ValidationResult {
        self.validate_with_schema(&EXPORT_VALIDATOR, data, 1)
    }

    /// Validate a single log entry.
    pub fn validate_entry(&self, data: &Value) -> ValidationResult {
        self.validate_with_schema(&ENTRY_VALIDATOR, data, 1)
    }

    /// Validate multiple entries (JSONL).
    pub fn validate_entries(&self, entries: &[Value]) -> ValidationResult {
        let mut all_errors = Vec::new();
        let mut checked = 0;

        for (i, entry) in entries.iter().enumerate() {
            checked += 1;
            let result = self.validate_entry(entry);

            if !result.valid {
                for error in result.errors {
                    let error_with_line = format!("Entry {}: {}", i + 1, error);
                    all_errors.push(error_with_line);

                    if all_errors.len() >= self.max_errors {
                        return ValidationResult::failure(all_errors, checked);
                    }
                }

                if !self.collect_all_errors {
                    return ValidationResult::failure(all_errors, checked);
                }
            }
        }

        if all_errors.is_empty() {
            ValidationResult::success(checked)
        } else {
            ValidationResult::failure(all_errors, checked)
        }
    }

    /// Internal validation helper.
    fn validate_with_schema(
        &self,
        schema: &Validator,
        data: &Value,
        entries_count: usize,
    ) -> ValidationResult {
        if schema.is_valid(data) {
            ValidationResult::success(entries_count)
        } else {
            let error_strings: Vec<String> = schema
                .iter_errors(data)
                .take(self.max_errors)
                .map(|e| format_validation_error(&e))
                .collect();

            ValidationResult::failure(error_strings, entries_count)
        }
    }
}

/// Format a validation error for display.
fn format_validation_error(error: &jsonschema::ValidationError) -> String {
    let path = error.instance_path.to_string();
    if path.is_empty() {
        format!("{}", error)
    } else {
        format!("at '{}': {}", path, error)
    }
}

/// Get the export schema as JSON.
#[must_use]
pub fn export_schema() -> &'static Value {
    &EXPORT_SCHEMA
}

/// Get the entry schema as JSON.
#[must_use]
pub fn entry_schema() -> &'static Value {
    &ENTRY_SCHEMA
}

/// Get the export schema as a pretty-printed string.
#[must_use]
pub fn export_schema_string() -> String {
    serde_json::to_string_pretty(&*EXPORT_SCHEMA).unwrap_or_default()
}

/// Get the entry schema as a pretty-printed string.
#[must_use]
pub fn entry_schema_string() -> String {
    serde_json::to_string_pretty(&*ENTRY_SCHEMA).unwrap_or_default()
}

/// Validate JSON data and return a Result.
pub fn validate_export(data: &Value) -> Result<()> {
    let validator = SchemaValidator::new();
    let result = validator.validate_export(data);

    if result.valid {
        Ok(())
    } else {
        Err(SnatchError::validation(format!(
            "Schema validation failed with {} error(s):\n{}",
            result.errors.len(),
            result.errors.join("\n")
        )))
    }
}

/// Validate JSONL entries and return a Result.
pub fn validate_entries(entries: &[Value]) -> Result<()> {
    let validator = SchemaValidator::new();
    let result = validator.validate_entries(entries);

    if result.valid {
        Ok(())
    } else {
        Err(SnatchError::validation(format!(
            "Schema validation failed for {}/{} entries:\n{}",
            result.errors.len(),
            result.entries_checked,
            result.errors.join("\n")
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_export_schema_is_valid() {
        // Schema should compile without error
        let _schema = &*EXPORT_VALIDATOR;
    }

    #[test]
    fn test_entry_schema_is_valid() {
        // Schema should compile without error
        let _schema = &*ENTRY_VALIDATOR;
    }

    #[test]
    fn test_valid_export() {
        let data = json!({
            "version": "1.0",
            "exported_at": "2024-01-15T10:30:00Z",
            "exporter": {
                "name": "claude-snatch",
                "version": "0.1.0"
            },
            "entries": []
        });

        let validator = SchemaValidator::new();
        let result = validator.validate_export(&data);
        assert!(result.valid, "Errors: {:?}", result.errors);
    }

    #[test]
    fn test_invalid_export_missing_version() {
        let data = json!({
            "exported_at": "2024-01-15T10:30:00Z",
            "exporter": {
                "name": "claude-snatch",
                "version": "0.1.0"
            },
            "entries": []
        });

        let validator = SchemaValidator::new();
        let result = validator.validate_export(&data);
        assert!(!result.valid);
        assert!(!result.errors.is_empty());
    }

    #[test]
    fn test_valid_entry() {
        let entry = json!({
            "type": "user",
            "uuid": "abc-123",
            "sessionId": "session-1",
            "message": {
                "role": "user",
                "content": "Hello"
            }
        });

        let validator = SchemaValidator::new();
        let result = validator.validate_entry(&entry);
        assert!(result.valid, "Errors: {:?}", result.errors);
    }

    #[test]
    fn test_valid_assistant_entry() {
        let entry = json!({
            "type": "assistant",
            "uuid": "def-456",
            "sessionId": "session-1",
            "message": {
                "role": "assistant",
                "model": "claude-3-5-sonnet-20241022",
                "content": [
                    { "type": "text", "text": "Hello!" },
                    { "type": "thinking", "thinking": "Let me think...", "signature": "sig123" }
                ],
                "stop_reason": "end_turn"
            },
            "usage": {
                "input_tokens": 100,
                "output_tokens": 50
            }
        });

        let validator = SchemaValidator::new();
        let result = validator.validate_entry(&entry);
        assert!(result.valid, "Errors: {:?}", result.errors);
    }

    #[test]
    fn test_export_schema_string() {
        let schema_str = export_schema_string();
        assert!(schema_str.contains("$schema"));
        assert!(schema_str.contains("Claude Snatch Export Format"));
    }

    #[test]
    fn test_validate_entries_batch() {
        let entries = vec![
            json!({ "type": "user", "message": { "role": "user", "content": "Hi" } }),
            json!({ "type": "assistant", "message": { "role": "assistant", "content": "Hello" } }),
        ];

        let validator = SchemaValidator::new();
        let result = validator.validate_entries(&entries);
        assert!(result.valid, "Errors: {:?}", result.errors);
        assert_eq!(result.entries_checked, 2);
    }

    #[test]
    fn test_export_with_analytics() {
        let data = json!({
            "version": "1.0",
            "exported_at": "2024-01-15T10:30:00Z",
            "exporter": {
                "name": "claude-snatch",
                "version": "0.1.0"
            },
            "analytics": {
                "total_messages": 10,
                "user_messages": 5,
                "assistant_messages": 5,
                "total_tokens": 5000,
                "input_tokens": 3000,
                "output_tokens": 2000,
                "tool_invocations": 3,
                "thinking_blocks": 2,
                "cache_hit_rate": 45.5,
                "estimated_cost": 0.05,
                "duration_seconds": 120,
                "primary_model": "claude-3-5-sonnet-20241022"
            },
            "entries": []
        });

        let validator = SchemaValidator::new();
        let result = validator.validate_export(&data);
        assert!(result.valid, "Errors: {:?}", result.errors);
    }

    #[test]
    fn test_export_with_tree_info() {
        let data = json!({
            "version": "1.0",
            "exported_at": "2024-01-15T10:30:00Z",
            "exporter": {
                "name": "claude-snatch",
                "version": "0.1.0"
            },
            "tree": {
                "total_nodes": 20,
                "main_thread_length": 15,
                "max_depth": 3,
                "branch_count": 2,
                "roots": ["uuid-1"],
                "branch_points": ["uuid-5", "uuid-10"]
            },
            "entries": []
        });

        let validator = SchemaValidator::new();
        let result = validator.validate_export(&data);
        assert!(result.valid, "Errors: {:?}", result.errors);
    }
}
