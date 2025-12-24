//! Synthetic JSONL test data generators.
//!
//! This module provides utilities for generating synthetic Claude Code
//! conversation logs for testing purposes.

use chrono::{DateTime, Duration, Utc};
use serde::Serialize;
use std::io::Write;
use uuid::Uuid;

/// Configuration for generating synthetic sessions.
#[derive(Debug, Clone)]
pub struct SessionConfig {
    /// Number of message exchanges (user + assistant pairs).
    pub exchanges: usize,
    /// Include thinking blocks in assistant messages.
    pub include_thinking: bool,
    /// Include tool use in some messages.
    pub include_tools: bool,
    /// Average length of text content in characters.
    pub avg_text_length: usize,
    /// Average length of thinking content in characters.
    pub avg_thinking_length: usize,
    /// Session ID to use (auto-generated if None).
    pub session_id: Option<String>,
    /// Starting timestamp.
    pub start_time: DateTime<Utc>,
}

impl Default for SessionConfig {
    fn default() -> Self {
        Self {
            exchanges: 10,
            include_thinking: true,
            include_tools: true,
            avg_text_length: 200,
            avg_thinking_length: 500,
            session_id: None,
            start_time: Utc::now(),
        }
    }
}

impl SessionConfig {
    /// Create a minimal session config (small, fast generation).
    pub fn minimal() -> Self {
        Self {
            exchanges: 3,
            include_thinking: false,
            include_tools: false,
            avg_text_length: 50,
            avg_thinking_length: 0,
            ..Default::default()
        }
    }

    /// Create a large session config for stress testing.
    pub fn large() -> Self {
        Self {
            exchanges: 100,
            include_thinking: true,
            include_tools: true,
            avg_text_length: 500,
            avg_thinking_length: 2000,
            ..Default::default()
        }
    }

    /// Create a huge session config for extreme stress testing.
    pub fn huge() -> Self {
        Self {
            exchanges: 1000,
            include_thinking: true,
            include_tools: true,
            avg_text_length: 1000,
            avg_thinking_length: 5000,
            ..Default::default()
        }
    }
}

/// User message entry.
#[derive(Debug, Serialize)]
struct UserEntry {
    #[serde(rename = "type")]
    entry_type: String,
    uuid: String,
    #[serde(rename = "parentUuid")]
    parent_uuid: Option<String>,
    timestamp: String,
    #[serde(rename = "sessionId")]
    session_id: String,
    version: String,
    message: UserMessage,
}

#[derive(Debug, Serialize)]
struct UserMessage {
    role: String,
    content: String,
}

/// Assistant message entry.
#[derive(Debug, Serialize)]
struct AssistantEntry {
    #[serde(rename = "type")]
    entry_type: String,
    uuid: String,
    #[serde(rename = "parentUuid")]
    parent_uuid: Option<String>,
    timestamp: String,
    #[serde(rename = "sessionId")]
    session_id: String,
    version: String,
    message: AssistantMessage,
}

#[derive(Debug, Serialize)]
struct AssistantMessage {
    id: String,
    #[serde(rename = "type")]
    message_type: String,
    role: String,
    content: Vec<ContentBlock>,
    model: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    stop_reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    usage: Option<Usage>,
}

#[derive(Debug, Serialize)]
#[serde(tag = "type")]
enum ContentBlock {
    #[serde(rename = "text")]
    Text { text: String },
    #[serde(rename = "thinking")]
    Thinking { thinking: String, signature: String },
    #[serde(rename = "tool_use")]
    ToolUse {
        id: String,
        name: String,
        input: serde_json::Value,
    },
}

#[derive(Debug, Serialize)]
struct Usage {
    input_tokens: u32,
    output_tokens: u32,
    cache_creation_input_tokens: u32,
    cache_read_input_tokens: u32,
}

