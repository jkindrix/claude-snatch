//! Analytics and statistics for Claude Code sessions.
//!
//! This module provides:
//! - Token usage calculation
//! - Cache efficiency metrics
//! - Tool invocation tracking
//! - Cost estimation
//! - Session duration analysis


use chrono::{DateTime, Duration, Utc};
use indexmap::IndexMap;

use crate::model::{
    usage::AggregatedUsage,
    AssistantMessage, ContentBlock, LogEntry,
};
use crate::reconstruction::Conversation;

/// Session analytics aggregator.
#[derive(Debug, Default)]
pub struct SessionAnalytics {
    /// Total token usage.
    pub usage: AggregatedUsage,
    /// Session start time.
    pub start_time: Option<DateTime<Utc>>,
    /// Session end time.
    pub end_time: Option<DateTime<Utc>>,
    /// Message counts by type.
    pub message_counts: MessageCounts,
    /// Tool invocation counts by tool name.
    pub tool_counts: IndexMap<String, usize>,
    /// Error counts by type.
    pub error_counts: IndexMap<String, usize>,
    /// Models used.
    pub models_used: IndexMap<String, usize>,
    /// Branch count.
    pub branch_count: usize,
    /// Thinking usage.
    pub thinking_stats: ThinkingStats,
}

impl SessionAnalytics {
    /// Create new analytics from a conversation.
    pub fn from_conversation(conversation: &Conversation) -> Self {
        let mut analytics = Self::default();
        analytics.process_conversation(conversation);
        analytics
    }

    /// Process a conversation to extract analytics.
    pub fn process_conversation(&mut self, conversation: &Conversation) {
        self.branch_count = conversation.branch_points().len();

        for node in conversation.nodes().values() {
            self.process_entry(&node.entry);
        }

        // Calculate cost
        self.usage.calculate_cost();
    }

    /// Process a single log entry.
    pub fn process_entry(&mut self, entry: &LogEntry) {
        // Update timestamps
        if let Some(ts) = entry.timestamp() {
            if self.start_time.is_none() || Some(ts) < self.start_time {
                self.start_time = Some(ts);
            }
            if self.end_time.is_none() || Some(ts) > self.end_time {
                self.end_time = Some(ts);
            }
        }

        match entry {
            LogEntry::User(user) => {
                self.message_counts.user += 1;

                // Count tool results
                let tool_results = user.message.tool_results();
                self.message_counts.tool_results += tool_results.len();

                for result in tool_results {
                    if result.is_explicit_error() {
                        *self.error_counts.entry("tool_error".to_string()).or_insert(0) += 1;
                    }
                }
            }
            LogEntry::Assistant(assistant) => {
                self.message_counts.assistant += 1;
                self.process_assistant(assistant);
            }
            LogEntry::System(system) => {
                self.message_counts.system += 1;

                if let Some(subtype) = &system.subtype {
                    if *subtype == crate::model::SystemSubtype::ApiError {
                        *self.error_counts.entry("api_error".to_string()).or_insert(0) += 1;
                    }
                }
            }
            LogEntry::Summary(_) => {
                self.message_counts.summary += 1;
            }
            LogEntry::FileHistorySnapshot(_) => {
                self.message_counts.file_snapshot += 1;
            }
            LogEntry::QueueOperation(_) => {
                self.message_counts.queue_operation += 1;
            }
            LogEntry::TurnEnd(_) => {
                self.message_counts.turn_end += 1;
            }
        }
    }

    /// Process an assistant message.
    fn process_assistant(&mut self, assistant: &AssistantMessage) {
        // Track model usage
        let model = &assistant.message.model;
        *self.models_used.entry(model.clone()).or_insert(0) += 1;

        // Add usage stats
        if let Some(usage) = &assistant.message.usage {
            self.usage.add_usage(model, usage);
        }

        // Process content blocks
        for content in &assistant.message.content {
            match content {
                ContentBlock::ToolUse(tool_use) => {
                    self.message_counts.tool_uses += 1;
                    *self.tool_counts.entry(tool_use.name.clone()).or_insert(0) += 1;
                    self.usage.record_tool(&tool_use.name);
                }
                ContentBlock::Thinking(thinking) => {
                    self.message_counts.thinking_blocks += 1;
                    self.thinking_stats.block_count += 1;
                    self.thinking_stats.total_chars += thinking.thinking.len();
                }
                ContentBlock::Text(_) => {
                    self.message_counts.text_blocks += 1;
                }
                ContentBlock::Image(_) => {
                    self.message_counts.image_blocks += 1;
                }
                _ => {}
            }
        }
    }

