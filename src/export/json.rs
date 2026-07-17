//! JSON export for conversation data.
//!
//! Provides a normalized, content-preserving JSON export that retains all
//! data — including unknown entry types, content blocks, and fields — for
//! forward compatibility. It is not byte-for-byte identical to the source
//! (fields may be reordered and orphan entries are emitted first), so it is
//! content-preserving rather than strictly lossless. Suitable for archival
//! and programmatic processing.

use std::collections::BTreeMap;
use std::io::Write;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::analytics::SessionAnalytics;
use crate::error::{Result, SnatchError};
use crate::model::LogEntry;
use crate::reconstruction::Conversation;

use super::{ExportOptions, Exporter};

/// JSON exporter (normalized, content-preserving).
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
    /// Resume-chain metadata, when this export merges a multi-file chain.
    chain: Option<ChainExportMeta>,
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
            chain: None,
        }
    }

    /// Attach resume-chain metadata to the export envelope.
    #[must_use]
    pub fn with_chain(mut self, chain: Option<ChainExportMeta>) -> Self {
        self.chain = chain;
        self
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
            validate_provider_export_bundle(conversation)?;
            let mut export = ConversationExport::from_conversation(
                conversation,
                options,
                self.include_analytics,
                self.include_tree_metadata,
            );
            if export
                .provider
                .as_ref()
                .is_some_and(|provider| provider.unidentified_entry_count != 0)
            {
                return Err(SnatchError::export(
                    "provider-normalized JSON contains an entry without deterministic identity",
                ));
            }
            export.chain = self.chain.clone();

            let value = serde_json::to_value(&export)?;
            self.write_json(writer, &value)?;
        } else {
            if conversation.provider_bundle().is_some() {
                return Err(SnatchError::export(
                    "provider-normalized JSON requires the versioned envelope so derivation metadata is not lost",
                ));
            }
            // Export entries directly as array
            let entries = conversation.entries_for_export(options.main_thread_only);

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

fn validate_provider_export_bundle(conversation: &Conversation) -> Result<()> {
    let Some(bundle) = conversation.provider_bundle() else {
        return Ok(());
    };
    let violations = bundle.validate_provenance();
    if violations.is_empty() {
        return Ok(());
    }
    Err(SnatchError::export(format!(
        "provider-normalized export has invalid provenance ({} violation{})",
        violations.len(),
        if violations.len() == 1 { "" } else { "s" }
    )))
}

/// Check if an entry should be included based on options.
fn should_include_entry(entry: &LogEntry, options: &ExportOptions) -> bool {
    match entry {
        LogEntry::User(_) => options.should_include_user(),
        LogEntry::Assistant(_) => options.should_include_assistant(),
        LogEntry::System(_) => options.should_include_system(),
        LogEntry::Summary(_) => options.should_include_summary(),
        // Always include structural and metadata entries
        LogEntry::FileHistorySnapshot(_)
        | LogEntry::QueueOperation(_)
        | LogEntry::TurnEnd(_)
        | LogEntry::Progress(_)
        | LogEntry::Attachment(_)
        | LogEntry::LastPrompt(_)
        | LogEntry::Mode(_)
        | LogEntry::PermissionMode(_)
        | LogEntry::AiTitle(_)
        | LogEntry::Unknown(_) => true,
    }
}

/// Filter entries based on options.
fn filter_entries(entries: &[&LogEntry], options: &ExportOptions) -> Vec<FilteredEntry> {
    entries
        .iter()
        .filter_map(|entry| {
            if !should_include_entry(entry, options) {
                return None;
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
    /// Create from a log entry.
    ///
    /// Block-level content filtering is applied upstream by the dispatch
    /// transform, so the entry is already pruned by the time it reaches here.
    fn from_entry(entry: &LogEntry, options: &ExportOptions) -> Self {
        // Serialize to Value first
        let mut value = serde_json::to_value(entry).unwrap_or(Value::Null);

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
    /// Resume-chain metadata, present only when a multi-file chain was merged.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub chain: Option<ChainExportMeta>,
    /// Provider-qualified identity and per-entry derivation/provenance for
    /// conversations normalized through a SourceProvider. Absent on classic
    /// Claude paths, preserving their established envelope.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider: Option<ProviderExportMetadata>,
    /// Analytics summary.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub analytics: Option<ExportAnalytics>,
    /// Tree structure information.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tree: Option<TreeInfo>,
    /// The conversation entries.
    pub entries: Vec<FilteredEntry>,
}

/// Machine-readable provider and derivation metadata for normalized exports.
///
/// Artifact references are opaque export-local labels rather than source
/// locators: provenance remains useful without leaking filesystem paths or
/// duplicating native content into a redacted normalized export.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderExportMetadata {
    /// Version of this normalized provider-metadata envelope.
    pub format_version: u32,
    /// Stable provider id (`claude-code`, `codex`, ...).
    pub id: String,
    /// Reversible provider-qualified logical session identity.
    pub qualified_session_id: String,
    /// Export-local artifact labels and non-sensitive structural attributes.
    pub artifacts: Vec<ExportArtifactRef>,
    /// Claude-shaped fields synthesized by the adapter, if any.
    pub field_derivations: Vec<ExportFieldDerivation>,
    /// Native-record accounting from the complete parsed bundle.
    pub record_accounting: ExportRecordAccounting,
    /// Entries in the rendered view that unexpectedly lacked deterministic
    /// identity. Valid provider bundles must report zero.
    pub unidentified_entry_count: usize,
    /// Per-rendered-entry identity and source-record derivation.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub entries: Vec<EntryDerivationExport>,
}

/// Non-sensitive export-local reference to one source artifact.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExportArtifactRef {
    /// Export-local opaque label (`artifact-0`, ...).
    pub reference: String,
    /// Provider-neutral physical form.
    pub form: String,
    /// Whether the provider classified this artifact as archived.
    pub archived: bool,
}

/// One normalized field synthesized by an adapter.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExportFieldDerivation {
    /// Serialized normalized field path.
    pub field: String,
    /// Stable derivation method identifier.
    pub method: String,
}

/// Aggregate native-record disposition counts.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExportRecordAccounting {
    /// Records normalized into entries.
    pub mapped: usize,
    /// Records intentionally suppressed with a reason.
    pub suppressed: usize,
    /// Parseable-but-unmodeled records retained as Unknown entries.
    pub unknown: usize,
    /// Damaged records from which entries were recovered.
    pub recovered: usize,
    /// Records that could not be parsed or recovered.
    pub unparseable: usize,
}

/// One normalized entry's deterministic identity and native origins.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EntryDerivationExport {
    /// Reversible deterministic normalized entry identity.
    pub entry_id: String,
    /// Native records that produced the entry.
    pub origins: Vec<ExportRecordRef>,
}

