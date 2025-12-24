//! Custom slash command extraction (BJ-007, BJ-008).
//!
//! Parses custom commands from the commands/ directory.

use crate::error::Result;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Custom slash command definition.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CustomCommand {
    /// Command name (from filename, without extension).
    pub name: String,

    /// Command description (first line or frontmatter).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,

    /// Full command template content.
    pub content: String,

    /// Source file path.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_path: Option<PathBuf>,

    /// File extension (md, txt, etc.).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub extension: Option<String>,
}

impl CustomCommand {
    /// Load all custom commands from a directory.
    pub fn load_from_dir(dir: &Path) -> Result<Vec<Self>> {
        if !dir.exists() {
            return Ok(Vec::new());
        }

        let mut commands = Vec::new();

        // Read all .md and .txt files in the directory
        for entry in std::fs::read_dir(dir)? {
            let entry = entry?;
            let path = entry.path();

            if path.is_file() {
                let ext = path.extension().and_then(|e| e.to_str());
                if matches!(ext, Some("md") | Some("txt")) {
                    if let Some(cmd) = Self::from_file(&path) {
                        commands.push(cmd);
                    }
                }
            }
        }

        // Sort by name for consistent ordering
        commands.sort_by(|a, b| a.name.cmp(&b.name));

        Ok(commands)
    }

    /// Load a single command from a file.
    pub fn from_file(path: &Path) -> Option<Self> {
        let content = std::fs::read_to_string(path).ok()?;
        let name = path.file_stem()?.to_str()?.to_string();
        let extension = path.extension().and_then(|e| e.to_str()).map(String::from);

        // Extract description from first line or frontmatter
        let description = extract_description(&content);

        Some(Self {
            name,
            description,
            content,
            source_path: Some(path.to_path_buf()),
            extension,
        })
    }

    /// Get the command invocation string (e.g., "/my-command").
    #[must_use]
    pub fn invocation(&self) -> String {
        format!("/{}", self.name)
    }

    /// Check if this command has arguments placeholders.
    #[must_use]
    pub fn has_arguments(&self) -> bool {
        self.content.contains("$ARGUMENTS")
            || self.content.contains("{{arguments}}")
            || self.content.contains("{args}")
    }

    /// Get estimated complexity based on content length.
    #[must_use]
    pub fn complexity(&self) -> CommandComplexity {
        let line_count = self.content.lines().count();
        let char_count = self.content.len();

        if line_count > 100 || char_count > 5000 {
            CommandComplexity::Complex
        } else if line_count > 20 || char_count > 1000 {
            CommandComplexity::Moderate
        } else {
            CommandComplexity::Simple
        }
    }
}

/// Command complexity level.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CommandComplexity {
    /// Simple command (<20 lines).
    Simple,
    /// Moderate command (20-100 lines).
    Moderate,
    /// Complex command (>100 lines).
    Complex,
}

/// Extract description from command content.
fn extract_description(content: &str) -> Option<String> {
    let trimmed = content.trim();

    // Check for YAML frontmatter
    if trimmed.starts_with("---") {
        if let Some(end) = trimmed[3..].find("---") {
            let frontmatter = &trimmed[3..3 + end];
            for line in frontmatter.lines() {
                if let Some(desc) = line.strip_prefix("description:") {
                    return Some(desc.trim().trim_matches('"').to_string());
                }
            }
        }
    }

    // Check for HTML comment description
    if trimmed.starts_with("<!--") {
        if let Some(end) = trimmed.find("-->") {
            let comment = &trimmed[4..end].trim();
            if !comment.is_empty() {
                return Some(comment.to_string());
            }
        }
    }

    // Use first non-empty line that's not a heading
    for line in trimmed.lines() {
        let line = line.trim();
        if !line.is_empty() && !line.starts_with('#') && !line.starts_with("---") {
            // Truncate long first lines
            let desc = if line.len() > 100 {
                format!("{}...", &line[..97])
            } else {
                line.to_string()
            };
            return Some(desc);
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::tempdir;

    #[test]
    fn test_load_commands_from_dir() {
        let dir = tempdir().unwrap();

        // Create a test command file
        let cmd_path = dir.path().join("test-cmd.md");
        let mut file = std::fs::File::create(&cmd_path).unwrap();
        writeln!(file, "# Test Command").unwrap();
        writeln!(file, "This is a test command template.").unwrap();

        let commands = CustomCommand::load_from_dir(dir.path()).unwrap();
        assert_eq!(commands.len(), 1);
        assert_eq!(commands[0].name, "test-cmd");
        assert_eq!(commands[0].invocation(), "/test-cmd");
    }

    #[test]
    fn test_description_extraction() {
        // From first line
        let desc = extract_description("A simple description\nMore content");
        assert_eq!(desc, Some("A simple description".to_string()));

        // From frontmatter
        let desc = extract_description("---\ndescription: \"My command\"\n---\nContent");
        assert_eq!(desc, Some("My command".to_string()));

        // From HTML comment
        let desc = extract_description("<!-- My description -->\nContent");
        assert_eq!(desc, Some("My description".to_string()));
    }

    #[test]
    fn test_command_complexity() {
        let simple = CustomCommand {
            name: "test".to_string(),
            description: None,
            content: "Short content".to_string(),
            source_path: None,
            extension: None,
        };
        assert_eq!(simple.complexity(), CommandComplexity::Simple);
    }
}
