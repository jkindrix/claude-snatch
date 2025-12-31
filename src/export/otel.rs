//! OpenTelemetry (OTLP) export format.
//!
//! Exports conversation data in OTLP JSON format, compatible with
//! OpenTelemetry collectors and observability platforms.
//!
//! # Trace Structure
//!
//! - Each conversation becomes a trace
//! - Each message turn becomes a span (user prompt → assistant response)
//! - Tool calls become child spans
//! - Thinking blocks are captured as span events
//!
//! # Example Output
//!
//! ```json
//! {
//!   "resourceSpans": [{
//!     "resource": {
//!       "attributes": [
//!         {"key": "service.name", "value": {"stringValue": "claude-code"}}
//!       ]
//!     },
//!     "scopeSpans": [{
//!       "scope": {"name": "claude-snatch"},
//!       "spans": [...]
//!     }]
//!   }]
//! }
//! ```

use std::io::Write;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::error::Result;
use crate::model::{ContentBlock, LogEntry};
use crate::reconstruction::Conversation;

use super::{ExportOptions, Exporter};

/// OpenTelemetry exporter.
#[derive(Debug, Clone, Default)]
pub struct OtelExporter {
    /// Service name for resource attributes.
    service_name: String,
    /// Whether to include thinking as events.
    include_thinking_events: bool,
    /// Whether to include tool details.
    include_tool_details: bool,
}

impl OtelExporter {
    /// Create a new OTEL exporter.
    pub fn new() -> Self {
        Self {
            service_name: "claude-code".to_string(),
            include_thinking_events: true,
            include_tool_details: true,
        }
    }

    /// Set the service name.
    #[must_use]
    pub fn with_service_name(mut self, name: impl Into<String>) -> Self {
        self.service_name = name.into();
        self
    }

    /// Set whether to include thinking blocks as events.
    #[must_use]
    pub fn with_thinking_events(mut self, include: bool) -> Self {
        self.include_thinking_events = include;
        self
    }

    /// Set whether to include tool details.
    #[must_use]
    pub fn with_tool_details(mut self, include: bool) -> Self {
        self.include_tool_details = include;
        self
    }
}

// OTLP JSON structures (simplified for export purposes)

/// OTLP export data structure.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OtlpExportData {
    /// Resource spans.
    pub resource_spans: Vec<ResourceSpans>,
}

/// Resource spans container.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResourceSpans {
    /// Resource information.
    pub resource: Resource,
    /// Scope spans.
    pub scope_spans: Vec<ScopeSpans>,
}

/// Resource information.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Resource {
    /// Resource attributes.
    pub attributes: Vec<KeyValue>,
}

/// Scope spans container.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScopeSpans {
    /// Instrumentation scope.
    pub scope: InstrumentationScope,
    /// Spans.
    pub spans: Vec<Span>,
}

/// Instrumentation scope.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InstrumentationScope {
    /// Scope name.
    pub name: String,
    /// Scope version.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
}

/// A span in the trace.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Span {
    /// Trace ID (hex string).
    pub trace_id: String,
    /// Span ID (hex string).
    pub span_id: String,
    /// Parent span ID (hex string).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_span_id: Option<String>,
    /// Span name.
    pub name: String,
    /// Span kind.
    pub kind: i32,
    /// Start time in nanoseconds.
    pub start_time_unix_nano: String,
    /// End time in nanoseconds.
    pub end_time_unix_nano: String,
    /// Span attributes.
    pub attributes: Vec<KeyValue>,
    /// Span events.
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub events: Vec<SpanEvent>,
    /// Status.
    pub status: SpanStatus,
}

/// Span event.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SpanEvent {
    /// Event name.
    pub name: String,
    /// Event time in nanoseconds.
    pub time_unix_nano: String,
    /// Event attributes.
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub attributes: Vec<KeyValue>,
}

/// Span status.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpanStatus {
    /// Status code (0=unset, 1=ok, 2=error).
    pub code: i32,
    /// Status message.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

/// Key-value attribute.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KeyValue {
    /// Attribute key.
    pub key: String,
    /// Attribute value.
    pub value: AnyValue,
}

