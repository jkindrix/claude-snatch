//! Export functionality for conversations and sessions.
//!
//! This module provides various export formats:
//! - Markdown: Human-readable conversation transcripts
//! - JSON: Lossless structured data export
//! - HTML: Rich formatted output
//! - Plain text: Simple formatted output with word wrapping
//! - CSV: Spreadsheet-compatible tabular data
//! - XML: Structured markup for integration
//!
//! All exporters support streaming output for large conversations
//! and configurable formatting options.
//!
//! # Example
//!
//! ```rust,no_run
//! use claude_snatch::export::{export_to_string, ExportFormat, ExportOptions};
//! use claude_snatch::parser::JsonlParser;
//! use claude_snatch::reconstruction::Conversation;
//!
//! fn export_session(jsonl_path: &str) -> claude_snatch::Result<String> {
//!     // Parse the session
//!     let mut parser = JsonlParser::new();
//!     let entries = parser.parse_file(jsonl_path)?;
//!
//!     // Build conversation and export to markdown
//!     let conversation = Conversation::from_entries(entries)?;
//!     let options = ExportOptions::default();
//!     export_to_string(&conversation, ExportFormat::Markdown, &options)
//! }
//! ```

mod csv;
mod html;
mod json;
mod markdown;
mod otel;
pub mod schema;
mod sqlite;
mod text;
mod xml;

pub use csv::*;
pub use html::*;
pub use json::*;
pub use markdown::*;
pub use otel::*;
pub use schema::{
    entry_schema, entry_schema_string, export_schema, export_schema_string,
    validate_entries, validate_export, SchemaValidator, ValidationResult,
};
pub use sqlite::*;
pub use text::*;
pub use xml::*;

use std::collections::HashSet;
use std::io::Write;
use std::path::Path;

use tracing::{debug, instrument};

use crate::error::{Result, SnatchError};
use crate::model::LogEntry;
use crate::reconstruction::Conversation;
use crate::util::AtomicFile;

/// Content types that can be filtered in exports.
///
/// Used with the `--only` flag for exclusive filtering.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ContentType {
    /// User messages/prompts (includes tool results within user entries).
    User,
    /// Only human-typed prompts (excludes tool results within user entries).
    Prompts,
    /// Assistant responses (text content).
    Assistant,
    /// Thinking/reasoning blocks.
    Thinking,
    /// Tool invocations.
    ToolUse,
    /// Tool results/outputs.
    ToolResults,
    /// System messages.
    System,
    /// Summary entries.
    Summary,
    /// Code blocks only (extracted from assistant responses).
    Code,
}

impl ContentType {
    /// Get a human-readable description.
    pub fn description(&self) -> &'static str {
        match self {
            Self::User => "user messages",
            Self::Prompts => "human-typed prompts only",
            Self::Assistant => "assistant responses",
            Self::Thinking => "thinking blocks",
            Self::ToolUse => "tool invocations",
            Self::ToolResults => "tool results",
            Self::System => "system messages",
            Self::Summary => "summary entries",
            Self::Code => "code blocks only",
        }
    }
}

/// Common export options shared across formats.
#[derive(Debug, Clone)]
pub struct ExportOptions {
    /// Include thinking blocks in output.
    pub include_thinking: bool,
    /// Include tool use details.
    pub include_tool_use: bool,
    /// Include tool results.
    pub include_tool_results: bool,
    /// Include system messages.
    pub include_system: bool,
    /// Include timestamps.
    pub include_timestamps: bool,
    /// Use relative timestamps (e.g., "2 hours ago").
    pub relative_timestamps: bool,
    /// Include usage statistics.
    pub include_usage: bool,
    /// Include metadata (UUIDs, session IDs, etc.).
    pub include_metadata: bool,
    /// Include images (base64 or references).
    pub include_images: bool,
    /// Maximum depth for nested content.
    pub max_depth: Option<usize>,
    /// Truncate long content at this length.
    pub truncate_at: Option<usize>,
    /// Include branch/sidechain content.
    pub include_branches: bool,
    /// Only export main thread.
    pub main_thread_only: bool,
    /// Configuration for sensitive data redaction.
    pub redaction: Option<crate::util::RedactionConfig>,
    /// Preview mode for redaction - highlights what would be redacted without actually redacting.
    /// When true, sensitive data is wrapped in markers like [WOULD-REDACT:type]...[/WOULD-REDACT]
    /// instead of being replaced with [REDACTED].
    pub redaction_preview: bool,
    /// Data minimization settings for privacy-conscious exports.
    pub minimization: Option<DataMinimizationConfig>,
    /// Exclusive content filter - when non-empty, only these content types are included.
    /// All other content is excluded. When empty, uses the include_* flags.
    pub only: HashSet<ContentType>,
}

/// Configuration for data minimization in shared exports.
///
/// This helps prepare exports for sharing by stripping or anonymizing
/// potentially identifying or sensitive structural information.
#[derive(Debug, Clone, Default)]
pub struct DataMinimizationConfig {
    /// Anonymize file paths (replace with generic paths).
    pub anonymize_paths: bool,
    /// Strip working directory information.
    pub strip_cwd: bool,
    /// Strip git branch/repository information.
    pub strip_git_info: bool,
    /// Anonymize session IDs (replace with sequential numbers).
    pub anonymize_session_ids: bool,
    /// Strip project names/paths.
    pub strip_project_info: bool,
    /// Strip user-identifying information from tool calls.
    pub strip_user_info: bool,
    /// Generalize timestamps (round to hour/day).
    pub generalize_timestamps: bool,
    /// Strip model names.
    pub strip_model_names: bool,
}

impl DataMinimizationConfig {
    /// Create config with no minimization.
    pub fn none() -> Self {
        Self::default()
    }

    /// Create config for maximum minimization (all options enabled).
    pub fn full() -> Self {
        Self {
            anonymize_paths: true,
            strip_cwd: true,
            strip_git_info: true,
            anonymize_session_ids: true,
            strip_project_info: true,
            strip_user_info: true,
            generalize_timestamps: true,
            strip_model_names: false, // Keep model names by default
        }
    }

    /// Create config suitable for public sharing.
    ///
    /// Anonymizes paths and IDs, strips location info, but keeps
    /// timestamps and model info for context.
    pub fn for_sharing() -> Self {
        Self {
            anonymize_paths: true,
            strip_cwd: true,
            strip_git_info: true,
            anonymize_session_ids: true,
            strip_project_info: true,
            strip_user_info: true,
            generalize_timestamps: false,
            strip_model_names: false,
        }
    }

