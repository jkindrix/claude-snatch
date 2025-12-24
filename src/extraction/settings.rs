//! Claude Code settings.json parsing (BJ-002, BJ-003).
//!
//! Parses global and project-level Claude Code settings files.

use crate::error::{Result, SnatchError};
use indexmap::IndexMap;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::Path;

/// Claude Code settings structure.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClaudeSettings {
    /// API configuration.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub api: Option<ApiSettings>,

    /// Model preferences.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<ModelSettings>,

    /// Tool permissions.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub permissions: Vec<PermissionRule>,

    /// Hook configurations (BJ-011).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub hooks: Vec<HookConfig>,

    /// Session retention settings (BJ-014).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_retention: Option<super::SessionRetention>,

    /// Sandbox configuration (BJ-015).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sandbox: Option<super::SandboxConfig>,

    /// Thinking mode configuration.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thinking: Option<ThinkingSettings>,

    /// TUI/display preferences.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub display: Option<DisplaySettings>,

    /// Custom environment variables.
    #[serde(default, skip_serializing_if = "IndexMap::is_empty")]
    pub env: IndexMap<String, String>,

    /// Unknown fields for forward compatibility.
    #[serde(flatten)]
    pub extra: IndexMap<String, Value>,
}

impl ClaudeSettings {
    /// Load settings from a file path.
    pub fn load(path: &Path) -> Result<Self> {
        if !path.exists() {
            return Err(SnatchError::FileNotFound {
                path: path.to_path_buf(),
            });
        }

        let content = std::fs::read_to_string(path).map_err(|e| {
            SnatchError::io(format!("Failed to read settings: {}", path.display()), e)
        })?;

        serde_json::from_str(&content).map_err(|e| {
            SnatchError::ConfigError {
                message: format!("Failed to parse settings.json: {e}"),
            }
        })
    }

    /// Check if any permissions are configured.
    #[must_use]
    pub fn has_permissions(&self) -> bool {
        !self.permissions.is_empty()
    }

    /// Check if any hooks are configured.
    #[must_use]
    pub fn has_hooks(&self) -> bool {
        !self.hooks.is_empty()
    }

    /// Get hooks for a specific event type.
    pub fn hooks_for_event(&self, event: &str) -> Vec<&HookConfig> {
        self.hooks
            .iter()
            .filter(|h| h.event.as_deref() == Some(event) || h.matcher.as_deref() == Some(event))
            .collect()
    }
}

/// API configuration settings.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApiSettings {
    /// API endpoint (for self-hosted).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub endpoint: Option<String>,

    /// Request timeout in seconds.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timeout: Option<u32>,

    /// Maximum retries.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_retries: Option<u32>,

    /// Unknown fields.
    #[serde(flatten)]
    pub extra: IndexMap<String, Value>,
}

/// Model preference settings.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelSettings {
    /// Default model ID.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default: Option<String>,

    /// Temperature setting.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f64>,

    /// Max tokens.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u32>,

    /// Unknown fields.
    #[serde(flatten)]
    pub extra: IndexMap<String, Value>,
}

/// Permission rule for tools.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PermissionRule {
    /// Tool name pattern.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool: Option<String>,

    /// Whether allowed or denied.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub allow: Option<bool>,

    /// Path restrictions.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub paths: Vec<String>,

    /// Glob patterns.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub patterns: Vec<String>,

    /// Unknown fields.
    #[serde(flatten)]
    pub extra: IndexMap<String, Value>,
}

/// Hook configuration (BJ-011).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HookConfig {
    /// Hook name/identifier.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,

    /// Event that triggers the hook.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub event: Option<String>,

    /// Pattern to match (alternative to event).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub matcher: Option<String>,

    /// Shell command to execute.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,

    /// Working directory.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,

    /// Timeout in milliseconds.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timeout: Option<u64>,

    /// Whether this hook is enabled.
    #[serde(default = "default_true")]
    pub enabled: bool,

    /// Unknown fields.
    #[serde(flatten)]
    pub extra: IndexMap<String, Value>,
}

fn default_true() -> bool {
    true
}

/// Thinking mode settings.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThinkingSettings {
    /// Thinking budget level.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub level: Option<String>,

    /// Whether disabled.
    #[serde(default)]
    pub disabled: bool,

    /// Unknown fields.
    #[serde(flatten)]
    pub extra: IndexMap<String, Value>,
}

/// Display/TUI settings.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DisplaySettings {
    /// Theme name.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub theme: Option<String>,

    /// Whether to show timestamps.
    #[serde(default)]
    pub show_timestamps: bool,

    /// Whether to show token usage.
    #[serde(default)]
    pub show_usage: bool,

    /// Unknown fields.
    #[serde(flatten)]
    pub extra: IndexMap<String, Value>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_settings_parsing() {
        let json = r#"{
            "model": {
                "default": "claude-sonnet-4-20250514"
            },
            "hooks": [
                {
                    "name": "lint-check",
                    "event": "pre-commit",
                    "command": "npm run lint"
                }
            ]
        }"#;

        let settings: ClaudeSettings = serde_json::from_str(json).unwrap();
        assert!(settings.model.is_some());
        assert_eq!(settings.hooks.len(), 1);
        assert!(settings.has_hooks());
    }

    #[test]
    fn test_empty_settings() {
        let settings = ClaudeSettings::default();
        assert!(!settings.has_permissions());
        assert!(!settings.has_hooks());
    }
}