/// Any value (OTLP-style).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum AnyValue {
    /// String value.
    StringValue(String),
    /// Integer value.
    IntValue(i64),
    /// Double value.
    DoubleValue(f64),
    /// Boolean value.
    BoolValue(bool),
    /// Array value.
    ArrayValue(ArrayValue),
    /// Key-value list.
    KvlistValue(KvlistValue),
}

/// Array value container.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArrayValue {
    /// Array values.
    pub values: Vec<AnyValue>,
}

/// Key-value list container.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KvlistValue {
    /// Key-value pairs.
    pub values: Vec<KeyValue>,
}

impl KeyValue {
    /// Create a string attribute.
    pub fn string(key: impl Into<String>, value: impl Into<String>) -> Self {
        Self {
            key: key.into(),
            value: AnyValue::StringValue(value.into()),
        }
    }

    /// Create an integer attribute.
    pub fn int(key: impl Into<String>, value: i64) -> Self {
        Self {
            key: key.into(),
            value: AnyValue::IntValue(value),
        }
    }

    /// Create a boolean attribute.
    pub fn bool(key: impl Into<String>, value: bool) -> Self {
        Self {
            key: key.into(),
            value: AnyValue::BoolValue(value),
        }
    }

    /// Create a double attribute.
    #[allow(dead_code)]
    pub fn double(key: impl Into<String>, value: f64) -> Self {
        Self {
            key: key.into(),
            value: AnyValue::DoubleValue(value),
        }
    }
}

/// Span kind constants.
pub mod span_kind {
    /// Internal span.
    pub const INTERNAL: i32 = 1;
    /// Server span (for user prompts).
    pub const SERVER: i32 = 2;
    /// Client span (for tool calls).
    pub const CLIENT: i32 = 3;
}

/// Status code constants.
pub mod status_code {
    /// Unset status.
    #[allow(dead_code)]
    pub const UNSET: i32 = 0;
    /// OK status.
    pub const OK: i32 = 1;
    /// Error status.
    #[allow(dead_code)]
    pub const ERROR: i32 = 2;
}

/// Generate a trace ID from a session ID.
fn trace_id_from_session(session_id: &str) -> String {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};

    let mut hasher = DefaultHasher::new();
    session_id.hash(&mut hasher);
    let hash1 = hasher.finish();
    session_id.chars().rev().collect::<String>().hash(&mut hasher);
    let hash2 = hasher.finish();

    format!("{:016x}{:016x}", hash1, hash2)
}

/// Generate a span ID from content.
fn span_id_from_content(content: &str, index: usize) -> String {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};

    let mut hasher = DefaultHasher::new();
    content.hash(&mut hasher);
    index.hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

/// Convert a DateTime to nanoseconds since epoch.
fn datetime_to_nanos(dt: &DateTime<Utc>) -> String {
    let nanos = dt.timestamp_nanos_opt().unwrap_or(0);
    nanos.to_string()
}

/// Extract text content from a LogEntry.
fn extract_text_content(entry: &LogEntry) -> String {
    match entry {
        LogEntry::User(user) => {
            match &user.message {
                crate::model::UserContent::Simple(simple) => simple.content.clone(),
                crate::model::UserContent::Blocks(blocks) => {
                    blocks.content
                        .iter()
                        .filter_map(|block| {
                            if let ContentBlock::Text(text) = block {
                                Some(text.text.as_str())
                            } else {
                                None
                            }
                        })
                        .collect::<Vec<_>>()
                        .join("\n")
                }
            }
        }
        LogEntry::Assistant(assistant) => {
            assistant
                .message
                .content
                .iter()
                .filter_map(|block| {
                    if let ContentBlock::Text(text) = block {
                        Some(text.text.as_str())
                    } else {
                        None
                    }
                })
                .collect::<Vec<_>>()
                .join("\n")
        }
        LogEntry::System(system) => {
            system.content.clone().unwrap_or_default()
        }
        _ => String::new(),
    }
}

