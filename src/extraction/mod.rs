//! Beyond-JSONL data extraction module.
//!
//! This module provides extraction capabilities for supplementary data sources
//! beyond the core JSONL session logs (BJ-001 through BJ-021):
//!
//! - `settings.json` - Global and project-level configuration
//! - `CLAUDE.md` - Custom instructions (global and project-level)
//! - `mcp.json` - MCP server configurations
//! - `commands/` - Custom slash commands
//! - `rules/` - Conversation rules
//! - `output-styles/` - Output formatting styles
//! - `filehistory/` - File backup contents
//! - `credentials.json` - API key presence detection

mod backup;
mod commands;
mod mcp;
mod rules;
mod settings;

pub use backup::*;
pub use commands::*;
pub use mcp::*;
pub use rules::*;
pub use settings::*;

use crate::discovery::ClaudeDirectory;
use crate::error::Result;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Complete extraction of all Beyond-JSONL data sources.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct BeyondJsonlData {
    /// Global settings from ~/.claude/settings.json (BJ-002)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub global_settings: Option<ClaudeSettings>,

    /// Project-specific settings (BJ-003)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project_settings: Option<ClaudeSettings>,

    /// Global CLAUDE.md instructions (BJ-004)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub global_claude_md: Option<String>,

    /// Project-specific CLAUDE.md (BJ-005)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project_claude_md: Option<String>,

    /// MCP server configurations (BJ-006)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mcp_config: Option<McpConfig>,

    /// Global custom commands (BJ-007)
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub global_commands: Vec<CustomCommand>,

    /// Project-specific commands (BJ-008)
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub project_commands: Vec<CustomCommand>,

    /// API key presence indicators (BJ-009)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub credentials_present: Option<CredentialsPresence>,

    /// Hook configurations from settings (BJ-011)
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub hooks: Vec<HookConfig>,

    /// Session retention configuration (BJ-014)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_retention: Option<SessionRetention>,

    /// Sandbox configuration (BJ-015)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sandbox_config: Option<SandboxConfig>,

    /// Global rules (BJ-017)
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub global_rules: Vec<Rule>,

    /// Project-specific rules (BJ-018)
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub project_rules: Vec<Rule>,

    /// Output styles (BJ-021)
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub output_styles: Vec<OutputStyle>,

    /// File backups summary (BJ-001)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file_history_summary: Option<FileHistorySummary>,
}

impl BeyondJsonlData {
    /// Extract all Beyond-JSONL data for the global Claude directory.
    pub fn extract_global(claude_dir: &ClaudeDirectory) -> Result<Self> {
        let mut data = Self::default();

        // BJ-002: Global settings
        data.global_settings = ClaudeSettings::load(&claude_dir.settings_path()).ok();

        // BJ-004: Global CLAUDE.md
        data.global_claude_md = read_claude_md(&claude_dir.claude_md_path());

        // BJ-006: MCP config
        data.mcp_config = McpConfig::load(&claude_dir.mcp_config_path()).ok();

        // BJ-007: Global custom commands
        data.global_commands = CustomCommand::load_from_dir(&claude_dir.commands_dir())
            .unwrap_or_default();

        // BJ-009: Credentials presence
        data.credentials_present = CredentialsPresence::detect(claude_dir.root());

        // BJ-011, BJ-014, BJ-015: Extract from settings
        if let Some(settings) = &data.global_settings {
            data.hooks = settings.hooks.clone();
            data.session_retention = settings.session_retention.clone();
            data.sandbox_config = settings.sandbox.clone();
        }

        // BJ-017: Global rules
        data.global_rules = Rule::load_from_dir(&claude_dir.rules_dir())
            .unwrap_or_default();

        // BJ-021: Output styles
        data.output_styles = OutputStyle::load_from_dir(&claude_dir.output_styles_dir())
            .unwrap_or_default();

        // BJ-001: File history summary
        data.file_history_summary = FileHistorySummary::from_dir(&claude_dir.file_history_dir()).ok();

        Ok(data)
    }

    /// Extract Beyond-JSONL data for a specific project.
    pub fn extract_for_project(claude_dir: &ClaudeDirectory, project_path: &Path) -> Result<Self> {
        let mut data = Self::extract_global(claude_dir)?;

        // BJ-003: Project settings
        let project_settings_path = project_path.join(".claude").join("settings.json");
        data.project_settings = ClaudeSettings::load(&project_settings_path).ok();

        // BJ-005: Project CLAUDE.md
        let project_claude_md_path = project_path.join(".claude").join("CLAUDE.md");
        data.project_claude_md = read_claude_md(&project_claude_md_path);

        // Also check for CLAUDE.md at project root
        if data.project_claude_md.is_none() {
            let root_claude_md = project_path.join("CLAUDE.md");
            data.project_claude_md = read_claude_md(&root_claude_md);
        }

        // BJ-008: Project commands
        let project_commands_dir = project_path.join(".claude").join("commands");
        data.project_commands = CustomCommand::load_from_dir(&project_commands_dir)
            .unwrap_or_default();

        // BJ-018: Project rules
        let project_rules_dir = project_path.join(".claude").join("rules");
        data.project_rules = Rule::load_from_dir(&project_rules_dir)
            .unwrap_or_default();

        Ok(data)
    }