/// Export-local reference to a native record.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExportRecordRef {
    /// Export-local artifact label.
    pub artifact: String,
    /// Zero-based native record ordinal within that artifact.
    pub ordinal: u64,
}

impl ProviderExportMetadata {
    fn from_conversation(
        conversation: &Conversation,
        main_thread_only: bool,
        options: Option<&ExportOptions>,
    ) -> Option<Self> {
        let bundle = conversation.provider_bundle()?;
        let mut artifacts: Vec<_> = bundle.descriptor.artifacts.iter().collect();
        artifacts.sort_by_key(|artifact| &artifact.snapshot.id);
        let artifact_labels: BTreeMap<_, _> = artifacts
            .iter()
            .enumerate()
            .map(|(index, artifact)| (artifact.snapshot.id.clone(), format!("artifact-{index}")))
            .collect();
        let artifacts = artifacts
            .into_iter()
            .enumerate()
            .map(|(index, artifact)| ExportArtifactRef {
                reference: format!("artifact-{index}"),
                form: match &artifact.form {
                    crate::provider::ArtifactForm::PlainFile => "plain_file".to_string(),
                    crate::provider::ArtifactForm::CompressedFile => "compressed_file".to_string(),
                    crate::provider::ArtifactForm::Database => "database".to_string(),
                    crate::provider::ArtifactForm::Other(_) => "other".to_string(),
                },
                archived: artifact.archived,
            })
            .collect();

        let field_derivations = bundle
            .field_derivations
            .iter()
            .map(|derivation| ExportFieldDerivation {
                field: match derivation.field {
                    crate::provider::NormalizedField::Uuid => "uuid",
                    crate::provider::NormalizedField::ParentUuid => "parentUuid",
                    crate::provider::NormalizedField::LogicalParentUuid => "logicalParentUuid",
                    crate::provider::NormalizedField::MessageId => "message.id",
                }
                .to_string(),
                method: match derivation.method {
                    crate::provider::FieldDerivationMethod::DeterministicEntryId => {
                        "deterministic_entry_id"
                    }
                    crate::provider::FieldDerivationMethod::PreviousNormalizedEmission => {
                        "previous_normalized_emission"
                    }
                }
                .to_string(),
            })
            .collect();

        let mut entries = Vec::new();
        let mut unidentified_entry_count = 0;
        for (entry, id) in conversation.identified_entries_for_export(main_thread_only) {
            if options.is_some_and(|options| !should_include_entry(entry, options)) {
                continue;
            }
            let Some(id) = id else {
                unidentified_entry_count += 1;
                continue;
            };
            let origins = bundle
                .entry_origins
                .get(id)
                .into_iter()
                .flatten()
                .map(|record| ExportRecordRef {
                    artifact: artifact_labels
                        .get(&record.artifact)
                        .cloned()
                        .unwrap_or_else(|| "unlisted-artifact".to_string()),
                    ordinal: record.ordinal,
                })
                .collect();
            entries.push(EntryDerivationExport {
                entry_id: id.to_string(),
                origins,
            });
        }

        let diagnostics = &bundle.diagnostics;
        Some(Self {
            format_version: 1,
            id: bundle.descriptor.key.provider.to_string(),
            qualified_session_id: bundle.descriptor.key.to_string(),
            artifacts,
            field_derivations,
            record_accounting: ExportRecordAccounting {
                mapped: diagnostics.mapped,
                suppressed: diagnostics.suppressed,
                unknown: diagnostics.unknown,
                recovered: diagnostics.recovered,
                unparseable: diagnostics.unparseable,
            },
            unidentified_entry_count,
            entries,
        })
    }
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