/// Extract thinking content from a LogEntry.
fn extract_thinking_content(entry: &LogEntry) -> Option<String> {
    if let LogEntry::Assistant(assistant) = entry {
        let thinking: Vec<_> = assistant
            .message
            .content
            .iter()
            .filter_map(|block| {
                if let ContentBlock::Thinking(thinking) = block {
                    Some(thinking.thinking.as_str())
                } else {
                    None
                }
            })
            .collect();
        if thinking.is_empty() {
            None
        } else {
            Some(thinking.join("\n"))
        }
    } else {
        None
    }
}

/// Extract tool calls from a LogEntry.
fn extract_tool_calls(entry: &LogEntry) -> Vec<(String, Option<serde_json::Value>)> {
    if let LogEntry::Assistant(assistant) = entry {
        assistant
            .message
            .content
            .iter()
            .filter_map(|block| {
                if let ContentBlock::ToolUse(tool_use) = block {
                    Some((tool_use.name.clone(), Some(tool_use.input.clone())))
                } else {
                    None
                }
            })
            .collect()
    } else {
        Vec::new()
    }
}

/// Get model name from a LogEntry.
fn get_model(entry: &LogEntry) -> Option<String> {
    if let LogEntry::Assistant(assistant) = entry {
        Some(assistant.message.model.clone())
    } else {
        None
    }
}

/// Get cwd from a LogEntry.
fn get_cwd(entry: &LogEntry) -> Option<String> {
    match entry {
        LogEntry::Assistant(m) => m.cwd.clone(),
        LogEntry::User(m) => m.cwd.clone(),
        LogEntry::System(m) => m.cwd.clone(),
        _ => None,
    }
}

