//! Schema version handling and migration.
//!
//! This module provides:
//! - Version-specific parsing strategies (SCHEMA-005)
//! - Schema migration for export formats (SCHEMA-006)
//! - Schema change detection and warnings (SCHEMA-008)

use std::collections::HashSet;

use super::SchemaVersion;

/// Parsing strategy based on schema version.
#[derive(Debug, Clone, Default)]
pub struct ParsingStrategy {
    /// Expected message types for this version.
    pub expected_types: HashSet<String>,
    /// Optional fields that may be missing.
    pub optional_fields: HashSet<String>,
    /// Fields that should be present.
    pub required_fields: HashSet<String>,
    /// Whether to expect sandbox mode fields.
    pub expect_sandbox: bool,
    /// Whether to expect hook events.
    pub expect_hooks: bool,
    /// Whether to expect thinking metadata.
    pub expect_thinking_metadata: bool,
    /// Whether to expect LSP tools.
    pub expect_lsp: bool,
}

impl ParsingStrategy {
    /// Create a parsing strategy for a specific schema version.
    #[must_use]
    pub fn for_version(version: &SchemaVersion) -> Self {
        let mut strategy = Self::default();

        // Common fields
        strategy.required_fields.insert("type".to_string());
        strategy.required_fields.insert("uuid".to_string());

        // Type expectations
        strategy.expected_types.insert("user".to_string());
        strategy.expected_types.insert("assistant".to_string());
        strategy.expected_types.insert("summary".to_string());

        // Version-specific adjustments
        match version {
            SchemaVersion::V1Legacy => {
                strategy.optional_fields.insert("parentUuid".to_string());
                strategy.optional_fields.insert("isSidechain".to_string());
            }
            SchemaVersion::V2Base => {
                strategy.required_fields.insert("parentUuid".to_string());
            }
            SchemaVersion::V2Sandbox | SchemaVersion::V2Slug => {
                strategy.expect_sandbox = true;
                strategy.required_fields.insert("parentUuid".to_string());
            }
            SchemaVersion::V2Hooks | SchemaVersion::V2Compact | SchemaVersion::V2Agents => {
                strategy.expect_sandbox = true;
                strategy.expect_hooks = true;
                strategy.expected_types.insert("system".to_string());
                strategy.required_fields.insert("parentUuid".to_string());
            }
            SchemaVersion::V2Unified | SchemaVersion::V2Thinking => {
                strategy.expect_sandbox = true;
                strategy.expect_hooks = true;
                strategy.expect_thinking_metadata = true;
                strategy.expected_types.insert("system".to_string());
                strategy.required_fields.insert("parentUuid".to_string());
            }
            SchemaVersion::V2Chrome | SchemaVersion::V2Lsp => {
                strategy.expect_sandbox = true;
                strategy.expect_hooks = true;
                strategy.expect_thinking_metadata = true;
                strategy.expect_lsp = true;
                strategy.expected_types.insert("system".to_string());
                strategy.required_fields.insert("parentUuid".to_string());
            }
            SchemaVersion::Unknown(_) => {
                // Be lenient for unknown versions
                strategy.optional_fields.insert("parentUuid".to_string());
                strategy.optional_fields.insert("isSidechain".to_string());
            }
        }

        strategy
    }

    /// Check if a message type is expected.
    #[must_use]
    pub fn is_type_expected(&self, msg_type: &str) -> bool {
        self.expected_types.contains(msg_type)
    }

    /// Check if a field is required.
    #[must_use]
    pub fn is_field_required(&self, field: &str) -> bool {
        self.required_fields.contains(field)
    }

    /// Check if a field is optional.
    #[must_use]
    pub fn is_field_optional(&self, field: &str) -> bool {
        self.optional_fields.contains(field)
    }
}

/// Schema change detection result.
#[derive(Debug, Clone)]
pub struct SchemaChangeWarning {
    /// Previous schema version.
    pub from: SchemaVersion,
    /// New schema version.
    pub to: SchemaVersion,
    /// Description of the change.
    pub description: String,
    /// Whether this is a breaking change.
    pub is_breaking: bool,
    /// Recommended action.
    pub recommendation: String,
}