        // Include compaction summaries (absent from the node tree) in the
        // serialized entries, but keep metadata extraction below on the thread.
        let export_entries = conversation.entries_for_export(options.main_thread_only);
        let filtered_entries = filter_entries(&export_entries, options);

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
                span_seconds: session_analytics.duration().map(|d| d.num_seconds()),
                primary_model: summary.primary_model,
                subagent_count: summary.subagent_count,
                subagent_tokens: summary.subagent_tokens,
                subagent_tool_invocations: summary.subagent_tool_invocations,
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
            // Set by the exporter (it knows whether a chain was merged).
            chain: None,
            provider: ProviderExportMetadata::from_conversation(
                conversation,
                options.main_thread_only,
                Some(options),
            ),
            analytics,
            tree,
            entries: filtered_entries,
        }
    }
}

/// Resume-chain metadata embedded in the JSON export envelope.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChainExportMeta {
    /// Root file id of the chain.
    pub root_id: String,
    /// All member file ids, in chain order.
    pub members: Vec<String>,
    /// Number of files merged.
    pub member_count: usize,
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
    /// Wall-clock span in seconds (first to last entry; includes idle time).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub span_seconds: Option<i64>,
    /// Primary model used.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub primary_model: Option<String>,
    /// Number of subagent (Task) invocations that reported usage.
    #[serde(skip_serializing_if = "is_zero_usize")]
    pub subagent_count: usize,
    /// Total tokens consumed by subagents (mined from Task results). Separate
    /// from `total_tokens`, which is the parent session's own usage.
    #[serde(skip_serializing_if = "is_zero_u64")]
    pub subagent_tokens: u64,
    /// Total tool invocations across all subagents.
    #[serde(skip_serializing_if = "is_zero_u64")]
    pub subagent_tool_invocations: u64,
}

