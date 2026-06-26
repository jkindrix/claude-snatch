//! Conversation reconstruction from JSONL entries.
//!
//! This module handles:
//! - Building conversation trees from parentUuid links
//! - Handling conversation branching/forking
//! - Preserving logicalParentUuid across compaction
//! - Grouping streaming chunks by message.id
//! - Identifying main threads vs sidechains
//! - Linking tool_use to corresponding tool_result
//!
//! # Example
//!
//! ```rust,no_run
//! use claude_snatch::reconstruction::Conversation;
//! use claude_snatch::parser::JsonlParser;
//!
//! fn main() -> claude_snatch::Result<()> {
//!     // Parse a session file
//!     let mut parser = JsonlParser::new();
//!     let entries = parser.parse_file("session.jsonl")?;
//!
//!     // Reconstruct the conversation tree
//!     let conversation = Conversation::from_entries(entries)?;
//!
//!     // Access conversation statistics
//!     println!("Total messages: {}", conversation.len());
//!     println!("Has branches: {}", conversation.has_branches());
//!     println!("Branch count: {}", conversation.branch_count());
//!
//!     // Get main thread entries
//!     for entry in conversation.main_thread_entries() {
//!         println!("Entry: {:?}", entry.uuid());
//!     }
//!
//!     Ok(())
//! }
//! ```

mod thread;
mod tree;

pub use thread::*;
pub use tree::*;

use std::collections::HashMap;

use indexmap::IndexMap;
use tracing::{debug, instrument, trace, warn};

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
    /// Uuid-less entries (compaction summaries, file-history snapshots, sidecar
    /// metadata such as last-prompt / mode / ai-title / permission-mode, plus
    /// queue-operation and turn-end markers). These carry no UUID and are
    /// therefore absent from the node tree. Retained in original input order so
    /// exporters can surface them. With them retained, `-f jsonl` is
    /// content-complete for every successfully-parsed, non-conflicting entry —
    /// but it is not byte-faithful (fields may reorder, orphans are emitted
    /// first, and unparseable or content-conflicting duplicate lines are
    /// dropped). Use `-f raw-jsonl` for a byte-for-byte archive.
    orphan_entries: Vec<LogEntry>,
    /// Entries dropped because their UUID already appeared. The first occurrence
    /// is kept; later duplicates are recorded here (never silently overwritten).
    /// This mainly surfaces when combining files (resume chains / subagents).
    duplicate_uuids: Vec<DuplicateUuid>,
}

/// A dropped duplicate-UUID entry, kept for diagnostics.
///
/// Duplicate UUIDs come in two flavours with very different significance:
/// an *exact* duplicate (byte-identical content to the kept first occurrence)
/// is benign — e.g. an overlapping boundary entry shared by two files in a
/// resume chain, where dropping the copy loses nothing. A *content conflict*
/// (same UUID, differing content) is a genuine collision where the dropped
/// entry's data would otherwise be unrecoverable, so the full entry is retained
/// here and surfaced loudly to consumers.
#[derive(Debug, Clone)]
pub struct DuplicateUuid {
    /// The UUID that was seen more than once.
    pub uuid: String,
    /// Which occurrence this was (2 = the second time the UUID appeared, etc.).
    pub occurrence: usize,
    /// Whether the dropped occurrence was byte-identical (by serialized content)
    /// to the kept first occurrence. Exact duplicates are benign; differing
    /// content is a real collision.
    pub is_exact: bool,
    /// The dropped (later) entry, retained so a content conflict is recoverable
    /// rather than silently lost.
    pub dropped: LogEntry,
}

