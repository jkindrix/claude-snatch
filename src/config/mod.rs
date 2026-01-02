//! Configuration management for claude-snatch.
//!
//! Handles:
//! - User preferences
//! - Default export options
//! - Theme settings
//! - Cache configuration

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{Result, SnatchError};
use crate::util::atomic_write;

/// Application configuration.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Config {
    /// Default export format.
    #[serde(default)]
    pub default_format: ExportFormatConfig,
    /// TUI theme.
    #[serde(default)]
    pub theme: ThemeConfig,
    /// Display options.
    #[serde(default)]
    pub display: DisplayConfig,
    /// Cache settings.
    #[serde(default)]
    pub cache: CacheConfig,
    /// Index settings.
    #[serde(default)]
    pub index: IndexConfig,
    /// Budget settings for cost alerts.
    #[serde(default)]
    pub budget: BudgetConfig,
}

/// Project-specific configuration filename.
pub const PROJECT_CONFIG_FILENAME: &str = ".claude-snatch.toml";

impl Config {
    /// Load configuration from default locations.
    pub fn load() -> Result<Self> {
        let config_path = default_config_path()?;
        if config_path.exists() {
            Self::load_from(&config_path)
        } else {
            Ok(Self::default())
        }
    }

    /// Load configuration with project-specific overrides.
    ///
    /// Searches for `.claude-snatch.toml` in the given project directory
    /// and merges it with the global configuration.
    pub fn load_for_project(project_dir: &Path) -> Result<Self> {
        // Start with global config
        let mut config = Self::load().unwrap_or_default();

        // Look for project config
        let project_config_path = project_dir.join(PROJECT_CONFIG_FILENAME);
        if project_config_path.exists() {
            let project_config = Self::load_from(&project_config_path)?;
            config.merge_from(&project_config);
        }

        Ok(config)
    }

    /// Load configuration from a specific path.
    pub fn load_from(path: &Path) -> Result<Self> {
        let content = std::fs::read_to_string(path).map_err(|e| {
            SnatchError::io(format!("Failed to read config file: {}", path.display()), e)
        })?;

        toml::from_str(&content).map_err(|e| {
            SnatchError::InvalidConfig {
                message: e.to_string(),
            }
        })
    }

    /// Merge another config into this one (other takes precedence).
    pub fn merge_from(&mut self, other: &Config) {
        // Merge export format config
        if other.default_format.format != "markdown" {
            self.default_format.format = other.default_format.format.clone();
        }
        self.default_format.include_thinking = other.default_format.include_thinking;
        self.default_format.include_tool_use = other.default_format.include_tool_use;
        self.default_format.include_timestamps = other.default_format.include_timestamps;
        self.default_format.pretty_json = other.default_format.pretty_json;

        // Merge theme config
        if other.theme.name != "default" {
            self.theme.name = other.theme.name.clone();
        }
        self.theme.color = other.theme.color;
        self.theme.unicode = other.theme.unicode;

        // Merge display config
        self.display.full_ids = other.display.full_ids;
        self.display.show_sizes = other.display.show_sizes;
        if other.display.truncate_at != 10000 {
            self.display.truncate_at = other.display.truncate_at;
        }
        if other.display.context_lines != 2 {
            self.display.context_lines = other.display.context_lines;
        }

        // Merge cache config
        self.cache.enabled = other.cache.enabled;
        if other.cache.directory.is_some() {
            self.cache.directory = other.cache.directory.clone();
        }
        if other.cache.max_size != 100 * 1024 * 1024 {
            self.cache.max_size = other.cache.max_size;
        }
        if other.cache.ttl_seconds != 3600 {
            self.cache.ttl_seconds = other.cache.ttl_seconds;
        }

        // Merge index config
        if other.index.directory.is_some() {
            self.index.directory = other.index.directory.clone();
        }

        // Merge budget config
        if other.budget.daily_limit.is_some() {
            self.budget.daily_limit = other.budget.daily_limit;
        }
        if other.budget.weekly_limit.is_some() {
            self.budget.weekly_limit = other.budget.weekly_limit;
        }
        if other.budget.monthly_limit.is_some() {
            self.budget.monthly_limit = other.budget.monthly_limit;
        }
        if other.budget.warning_threshold != 0.8 {
            self.budget.warning_threshold = other.budget.warning_threshold;
        }
        self.budget.show_in_stats = other.budget.show_in_stats;
    }