#[allow(clippy::trivially_copy_pass_by_ref)]
fn is_zero_usize(v: &usize) -> bool {
    *v == 0
}

#[allow(clippy::trivially_copy_pass_by_ref)]
fn is_zero_u64(v: &u64) -> bool {
    *v == 0
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

/// Streaming JSON exporter for memory-efficient large file export.
///
/// Writes JSON entries one at a time without loading the entire
/// dataset into memory. Suitable for sessions with thousands of entries.
#[derive(Debug, Clone)]
pub struct StreamingJsonExporter {
    /// Pretty-print the JSON output.
    pretty: bool,
    /// Current indentation level (for pretty printing).
    indent_level: usize,
    /// Whether we've written the first entry.
    first_entry: bool,
}

impl Default for StreamingJsonExporter {
    fn default() -> Self {
        Self::new()
    }
}

impl StreamingJsonExporter {
    /// Create a new streaming JSON exporter.
    #[must_use]
    pub fn new() -> Self {
        Self {
            pretty: false,
            indent_level: 0,
            first_entry: true,
        }
    }

    /// Enable pretty-printing.
    #[must_use]
    pub fn pretty(mut self, pretty: bool) -> Self {
        self.pretty = pretty;
        self
    }

    /// Write the opening of a JSON array.
    pub fn begin_array<W: Write>(&mut self, writer: &mut W) -> Result<()> {
        write!(writer, "[")?;
        if self.pretty {
            writeln!(writer)?;
        }
        self.indent_level += 1;
        self.first_entry = true;
        Ok(())
    }

    /// Write the closing of a JSON array.
    pub fn end_array<W: Write>(&mut self, writer: &mut W) -> Result<()> {
        self.indent_level = self.indent_level.saturating_sub(1);
        if self.pretty {
            writeln!(writer)?;
        }
        write!(writer, "]")?;
        Ok(())
    }

    /// Write a single entry to the JSON array.
    pub fn write_entry<W: Write>(&mut self, writer: &mut W, entry: &LogEntry) -> Result<()> {
        // Handle comma separation
        if !self.first_entry {
            write!(writer, ",")?;
            if self.pretty {
                writeln!(writer)?;
            }
        }
        self.first_entry = false;

        // Serialize entry
        let json = if self.pretty {
            let value = serde_json::to_value(entry)?;
            let mut json_str = serde_json::to_string_pretty(&value)?;
            // Indent each line
            let indent = "  ".repeat(self.indent_level);
            json_str = json_str
                .lines()
                .map(|line| format!("{}{}", indent, line))
                .collect::<Vec<_>>()
                .join("\n");
            json_str
        } else {
            serde_json::to_string(entry)?
        };

        write!(writer, "{}", json)?;
        Ok(())
    }

    /// Write a filtered entry to the JSON array.
    pub fn write_filtered_entry<W: Write>(
        &mut self,
        writer: &mut W,
        entry: &FilteredEntry,
    ) -> Result<()> {
        // Handle comma separation
        if !self.first_entry {
            write!(writer, ",")?;
            if self.pretty {
                writeln!(writer)?;
            }
        }
        self.first_entry = false;

        // Serialize entry
        let json = if self.pretty {
            let mut json_str = serde_json::to_string_pretty(entry)?;
            // Indent each line
            let indent = "  ".repeat(self.indent_level);
            json_str = json_str
                .lines()
                .map(|line| format!("{}{}", indent, line))
                .collect::<Vec<_>>()
                .join("\n");
            json_str
        } else {
            serde_json::to_string(entry)?
        };

        write!(writer, "{}", json)?;
        Ok(())
    }

    /// Stream a conversation to JSON format.
    ///
    /// This method writes entries one at a time, keeping memory usage
    /// constant regardless of conversation size.
    pub fn stream_conversation<W: Write>(
        &mut self,
        conversation: &Conversation,
        writer: &mut W,
        options: &ExportOptions,
    ) -> Result<()> {
        if conversation.provider_bundle().is_some() {
            return Err(SnatchError::export(
                "streaming JSON arrays cannot represent provider derivation metadata; use JsonExporter or JSONL",
            ));
        }
        let entries = if options.main_thread_only {
            conversation.main_thread_entries()
        } else {
            conversation.chronological_entries()
        };

        self.begin_array(writer)?;

        for entry in entries {
            // Apply entry-level filtering using exclusive filter support
            if !should_include_entry(entry, options) {
                continue;
            }

            let filtered = FilteredEntry::from_entry(entry, options);
            self.write_filtered_entry(writer, &filtered)?;
        }

        self.end_array(writer)?;
        Ok(())
    }

    /// Stream entries to JSON format.
    ///
    /// This method writes entries one at a time, keeping memory usage
    /// constant regardless of the number of entries.
    pub fn stream_entries<W: Write>(
        &mut self,
        entries: &[LogEntry],
        writer: &mut W,
        options: &ExportOptions,
    ) -> Result<()> {
        self.begin_array(writer)?;

        for entry in entries {
            // Apply entry-level filtering using exclusive filter support
            if !should_include_entry(entry, options) {
                continue;
            }

            let filtered = FilteredEntry::from_entry(entry, options);
            self.write_filtered_entry(writer, &filtered)?;
        }

        self.end_array(writer)?;
        Ok(())
    }

    /// Stream entries from an iterator (for truly lazy evaluation).
    ///
    /// This is the most memory-efficient method as it can process
    /// entries from a lazy iterator without collecting them first.
    pub fn stream_from_iter<'a, W: Write, I>(
        &mut self,
        iter: I,
        writer: &mut W,
        options: &ExportOptions,
    ) -> Result<()>
    where
        I: Iterator<Item = &'a LogEntry>,
    {
        self.begin_array(writer)?;

        for entry in iter {
            // Apply entry-level filtering using exclusive filter support
            if !should_include_entry(entry, options) {
                continue;
            }

            let filtered = FilteredEntry::from_entry(entry, options);
            self.write_filtered_entry(writer, &filtered)?;
        }

        self.end_array(writer)?;
        Ok(())
    }

    /// Reset the exporter state for reuse.
    pub fn reset(&mut self) {
        self.indent_level = 0;
        self.first_entry = true;
    }
}

