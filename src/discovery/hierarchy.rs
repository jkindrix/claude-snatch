//! Agent hierarchy discovery and management.
//!
//! Links parent sessions to their spawned subagent sessions based on
//! temporal proximity (subagents created during parent session's active period).

use std::collections::HashMap;

use crate::error::Result;
use crate::model::LogEntry;
use crate::analytics::SessionAnalytics;

use super::project::Project;
use super::session::Session;

/// A node in the agent hierarchy tree.
#[derive(Debug, Clone)]
pub struct AgentNode {
    /// The session for this node.
    pub session: Session,
    /// Child agent sessions spawned by this session.
    pub children: Vec<AgentNode>,
    /// Depth in the hierarchy (0 = root).
    pub depth: usize,
}

impl AgentNode {
    /// Create a leaf node (no children).
    pub fn leaf(session: Session, depth: usize) -> Self {
        Self {
            session,
            children: Vec::new(),
            depth,
        }
    }

    /// Create a node with children.
    pub fn with_children(session: Session, children: Vec<AgentNode>, depth: usize) -> Self {
        Self {
            session,
            children,
            depth,
        }
    }

    /// Count total sessions in this subtree.
    pub fn total_sessions(&self) -> usize {
        1 + self.children.iter().map(|c| c.total_sessions()).sum::<usize>()
    }

    /// Flatten the hierarchy into a list with depth info.
    pub fn flatten(&self) -> Vec<(usize, &Session)> {
        let mut result = vec![(self.depth, &self.session)];
        for child in &self.children {
            result.extend(child.flatten());
        }
        result
    }

    /// Get all session IDs in this subtree.
    pub fn all_session_ids(&self) -> Vec<&str> {
        let mut ids = vec![self.session.session_id()];
        for child in &self.children {
            ids.extend(child.all_session_ids());
        }
        ids
    }
}

/// Agent hierarchy builder.
pub struct HierarchyBuilder {
    /// Time window (in seconds) to consider for parent-child relationships.
    time_window_secs: u64,
}

impl Default for HierarchyBuilder {
    fn default() -> Self {
        Self {
            // Default: subagents must be created within 1 hour of parent modification
            time_window_secs: 3600,
        }
    }
}

impl HierarchyBuilder {
    /// Create a new hierarchy builder.
    pub fn new() -> Self {
        Self::default()
    }

    /// Set the time window for matching subagents to parents.
    pub fn with_time_window(mut self, seconds: u64) -> Self {
        self.time_window_secs = seconds;
        self
    }

    /// Build the agent hierarchy for a project.
    pub fn build_for_project(&self, project: &Project) -> Result<Vec<AgentNode>> {
        let sessions = project.sessions()?;
        self.build_hierarchy(sessions)
    }

    /// Build hierarchy from a list of sessions.
    pub fn build_hierarchy(&self, sessions: Vec<Session>) -> Result<Vec<AgentNode>> {
        // Separate main sessions and subagents
        let (main_sessions, subagents): (Vec<_>, Vec<_>) =
            sessions.into_iter().partition(|s| !s.is_subagent());

        // Group subagents by potential parent based on timing
        let mut parent_to_children: HashMap<String, Vec<Session>> = HashMap::new();
        let mut unassigned_subagents: Vec<Session> = Vec::new();

        for subagent in subagents {
            let subagent_time = subagent.modified_time();

            // Find the best matching parent (most recent main session before subagent)
            let mut best_parent: Option<&Session> = None;
            let mut best_time_diff = u64::MAX;

            for parent in &main_sessions {
                let parent_time = parent.modified_time();

                // Subagent should be modified during or after parent activity
                if let (Ok(parent_dur), Ok(subagent_dur)) = (
                    parent_time.duration_since(std::time::UNIX_EPOCH),
                    subagent_time.duration_since(std::time::UNIX_EPOCH),
                ) {
                    let parent_secs = parent_dur.as_secs();
                    let subagent_secs = subagent_dur.as_secs();

                    // Subagent created within time window of parent activity
                    if subagent_secs >= parent_secs.saturating_sub(self.time_window_secs)
                        && subagent_secs <= parent_secs.saturating_add(self.time_window_secs)
                    {
                        let diff = subagent_secs.abs_diff(parent_secs);

                        if diff < best_time_diff {
                            best_time_diff = diff;
                            best_parent = Some(parent);
                        }
                    }
                }
            }

            if let Some(parent) = best_parent {
                parent_to_children
                    .entry(parent.session_id().to_string())
                    .or_default()
                    .push(subagent);
            } else {
                unassigned_subagents.push(subagent);
            }
        }

        // Build hierarchy nodes
        let mut nodes: Vec<AgentNode> = Vec::new();

        for parent in main_sessions {
            let children = parent_to_children
                .remove(parent.session_id())
                .unwrap_or_default()
                .into_iter()
                .map(|s| AgentNode::leaf(s, 1))
                .collect();

            nodes.push(AgentNode::with_children(parent, children, 0));
        }

        // Add unassigned subagents as root-level nodes
        for subagent in unassigned_subagents {
            nodes.push(AgentNode::leaf(subagent, 0));
        }

        // Sort by modification time (newest first)
        nodes.sort_by(|a, b| b.session.modified_time().cmp(&a.session.modified_time()));

        Ok(nodes)
    }
}