/// Generate synthetic session data and write to a writer.
pub fn generate_session<W: Write>(config: &SessionConfig, writer: &mut W) -> std::io::Result<()> {
    let session_id = config
        .session_id
        .clone()
        .unwrap_or_else(|| format!("test-session-{}", Uuid::new_v4()));

    let mut current_time = config.start_time;
    let mut parent_uuid: Option<String> = None;
    let mut message_count = 0;

    for exchange_idx in 0..config.exchanges {
        // Generate user message
        let user_uuid = Uuid::new_v4().to_string();
        let user_entry = UserEntry {
            entry_type: "user".to_string(),
            uuid: user_uuid.clone(),
            parent_uuid: parent_uuid.clone(),
            timestamp: current_time.to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
            session_id: session_id.clone(),
            version: "2.0.74".to_string(),
            message: UserMessage {
                role: "user".to_string(),
                content: generate_user_text(exchange_idx, config.avg_text_length),
            },
        };

        writeln!(writer, "{}", serde_json::to_string(&user_entry)?)?;
        parent_uuid = Some(user_uuid);
        current_time = current_time + Duration::seconds(1);
        message_count += 1;

        // Generate assistant message
        let assistant_uuid = Uuid::new_v4().to_string();
        let mut content = Vec::new();

        // Add thinking block if configured
        if config.include_thinking {
            content.push(ContentBlock::Thinking {
                thinking: generate_thinking_text(exchange_idx, config.avg_thinking_length),
                signature: generate_signature(exchange_idx),
            });
        }

        // Add tool use for some messages
        if config.include_tools && exchange_idx % 3 == 1 {
            content.push(ContentBlock::ToolUse {
                id: format!("toolu_{:03}", message_count),
                name: random_tool_name(exchange_idx),
                input: generate_tool_input(exchange_idx),
            });
        } else {
            content.push(ContentBlock::Text {
                text: generate_assistant_text(exchange_idx, config.avg_text_length),
            });
        }

        let assistant_entry = AssistantEntry {
            entry_type: "assistant".to_string(),
            uuid: assistant_uuid.clone(),
            parent_uuid: parent_uuid.clone(),
            timestamp: current_time.to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
            session_id: session_id.clone(),
            version: "2.0.74".to_string(),
            message: AssistantMessage {
                id: format!("msg_{:03}", message_count),
                message_type: "message".to_string(),
                role: "assistant".to_string(),
                content,
                model: "claude-sonnet-4-20250514".to_string(),
                stop_reason: Some(if config.include_tools && exchange_idx % 3 == 1 {
                    "tool_use".to_string()
                } else {
                    "end_turn".to_string()
                }),
                usage: Some(Usage {
                    input_tokens: (exchange_idx as u32 * 50 + 100),
                    output_tokens: (config.avg_text_length / 4) as u32,
                    cache_creation_input_tokens: 0,
                    cache_read_input_tokens: (exchange_idx as u32 * 10),
                }),
            },
        };

        writeln!(writer, "{}", serde_json::to_string(&assistant_entry)?)?;
        parent_uuid = Some(assistant_uuid);
        current_time = current_time + Duration::seconds(2);
        message_count += 1;
    }

    Ok(())
}

/// Generate a large test file with the specified number of sessions.
pub fn generate_large_file<W: Write>(
    num_sessions: usize,
    exchanges_per_session: usize,
    writer: &mut W,
) -> std::io::Result<()> {
    let mut config = SessionConfig {
        exchanges: exchanges_per_session,
        include_thinking: true,
        include_tools: true,
        avg_text_length: 300,
        avg_thinking_length: 1000,
        ..Default::default()
    };

    for i in 0..num_sessions {
        config.session_id = Some(format!("large-session-{:04}", i));
        config.start_time = Utc::now() - Duration::days(i as i64);
        generate_session(&config, writer)?;
    }

    Ok(())
}

// Helper functions for generating realistic content

fn generate_user_text(idx: usize, avg_length: usize) -> String {
    let templates = [
        "Can you help me understand {}?",
        "Please explain how {} works.",
        "I need assistance with {}.",
        "What is the best way to {}?",
        "Could you review this code for {}?",
        "How do I implement {} in this project?",
        "Can you fix the bug related to {}?",
        "Please add a feature for {}.",
    ];

    let topics = [
        "parsing JSONL files",
        "implementing the TUI",
        "handling errors gracefully",
        "optimizing performance",
        "writing unit tests",
        "documenting the API",
        "managing dependencies",
        "configuring the build",
    ];

    let template = templates[idx % templates.len()];
    let topic = topics[idx % topics.len()];
    let base = template.replace("{}", topic);

    // Pad to approximate desired length
    let padding = if base.len() < avg_length {
        format!(" Additional context: {}", "relevant details ".repeat((avg_length - base.len()) / 18))
    } else {
        String::new()
    };

    format!("{}{}", base, padding)
}

fn generate_assistant_text(idx: usize, avg_length: usize) -> String {
    let starters = [
        "I'd be happy to help with that.",
        "Great question!",
        "Let me explain.",
        "Here's how you can do that:",
        "I've analyzed the code and found:",
        "Based on my review:",
    ];

    let content_parts = [
        "The implementation follows best practices for Rust development.",
        "Consider using the standard library functions for this.",
        "Error handling is crucial here - use Result types.",
        "Performance can be improved by using iterators.",
        "Testing this functionality requires mocking the dependencies.",
        "Documentation should explain the public API clearly.",
    ];

    let starter = starters[idx % starters.len()];
    let content = content_parts[idx % content_parts.len()];
    let base = format!("{} {}", starter, content);

    // Pad to approximate desired length
    if base.len() < avg_length {
        let additional = "This approach ensures maintainability and follows Rust idioms. ";
        format!("{} {}", base, additional.repeat((avg_length - base.len()) / additional.len() + 1))
    } else {
        base
    }
}