/// Export a single entry to JSON line (for JSONL output).
pub fn entry_to_jsonl(entry: &LogEntry) -> Result<String> {
    Ok(serde_json::to_string(entry)?)
}

/// Export entries to JSONL format.
pub fn entries_to_jsonl<W: Write>(entries: &[LogEntry], writer: &mut W) -> Result<()> {
    for entry in entries {
        writeln!(writer, "{}", entry_to_jsonl(entry)?)?;
    }
    Ok(())
}

/// Export conversation to JSONL format.
///
/// Classic conversations remain one native-shaped normalized entry per line.
/// Provider-bundle conversations use a versioned metadata header followed by
/// versioned entry wrappers so provider identity and derivation are
/// machine-readable (acceptance invariant #7).
pub fn conversation_to_jsonl<W: Write>(
    conversation: &Conversation,
    writer: &mut W,
    main_thread_only: bool,
) -> Result<()> {
    // Use entries_for_export (like the 6 sibling exporters) so uuid-less
    // entries (compaction summaries and, after retention, file-history-snapshot
    // / last-prompt / mode / ai-title / permission-mode / queue-operation /
    // turn-end) survive the round-trip instead of being silently dropped.
    validate_provider_export_bundle(conversation)?;
    if let Some(mut metadata) =
        ProviderExportMetadata::from_conversation(conversation, main_thread_only, None)
    {
        if metadata.unidentified_entry_count != 0 {
            return Err(SnatchError::export(
                "provider-normalized JSONL contains an entry without deterministic identity",
            ));
        }
        let derivations: BTreeMap<_, _> = metadata
            .entries
            .iter()
            .cloned()
            .map(|entry| (entry.entry_id.clone(), entry))
            .collect();
        metadata.entries.clear();
        let header = serde_json::json!({
            "type": "snatch_normalized_metadata",
            "provider": metadata,
        });
        writeln!(writer, "{}", serde_json::to_string(&header)?)?;
        for (entry, id) in conversation.identified_entries_for_export(main_thread_only) {
            let derivation = id.and_then(|id| derivations.get(&id.to_string()));
            let wrapped = serde_json::json!({
                "type": "snatch_normalized_entry",
                "format_version": 1,
                "derivation": derivation,
                "entry": entry,
            });
            writeln!(writer, "{}", serde_json::to_string(&wrapped)?)?;
        }
    } else {
        let entries = conversation.entries_for_export(main_thread_only);
        for entry in entries {
            let json = serde_json::to_string(entry)?;
            writeln!(writer, "{json}")?;
        }
    }
    Ok(())
}