impl OtelExporter {
    /// Export conversation to OTLP format.
    fn export_conversation_inner(
        &self,
        conversation: &Conversation,
        options: &ExportOptions,
    ) -> OtlpExportData {
        let entries = conversation.main_thread_entries();

        let session_id = entries
            .first()
            .and_then(|e| e.session_id())
            .unwrap_or("unknown");

        let trace_id = trace_id_from_session(session_id);
        let mut spans = Vec::new();

        // Create root span for the conversation
        let conversation_span_id = span_id_from_content(session_id, 0);
        let (start_time, end_time) = self.get_time_range(&entries);

        let mut root_attributes = vec![
            KeyValue::string("session.id", session_id),
            KeyValue::int("conversation.turns", entries.len() as i64 / 2),
        ];

        // Add model information from first assistant message
        if let Some(entry) = entries.iter().find(|e| matches!(e, LogEntry::Assistant(_))) {
            if let Some(model) = get_model(entry) {
                if !options.should_strip_model() {
                    root_attributes.push(KeyValue::string("llm.model", model));
                }
            }
            if let Some(cwd) = get_cwd(entry) {
                if !options.should_strip_cwd() {
                    root_attributes.push(KeyValue::string("process.cwd", cwd));
                }
            }
        }

        // Calculate total usage
        let mut total_input_tokens: u64 = 0;
        let mut total_output_tokens: u64 = 0;
        let mut total_cache_read: u64 = 0;
        let mut total_cache_write: u64 = 0;

        for entry in &entries {
            if let Some(usage) = entry.usage() {
                total_input_tokens += usage.input_tokens;
                total_output_tokens += usage.output_tokens;
                if let Some(cache) = usage.cache_read_input_tokens {
                    total_cache_read += cache;
                }
                if let Some(cache) = usage.cache_creation_input_tokens {
                    total_cache_write += cache;
                }
            }
        }

        if total_input_tokens > 0 || total_output_tokens > 0 {
            root_attributes.push(KeyValue::int("llm.usage.input_tokens", total_input_tokens as i64));
            root_attributes.push(KeyValue::int("llm.usage.output_tokens", total_output_tokens as i64));
            if total_cache_read > 0 {
                root_attributes.push(KeyValue::int("llm.usage.cache_read_tokens", total_cache_read as i64));
            }
            if total_cache_write > 0 {
                root_attributes.push(KeyValue::int("llm.usage.cache_write_tokens", total_cache_write as i64));
            }
        }

        let root_span = Span {
            trace_id: trace_id.clone(),
            span_id: conversation_span_id.clone(),
            parent_span_id: None,
            name: format!("conversation/{}", &session_id[..8.min(session_id.len())]),
            kind: span_kind::SERVER,
            start_time_unix_nano: datetime_to_nanos(&start_time),
            end_time_unix_nano: datetime_to_nanos(&end_time),
            attributes: root_attributes,
            events: Vec::new(),
            status: SpanStatus {
                code: status_code::OK,
                message: None,
            },
        };
        spans.push(root_span);

        // Create spans for each turn (user + assistant pair)
        let mut turn_index = 0;
        let mut i = 0;
        while i < entries.len() {
            let entry = entries[i];

            // Look for user message followed by assistant response
            if matches!(entry, LogEntry::User(_)) {
                turn_index += 1;
                let turn_span_id = span_id_from_content(&format!("turn-{}", turn_index), turn_index);

                let turn_start = entry.timestamp().unwrap_or_else(Utc::now);
                let mut turn_end = turn_start;
                let mut turn_events = Vec::new();
                let mut turn_attributes = vec![
                    KeyValue::int("turn.index", turn_index as i64),
                    KeyValue::string("turn.user_content", self.truncate_content(&extract_text_content(entry), 1000)),
                ];

                // Check for assistant response
                if i + 1 < entries.len() {
                    if let LogEntry::Assistant(_) = entries[i + 1] {
                        let assistant_entry = entries[i + 1];
                        turn_end = assistant_entry.timestamp().unwrap_or(turn_end);

                        // Add assistant content
                        turn_attributes.push(KeyValue::string(
                            "turn.assistant_content",
                            self.truncate_content(&extract_text_content(assistant_entry), 1000),
                        ));

                        // Add thinking as events if enabled
                        if self.include_thinking_events && options.should_include_thinking() {
                            if let Some(thinking) = extract_thinking_content(assistant_entry) {
                                turn_events.push(SpanEvent {
                                    name: "thinking".to_string(),
                                    time_unix_nano: datetime_to_nanos(&turn_start),
                                    attributes: vec![KeyValue::string(
                                        "content",
                                        self.truncate_content(&thinking, 2000),
                                    )],
                                });
                            }
                        }

                        // Add tool calls as child spans
                        if self.include_tool_details && options.should_include_tool_use() {
                            let tool_calls = extract_tool_calls(assistant_entry);
                            for (tool_idx, (tool_name, tool_input)) in tool_calls.iter().enumerate() {
                                let tool_span_id = span_id_from_content(
                                    &format!("tool-{}-{}", turn_index, tool_idx),
                                    turn_index * 1000 + tool_idx,
                                );

                                let mut tool_attrs = vec![
                                    KeyValue::string("tool.name", tool_name.clone()),
                                ];

                                // Add tool input (truncated)
                                if let Some(input) = tool_input {
                                    tool_attrs.push(KeyValue::string(
                                        "tool.input",
                                        self.truncate_content(&input.to_string(), 500),
                                    ));
                                }

                                let tool_span = Span {
                                    trace_id: trace_id.clone(),
                                    span_id: tool_span_id,
                                    parent_span_id: Some(turn_span_id.clone()),
                                    name: format!("tool/{}", tool_name),
                                    kind: span_kind::CLIENT,
                                    start_time_unix_nano: datetime_to_nanos(&turn_start),
                                    end_time_unix_nano: datetime_to_nanos(&turn_end),
                                    attributes: tool_attrs,
                                    events: Vec::new(),
                                    status: SpanStatus {
                                        code: status_code::OK,
                                        message: None,
                                    },
                                };
                                spans.push(tool_span);
                            }
                        }

                        // Add usage to turn
                        if let Some(usage) = assistant_entry.usage() {
                            turn_attributes.push(KeyValue::int("llm.usage.input_tokens", usage.input_tokens as i64));
                            turn_attributes.push(KeyValue::int("llm.usage.output_tokens", usage.output_tokens as i64));
                        }

                        i += 1; // Skip the assistant entry
                    }
                }

                let turn_span = Span {
                    trace_id: trace_id.clone(),
                    span_id: turn_span_id,
                    parent_span_id: Some(conversation_span_id.clone()),
                    name: format!("turn/{}", turn_index),
                    kind: span_kind::INTERNAL,
                    start_time_unix_nano: datetime_to_nanos(&turn_start),
                    end_time_unix_nano: datetime_to_nanos(&turn_end),
                    attributes: turn_attributes,
                    events: turn_events,
                    status: SpanStatus {
                        code: status_code::OK,
                        message: None,
                    },
                };
                spans.push(turn_span);
            }

            i += 1;
        }

        // Build resource spans
        let resource = Resource {
            attributes: vec![
                KeyValue::string("service.name", self.service_name.clone()),
                KeyValue::string("service.version", crate::VERSION),
                KeyValue::string("telemetry.sdk.name", "claude-snatch"),
                KeyValue::string("telemetry.sdk.language", "rust"),
            ],
        };

        let scope_spans = ScopeSpans {
            scope: InstrumentationScope {
                name: "claude-snatch".to_string(),
                version: Some(crate::VERSION.to_string()),
            },
            spans,
        };

        OtlpExportData {
            resource_spans: vec![ResourceSpans {
                resource,
                scope_spans: vec![scope_spans],
            }],
        }
    }