    /// Get session duration.
    #[must_use]
    pub fn duration(&self) -> Option<Duration> {
        match (&self.start_time, &self.end_time) {
            (Some(start), Some(end)) => Some(*end - *start),
            _ => None,
        }
    }

    /// Get duration as human-readable string.
    #[must_use]
    pub fn duration_string(&self) -> Option<String> {
        self.duration().map(|d| {
            let total_secs = d.num_seconds();
            if total_secs < 60 {
                format!("{total_secs}s")
            } else if total_secs < 3600 {
                format!("{}m {}s", total_secs / 60, total_secs % 60)
            } else {
                format!(
                    "{}h {}m {}s",
                    total_secs / 3600,
                    (total_secs % 3600) / 60,
                    total_secs % 60
                )
            }
        })
    }

    /// Get the primary model used.
    #[must_use]
    pub fn primary_model(&self) -> Option<&str> {
        self.models_used
            .iter()
            .max_by_key(|(_, count)| *count)
            .map(|(model, _)| model.as_str())
    }

    /// Get top N tools by usage.
    #[must_use]
    pub fn top_tools(&self, n: usize) -> Vec<(&str, usize)> {
        let mut tools: Vec<_> = self.tool_counts.iter().collect();
        tools.sort_by(|a, b| b.1.cmp(a.1));
        tools.into_iter().take(n).map(|(k, v)| (k.as_str(), *v)).collect()
    }

    /// Get total error count.
    #[must_use]
    pub fn total_errors(&self) -> usize {
        self.error_counts.values().sum()
    }

    /// Get cache efficiency percentage.
    #[must_use]
    pub fn cache_efficiency(&self) -> f64 {
        self.usage.usage.cache_hit_rate()
    }

    /// Generate a summary report.
    #[must_use]
    pub fn summary_report(&self) -> AnalyticsSummary {
        AnalyticsSummary {
            duration: self.duration(),
            total_messages: self.message_counts.total(),
            user_messages: self.message_counts.user,
            assistant_messages: self.message_counts.assistant,
            total_tokens: self.usage.usage.total_tokens(),
            input_tokens: self.usage.usage.total_input_tokens(),
            output_tokens: self.usage.usage.output_tokens,
            tool_invocations: self.message_counts.tool_uses,
            unique_tools: self.tool_counts.len(),
            thinking_blocks: self.thinking_stats.block_count,
            error_count: self.total_errors(),
            cache_hit_rate: self.cache_efficiency(),
            estimated_cost: self.usage.estimated_cost,
            branch_count: self.branch_count,
            primary_model: self.primary_model().map(String::from),
        }
    }

    /// Get usage predictions based on current session.
    #[must_use]
    pub fn predictions(&self, monthly_limit: Option<u64>) -> UsagePrediction {
        UsagePrediction::calculate(self, monthly_limit)
    }

    /// Get usage predictions with default Pro limit.
    #[must_use]
    pub fn predictions_pro(&self) -> UsagePrediction {
        self.predictions(Some(limits::CLAUDE_PRO_MONTHLY))
    }
}

/// Message counts by type.
#[derive(Debug, Clone, Default)]
pub struct MessageCounts {
    /// User messages.
    pub user: usize,
    /// Assistant messages.
    pub assistant: usize,
    /// System messages.
    pub system: usize,
    /// Summary messages.
    pub summary: usize,
    /// File snapshot messages.
    pub file_snapshot: usize,
    /// Queue operations.
    pub queue_operation: usize,
    /// Turn end markers.
    pub turn_end: usize,
    /// Text content blocks.
    pub text_blocks: usize,
    /// Tool use blocks.
    pub tool_uses: usize,
    /// Tool result blocks.
    pub tool_results: usize,
    /// Thinking blocks.
    pub thinking_blocks: usize,
    /// Image blocks.
    pub image_blocks: usize,
}

