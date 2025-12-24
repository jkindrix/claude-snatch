//! Conversation reconstruction from JSONL entries.
//!
//! This module handles:
//! - Building conversation trees from parentUuid links
//! - Handling conversation branching/forking
//! - Preserving logicalParentUuid across compaction
//! - Grouping streaming chunks by message.id
//! - Identifying main threads vs sidechains
//! - Linking tool_use to corresponding tool_result

mod tree;
mod thread;

pub use tree::*;
pub use thread::*;

use std::collections::HashMap;

use indexmap::IndexMap;

use crate::error::Result;
use crate::model::{ContentBlock, LogEntry};

/// A node in the conversation tree.
#[derive(Debug, Clone)]
pub struct ConversationNode {
    /// The log entry at this node.
    pub entry: LogEntry,
    /// UUID of this node.
    pub uuid: String,
    /// Parent UUID (if any).
    pub parent_uuid: Option<String>,
    /// Child UUIDs.
    pub children: Vec<String>,
    /// Depth in the tree (0 = root).
    pub depth: usize,
    /// Whether this node is on the main thread.
    pub is_main_thread: bool,
    /// Whether this is a branch point (has multiple children).
    pub is_branch_point: bool,
}

impl ConversationNode {
    /// Create a new node from a log entry.
    pub fn new(entry: LogEntry, depth: usize) -> Option<Self> {
        let uuid = entry.uuid()?.to_string();
        let parent_uuid = entry.parent_uuid().map(String::from);

        Some(Self {
            entry,
            uuid,
            parent_uuid,
            children: Vec::new(),
            depth,
            is_main_thread: true,
            is_branch_point: false,
        })
    }
}

/// A reconstructed conversation with tree structure.
#[derive(Debug)]
pub struct Conversation {
    /// All nodes indexed by UUID.
    nodes: IndexMap<String, ConversationNode>,
    /// Root node UUIDs (nodes with no parent).
    roots: Vec<String>,
    /// The main thread (chronological order, following deepest path).
    main_thread: Vec<String>,
    /// Branch points where conversation forked.
    branch_points: Vec<String>,
    /// Tool use to tool result mapping.
    tool_links: HashMap<String, String>,
    /// Message ID groupings (streaming chunks).
    message_groups: HashMap<String, Vec<String>>,
}

impl Conversation {
    /// Build a conversation from log entries.
    pub fn from_entries(entries: Vec<LogEntry>) -> Result<Self> {
        let mut nodes = IndexMap::new();
        let mut uuid_to_index = HashMap::new();
        let mut roots = Vec::new();
        let mut tool_uses: HashMap<String, String> = HashMap::new(); // tool_use_id -> node_uuid
        let mut tool_links = HashMap::new();
        let mut message_groups: HashMap<String, Vec<String>> = HashMap::new();

        // First pass: create nodes and track tool uses
        for entry in entries {
            if let Some(uuid) = entry.uuid() {
                let uuid = uuid.to_string();
                let parent_uuid = entry.parent_uuid().map(String::from);
                let depth = 0; // Will calculate in second pass

                // Track message ID groups
                if let LogEntry::Assistant(ref assistant) = entry {
                    let msg_id = &assistant.message.id;
                    message_groups
                        .entry(msg_id.clone())
                        .or_default()
                        .push(uuid.clone());

                    // Track tool uses for linking
                    for content in &assistant.message.content {
                        if let ContentBlock::ToolUse(tool_use) = content {
                            tool_uses.insert(tool_use.id.clone(), uuid.clone());
                        }
                    }
                }

                // Track tool results and link back
                if let LogEntry::User(ref user) = entry {
                    for tool_result in user.message.tool_results() {
                        if let Some(tool_uuid) = tool_uses.get(&tool_result.tool_use_id) {
                            tool_links.insert(tool_result.tool_use_id.clone(), tool_uuid.clone());
                        }
                    }
                }

                if parent_uuid.is_none() {
                    roots.push(uuid.clone());
                }

                let node = ConversationNode {
                    entry,
                    uuid: uuid.clone(),
                    parent_uuid,
                    children: Vec::new(),
                    depth,
                    is_main_thread: true,
                    is_branch_point: false,
                };

                uuid_to_index.insert(uuid.clone(), nodes.len());
                nodes.insert(uuid, node);
            }
        }

        // Second pass: build parent-child relationships
        let node_keys: Vec<String> = nodes.keys().cloned().collect();
        for uuid in &node_keys {
            if let Some(node) = nodes.get(uuid) {
                if let Some(parent_uuid) = &node.parent_uuid {
                    let parent_uuid = parent_uuid.clone();
                    let uuid = uuid.clone();
                    if let Some(parent_node) = nodes.get_mut(&parent_uuid) {
                        parent_node.children.push(uuid);
                    }
                }
            }
        }

        // Third pass: calculate depths and identify main thread
        let mut main_thread = Vec::new();
        let mut branch_points = Vec::new();

        // Calculate depths via BFS from roots
        for root_uuid in &roots {
            let mut queue = vec![(root_uuid.clone(), 0)];

            while let Some((uuid, depth)) = queue.pop() {
                if let Some(node) = nodes.get_mut(&uuid) {
                    node.depth = depth;

                    if node.children.len() > 1 {
                        node.is_branch_point = true;
                        branch_points.push(uuid.clone());
                    }

                    for child_uuid in &node.children {
                        queue.push((child_uuid.clone(), depth + 1));
                    }
                }
            }
        }

        // Build main thread (follow chronological order and deepest path)
        if let Some(first_root) = roots.first() {
            let mut current = first_root.clone();
            main_thread.push(current.clone());

            loop {
                if let Some(node) = nodes.get(&current) {
                    if node.children.is_empty() {
                        break;
                    }

                    // Follow the chronologically first child (or latest for tie-break)
                    let next = node.children.first().cloned();
                    if let Some(next_uuid) = next {
                        main_thread.push(next_uuid.clone());
                        current = next_uuid;
                    } else {
                        break;
                    }
                } else {
                    break;
                }
            }
        }

        // Mark non-main-thread nodes
        let main_set: std::collections::HashSet<_> = main_thread.iter().cloned().collect();
        for (uuid, node) in &mut nodes {
            node.is_main_thread = main_set.contains(uuid);
        }

        Ok(Self {
            nodes,
            roots,
            main_thread,
            branch_points,
            tool_links,
            message_groups,
        })
    }