    /// Get the time range of entries.
    fn get_time_range(&self, entries: &[&LogEntry]) -> (DateTime<Utc>, DateTime<Utc>) {
        let start = entries
            .first()
            .and_then(|e| e.timestamp())
            .unwrap_or_else(Utc::now);

        let end = entries
            .last()
            .and_then(|e| e.timestamp())
            .unwrap_or(start);

        (start, end)
    }

    /// Truncate content to a maximum length.
    fn truncate_content(&self, content: &str, max_len: usize) -> String {
        if content.len() <= max_len {
            content.to_string()
        } else {
            format!("{}...", &content[..max_len.saturating_sub(3)])
        }
    }

    /// Export entries to spans (for raw entry export).
    fn export_entries_inner(&self, entries: &[LogEntry]) -> OtlpExportData {
        let mut spans = Vec::new();
        let trace_id = trace_id_from_session("raw-entries");

        for (idx, entry) in entries.iter().enumerate() {
            let span_id = span_id_from_content(&format!("entry-{}", idx), idx);
            let timestamp = entry.timestamp().unwrap_or_else(Utc::now);

            let mut attributes = vec![
                KeyValue::string("entry.type", entry.message_type()),
            ];

            if let Some(uuid) = entry.uuid() {
                attributes.push(KeyValue::string("entry.uuid", uuid.to_string()));
            }

            if let Some(session) = entry.session_id() {
                attributes.push(KeyValue::string("session.id", session.to_string()));
            }

            if let Some(model) = get_model(entry) {
                attributes.push(KeyValue::string("llm.model", model));
            }

            // Add usage if available
            if let Some(usage) = entry.usage() {
                attributes.push(KeyValue::int("llm.usage.input_tokens", usage.input_tokens as i64));
                attributes.push(KeyValue::int("llm.usage.output_tokens", usage.output_tokens as i64));
            }

            let span = Span {
                trace_id: trace_id.clone(),
                span_id,
                parent_span_id: None,
                name: format!("entry/{}", entry.message_type()),
                kind: span_kind::INTERNAL,
                start_time_unix_nano: datetime_to_nanos(&timestamp),
                end_time_unix_nano: datetime_to_nanos(&timestamp),
                attributes,
                events: Vec::new(),
                status: SpanStatus {
                    code: status_code::OK,
                    message: None,
                },
            };
            spans.push(span);
        }

        let resource = Resource {
            attributes: vec![
                KeyValue::string("service.name", self.service_name.clone()),
                KeyValue::string("telemetry.sdk.name", "claude-snatch"),
            ],
        };

        let scope_spans = ScopeSpans {
            scope: InstrumentationScope {
                name: "claude-snatch".to_string(),
                version: Some(crate::VERSION.to_string()),
            },
            spans,
        };

        OtlpExportData {
            resource_spans: vec![ResourceSpans {
                resource,
                scope_spans: vec![scope_spans],
            }],
        }
    }
}