    /// Check if any minimization is enabled.
    pub fn is_enabled(&self) -> bool {
        self.anonymize_paths
            || self.strip_cwd
            || self.strip_git_info
            || self.anonymize_session_ids
            || self.strip_project_info
            || self.strip_user_info
            || self.generalize_timestamps
            || self.strip_model_names
    }

    /// Anonymize a file path.
    pub fn anonymize_path(&self, path: &str) -> String {
        if !self.anonymize_paths {
            return path.to_string();
        }
        // Replace path with just filename and generic prefix
        let file_name = std::path::Path::new(path)
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("file");
        format!("/project/{}", file_name)
    }

    /// Anonymize a session ID.
    pub fn anonymize_session_id(&self, original: &str, counter: usize) -> String {
        if !self.anonymize_session_ids {
            return original.to_string();
        }
        format!("session-{:04}", counter)
    }

    /// Generalize a timestamp (round to nearest hour).
    pub fn generalize_timestamp(&self, ts: chrono::DateTime<chrono::Utc>) -> chrono::DateTime<chrono::Utc> {
        if !self.generalize_timestamps {
            return ts;
        }
        use chrono::Timelike;
        ts.with_minute(0).unwrap_or(ts)
            .with_second(0).unwrap_or(ts)
            .with_nanosecond(0).unwrap_or(ts)
    }
}

/// GDPR-compliant export configuration.
///
/// Provides Article 15 (Right of access) and Article 20 (Data portability)
/// compliant metadata and formatting for personal data exports.
#[derive(Debug, Clone, Default)]
pub struct GdprConfig {
    /// Include GDPR metadata envelope.
    pub include_envelope: bool,
    /// Data controller information.
    pub data_controller: Option<String>,
    /// Processing purpose description.
    pub processing_purpose: Option<String>,
    /// Legal basis for processing (e.g., "consent", "legitimate_interest").
    pub legal_basis: Option<String>,
    /// Data retention period description.
    pub retention_period: Option<String>,
    /// Data subject identifier (optional, for multi-user scenarios).
    pub data_subject_id: Option<String>,
    /// Export reason (e.g., "access_request", "portability_request").
    pub export_reason: Option<GdprExportReason>,
    /// Include data processing history.
    pub include_processing_history: bool,
    /// Include source information.
    pub include_source_info: bool,
}

/// Reason for GDPR data export.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GdprExportReason {
    /// Article 15: Right of access by the data subject.
    AccessRequest,
    /// Article 20: Right to data portability.
    PortabilityRequest,
    /// Article 17: Right to erasure (export before deletion).
    ErasurePreExport,
    /// Internal record keeping.
    InternalAudit,
    /// Backup/archival purpose.
    Archival,
}

impl GdprConfig {
    /// Create a new GDPR config for access requests (Article 15).
    pub fn for_access_request() -> Self {
        Self {
            include_envelope: true,
            export_reason: Some(GdprExportReason::AccessRequest),
            include_processing_history: true,
            include_source_info: true,
            ..Default::default()
        }
    }

    /// Create a new GDPR config for data portability (Article 20).
    pub fn for_portability() -> Self {
        Self {
            include_envelope: true,
            export_reason: Some(GdprExportReason::PortabilityRequest),
            include_processing_history: false,
            include_source_info: true,
            ..Default::default()
        }
    }

    /// Create a new GDPR config for pre-erasure export (Article 17).
    pub fn for_erasure() -> Self {
        Self {
            include_envelope: true,
            export_reason: Some(GdprExportReason::ErasurePreExport),
            include_processing_history: true,
            include_source_info: true,
            ..Default::default()
        }
    }

    /// Set the data controller information.
    pub fn with_controller(mut self, controller: impl Into<String>) -> Self {
        self.data_controller = Some(controller.into());
        self
    }

    /// Set the processing purpose.
    pub fn with_purpose(mut self, purpose: impl Into<String>) -> Self {
        self.processing_purpose = Some(purpose.into());
        self
    }

    /// Set the legal basis.
    pub fn with_legal_basis(mut self, basis: impl Into<String>) -> Self {
        self.legal_basis = Some(basis.into());
        self
    }

    /// Set the retention period.
    pub fn with_retention(mut self, retention: impl Into<String>) -> Self {
        self.retention_period = Some(retention.into());
        self
    }

    /// Set the data subject ID.
    pub fn with_subject(mut self, subject_id: impl Into<String>) -> Self {
        self.data_subject_id = Some(subject_id.into());
        self
    }
}

/// GDPR-compliant metadata envelope for exports.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct GdprEnvelope {
    /// GDPR compliance version.
    pub gdpr_version: String,
    /// Export timestamp.
    pub exported_at: String,
    /// Exporter tool information.
    pub exporter: GdprExporterInfo,
    /// Data controller details.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data_controller: Option<String>,
    /// Processing purpose.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub processing_purpose: Option<String>,
    /// Legal basis for processing.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub legal_basis: Option<String>,
    /// Data retention policy.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub retention_policy: Option<String>,
    /// Data subject identifier.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data_subject_id: Option<String>,
    /// Reason for export.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub export_reason: Option<String>,
    /// Rights information.
    pub data_subject_rights: DataSubjectRights,
    /// Data categories present in export.
    pub data_categories: Vec<String>,
    /// Processing activities.
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub processing_activities: Vec<String>,
}

/// Exporter information for GDPR envelope.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct GdprExporterInfo {
    /// Tool name.
    pub name: String,
    /// Tool version.
    pub version: String,
    /// Export format.
    pub format: String,
}

/// Data subject rights information.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct DataSubjectRights {
    /// Article 15: Right of access.
    pub access: bool,
    /// Article 16: Right to rectification.
    pub rectification: bool,
    /// Article 17: Right to erasure.
    pub erasure: bool,
    /// Article 18: Right to restriction of processing.
    pub restriction: bool,
    /// Article 20: Right to data portability.
    pub portability: bool,
    /// Article 21: Right to object.
    pub objection: bool,
}

impl Default for DataSubjectRights {
    fn default() -> Self {
        Self {
            access: true,
            rectification: true,
            erasure: true,
            restriction: true,
            portability: true,
            objection: true,
        }
    }
}

