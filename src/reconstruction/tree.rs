//! Tree-based conversation representation and traversal.
//!
//! This module provides utilities for traversing and manipulating
//! conversation trees, including iterators and path finding.

use std::collections::VecDeque;

use super::{Conversation, ConversationNode};
use crate::model::LogEntry;

/// Iterator over conversation nodes in depth-first order.
pub struct DepthFirstIterator<'a> {
    conversation: &'a Conversation,
    stack: Vec<&'a str>,
}

impl<'a> DepthFirstIterator<'a> {
    /// Create a new depth-first iterator.
    pub fn new(conversation: &'a Conversation) -> Self {
        let stack = conversation.roots().iter().rev().map(String::as_str).collect();
        Self { conversation, stack }
    }
}

impl<'a> Iterator for DepthFirstIterator<'a> {
    type Item = &'a ConversationNode;

    fn next(&mut self) -> Option<Self::Item> {
        while let Some(uuid) = self.stack.pop() {
            if let Some(node) = self.conversation.get_node(uuid) {
                // Push children in reverse order so we process them in order
                for child_uuid in node.children.iter().rev() {
                    self.stack.push(child_uuid);
                }
                return Some(node);
            }
        }
        None
    }
}

/// Iterator over conversation nodes in breadth-first order.
pub struct BreadthFirstIterator<'a> {
    conversation: &'a Conversation,
    queue: VecDeque<&'a str>,
}

impl<'a> BreadthFirstIterator<'a> {
    /// Create a new breadth-first iterator.
    pub fn new(conversation: &'a Conversation) -> Self {
        let queue = conversation.roots().iter().map(String::as_str).collect();
        Self { conversation, queue }
    }
}

impl<'a> Iterator for BreadthFirstIterator<'a> {
    type Item = &'a ConversationNode;

    fn next(&mut self) -> Option<Self::Item> {
        while let Some(uuid) = self.queue.pop_front() {
            if let Some(node) = self.conversation.get_node(uuid) {
                for child_uuid in &node.children {
                    self.queue.push_back(child_uuid);
                }
                return Some(node);
            }
        }
        None
    }
}

/// Iterator over only the main thread nodes.
pub struct MainThreadIterator<'a> {
    conversation: &'a Conversation,
    index: usize,
}

impl<'a> MainThreadIterator<'a> {
    /// Create a new main thread iterator.
    pub fn new(conversation: &'a Conversation) -> Self {
        Self {
            conversation,
            index: 0,
        }
    }
}

impl<'a> Iterator for MainThreadIterator<'a> {
    type Item = &'a ConversationNode;

    fn next(&mut self) -> Option<Self::Item> {
        let main_thread = self.conversation.main_thread();
        if self.index >= main_thread.len() {
            return None;
        }

        let uuid = &main_thread[self.index];
        self.index += 1;
        self.conversation.get_node(uuid)
    }
}

/// A path through the conversation tree.
#[derive(Debug, Clone)]
pub struct ConversationPath {
    /// UUIDs in path order from root to leaf.
    pub uuids: Vec<String>,
}

impl ConversationPath {
    /// Create a new empty path.
    #[must_use]
    pub fn new() -> Self {
        Self { uuids: Vec::new() }
    }

    /// Create a path from UUIDs.
    #[must_use]
    pub fn from_uuids(uuids: Vec<String>) -> Self {
        Self { uuids }
    }

    /// Get the length of the path.
    #[must_use]
    pub fn len(&self) -> usize {
        self.uuids.len()
    }

    /// Check if the path is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.uuids.is_empty()
    }

    /// Get the root UUID.
    #[must_use]
    pub fn root(&self) -> Option<&str> {
        self.uuids.first().map(String::as_str)
    }

    /// Get the leaf UUID.
    #[must_use]
    pub fn leaf(&self) -> Option<&str> {
        self.uuids.last().map(String::as_str)
    }

    /// Check if a UUID is in the path.
    #[must_use]
    pub fn contains(&self, uuid: &str) -> bool {
        self.uuids.iter().any(|u| u == uuid)
    }
}

impl Default for ConversationPath {
    fn default() -> Self {
        Self::new()
    }
}