impl Conversation {
    /// Build a conversation from log entries.
    #[instrument(skip(entries), fields(entry_count = entries.len()))]
    pub fn from_entries(entries: Vec<LogEntry>) -> Result<Self> {
        debug!("Building conversation tree");
        let mut nodes: IndexMap<String, ConversationNode> = IndexMap::new();
        let mut roots = Vec::new();
        let mut tool_uses: HashMap<String, String> = HashMap::new(); // tool_use_id -> node_uuid
        let mut tool_links = HashMap::new();
        let mut message_groups: HashMap<String, Vec<String>> = HashMap::new();
        let mut orphan_entries: Vec<LogEntry> = Vec::new();
        let mut duplicate_uuids: Vec<DuplicateUuid> = Vec::new();

        // First pass: create nodes and track tool uses
        for entry in entries {
            if let Some(uuid) = entry.uuid() {
                let uuid = uuid.to_string();
                // Keep the first occurrence; record later duplicates rather than
                // silently overwriting (which would drop the first entry's data).
                // Classify exact duplicates (benign chain overlap) vs content
                // conflicts (same UUID, differing content) so consumers can stay
                // quiet about the former and loud about the latter.
                if let Some(existing) = nodes.get(&uuid) {
                    let is_exact = serde_json::to_string(&existing.entry).ok()
                        == serde_json::to_string(&entry).ok();
                    let occurrence = duplicate_uuids.iter().filter(|d| d.uuid == uuid).count() + 2;
                    duplicate_uuids.push(DuplicateUuid {
                        uuid,
                        occurrence,
                        is_exact,
                        dropped: entry,
                    });
                    continue;
                }
                // Use logicalParentUuid to bridge compaction boundaries:
                // When parentUuid is null but logicalParentUuid exists (compact_boundary),
                // use the logical parent to maintain a continuous main thread.
                let parent_uuid = entry
                    .parent_uuid()
                    .or_else(|| entry.logical_parent_uuid())
                    .map(String::from);
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

                nodes.insert(uuid, node);
            } else {
                // No UUID, so it never becomes a node. Retain every uuid-less
                // entry type (summary, file-history-snapshot, last-prompt, mode,
                // ai-title, permission-mode, queue-operation, turn-end, and any
                // unmodeled `Unknown` line) in input order so exporters can
                // surface them and `-f jsonl` round-trips by content. They are
                // re-emitted ahead of the uuid-bearing thread because most carry
                // no timestamp to interleave by (see `entries_for_export`).
                orphan_entries.push(entry);
            }
        }

        // Cycle break: parentUuid edges form a forest, but the logicalParentUuid
        // fallback used above to bridge compaction boundaries can point back into a
        // node's own descendant chain, wiring a cycle into the tree. Any walk over a
        // cyclic graph fails to terminate — recursive walks overflow the stack,
        // iterative walks loop forever. Each node has at most one parent, so the
        // parent graph is functional and every component holds at most one cycle.
        // Detect each cycle and cut it at a logical-derived edge (one whose real
        // parentUuid is absent), promoting that node to a root.
        {
            let parents: HashMap<String, String> = nodes
                .iter()
                .filter_map(|(u, n)| n.parent_uuid.clone().map(|p| (u.clone(), p)))
                .collect();
            let mut settled: std::collections::HashSet<String> = std::collections::HashSet::new();
            let mut cuts: Vec<String> = Vec::new();
            for start in nodes.keys() {
                if settled.contains(start) {
                    continue;
                }
                let mut path: Vec<String> = Vec::new();
                let mut on_path: std::collections::HashSet<String> =
                    std::collections::HashSet::new();
                let mut cur = start.clone();
                loop {
                    if settled.contains(&cur) {
                        break;
                    }
                    if !on_path.insert(cur.clone()) {
                        // `cur` repeats: the cycle is path[idx..] where path[idx] == cur.
                        let idx = path.iter().position(|x| *x == cur).unwrap_or(0);
                        let victim = path[idx..]
                            .iter()
                            .find(|u| {
                                nodes
                                    .get(*u)
                                    .is_some_and(|n| n.entry.parent_uuid().is_none())
                            })
                            .cloned()
                            .unwrap_or_else(|| cur.clone());
                        cuts.push(victim);
                        break;
                    }
                    path.push(cur.clone());
                    match parents.get(&cur) {
                        Some(p) => cur = p.clone(),
                        None => break,
                    }
                }
                for u in path {
                    settled.insert(u);
                }
            }
            for u in cuts {
                if let Some(n) = nodes.get_mut(&u) {
                    n.parent_uuid = None;
                }
                if !roots.contains(&u) {
                    roots.push(u);
                }
            }
        }

        // Second pass: build parent-child relationships and promote orphans to roots
        let node_keys: Vec<String> = nodes.keys().cloned().collect();
        for uuid in &node_keys {
            if let Some(node) = nodes.get(uuid) {
                if let Some(parent_uuid) = &node.parent_uuid {
                    let parent_uuid = parent_uuid.clone();
                    let uuid = uuid.clone();
                    if let Some(parent_node) = nodes.get_mut(&parent_uuid) {
                        parent_node.children.push(uuid);
                    } else {
                        // Parent was skipped during parsing (e.g. unknown entry type
                        // like "progress"). Promote this node to a root so it's
                        // reachable in the tree.
                        roots.push(uuid);
                    }
                }
            }
        }

        // Third pass: calculate depths and identify branch points
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

        // Score each subtree by the number of *conversational* nodes it
        // contains, for main thread selection. Progress notifications
        // (bash_progress/hook_progress/agent_progress) form long dead-end
        // sibling chains off an assistant node during a long-running tool
        // call. Counting them lets a progress chain outweigh the real
        // continuation at a fork, so the walk follows the progress chain and
        // dead-ends — silently dropping the rest of the conversation.
        // Weighting progress nodes as 0 keeps the walk on the conversation.
        fn is_conversational(entry: &LogEntry) -> bool {
            !matches!(entry, LogEntry::Progress(_))
        }
        // Iterative post-order traversal (children before parent). A recursive
        // walk recurses once per child and so reaches a depth equal to the
        // chain length; a long linear conversation (thousands of messages)
        // overflows the stack. The explicit stack keeps the depth in heap.
        let mut subtree_scores: HashMap<String, usize> = HashMap::new();
        for start in nodes.keys() {
            if subtree_scores.contains_key(start) {
                continue;
            }
            let mut stack: Vec<(&str, bool)> = vec![(start.as_str(), false)];
            while let Some((uuid, processed)) = stack.pop() {
                if subtree_scores.contains_key(uuid) {
                    continue;
                }
                if processed {
                    let size = nodes.get(uuid).map_or(0, |node| {
                        let self_weight = usize::from(is_conversational(&node.entry));
                        self_weight
                            + node
                                .children
                                .iter()
                                .map(|c| subtree_scores.get(c.as_str()).copied().unwrap_or(0))
                                .sum::<usize>()
                    });
                    subtree_scores.insert(uuid.to_string(), size);
                } else {
                    stack.push((uuid, true));
                    if let Some(node) = nodes.get(uuid) {
                        for c in &node.children {
                            if !subtree_scores.contains_key(c.as_str()) {
                                stack.push((c.as_str(), false));
                            }
                        }
                    }
                }
            }
        }

        // Latest timestamp among *conversational* nodes in each subtree.
        // Recency distinguishes the canonical (active) branch from an abandoned
        // edit/retry branch at a fork: the abandoned branch can be larger but is
        // older. It also reinforces the progress fix — a progress dead-end has no
        // conversational descendants, so it scores None and loses to the real
        // continuation, which is always newer than the progress events.
        // Iterative post-order traversal, same rationale as subtree_scores above.
        let mut latest_conv_ts: HashMap<String, Option<chrono::DateTime<chrono::Utc>>> =
            HashMap::new();
        for start in nodes.keys() {
            if latest_conv_ts.contains_key(start) {
                continue;
            }
            let mut stack: Vec<(&str, bool)> = vec![(start.as_str(), false)];
            while let Some((uuid, processed)) = stack.pop() {
                if latest_conv_ts.contains_key(uuid) {
                    continue;
                }
                if processed {
                    let ts = nodes.get(uuid).and_then(|node| {
                        let own = if is_conversational(&node.entry) {
                            node.entry.timestamp()
                        } else {
                            None
                        };
                        let child_max = node
                            .children
                            .iter()
                            .filter_map(|c| latest_conv_ts.get(c.as_str()).copied().flatten())
                            .max();
                        own.into_iter().chain(child_max).max()
                    });
                    latest_conv_ts.insert(uuid.to_string(), ts);
                } else {
                    stack.push((uuid, true));
                    if let Some(node) = nodes.get(uuid) {
                        for c in &node.children {
                            if !latest_conv_ts.contains_key(c.as_str()) {
                                stack.push((c.as_str(), false));
                            }
                        }
                    }
                }
            }
        }
        // Selection key: most recent conversational activity first, subtree size
        // as a tiebreak.
        let select_key = |uuid: &str| {
            (
                latest_conv_ts.get(uuid).copied().flatten(),
                subtree_scores.get(uuid).copied().unwrap_or(0),
            )
        };

        // Start from the root whose subtree holds the most conversational
        // nodes. When dropped entries fragment the tree into several roots,
        // this keeps the main thread on the largest real fragment rather than
        // an arbitrary first root.
        // Roots that hold conversational content, ordered by activity so older
        // fragments come first. When a dropped logical parent fragments the tree
        // (e.g. a compaction boundary whose parent line was lost), walking every
        // conversational root — not just the most recent — keeps the otherwise
        // orphaned preamble on the main thread. Pure-progress roots are excluded.
        let mut conv_roots: Vec<String> = roots
            .iter()
            .filter(|r| latest_conv_ts.get(*r).copied().flatten().is_some())
            .cloned()
            .collect();
        if conv_roots.is_empty() {
            conv_roots = roots
                .iter()
                .max_by_key(|r| select_key(r))
                .cloned()
                .into_iter()
                .collect();
        }
        conv_roots.sort_by_key(|r| latest_conv_ts.get(r).copied().flatten());

        // Build the main thread by walking each root in turn, following the
        // child with the most recent conversational activity (subtree size
        // breaks ties). A visited set guards against revisits/cycles.
        let mut visited: std::collections::HashSet<String> = std::collections::HashSet::new();
        for root in conv_roots {
            let mut current = root;
            if !visited.insert(current.clone()) {
                continue;
            }
            main_thread.push(current.clone());
            loop {
                let Some(node) = nodes.get(&current) else {
                    break;
                };
                if node.children.is_empty() {
                    break;
                }
                let Some(next_uuid) = node.children.iter().max_by_key(|c| select_key(c)).cloned()
                else {
                    break;
                };
                if !visited.insert(next_uuid.clone()) {
                    break;
                }
                main_thread.push(next_uuid.clone());
                current = next_uuid;
            }
        }

        // Mark non-main-thread nodes
        let main_set: std::collections::HashSet<_> = main_thread.iter().cloned().collect();
        for (uuid, node) in &mut nodes {
            node.is_main_thread = main_set.contains(uuid);
        }

        debug!(
            nodes = nodes.len(),
            roots = roots.len(),
            main_thread_len = main_thread.len(),
            branches = branch_points.len(),
            "Conversation tree built"
        );
        trace!(
            tool_links = tool_links.len(),
            message_groups = message_groups.len(),
            "Tool and message linkage complete"
        );

        if !duplicate_uuids.is_empty() {
            let conflicts = duplicate_uuids.iter().filter(|d| !d.is_exact).count();
            if conflicts > 0 {
                warn!(
                    conflicts,
                    total = duplicate_uuids.len(),
                    "Dropped content-conflicting duplicate-UUID entries (kept first occurrence)"
                );
            } else {
                debug!(
                    count = duplicate_uuids.len(),
                    "Deduplicated exact-duplicate UUID entries (benign overlap)"
                );
            }
        }

        Ok(Self {
            nodes,
            roots,
            main_thread,
            branch_points,
            tool_links,
            message_groups,
            orphan_entries,
            duplicate_uuids,
        })
    }