    /// Save configuration to the default location.
    pub fn save(&self) -> Result<()> {
        let config_path = default_config_path()?;
        self.save_to(&config_path)
    }

    /// Save configuration to a specific path.
    ///
    /// Uses atomic file writes to ensure configuration integrity.
    /// The config is written to a temporary file first, then atomically
    /// renamed to the target path.
    pub fn save_to(&self, path: &Path) -> Result<()> {
        let content = toml::to_string_pretty(self).map_err(|e| {
            SnatchError::InvalidConfig {
                message: format!("Failed to serialize config: {e}"),
            }
        })?;

        // Use atomic write - it handles parent directory creation
        atomic_write(path, content.as_bytes())?;

        Ok(())
    }
}

/// Export format configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExportFormatConfig {
    /// Default format for export command.
    #[serde(default = "default_format")]
    pub format: String,
    /// Include thinking blocks by default.
    #[serde(default = "default_true")]
    pub include_thinking: bool,
    /// Include tool use by default.
    #[serde(default = "default_true")]
    pub include_tool_use: bool,
    /// Include timestamps by default.
    #[serde(default = "default_true")]
    pub include_timestamps: bool,
    /// Pretty-print JSON by default.
    #[serde(default)]
    pub pretty_json: bool,
}

impl Default for ExportFormatConfig {
    fn default() -> Self {
        Self {
            format: "markdown".to_string(),
            include_thinking: true,
            include_tool_use: true,
            include_timestamps: true,
            pretty_json: false,
        }
    }
}

/// Theme configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ThemeConfig {
    /// Theme name.
    #[serde(default = "default_theme")]
    pub name: String,
    /// Use color output.
    #[serde(default = "default_true")]
    pub color: bool,
    /// Use Unicode characters.
    #[serde(default = "default_true")]
    pub unicode: bool,
}

impl Default for ThemeConfig {
    fn default() -> Self {
        Self {
            name: "default".to_string(),
            color: true,
            unicode: true,
        }
    }
}

/// Display configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DisplayConfig {
    /// Show full UUIDs.
    #[serde(default)]
    pub full_ids: bool,
    /// Show file sizes.
    #[serde(default = "default_true")]
    pub show_sizes: bool,
    /// Truncate long content at this length.
    #[serde(default = "default_truncate")]
    pub truncate_at: usize,
    /// Number of context lines for search.
    #[serde(default = "default_context")]
    pub context_lines: usize,
}

impl Default for DisplayConfig {
    fn default() -> Self {
        Self {
            full_ids: false,
            show_sizes: true,
            truncate_at: 10000,
            context_lines: 2,
        }
    }
}

/// Cache configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CacheConfig {
    /// Enable caching.
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Cache directory.
    #[serde(default)]
    pub directory: Option<PathBuf>,
    /// Maximum cache size in bytes.
    #[serde(default = "default_cache_size")]
    pub max_size: u64,
    /// Cache TTL in seconds.
    #[serde(default = "default_cache_ttl")]
    pub ttl_seconds: u64,
}

/// Index configuration.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct IndexConfig {
    /// Custom index directory.
    #[serde(default)]
    pub directory: Option<PathBuf>,
}

/// Budget configuration for cost alerts.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BudgetConfig {
    /// Daily budget limit in USD.
    #[serde(default)]
    pub daily_limit: Option<f64>,
    /// Weekly budget limit in USD.
    #[serde(default)]
    pub weekly_limit: Option<f64>,
    /// Monthly budget limit in USD.
    #[serde(default)]
    pub monthly_limit: Option<f64>,
    /// Warning threshold as a percentage of the limit (0.0 - 1.0).
    /// When usage exceeds this percentage of the limit, show a warning.
    /// Default is 0.8 (80%).
    #[serde(default = "default_warning_threshold")]
    pub warning_threshold: f64,
    /// Whether to show budget status in stats output.
    #[serde(default = "default_true")]
    pub show_in_stats: bool,
}