/// [`Exporter`] wrapper for JSONL (line-delimited JSON entries).
///
/// The library counterpart to the CLI's `jsonl` format, for consumers that
/// dispatch over the [`Exporter`] trait rather than calling
/// [`conversation_to_jsonl`] directly.
///
/// Note: `raw-jsonl` (the byte-faithful passthrough of the original Claude Code
/// source file) is intentionally *not* an `Exporter` — it streams bytes from
/// disk rather than rendering a [`Conversation`], so it has no
/// `Conversation`-based representation to expose here. It stays a CLI/file-level
/// operation by design.
#[derive(Debug, Default)]
pub struct JsonlExporter;

impl JsonlExporter {
    /// Create a new JSONL exporter.
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

impl Exporter for JsonlExporter {
    fn export_conversation<W: Write>(
        &self,
        conversation: &Conversation,
        writer: &mut W,
        options: &ExportOptions,
    ) -> Result<()> {
        conversation_to_jsonl(conversation, writer, options.main_thread_only)
    }

    fn export_entries<W: Write>(
        &self,
        entries: &[LogEntry],
        writer: &mut W,
        _options: &ExportOptions,
    ) -> Result<()> {
        for entry in entries {
            writeln!(writer, "{}", serde_json::to_string(entry)?)?;
        }
        Ok(())
    }
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
        self.only_in_first.is_empty() && self.only_in_second.is_empty() && self.different.is_empty()
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
                            diff.different
                                .push((i + 1, (*l1).to_string(), (*l2).to_string()));
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

    fn fake_provider_conversation() -> Conversation {
        use crate::provider::SourceProvider as _;
        let parsed = crate::provider::fake::FakeProvider
            .parse(&crate::provider::fake::multi_artifact_key())
            .unwrap();
        Conversation::from_parsed_session(std::sync::Arc::new(parsed)).unwrap()
    }

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
    fn provider_json_refuses_metadata_stripping_export_modes() {
        let conversation = fake_provider_conversation();
        let options = ExportOptions::default();
        let mut out = Vec::new();
        let direct = JsonExporter::new()
            .with_envelope(false)
            .export_conversation(&conversation, &mut out, &options)
            .expect_err("direct arrays would strip provider derivation")
            .to_string();
        assert!(
            direct.contains("requires the versioned envelope"),
            "{direct}"
        );

        let mut streaming = StreamingJsonExporter::new();
        let error = streaming
            .stream_conversation(&conversation, &mut out, &options)
            .expect_err("streaming arrays would strip provider derivation")
            .to_string();
        assert!(
            error.contains("cannot represent provider derivation"),
            "{error}"
        );
    }

