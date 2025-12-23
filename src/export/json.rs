//! JSON export for lossless conversation data.
//!
//! Provides lossless JSON export that preserves all data including
//! unknown fields for forward compatibility. This format is suitable
//! for archival and programmatic processing.

use std::io::Write;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::analytics::SessionAnalytics;
use crate::error::Result;
use crate::model::LogEntry;
use crate::reconstruction::Conversation;

use super::{ExportOptions, Exporter};

/// JSON exporter for lossless data export.
#[derive(Debug, Clone)]
pub struct JsonExporter {
    /// Pretty-print the JSON output.
    pretty: bool,
    /// Include analytics in output.
    include_analytics: bool,
    /// Include tree structure metadata.
    include_tree_metadata: bool,
    /// Wrap entries in envelope with metadata.
    use_envelope: bool,
}

impl Default for JsonExporter {
    fn default() -> Self {
        Self::new()
    }
}

impl JsonExporter {
    /// Create a new JSON exporter.
    #[must_use]
    pub fn new() -> Self {
        Self {
            pretty: false,
            include_analytics: true,
            include_tree_metadata: true,
            use_envelope: true,
        }
    }

    /// Enable pretty-printing.
    #[must_use]
    pub fn pretty(mut self, pretty: bool) -> Self {
        self.pretty = pretty;
        self
    }

    /// Include analytics in output.
    #[must_use]
    pub fn with_analytics(mut self, include: bool) -> Self {
        self.include_analytics = include;
        self
    }

    /// Include tree structure metadata.
    #[must_use]
    pub fn with_tree_metadata(mut self, include: bool) -> Self {
        self.include_tree_metadata = include;
        self
    }

    /// Use envelope wrapper.
    #[must_use]
    pub fn with_envelope(mut self, use_envelope: bool) -> Self {
        self.use_envelope = use_envelope;
        self
    }

    /// Write JSON value to writer.
    fn write_json<W: Write>(&self, writer: &mut W, value: &Value) -> Result<()> {
        if self.pretty {
            serde_json::to_writer_pretty(writer, value)?;
        } else {
            serde_json::to_writer(writer, value)?;
        }
        Ok(())
    }
}

impl Exporter for JsonExporter {
    fn export_conversation<W: Write>(
        &self,
        conversation: &Conversation,
        writer: &mut W,
        options: &ExportOptions,
    ) -> Result<()> {
        if self.use_envelope {
            let export = ConversationExport::from_conversation(
                conversation,
                options,
                self.include_analytics,
                self.include_tree_metadata,
            );

            let value = serde_json::to_value(&export)?;
            self.write_json(writer, &value)?;
        } else {
            // Export entries directly as array
            let entries = if options.main_thread_only {
                conversation.main_thread_entries()
            } else {
                conversation.chronological_entries()
            };

            let filtered = filter_entries(&entries, options);
            let value = serde_json::to_value(&filtered)?;
            self.write_json(writer, &value)?;
        }

        Ok(())
    }

    fn export_entries<W: Write>(
        &self,
        entries: &[LogEntry],
        writer: &mut W,
        options: &ExportOptions,
    ) -> Result<()> {
        let refs: Vec<&LogEntry> = entries.iter().collect();
        let filtered = filter_entries(&refs, options);
        let value = serde_json::to_value(&filtered)?;
        self.write_json(writer, &value)?;
        Ok(())
    }
}

/// Filter entries based on options.
fn filter_entries(entries: &[&LogEntry], options: &ExportOptions) -> Vec<FilteredEntry> {
    entries
        .iter()
        .filter_map(|entry| {
            if !options.include_system {
                if let LogEntry::System(_) = entry {
                    return None;
                }
            }

            Some(FilteredEntry::from_entry(entry, options))
        })
        .collect()
}

/// Wrapper for filtered entry with content filtering.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum FilteredEntry {
    /// Full entry (no filtering applied).
    Full(Value),
    /// Filtered entry with some content removed.
    Filtered(Value),
}

impl FilteredEntry {
    /// Create from a log entry with filtering.
    fn from_entry(entry: &LogEntry, options: &ExportOptions) -> Self {
        // Serialize to Value first
        let mut value = serde_json::to_value(entry).unwrap_or(Value::Null);

        // Apply content filtering if needed
        if !options.include_thinking
            || !options.include_tool_use
            || !options.include_tool_results
            || !options.include_images
        {
            if let Some(content) = value.get_mut("message").and_then(|m| m.get_mut("content")) {
                if let Some(arr) = content.as_array_mut() {
                    arr.retain(|block| {
                        let block_type = block.get("type").and_then(Value::as_str);
                        match block_type {
                            Some("thinking") => options.include_thinking,
                            Some("tool_use") => options.include_tool_use,
                            Some("tool_result") => options.include_tool_results,
                            Some("image") => options.include_images,
                            _ => true,
                        }
                    });
                }
            }
        }

        // Apply truncation if configured
        if let Some(max_len) = options.truncate_at {
            truncate_content(&mut value, max_len);
        }

        FilteredEntry::Full(value)
    }
}