impl MessageCounts {
    /// Get total message count.
    #[must_use]
    pub fn total(&self) -> usize {
        self.user
            + self.assistant
            + self.system
            + self.summary
            + self.file_snapshot
            + self.queue_operation
            + self.turn_end
    }

    /// Get conversation message count (user + assistant only).
    #[must_use]
    pub fn conversation(&self) -> usize {
        self.user + self.assistant
    }
}

/// Thinking block statistics.
#[derive(Debug, Clone, Default)]
pub struct ThinkingStats {
    /// Number of thinking blocks.
    pub block_count: usize,
    /// Total characters in thinking.
    pub total_chars: usize,
}

impl ThinkingStats {
    /// Average thinking block length.
    #[must_use]
    pub fn average_length(&self) -> usize {
        if self.block_count == 0 {
            0
        } else {
            self.total_chars / self.block_count
        }
    }
}

/// Summary of session analytics.
#[derive(Debug, Clone)]
pub struct AnalyticsSummary {
    /// Session duration.
    pub duration: Option<Duration>,
    /// Total messages.
    pub total_messages: usize,
    /// User messages.
    pub user_messages: usize,
    /// Assistant messages.
    pub assistant_messages: usize,
    /// Total tokens.
    pub total_tokens: u64,
    /// Input tokens.
    pub input_tokens: u64,
    /// Output tokens.
    pub output_tokens: u64,
    /// Tool invocations.
    pub tool_invocations: usize,
    /// Unique tools used.
    pub unique_tools: usize,
    /// Thinking blocks.
    pub thinking_blocks: usize,
    /// Error count.
    pub error_count: usize,
    /// Cache hit rate percentage.
    pub cache_hit_rate: f64,
    /// Estimated cost in USD.
    pub estimated_cost: Option<f64>,
    /// Branch count.
    pub branch_count: usize,
    /// Primary model used.
    pub primary_model: Option<String>,
}

impl AnalyticsSummary {
    /// Format cost as currency string.
    #[must_use]
    pub fn cost_string(&self) -> String {
        match self.estimated_cost {
            Some(cost) if cost < 0.01 => format!("${cost:.4}"),
            Some(cost) if cost < 1.0 => format!("${cost:.3}"),
            Some(cost) => format!("${cost:.2}"),
            None => "N/A".to_string(),
        }
    }

    /// Format duration as string.
    #[must_use]
    pub fn duration_string(&self) -> String {
        self.duration
            .map(|d| {
                let secs = d.num_seconds();
                if secs < 60 {
                    format!("{secs}s")
                } else if secs < 3600 {
                    format!("{}m {}s", secs / 60, secs % 60)
                } else {
                    format!("{}h {}m", secs / 3600, (secs % 3600) / 60)
                }
            })
            .unwrap_or_else(|| "N/A".to_string())
    }
}

/// Usage rate calculation and predictions.
#[derive(Debug, Clone, Default)]
pub struct UsagePrediction {
    /// Average tokens per hour.
    pub tokens_per_hour: f64,
    /// Average messages per hour.
    pub messages_per_hour: f64,
    /// Average cost per hour.
    pub cost_per_hour: Option<f64>,
    /// Estimated hours until monthly token limit.
    pub hours_to_limit: Option<f64>,
    /// Estimated messages until limit.
    pub messages_to_limit: Option<u64>,
    /// Current usage as percentage of limit.
    pub usage_percentage: Option<f64>,
    /// Daily projection (tokens).
    pub daily_projection: f64,
    /// Monthly projection (tokens).
    pub monthly_projection: f64,
    /// Monthly cost projection.
    pub monthly_cost_projection: Option<f64>,
}