impl Conversation {
    /// Get a depth-first iterator over all nodes.
    pub fn iter_depth_first(&self) -> DepthFirstIterator<'_> {
        DepthFirstIterator::new(self)
    }

    /// Get a breadth-first iterator over all nodes.
    pub fn iter_breadth_first(&self) -> BreadthFirstIterator<'_> {
        BreadthFirstIterator::new(self)
    }

    /// Get an iterator over the main thread.
    pub fn iter_main_thread(&self) -> MainThreadIterator<'_> {
        MainThreadIterator::new(self)
    }

    /// Find the path from root to a specific node.
    #[must_use]
    pub fn path_to(&self, uuid: &str) -> Option<ConversationPath> {
        let mut path = Vec::new();
        let mut current = uuid.to_string();

        loop {
            path.push(current.clone());
            if let Some(node) = self.get_node(&current) {
                if let Some(parent) = &node.parent_uuid {
                    current = parent.clone();
                } else {
                    break;
                }
            } else {
                return None;
            }
        }

        path.reverse();
        Some(ConversationPath::from_uuids(path))
    }

    /// Find the common ancestor of two nodes.
    #[must_use]
    pub fn common_ancestor(&self, uuid1: &str, uuid2: &str) -> Option<String> {
        let path1 = self.path_to(uuid1)?;
        let path2 = self.path_to(uuid2)?;

        let mut common = None;
        for (u1, u2) in path1.uuids.iter().zip(path2.uuids.iter()) {
            if u1 == u2 {
                common = Some(u1.clone());
            } else {
                break;
            }
        }
        common
    }

    /// Get all leaf nodes (nodes with no children).
    #[must_use]
    pub fn leaves(&self) -> Vec<&ConversationNode> {
        self.nodes
            .values()
            .filter(|n| n.children.is_empty())
            .collect()
    }

    /// Get all nodes at a specific depth.
    #[must_use]
    pub fn nodes_at_depth(&self, depth: usize) -> Vec<&ConversationNode> {
        self.nodes.values().filter(|n| n.depth == depth).collect()
    }

    /// Get the subtree rooted at a specific node.
    #[must_use]
    pub fn subtree(&self, root_uuid: &str) -> Vec<&ConversationNode> {
        let mut result = Vec::new();
        let mut stack = vec![root_uuid];

        while let Some(uuid) = stack.pop() {
            if let Some(node) = self.get_node(uuid) {
                result.push(node);
                for child_uuid in &node.children {
                    stack.push(child_uuid);
                }
            }
        }

        result
    }

    /// Count nodes in subtree rooted at a specific node.
    #[must_use]
    pub fn subtree_size(&self, root_uuid: &str) -> usize {
        self.subtree(root_uuid).len()
    }

    /// Get all branch paths (alternative conversation branches).
    #[must_use]
    pub fn branch_paths(&self) -> Vec<ConversationPath> {
        let mut paths = Vec::new();

        for leaf in self.leaves() {
            if !leaf.is_main_thread {
                if let Some(path) = self.path_to(&leaf.uuid) {
                    paths.push(path);
                }
            }
        }

        paths
    }
}

/// A conversation turn (user message + assistant response).
#[derive(Debug, Clone)]
pub struct ConversationTurn<'a> {
    /// The user message (if present).
    pub user_message: Option<&'a LogEntry>,
    /// The assistant response (if present).
    pub assistant_message: Option<&'a LogEntry>,
    /// Tool uses in the assistant response.
    pub tool_uses: Vec<&'a crate::model::content::ToolUse>,
    /// Tool results from the user message.
    pub tool_results: Vec<&'a crate::model::content::ToolResult>,
}

impl Conversation {
    /// Extract conversation turns (user + assistant pairs).
    #[must_use]
    pub fn turns(&self) -> Vec<ConversationTurn<'_>> {
        let mut turns = Vec::new();
        let main_thread = self.main_thread_entries();

        let mut i = 0;
        while i < main_thread.len() {
            let entry = main_thread[i];

            match entry {
                LogEntry::User(_) => {
                    let mut turn = ConversationTurn {
                        user_message: Some(entry),
                        assistant_message: None,
                        tool_uses: Vec::new(),
                        tool_results: Vec::new(),
                    };

                    // Extract tool results from user message
                    if let LogEntry::User(user) = entry {
                        turn.tool_results = user.message.tool_results();
                    }

                    // Look for following assistant message
                    if i + 1 < main_thread.len() {
                        if let LogEntry::Assistant(assistant) = main_thread[i + 1] {
                            turn.assistant_message = Some(main_thread[i + 1]);
                            turn.tool_uses = assistant.message.tool_uses();
                            i += 1;
                        }
                    }

                    turns.push(turn);
                }
                LogEntry::Assistant(assistant) => {
                    // Orphan assistant message (no preceding user message)
                    turns.push(ConversationTurn {
                        user_message: None,
                        assistant_message: Some(entry),
                        tool_uses: assistant.message.tool_uses(),
                        tool_results: Vec::new(),
                    });
                }
                _ => {}
            }

            i += 1;
        }

        turns
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_conversation_path() {
        let path = ConversationPath::from_uuids(vec![
            "root".to_string(),
            "mid".to_string(),
            "leaf".to_string(),
        ]);

        assert_eq!(path.len(), 3);
        assert_eq!(path.root(), Some("root"));
        assert_eq!(path.leaf(), Some("leaf"));
        assert!(path.contains("mid"));
        assert!(!path.contains("other"));
    }
}