    #[test]
    fn test_jsonl_export_preserves_all_entry_types() {
        // Issue 0003: `-f jsonl` must round-trip every entry type by count,
        // including uuid-less summaries and file-history snapshots that were
        // previously dropped.
        use crate::model::LogEntry;
        use crate::reconstruction::Conversation;

        let lines = [
            r#"{"type":"user","uuid":"1","sessionId":"s","version":"2.0","message":{"role":"user","content":"hi"},"timestamp":"2026-01-01T00:00:00Z"}"#,
            r#"{"type":"summary","summary":"a title","leafUuid":"1"}"#,
            r#"{"type":"file-history-snapshot","messageId":"m1","snapshot":{"messageId":"m1","timestamp":"2026-01-01T00:00:00Z","trackedFileBackups":{}}}"#,
            r#"{"type":"last-prompt","sessionId":"s","prompt":"hi"}"#,
        ];
        let entries: Vec<LogEntry> = lines
            .iter()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        let conv = Conversation::from_entries(entries).unwrap();

        let mut out = Vec::new();
        conversation_to_jsonl(&conv, &mut out, false).unwrap();
        let out = String::from_utf8(out).unwrap();

        let mut counts: std::collections::BTreeMap<String, usize> =
            std::collections::BTreeMap::default();
        for line in out.lines().filter(|l| !l.trim().is_empty()) {
            let v: Value = serde_json::from_str(line).unwrap();
            let ty = v.get("type").and_then(Value::as_str).unwrap().to_string();
            *counts.entry(ty).or_default() += 1;
        }

        assert_eq!(counts.get("user"), Some(&1));
        assert_eq!(
            counts.get("summary"),
            Some(&1),
            "summary must survive jsonl"
        );
        assert_eq!(
            counts.get("file-history-snapshot"),
            Some(&1),
            "file-history-snapshot must survive jsonl"
        );
        assert_eq!(counts.get("last-prompt"), Some(&1));
        // All four input entry types are present in the output.
        assert_eq!(counts.values().sum::<usize>(), 4);
    }

    #[test]
    fn test_jsonl_exporter_matches_function() {
        let lines = [
            r#"{"type":"user","uuid":"1","sessionId":"s","version":"2.0","message":{"role":"user","content":"hi"},"timestamp":"2026-01-01T00:00:00Z"}"#,
            r#"{"type":"summary","summary":"a title","leafUuid":"1"}"#,
        ];
        let entries: Vec<LogEntry> = lines
            .iter()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        let conv = Conversation::from_entries(entries.clone()).unwrap();
        let opts = ExportOptions::default();

        // The Exporter wrapper produces exactly what the function does.
        let mut via_fn = Vec::new();
        conversation_to_jsonl(&conv, &mut via_fn, opts.main_thread_only).unwrap();
        let mut via_exporter = Vec::new();
        JsonlExporter::new()
            .export_conversation(&conv, &mut via_exporter, &opts)
            .unwrap();
        assert_eq!(via_fn, via_exporter);

        // export_entries emits one JSON line per entry.
        let mut ent_out = Vec::new();
        JsonlExporter::new()
            .export_entries(&entries, &mut ent_out, &opts)
            .unwrap();
        let ent_out = String::from_utf8(ent_out).unwrap();
        assert_eq!(ent_out.lines().filter(|l| !l.is_empty()).count(), 2);
    }

    #[test]
    fn test_streaming_exporter_compact() {
        let mut exporter = StreamingJsonExporter::new();
        let mut output = Vec::new();

        exporter.begin_array(&mut output).unwrap();
        exporter.end_array(&mut output).unwrap();

        let result = String::from_utf8(output).unwrap();
        assert_eq!(result, "[]");
    }

    #[test]
    fn test_streaming_exporter_pretty() {
        let mut exporter = StreamingJsonExporter::new().pretty(true);
        let mut output = Vec::new();

        exporter.begin_array(&mut output).unwrap();
        exporter.end_array(&mut output).unwrap();

        let result = String::from_utf8(output).unwrap();
        assert!(result.contains("[\n"));
        assert!(result.contains("\n]"));
    }

    #[test]
    fn test_streaming_exporter_reset() {
        let mut exporter = StreamingJsonExporter::new();
        let mut output = Vec::new();

        // First array
        exporter.begin_array(&mut output).unwrap();
        exporter.end_array(&mut output).unwrap();

        // Reset and write second array
        exporter.reset();
        exporter.begin_array(&mut output).unwrap();
        exporter.end_array(&mut output).unwrap();

        let result = String::from_utf8(output).unwrap();
        assert_eq!(result, "[][]");
    }

    #[test]
    fn test_streaming_exporter_default() {
        let exporter = StreamingJsonExporter::default();
        assert!(!exporter.pretty);
        assert!(exporter.first_entry);
        assert_eq!(exporter.indent_level, 0);
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