impl UsagePrediction {
    /// Calculate usage predictions from analytics and duration.
    pub fn calculate(analytics: &SessionAnalytics, monthly_limit: Option<u64>) -> Self {
        let duration_hours = analytics
            .duration()
            .map(|d| d.num_seconds() as f64 / 3600.0)
            .unwrap_or(1.0)
            .max(0.1); // Minimum 6 minutes to avoid division issues

        let total_tokens = analytics.usage.usage.total_tokens() as f64;
        let tokens_per_hour = total_tokens / duration_hours;

        let total_messages = analytics.message_counts.conversation() as f64;
        let messages_per_hour = total_messages / duration_hours;

        let cost_per_hour = analytics.usage.estimated_cost.map(|c| c / duration_hours);

        // Daily and monthly projections (assuming 8 hour work days, 22 work days/month)
        let daily_projection = tokens_per_hour * 8.0;
        let monthly_projection = daily_projection * 22.0;
        let monthly_cost_projection = cost_per_hour.map(|c| c * 8.0 * 22.0);

        // Calculate time to limit
        let (hours_to_limit, messages_to_limit, usage_percentage) = if let Some(limit) = monthly_limit {
            let remaining = limit.saturating_sub(analytics.usage.usage.total_tokens());
            let hours = if tokens_per_hour > 0.0 {
                Some(remaining as f64 / tokens_per_hour)
            } else {
                None
            };
            let messages = if messages_per_hour > 0.0 {
                Some((remaining as f64 / (total_tokens / total_messages.max(1.0))) as u64)
            } else {
                None
            };
            let percentage = Some(analytics.usage.usage.total_tokens() as f64 / limit as f64 * 100.0);
            (hours, messages, percentage)
        } else {
            (None, None, None)
        };

        Self {
            tokens_per_hour,
            messages_per_hour,
            cost_per_hour,
            hours_to_limit,
            messages_to_limit,
            usage_percentage,
            daily_projection,
            monthly_projection,
            monthly_cost_projection,
        }
    }

    /// Format hours to limit as human-readable string.
    #[must_use]
    pub fn time_to_limit_string(&self) -> String {
        match self.hours_to_limit {
            Some(hours) if hours < 1.0 => format!("{:.0}m", hours * 60.0),
            Some(hours) if hours < 24.0 => format!("{:.1}h", hours),
            Some(hours) if hours < 168.0 => format!("{:.1} days", hours / 24.0),
            Some(hours) => format!("{:.1} weeks", hours / 168.0),
            None => "N/A".to_string(),
        }
    }

    /// Format usage percentage as string.
    #[must_use]
    pub fn usage_percentage_string(&self) -> String {
        match self.usage_percentage {
            Some(pct) => format!("{:.1}%", pct),
            None => "N/A".to_string(),
        }
    }

    /// Format tokens per hour as string.
    #[must_use]
    pub fn rate_string(&self) -> String {
        if self.tokens_per_hour < 1000.0 {
            format!("{:.0} tokens/hr", self.tokens_per_hour)
        } else if self.tokens_per_hour < 1_000_000.0 {
            format!("{:.1}K tokens/hr", self.tokens_per_hour / 1000.0)
        } else {
            format!("{:.2}M tokens/hr", self.tokens_per_hour / 1_000_000.0)
        }
    }

    /// Format monthly projection as string.
    #[must_use]
    pub fn monthly_projection_string(&self) -> String {
        if self.monthly_projection < 1_000_000.0 {
            format!("{:.0}K tokens/month", self.monthly_projection / 1000.0)
        } else {
            format!("{:.1}M tokens/month", self.monthly_projection / 1_000_000.0)
        }
    }
}

/// Known model token limits (monthly).
pub mod limits {
    /// Default monthly token limit for Claude Pro.
    pub const CLAUDE_PRO_MONTHLY: u64 = 100_000_000; // 100M tokens (estimated)

    /// Default monthly token limit for Claude Teams.
    pub const CLAUDE_TEAMS_MONTHLY: u64 = 500_000_000; // 500M tokens (estimated)

    /// Default monthly token limit for Claude Enterprise.
    pub const CLAUDE_ENTERPRISE_MONTHLY: u64 = 1_000_000_000; // 1B tokens (estimated)
}