impl Default for BudgetConfig {
    fn default() -> Self {
        Self {
            daily_limit: None,
            weekly_limit: None,
            monthly_limit: None,
            warning_threshold: 0.8,
            show_in_stats: true,
        }
    }
}

impl BudgetConfig {
    /// Check if any budget limits are configured.
    pub fn has_limits(&self) -> bool {
        self.daily_limit.is_some() || self.weekly_limit.is_some() || self.monthly_limit.is_some()
    }

    /// Check budget status against current costs.
    pub fn check(&self, daily_cost: f64, weekly_cost: f64, monthly_cost: f64) -> BudgetStatus {
        let daily = self.daily_limit.map(|limit| {
            BudgetAlert::new("Daily", daily_cost, limit, self.warning_threshold)
        });

        let weekly = self.weekly_limit.map(|limit| {
            BudgetAlert::new("Weekly", weekly_cost, limit, self.warning_threshold)
        });

        let monthly = self.monthly_limit.map(|limit| {
            BudgetAlert::new("Monthly", monthly_cost, limit, self.warning_threshold)
        });

        BudgetStatus { daily, weekly, monthly }
    }
}

/// Budget status for all configured periods.
#[derive(Debug, Clone, Default)]
pub struct BudgetStatus {
    /// Daily budget alert (if daily limit configured).
    pub daily: Option<BudgetAlert>,
    /// Weekly budget alert (if weekly limit configured).
    pub weekly: Option<BudgetAlert>,
    /// Monthly budget alert (if monthly limit configured).
    pub monthly: Option<BudgetAlert>,
}

impl BudgetStatus {
    /// Check if any budget is exceeded.
    pub fn any_exceeded(&self) -> bool {
        [&self.daily, &self.weekly, &self.monthly]
            .iter()
            .filter_map(|a| a.as_ref())
            .any(|a| a.exceeded)
    }

    /// Check if any budget is in warning state.
    pub fn any_warning(&self) -> bool {
        [&self.daily, &self.weekly, &self.monthly]
            .iter()
            .filter_map(|a| a.as_ref())
            .any(|a| a.warning)
    }

    /// Get all alerts that need attention (warning or exceeded).
    pub fn alerts(&self) -> Vec<&BudgetAlert> {
        [&self.daily, &self.weekly, &self.monthly]
            .iter()
            .filter_map(|a| a.as_ref())
            .filter(|a| a.warning || a.exceeded)
            .collect()
    }
}

/// A single budget alert for a time period.
#[derive(Debug, Clone)]
pub struct BudgetAlert {
    /// Period name (e.g., "Daily", "Weekly").
    pub period: String,
    /// Current spending.
    pub spent: f64,
    /// Budget limit.
    pub limit: f64,
    /// Percentage used (0.0 - 1.0+).
    pub percent_used: f64,
    /// Remaining budget (can be negative if exceeded).
    pub remaining: f64,
    /// Whether the budget is exceeded.
    pub exceeded: bool,
    /// Whether the budget is in warning state.
    pub warning: bool,
}

impl BudgetAlert {
    fn new(period: &str, spent: f64, limit: f64, warning_threshold: f64) -> Self {
        let percent_used = if limit > 0.0 { spent / limit } else { 0.0 };
        let remaining = limit - spent;
        let exceeded = spent >= limit;
        let warning = !exceeded && percent_used >= warning_threshold;

        Self {
            period: period.to_string(),
            spent,
            limit,
            percent_used,
            remaining,
            exceeded,
            warning,
        }
    }

    /// Get a status indicator for display.
    pub fn status_indicator(&self) -> &'static str {
        if self.exceeded {
            "EXCEEDED"
        } else if self.warning {
            "WARNING"
        } else {
            "OK"
        }
    }

    /// Get a colored status indicator (ANSI escape codes).
    pub fn colored_status(&self) -> String {
        if self.exceeded {
            "\x1b[1;31mEXCEEDED\x1b[0m".to_string()
        } else if self.warning {
            "\x1b[1;33mWARNING\x1b[0m".to_string()
        } else {
            "\x1b[32mOK\x1b[0m".to_string()
        }
    }
}

fn default_warning_threshold() -> f64 {
    0.8
}

impl Default for CacheConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            directory: None,
            max_size: 100 * 1024 * 1024, // 100 MB
            ttl_seconds: 3600,           // 1 hour
        }
    }
}

