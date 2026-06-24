//! Conversation thread management and streaming chunk handling.
//!
//! This module handles:
//! - Grouping streaming chunks by message.id
//! - Reconstructing complete messages from chunks

use std::collections::HashMap;

use crate::model::{ContentBlock, LogEntry};

/// Groups streaming message chunks by their message.id.
#[derive(Debug, Default)]
pub struct MessageGrouper {
    /// Groups indexed by message ID.
    groups: HashMap<String, MessageGroup>,
    /// Processing order.
    order: Vec<String>,
}

impl MessageGrouper {
    /// Create a new grouper.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Add an entry to the appropriate group.
    pub fn add(&mut self, entry: LogEntry) {
        if let LogEntry::Assistant(ref assistant) = entry {
            let msg_id = assistant.message.id.clone();

            if !self.groups.contains_key(&msg_id) {
                self.order.push(msg_id.clone());
            }

            self.groups.entry(msg_id).or_default().add_chunk(entry);
        }
    }

    /// Process a batch of entries.
    pub fn add_all(&mut self, entries: impl IntoIterator<Item = LogEntry>) {
        for entry in entries {
            self.add(entry);
        }
    }

    /// Get all message groups in order.
    #[must_use]
    pub fn groups(&self) -> Vec<&MessageGroup> {
        self.order
            .iter()
            .filter_map(|id| self.groups.get(id))
            .collect()
    }

    /// Get a specific group by message ID.
    #[must_use]
    pub fn get_group(&self, message_id: &str) -> Option<&MessageGroup> {
        self.groups.get(message_id)
    }

    /// Get the number of groups.
    #[must_use]
    pub fn len(&self) -> usize {
        self.groups.len()
    }

    /// Check if empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.groups.is_empty()
    }

    /// Reconstruct complete messages from chunks.
    #[must_use]
    pub fn reconstruct(&self) -> Vec<ReconstructedMessage> {
        self.groups().iter().map(|g| g.reconstruct()).collect()
    }
}

/// A group of streaming chunks sharing a message.id.
#[derive(Debug)]
pub struct MessageGroup {
    /// All chunks in this group.
    chunks: Vec<LogEntry>,
    /// Combined content blocks.
    combined_content: Vec<ContentBlock>,
}

impl MessageGroup {
    /// Create a new empty group.
    #[must_use]
    pub fn new() -> Self {
        Self {
            chunks: Vec::new(),
            combined_content: Vec::new(),
        }
    }

    /// Add a chunk to this group.
    pub fn add_chunk(&mut self, entry: LogEntry) {
        if let LogEntry::Assistant(ref assistant) = entry {
            for content in &assistant.message.content {
                self.combined_content.push(content.clone());
            }
        }
        self.chunks.push(entry);
    }

    /// Get the number of chunks.
    #[must_use]
    pub fn chunk_count(&self) -> usize {
        self.chunks.len()
    }

    /// Get all chunks.
    #[must_use]
    pub fn chunks(&self) -> &[LogEntry] {
        &self.chunks
    }

    /// Get combined content blocks.
    #[must_use]
    pub fn content(&self) -> &[ContentBlock] {
        &self.combined_content
    }

    /// Get the first chunk (contains base metadata).
    #[must_use]
    pub fn first(&self) -> Option<&LogEntry> {
        self.chunks.first()
    }

    /// Get the last chunk (contains final state).
    #[must_use]
    pub fn last(&self) -> Option<&LogEntry> {
        self.chunks.last()
    }

    /// Reconstruct a complete message from chunks.
    #[must_use]
    pub fn reconstruct(&self) -> ReconstructedMessage {
        let first = self.first();
        let last = self.last();

        // Extract metadata from first chunk
        let (uuid, timestamp, session_id, version, model) =
            if let Some(LogEntry::Assistant(a)) = first {
                (
                    a.uuid.clone(),
                    a.timestamp,
                    a.session_id.clone(),
                    a.version.clone(),
                    a.message.model.clone(),
                )
            } else {
                (
                    String::new(),
                    chrono::Utc::now(),
                    String::new(),
                    String::new(),
                    String::new(),
                )
            };

        // Get final stop reason from last chunk
        let stop_reason = if let Some(LogEntry::Assistant(a)) = last {
            a.message.stop_reason.clone()
        } else {
            None
        };

        // Get final usage from last chunk
        let usage = if let Some(LogEntry::Assistant(a)) = last {
            a.message.usage.clone()
        } else {
            None
        };

        ReconstructedMessage {
            uuid,
            message_id: first
                .and_then(|e| {
                    if let LogEntry::Assistant(a) = e {
                        Some(a.message.id.clone())
                    } else {
                        None
                    }
                })
                .unwrap_or_default(),
            timestamp,
            session_id,
            version,
            model,
            content: self.combined_content.clone(),
            stop_reason,
            usage,
            chunk_count: self.chunks.len(),
        }
    }
}

