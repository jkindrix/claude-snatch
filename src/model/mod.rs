//! Data model for Claude Code JSONL logs.
//!
//! This module provides strongly-typed structures for all message types,
//! content blocks, and metadata captured in Claude Code session logs.
//! The model supports 77+ documented data elements with forward-compatible
//! unknown field preservation.

pub mod content;
pub mod message;
pub mod metadata;
pub mod tools;
pub mod usage;

pub use content::*;
pub use message::*;
pub use metadata::*;
pub use tools::*;
pub use usage::*;

use indexmap::IndexMap;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Raw JSON value that preserves unknown fields for forward compatibility.
/// This enables lossless round-trip even when new fields are added in future Claude Code versions.
pub type UnknownFields = IndexMap<String, Value>;

/// A wrapper that captures both known fields and unknown fields.
/// Used for forward-compatible parsing.
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct WithUnknown<T> {
    /// The known/parsed fields.
    #[serde(flatten)]
    pub inner: T,

    /// Any unknown fields preserved for lossless round-trip.
    #[serde(flatten)]
    pub unknown: UnknownFields,
}

impl<T> WithUnknown<T> {
    /// Create a new wrapper with no unknown fields.
    pub fn new(inner: T) -> Self {
        Self {
            inner,
            unknown: IndexMap::new(),
        }
    }

    /// Create a wrapper with unknown fields.
    pub fn with_unknown(inner: T, unknown: UnknownFields) -> Self {
        Self { inner, unknown }
    }

    /// Check if there are any unknown fields.
    pub fn has_unknown(&self) -> bool {
        !self.unknown.is_empty()
    }

    /// Get a reference to the inner value.
    pub const fn as_inner(&self) -> &T {
        &self.inner
    }

    /// Get a mutable reference to the inner value.
    pub fn as_inner_mut(&mut self) -> &mut T {
        &mut self.inner
    }

    /// Consume the wrapper and return the inner value.
    pub fn into_inner(self) -> T {
        self.inner
    }
}

impl<T> std::ops::Deref for WithUnknown<T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}

impl<T> std::ops::DerefMut for WithUnknown<T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.inner
    }
}

/// Schema version identifier for Claude Code JSONL format.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SchemaVersion {
    /// Original format (v1.x)
    V1Legacy,
    /// Major restructure (v2.0.0 - v2.0.29)
    V2Base,
    /// Sandbox mode (v2.0.30 - v2.0.39)
    V2Sandbox,
    /// Session slug (v2.0.40 - v2.0.44)
    V2Slug,
    /// Hook events (v2.0.45 - v2.0.55)
    V2Hooks,
    /// Compact metadata (v2.0.56 - v2.0.59)
    V2Compact,
    /// Background agents (v2.0.60 - v2.0.63)
    V2Agents,
    /// Unified TaskOutput (v2.0.64 - v2.0.69) - BREAKING CHANGE
    V2Unified,
    /// Thinking metadata (v2.0.70 - v2.0.71)
    V2Thinking,
    /// Chrome MCP (v2.0.72 - v2.0.73)
    V2Chrome,
    /// LSP tool (v2.0.74+)
    V2Lsp,
    /// Unknown version for forward compatibility
    Unknown(String),
}

impl SchemaVersion {
    /// Parse a version string into a schema version.
    #[must_use]
    pub fn from_version_string(version: &str) -> Self {
        // Parse as semver-like
        let parts: Vec<&str> = version.split('.').collect();
        if parts.len() < 3 {
            return Self::Unknown(version.to_string());
        }

        let major: u32 = parts[0].parse().unwrap_or(0);
        let minor: u32 = parts[1].parse().unwrap_or(0);
        let patch: u32 = parts[2].parse().unwrap_or(0);

        match (major, minor, patch) {
            (1, _, _) => Self::V1Legacy,
            (2, 0, 0..=29) => Self::V2Base,
            (2, 0, 30..=39) => Self::V2Sandbox,
            (2, 0, 40..=44) => Self::V2Slug,
            (2, 0, 45..=55) => Self::V2Hooks,
            (2, 0, 56..=59) => Self::V2Compact,
            (2, 0, 60..=63) => Self::V2Agents,
            (2, 0, 64..=69) => Self::V2Unified,
            (2, 0, 70..=71) => Self::V2Thinking,
            (2, 0, 72..=73) => Self::V2Chrome,
            (2, 0, 74..) => Self::V2Lsp,
            (2, 1.., _) => Self::V2Lsp, // Assume newer versions use latest schema
            _ => Self::Unknown(version.to_string()),
        }
    }