// Default value functions for serde
fn default_true() -> bool {
    true
}

fn default_format() -> String {
    "markdown".to_string()
}

fn default_theme() -> String {
    "default".to_string()
}

fn default_truncate() -> usize {
    10000
}

fn default_context() -> usize {
    2
}

fn default_cache_size() -> u64 {
    100 * 1024 * 1024
}

fn default_cache_ttl() -> u64 {
    3600
}

/// Get the default configuration path.
pub fn default_config_path() -> Result<PathBuf> {
    let config_dir = dirs::config_dir().ok_or_else(|| {
        SnatchError::Unsupported {
            feature: "config directory discovery".to_string(),
        }
    })?;

    Ok(config_dir.join("claude-snatch").join("config.toml"))
}

/// Get the default cache directory.
pub fn default_cache_dir() -> Result<PathBuf> {
    let cache_dir = dirs::cache_dir().ok_or_else(|| {
        SnatchError::Unsupported {
            feature: "cache directory discovery".to_string(),
        }
    })?;

    Ok(cache_dir.join("claude-snatch"))
}

/// Get the default templates directory.
pub fn default_templates_dir() -> Result<PathBuf> {
    let config_dir = dirs::config_dir().ok_or_else(|| {
        SnatchError::Unsupported {
            feature: "config directory discovery".to_string(),
        }
    })?;

    Ok(config_dir.join("claude-snatch").join("templates"))
}

/// Export template configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExportTemplate {
    /// Template name (filename without extension).
    pub name: String,
    /// Template description.
    #[serde(default)]
    pub description: String,
    /// Template content format (markdown, text, html).
    #[serde(default = "default_template_format")]
    pub format: String,
    /// Template content (uses Handlebars-like syntax).
    pub content: String,
    /// Header to prepend to output.
    #[serde(default)]
    pub header: Option<String>,
    /// Footer to append to output.
    #[serde(default)]
    pub footer: Option<String>,
    /// Entry separator.
    #[serde(default = "default_entry_separator")]
    pub entry_separator: String,
    /// User message template.
    #[serde(default)]
    pub user_template: Option<String>,
    /// Assistant message template.
    #[serde(default)]
    pub assistant_template: Option<String>,
    /// System message template.
    #[serde(default)]
    pub system_template: Option<String>,
    /// Thinking block template.
    #[serde(default)]
    pub thinking_template: Option<String>,
    /// Tool use template.
    #[serde(default)]
    pub tool_use_template: Option<String>,
    /// Tool result template.
    #[serde(default)]
    pub tool_result_template: Option<String>,
}

fn default_template_format() -> String {
    "text".to_string()
}

fn default_entry_separator() -> String {
    "\n---\n\n".to_string()
}

impl ExportTemplate {
    /// Load a template from a file.
    pub fn load_from(path: &Path) -> Result<Self> {
        let content = std::fs::read_to_string(path).map_err(|e| {
            SnatchError::io(format!("Failed to read template file: {}", path.display()), e)
        })?;

        // Try TOML first
        if path.extension().map(|e| e == "toml").unwrap_or(false) {
            toml::from_str(&content).map_err(|e| {
                SnatchError::InvalidConfig {
                    message: format!("Failed to parse template: {}", e),
                }
            })
        } else {
            // For non-TOML files, treat the whole content as a simple template
            let name = path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("unknown")
                .to_string();

            Ok(Self {
                name,
                description: String::new(),
                format: default_template_format(),
                content,
                header: None,
                footer: None,
                entry_separator: default_entry_separator(),
                user_template: None,
                assistant_template: None,
                system_template: None,
                thinking_template: None,
                tool_use_template: None,
                tool_result_template: None,
            })
        }
    }
}

/// List available export templates.
pub fn list_templates() -> Result<Vec<ExportTemplate>> {
    let templates_dir = default_templates_dir()?;

    if !templates_dir.exists() {
        return Ok(Vec::new());
    }

    let mut templates = Vec::new();

    let entries = std::fs::read_dir(&templates_dir).map_err(|e| {
        SnatchError::io(format!("Failed to read templates directory: {}", templates_dir.display()), e)
    })?;

    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_file() {
            if let Some(ext) = path.extension() {
                if ext == "toml" || ext == "md" || ext == "txt" || ext == "html" {
                    if let Ok(template) = ExportTemplate::load_from(&path) {
                        templates.push(template);
                    }
                }
            }
        }
    }

    templates.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(templates)
}