    /// Check if any Beyond-JSONL data was found.
    #[must_use]
    pub fn has_data(&self) -> bool {
        self.global_settings.is_some()
            || self.project_settings.is_some()
            || self.global_claude_md.is_some()
            || self.project_claude_md.is_some()
            || self.mcp_config.is_some()
            || !self.global_commands.is_empty()
            || !self.project_commands.is_empty()
            || self.credentials_present.is_some()
            || !self.hooks.is_empty()
            || self.session_retention.is_some()
            || self.sandbox_config.is_some()
            || !self.global_rules.is_empty()
            || !self.project_rules.is_empty()
            || !self.output_styles.is_empty()
            || self.file_history_summary.is_some()
    }

    /// Get count of extracted data sources.
    #[must_use]
    pub fn data_source_count(&self) -> usize {
        let mut count = 0;
        if self.global_settings.is_some() { count += 1; }
        if self.project_settings.is_some() { count += 1; }
        if self.global_claude_md.is_some() { count += 1; }
        if self.project_claude_md.is_some() { count += 1; }
        if self.mcp_config.is_some() { count += 1; }
        if !self.global_commands.is_empty() { count += 1; }
        if !self.project_commands.is_empty() { count += 1; }
        if self.credentials_present.is_some() { count += 1; }
        if !self.hooks.is_empty() { count += 1; }
        if self.session_retention.is_some() { count += 1; }
        if self.sandbox_config.is_some() { count += 1; }
        if !self.global_rules.is_empty() { count += 1; }
        if !self.project_rules.is_empty() { count += 1; }
        if !self.output_styles.is_empty() { count += 1; }
        if self.file_history_summary.is_some() { count += 1; }
        count
    }
}

/// API key presence detection (BJ-009).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CredentialsPresence {
    /// Whether any credentials file exists.
    pub file_exists: bool,
    /// Whether Anthropic API key is configured.
    pub anthropic_key_present: bool,
    /// Path to credentials file (if found).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub credentials_path: Option<PathBuf>,
}

impl CredentialsPresence {
    /// Detect API key presence without reading actual keys.
    pub fn detect(claude_root: &Path) -> Option<Self> {
        let credentials_path = claude_root.join("credentials.json");

        if !credentials_path.exists() {
            return Some(Self {
                file_exists: false,
                anthropic_key_present: false,
                credentials_path: None,
            });
        }

        // Read the file to check structure, but never expose actual key values
        let content = std::fs::read_to_string(&credentials_path).ok()?;
        let json: serde_json::Value = serde_json::from_str(&content).ok()?;

        let anthropic_key_present = json.get("anthropicApiKey")
            .and_then(|v| v.as_str())
            .map(|s| !s.is_empty())
            .unwrap_or(false);

        Some(Self {
            file_exists: true,
            anthropic_key_present,
            credentials_path: Some(credentials_path),
        })
    }
}

/// Session retention configuration (BJ-014).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionRetention {
    /// Maximum number of sessions to keep.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_sessions: Option<u32>,
    /// Maximum age in days.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_age_days: Option<u32>,
    /// Whether to auto-cleanup on startup.
    #[serde(default)]
    pub auto_cleanup: bool,
}

/// Sandbox configuration (BJ-015).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SandboxConfig {
    /// Whether sandbox mode is enabled.
    #[serde(default)]
    pub enabled: bool,
    /// Allowed directories.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub allowed_directories: Vec<PathBuf>,
    /// Blocked commands.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub blocked_commands: Vec<String>,
}

/// Output style definition (BJ-021).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OutputStyle {
    /// Style name (from filename).
    pub name: String,
    /// Style content/template.
    pub content: String,
    /// Source file path.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_path: Option<PathBuf>,
}

impl OutputStyle {
    /// Load all output styles from a directory.
    pub fn load_from_dir(dir: &Path) -> Result<Vec<Self>> {
        if !dir.exists() {
            return Ok(Vec::new());
        }

        let mut styles = Vec::new();
        for entry in std::fs::read_dir(dir)? {
            let entry = entry?;
            let path = entry.path();

            if path.is_file() && path.extension().map(|e| e == "md").unwrap_or(false) {
                if let Ok(content) = std::fs::read_to_string(&path) {
                    let name = path.file_stem()
                        .and_then(|s| s.to_str())
                        .unwrap_or("unknown")
                        .to_string();

                    styles.push(Self {
                        name,
                        content,
                        source_path: Some(path),
                    });
                }
            }
        }

        Ok(styles)
    }
}

/// Read CLAUDE.md file contents.
fn read_claude_md(path: &Path) -> Option<String> {
    if path.exists() {
        std::fs::read_to_string(path).ok()
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_credentials_presence_no_file() {
        let temp_dir = tempfile::tempdir().unwrap();
        let presence = CredentialsPresence::detect(temp_dir.path()).unwrap();

        assert!(!presence.file_exists);
        assert!(!presence.anthropic_key_present);
    }

    #[test]
    fn test_beyond_jsonl_data_default() {
        let data = BeyondJsonlData::default();
        assert!(!data.has_data());
        assert_eq!(data.data_source_count(), 0);
    }
}