/// Truncate string content in a value.
fn truncate_content(value: &mut Value, max_len: usize) {
    match value {
        Value::String(s) if s.len() > max_len => {
            s.truncate(max_len);
            s.push_str("...[truncated]");
        }
        Value::Array(arr) => {
            for item in arr {
                truncate_content(item, max_len);
            }
        }
        Value::Object(obj) => {
            for (_, v) in obj {
                truncate_content(v, max_len);
            }
        }
        _ => {}
    }
}

/// Complete conversation export with envelope.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConversationExport {
    /// Export format version.
    pub version: String,
    /// Export timestamp.
    pub exported_at: String,
    /// Export tool information.
    pub exporter: ExporterInfo,
    /// Session metadata.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<ExportMetadata>,
    /// Analytics summary.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub analytics: Option<ExportAnalytics>,
    /// Tree structure information.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tree: Option<TreeInfo>,
    /// The conversation entries.
    pub entries: Vec<FilteredEntry>,
}

impl ConversationExport {
    /// Create export from conversation.
    fn from_conversation(
        conversation: &Conversation,
        options: &ExportOptions,
        include_analytics: bool,
        include_tree: bool,
    ) -> Self {
        let entries = if options.main_thread_only {
            conversation.main_thread_entries()
        } else {
            conversation.chronological_entries()
        };

        let filtered_entries = filter_entries(&entries, options);

        // Extract metadata from first entry
        let metadata = entries.first().map(|first| {
            ExportMetadata {
                session_id: first.session_id().map(String::from),
                version: first.version().map(String::from),
                project_path: None, // Would need to be passed in
            }
        });

        // Generate analytics
        let analytics = if include_analytics {
            let session_analytics = SessionAnalytics::from_conversation(conversation);
            let summary = session_analytics.summary_report();
            Some(ExportAnalytics {
                total_messages: summary.total_messages,
                user_messages: summary.user_messages,
                assistant_messages: summary.assistant_messages,
                total_tokens: summary.total_tokens,
                input_tokens: summary.input_tokens,
                output_tokens: summary.output_tokens,
                tool_invocations: summary.tool_invocations,
                thinking_blocks: summary.thinking_blocks,
                cache_hit_rate: summary.cache_hit_rate,
                estimated_cost: summary.estimated_cost,
                duration_seconds: session_analytics.duration().map(|d| d.num_seconds()),
                primary_model: summary.primary_model,
            })
        } else {
            None
        };

        // Tree info
        let tree = if include_tree {
            let stats = conversation.statistics();
            Some(TreeInfo {
                total_nodes: stats.total_nodes,
                main_thread_length: stats.main_thread_length,
                max_depth: stats.max_depth,
                branch_count: stats.branch_count,
                roots: conversation.roots().to_vec(),
                branch_points: conversation.branch_points().to_vec(),
            })
        } else {
            None
        };

        Self {
            version: "1.0".to_string(),
            exported_at: chrono::Utc::now().to_rfc3339(),
            exporter: ExporterInfo {
                name: crate::NAME.to_string(),
                version: crate::VERSION.to_string(),
            },
            metadata,
            analytics,
            tree,
            entries: filtered_entries,
        }
    }
}

/// Exporter tool information.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExporterInfo {
    /// Tool name.
    pub name: String,
    /// Tool version.
    pub version: String,
}

/// Session metadata in export.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExportMetadata {
    /// Session ID.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    /// Claude Code version.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    /// Project path.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project_path: Option<String>,
}

/// Analytics summary in export.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExportAnalytics {
    /// Total messages.
    pub total_messages: usize,
    /// User messages.
    pub user_messages: usize,
    /// Assistant messages.
    pub assistant_messages: usize,
    /// Total tokens.
    pub total_tokens: u64,
    /// Input tokens.
    pub input_tokens: u64,
    /// Output tokens.
    pub output_tokens: u64,
    /// Tool invocations.
    pub tool_invocations: usize,
    /// Thinking blocks.
    pub thinking_blocks: usize,
    /// Cache hit rate percentage.
    pub cache_hit_rate: f64,
    /// Estimated cost in USD.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub estimated_cost: Option<f64>,
    /// Duration in seconds.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration_seconds: Option<i64>,
    /// Primary model used.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub primary_model: Option<String>,
}

/// Tree structure information.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TreeInfo {
    /// Total node count.
    pub total_nodes: usize,
    /// Main thread length.
    pub main_thread_length: usize,
    /// Maximum tree depth.
    pub max_depth: usize,
    /// Number of branch points.
    pub branch_count: usize,
    /// Root node UUIDs.
    pub roots: Vec<String>,
    /// Branch point UUIDs.
    pub branch_points: Vec<String>,
}

/// Export a single entry to JSON line (for JSONL output).
pub fn entry_to_jsonl(entry: &LogEntry) -> Result<String> {
    Ok(serde_json::to_string(entry)?)
}