/// SPDX license information for export metadata.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct SpdxLicenseInfo {
    /// SPDX license identifier.
    pub identifier: String,
    /// Full license name.
    pub name: String,
    /// License URL.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
}

impl SpdxLicenseInfo {
    /// Create SPDX info for MIT license (claude-snatch license).
    pub fn mit() -> Self {
        Self {
            identifier: "MIT".to_string(),
            name: "MIT License".to_string(),
            url: Some("https://opensource.org/licenses/MIT".to_string()),
        }
    }

    /// Create SPDX info for Apache 2.0 license.
    pub fn apache_2_0() -> Self {
        Self {
            identifier: "Apache-2.0".to_string(),
            name: "Apache License 2.0".to_string(),
            url: Some("https://www.apache.org/licenses/LICENSE-2.0".to_string()),
        }
    }

    /// Create SPDX info from an identifier.
    pub fn from_identifier(identifier: impl Into<String>) -> Self {
        let id = identifier.into();
        let (name, url) = match id.as_str() {
            "MIT" => ("MIT License".to_string(), Some("https://opensource.org/licenses/MIT".to_string())),
            "Apache-2.0" => ("Apache License 2.0".to_string(), Some("https://www.apache.org/licenses/LICENSE-2.0".to_string())),
            "BSD-3-Clause" => ("BSD 3-Clause License".to_string(), Some("https://opensource.org/licenses/BSD-3-Clause".to_string())),
            "GPL-3.0" => ("GNU General Public License v3.0".to_string(), Some("https://www.gnu.org/licenses/gpl-3.0.html".to_string())),
            "GPL-2.0" => ("GNU General Public License v2.0".to_string(), Some("https://www.gnu.org/licenses/old-licenses/gpl-2.0.html".to_string())),
            "LGPL-3.0" => ("GNU Lesser General Public License v3.0".to_string(), Some("https://www.gnu.org/licenses/lgpl-3.0.html".to_string())),
            "MPL-2.0" => ("Mozilla Public License 2.0".to_string(), Some("https://www.mozilla.org/en-US/MPL/2.0/".to_string())),
            "ISC" => ("ISC License".to_string(), Some("https://opensource.org/licenses/ISC".to_string())),
            "Unlicense" => ("The Unlicense".to_string(), Some("https://unlicense.org/".to_string())),
            _ => (id.clone(), None),
        };
        Self { identifier: id, name, url }
    }
}

/// Export license information with SPDX identifiers.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ExportLicenseInfo {
    /// Tool license.
    pub tool_license: SpdxLicenseInfo,
    /// Content license (if applicable).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content_license: Option<SpdxLicenseInfo>,
    /// Third-party dependencies.
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub dependencies: Vec<DependencyLicense>,
}

/// Third-party dependency license information.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct DependencyLicense {
    /// Dependency name.
    pub name: String,
    /// Dependency version.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    /// SPDX license identifier.
    pub license: String,
    /// Repository URL.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub repository: Option<String>,
}

impl ExportLicenseInfo {
    /// Create default license info for claude-snatch exports.
    pub fn default_for_tool() -> Self {
        Self {
            tool_license: SpdxLicenseInfo::mit(),
            content_license: None,
            dependencies: Vec::new(),
        }
    }

    /// Add a content license.
    pub fn with_content_license(mut self, license: SpdxLicenseInfo) -> Self {
        self.content_license = Some(license);
        self
    }

    /// Add dependency license info.
    pub fn add_dependency(&mut self, name: &str, version: Option<&str>, license: &str, repo: Option<&str>) {
        self.dependencies.push(DependencyLicense {
            name: name.to_string(),
            version: version.map(String::from),
            license: license.to_string(),
            repository: repo.map(String::from),
        });
    }

    /// Get key dependencies for attribution.
    pub fn with_key_dependencies(mut self) -> Self {
        // Add key dependencies used by claude-snatch
        self.add_dependency("serde", None, "MIT OR Apache-2.0", Some("https://github.com/serde-rs/serde"));
        self.add_dependency("tokio", None, "MIT", Some("https://github.com/tokio-rs/tokio"));
        self.add_dependency("clap", None, "MIT OR Apache-2.0", Some("https://github.com/clap-rs/clap"));
        self.add_dependency("ratatui", None, "MIT", Some("https://github.com/ratatui/ratatui"));
        self.add_dependency("chrono", None, "MIT OR Apache-2.0", Some("https://github.com/chronotope/chrono"));
        self.add_dependency("tantivy", None, "MIT", Some("https://github.com/quickwit-oss/tantivy"));
        self
    }
}

impl GdprEnvelope {
    /// Create a GDPR envelope from configuration.
    pub fn from_config(config: &GdprConfig, format: &str) -> Self {
        let export_reason = config.export_reason.map(|r| match r {
            GdprExportReason::AccessRequest => "access_request_article_15".to_string(),
            GdprExportReason::PortabilityRequest => "portability_request_article_20".to_string(),
            GdprExportReason::ErasurePreExport => "pre_erasure_article_17".to_string(),
            GdprExportReason::InternalAudit => "internal_audit".to_string(),
            GdprExportReason::Archival => "archival".to_string(),
        });

        let mut data_categories = vec![
            "conversation_content".to_string(),
            "timestamps".to_string(),
            "usage_statistics".to_string(),
        ];
        if config.include_processing_history {
            data_categories.push("processing_history".to_string());
        }
        if config.include_source_info {
            data_categories.push("source_information".to_string());
        }

        let processing_activities = if config.include_processing_history {
            vec![
                "data_collection".to_string(),
                "data_storage".to_string(),
                "data_export".to_string(),
            ]
        } else {
            Vec::new()
        };

        Self {
            gdpr_version: "GDPR_2016/679".to_string(),
            exported_at: chrono::Utc::now().to_rfc3339(),
            exporter: GdprExporterInfo {
                name: crate::NAME.to_string(),
                version: crate::VERSION.to_string(),
                format: format.to_string(),
            },
            data_controller: config.data_controller.clone(),
            processing_purpose: config.processing_purpose.clone(),
            legal_basis: config.legal_basis.clone(),
            retention_policy: config.retention_period.clone(),
            data_subject_id: config.data_subject_id.clone(),
            export_reason,
            data_subject_rights: DataSubjectRights::default(),
            data_categories,
            processing_activities,
        }
    }
}