impl SchemaChangeWarning {
    /// Format as a warning message.
    #[must_use]
    pub fn format(&self) -> String {
        let severity = if self.is_breaking { "BREAKING" } else { "WARNING" };
        format!(
            "[{}] Schema changed from {:?} to {:?}: {}. {}",
            severity, self.from, self.to, self.description, self.recommendation
        )
    }
}

/// Detect schema changes between versions.
#[must_use]
pub fn detect_schema_change(from: &SchemaVersion, to: &SchemaVersion) -> Option<SchemaChangeWarning> {
    if from == to {
        return None;
    }

    // Check for breaking changes
    let is_breaking = matches!(
        (from, to),
        (SchemaVersion::V2Agents, SchemaVersion::V2Unified)
            | (SchemaVersion::V2Agents, SchemaVersion::V2Thinking)
            | (SchemaVersion::V2Agents, SchemaVersion::V2Chrome)
            | (SchemaVersion::V2Agents, SchemaVersion::V2Lsp)
    );

    let description = match (from, to) {
        (SchemaVersion::V1Legacy, _) => "Upgraded from legacy v1.x format".to_string(),
        (SchemaVersion::V2Agents, SchemaVersion::V2Unified) => {
            "Task output structure unified (breaking)".to_string()
        }
        (_, SchemaVersion::V2Lsp) => "Upgraded to latest LSP-enabled schema".to_string(),
        _ => format!("Schema version changed from {:?} to {:?}", from, to),
    };

    let recommendation = if is_breaking {
        "Re-export data with latest version for consistency".to_string()
    } else {
        "No action required; changes are backward compatible".to_string()
    };

    Some(SchemaChangeWarning {
        from: from.clone(),
        to: to.clone(),
        description,
        is_breaking,
        recommendation,
    })
}

/// Schema migration utilities.
pub struct SchemaMigration;

impl SchemaMigration {
    /// Migrate a JSON value from one schema version to another.
    ///
    /// This function normalizes the JSON structure to the target schema,
    /// adding missing fields with default values and removing obsolete fields.
    pub fn migrate(
        value: &mut serde_json::Value,
        from: &SchemaVersion,
        to: &SchemaVersion,
    ) -> MigrationResult {
        let mut result = MigrationResult::default();

        if from == to {
            return result;
        }

        // Ensure version field is updated
        if let Some(obj) = value.as_object_mut() {
            obj.insert("version".to_string(), serde_json::json!("2.0.74"));
            result.fields_updated.push("version".to_string());
        }

        // Add missing parentUuid for v1.x logs
        if matches!(from, SchemaVersion::V1Legacy) {
            if let Some(obj) = value.as_object_mut() {
                if !obj.contains_key("parentUuid") {
                    obj.insert("parentUuid".to_string(), serde_json::Value::Null);
                    result.fields_added.push("parentUuid".to_string());
                }
            }
        }

        // Normalize isSidechain
        if let Some(obj) = value.as_object_mut() {
            if !obj.contains_key("isSidechain") {
                obj.insert("isSidechain".to_string(), serde_json::json!(false));
                result.fields_added.push("isSidechain".to_string());
            }
        }

        // Add timestamp if missing
        if let Some(obj) = value.as_object_mut() {
            if !obj.contains_key("timestamp") {
                obj.insert(
                    "timestamp".to_string(),
                    serde_json::json!(chrono::Utc::now().to_rfc3339()),
                );
                result.fields_added.push("timestamp".to_string());
            }
        }

        result.success = true;
        result
    }

    /// Get the target schema version for export.
    #[must_use]
    pub fn target_version() -> SchemaVersion {
        SchemaVersion::V2Lsp
    }
}

/// Result of a schema migration.
#[derive(Debug, Clone, Default)]
pub struct MigrationResult {
    /// Whether the migration was successful.
    pub success: bool,
    /// Fields that were added.
    pub fields_added: Vec<String>,
    /// Fields that were updated.
    pub fields_updated: Vec<String>,
    /// Fields that were removed.
    pub fields_removed: Vec<String>,
    /// Warnings generated during migration.
    pub warnings: Vec<String>,
}

