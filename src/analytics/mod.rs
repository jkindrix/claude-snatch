//! Analytics and statistics for Claude Code sessions.
//!
//! This module provides:
//! - Token usage calculation
//! - Cache efficiency metrics
//! - Tool invocation tracking
//! - Cost estimation
//! - Session duration analysis


use chrono::{DateTime, Datelike, Duration, Utc};
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
    /// File modification tracking.
    pub file_stats: FileModificationStats,
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

        let timestamp = Some(assistant.timestamp);

        // Process content blocks
        for content in &assistant.message.content {
            match content {
                ContentBlock::ToolUse(tool_use) => {
                    self.message_counts.tool_uses += 1;
                    *self.tool_counts.entry(tool_use.name.clone()).or_insert(0) += 1;
                    self.usage.record_tool(&tool_use.name);

                    // Track file modifications for Edit and Write tools
                    self.track_file_modification(tool_use, timestamp);
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

    /// Track file modification from a tool use.
    fn track_file_modification(&mut self, tool_use: &crate::model::content::ToolUse, timestamp: Option<DateTime<Utc>>) {
        match tool_use.name.as_str() {
            "Edit" => {
                if let (Some(file_path), Some(old_string), Some(new_string)) = (
                    tool_use.input.get("file_path").and_then(|v| v.as_str()),
                    tool_use.input.get("old_string").and_then(|v| v.as_str()),
                    tool_use.input.get("new_string").and_then(|v| v.as_str()),
                ) {
                    self.file_stats.record_edit(file_path, old_string, new_string, timestamp);
                }
            }
            "Write" => {
                if let (Some(file_path), Some(content)) = (
                    tool_use.input.get("file_path").and_then(|v| v.as_str()),
                    tool_use.input.get("content").and_then(|v| v.as_str()),
                ) {
                    self.file_stats.record_write(file_path, content, timestamp);
                }
            }
            _ => {}
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

/// File modification statistics.
#[derive(Debug, Clone, Default)]
pub struct FileModificationStats {
    /// Files modified with modification count.
    pub files: IndexMap<String, FileModificationEntry>,
    /// File extensions modified with count.
    pub extensions: IndexMap<String, usize>,
    /// Total lines added.
    pub total_lines_added: usize,
    /// Total lines removed.
    pub total_lines_removed: usize,
    /// Total modifications.
    pub total_modifications: usize,
    /// Files created (Write tool).
    pub files_created: usize,
    /// Files edited (Edit tool).
    pub files_edited: usize,
}

/// Entry for a single file's modification history.
#[derive(Debug, Clone, Default)]
pub struct FileModificationEntry {
    /// Number of modifications.
    pub modification_count: usize,
    /// Lines added across all modifications.
    pub lines_added: usize,
    /// Lines removed across all modifications.
    pub lines_removed: usize,
    /// First modification timestamp.
    pub first_modified: Option<DateTime<Utc>>,
    /// Last modification timestamp.
    pub last_modified: Option<DateTime<Utc>>,
}

impl FileModificationStats {
    /// Record a file modification from an Edit tool call.
    pub fn record_edit(&mut self, file_path: &str, old_string: &str, new_string: &str, timestamp: Option<DateTime<Utc>>) {
        let entry = self.files.entry(file_path.to_string()).or_default();
        entry.modification_count += 1;

        // Calculate line changes
        let old_lines = old_string.lines().count();
        let new_lines = new_string.lines().count();
        if new_lines > old_lines {
            let added = new_lines - old_lines;
            entry.lines_added += added;
            self.total_lines_added += added;
        } else {
            let removed = old_lines - new_lines;
            entry.lines_removed += removed;
            self.total_lines_removed += removed;
        }

        // Update timestamps
        if let Some(ts) = timestamp {
            if entry.first_modified.is_none() || Some(ts) < entry.first_modified {
                entry.first_modified = Some(ts);
            }
            if entry.last_modified.is_none() || Some(ts) > entry.last_modified {
                entry.last_modified = Some(ts);
            }
        }

        // Track extension
        if let Some(ext) = std::path::Path::new(file_path).extension().and_then(|e| e.to_str()) {
            *self.extensions.entry(ext.to_string()).or_insert(0) += 1;
        }

        self.total_modifications += 1;
        self.files_edited += 1;
    }

    /// Record a file creation from a Write tool call.
    pub fn record_write(&mut self, file_path: &str, content: &str, timestamp: Option<DateTime<Utc>>) {
        let entry = self.files.entry(file_path.to_string()).or_default();
        entry.modification_count += 1;

        let lines = content.lines().count();
        entry.lines_added += lines;
        self.total_lines_added += lines;

        if let Some(ts) = timestamp {
            if entry.first_modified.is_none() || Some(ts) < entry.first_modified {
                entry.first_modified = Some(ts);
            }
            if entry.last_modified.is_none() || Some(ts) > entry.last_modified {
                entry.last_modified = Some(ts);
            }
        }

        // Track extension
        if let Some(ext) = std::path::Path::new(file_path).extension().and_then(|e| e.to_str()) {
            *self.extensions.entry(ext.to_string()).or_insert(0) += 1;
        }

        self.total_modifications += 1;
        self.files_created += 1;
    }

    /// Get top N most modified files.
    #[must_use]
    pub fn top_files(&self, n: usize) -> Vec<(&str, &FileModificationEntry)> {
        let mut files: Vec<_> = self.files.iter().collect();
        files.sort_by(|a, b| b.1.modification_count.cmp(&a.1.modification_count));
        files.into_iter().take(n).map(|(k, v)| (k.as_str(), v)).collect()
    }

    /// Get most common file extensions.
    #[must_use]
    pub fn top_extensions(&self, n: usize) -> Vec<(&str, usize)> {
        let mut exts: Vec<_> = self.extensions.iter().collect();
        exts.sort_by(|a, b| b.1.cmp(a.1));
        exts.into_iter().take(n).map(|(k, v)| (k.as_str(), *v)).collect()
    }

    /// Get total unique files modified.
    #[must_use]
    pub fn unique_files(&self) -> usize {
        self.files.len()
    }

    /// Get net line change (added - removed).
    #[must_use]
    pub fn net_lines(&self) -> i64 {
        self.total_lines_added as i64 - self.total_lines_removed as i64
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

/// Time bucket granularity for usage trends.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrendGranularity {
    /// Hourly buckets.
    Hourly,
    /// Daily buckets.
    Daily,
    /// Weekly buckets.
    Weekly,
    /// Monthly buckets.
    Monthly,
}

impl TrendGranularity {
    /// Get bucket key for a timestamp.
    #[must_use]
    pub fn bucket_key(&self, timestamp: DateTime<Utc>) -> String {
        match self {
            Self::Hourly => timestamp.format("%Y-%m-%d %H:00").to_string(),
            Self::Daily => timestamp.format("%Y-%m-%d").to_string(),
            Self::Weekly => {
                let week = timestamp.iso_week();
                format!("{}-W{:02}", week.year(), week.week())
            }
            Self::Monthly => timestamp.format("%Y-%m").to_string(),
        }
    }
}

/// Data point in a usage trend.
#[derive(Debug, Clone, Default)]
pub struct TrendDataPoint {
    /// Time bucket label.
    pub bucket: String,
    /// Token count in this bucket.
    pub tokens: u64,
    /// Message count in this bucket.
    pub messages: usize,
    /// Tool invocations in this bucket.
    pub tool_uses: usize,
    /// Session count in this bucket.
    pub sessions: usize,
    /// Estimated cost in this bucket.
    pub cost: Option<f64>,
}

/// Usage trends over time.
#[derive(Debug, Clone, Default)]
pub struct UsageTrends {
    /// Trend granularity.
    pub granularity: Option<TrendGranularity>,
    /// Data points ordered by time.
    pub data_points: Vec<TrendDataPoint>,
    /// Peak tokens in a single bucket.
    pub peak_tokens: u64,
    /// Peak bucket label.
    pub peak_bucket: Option<String>,
    /// Average tokens per bucket.
    pub avg_tokens: f64,
    /// Total tokens across all buckets.
    pub total_tokens: u64,
    /// Trend direction: positive = increasing, negative = decreasing.
    pub trend_direction: f64,
}

impl UsageTrends {
    /// Create trends from a list of sessions.
    pub fn from_sessions(
        sessions: &[(DateTime<Utc>, SessionAnalytics)],
        granularity: TrendGranularity,
    ) -> Self {
        let mut buckets: IndexMap<String, TrendDataPoint> = IndexMap::new();

        for (timestamp, analytics) in sessions {
            let bucket_key = granularity.bucket_key(*timestamp);
            let entry = buckets.entry(bucket_key.clone()).or_insert_with(|| TrendDataPoint {
                bucket: bucket_key,
                ..Default::default()
            });

            entry.tokens += analytics.usage.usage.total_tokens();
            entry.messages += analytics.message_counts.total();
            entry.tool_uses += analytics.message_counts.tool_uses;
            entry.sessions += 1;
            if let Some(cost) = analytics.usage.estimated_cost {
                *entry.cost.get_or_insert(0.0) += cost;
            }
        }

        // Sort buckets by key (chronological order)
        buckets.sort_keys();

        let data_points: Vec<TrendDataPoint> = buckets.into_values().collect();

        // Calculate statistics
        let total_tokens: u64 = data_points.iter().map(|d| d.tokens).sum();
        let avg_tokens = if data_points.is_empty() {
            0.0
        } else {
            total_tokens as f64 / data_points.len() as f64
        };

        let (peak_tokens, peak_bucket) = data_points
            .iter()
            .max_by_key(|d| d.tokens)
            .map(|d| (d.tokens, Some(d.bucket.clone())))
            .unwrap_or((0, None));

        // Calculate trend direction using linear regression slope
        let trend_direction = if data_points.len() >= 2 {
            let n = data_points.len() as f64;
            let sum_x: f64 = (0..data_points.len()).map(|i| i as f64).sum();
            let sum_y: f64 = data_points.iter().map(|d| d.tokens as f64).sum();
            let sum_xy: f64 = data_points.iter().enumerate()
                .map(|(i, d)| i as f64 * d.tokens as f64)
                .sum();
            let sum_x2: f64 = (0..data_points.len()).map(|i| (i * i) as f64).sum();

            let slope = (n * sum_xy - sum_x * sum_y) / (n * sum_x2 - sum_x * sum_x);
            slope
        } else {
            0.0
        };

        Self {
            granularity: Some(granularity),
            data_points,
            peak_tokens,
            peak_bucket,
            avg_tokens,
            total_tokens,
            trend_direction,
        }
    }

    /// Get trend description.
    #[must_use]
    pub fn trend_description(&self) -> &'static str {
        if self.trend_direction > 100.0 {
            "Strongly increasing"
        } else if self.trend_direction > 10.0 {
            "Increasing"
        } else if self.trend_direction > -10.0 {
            "Stable"
        } else if self.trend_direction > -100.0 {
            "Decreasing"
        } else {
            "Strongly decreasing"
        }
    }

    /// Format as a simple ASCII chart.
    #[must_use]
    pub fn ascii_chart(&self, width: usize) -> String {
        if self.data_points.is_empty() || self.peak_tokens == 0 {
            return "No data available".to_string();
        }

        let mut chart = String::new();
        let max_label_len = self.data_points.iter()
            .map(|d| d.bucket.len())
            .max()
            .unwrap_or(10);

        let bar_width = width.saturating_sub(max_label_len + 3);

        for point in &self.data_points {
            let bar_len = ((point.tokens as f64 / self.peak_tokens as f64) * bar_width as f64) as usize;
            let bar = "█".repeat(bar_len.max(1));
            chart.push_str(&format!(
                "{:>width$} │{}\n",
                point.bucket,
                bar,
                width = max_label_len
            ));
        }

        chart
    }
}

/// Response time statistics.
#[derive(Debug, Clone, Default)]
pub struct ResponseTimeStats {
    /// Average time between user message and assistant response (seconds).
    pub avg_response_time_secs: f64,
    /// Minimum response time.
    pub min_response_time_secs: f64,
    /// Maximum response time.
    pub max_response_time_secs: f64,
    /// Median response time.
    pub median_response_time_secs: f64,
    /// 95th percentile response time.
    pub p95_response_time_secs: f64,
    /// Number of response pairs analyzed.
    pub sample_count: usize,
}

impl ResponseTimeStats {
    /// Calculate response time statistics from a conversation.
    pub fn from_conversation(conversation: &Conversation) -> Self {
        let mut response_times: Vec<f64> = Vec::new();

        // Get entries sorted by timestamp
        let mut entries: Vec<_> = conversation.nodes().values()
            .map(|n| &n.entry)
            .collect();

        entries.sort_by_key(|e| e.timestamp());

        // Find pairs of user -> assistant messages
        let mut prev_user_time: Option<DateTime<Utc>> = None;

        for entry in &entries {
            match entry {
                LogEntry::User(user) => {
                    prev_user_time = Some(user.timestamp);
                }
                LogEntry::Assistant(assistant) => {
                    if let Some(user_time) = prev_user_time {
                        let response_time = (assistant.timestamp - user_time).num_milliseconds() as f64 / 1000.0;
                        if response_time > 0.0 && response_time < 3600.0 {
                            // Only count reasonable response times (< 1 hour)
                            response_times.push(response_time);
                        }
                    }
                    prev_user_time = None;
                }
                _ => {}
            }
        }

        if response_times.is_empty() {
            return Self::default();
        }

        response_times.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));

        let sum: f64 = response_times.iter().sum();
        let count = response_times.len();

        let median_idx = count / 2;
        let p95_idx = (count as f64 * 0.95) as usize;

        Self {
            avg_response_time_secs: sum / count as f64,
            min_response_time_secs: *response_times.first().unwrap_or(&0.0),
            max_response_time_secs: *response_times.last().unwrap_or(&0.0),
            median_response_time_secs: response_times.get(median_idx).copied().unwrap_or(0.0),
            p95_response_time_secs: response_times.get(p95_idx).copied().unwrap_or(0.0),
            sample_count: count,
        }
    }

    /// Format as a readable string.
    #[must_use]
    pub fn summary(&self) -> String {
        if self.sample_count == 0 {
            return "No response time data available".to_string();
        }

        format!(
            "Response times: avg={:.1}s, median={:.1}s, p95={:.1}s (n={})",
            self.avg_response_time_secs,
            self.median_response_time_secs,
            self.p95_response_time_secs,
            self.sample_count
        )
    }
}

/// Cross-session efficiency metrics.
#[derive(Debug, Clone, Default)]
pub struct EfficiencyMetrics {
    /// Average tokens per message.
    pub tokens_per_message: f64,
    /// Average tool uses per session.
    pub tool_uses_per_session: f64,
    /// Cache hit rate across all sessions.
    pub overall_cache_hit_rate: f64,
    /// Average session duration.
    pub avg_session_duration_mins: f64,
    /// Thinking tokens ratio (thinking / total output).
    pub thinking_ratio: f64,
    /// Tool efficiency (successful / total tool uses).
    pub tool_success_rate: f64,
}

impl EfficiencyMetrics {
    /// Calculate efficiency metrics from project analytics.
    pub fn from_project(analytics: &ProjectAnalytics) -> Self {
        let total_messages = analytics.message_counts.total();
        let total_tokens = analytics.total_usage.usage.total_tokens();

        let tokens_per_message = if total_messages > 0 {
            total_tokens as f64 / total_messages as f64
        } else {
            0.0
        };

        let tool_uses_per_session = if analytics.session_count > 0 {
            analytics.message_counts.tool_uses as f64 / analytics.session_count as f64
        } else {
            0.0
        };

        let overall_cache_hit_rate = {
            let usage = &analytics.total_usage.usage;
            let total_input = usage.total_input_tokens();
            let cache_read = usage.cache_read_input_tokens.unwrap_or(0);
            if total_input > 0 {
                cache_read as f64 / total_input as f64 * 100.0
            } else {
                0.0
            }
        };

        let avg_session_duration_mins = if analytics.session_count > 0 {
            analytics.total_duration.num_minutes() as f64 / analytics.session_count as f64
        } else {
            0.0
        };

        // Thinking ratio is hard to calculate without tracking thinking tokens separately
        let thinking_ratio = 0.0; // Placeholder

        // Tool success rate - we'd need error tracking for this
        let tool_success_rate = 100.0; // Placeholder - assume 100% for now

        Self {
            tokens_per_message,
            tool_uses_per_session,
            overall_cache_hit_rate,
            avg_session_duration_mins,
            thinking_ratio,
            tool_success_rate,
        }
    }

    /// Format as a readable report.
    #[must_use]
    pub fn report(&self) -> String {
        format!(
            "Efficiency Metrics:\n\
             - Tokens per message:     {:.0}\n\
             - Tool uses per session:  {:.1}\n\
             - Cache hit rate:         {:.1}%\n\
             - Avg session duration:   {:.0} minutes",
            self.tokens_per_message,
            self.tool_uses_per_session,
            self.overall_cache_hit_rate,
            self.avg_session_duration_mins
        )
    }
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

    #[test]
    fn test_trend_granularity_bucket_keys() {
        let timestamp = DateTime::parse_from_rfc3339("2025-06-15T14:30:00Z")
            .unwrap()
            .with_timezone(&Utc);

        assert_eq!(TrendGranularity::Hourly.bucket_key(timestamp), "2025-06-15 14:00");
        assert_eq!(TrendGranularity::Daily.bucket_key(timestamp), "2025-06-15");
        assert_eq!(TrendGranularity::Monthly.bucket_key(timestamp), "2025-06");
    }

    #[test]
    fn test_usage_trends_empty() {
        let trends = UsageTrends::from_sessions(&[], TrendGranularity::Daily);
        assert!(trends.data_points.is_empty());
        assert_eq!(trends.peak_tokens, 0);
        assert_eq!(trends.total_tokens, 0);
        assert_eq!(trends.trend_direction, 0.0);
    }

    #[test]
    fn test_usage_trends_single_session() {
        let timestamp = Utc::now();
        let mut analytics = SessionAnalytics::default();
        analytics.usage.usage.output_tokens = 1000;
        analytics.message_counts.user = 5;

        let sessions = vec![(timestamp, analytics)];
        let trends = UsageTrends::from_sessions(&sessions, TrendGranularity::Daily);

        assert_eq!(trends.data_points.len(), 1);
        assert_eq!(trends.total_tokens, 1000);
        assert_eq!(trends.peak_tokens, 1000);
    }

    #[test]
    fn test_usage_trends_trend_direction() {
        // Create sessions with increasing token usage
        let mut sessions = Vec::new();
        for i in 0..5 {
            let timestamp = Utc::now() - Duration::days(4 - i);
            let mut analytics = SessionAnalytics::default();
            analytics.usage.usage.output_tokens = (i as u64 + 1) * 1000;
            sessions.push((timestamp, analytics));
        }

        let trends = UsageTrends::from_sessions(&sessions, TrendGranularity::Daily);

        // Trend should be positive (increasing)
        assert!(trends.trend_direction > 0.0);
    }

    #[test]
    fn test_usage_trends_ascii_chart() {
        let trends = UsageTrends {
            granularity: Some(TrendGranularity::Daily),
            data_points: vec![
                TrendDataPoint { bucket: "2025-01".to_string(), tokens: 100, ..Default::default() },
                TrendDataPoint { bucket: "2025-02".to_string(), tokens: 200, ..Default::default() },
            ],
            peak_tokens: 200,
            peak_bucket: Some("2025-02".to_string()),
            avg_tokens: 150.0,
            total_tokens: 300,
            trend_direction: 100.0,
        };

        let chart = trends.ascii_chart(40);
        assert!(chart.contains("2025-01"));
        assert!(chart.contains("2025-02"));
        assert!(chart.contains("█"));
    }

    #[test]
    fn test_trend_description() {
        let mut trends = UsageTrends::default();

        trends.trend_direction = 150.0;
        assert_eq!(trends.trend_description(), "Strongly increasing");

        trends.trend_direction = 50.0;
        assert_eq!(trends.trend_description(), "Increasing");

        trends.trend_direction = 0.0;
        assert_eq!(trends.trend_description(), "Stable");

        trends.trend_direction = -50.0;
        assert_eq!(trends.trend_description(), "Decreasing");

        trends.trend_direction = -150.0;
        assert_eq!(trends.trend_description(), "Strongly decreasing");
    }

    #[test]
    fn test_response_time_stats_empty() {
        let stats = ResponseTimeStats::default();
        assert_eq!(stats.sample_count, 0);
        assert_eq!(stats.avg_response_time_secs, 0.0);
    }

    #[test]
    fn test_response_time_stats_summary() {
        let stats = ResponseTimeStats {
            avg_response_time_secs: 5.5,
            min_response_time_secs: 1.0,
            max_response_time_secs: 15.0,
            median_response_time_secs: 4.0,
            p95_response_time_secs: 12.0,
            sample_count: 100,
        };

        let summary = stats.summary();
        assert!(summary.contains("avg=5.5s"));
        assert!(summary.contains("median=4.0s"));
        assert!(summary.contains("p95=12.0s"));
        assert!(summary.contains("n=100"));
    }

    #[test]
    fn test_efficiency_metrics_default() {
        let metrics = EfficiencyMetrics::default();
        assert_eq!(metrics.tokens_per_message, 0.0);
        assert_eq!(metrics.tool_uses_per_session, 0.0);
    }

    #[test]
    fn test_efficiency_metrics_from_project() {
        let mut analytics = ProjectAnalytics::default();
        analytics.session_count = 10;
        analytics.message_counts.user = 50;
        analytics.message_counts.assistant = 50;
        analytics.message_counts.tool_uses = 30;
        analytics.total_usage.usage.output_tokens = 10000;
        analytics.total_usage.usage.cache_read_input_tokens = Some(5000);
        analytics.total_usage.usage.input_tokens = 10000;
        analytics.total_duration = Duration::hours(10);

        let metrics = EfficiencyMetrics::from_project(&analytics);

        assert!(metrics.tokens_per_message > 0.0);
        assert_eq!(metrics.tool_uses_per_session, 3.0);
        assert!(metrics.overall_cache_hit_rate > 0.0);
        assert_eq!(metrics.avg_session_duration_mins, 60.0);
    }

    #[test]
    fn test_efficiency_metrics_report() {
        let metrics = EfficiencyMetrics {
            tokens_per_message: 500.0,
            tool_uses_per_session: 5.0,
            overall_cache_hit_rate: 75.0,
            avg_session_duration_mins: 30.0,
            thinking_ratio: 0.0,
            tool_success_rate: 100.0,
        };

        let report = metrics.report();
        assert!(report.contains("Efficiency Metrics:"));
        assert!(report.contains("Tokens per message:     500"));
        assert!(report.contains("Tool uses per session:  5.0"));
        assert!(report.contains("Cache hit rate:         75.0%"));
    }

    #[test]
    fn test_file_modification_stats_default() {
        let stats = FileModificationStats::default();
        assert_eq!(stats.unique_files(), 0);
        assert_eq!(stats.total_modifications, 0);
        assert_eq!(stats.net_lines(), 0);
    }

    #[test]
    fn test_file_modification_stats_record_edit() {
        let mut stats = FileModificationStats::default();
        stats.record_edit(
            "/src/main.rs",
            "fn main() {\n    println!(\"hello\");\n}",
            "fn main() {\n    println!(\"hello, world!\");\n    println!(\"extra line\");\n}",
            None,
        );

        assert_eq!(stats.unique_files(), 1);
        assert_eq!(stats.files_edited, 1);
        assert_eq!(stats.total_modifications, 1);
        assert_eq!(stats.total_lines_added, 1); // 4 lines - 3 lines = 1 added

        // Check extension tracking
        let exts = stats.top_extensions(5);
        assert_eq!(exts.len(), 1);
        assert_eq!(exts[0].0, "rs");
    }

    #[test]
    fn test_file_modification_stats_record_write() {
        let mut stats = FileModificationStats::default();
        stats.record_write(
            "/src/new_file.py",
            "def hello():\n    print('hello')\n\nhello()",
            None,
        );

        assert_eq!(stats.unique_files(), 1);
        assert_eq!(stats.files_created, 1);
        assert_eq!(stats.total_modifications, 1);
        assert_eq!(stats.total_lines_added, 4);

        // Check extension tracking
        let exts = stats.top_extensions(5);
        assert_eq!(exts.len(), 1);
        assert_eq!(exts[0].0, "py");
    }

    #[test]
    fn test_file_modification_stats_multiple_edits() {
        let mut stats = FileModificationStats::default();

        // Edit same file multiple times
        stats.record_edit("/src/lib.rs", "a", "ab", None);
        stats.record_edit("/src/lib.rs", "ab", "abc", None);
        stats.record_edit("/src/mod.rs", "x", "y", None);

        assert_eq!(stats.unique_files(), 2);
        assert_eq!(stats.total_modifications, 3);

        // lib.rs should have 2 modifications
        let top = stats.top_files(5);
        assert_eq!(top.len(), 2);
        assert_eq!(top[0].0, "/src/lib.rs");
        assert_eq!(top[0].1.modification_count, 2);
    }

    #[test]
    fn test_file_modification_stats_net_lines() {
        let mut stats = FileModificationStats::default();

        // Add 5 lines
        stats.record_write("/test.rs", "1\n2\n3\n4\n5", None);
        // Remove 2 lines
        stats.record_edit("/test.rs", "1\n2\n3\n4\n5", "1\n2\n3", None);

        assert_eq!(stats.total_lines_added, 5);
        assert_eq!(stats.total_lines_removed, 2);
        assert_eq!(stats.net_lines(), 3);
    }

    #[test]
    fn test_file_modification_entry_timestamps() {
        use chrono::TimeZone;
        let mut stats = FileModificationStats::default();

        let ts1 = Utc.with_ymd_and_hms(2025, 1, 1, 10, 0, 0).unwrap();
        let ts2 = Utc.with_ymd_and_hms(2025, 1, 1, 12, 0, 0).unwrap();

        stats.record_edit("/file.rs", "a", "b", Some(ts1));
        stats.record_edit("/file.rs", "b", "c", Some(ts2));

        let entry = &stats.files["/file.rs"];
        assert_eq!(entry.first_modified, Some(ts1));
        assert_eq!(entry.last_modified, Some(ts2));
    }
}