impl Default for ExportOptions {
    fn default() -> Self {
        Self {
            include_thinking: true,
            include_tool_use: true,
            include_tool_results: true,
            include_system: false,
            include_timestamps: true,
            relative_timestamps: false,
            include_usage: true,
            include_metadata: false,
            include_images: true,
            max_depth: None,
            truncate_at: None,
            include_branches: false,
            main_thread_only: true,
            redaction: None,
            redaction_preview: false,
            minimization: None,
            only: HashSet::new(),
        }
    }
}

impl ExportOptions {
    /// Create options for full export (all content).
    #[must_use]
    pub fn full() -> Self {
        Self {
            include_thinking: true,
            include_tool_use: true,
            include_tool_results: true,
            include_system: true,
            include_timestamps: true,
            relative_timestamps: false,
            include_usage: true,
            include_metadata: true,
            include_images: true,
            max_depth: None,
            truncate_at: None,
            include_branches: true,
            main_thread_only: false,
            redaction: None,
            redaction_preview: false,
            minimization: None,
            only: HashSet::new(),
        }
    }

    /// Create options for minimal export (conversation only).
    #[must_use]
    pub fn minimal() -> Self {
        Self {
            include_thinking: false,
            include_tool_use: false,
            include_tool_results: false,
            include_system: false,
            include_timestamps: false,
            relative_timestamps: false,
            include_usage: false,
            include_metadata: false,
            include_images: false,
            max_depth: None,
            truncate_at: None,
            include_branches: false,
            main_thread_only: true,
            redaction: None,
            redaction_preview: false,
            minimization: None,
            only: HashSet::new(),
        }
    }

    /// Create options suitable for public sharing.
    ///
    /// This applies both data minimization and security redaction
    /// to prepare the export for safe sharing.
    #[must_use]
    pub fn for_sharing() -> Self {
        Self {
            include_thinking: false,
            include_tool_use: true,
            include_tool_results: false,
            include_system: false,
            include_timestamps: true,
            relative_timestamps: true,
            include_usage: false,
            include_metadata: false,
            include_images: false,
            max_depth: None,
            truncate_at: None,
            include_branches: false,
            main_thread_only: true,
            redaction: Some(crate::util::RedactionConfig::security()),
            redaction_preview: false,
            minimization: Some(DataMinimizationConfig::for_sharing()),
            only: HashSet::new(),
        }
    }

    /// Builder: include thinking blocks.
    #[must_use]
    pub fn with_thinking(mut self, include: bool) -> Self {
        self.include_thinking = include;
        self
    }

    /// Builder: include tool use.
    #[must_use]
    pub fn with_tool_use(mut self, include: bool) -> Self {
        self.include_tool_use = include;
        self
    }

    /// Builder: include metadata.
    #[must_use]
    pub fn with_metadata(mut self, include: bool) -> Self {
        self.include_metadata = include;
        self
    }

    /// Builder: include branches.
    #[must_use]
    pub fn with_branches(mut self, include: bool) -> Self {
        self.include_branches = include;
        self.main_thread_only = !include;
        self
    }

    /// Builder: use relative timestamps (e.g., "2 hours ago").
    #[must_use]
    pub fn with_relative_timestamps(mut self, relative: bool) -> Self {
        self.relative_timestamps = relative;
        self
    }

    /// Builder: set redaction configuration.
    ///
    /// When set, sensitive data (API keys, passwords, emails, etc.) will be
    /// redacted from the exported content based on the configuration.
    #[must_use]
    pub fn with_redaction(mut self, config: crate::util::RedactionConfig) -> Self {
        self.redaction = Some(config);
        self
    }

    /// Builder: enable security-focused redaction.
    ///
    /// This enables redaction of API keys, passwords, credit cards, SSN,
    /// AWS keys, and URL credentials. Emails, IP addresses, and phone
    /// numbers are not redacted by default.
    #[must_use]
    pub fn with_security_redaction(mut self) -> Self {
        self.redaction = Some(crate::util::RedactionConfig::security());
        self
    }

    /// Builder: enable full redaction of all sensitive data types.
    #[must_use]
    pub fn with_full_redaction(mut self) -> Self {
        self.redaction = Some(crate::util::RedactionConfig::all());
        self
    }

    /// Builder: enable redaction preview mode.
    ///
    /// In preview mode, sensitive data is highlighted with markers like
    /// `[WOULD-REDACT:ApiKey]...[/WOULD-REDACT]` instead of being replaced.
    /// This allows reviewing what would be redacted without losing the data.
    #[must_use]
    pub fn with_redaction_preview(mut self, preview: bool) -> Self {
        self.redaction_preview = preview;
        self
    }