    /// Check if this version supports a given feature.
    #[must_use]
    pub fn supports_feature(&self, feature: &str) -> bool {
        match feature {
            "sandbox" => !matches!(self, Self::V1Legacy | Self::V2Base),
            "slug" => !matches!(self, Self::V1Legacy | Self::V2Base | Self::V2Sandbox),
            "hooks" => !matches!(
                self,
                Self::V1Legacy | Self::V2Base | Self::V2Sandbox | Self::V2Slug
            ),
            "compact_metadata" => !matches!(
                self,
                Self::V1Legacy | Self::V2Base | Self::V2Sandbox | Self::V2Slug | Self::V2Hooks
            ),
            "background_agents" => !matches!(
                self,
                Self::V1Legacy
                    | Self::V2Base
                    | Self::V2Sandbox
                    | Self::V2Slug
                    | Self::V2Hooks
                    | Self::V2Compact
            ),
            "task_output" => !matches!(
                self,
                Self::V1Legacy
                    | Self::V2Base
                    | Self::V2Sandbox
                    | Self::V2Slug
                    | Self::V2Hooks
                    | Self::V2Compact
                    | Self::V2Agents
            ),
            "thinking_metadata" => matches!(
                self,
                Self::V2Thinking | Self::V2Chrome | Self::V2Lsp | Self::Unknown(_)
            ),
            "chrome_mcp" => matches!(self, Self::V2Chrome | Self::V2Lsp | Self::Unknown(_)),
            "lsp" => matches!(self, Self::V2Lsp | Self::Unknown(_)),
            _ => false,
        }
    }
}

impl Default for SchemaVersion {
    fn default() -> Self {
        Self::V2Lsp
    }
}

impl std::fmt::Display for SchemaVersion {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::V1Legacy => write!(f, "v1_legacy"),
            Self::V2Base => write!(f, "v2_base"),
            Self::V2Sandbox => write!(f, "v2_sandbox"),
            Self::V2Slug => write!(f, "v2_slug"),
            Self::V2Hooks => write!(f, "v2_hooks"),
            Self::V2Compact => write!(f, "v2_compact"),
            Self::V2Agents => write!(f, "v2_agents"),
            Self::V2Unified => write!(f, "v2_unified"),
            Self::V2Thinking => write!(f, "v2_thinking"),
            Self::V2Chrome => write!(f, "v2_chrome"),
            Self::V2Lsp => write!(f, "v2_lsp"),
            Self::Unknown(v) => write!(f, "unknown({v})"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_schema_version_parsing() {
        assert_eq!(
            SchemaVersion::from_version_string("1.0.0"),
            SchemaVersion::V1Legacy
        );
        assert_eq!(
            SchemaVersion::from_version_string("2.0.0"),
            SchemaVersion::V2Base
        );
        assert_eq!(
            SchemaVersion::from_version_string("2.0.35"),
            SchemaVersion::V2Sandbox
        );
        assert_eq!(
            SchemaVersion::from_version_string("2.0.74"),
            SchemaVersion::V2Lsp
        );
        assert_eq!(
            SchemaVersion::from_version_string("2.0.100"),
            SchemaVersion::V2Lsp
        );
    }

    #[test]
    fn test_feature_support() {
        let v2_base = SchemaVersion::V2Base;
        assert!(!v2_base.supports_feature("sandbox"));
        assert!(!v2_base.supports_feature("lsp"));

        let v2_lsp = SchemaVersion::V2Lsp;
        assert!(v2_lsp.supports_feature("sandbox"));
        assert!(v2_lsp.supports_feature("lsp"));
        assert!(v2_lsp.supports_feature("thinking_metadata"));
    }
}