impl Exporter for OtelExporter {
    fn export_conversation<W: Write>(
        &self,
        conversation: &Conversation,
        writer: &mut W,
        options: &ExportOptions,
    ) -> Result<()> {
        let data = self.export_conversation_inner(conversation, options);
        let json = serde_json::to_string_pretty(&data)?;
        writer.write_all(json.as_bytes())?;
        Ok(())
    }

    fn export_entries<W: Write>(
        &self,
        entries: &[LogEntry],
        writer: &mut W,
        _options: &ExportOptions,
    ) -> Result<()> {
        let data = self.export_entries_inner(entries);
        let json = serde_json::to_string_pretty(&data)?;
        writer.write_all(json.as_bytes())?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_trace_id_generation() {
        let id1 = trace_id_from_session("session-123");
        let id2 = trace_id_from_session("session-123");
        let id3 = trace_id_from_session("session-456");

        // Same input produces same output
        assert_eq!(id1, id2);
        // Different input produces different output
        assert_ne!(id1, id3);
        // Correct length (32 hex chars)
        assert_eq!(id1.len(), 32);
    }

    #[test]
    fn test_span_id_generation() {
        let id1 = span_id_from_content("content", 0);
        let id2 = span_id_from_content("content", 0);
        let id3 = span_id_from_content("content", 1);

        // Same input produces same output
        assert_eq!(id1, id2);
        // Different index produces different output
        assert_ne!(id1, id3);
        // Correct length (16 hex chars)
        assert_eq!(id1.len(), 16);
    }

    #[test]
    fn test_key_value_string() {
        let kv = KeyValue::string("key", "value");
        assert_eq!(kv.key, "key");
        match kv.value {
            AnyValue::StringValue(s) => assert_eq!(s, "value"),
            _ => panic!("Expected StringValue"),
        }
    }

    #[test]
    fn test_key_value_int() {
        let kv = KeyValue::int("count", 42);
        assert_eq!(kv.key, "count");
        match kv.value {
            AnyValue::IntValue(i) => assert_eq!(i, 42),
            _ => panic!("Expected IntValue"),
        }
    }

    #[test]
    fn test_key_value_bool() {
        let kv = KeyValue::bool("enabled", true);
        assert_eq!(kv.key, "enabled");
        match kv.value {
            AnyValue::BoolValue(b) => assert!(b),
            _ => panic!("Expected BoolValue"),
        }
    }

    #[test]
    fn test_exporter_builder() {
        let exporter = OtelExporter::new()
            .with_service_name("my-service")
            .with_thinking_events(false)
            .with_tool_details(false);

        assert_eq!(exporter.service_name, "my-service");
        assert!(!exporter.include_thinking_events);
        assert!(!exporter.include_tool_details);
    }

    #[test]
    fn test_truncate_content() {
        let exporter = OtelExporter::new();

        // Short content unchanged
        let short = "hello";
        assert_eq!(exporter.truncate_content(short, 10), "hello");

        // Long content truncated
        let long = "hello world";
        assert_eq!(exporter.truncate_content(long, 8), "hello...");
    }

    #[test]
    fn test_otlp_serialization() {
        let data = OtlpExportData {
            resource_spans: vec![ResourceSpans {
                resource: Resource {
                    attributes: vec![KeyValue::string("service.name", "test")],
                },
                scope_spans: vec![ScopeSpans {
                    scope: InstrumentationScope {
                        name: "test-scope".to_string(),
                        version: Some("1.0.0".to_string()),
                    },
                    spans: vec![Span {
                        trace_id: "00000000000000000000000000000001".to_string(),
                        span_id: "0000000000000001".to_string(),
                        parent_span_id: None,
                        name: "test-span".to_string(),
                        kind: span_kind::INTERNAL,
                        start_time_unix_nano: "1000000000".to_string(),
                        end_time_unix_nano: "2000000000".to_string(),
                        attributes: vec![],
                        events: vec![],
                        status: SpanStatus {
                            code: status_code::OK,
                            message: None,
                        },
                    }],
                }],
            }],
        };

        let json = serde_json::to_string(&data).unwrap();
        assert!(json.contains("resourceSpans"));
        assert!(json.contains("test-span"));
        assert!(json.contains("service.name"));
    }
}