/// Export entries to JSONL format.
pub fn entries_to_jsonl<W: Write>(
    entries: &[LogEntry],
    writer: &mut W,
) -> Result<()> {
    for entry in entries {
        writeln!(writer, "{}", entry_to_jsonl(entry)?)?;
    }
    Ok(())
}

/// Export conversation to JSONL format (one entry per line).
pub fn conversation_to_jsonl<W: Write>(
    conversation: &Conversation,
    writer: &mut W,
    main_thread_only: bool,
) -> Result<()> {
    let entries = if main_thread_only {
        conversation.main_thread_entries()
    } else {
        conversation.chronological_entries()
    };

    for entry in entries {
        let json = serde_json::to_string(entry)?;
        writeln!(writer, "{json}")?;
    }
    Ok(())
}

/// Parse JSONL and re-export (for round-trip testing).
pub fn round_trip_jsonl(input: &str) -> Result<String> {
    let mut output = String::new();

    for line in input.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        // Parse as generic Value to preserve all fields
        let value: Value = serde_json::from_str(trimmed)?;
        output.push_str(&serde_json::to_string(&value)?);
        output.push('\n');
    }

    Ok(output)
}

/// Diff two JSONL files for round-trip verification.
#[derive(Debug, Clone, Default)]
pub struct JsonlDiff {
    /// Lines only in first file.
    pub only_in_first: Vec<usize>,
    /// Lines only in second file.
    pub only_in_second: Vec<usize>,
    /// Lines that differ.
    pub different: Vec<(usize, String, String)>,
    /// Lines that match.
    pub matching: usize,
}

impl JsonlDiff {
    /// Check if files are identical.
    #[must_use]
    pub fn is_identical(&self) -> bool {
        self.only_in_first.is_empty()
            && self.only_in_second.is_empty()
            && self.different.is_empty()
    }

    /// Compare two JSONL strings.
    pub fn compare(first: &str, second: &str) -> Self {
        let lines1: Vec<&str> = first.lines().collect();
        let lines2: Vec<&str> = second.lines().collect();

        let mut diff = Self::default();
        let max_len = lines1.len().max(lines2.len());

        for i in 0..max_len {
            match (lines1.get(i), lines2.get(i)) {
                (Some(l1), Some(l2)) => {
                    // Normalize JSON for comparison
                    let v1: std::result::Result<Value, _> = serde_json::from_str(l1);
                    let v2: std::result::Result<Value, _> = serde_json::from_str(l2);

                    match (v1, v2) {
                        (Ok(v1), Ok(v2)) if v1 == v2 => {
                            diff.matching += 1;
                        }
                        _ => {
                            diff.different.push((i + 1, (*l1).to_string(), (*l2).to_string()));
                        }
                    }
                }
                (Some(_), None) => {
                    diff.only_in_first.push(i + 1);
                }
                (None, Some(_)) => {
                    diff.only_in_second.push(i + 1);
                }
                (None, None) => unreachable!(),
            }
        }

        diff
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_json_exporter_builder() {
        let exporter = JsonExporter::new()
            .pretty(true)
            .with_analytics(false)
            .with_envelope(false);

        assert!(exporter.pretty);
        assert!(!exporter.include_analytics);
        assert!(!exporter.use_envelope);
    }

    #[test]
    fn test_truncate_content() {
        let mut value = serde_json::json!({
            "text": "a".repeat(100),
            "nested": {
                "inner": "b".repeat(100)
            }
        });

        truncate_content(&mut value, 20);

        let text = value.get("text").and_then(Value::as_str).unwrap();
        assert!(text.contains("[truncated]"));
        assert!(text.len() < 100);
    }

    #[test]
    fn test_jsonl_diff_identical() {
        let a = r#"{"a":1}
{"b":2}"#;
        let b = r#"{"a":1}
{"b":2}"#;

        let diff = JsonlDiff::compare(a, b);
        assert!(diff.is_identical());
        assert_eq!(diff.matching, 2);
    }

    #[test]
    fn test_jsonl_diff_different() {
        let a = r#"{"a":1}
{"b":2}"#;
        let b = r#"{"a":1}
{"b":3}"#;

        let diff = JsonlDiff::compare(a, b);
        assert!(!diff.is_identical());
        assert_eq!(diff.matching, 1);
        assert_eq!(diff.different.len(), 1);
    }

    #[test]
    fn test_round_trip() {
        let input = r#"{"type":"user","uuid":"1","extra":{"unknown":"preserved"}}
{"type":"assistant","uuid":"2"}"#;

        let output = round_trip_jsonl(input).unwrap();

        // Parse both and compare
        for (line_in, line_out) in input.lines().zip(output.lines()) {
            let v1: Value = serde_json::from_str(line_in).unwrap();
            let v2: Value = serde_json::from_str(line_out).unwrap();
            assert_eq!(v1, v2);
        }
    }
}