    /// Apply redaction to text if configured.
    ///
    /// Returns the original text if no redaction is configured.
    /// In normal mode, returns the redacted text.
    /// In preview mode, returns text with sensitive data highlighted.
    #[must_use]
    pub fn redact<'a>(&self, text: &'a str) -> std::borrow::Cow<'a, str> {
        match &self.redaction {
            Some(config) if config.is_enabled() => {
                if self.redaction_preview {
                    crate::util::preview_redactions(text, config)
                } else {
                    crate::util::redact_sensitive(text, config)
                }
            }
            _ => std::borrow::Cow::Borrowed(text),
        }
    }

    /// Builder: set data minimization configuration.
    #[must_use]
    pub fn with_minimization(mut self, config: DataMinimizationConfig) -> Self {
        self.minimization = Some(config);
        self
    }

    /// Builder: enable minimization for public sharing.
    #[must_use]
    pub fn with_sharing_minimization(mut self) -> Self {
        self.minimization = Some(DataMinimizationConfig::for_sharing());
        self
    }

    /// Builder: enable full minimization.
    #[must_use]
    pub fn with_full_minimization(mut self) -> Self {
        self.minimization = Some(DataMinimizationConfig::full());
        self
    }

    /// Check if minimization is enabled.
    #[must_use]
    pub fn has_minimization(&self) -> bool {
        self.minimization.as_ref().is_some_and(|m| m.is_enabled())
    }

    /// Apply minimization to a file path if configured.
    #[must_use]
    pub fn minimize_path(&self, path: &str) -> String {
        match &self.minimization {
            Some(config) if config.anonymize_paths => config.anonymize_path(path),
            _ => path.to_string(),
        }
    }

    /// Apply minimization to a session ID if configured.
    #[must_use]
    pub fn minimize_session_id(&self, session_id: &str, counter: usize) -> String {
        match &self.minimization {
            Some(config) if config.anonymize_session_ids => config.anonymize_session_id(session_id, counter),
            _ => session_id.to_string(),
        }
    }

    /// Check if git info should be stripped.
    #[must_use]
    pub fn should_strip_git_info(&self) -> bool {
        self.minimization.as_ref().is_some_and(|m| m.strip_git_info)
    }

    /// Check if project info should be stripped.
    #[must_use]
    pub fn should_strip_project_info(&self) -> bool {
        self.minimization.as_ref().is_some_and(|m| m.strip_project_info)
    }

    /// Check if CWD should be stripped.
    #[must_use]
    pub fn should_strip_cwd(&self) -> bool {
        self.minimization.as_ref().is_some_and(|m| m.strip_cwd)
    }

    /// Check if model names should be stripped.
    #[must_use]
    pub fn should_strip_model(&self) -> bool {
        self.minimization.as_ref().is_some_and(|m| m.strip_model_names)
    }

    /// Check if exclusive filtering is active.
    #[must_use]
    pub fn has_exclusive_filter(&self) -> bool {
        !self.only.is_empty()
    }

    /// Builder: set exclusive content filter.
    #[must_use]
    pub fn with_only(mut self, types: HashSet<ContentType>) -> Self {
        self.only = types;
        self
    }

    /// Check if a specific content type should be included.
    ///
    /// When `only` is non-empty, only content types in that set are included.
    /// When `only` is empty, the include_* flags are used.
    #[must_use]
    pub fn should_include(&self, content_type: ContentType) -> bool {
        if self.has_exclusive_filter() {
            self.only.contains(&content_type)
        } else {
            match content_type {
                ContentType::User => true, // Always include user messages by default
                ContentType::Prompts => true, // Prompts are part of user messages
                ContentType::Assistant => true, // Always include assistant messages by default
                ContentType::Thinking => self.include_thinking,
                ContentType::ToolUse => self.include_tool_use,
                ContentType::ToolResults => self.include_tool_results,
                ContentType::System => self.include_system,
                ContentType::Summary => true, // Include summaries by default
                ContentType::Code => false, // Code extraction only when explicitly requested
            }
        }
    }

    /// Check if user messages should be included.
    #[must_use]
    pub fn should_include_user(&self) -> bool {
        if self.has_exclusive_filter() {
            // Include user entries if User or Prompts is in the filter
            self.only.contains(&ContentType::User) || self.only.contains(&ContentType::Prompts)
        } else {
            true
        }
    }

    /// Check if assistant messages should be included.
    #[must_use]
    pub fn should_include_assistant(&self) -> bool {
        if self.has_exclusive_filter() {
            // Include assistant entries if Assistant is in the filter,
            // OR if any assistant content types (Thinking, ToolUse, ToolResults, Code) are in the filter
            self.only.contains(&ContentType::Assistant)
                || self.only.contains(&ContentType::Thinking)
                || self.only.contains(&ContentType::ToolUse)
                || self.only.contains(&ContentType::ToolResults)
                || self.only.contains(&ContentType::Code)
        } else {
            true
        }
    }

    /// Check if thinking blocks should be included.
    #[must_use]
    pub fn should_include_thinking(&self) -> bool {
        self.should_include(ContentType::Thinking)
    }

    /// Check if tool use should be included.
    #[must_use]
    pub fn should_include_tool_use(&self) -> bool {
        self.should_include(ContentType::ToolUse)
    }

    /// Check if tool results should be included.
    ///
    /// When `--only prompts` is used, tool results within user messages are excluded
    /// unless `--only tool-results` is also specified.
    #[must_use]
    pub fn should_include_tool_results(&self) -> bool {
        if self.has_exclusive_filter() {
            // If Prompts is in the filter without ToolResults, exclude tool results
            if self.only.contains(&ContentType::Prompts) && !self.only.contains(&ContentType::ToolResults) {
                return false;
            }
            self.only.contains(&ContentType::ToolResults)
        } else {
            self.include_tool_results
        }
    }

    /// Check if system messages should be included.
    #[must_use]
    pub fn should_include_system(&self) -> bool {
        self.should_include(ContentType::System)
    }

    /// Check if summary entries should be included.
    #[must_use]
    pub fn should_include_summary(&self) -> bool {
        self.should_include(ContentType::Summary)
    }

    /// Check if only prompts mode is active (excludes tool results from user messages).
    #[must_use]
    pub fn is_prompts_only(&self) -> bool {
        self.has_exclusive_filter()
            && self.only.contains(&ContentType::Prompts)
            && !self.only.contains(&ContentType::ToolResults)
    }

    /// Check if code-only mode is active (extract only code blocks).
    #[must_use]
    pub fn is_code_only(&self) -> bool {
        self.has_exclusive_filter() && self.only.contains(&ContentType::Code)
    }
}

/// Export format specification.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExportFormat {
    /// Markdown format.
    Markdown,
    /// JSON format (lossless).
    Json,
    /// Pretty-printed JSON.
    JsonPretty,
    /// Plain text with word wrapping.
    Text,
    /// HTML formatted output.
    Html,
    /// CSV tabular data.
    Csv,
    /// XML structured markup.
    Xml,
    /// SQLite database.
    Sqlite,
    /// OpenTelemetry (OTLP JSON format).
    Otel,
}

impl ExportFormat {
    /// Get the file extension for this format.
    #[must_use]
    pub const fn extension(&self) -> &'static str {
        match self {
            Self::Markdown => "md",
            Self::Json | Self::JsonPretty => "json",
            Self::Text => "txt",
            Self::Html => "html",
            Self::Csv => "csv",
            Self::Xml => "xml",
            Self::Sqlite => "db",
            Self::Otel => "otlp.json",
        }
    }

    /// Parse format from string.
    #[must_use]
    pub fn from_str(s: &str) -> Option<Self> {
        match s.to_lowercase().as_str() {
            "markdown" | "md" => Some(Self::Markdown),
            "json" => Some(Self::Json),
            "json-pretty" | "jsonpretty" => Some(Self::JsonPretty),
            "text" | "txt" => Some(Self::Text),
            "html" => Some(Self::Html),
            "csv" => Some(Self::Csv),
            "xml" => Some(Self::Xml),
            "sqlite" | "db" | "sql" => Some(Self::Sqlite),
            "otel" | "otlp" | "opentelemetry" => Some(Self::Otel),
            _ => None,
        }
    }

    /// Check if this format requires a file (cannot write to stdout).
    #[must_use]
    pub const fn requires_file(&self) -> bool {
        matches!(self, Self::Sqlite)
    }
}