/// Aggregate analytics across multiple sessions.
#[derive(Debug, Default)]
pub struct ProjectAnalytics {
    /// Session count.
    pub session_count: usize,
    /// Total usage across all sessions.
    pub total_usage: AggregatedUsage,
    /// Combined message counts.
    pub message_counts: MessageCounts,
    /// Combined tool counts.
    pub tool_counts: IndexMap<String, usize>,
    /// Total duration across all sessions.
    pub total_duration: Duration,
    /// Model usage breakdown.
    pub model_usage: IndexMap<String, u64>,
}

impl ProjectAnalytics {
    /// Add a session's analytics.
    pub fn add_session(&mut self, session: &SessionAnalytics) {
        self.session_count += 1;

        // Merge usage
        self.total_usage.usage.merge(&session.usage.usage);

        // Merge message counts
        self.message_counts.user += session.message_counts.user;
        self.message_counts.assistant += session.message_counts.assistant;
        self.message_counts.system += session.message_counts.system;
        self.message_counts.tool_uses += session.message_counts.tool_uses;
        self.message_counts.tool_results += session.message_counts.tool_results;
        self.message_counts.thinking_blocks += session.message_counts.thinking_blocks;

        // Merge tool counts
        for (tool, count) in &session.tool_counts {
            *self.tool_counts.entry(tool.clone()).or_insert(0) += count;
        }

        // Add duration
        if let Some(duration) = session.duration() {
            self.total_duration = self.total_duration + duration;
        }

        // Merge model usage
        for (model, count) in &session.models_used {
            *self.model_usage.entry(model.clone()).or_insert(0) += *count as u64;
        }
    }

    /// Calculate estimated total cost.
    pub fn calculate_cost(&mut self) {
        self.total_usage.calculate_cost();
    }
}

/// Fidelity scoring for extraction quality assessment.
///
/// This analyzes parsed sessions and calculates how completely
/// they capture all documented JSONL data elements.
#[derive(Debug, Clone, Default)]
pub struct FidelityScore {
    /// Core content elements found (out of 10).
    pub core_content: CategoryScore,
    /// Identity and linking elements found (out of 7).
    pub identity_linking: CategoryScore,
    /// Usage and token elements found (out of 11).
    pub usage_tokens: CategoryScore,
    /// Environment elements found (out of 4).
    pub environment: CategoryScore,
    /// Agent hierarchy elements found (out of 4).
    pub agent_hierarchy: CategoryScore,
    /// Error recovery elements found (out of 7).
    pub error_recovery: CategoryScore,
    /// System metadata elements found (out of 9).
    pub system_metadata: CategoryScore,
    /// Specialized message elements found (out of 14+).
    pub specialized: CategoryScore,
    /// Overall score (0-100).
    pub overall_score: f64,
    /// Grade (A-F).
    pub grade: char,
    /// Recommendations for improvement.
    pub recommendations: Vec<String>,
}

/// Score for a category of elements.
#[derive(Debug, Clone, Default)]
pub struct CategoryScore {
    /// Number of elements found.
    pub found: usize,
    /// Total possible elements.
    pub total: usize,
    /// Percentage score.
    pub percentage: f64,
}

impl CategoryScore {
    /// Create a new category score.
    fn new(found: usize, total: usize) -> Self {
        let percentage = if total > 0 {
            (found as f64 / total as f64) * 100.0
        } else {
            100.0
        };
        Self { found, total, percentage }
    }
}