/// Aggregated statistics across a session hierarchy.
#[derive(Debug, Clone, Default)]
pub struct AggregatedStats {
    /// Total sessions in the hierarchy.
    pub total_sessions: usize,
    /// Main sessions count.
    pub main_sessions: usize,
    /// Subagent sessions count.
    pub subagent_sessions: usize,
    /// Combined total messages.
    pub total_messages: usize,
    /// Combined user messages.
    pub user_messages: usize,
    /// Combined assistant messages.
    pub assistant_messages: usize,
    /// Combined total tokens.
    pub total_tokens: u64,
    /// Combined input tokens.
    pub input_tokens: u64,
    /// Combined output tokens.
    pub output_tokens: u64,
    /// Combined tool invocations.
    pub tool_invocations: usize,
    /// Combined thinking blocks.
    pub thinking_blocks: usize,
}

impl AggregatedStats {
    /// Aggregate statistics from multiple session analytics.
    pub fn from_analytics(analytics: &[SessionAnalytics]) -> Self {
        let mut stats = Self {
            total_sessions: analytics.len(),
            ..Default::default()
        };

        for a in analytics {
            let summary = a.summary_report();
            stats.total_messages += summary.total_messages;
            stats.user_messages += summary.user_messages;
            stats.assistant_messages += summary.assistant_messages;
            stats.total_tokens += summary.total_tokens;
            stats.input_tokens += summary.input_tokens;
            stats.output_tokens += summary.output_tokens;
            stats.tool_invocations += summary.tool_invocations;
            stats.thinking_blocks += summary.thinking_blocks;
        }

        stats
    }

    /// Aggregate statistics from a hierarchy node.
    pub fn from_node(node: &AgentNode) -> Result<Self> {
        let mut analytics_list = Vec::new();
        let mut main_count = 0;
        let mut subagent_count = 0;

        Self::collect_analytics(node, &mut analytics_list, &mut main_count, &mut subagent_count)?;

        let mut stats = Self::from_analytics(&analytics_list);
        stats.main_sessions = main_count;
        stats.subagent_sessions = subagent_count;

        Ok(stats)
    }

    /// Recursively collect analytics from node tree.
    fn collect_analytics(
        node: &AgentNode,
        analytics: &mut Vec<SessionAnalytics>,
        main_count: &mut usize,
        subagent_count: &mut usize,
    ) -> Result<()> {
        use crate::reconstruction::Conversation;

        // Parse and analyze this session
        let entries = node.session.parse()?;
        if let Ok(conversation) = Conversation::from_entries(entries) {
            analytics.push(SessionAnalytics::from_conversation(&conversation));
        }

        if node.session.is_subagent() {
            *subagent_count += 1;
        } else {
            *main_count += 1;
        }

        // Recurse into children
        for child in &node.children {
            Self::collect_analytics(child, analytics, main_count, subagent_count)?;
        }

        Ok(())
    }
}

/// Export all entries from a hierarchy node in chronological order.
pub fn collect_hierarchy_entries(node: &AgentNode) -> Result<Vec<(String, LogEntry)>> {
    let mut entries: Vec<(String, chrono::DateTime<chrono::Utc>, String, LogEntry)> = Vec::new();

    collect_entries_recursive(node, &mut entries)?;

    // Sort by timestamp
    entries.sort_by_key(|(_, timestamp, _, _)| *timestamp);

    // Return with session labels
    Ok(entries.into_iter().map(|(label, _, _, entry)| (label, entry)).collect())
}

/// Recursively collect entries with timestamps.
fn collect_entries_recursive(
    node: &AgentNode,
    entries: &mut Vec<(String, chrono::DateTime<chrono::Utc>, String, LogEntry)>,
) -> Result<()> {
    let session_id = node.session.session_id();
    let label = if node.session.is_subagent() {
        format!("[Agent {}]", &session_id[..8.min(session_id.len())])
    } else {
        format!("[Main {}]", &session_id[..8.min(session_id.len())])
    };

    let parsed_entries = node.session.parse()?;

    for entry in parsed_entries {
        // Use entry timestamp, defaulting to epoch if not available
        let timestamp = entry.timestamp().unwrap_or_else(|| {
            // Unix epoch (0, 0) is always a valid timestamp
            chrono::DateTime::from_timestamp(0, 0).expect("unix epoch is valid")
        });
        entries.push((label.clone(), timestamp, session_id.to_string(), entry));
    }

    for child in &node.children {
        collect_entries_recursive(child, entries)?;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_hierarchy_builder_default() {
        let builder = HierarchyBuilder::new();
        assert_eq!(builder.time_window_secs, 3600);
    }

    #[test]
    fn test_hierarchy_builder_custom_window() {
        let builder = HierarchyBuilder::new().with_time_window(7200);
        assert_eq!(builder.time_window_secs, 7200);
    }
}