/// Trait for exporters.
pub trait Exporter {
    /// Export a conversation to the writer.
    fn export_conversation<W: Write>(
        &self,
        conversation: &Conversation,
        writer: &mut W,
        options: &ExportOptions,
    ) -> Result<()>;

    /// Export raw entries to the writer.
    fn export_entries<W: Write>(
        &self,
        entries: &[LogEntry],
        writer: &mut W,
        options: &ExportOptions,
    ) -> Result<()>;
}

/// Export a conversation to a file.
///
/// This function uses atomic file writes to ensure data integrity.
/// Content is written to a temporary file first, then atomically
/// renamed to the target path.
#[instrument(skip(conversation, options), fields(path = %path.as_ref().display(), format = ?format))]
pub fn export_to_file(
    conversation: &Conversation,
    path: impl AsRef<Path>,
    format: ExportFormat,
    options: &ExportOptions,
) -> Result<()> {
    let path = path.as_ref();
    debug!(nodes = conversation.len(), "Exporting conversation to file");

    // SQLite handles its own file creation
    if matches!(format, ExportFormat::Sqlite) {
        let exporter = SqliteExporter::new();
        return exporter.export_to_file(conversation, path, options);
    }

    // Use atomic file writing for all other formats
    let mut atomic = AtomicFile::create(path)?;
    let mut writer = std::io::BufWriter::new(atomic.writer());

    match format {
        ExportFormat::Markdown => {
            let exporter = MarkdownExporter::new();
            exporter.export_conversation(conversation, &mut writer, options)?;
        }
        ExportFormat::Json => {
            let exporter = JsonExporter::new();
            exporter.export_conversation(conversation, &mut writer, options)?;
        }
        ExportFormat::JsonPretty => {
            let exporter = JsonExporter::new().pretty(true);
            exporter.export_conversation(conversation, &mut writer, options)?;
        }
        ExportFormat::Text => {
            let exporter = TextExporter::new();
            exporter.export_conversation(conversation, &mut writer, options)?;
        }
        ExportFormat::Html => {
            let exporter = HtmlExporter::new();
            exporter.export_conversation(conversation, &mut writer, options)?;
        }
        ExportFormat::Csv => {
            let exporter = CsvExporter::new();
            exporter.export_conversation(conversation, &mut writer, options)?;
        }
        ExportFormat::Xml => {
            let exporter = XmlExporter::new();
            exporter.export_conversation(conversation, &mut writer, options)?;
        }
        ExportFormat::Otel => {
            let exporter = OtelExporter::new();
            exporter.export_conversation(conversation, &mut writer, options)?;
        }
        ExportFormat::Sqlite => {
            unreachable!("SQLite handled above");
        }
    }

    // Flush the BufWriter before finishing atomic write
    writer.flush().map_err(|e| {
        SnatchError::io(format!("Failed to flush output file: {}", path.display()), e)
    })?;

    // Drop the BufWriter to release the borrow on atomic.writer()
    drop(writer);

    // Complete the atomic write
    atomic.finish()?;

    Ok(())
}

/// Export a conversation to a string.
#[instrument(skip(conversation, options), fields(format = ?format))]
pub fn export_to_string(
    conversation: &Conversation,
    format: ExportFormat,
    options: &ExportOptions,
) -> Result<String> {
    debug!(nodes = conversation.len(), "Exporting conversation to string");
    let mut buffer = Vec::new();

    match format {
        ExportFormat::Markdown => {
            let exporter = MarkdownExporter::new();
            exporter.export_conversation(conversation, &mut buffer, options)?;
        }
        ExportFormat::Json => {
            let exporter = JsonExporter::new();
            exporter.export_conversation(conversation, &mut buffer, options)?;
        }
        ExportFormat::JsonPretty => {
            let exporter = JsonExporter::new().pretty(true);
            exporter.export_conversation(conversation, &mut buffer, options)?;
        }
        ExportFormat::Text => {
            let exporter = TextExporter::new();
            exporter.export_conversation(conversation, &mut buffer, options)?;
        }
        ExportFormat::Html => {
            let exporter = HtmlExporter::new();
            exporter.export_conversation(conversation, &mut buffer, options)?;
        }
        ExportFormat::Csv => {
            let exporter = CsvExporter::new();
            exporter.export_conversation(conversation, &mut buffer, options)?;
        }
        ExportFormat::Xml => {
            let exporter = XmlExporter::new();
            exporter.export_conversation(conversation, &mut buffer, options)?;
        }
        ExportFormat::Otel => {
            let exporter = OtelExporter::new();
            exporter.export_conversation(conversation, &mut buffer, options)?;
        }
        ExportFormat::Sqlite => {
            return Err(SnatchError::export(
                "SQLite export requires a file path, not a string buffer",
            ));
        }
    }

    String::from_utf8(buffer).map_err(SnatchError::from)
}

use chrono::{DateTime, Utc};

/// Format a timestamp, optionally as relative time.
///
/// If `relative` is true, returns human-readable relative time like "2 hours ago".
/// Otherwise returns ISO-8601 formatted timestamp.
pub fn format_timestamp(ts: &DateTime<Utc>, relative: bool) -> String {
    if relative {
        format_relative_time(ts)
    } else {
        ts.format("%Y-%m-%d %H:%M:%S UTC").to_string()
    }
}