impl FidelityScore {
    /// Calculate fidelity score from a conversation.
    pub fn from_conversation(conversation: &Conversation) -> Self {
        let mut score = Self::default();

        // Count elements across all entries
        let mut has_user_text = false;
        let mut has_assistant_text = false;
        let mut has_thinking = false;
        let mut has_tool_calls = false;
        let mut has_tool_results = false;
        let mut has_tool_ids = false;
        let mut has_images = false;
        let mut has_timestamps = false;
        let mut has_uuids = false;
        let mut has_parent_uuids = false;
        let mut has_session_id = false;
        let mut has_model = false;
        let mut has_usage = false;
        let mut has_cache_stats = false;
        let mut has_cwd = false;
        let mut has_version = false;
        let mut has_sidechain = false;
        let mut has_api_error = false;
        let mut has_system_events = false;
        let mut has_thinking_metadata = false;
        let mut has_tool_structured_output = false;

        for node in conversation.nodes().values() {
            match &node.entry {
                LogEntry::User(user) => {
                    has_user_text = true;
                    // timestamps are always present as DateTime<Utc>
                    has_timestamps = true;
                    if !user.uuid.is_empty() {
                        has_uuids = true;
                    }
                    if user.parent_uuid.is_some() {
                        has_parent_uuids = true;
                    }
                    if !user.session_id.is_empty() {
                        has_session_id = true;
                    }
                    if user.cwd.is_some() {
                        has_cwd = true;
                    }
                    // version is always present as String
                    if !user.version.is_empty() {
                        has_version = true;
                    }
                    // is_sidechain is always present as bool
                    has_sidechain = true;
                    // Check for tool results
                    let results = user.message.tool_results();
                    if !results.is_empty() {
                        has_tool_results = true;
                    }
                    // Check for images
                    if !user.message.images().is_empty() {
                        has_images = true;
                    }
                }
                LogEntry::Assistant(assistant) => {
                    has_assistant_text = true;
                    // timestamps are always present as DateTime<Utc>
                    has_timestamps = true;
                    // model is always present as String
                    if !assistant.message.model.is_empty() {
                        has_model = true;
                    }
                    if let Some(usage) = &assistant.message.usage {
                        has_usage = true;
                        if usage.cache_creation_input_tokens.is_some()
                            || usage.cache_read_input_tokens.is_some()
                        {
                            has_cache_stats = true;
                        }
                    }
                    // Check for thinking blocks
                    for block in &assistant.message.content {
                        if matches!(block, ContentBlock::Thinking { .. }) {
                            has_thinking = true;
                            has_thinking_metadata = true;
                        }
                        if matches!(block, ContentBlock::ToolUse { .. }) {
                            has_tool_calls = true;
                            has_tool_ids = true;
                        }
                    }
                }
                LogEntry::System(system) => {
                    has_system_events = true;
                    if system.subtype == Some(crate::model::SystemSubtype::ApiError) {
                        has_api_error = true;
                    }
                }
                _ => {}
            }
        }

        // Calculate category scores
        score.core_content = CategoryScore::new(
            [has_user_text, has_assistant_text, has_thinking, has_tool_calls,
             has_tool_results, has_tool_ids, has_images].iter().filter(|&&x| x).count() + 3, // +3 for always present
            10,
        );

        score.identity_linking = CategoryScore::new(
            [has_timestamps, has_uuids, has_parent_uuids, has_session_id].iter().filter(|&&x| x).count() + 2,
            7,
        );

        score.usage_tokens = CategoryScore::new(
            [has_model, has_usage, has_cache_stats].iter().filter(|&&x| x).count() + 3,
            11,
        );

        score.environment = CategoryScore::new(
            [has_cwd, has_version].iter().filter(|&&x| x).count() + 1,
            4,
        );

        score.agent_hierarchy = CategoryScore::new(
            [has_sidechain].iter().filter(|&&x| x).count() + 2,
            4,
        );

        score.error_recovery = CategoryScore::new(
            [has_api_error].iter().filter(|&&x| x).count() + 2,
            7,
        );

        score.system_metadata = CategoryScore::new(
            [has_system_events, has_thinking_metadata].iter().filter(|&&x| x).count() + 3,
            9,
        );

        score.specialized = CategoryScore::new(
            [has_tool_structured_output].iter().filter(|&&x| x).count() + 5,
            14,
        );

        // Calculate overall score
        let total_found: usize = [
            &score.core_content,
            &score.identity_linking,
            &score.usage_tokens,
            &score.environment,
            &score.agent_hierarchy,
            &score.error_recovery,
            &score.system_metadata,
            &score.specialized,
        ].iter().map(|c| c.found).sum();

        let total_possible: usize = [
            &score.core_content,
            &score.identity_linking,
            &score.usage_tokens,
            &score.environment,
            &score.agent_hierarchy,
            &score.error_recovery,
            &score.system_metadata,
            &score.specialized,
        ].iter().map(|c| c.total).sum();

        score.overall_score = (total_found as f64 / total_possible as f64) * 100.0;

        score.grade = match score.overall_score {
            s if s >= 90.0 => 'A',
            s if s >= 80.0 => 'B',
            s if s >= 70.0 => 'C',
            s if s >= 60.0 => 'D',
            _ => 'F',
        };

        // Generate recommendations
        if !has_thinking {
            score.recommendations.push("Enable thinking blocks for complete extraction".to_string());
        }
        if !has_cache_stats {
            score.recommendations.push("Cache statistics may not be available in older sessions".to_string());
        }
        if !has_tool_calls && !has_tool_results {
            score.recommendations.push("No tool usage detected - may be a conversation-only session".to_string());
        }

        score
    }