    /// Entries dropped because their UUID already appeared (first kept).
    #[must_use]
    pub fn duplicate_uuids(&self) -> &[DuplicateUuid] {
        &self.duplicate_uuids
    }

    /// Duplicate-UUID entries whose content differed from the kept first
    /// occurrence — genuine collisions, not benign chain overlap.
    pub fn conflicting_duplicates(&self) -> impl Iterator<Item = &DuplicateUuid> {
        self.duplicate_uuids.iter().filter(|d| !d.is_exact)
    }

    /// A one-line human-facing notice if any duplicate-UUID entries had
    /// *differing* content (a real collision worth surfacing). Returns `None`
    /// when there are no duplicates or only benign exact duplicates, so callers
    /// can print it unconditionally without crying wolf over chain overlap.
    #[must_use]
    pub fn duplicate_notice(&self) -> Option<String> {
        let conflicts = self.conflicting_duplicates().count();
        if conflicts == 0 {
            return None;
        }
        Some(format!(
            "⚠ {conflicts} duplicate-UUID entr{} with differing content dropped (kept first occurrence)",
            if conflicts == 1 { "y" } else { "ies" }
        ))
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

    /// Number of distinct assistant turns (message IDs).
    ///
    /// One assistant turn can be written as several JSONL lines (streaming
    /// chunks) sharing a single `message.id`; those arrive as separate nodes.
    /// This counts turns, not chunks.
    #[must_use]
    pub fn message_group_count(&self) -> usize {
        self.message_groups.len()
    }

    /// Get the main thread as entries.
    #[must_use]
    pub fn main_thread_entries(&self) -> Vec<&LogEntry> {
        self.main_thread
            .iter()
            .filter_map(|uuid| self.nodes.get(uuid).map(|n| &n.entry))
            .collect()
    }

    /// Uuid-less entries dropped from the node tree (summaries,
    /// file-history-snapshots, sidecar metadata, queue-operation and turn-end
    /// markers), preserved in original input order.
    #[must_use]
    pub fn orphan_entries(&self) -> &[LogEntry] {
        &self.orphan_entries
    }

    /// Entries for rendering an export: the uuid-less entries (which are absent
    /// from the node tree) followed by the rendered thread.
    ///
    /// `main_thread_only` selects the main thread vs. the full chronological
    /// flatten for the thread portion. Ordering rule: the uuid-less entries are
    /// surfaced first, in original input-file order, then the uuid-bearing
    /// thread. Most uuid-less entries carry no timestamp to interleave by, so
    /// this is deterministic but NOT byte-order-identical to the source file
    /// (e.g. a summary recorded mid-file is re-emitted ahead of the thread).
    #[must_use]
    pub fn entries_for_export(&self, main_thread_only: bool) -> Vec<&LogEntry> {
        let mut entries: Vec<&LogEntry> = self.orphan_entries.iter().collect();
        if main_thread_only {
            entries.extend(self.main_thread_entries());
        } else {
            entries.extend(self.chronological_entries());
        }
        entries
    }

    /// Get all entries in chronological order (flattened tree).
    #[must_use]
    pub fn chronological_entries(&self) -> Vec<&LogEntry> {
        let mut entries: Vec<_> = self.nodes.values().map(|n| &n.entry).collect();
        entries.sort_by(|a, b| match (a.timestamp(), b.timestamp()) {
            (Some(ta), Some(tb)) => ta.cmp(&tb),
            (Some(_), None) => std::cmp::Ordering::Less,
            (None, Some(_)) => std::cmp::Ordering::Greater,
            (None, None) => std::cmp::Ordering::Equal,
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
            // Distinct assistant turns: streaming chunks share one message.id
            // across several nodes, so count groups rather than nodes.
            assistant_messages: self.message_groups.len(),
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
pub fn reconstruct_from_session(session: &crate::discovery::Session) -> Result<Conversation> {
    let entries = session.parse()?;
    Conversation::from_entries(entries)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{SystemMessage, SystemSubtype, UserContent, UserMessage, UserSimpleContent};
    use chrono::Utc;
    use indexmap::IndexMap;

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
            is_compact_summary: None,
            is_visible_in_transcript_only: None,
            thinking_metadata: None,
            todos: vec![],
            tool_use_result: None,
            message: UserContent::Simple(UserSimpleContent {
                role: "user".to_string(),
                content: "test".to_string(),
                extra: IndexMap::new(),
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
    fn test_summary_entry_retained_for_export() {
        let summary = LogEntry::Summary(crate::model::SummaryMessage {
            summary: "session title".to_string(),
            leaf_uuid: Some("2".to_string()),
            is_compact_summary: None,
            extra: IndexMap::new(),
        });
        let entries = vec![
            make_user_entry("1", None),
            make_user_entry("2", Some("1")),
            summary,
        ];

        let conv = Conversation::from_entries(entries).unwrap();

        // The Summary has no UUID, so it is not a node and stays off the thread.
        assert_eq!(conv.len(), 2);
        assert!(!conv
            .main_thread_entries()
            .iter()
            .any(|e| matches!(e, LogEntry::Summary(_))));

        // But it is retained and surfaced for export, ahead of the thread.
        assert_eq!(conv.orphan_entries().len(), 1);
        let exported = conv.entries_for_export(true);
        assert!(matches!(exported.first(), Some(LogEntry::Summary(_))));
        assert_eq!(exported.len(), 3);
    }

    #[test]
    fn test_all_uuidless_entry_types_retained_for_export() {
        // Every uuid-less entry type must be retained so `-f jsonl` round-trips
        // by content (issue 0003). Parse realistic lines straight from JSON.
        let fhs: LogEntry = serde_json::from_str(
            r#"{"type":"file-history-snapshot","messageId":"m1","snapshot":{"messageId":"m1","timestamp":"2026-01-01T00:00:00Z","trackedFileBackups":{}}}"#,
        )
        .unwrap();
        let last_prompt: LogEntry =
            serde_json::from_str(r#"{"type":"last-prompt","sessionId":"s","lastPrompt":"hi"}"#)
                .unwrap();
        let mode: LogEntry =
            serde_json::from_str(r#"{"type":"mode","sessionId":"s","mode":"normal"}"#).unwrap();
        let ai_title: LogEntry =
            serde_json::from_str(r#"{"type":"ai-title","sessionId":"s","aiTitle":"t"}"#).unwrap();
        let perm: LogEntry = serde_json::from_str(
            r#"{"type":"permission-mode","sessionId":"s","permissionMode":"default"}"#,
        )
        .unwrap();
        let queue: LogEntry = serde_json::from_str(
            r#"{"type":"queue-operation","sessionId":"s","timestamp":"2026-01-01T00:00:00Z","operation":"enqueue"}"#,
        )
        .unwrap();
        let turn_end: LogEntry =
            serde_json::from_str(r#"{"type":"turn_end","timestamp":"2026-01-01T00:00:00Z"}"#)
                .unwrap();

        // Confirm none of these carry a UUID (so they never become nodes).
        for e in [
            &fhs,
            &last_prompt,
            &mode,
            &ai_title,
            &perm,
            &queue,
            &turn_end,
        ] {
            assert!(e.uuid().is_none());
        }

        let entries = vec![
            make_user_entry("1", None),
            fhs,
            last_prompt,
            mode,
            ai_title,
            perm,
            queue,
            turn_end,
            make_user_entry("2", Some("1")),
        ];

        let conv = Conversation::from_entries(entries).unwrap();

        // 2 uuid-bearing nodes; the other 7 are retained as orphan entries.
        assert_eq!(conv.len(), 2);
        assert_eq!(conv.orphan_entries().len(), 7);

        // entries_for_export surfaces all 9 (7 orphan + 2 thread).
        let exported = conv.entries_for_export(true);
        assert_eq!(exported.len(), 9);
        assert!(exported
            .iter()
            .any(|e| matches!(e, LogEntry::FileHistorySnapshot(_))));
        assert!(exported
            .iter()
            .any(|e| matches!(e, LogEntry::LastPrompt(_))));
        assert!(exported.iter().any(|e| matches!(e, LogEntry::TurnEnd(_))));

        // Orphan entries are emitted first, in original input order.
        assert!(matches!(
            exported.first(),
            Some(LogEntry::FileHistorySnapshot(_))
        ));
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

    fn make_compact_boundary(uuid: &str, logical_parent: &str) -> LogEntry {
        LogEntry::System(SystemMessage {
            uuid: uuid.to_string(),
            parent_uuid: None,
            logical_parent_uuid: Some(logical_parent.to_string()),
            subtype: Some(SystemSubtype::CompactBoundary),
            content: Some("Conversation compacted".to_string()),
            level: Some("info".to_string()),
            is_meta: None,
            timestamp: Utc::now(),
            session_id: Some("test".to_string()),
            version: None,
            cwd: None,
            git_branch: None,
            is_sidechain: None,
            user_type: None,
            compact_metadata: None,
            error: None,
            retry_in_ms: None,
            retry_attempt: None,
            max_retries: None,
            cause: None,
            hook_count: None,
            hook_infos: vec![],
            has_output: None,
            prevented_continuation: None,
            stop_reason: None,
            tool_use_id: None,
            checkpoint_id: None,
            target_uuid: None,
            rewind_mode: None,
            affected_files: vec![],
            new_name: None,
            old_name: None,
            extra: IndexMap::new(),
        })
    }

    #[test]
    fn test_orphaned_entries_promoted_to_roots() {
        // Simulate entries whose parent UUID doesn't exist in the parsed nodes
        // (e.g. parent was a "progress" entry that was skipped during parsing).
        // These orphans should be promoted to roots so the tree is still reachable.
        let entries = vec![
            // No entry with UUID "missing-parent" — it was skipped
            make_user_entry("2", Some("missing-parent")),
            make_user_entry("3", Some("2")),
            make_user_entry("4", Some("3")),
        ];

        let conv = Conversation::from_entries(entries).unwrap();

        assert_eq!(conv.len(), 3);
        // "2" should be promoted to a root since its parent doesn't exist
        assert_eq!(conv.roots().len(), 1);
        assert_eq!(conv.roots()[0], "2");
        // Main thread should include all 3 entries
        assert_eq!(conv.main_thread().len(), 3);
    }

    #[test]
    fn test_duplicate_uuid_keeps_first_and_records_diagnostic() {
        // Two entries share UUID "2". The first is kept; later duplicates are
        // recorded with their occurrence index instead of silently overwriting.
        let entries = vec![
            make_user_entry("1", None),
            make_user_entry("2", Some("1")),
            make_user_entry("2", Some("1")), // 2nd occurrence
            make_user_entry("2", Some("1")), // 3rd occurrence
        ];
        let conv = Conversation::from_entries(entries).unwrap();
        assert_eq!(
            conv.len(),
            2,
            "only first occurrence of each uuid is a node"
        );
        let dups = conv.duplicate_uuids();
        assert_eq!(dups.len(), 2);
        assert!(dups.iter().all(|d| d.uuid == "2"));
        assert_eq!(dups[0].occurrence, 2);
        assert_eq!(dups[1].occurrence, 3);
    }

    #[test]
    fn test_exact_duplicate_is_benign() {
        // Two byte-identical entries with the same UUID (e.g. an overlapping
        // boundary entry shared by two files in a resume chain). Deduplicated
        // without surfacing a conflict — nothing is lost.
        let dup = make_user_entry("2", Some("1"));
        let entries = vec![make_user_entry("1", None), dup.clone(), dup];
        let conv = Conversation::from_entries(entries).unwrap();
        let dups = conv.duplicate_uuids();
        assert_eq!(dups.len(), 1);
        assert!(dups[0].is_exact, "identical content is an exact duplicate");
        assert_eq!(conv.conflicting_duplicates().count(), 0);
        assert!(conv.duplicate_notice().is_none());
    }

    #[test]
    fn test_conflicting_duplicate_is_surfaced_and_recoverable() {
        // Same UUID, differing content — a genuine collision. The dropped entry
        // is retained (recoverable) and a notice is surfaced.
        let first = make_user_entry("2", Some("1"));
        let mut second = first.clone();
        if let LogEntry::User(ref mut u) = second {
            u.message = UserContent::Simple(UserSimpleContent {
                role: "user".to_string(),
                content: "different content".to_string(),
                extra: IndexMap::new(),
            });
        }
        let entries = vec![make_user_entry("1", None), first, second];
        let conv = Conversation::from_entries(entries).unwrap();
        let conflicts: Vec<_> = conv.conflicting_duplicates().collect();
        assert_eq!(conflicts.len(), 1);
        assert!(!conflicts[0].is_exact);
        match &conflicts[0].dropped {
            LogEntry::User(u) => match &u.message {
                UserContent::Simple(s) => assert_eq!(s.content, "different content"),
                UserContent::Blocks(_) => panic!("unexpected content shape"),
            },
            _ => panic!("expected user entry"),
        }
        assert!(conv.duplicate_notice().is_some());
    }

    #[test]
    fn test_logical_parent_cycle_is_broken() {
        // A compaction boundary's logicalParentUuid can point into its own
        // descendant chain, wiring a cycle into the tree (real parentUuid edges
        // stay acyclic, but the logical fallback closes a loop). "2" -> "B" via a
        // real parentUuid and "B" -> "2" via logicalParentUuid forms a 2-node cycle.
        // Reconstruction must break it instead of looping forever / overflowing the
        // stack. Regression test for the MCP server crash on deep cyclic sessions.
        let entries = vec![
            make_user_entry("root", None),
            make_user_entry("2", Some("B")),
            make_compact_boundary("B", "2"),
        ];

        // Reaching these assertions at all proves termination.
        let conv = Conversation::from_entries(entries).unwrap();
        assert_eq!(conv.len(), 3);
        // The cycle is cut at the logical-derived edge ("B"), promoting it to a root.
        assert!(conv.roots().contains(&"B".to_string()));
    }

    fn make_progress_entry(uuid: &str, parent: Option<&str>) -> LogEntry {
        use crate::model::message::{ProgressData, ProgressMessage};
        LogEntry::Progress(ProgressMessage {
            uuid: uuid.to_string(),
            parent_uuid: parent.map(String::from),
            timestamp: Utc::now(),
            session_id: "test".to_string(),
            tool_use_id: None,
            parent_tool_use_id: None,
            agent_id: None,
            is_sidechain: false,
            slug: None,
            data: ProgressData {
                progress_type: "bash_progress".to_string(),
                agent_id: None,
                prompt: None,
                extra: IndexMap::new(),
            },
            extra: IndexMap::new(),
        })
    }

    #[test]
    fn test_progress_chain_does_not_truncate_main_thread() {
        // An assistant node forks into a long progress chain (bash_progress
        // notifications during a long tool call) and the real user
        // continuation. The progress chain is longer, but the main thread must
        // follow the conversation, not dead-end into the progress chain.
        let mut entries = vec![
            make_user_entry("1", None),
            make_user_entry("assistant", Some("1")),
        ];
        // Progress chain off "assistant": 5 nodes.
        let mut prev = "assistant".to_string();
        for i in 0..5 {
            let id = format!("p{i}");
            entries.push(make_progress_entry(&id, Some(&prev)));
            prev = id;
        }
        // Real continuation off "assistant": 2 nodes (shorter than progress).
        entries.push(make_user_entry("cont1", Some("assistant")));
        entries.push(make_user_entry("cont2", Some("cont1")));

        let conv = Conversation::from_entries(entries).unwrap();
        let thread: Vec<&str> = conv.main_thread().iter().map(String::as_str).collect();

        // The thread must reach the real continuation, not stop in progress.
        assert!(thread.contains(&"cont1"), "thread: {thread:?}");
        assert!(thread.contains(&"cont2"), "thread: {thread:?}");
        assert!(
            !thread.iter().any(|u| u.starts_with('p')),
            "main thread walked into progress chain: {thread:?}"
        );
    }

    #[test]
    fn test_compaction_boundary_bridging() {
        // Simulate: entries 1-3, then compaction boundary with logicalParentUuid=3,
        // then entries 4-5 parented to the boundary.
        let entries = vec![
            make_user_entry("1", None),
            make_user_entry("2", Some("1")),
            make_user_entry("3", Some("2")),
            make_compact_boundary("cb", "3"),
            make_user_entry("4", Some("cb")),
            make_user_entry("5", Some("4")),
        ];

        let conv = Conversation::from_entries(entries).unwrap();

        // All 6 entries should be in the tree
        assert_eq!(conv.len(), 6);
        // Only one root (entry "1"), because the compact boundary used logicalParentUuid
        assert_eq!(conv.roots().len(), 1);
        // Main thread should span the entire conversation: 1 -> 2 -> 3 -> cb -> 4 -> 5
        assert_eq!(conv.main_thread().len(), 6);
        // All entries accessible via main_thread_entries
        assert_eq!(conv.main_thread_entries().len(), 6);
    }

    fn user_ts(uuid: &str, parent: Option<&str>, ts: &str) -> LogEntry {
        let parent_json = parent.map_or("null".to_string(), |p| format!("\"{p}\""));
        let json = format!(
            r#"{{"type":"user","uuid":"{uuid}","parentUuid":{parent_json},"timestamp":"{ts}","sessionId":"s","version":"2.1.0","isSidechain":false,"message":{{"role":"user","content":"x"}}}}"#
        );
        serde_json::from_str(&json).unwrap()
    }

    #[test]
    fn test_multi_root_preserves_preamble() {
        // Preamble root (u1->a1) and a post-compaction fragment whose
        // compact_boundary logical parent was dropped (own root). Only the
        // most-recent root is walked, so the preamble vanishes.
        let entries = vec![
            make_user_entry("u1", None),
            make_user_entry("a1", Some("u1")),
            make_compact_boundary("cb", "DROPPED-PARENT"),
            make_user_entry("u2", Some("cb")),
            make_user_entry("a2", Some("u2")),
        ];
        let conv = Conversation::from_entries(entries).unwrap();
        let thread: Vec<&str> = conv.main_thread().iter().map(String::as_str).collect();
        assert!(
            thread.contains(&"u1"),
            "preamble lost from main thread: {thread:?}"
        );
    }

    fn assistant_chunk(uuid: &str, parent: Option<&str>, msg_id: &str) -> LogEntry {
        let parent_json = parent.map_or("null".to_string(), |p| format!("\"{p}\""));
        let json = format!(
            r#"{{"type":"assistant","uuid":"{uuid}","parentUuid":{parent_json},"timestamp":"2026-01-01T00:00:00Z","sessionId":"s","version":"2.1.0","isSidechain":false,"message":{{"id":"{msg_id}","type":"message","role":"assistant","model":"m","content":[{{"type":"text","text":"x"}}]}}}}"#
        );
        serde_json::from_str(&json).unwrap()
    }

    #[test]
    fn test_streaming_chunks_count_as_one_assistant_turn() {
        // One assistant turn is written as several JSONL lines (streaming chunks)
        // sharing a single message.id, each a distinct node. Turn counts must
        // dedup by message.id rather than counting nodes.
        let entries = vec![
            make_user_entry("u1", None),
            // Turn A: 3 chunks sharing msg_A.
            assistant_chunk("c1", Some("u1"), "msg_A"),
            assistant_chunk("c2", Some("c1"), "msg_A"),
            assistant_chunk("c3", Some("c2"), "msg_A"),
            // Turn B: single chunk.
            assistant_chunk("c4", Some("c3"), "msg_B"),
        ];

        let conv = Conversation::from_entries(entries).unwrap();

        // All 5 lines remain distinct nodes.
        assert_eq!(conv.len(), 5);
        // But only 2 distinct assistant turns.
        assert_eq!(conv.message_group_count(), 2);
        assert_eq!(conv.statistics().assistant_messages, 2);
    }

    #[test]
    fn test_branch_point_follows_recent_canonical_branch() {
        // At an edit/retry fork, the abandoned branch can be LONGER than the
        // newer canonical branch. The main thread should follow the recent
        // (canonical) branch, not the larger abandoned one.
        let entries = vec![
            user_ts("u0", None, "2026-01-01T00:00:00Z"),
            user_ts("a1", Some("u0"), "2026-01-01T00:01:00Z"),
            // abandoned branch: larger (3 nodes), older
            user_ts("uold", Some("a1"), "2026-01-01T00:02:00Z"),
            user_ts("aold1", Some("uold"), "2026-01-01T00:03:00Z"),
            user_ts("aold2", Some("aold1"), "2026-01-01T00:04:00Z"),
            // canonical branch: smaller (2 nodes), newer
            user_ts("unew", Some("a1"), "2026-01-01T00:10:00Z"),
            user_ts("anew", Some("unew"), "2026-01-01T00:11:00Z"),
        ];
        let conv = Conversation::from_entries(entries).unwrap();
        let thread: Vec<&str> = conv.main_thread().iter().map(String::as_str).collect();
        assert!(
            thread.contains(&"unew") && thread.contains(&"anew"),
            "main thread should follow the recent canonical branch, got {thread:?}"
        );
    }
}
