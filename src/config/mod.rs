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
#[derive(Debug, Clone, Serialize, Deserialize)]
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
}

impl Default for Config {
    fn default() -> Self {
        Self {
            default_format: ExportFormatConfig::default(),
            theme: ThemeConfig::default(),
            display: DisplayConfig::default(),
            cache: CacheConfig::default(),
        }
    }
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