    /// Format as a detailed report.
    #[must_use]
    pub fn report(&self) -> String {
        let mut report = String::new();
        report.push_str(&format!("Fidelity Score: {:.1}% (Grade: {})\n", self.overall_score, self.grade));
        report.push_str("\nCategory Breakdown:\n");
        report.push_str(&format!("  Core Content:      {}/{} ({:.0}%)\n",
            self.core_content.found, self.core_content.total, self.core_content.percentage));
        report.push_str(&format!("  Identity/Linking:  {}/{} ({:.0}%)\n",
            self.identity_linking.found, self.identity_linking.total, self.identity_linking.percentage));
        report.push_str(&format!("  Usage/Tokens:      {}/{} ({:.0}%)\n",
            self.usage_tokens.found, self.usage_tokens.total, self.usage_tokens.percentage));
        report.push_str(&format!("  Environment:       {}/{} ({:.0}%)\n",
            self.environment.found, self.environment.total, self.environment.percentage));
        report.push_str(&format!("  Agent Hierarchy:   {}/{} ({:.0}%)\n",
            self.agent_hierarchy.found, self.agent_hierarchy.total, self.agent_hierarchy.percentage));
        report.push_str(&format!("  Error Recovery:    {}/{} ({:.0}%)\n",
            self.error_recovery.found, self.error_recovery.total, self.error_recovery.percentage));
        report.push_str(&format!("  System Metadata:   {}/{} ({:.0}%)\n",
            self.system_metadata.found, self.system_metadata.total, self.system_metadata.percentage));
        report.push_str(&format!("  Specialized:       {}/{} ({:.0}%)\n",
            self.specialized.found, self.specialized.total, self.specialized.percentage));

        if !self.recommendations.is_empty() {
            report.push_str("\nRecommendations:\n");
            for rec in &self.recommendations {
                report.push_str(&format!("  - {rec}\n"));
            }
        }

        report
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_message_counts() {
        let counts = MessageCounts {
            user: 10,
            assistant: 15,
            system: 2,
            ..Default::default()
        };

        assert_eq!(counts.total(), 27);
        assert_eq!(counts.conversation(), 25);
    }

    #[test]
    fn test_thinking_stats() {
        let stats = ThinkingStats {
            block_count: 5,
            total_chars: 500,
        };

        assert_eq!(stats.average_length(), 100);
    }

    #[test]
    fn test_cost_string() {
        let summary = AnalyticsSummary {
            duration: None,
            total_messages: 0,
            user_messages: 0,
            assistant_messages: 0,
            total_tokens: 0,
            input_tokens: 0,
            output_tokens: 0,
            tool_invocations: 0,
            unique_tools: 0,
            thinking_blocks: 0,
            error_count: 0,
            cache_hit_rate: 0.0,
            estimated_cost: Some(0.0042),
            branch_count: 0,
            primary_model: None,
        };

        assert_eq!(summary.cost_string(), "$0.0042");
    }

    #[test]
    fn test_usage_prediction() {
        let mut analytics = SessionAnalytics::default();
        // Simulate 1 hour session with 100K tokens, 50 messages
        analytics.start_time = Some(Utc::now() - Duration::hours(1));
        analytics.end_time = Some(Utc::now());
        analytics.usage.usage.output_tokens = 50_000;
        analytics.usage.usage.cache_read_input_tokens = Some(50_000);
        analytics.message_counts.user = 25;
        analytics.message_counts.assistant = 25;

        let prediction = analytics.predictions(Some(1_000_000)); // 1M limit

        // Should calculate approximately 100K tokens/hour
        assert!(prediction.tokens_per_hour > 90_000.0);
        assert!(prediction.tokens_per_hour < 110_000.0);

        // Should have hours_to_limit set
        assert!(prediction.hours_to_limit.is_some());

        // Format strings should work
        assert!(!prediction.rate_string().is_empty());
        assert!(!prediction.time_to_limit_string().is_empty());
        assert!(!prediction.usage_percentage_string().is_empty());
    }

    #[test]
    fn test_usage_prediction_formatting() {
        let prediction = UsagePrediction {
            tokens_per_hour: 50_000.0,
            messages_per_hour: 10.0,
            cost_per_hour: Some(0.50),
            hours_to_limit: Some(2.5),
            messages_to_limit: Some(25),
            usage_percentage: Some(75.0),
            daily_projection: 400_000.0,
            monthly_projection: 8_800_000.0,
            monthly_cost_projection: Some(88.0),
        };

        assert_eq!(prediction.rate_string(), "50.0K tokens/hr");
        assert_eq!(prediction.time_to_limit_string(), "2.5h");
        assert_eq!(prediction.usage_percentage_string(), "75.0%");
        assert_eq!(prediction.monthly_projection_string(), "8.8M tokens/month");
    }

    #[test]
    fn test_category_score() {
        let score = CategoryScore::new(7, 10);
        assert_eq!(score.found, 7);
        assert_eq!(score.total, 10);
        assert!((score.percentage - 70.0).abs() < 0.01);
    }

    #[test]
    fn test_category_score_full() {
        let score = CategoryScore::new(10, 10);
        assert!((score.percentage - 100.0).abs() < 0.01);
    }

    #[test]
    fn test_category_score_empty() {
        let score = CategoryScore::new(0, 10);
        assert!((score.percentage - 0.0).abs() < 0.01);
    }

    #[test]
    fn test_category_score_zero_total() {
        let score = CategoryScore::new(0, 0);
        assert!((score.percentage - 100.0).abs() < 0.01);
    }

    #[test]
    fn test_fidelity_score_default() {
        let score = FidelityScore::default();
        assert_eq!(score.overall_score, 0.0);
        assert_eq!(score.grade, '\0');
        assert!(score.recommendations.is_empty());
    }

    #[test]
    fn test_fidelity_grade_boundaries() {
        // Test grade calculation
        let mut score = FidelityScore::default();

        score.overall_score = 95.0;
        score.grade = match score.overall_score {
            s if s >= 90.0 => 'A',
            s if s >= 80.0 => 'B',
            s if s >= 70.0 => 'C',
            s if s >= 60.0 => 'D',
            _ => 'F',
        };
        assert_eq!(score.grade, 'A');

        score.overall_score = 85.0;
        score.grade = match score.overall_score {
            s if s >= 90.0 => 'A',
            s if s >= 80.0 => 'B',
            s if s >= 70.0 => 'C',
            s if s >= 60.0 => 'D',
            _ => 'F',
        };
        assert_eq!(score.grade, 'B');

        score.overall_score = 55.0;
        score.grade = match score.overall_score {
            s if s >= 90.0 => 'A',
            s if s >= 80.0 => 'B',
            s if s >= 70.0 => 'C',
            s if s >= 60.0 => 'D',
            _ => 'F',
        };
        assert_eq!(score.grade, 'F');
    }

    #[test]
    fn test_fidelity_report_format() {
        let mut score = FidelityScore::default();
        score.overall_score = 75.5;
        score.grade = 'C';
        score.core_content = CategoryScore::new(8, 10);
        score.identity_linking = CategoryScore::new(5, 7);
        score.recommendations.push("Test recommendation".to_string());

        let report = score.report();
        assert!(report.contains("Fidelity Score: 75.5%"));
        assert!(report.contains("Grade: C"));
        assert!(report.contains("Core Content:      8/10"));
        assert!(report.contains("Recommendations:"));
        assert!(report.contains("Test recommendation"));
    }
}
