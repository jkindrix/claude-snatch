//! MCP server configuration extraction (BJ-006).
//!
//! Parses the mcp.json configuration file.

use crate::error::{Result, SnatchError};
use indexmap::IndexMap;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::Path;

/// MCP (Model Context Protocol) configuration.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpConfig {
    /// Configured MCP servers.
    #[serde(default, rename = "mcpServers")]
    pub mcp_servers: IndexMap<String, McpServer>,

    /// Unknown fields for forward compatibility.
    #[serde(flatten)]
    pub extra: IndexMap<String, Value>,
}

impl McpConfig {
    /// Load MCP configuration from a file.
    pub fn load(path: &Path) -> Result<Self> {
        if !path.exists() {
            return Err(SnatchError::FileNotFound {
                path: path.to_path_buf(),
            });
        }

        let content = std::fs::read_to_string(path).map_err(|e| {
            SnatchError::io(format!("Failed to read MCP config: {}", path.display()), e)
        })?;

        serde_json::from_str(&content).map_err(|e| {
            SnatchError::ConfigError {
                message: format!("Failed to parse mcp.json: {e}"),
            }
        })
    }

    /// Get the number of configured servers.
    #[must_use]
    pub fn server_count(&self) -> usize {
        self.mcp_servers.len()
    }

    /// Check if any servers are configured.
    #[must_use]
    pub fn has_servers(&self) -> bool {
        !self.mcp_servers.is_empty()
    }

    /// Get server names.
    pub fn server_names(&self) -> Vec<&str> {
        self.mcp_servers.keys().map(String::as_str).collect()
    }

    /// Get a specific server by name.
    pub fn get_server(&self, name: &str) -> Option<&McpServer> {
        self.mcp_servers.get(name)
    }

    /// Check if Chrome MCP server is configured (BJ-020).
    #[must_use]
    pub fn has_chrome_mcp(&self) -> bool {
        self.mcp_servers.contains_key("chrome")
            || self.mcp_servers.keys().any(|k| k.contains("chrome"))
    }
}

/// MCP server configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpServer {
    /// Server command to execute.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,

    /// Command arguments.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub args: Vec<String>,

    /// Environment variables.
    #[serde(default, skip_serializing_if = "IndexMap::is_empty")]
    pub env: IndexMap<String, String>,

    /// Working directory.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,

    /// Transport type (stdio, http, etc.).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub transport: Option<String>,

    /// URL for HTTP transport.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,

    /// Whether this server is disabled.
    #[serde(default)]
    pub disabled: bool,

    /// Server-specific capabilities.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub capabilities: Option<McpCapabilities>,

    /// Unknown fields.
    #[serde(flatten)]
    pub extra: IndexMap<String, Value>,
}

impl McpServer {
    /// Get the transport type, defaulting to "stdio".
    #[must_use]
    pub fn transport_type(&self) -> &str {
        self.transport.as_deref().unwrap_or("stdio")
    }

    /// Check if this is an HTTP-based server.
    #[must_use]
    pub fn is_http(&self) -> bool {
        matches!(self.transport_type(), "http" | "https" | "sse")
    }

    /// Check if this is a stdio-based server.
    #[must_use]
    pub fn is_stdio(&self) -> bool {
        self.transport_type() == "stdio"
    }

    /// Get the full command with arguments.
    #[must_use]
    pub fn full_command(&self) -> String {
        let mut parts = Vec::new();
        if let Some(cmd) = &self.command {
            parts.push(cmd.clone());
        }
        parts.extend(self.args.iter().cloned());
        parts.join(" ")
    }
}

/// MCP server capabilities.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpCapabilities {
    /// Supported tools.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tools: Vec<String>,

    /// Supported resources.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub resources: Vec<String>,

    /// Supported prompts.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub prompts: Vec<String>,

    /// Unknown fields.
    #[serde(flatten)]
    pub extra: IndexMap<String, Value>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_mcp_config_parsing() {
        let json = r#"{
            "mcpServers": {
                "filesystem": {
                    "command": "npx",
                    "args": ["-y", "@anthropic/mcp-server-filesystem"],
                    "env": {
                        "MCP_ROOT": "/home/user"
                    }
                },
                "chrome": {
                    "command": "npx",
                    "args": ["-y", "@anthropic/mcp-server-chrome"],
                    "disabled": true
                }
            }
        }"#;

        let config: McpConfig = serde_json::from_str(json).unwrap();
        assert_eq!(config.server_count(), 2);
        assert!(config.has_chrome_mcp());

        let fs = config.get_server("filesystem").unwrap();
        assert_eq!(fs.command, Some("npx".to_string()));
        assert!(!fs.disabled);

        let chrome = config.get_server("chrome").unwrap();
        assert!(chrome.disabled);
    }

    #[test]
    fn test_empty_config() {
        let config = McpConfig::default();
        assert_eq!(config.server_count(), 0);
        assert!(!config.has_servers());
        assert!(!config.has_chrome_mcp());
    }

    #[test]
    fn test_server_transport() {
        let server = McpServer {
            command: Some("node".to_string()),
            args: vec!["server.js".to_string()],
            env: IndexMap::new(),
            cwd: None,
            transport: None,
            url: None,
            disabled: false,
            capabilities: None,
            extra: IndexMap::new(),
        };

        assert_eq!(server.transport_type(), "stdio");
        assert!(server.is_stdio());
        assert!(!server.is_http());
    }
}