impl Default for MessageGroup {
    fn default() -> Self {
        Self::new()
    }
}

/// A complete message reconstructed from streaming chunks.
#[derive(Debug, Clone)]
pub struct ReconstructedMessage {
    /// Entry UUID.
    pub uuid: String,
    /// API message ID.
    pub message_id: String,
    /// Timestamp.
    pub timestamp: chrono::DateTime<chrono::Utc>,
    /// Session ID.
    pub session_id: String,
    /// Claude Code version.
    pub version: String,
    /// Model used.
    pub model: String,
    /// Combined content blocks.
    pub content: Vec<ContentBlock>,
    /// Final stop reason.
    pub stop_reason: Option<crate::model::content::StopReason>,
    /// Final usage statistics.
    pub usage: Option<crate::model::usage::Usage>,
    /// Number of chunks combined.
    pub chunk_count: usize,
}

impl ReconstructedMessage {
    /// Get all text from the message.
    #[must_use]
    pub fn text(&self) -> String {
        self.content
            .iter()
            .filter_map(|c| {
                if let ContentBlock::Text(t) = c {
                    Some(t.text.as_str())
                } else {
                    None
                }
            })
            .collect::<Vec<_>>()
            .join("")
    }

    /// Get all thinking blocks.
    #[must_use]
    pub fn thinking(&self) -> Vec<&crate::model::content::ThinkingBlock> {
        self.content
            .iter()
            .filter_map(|c| {
                if let ContentBlock::Thinking(t) = c {
                    Some(t)
                } else {
                    None
                }
            })
            .collect()
    }

    /// Get all tool uses.
    #[must_use]
    pub fn tool_uses(&self) -> Vec<&crate::model::content::ToolUse> {
        self.content
            .iter()
            .filter_map(|c| {
                if let ContentBlock::ToolUse(t) = c {
                    Some(t)
                } else {
                    None
                }
            })
            .collect()
    }

    /// Check if this message contains thinking.
    #[must_use]
    pub fn has_thinking(&self) -> bool {
        self.content
            .iter()
            .any(|c| matches!(c, ContentBlock::Thinking(_)))
    }

    /// Check if this message contains tool uses.
    #[must_use]
    pub fn has_tool_use(&self) -> bool {
        self.content
            .iter()
            .any(|c| matches!(c, ContentBlock::ToolUse(_)))
    }
}

#[cfg(test)]
mod tests {
    // Test assertions compare exactly-representable float values (0.0, integer-valued
    // costs/scores); the float_cmp lint is a false positive for these.
    #![allow(clippy::float_cmp)]
    use super::*;
    use crate::model::{AssistantContent, AssistantMessage, SummaryMessage, TextBlock};
    use chrono::Utc;
    use indexmap::IndexMap;

    fn make_assistant_entry(msg_id: &str, uuid: &str, text: &str) -> LogEntry {
        LogEntry::Assistant(AssistantMessage {
            uuid: uuid.to_string(),
            parent_uuid: None,
            timestamp: Utc::now(),
            session_id: "test-session".to_string(),
            version: "1.0.0".to_string(),
            cwd: None,
            git_branch: None,
            user_type: None,
            is_sidechain: false,
            is_teammate: None,
            agent_id: None,
            slug: None,
            request_id: None,
            is_api_error_message: None,
            message: AssistantContent {
                id: msg_id.to_string(),
                msg_type: "message".to_string(),
                role: "assistant".to_string(),
                content: vec![ContentBlock::Text(TextBlock {
                    text: text.to_string(),
                    extra: IndexMap::new(),
                })],
                model: "test-model".to_string(),
                stop_reason: None,
                stop_sequence: None,
                usage: None,
                container: None,
                context_management: None,
                extra: IndexMap::new(),
            },
            extra: IndexMap::new(),
        })
    }

    #[test]
    fn test_message_grouper_new() {
        let grouper = MessageGrouper::new();
        assert!(grouper.is_empty());
        assert_eq!(grouper.len(), 0);
    }

    #[test]
    fn test_message_grouper_add() {
        let mut grouper = MessageGrouper::new();
        let entry = make_assistant_entry("msg-1", "uuid-1", "Hello");
        grouper.add(entry);

        assert!(!grouper.is_empty());
        assert_eq!(grouper.len(), 1);
        assert!(grouper.get_group("msg-1").is_some());
    }

    #[test]
    fn test_message_grouper_add_multiple_same_id() {
        let mut grouper = MessageGrouper::new();
        grouper.add(make_assistant_entry("msg-1", "uuid-1", "Hello"));
        grouper.add(make_assistant_entry("msg-1", "uuid-2", " world"));

        assert_eq!(grouper.len(), 1);
        let group = grouper.get_group("msg-1").unwrap();
        assert_eq!(group.chunk_count(), 2);
    }