/// Format a timestamp as relative time (e.g., "2 hours ago", "yesterday").
pub fn format_relative_time(ts: &DateTime<Utc>) -> String {
    let now = Utc::now();
    let duration = now.signed_duration_since(*ts);

    if duration.num_seconds() < 0 {
        // Future time
        let abs = -duration.num_seconds();
        if abs < 60 {
            return "in a moment".to_string();
        } else if abs < 3600 {
            let mins = abs / 60;
            return format!("in {} minute{}", mins, if mins == 1 { "" } else { "s" });
        } else if abs < 86400 {
            let hours = abs / 3600;
            return format!("in {} hour{}", hours, if hours == 1 { "" } else { "s" });
        } else {
            let days = abs / 86400;
            return format!("in {} day{}", days, if days == 1 { "" } else { "s" });
        }
    }

    let secs = duration.num_seconds();

    if secs < 60 {
        "just now".to_string()
    } else if secs < 3600 {
        let mins = secs / 60;
        format!("{} minute{} ago", mins, if mins == 1 { "" } else { "s" })
    } else if secs < 86400 {
        let hours = secs / 3600;
        format!("{} hour{} ago", hours, if hours == 1 { "" } else { "s" })
    } else if secs < 172800 {
        "yesterday".to_string()
    } else if secs < 604800 {
        let days = secs / 86400;
        format!("{} days ago", days)
    } else if secs < 2592000 {
        let weeks = secs / 604800;
        format!("{} week{} ago", weeks, if weeks == 1 { "" } else { "s" })
    } else if secs < 31536000 {
        let months = secs / 2592000;
        format!("{} month{} ago", months, if months == 1 { "" } else { "s" })
    } else {
        let years = secs / 31536000;
        format!("{} year{} ago", years, if years == 1 { "" } else { "s" })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_export_format_extension() {
        assert_eq!(ExportFormat::Markdown.extension(), "md");
        assert_eq!(ExportFormat::Json.extension(), "json");
        assert_eq!(ExportFormat::Text.extension(), "txt");
    }

    #[test]
    fn test_export_format_from_str() {
        assert_eq!(ExportFormat::from_str("markdown"), Some(ExportFormat::Markdown));
        assert_eq!(ExportFormat::from_str("MD"), Some(ExportFormat::Markdown));
        assert_eq!(ExportFormat::from_str("json"), Some(ExportFormat::Json));
        assert_eq!(ExportFormat::from_str("invalid"), None);
    }

    #[test]
    fn test_export_options_builders() {
        let opts = ExportOptions::default()
            .with_thinking(false)
            .with_metadata(true);

        assert!(!opts.include_thinking);
        assert!(opts.include_metadata);
    }

    #[test]
    fn test_relative_timestamps_builder() {
        let opts = ExportOptions::default().with_relative_timestamps(true);
        assert!(opts.relative_timestamps);
    }

    #[test]
    fn test_format_timestamp_absolute() {
        use chrono::TimeZone;
        let ts = Utc.with_ymd_and_hms(2025, 12, 24, 10, 30, 0).unwrap();
        let result = format_timestamp(&ts, false);
        assert_eq!(result, "2025-12-24 10:30:00 UTC");
    }

    #[test]
    fn test_format_relative_time_just_now() {
        let ts = Utc::now();
        let result = format_relative_time(&ts);
        assert_eq!(result, "just now");
    }

    #[test]
    fn test_format_relative_time_minutes() {
        use chrono::Duration;
        let ts = Utc::now() - Duration::minutes(5);
        let result = format_relative_time(&ts);
        assert_eq!(result, "5 minutes ago");
    }

    #[test]
    fn test_format_relative_time_hours() {
        use chrono::Duration;
        let ts = Utc::now() - Duration::hours(3);
        let result = format_relative_time(&ts);
        assert_eq!(result, "3 hours ago");
    }

    #[test]
    fn test_format_relative_time_yesterday() {
        use chrono::Duration;
        let ts = Utc::now() - Duration::hours(30);
        let result = format_relative_time(&ts);
        assert_eq!(result, "yesterday");
    }

    #[test]
    fn test_format_relative_time_days() {
        use chrono::Duration;
        let ts = Utc::now() - Duration::days(4);
        let result = format_relative_time(&ts);
        assert_eq!(result, "4 days ago");
    }

    #[test]
    fn test_format_relative_time_weeks() {
        use chrono::Duration;
        let ts = Utc::now() - Duration::weeks(2);
        let result = format_relative_time(&ts);
        assert_eq!(result, "2 weeks ago");
    }

    #[test]
    fn test_data_minimization_config_none() {
        let config = DataMinimizationConfig::none();
        assert!(!config.is_enabled());
    }

    #[test]
    fn test_data_minimization_config_full() {
        let config = DataMinimizationConfig::full();
        assert!(config.is_enabled());
        assert!(config.anonymize_paths);
        assert!(config.strip_cwd);
        assert!(config.strip_git_info);
        assert!(config.anonymize_session_ids);
    }

    #[test]
    fn test_data_minimization_config_for_sharing() {
        let config = DataMinimizationConfig::for_sharing();
        assert!(config.is_enabled());
        assert!(config.anonymize_paths);
        assert!(!config.generalize_timestamps); // Keeps timestamps for context
        assert!(!config.strip_model_names); // Keeps model names
    }

    #[test]
    fn test_data_minimization_anonymize_path() {
        let config = DataMinimizationConfig::full();
        let result = config.anonymize_path("/home/user/projects/secret-project/src/main.rs");
        assert_eq!(result, "/project/main.rs");
        assert!(!result.contains("user"));
        assert!(!result.contains("secret-project"));
    }

    #[test]
    fn test_data_minimization_anonymize_session_id() {
        let config = DataMinimizationConfig::full();
        let result = config.anonymize_session_id("01930a2e-3f4b-7c8d-9e0f-123456789abc", 5);
        assert_eq!(result, "session-0005");
        assert!(!result.contains("01930a2e"));
    }

    #[test]
    fn test_data_minimization_generalize_timestamp() {
        use chrono::{TimeZone, Timelike};
        let config = DataMinimizationConfig::full();
        let ts = Utc.with_ymd_and_hms(2025, 12, 24, 10, 45, 32).unwrap();
        let result = config.generalize_timestamp(ts);
        assert_eq!(result.minute(), 0);
        assert_eq!(result.second(), 0);
        assert_eq!(result.hour(), 10); // Hour preserved
    }

    #[test]
    fn test_export_options_for_sharing() {
        let opts = ExportOptions::for_sharing();
        assert!(opts.redaction.is_some());
        assert!(opts.minimization.is_some());
        assert!(!opts.include_thinking);
        assert!(opts.relative_timestamps);
    }

    #[test]
    fn test_export_options_minimize_path() {
        let opts = ExportOptions::default().with_sharing_minimization();
        let result = opts.minimize_path("/home/user/code/app.py");
        assert_eq!(result, "/project/app.py");
    }

    #[test]
    fn test_export_options_minimize_session_id() {
        let opts = ExportOptions::default().with_sharing_minimization();
        let result = opts.minimize_session_id("abc123-def456", 7);
        assert_eq!(result, "session-0007");
    }

    #[test]
    fn test_export_options_no_minimization() {
        let opts = ExportOptions::default();
        assert!(!opts.has_minimization());
        // Without minimization, paths are unchanged
        let result = opts.minimize_path("/home/user/code/app.py");
        assert_eq!(result, "/home/user/code/app.py");
    }

    #[test]
    fn test_gdpr_config_default() {
        let config = GdprConfig::default();
        assert!(!config.include_envelope);
        assert!(config.data_controller.is_none());
        assert!(config.export_reason.is_none());
    }

    #[test]
    fn test_gdpr_config_access_request() {
        let config = GdprConfig::for_access_request();
        assert!(config.include_envelope);
        assert_eq!(config.export_reason, Some(GdprExportReason::AccessRequest));
        assert!(config.include_processing_history);
        assert!(config.include_source_info);
    }

    #[test]
    fn test_gdpr_config_portability() {
        let config = GdprConfig::for_portability();
        assert!(config.include_envelope);
        assert_eq!(config.export_reason, Some(GdprExportReason::PortabilityRequest));
        assert!(!config.include_processing_history);
    }

    #[test]
    fn test_gdpr_config_erasure() {
        let config = GdprConfig::for_erasure();
        assert!(config.include_envelope);
        assert_eq!(config.export_reason, Some(GdprExportReason::ErasurePreExport));
        assert!(config.include_processing_history);
    }

    #[test]
    fn test_gdpr_config_builders() {
        let config = GdprConfig::for_access_request()
            .with_controller("ACME Corp")
            .with_purpose("AI conversation assistance")
            .with_legal_basis("consent")
            .with_retention("30 days")
            .with_subject("user@example.com");

        assert_eq!(config.data_controller, Some("ACME Corp".to_string()));
        assert_eq!(config.processing_purpose, Some("AI conversation assistance".to_string()));
        assert_eq!(config.legal_basis, Some("consent".to_string()));
        assert_eq!(config.retention_period, Some("30 days".to_string()));
        assert_eq!(config.data_subject_id, Some("user@example.com".to_string()));
    }

    #[test]
    fn test_gdpr_envelope_from_config() {
        let config = GdprConfig::for_access_request()
            .with_controller("Test Corp")
            .with_purpose("Testing");

        let envelope = GdprEnvelope::from_config(&config, "json");

        assert_eq!(envelope.gdpr_version, "GDPR_2016/679");
        assert_eq!(envelope.data_controller, Some("Test Corp".to_string()));
        assert_eq!(envelope.processing_purpose, Some("Testing".to_string()));
        assert_eq!(envelope.export_reason, Some("access_request_article_15".to_string()));
        assert!(envelope.data_categories.contains(&"conversation_content".to_string()));
        assert!(envelope.data_categories.contains(&"processing_history".to_string()));
    }

    #[test]
    fn test_data_subject_rights_default() {
        let rights = DataSubjectRights::default();
        assert!(rights.access);
        assert!(rights.rectification);
        assert!(rights.erasure);
        assert!(rights.restriction);
        assert!(rights.portability);
        assert!(rights.objection);
    }

    #[test]
    fn test_gdpr_export_reasons() {
        let config = GdprConfig::for_portability();
        let envelope = GdprEnvelope::from_config(&config, "json");
        assert_eq!(envelope.export_reason, Some("portability_request_article_20".to_string()));

        let config = GdprConfig::for_erasure();
        let envelope = GdprEnvelope::from_config(&config, "json");
        assert_eq!(envelope.export_reason, Some("pre_erasure_article_17".to_string()));
    }

    #[test]
    fn test_gdpr_envelope_processing_activities() {
        // With processing history
        let config = GdprConfig::for_access_request();
        let envelope = GdprEnvelope::from_config(&config, "json");
        assert!(!envelope.processing_activities.is_empty());

        // Without processing history
        let config = GdprConfig::for_portability();
        let envelope = GdprEnvelope::from_config(&config, "json");
        assert!(envelope.processing_activities.is_empty());
    }

    #[test]
    fn test_spdx_license_mit() {
        let license = SpdxLicenseInfo::mit();
        assert_eq!(license.identifier, "MIT");
        assert_eq!(license.name, "MIT License");
        assert!(license.url.is_some());
    }

    #[test]
    fn test_spdx_license_apache() {
        let license = SpdxLicenseInfo::apache_2_0();
        assert_eq!(license.identifier, "Apache-2.0");
        assert_eq!(license.name, "Apache License 2.0");
    }

    #[test]
    fn test_spdx_license_from_identifier() {
        let license = SpdxLicenseInfo::from_identifier("GPL-3.0");
        assert_eq!(license.identifier, "GPL-3.0");
        assert!(license.name.contains("GNU"));
        assert!(license.url.is_some());

        // Unknown license
        let unknown = SpdxLicenseInfo::from_identifier("CUSTOM-1.0");
        assert_eq!(unknown.identifier, "CUSTOM-1.0");
        assert!(unknown.url.is_none());
    }

    #[test]
    fn test_export_license_info_default() {
        let info = ExportLicenseInfo::default_for_tool();
        assert_eq!(info.tool_license.identifier, "MIT");
        assert!(info.content_license.is_none());
        assert!(info.dependencies.is_empty());
    }

    #[test]
    fn test_export_license_info_with_dependencies() {
        let info = ExportLicenseInfo::default_for_tool().with_key_dependencies();
        assert!(!info.dependencies.is_empty());
        assert!(info.dependencies.iter().any(|d| d.name == "serde"));
        assert!(info.dependencies.iter().any(|d| d.name == "tokio"));
    }

    #[test]
    fn test_export_license_info_add_dependency() {
        let mut info = ExportLicenseInfo::default_for_tool();
        info.add_dependency("test-crate", Some("1.0.0"), "MIT", Some("https://example.com"));

        assert_eq!(info.dependencies.len(), 1);
        assert_eq!(info.dependencies[0].name, "test-crate");
        assert_eq!(info.dependencies[0].version, Some("1.0.0".to_string()));
        assert_eq!(info.dependencies[0].license, "MIT");
    }
}