impl MigrationResult {
    /// Check if any changes were made.
    #[must_use]
    pub fn has_changes(&self) -> bool {
        !self.fields_added.is_empty()
            || !self.fields_updated.is_empty()
            || !self.fields_removed.is_empty()
    }

    /// Get a summary of changes.
    #[must_use]
    pub fn summary(&self) -> String {
        let mut parts = Vec::new();
        if !self.fields_added.is_empty() {
            parts.push(format!("{} added", self.fields_added.len()));
        }
        if !self.fields_updated.is_empty() {
            parts.push(format!("{} updated", self.fields_updated.len()));
        }
        if !self.fields_removed.is_empty() {
            parts.push(format!("{} removed", self.fields_removed.len()));
        }
        if parts.is_empty() {
            "No changes".to_string()
        } else {
            parts.join(", ")
        }
    }
}

/// Schema compatibility checker.
pub struct SchemaCompatibility;

impl SchemaCompatibility {
    /// Check if two versions are compatible for merging/comparison.
    #[must_use]
    pub fn are_compatible(v1: &SchemaVersion, v2: &SchemaVersion) -> bool {
        // All v2.x versions are generally compatible
        !matches!(v1, SchemaVersion::V1Legacy) && !matches!(v2, SchemaVersion::V1Legacy)
    }

    /// Get compatibility warnings between versions.
    #[must_use]
    pub fn get_warnings(v1: &SchemaVersion, v2: &SchemaVersion) -> Vec<String> {
        let mut warnings = Vec::new();

        if matches!(v1, SchemaVersion::V1Legacy) || matches!(v2, SchemaVersion::V1Legacy) {
            warnings.push("v1.x format may have missing fields".to_string());
        }

        // Check for breaking changes around v2.0.64
        if (matches!(v1, SchemaVersion::V2Agents) && !matches!(v2, SchemaVersion::V2Agents))
            || (!matches!(v1, SchemaVersion::V2Agents) && matches!(v2, SchemaVersion::V2Agents))
        {
            warnings.push("TaskOutput structure changed at v2.0.64".to_string());
        }

        warnings
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parsing_strategy_v1() {
        let strategy = ParsingStrategy::for_version(&SchemaVersion::V1Legacy);
        assert!(strategy.is_field_optional("parentUuid"));
        assert!(!strategy.expect_sandbox);
    }

    #[test]
    fn test_parsing_strategy_v2_lsp() {
        let strategy = ParsingStrategy::for_version(&SchemaVersion::V2Lsp);
        assert!(strategy.is_type_expected("system"));
        assert!(strategy.expect_lsp);
        assert!(strategy.expect_thinking_metadata);
    }

    #[test]
    fn test_schema_change_detection() {
        let warning = detect_schema_change(&SchemaVersion::V2Agents, &SchemaVersion::V2Unified);
        assert!(warning.is_some());
        let warning = warning.unwrap();
        assert!(warning.is_breaking);
    }

    #[test]
    fn test_schema_change_same_version() {
        let warning = detect_schema_change(&SchemaVersion::V2Lsp, &SchemaVersion::V2Lsp);
        assert!(warning.is_none());
    }

    #[test]
    fn test_migration() {
        let mut value = serde_json::json!({
            "type": "user",
            "uuid": "test"
        });

        let result = SchemaMigration::migrate(
            &mut value,
            &SchemaVersion::V1Legacy,
            &SchemaVersion::V2Lsp,
        );

        assert!(result.success);
        assert!(result.has_changes());
        assert!(value.get("parentUuid").is_some());
        assert!(value.get("isSidechain").is_some());
    }

    #[test]
    fn test_compatibility() {
        assert!(SchemaCompatibility::are_compatible(
            &SchemaVersion::V2Base,
            &SchemaVersion::V2Lsp
        ));
        assert!(!SchemaCompatibility::are_compatible(
            &SchemaVersion::V1Legacy,
            &SchemaVersion::V2Lsp
        ));
    }

    #[test]
    fn test_compatibility_warnings() {
        let warnings = SchemaCompatibility::get_warnings(
            &SchemaVersion::V1Legacy,
            &SchemaVersion::V2Lsp,
        );
        assert!(!warnings.is_empty());
    }
}