fn generate_thinking_text(idx: usize, avg_length: usize) -> String {
    let thoughts = [
        "Let me analyze this request carefully.",
        "I need to consider the implications of this change.",
        "This involves multiple components that need coordination.",
        "I should check for edge cases and potential issues.",
        "The user wants to understand the underlying mechanism.",
    ];

    let analysis_parts = [
        "Looking at the code structure, I can see that the module handles data parsing.",
        "The current implementation uses a streaming approach which is efficient.",
        "There are several factors to consider: performance, maintainability, and correctness.",
        "The error handling could be improved to provide better user feedback.",
        "Testing this thoroughly requires both unit tests and integration tests.",
    ];

    let base = format!("{} {}", thoughts[idx % thoughts.len()], analysis_parts[idx % analysis_parts.len()]);

    // Pad to approximate desired length
    if base.len() < avg_length {
        let reasoning = "Considering the architecture and design patterns used in this codebase, \
                         the best approach would be to maintain consistency with existing patterns \
                         while introducing improvements where possible. ";
        format!("{}\n\n{}", base, reasoning.repeat((avg_length - base.len()) / reasoning.len() + 1))
    } else {
        base
    }
}

fn random_tool_name(idx: usize) -> String {
    let tools = ["Bash", "Read", "Write", "Edit", "Glob", "Grep", "Task"];
    tools[idx % tools.len()].to_string()
}

fn generate_tool_input(idx: usize) -> serde_json::Value {
    let tools_inputs = [
        serde_json::json!({"command": "ls -la", "description": "List directory contents"}),
        serde_json::json!({"file_path": "/path/to/file.rs"}),
        serde_json::json!({"file_path": "/path/to/output.txt", "content": "Generated content"}),
        serde_json::json!({"file_path": "/path/to/edit.rs", "old_string": "old", "new_string": "new"}),
        serde_json::json!({"pattern": "**/*.rs"}),
        serde_json::json!({"pattern": "fn main", "path": "src/"}),
        serde_json::json!({"prompt": "Search for implementation", "description": "Find code"}),
    ];
    tools_inputs[idx % tools_inputs.len()].clone()
}

fn generate_signature(idx: usize) -> String {
    // Generate a realistic-looking base64 signature (Claude uses cryptographic signatures)
    format!(
        "ErU{}{}{}{}",
        base64_char(idx),
        base64_char(idx * 7),
        base64_char(idx * 13),
        "A".repeat(40 + (idx % 20))
    )
}

fn base64_char(n: usize) -> char {
    const CHARS: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    CHARS[n % CHARS.len()] as char
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_generate_minimal_session() {
        let config = SessionConfig::minimal();
        let mut buffer = Vec::new();
        generate_session(&config, &mut buffer).unwrap();

        let content = String::from_utf8(buffer).unwrap();
        let lines: Vec<&str> = content.lines().collect();

        // 3 exchanges = 6 messages (user + assistant each)
        assert_eq!(lines.len(), 6);

        // Verify valid JSON
        for line in lines {
            assert!(serde_json::from_str::<serde_json::Value>(line).is_ok());
        }
    }

    #[test]
    fn test_generate_session_with_thinking() {
        let config = SessionConfig {
            exchanges: 2,
            include_thinking: true,
            include_tools: false,
            ..Default::default()
        };
        let mut buffer = Vec::new();
        generate_session(&config, &mut buffer).unwrap();

        let content = String::from_utf8(buffer).unwrap();
        assert!(content.contains("\"thinking\""));
    }

    #[test]
    fn test_generate_session_with_tools() {
        let config = SessionConfig {
            exchanges: 5,
            include_thinking: false,
            include_tools: true,
            ..Default::default()
        };
        let mut buffer = Vec::new();
        generate_session(&config, &mut buffer).unwrap();

        let content = String::from_utf8(buffer).unwrap();
        assert!(content.contains("\"tool_use\""));
    }

    #[test]
    fn test_generate_large_file() {
        let mut buffer = Vec::new();
        generate_large_file(3, 2, &mut buffer).unwrap();

        let content = String::from_utf8(buffer).unwrap();
        let lines: Vec<&str> = content.lines().collect();

        // 3 sessions * 2 exchanges * 2 messages = 12 lines
        assert_eq!(lines.len(), 12);
    }
}
