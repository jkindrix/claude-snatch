//! Conversation rules extraction (BJ-017, BJ-018).
//!
//! Parses rule files from global and project rules/ directories.

use crate::error::Result;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Conversation rule definition.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Rule {
    /// Rule name (from filename).
    pub name: String,

    /// Rule description (from first line or frontmatter).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,

    /// Rule content.
    pub content: String,

    /// Rule type/category.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rule_type: Option<RuleType>,

    /// Source file path.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_path: Option<PathBuf>,

    /// Whether this rule is active.
    #[serde(default = "default_true")]
    pub active: bool,

    /// Priority (higher = applied first).
    #[serde(default)]
    pub priority: i32,
}

fn default_true() -> bool {
    true
}

/// Rule type/category.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RuleType {
    /// Always applied.
    Always,
    /// Applied conditionally.
    Conditional,
    /// Applied for specific file types.
    FileType,
    /// Applied for specific tools.
    Tool,
    /// Custom/unknown type.
    Custom,
}

impl Rule {
    /// Load all rules from a directory.
    pub fn load_from_dir(dir: &Path) -> Result<Vec<Self>> {
        if !dir.exists() {
            return Ok(Vec::new());
        }

        let mut rules = Vec::new();

        for entry in std::fs::read_dir(dir)? {
            let entry = entry?;
            let path = entry.path();

            if path.is_file() {
                let ext = path.extension().and_then(|e| e.to_str());
                if matches!(ext, Some("md") | Some("txt") | Some("rule")) {
                    if let Some(rule) = Self::from_file(&path) {
                        rules.push(rule);
                    }
                }
            }
        }

        // Sort by priority (descending), then by name
        rules.sort_by(|a, b| {
            b.priority.cmp(&a.priority).then_with(|| a.name.cmp(&b.name))
        });

        Ok(rules)
    }

    /// Load a single rule from a file.
    pub fn from_file(path: &Path) -> Option<Self> {
        let content = std::fs::read_to_string(path).ok()?;
        let name = path.file_stem()?.to_str()?.to_string();

        // Parse frontmatter and content
        let (description, rule_type, priority, active, body) = parse_rule_content(&content);

        Some(Self {
            name,
            description,
            content: body,
            rule_type,
            source_path: Some(path.to_path_buf()),
            active,
            priority,
        })
    }

    /// Check if this rule applies to a given file path.
    #[must_use]
    pub fn applies_to_file(&self, file_path: &str) -> bool {
        match &self.rule_type {
            Some(RuleType::FileType) => {
                // Check if rule content mentions this file type
                if let Some(ext) = Path::new(file_path).extension().and_then(|e| e.to_str()) {
                    self.content.contains(ext)
                } else {
                    false
                }
            }
            Some(RuleType::Always) | None => true,
            _ => false,
        }
    }

    /// Get the effective content with any preprocessing.
    #[must_use]
    pub fn effective_content(&self) -> &str {
        &self.content
    }
}

/// Parse rule content extracting metadata from frontmatter.
fn parse_rule_content(content: &str) -> (Option<String>, Option<RuleType>, i32, bool, String) {
    let trimmed = content.trim();

    // Check for YAML frontmatter
    if let Some(after_prefix) = trimmed.strip_prefix("---") {
        if let Some(end_idx) = after_prefix.find("---") {
            let frontmatter = &after_prefix[..end_idx];
            let body = after_prefix[end_idx + 3..].trim().to_string();

            let mut description = None;
            let mut rule_type = None;
            let mut priority = 0;
            let mut active = true;

            for line in frontmatter.lines() {
                if let Some(value) = line.strip_prefix("description:") {
                    description = Some(value.trim().trim_matches('"').to_string());
                } else if let Some(value) = line.strip_prefix("type:") {
                    rule_type = parse_rule_type(value.trim());
                } else if let Some(value) = line.strip_prefix("priority:") {
                    priority = value.trim().parse().unwrap_or(0);
                } else if let Some(value) = line.strip_prefix("active:") {
                    active = value.trim() != "false";
                }
            }

            return (description, rule_type, priority, active, body);
        }
    }

    // No frontmatter - extract description from first line
    let description = trimmed
        .lines()
        .next()
        .filter(|l| !l.starts_with('#'))
        .map(|l| l.trim().to_string());

    (description, None, 0, true, trimmed.to_string())
}

/// Parse rule type from string.
fn parse_rule_type(s: &str) -> Option<RuleType> {
    match s.to_lowercase().as_str() {
        "always" => Some(RuleType::Always),
        "conditional" => Some(RuleType::Conditional),
        "filetype" | "file-type" | "file_type" => Some(RuleType::FileType),
        "tool" => Some(RuleType::Tool),
        "custom" => Some(RuleType::Custom),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::tempdir;

    #[test]
    fn test_load_rules_from_dir() {
        let dir = tempdir().unwrap();

        // Create a test rule file
        let rule_path = dir.path().join("test-rule.md");
        let mut file = std::fs::File::create(&rule_path).unwrap();
        writeln!(file, "---").unwrap();
        writeln!(file, "description: Test rule description").unwrap();
        writeln!(file, "type: always").unwrap();
        writeln!(file, "priority: 10").unwrap();
        writeln!(file, "---").unwrap();
        writeln!(file, "This is the rule content.").unwrap();

        let rules = Rule::load_from_dir(dir.path()).unwrap();
        assert_eq!(rules.len(), 1);
        assert_eq!(rules[0].name, "test-rule");
        assert_eq!(rules[0].description, Some("Test rule description".to_string()));
        assert_eq!(rules[0].rule_type, Some(RuleType::Always));
        assert_eq!(rules[0].priority, 10);
        assert!(rules[0].active);
    }

    #[test]
    fn test_parse_rule_without_frontmatter() {
        let content = "This is a simple rule.\nWith multiple lines.";
        let (desc, rule_type, priority, active, body) = parse_rule_content(content);

        assert_eq!(desc, Some("This is a simple rule.".to_string()));
        assert!(rule_type.is_none());
        assert_eq!(priority, 0);
        assert!(active);
        assert!(body.contains("multiple lines"));
    }
}