    #[test]
    fn test_message_grouper_add_different_ids() {
        let mut grouper = MessageGrouper::new();
        grouper.add(make_assistant_entry("msg-1", "uuid-1", "First"));
        grouper.add(make_assistant_entry("msg-2", "uuid-2", "Second"));

        assert_eq!(grouper.len(), 2);
        assert!(grouper.get_group("msg-1").is_some());
        assert!(grouper.get_group("msg-2").is_some());
    }

    #[test]
    fn test_message_grouper_add_all() {
        let mut grouper = MessageGrouper::new();
        let entries = vec![
            make_assistant_entry("msg-1", "uuid-1", "First"),
            make_assistant_entry("msg-1", "uuid-2", " part"),
            make_assistant_entry("msg-2", "uuid-3", "Second"),
        ];
        grouper.add_all(entries);

        assert_eq!(grouper.len(), 2);
        assert_eq!(grouper.get_group("msg-1").unwrap().chunk_count(), 2);
        assert_eq!(grouper.get_group("msg-2").unwrap().chunk_count(), 1);
    }

    #[test]
    fn test_message_grouper_groups_order() {
        let mut grouper = MessageGrouper::new();
        grouper.add(make_assistant_entry("msg-a", "uuid-1", "A"));
        grouper.add(make_assistant_entry("msg-b", "uuid-2", "B"));
        grouper.add(make_assistant_entry("msg-c", "uuid-3", "C"));

        let groups = grouper.groups();
        assert_eq!(groups.len(), 3);
        // Order should be preserved
        assert_eq!(groups[0].chunk_count(), 1);
    }

    #[test]
    fn test_message_grouper_reconstruct() {
        let mut grouper = MessageGrouper::new();
        grouper.add(make_assistant_entry("msg-1", "uuid-1", "Hello"));
        grouper.add(make_assistant_entry("msg-1", "uuid-2", " world"));

        let messages = grouper.reconstruct();
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].text(), "Hello world");
        assert_eq!(messages[0].chunk_count, 2);
    }

    #[test]
    fn test_message_grouper_ignores_non_assistant() {
        let mut grouper = MessageGrouper::new();
        grouper.add(LogEntry::Summary(SummaryMessage {
            summary: "test".to_string(),
            leaf_uuid: None,
            is_compact_summary: None,
            extra: IndexMap::new(),
        }));

        assert!(grouper.is_empty());
    }

    #[test]
    fn test_message_group_new() {
        let group = MessageGroup::new();
        assert_eq!(group.chunk_count(), 0);
        assert!(group.chunks().is_empty());
        assert!(group.content().is_empty());
        assert!(group.first().is_none());
        assert!(group.last().is_none());
    }

    #[test]
    fn test_message_group_add_chunk() {
        let mut group = MessageGroup::new();
        group.add_chunk(make_assistant_entry("msg-1", "uuid-1", "Hello"));

        assert_eq!(group.chunk_count(), 1);
        assert_eq!(group.content().len(), 1);
        assert!(group.first().is_some());
        assert!(group.last().is_some());
    }

    #[test]
    fn test_message_group_default() {
        let group = MessageGroup::default();
        assert_eq!(group.chunk_count(), 0);
    }

    #[test]
    fn test_reconstructed_message_text() {
        let msg = ReconstructedMessage {
            uuid: "uuid".to_string(),
            message_id: "msg".to_string(),
            timestamp: Utc::now(),
            session_id: "session".to_string(),
            version: "1.0".to_string(),
            model: "model".to_string(),
            content: vec![
                ContentBlock::Text(TextBlock {
                    text: "Hello ".to_string(),
                    extra: IndexMap::new(),
                }),
                ContentBlock::Text(TextBlock {
                    text: "world".to_string(),
                    extra: IndexMap::new(),
                }),
            ],
            stop_reason: None,
            usage: None,
            chunk_count: 1,
        };

        assert_eq!(msg.text(), "Hello world");
    }

    #[test]
    fn test_reconstructed_message_has_thinking() {
        let msg = ReconstructedMessage {
            uuid: "uuid".to_string(),
            message_id: "msg".to_string(),
            timestamp: Utc::now(),
            session_id: "session".to_string(),
            version: "1.0".to_string(),
            model: "model".to_string(),
            content: vec![ContentBlock::Text(TextBlock {
                text: "text".to_string(),
                extra: IndexMap::new(),
            })],
            stop_reason: None,
            usage: None,
            chunk_count: 1,
        };

        assert!(!msg.has_thinking());
        assert!(!msg.has_tool_use());
    }
}