    /// Get all nodes.
    #[must_use]
    pub fn nodes(&self) -> &IndexMap<String, ConversationNode> {
        &self.nodes
    }

    /// Get a node by UUID.
    #[must_use]
    pub fn get_node(&self, uuid: &str) -> Option<&ConversationNode> {
        self.nodes.get(uuid)
    }

    /// Get root node UUIDs.
    #[must_use]
    pub fn roots(&self) -> &[String] {
        &self.roots
    }

    /// Get the main thread UUIDs in order.
    #[must_use]
    pub fn main_thread(&self) -> &[String] {
        &self.main_thread
    }

    /// Get branch point UUIDs.
    #[must_use]
    pub fn branch_points(&self) -> &[String] {
        &self.branch_points
    }

    /// Check if the conversation has branches.
    #[must_use]
    pub fn has_branches(&self) -> bool {
        !self.branch_points.is_empty()
    }

    /// Get the number of branch points in the conversation.
    #[must_use]
    pub fn branch_count(&self) -> usize {
        self.branch_points.len()
    }

    /// Get the tool result UUID for a tool use ID.
    #[must_use]
    pub fn tool_result_for(&self, tool_use_id: &str) -> Option<&str> {
        self.tool_links.get(tool_use_id).map(String::as_str)
    }

    /// Get all UUIDs that share a message ID (streaming chunks).
    #[must_use]
    pub fn message_group(&self, message_id: &str) -> Option<&[String]> {
        self.message_groups.get(message_id).map(Vec::as_slice)
    }

    /// Get the main thread as entries.
    #[must_use]
    pub fn main_thread_entries(&self) -> Vec<&LogEntry> {
        self.main_thread
            .iter()
            .filter_map(|uuid| self.nodes.get(uuid).map(|n| &n.entry))
            .collect()
    }

    /// Get all entries in chronological order (flattened tree).
    #[must_use]
    pub fn chronological_entries(&self) -> Vec<&LogEntry> {
        let mut entries: Vec<_> = self.nodes.values().map(|n| &n.entry).collect();
        entries.sort_by(|a, b| {
            match (a.timestamp(), b.timestamp()) {
                (Some(ta), Some(tb)) => ta.cmp(&tb),
                (Some(_), None) => std::cmp::Ordering::Less,
                (None, Some(_)) => std::cmp::Ordering::Greater,
                (None, None) => std::cmp::Ordering::Equal,
            }
        });
        entries
    }

    /// Get the depth of the deepest node.
    #[must_use]
    pub fn max_depth(&self) -> usize {
        self.nodes.values().map(|n| n.depth).max().unwrap_or(0)
    }

    /// Get node count.
    #[must_use]
    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    /// Check if empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }

    /// Get children of a node.
    #[must_use]
    pub fn children_of(&self, uuid: &str) -> Vec<&ConversationNode> {
        self.nodes
            .get(uuid)
            .map(|n| {
                n.children
                    .iter()
                    .filter_map(|c| self.nodes.get(c))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Get parent of a node.
    #[must_use]
    pub fn parent_of(&self, uuid: &str) -> Option<&ConversationNode> {
        self.nodes
            .get(uuid)
            .and_then(|n| n.parent_uuid.as_ref())
            .and_then(|p| self.nodes.get(p))
    }

    /// Get statistics about the conversation.
    #[must_use]
    pub fn statistics(&self) -> ConversationStats {
        let mut user_count = 0;
        let mut assistant_count = 0;
        let mut system_count = 0;
        let mut tool_use_count = 0;
        let mut tool_result_count = 0;
        let mut thinking_count = 0;

        for node in self.nodes.values() {
            match &node.entry {
                LogEntry::User(user) => {
                    user_count += 1;
                    tool_result_count += user.message.tool_results().len();
                }
                LogEntry::Assistant(assistant) => {
                    assistant_count += 1;
                    for content in &assistant.message.content {
                        match content {
                            ContentBlock::ToolUse(_) => tool_use_count += 1,
                            ContentBlock::Thinking(_) => thinking_count += 1,
                            _ => {}
                        }
                    }
                }
                LogEntry::System(_) => system_count += 1,
                _ => {}
            }
        }

        ConversationStats {
            total_nodes: self.nodes.len(),
            user_messages: user_count,
            assistant_messages: assistant_count,
            system_messages: system_count,
            tool_uses: tool_use_count,
            tool_results: tool_result_count,
            thinking_blocks: thinking_count,
            branch_count: self.branch_points.len(),
            max_depth: self.max_depth(),
            main_thread_length: self.main_thread.len(),
        }
    }
}

/// Statistics about a conversation.
#[derive(Debug, Clone, Default)]
pub struct ConversationStats {
    /// Total node count.
    pub total_nodes: usize,
    /// User message count.
    pub user_messages: usize,
    /// Assistant message count.
    pub assistant_messages: usize,
    /// System message count.
    pub system_messages: usize,
    /// Tool use count.
    pub tool_uses: usize,
    /// Tool result count.
    pub tool_results: usize,
    /// Thinking block count.
    pub thinking_blocks: usize,
    /// Number of branch points.
    pub branch_count: usize,
    /// Maximum tree depth.
    pub max_depth: usize,
    /// Main thread length.
    pub main_thread_length: usize,
}

impl ConversationStats {
    /// Check if tool uses and results are balanced.
    #[must_use]
    pub fn tools_balanced(&self) -> bool {
        self.tool_uses == self.tool_results
    }
}

/// Reconstruct a conversation from a session.
pub fn reconstruct_from_session(
    session: &crate::discovery::Session,
) -> Result<Conversation> {
    let entries = session.parse()?;
    Conversation::from_entries(entries)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use indexmap::IndexMap;
    use crate::model::{UserContent, UserMessage, UserSimpleContent};

    fn make_user_entry(uuid: &str, parent: Option<&str>) -> LogEntry {
        LogEntry::User(UserMessage {
            uuid: uuid.to_string(),
            parent_uuid: parent.map(String::from),
            timestamp: Utc::now(),
            session_id: "test".to_string(),
            version: "2.0.74".to_string(),
            cwd: None,
            git_branch: None,
            user_type: None,
            is_sidechain: false,
            is_teammate: None,
            agent_id: None,
            slug: None,
            is_meta: None,
            is_visible_in_transcript_only: None,
            thinking_metadata: None,
            todos: vec![],
            tool_use_result: None,
            message: UserContent::Simple(UserSimpleContent {
                role: "user".to_string(),
                content: "test".to_string(),
            }),
            extra: IndexMap::new(),
        })
    }

    #[test]
    fn test_simple_conversation() {
        let entries = vec![
            make_user_entry("1", None),
            make_user_entry("2", Some("1")),
            make_user_entry("3", Some("2")),
        ];

        let conv = Conversation::from_entries(entries).unwrap();

        assert_eq!(conv.len(), 3);
        assert_eq!(conv.roots().len(), 1);
        assert_eq!(conv.main_thread().len(), 3);
        assert!(!conv.has_branches());
    }

    #[test]
    fn test_branching_conversation() {
        let entries = vec![
            make_user_entry("1", None),
            make_user_entry("2a", Some("1")),
            make_user_entry("2b", Some("1")),
            make_user_entry("3", Some("2a")),
        ];

        let conv = Conversation::from_entries(entries).unwrap();

        assert_eq!(conv.len(), 4);
        assert!(conv.has_branches());
        assert_eq!(conv.branch_points().len(), 1);
    }
}
