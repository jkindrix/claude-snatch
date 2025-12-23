//! Conversation thread management and streaming chunk handling.
//!
//! This module handles:
//! - Grouping streaming chunks by message.id
//! - Reconstructing complete messages from chunks
//! - Managing retry chains for error recovery

use std::collections::HashMap;


use crate::model::{
    ContentBlock, LogEntry, SystemMessage,
    SystemSubtype,
};

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

            self.groups
                .entry(msg_id)
                .or_insert_with(MessageGroup::new)
                .add_chunk(entry);
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
        let (uuid, timestamp, session_id, version, model) = if let Some(LogEntry::Assistant(a)) = first
        {
            (
                a.uuid.clone(),
                a.timestamp,
                a.session_id.clone(),
                a.version.clone(),
                a.message.model.clone(),
            )
        } else {
            (String::new(), chrono::Utc::now(), String::new(), String::new(), String::new())
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
        self.content.iter().any(|c| matches!(c, ContentBlock::Thinking(_)))
    }

    /// Check if this message contains tool uses.
    #[must_use]
    pub fn has_tool_use(&self) -> bool {
        self.content.iter().any(|c| matches!(c, ContentBlock::ToolUse(_)))
    }
}

/// Tracks error recovery and retry chains.
#[derive(Debug, Default)]
pub struct RetryChainTracker {
    /// Retry chains indexed by original request UUID.
    chains: HashMap<String, RetryChain>,
}

impl RetryChainTracker {
    /// Create a new tracker.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Process entries and build retry chains.
    pub fn process(&mut self, entries: &[LogEntry]) {
        for entry in entries {
            if let LogEntry::System(system) = entry {
                if system.subtype == Some(SystemSubtype::ApiError) {
                    self.add_error(system);
                }
            }
        }
    }

    /// Add an API error to tracking.
    fn add_error(&mut self, error: &SystemMessage) {
        let uuid = error.uuid.clone();
        let parent_uuid = error.parent_uuid.clone();
        let retry_attempt = error.retry_attempt.unwrap_or(0);
        let max_retries = error.max_retries.unwrap_or(0);
        let retry_in_ms = error.retry_in_ms;

        // Find or create chain
        let chain_root = if retry_attempt == 1 {
            // First retry - create new chain from parent
            parent_uuid.clone().unwrap_or_else(|| uuid.clone())
        } else {
            // Subsequent retry - find existing chain
            self.find_chain_for(&uuid).unwrap_or_else(|| uuid.clone())
        };

        let chain = self.chains.entry(chain_root).or_insert_with(RetryChain::new);
        chain.add_attempt(RetryAttempt {
            uuid,
            retry_attempt,
            max_retries,
            retry_in_ms,
            timestamp: error.timestamp,
        });
    }

    /// Find which chain a UUID belongs to.
    fn find_chain_for(&self, uuid: &str) -> Option<String> {
        for (root, chain) in &self.chains {
            if chain.attempts.iter().any(|a| a.uuid == uuid) {
                return Some(root.clone());
            }
        }
        None
    }

    /// Get all retry chains.
    #[must_use]
    pub fn chains(&self) -> &HashMap<String, RetryChain> {
        &self.chains
    }

    /// Get retry chain starting from a UUID.
    #[must_use]
    pub fn chain_from(&self, uuid: &str) -> Option<&RetryChain> {
        self.chains.get(uuid)
    }

    /// Get statistics about retry behavior.
    #[must_use]
    pub fn statistics(&self) -> RetryStatistics {
        let total_chains = self.chains.len();
        let total_retries: usize = self.chains.values().map(|c| c.attempts.len()).sum();
        let max_retries_seen = self
            .chains
            .values()
            .map(|c| c.attempts.len())
            .max()
            .unwrap_or(0);
        let successful_recoveries = self.chains.values().filter(|c| c.succeeded).count();

        RetryStatistics {
            total_chains,
            total_retries,
            max_retries_seen,
            successful_recoveries,
        }
    }
}

/// A chain of retry attempts.
#[derive(Debug)]
pub struct RetryChain {
    /// Individual retry attempts.
    pub attempts: Vec<RetryAttempt>,
    /// Whether the chain ended in success.
    pub succeeded: bool,
}

impl RetryChain {
    /// Create a new empty chain.
    #[must_use]
    pub fn new() -> Self {
        Self {
            attempts: Vec::new(),
            succeeded: false,
        }
    }

    /// Add a retry attempt.
    pub fn add_attempt(&mut self, attempt: RetryAttempt) {
        self.attempts.push(attempt);
    }

    /// Get the number of attempts.
    #[must_use]
    pub fn attempt_count(&self) -> usize {
        self.attempts.len()
    }

    /// Get total retry delay in milliseconds.
    #[must_use]
    pub fn total_delay_ms(&self) -> f64 {
        self.attempts
            .iter()
            .filter_map(|a| a.retry_in_ms)
            .sum()
    }
}

impl Default for RetryChain {
    fn default() -> Self {
        Self::new()
    }
}

/// A single retry attempt.
#[derive(Debug, Clone)]
pub struct RetryAttempt {
    /// UUID of this attempt.
    pub uuid: String,
    /// Retry attempt number.
    pub retry_attempt: u32,
    /// Maximum retries allowed.
    pub max_retries: u32,
    /// Milliseconds until next retry.
    pub retry_in_ms: Option<f64>,
    /// Timestamp of attempt.
    pub timestamp: chrono::DateTime<chrono::Utc>,
}

/// Statistics about retry behavior.
#[derive(Debug, Clone, Default)]
pub struct RetryStatistics {
    /// Total number of retry chains.
    pub total_chains: usize,
    /// Total number of retry attempts.
    pub total_retries: usize,
    /// Maximum retries in any single chain.
    pub max_retries_seen: usize,
    /// Number of chains that eventually succeeded.
    pub successful_recoveries: usize,
}

impl RetryStatistics {
    /// Calculate success rate.
    #[must_use]
    pub fn success_rate(&self) -> f64 {
        if self.total_chains == 0 {
            return 0.0;
        }
        (self.successful_recoveries as f64 / self.total_chains as f64) * 100.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_message_grouper() {
        // Would need actual entries to test fully
        let grouper = MessageGrouper::new();
        assert!(grouper.is_empty());
        assert_eq!(grouper.len(), 0);
    }

    #[test]
    fn test_retry_statistics() {
        let stats = RetryStatistics {
            total_chains: 10,
            total_retries: 25,
            max_retries_seen: 5,
            successful_recoveries: 8,
        };

        assert!((stats.success_rate() - 80.0).abs() < 0.001);
    }
}