/// Load a specific template by name.
pub fn load_template(name: &str) -> Result<ExportTemplate> {
    let templates_dir = default_templates_dir()?;

    // Try different extensions
    for ext in &["toml", "md", "txt", "html"] {
        let path = templates_dir.join(format!("{}.{}", name, ext));
        if path.exists() {
            return ExportTemplate::load_from(&path);
        }
    }

    Err(SnatchError::ConfigError {
        message: format!("Template '{}' not found in {}", name, templates_dir.display()),
    })
}

/// Create a sample template in the templates directory.
pub fn create_sample_template() -> Result<PathBuf> {
    let templates_dir = default_templates_dir()?;

    // Create templates directory if it doesn't exist
    std::fs::create_dir_all(&templates_dir).map_err(|e| {
        SnatchError::io(format!("Failed to create templates directory: {}", templates_dir.display()), e)
    })?;

    let sample_path = templates_dir.join("summary.toml");

    // Don't overwrite if it exists
    if sample_path.exists() {
        return Ok(sample_path);
    }

    let sample_content = r#"# Summary Export Template
# This template creates a concise summary of the conversation

name = "summary"
description = "Concise conversation summary with key points"
format = "markdown"

header = """
# Session Summary

Generated: {{timestamp}}
Project: {{project_path}}
Session: {{session_id}}

---

"""

footer = """

---

*Total messages: {{message_count}}*
*Total tokens: {{total_tokens}}*
"""

entry_separator = "\n"

user_template = "**User:** {{content}}\n"
assistant_template = "**Claude:** {{content}}\n"
thinking_template = ""  # Skip thinking blocks in summary
tool_use_template = "- Used tool: `{{tool_name}}`\n"
tool_result_template = ""  # Skip tool results in summary
"#;

    std::fs::write(&sample_path, sample_content).map_err(|e| {
        SnatchError::io(format!("Failed to write sample template: {}", sample_path.display()), e)
    })?;

    Ok(sample_path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_config() {
        let config = Config::default();
        assert_eq!(config.default_format.format, "markdown");
        assert!(config.theme.color);
        assert!(config.cache.enabled);
    }

    #[test]
    fn test_config_serialization() {
        let config = Config::default();
        let toml = toml::to_string(&config).unwrap();
        let parsed: Config = toml::from_str(&toml).unwrap();
        assert_eq!(parsed.default_format.format, config.default_format.format);
    }

    #[test]
    fn test_config_merge() {
        let mut base = Config::default();
        let mut override_config = Config::default();

        // Set some overrides
        override_config.default_format.format = "json".to_string();
        override_config.theme.name = "dark".to_string();
        override_config.display.truncate_at = 5000;

        base.merge_from(&override_config);

        assert_eq!(base.default_format.format, "json");
        assert_eq!(base.theme.name, "dark");
        assert_eq!(base.display.truncate_at, 5000);
    }

    #[test]
    fn test_load_for_project() {
        let temp_dir = tempfile::tempdir().unwrap();

        // Create project config
        let project_config = r#"
[default_format]
format = "text"
include_thinking = false

[display]
truncate_at = 3000
"#;

        std::fs::write(
            temp_dir.path().join(PROJECT_CONFIG_FILENAME),
            project_config
        ).unwrap();

        let config = Config::load_for_project(temp_dir.path()).unwrap();

        assert_eq!(config.default_format.format, "text");
        assert!(!config.default_format.include_thinking);
        assert_eq!(config.display.truncate_at, 3000);
        // Defaults should be preserved where not overridden
        assert!(config.theme.color);
    }

    #[test]
    fn test_load_for_project_no_config() {
        let temp_dir = tempfile::tempdir().unwrap();

        // No project config file
        let config = Config::load_for_project(temp_dir.path()).unwrap();

        // Should return defaults
        assert_eq!(config.default_format.format, "markdown");
        assert!(config.cache.enabled);
    }
}
