//! Usage statistics for Claude Code JSONL logs.
//!
//! This module defines token usage structures including:
//! - Input/output tokens
//! - Cache creation/read tokens
//! - Ephemeral cache tokens (5m and 1h)
//! - Server tool usage (web_search, web_fetch)

use indexmap::IndexMap;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Token usage statistics.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct Usage {
    /// Fresh (non-cached) input tokens.
    #[serde(default)]
    pub input_tokens: u64,

    /// Generated output tokens.
    #[serde(default)]
    pub output_tokens: u64,

    /// Tokens used to build cache.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_creation_input_tokens: Option<u64>,

    /// Tokens retrieved from cache.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_read_input_tokens: Option<u64>,

    /// Service classification ("standard", etc.).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub service_tier: Option<String>,

    /// Ephemeral cache breakdown.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_creation: Option<CacheCreationDetails>,

    /// Server tool usage.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub server_tool_use: Option<ServerToolUse>,

    /// Unknown fields for forward compatibility.
    #[serde(flatten)]
    pub extra: IndexMap<String, Value>,
}

impl Usage {
    /// Calculate total input tokens (fresh + cached).
    #[must_use]
    pub fn total_input_tokens(&self) -> u64 {
        self.input_tokens
            + self.cache_creation_input_tokens.unwrap_or(0)
            + self.cache_read_input_tokens.unwrap_or(0)
    }

    /// Calculate total tokens (input + output).
    #[must_use]
    pub fn total_tokens(&self) -> u64 {
        self.total_input_tokens() + self.output_tokens
    }

    /// Calculate cache hit rate as a percentage.
    #[must_use]
    pub fn cache_hit_rate(&self) -> f64 {
        let total_cache = self.cache_creation_input_tokens.unwrap_or(0)
            + self.cache_read_input_tokens.unwrap_or(0);
        if total_cache == 0 {
            return 0.0;
        }
        let cache_read = self.cache_read_input_tokens.unwrap_or(0) as f64;
        (cache_read / total_cache as f64) * 100.0
    }

    /// Calculate cache efficiency (read/write ratio).
    #[must_use]
    pub fn cache_efficiency(&self) -> Option<f64> {
        let write = self.cache_creation_input_tokens?;
        let read = self.cache_read_input_tokens.unwrap_or(0);
        if write == 0 {
            return None;
        }
        Some(read as f64 / write as f64)
    }

    /// Check if this usage includes any caching.
    #[must_use]
    pub fn has_caching(&self) -> bool {
        self.cache_creation_input_tokens.is_some() || self.cache_read_input_tokens.is_some()
    }

    /// Check if this usage includes server tool usage.
    #[must_use]
    pub fn has_server_tool_use(&self) -> bool {
        self.server_tool_use.is_some()
    }

    /// Get total web search requests.
    #[must_use]
    pub fn web_search_requests(&self) -> u32 {
        self.server_tool_use
            .as_ref()
            .and_then(|s| s.web_search_requests)
            .unwrap_or(0)
    }

    /// Get total web fetch requests.
    #[must_use]
    pub fn web_fetch_requests(&self) -> u32 {
        self.server_tool_use
            .as_ref()
            .and_then(|s| s.web_fetch_requests)
            .unwrap_or(0)
    }

    /// Merge another Usage into this one (accumulate).
    pub fn merge(&mut self, other: &Self) {
        self.input_tokens += other.input_tokens;
        self.output_tokens += other.output_tokens;

        if let Some(other_cache_creation) = other.cache_creation_input_tokens {
            *self.cache_creation_input_tokens.get_or_insert(0) += other_cache_creation;
        }

        if let Some(other_cache_read) = other.cache_read_input_tokens {
            *self.cache_read_input_tokens.get_or_insert(0) += other_cache_read;
        }

        if let Some(other_cache) = &other.cache_creation {
            let cache = self.cache_creation.get_or_insert_with(CacheCreationDetails::default);
            if let Some(tokens) = other_cache.ephemeral_5m_input_tokens {
                *cache.ephemeral_5m_input_tokens.get_or_insert(0) += tokens;
            }
            if let Some(tokens) = other_cache.ephemeral_1h_input_tokens {
                *cache.ephemeral_1h_input_tokens.get_or_insert(0) += tokens;
            }
        }

        if let Some(other_tools) = &other.server_tool_use {
            let tools = self.server_tool_use.get_or_insert_with(ServerToolUse::default);
            if let Some(count) = other_tools.web_search_requests {
                *tools.web_search_requests.get_or_insert(0) += count;
            }
            if let Some(count) = other_tools.web_fetch_requests {
                *tools.web_fetch_requests.get_or_insert(0) += count;
            }
        }
    }
}

/// Ephemeral cache details.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct CacheCreationDetails {
    /// 5-minute ephemeral cache tokens.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ephemeral_5m_input_tokens: Option<u64>,

    /// 1-hour ephemeral cache tokens.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ephemeral_1h_input_tokens: Option<u64>,

    /// Unknown fields for forward compatibility.
    #[serde(flatten)]
    pub extra: IndexMap<String, Value>,
}

impl CacheCreationDetails {
    /// Get total ephemeral tokens.
    #[must_use]
    pub fn total_ephemeral_tokens(&self) -> u64 {
        self.ephemeral_5m_input_tokens.unwrap_or(0) + self.ephemeral_1h_input_tokens.unwrap_or(0)
    }
}

/// Server tool usage statistics.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct ServerToolUse {
    /// WebSearch API call count.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub web_search_requests: Option<u32>,

    /// WebFetch API call count.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub web_fetch_requests: Option<u32>,

    /// Unknown fields for forward compatibility.
    #[serde(flatten)]
    pub extra: IndexMap<String, Value>,
}

impl ServerToolUse {
    /// Get total server tool requests.
    #[must_use]
    pub fn total_requests(&self) -> u32 {
        self.web_search_requests.unwrap_or(0) + self.web_fetch_requests.unwrap_or(0)
    }
}

/// Cost estimation for token usage.
#[derive(Debug, Clone, Default)]
pub struct CostEstimate {
    /// Cost for input tokens.
    pub input_cost: f64,
    /// Cost for output tokens.
    pub output_cost: f64,
    /// Cost for cache writes.
    pub cache_write_cost: f64,
    /// Cost for cache reads (typically cheaper).
    pub cache_read_cost: f64,
    /// Total cost.
    pub total_cost: f64,
    /// Currency (typically "USD").
    pub currency: String,
}

/// Pricing configuration per model.
#[derive(Debug, Clone)]
pub struct ModelPricing {
    /// Model identifier.
    pub model: String,
    /// Cost per million input tokens.
    pub input_per_million: f64,
    /// Cost per million output tokens.
    pub output_per_million: f64,
    /// Cost per million cache write tokens.
    pub cache_write_per_million: f64,
    /// Cost per million cache read tokens.
    pub cache_read_per_million: f64,
}

impl ModelPricing {
    /// Create pricing for Claude Opus 4.5.
    #[must_use]
    pub fn claude_opus_4_5() -> Self {
        Self {
            model: "claude-opus-4-5-20251101".to_string(),
            input_per_million: 15.0,    // $15/M input
            output_per_million: 75.0,   // $75/M output
            cache_write_per_million: 18.75, // 1.25x input
            cache_read_per_million: 1.5,    // 0.1x input
        }
    }

    /// Create pricing for Claude Sonnet 4.
    #[must_use]
    pub fn claude_sonnet_4() -> Self {
        Self {
            model: "claude-sonnet-4-20250514".to_string(),
            input_per_million: 3.0,     // $3/M input
            output_per_million: 15.0,   // $15/M output
            cache_write_per_million: 3.75,  // 1.25x input
            cache_read_per_million: 0.3,    // 0.1x input
        }
    }

    /// Create pricing for Claude Haiku 3.5.
    #[must_use]
    pub fn claude_haiku_3_5() -> Self {
        Self {
            model: "claude-3-5-haiku-20241022".to_string(),
            input_per_million: 1.0,     // $1/M input
            output_per_million: 5.0,    // $5/M output
            cache_write_per_million: 1.25,  // 1.25x input
            cache_read_per_million: 0.1,    // 0.1x input
        }
    }

    /// Calculate cost for given usage.
    #[must_use]
    pub fn calculate_cost(&self, usage: &Usage) -> CostEstimate {
        let input_cost = (usage.input_tokens as f64 / 1_000_000.0) * self.input_per_million;
        let output_cost = (usage.output_tokens as f64 / 1_000_000.0) * self.output_per_million;
        let cache_write_cost = (usage.cache_creation_input_tokens.unwrap_or(0) as f64 / 1_000_000.0)
            * self.cache_write_per_million;
        let cache_read_cost = (usage.cache_read_input_tokens.unwrap_or(0) as f64 / 1_000_000.0)
            * self.cache_read_per_million;

        let total_cost = input_cost + output_cost + cache_write_cost + cache_read_cost;

        CostEstimate {
            input_cost,
            output_cost,
            cache_write_cost,
            cache_read_cost,
            total_cost,
            currency: "USD".to_string(),
        }
    }

    /// Get pricing for a model by name.
    #[must_use]
    pub fn for_model(model: &str) -> Option<Self> {
        if model.contains("opus") {
            Some(Self::claude_opus_4_5())
        } else if model.contains("sonnet") {
            Some(Self::claude_sonnet_4())
        } else if model.contains("haiku") {
            Some(Self::claude_haiku_3_5())
        } else {
            None
        }
    }
}

/// Aggregated usage statistics for a session or project.
#[derive(Debug, Clone, Default)]
pub struct AggregatedUsage {
    /// Total messages processed.
    pub message_count: usize,
    /// Combined usage statistics.
    pub usage: Usage,
    /// Usage breakdown by model.
    pub by_model: IndexMap<String, Usage>,
    /// Total tool invocations.
    pub tool_invocations: usize,
    /// Tool invocations by name.
    pub tools_by_name: IndexMap<String, usize>,
    /// Total errors encountered.
    pub error_count: usize,
    /// Estimated total cost.
    pub estimated_cost: Option<f64>,
}

impl AggregatedUsage {
    /// Add usage from a single message.
    pub fn add_usage(&mut self, model: &str, usage: &Usage) {
        self.message_count += 1;
        self.usage.merge(usage);

        let model_usage = self.by_model.entry(model.to_string()).or_default();
        model_usage.merge(usage);
    }

    /// Record a tool invocation.
    pub fn record_tool(&mut self, tool_name: &str) {
        self.tool_invocations += 1;
        *self.tools_by_name.entry(tool_name.to_string()).or_insert(0) += 1;
    }

    /// Calculate estimated cost based on model usage.
    pub fn calculate_cost(&mut self) {
        let mut total = 0.0;

        for (model, usage) in &self.by_model {
            if let Some(pricing) = ModelPricing::for_model(model) {
                let cost = pricing.calculate_cost(usage);
                total += cost.total_cost;
            }
        }

        self.estimated_cost = Some(total);
    }

    /// Get the most used tool.
    #[must_use]
    pub fn most_used_tool(&self) -> Option<(&str, usize)> {
        self.tools_by_name
            .iter()
            .max_by_key(|(_, count)| *count)
            .map(|(name, count)| (name.as_str(), *count))
    }

    /// Get the most used model.
    #[must_use]
    pub fn most_used_model(&self) -> Option<(&str, u64)> {
        self.by_model
            .iter()
            .max_by_key(|(_, usage)| usage.total_tokens())
            .map(|(model, usage)| (model.as_str(), usage.total_tokens()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_usage_totals() {
        let usage = Usage {
            input_tokens: 100,
            output_tokens: 50,
            cache_creation_input_tokens: Some(200),
            cache_read_input_tokens: Some(150),
            ..Default::default()
        };

        assert_eq!(usage.total_input_tokens(), 450);
        assert_eq!(usage.total_tokens(), 500);
    }

    #[test]
    fn test_cache_hit_rate() {
        let usage = Usage {
            input_tokens: 100,
            output_tokens: 50,
            cache_creation_input_tokens: Some(100),
            cache_read_input_tokens: Some(300),
            ..Default::default()
        };

        let hit_rate = usage.cache_hit_rate();
        assert!((hit_rate - 75.0).abs() < 0.1);
    }

    #[test]
    fn test_cost_calculation() {
        let usage = Usage {
            input_tokens: 1_000_000,
            output_tokens: 100_000,
            cache_creation_input_tokens: Some(500_000),
            cache_read_input_tokens: Some(2_000_000),
            ..Default::default()
        };

        let pricing = ModelPricing::claude_sonnet_4();
        let cost = pricing.calculate_cost(&usage);

        // Input: 1M * $3/M = $3
        // Output: 0.1M * $15/M = $1.5
        // Cache write: 0.5M * $3.75/M = $1.875
        // Cache read: 2M * $0.3/M = $0.6
        // Total: $7.975
        assert!((cost.input_cost - 3.0).abs() < 0.001);
        assert!((cost.output_cost - 1.5).abs() < 0.001);
    }

    #[test]
    fn test_usage_merge() {
        let mut usage1 = Usage {
            input_tokens: 100,
            output_tokens: 50,
            ..Default::default()
        };

        let usage2 = Usage {
            input_tokens: 200,
            output_tokens: 100,
            cache_read_input_tokens: Some(50),
            ..Default::default()
        };

        usage1.merge(&usage2);

        assert_eq!(usage1.input_tokens, 300);
        assert_eq!(usage1.output_tokens, 150);
        assert_eq!(usage1.cache_read_input_tokens, Some(50));
    }

    #[test]
    fn test_usage_default() {
        let usage = Usage::default();
        assert_eq!(usage.input_tokens, 0);
        assert_eq!(usage.output_tokens, 0);
        assert_eq!(usage.total_tokens(), 0);
        assert!(!usage.has_caching());
        assert!(!usage.has_server_tool_use());
    }

    #[test]
    fn test_usage_cache_hit_rate_no_cache() {
        let usage = Usage::default();
        assert_eq!(usage.cache_hit_rate(), 0.0);
    }

    #[test]
    fn test_usage_cache_efficiency() {
        let usage = Usage {
            cache_creation_input_tokens: Some(100),
            cache_read_input_tokens: Some(500),
            ..Default::default()
        };

        let efficiency = usage.cache_efficiency().unwrap();
        assert!((efficiency - 5.0).abs() < 0.01);
    }

    #[test]
    fn test_usage_cache_efficiency_no_write() {
        let usage = Usage {
            cache_creation_input_tokens: Some(0),
            cache_read_input_tokens: Some(500),
            ..Default::default()
        };

        assert!(usage.cache_efficiency().is_none());
    }

    #[test]
    fn test_usage_cache_efficiency_no_cache() {
        let usage = Usage::default();
        assert!(usage.cache_efficiency().is_none());
    }

    #[test]
    fn test_usage_has_caching() {
        let mut usage = Usage::default();
        assert!(!usage.has_caching());

        usage.cache_creation_input_tokens = Some(100);
        assert!(usage.has_caching());

        usage = Usage::default();
        usage.cache_read_input_tokens = Some(100);
        assert!(usage.has_caching());
    }

    #[test]
    fn test_usage_web_search_requests() {
        let usage = Usage {
            server_tool_use: Some(ServerToolUse {
                web_search_requests: Some(5),
                ..Default::default()
            }),
            ..Default::default()
        };

        assert_eq!(usage.web_search_requests(), 5);
        assert!(usage.has_server_tool_use());
    }

    #[test]
    fn test_usage_web_fetch_requests() {
        let usage = Usage {
            server_tool_use: Some(ServerToolUse {
                web_fetch_requests: Some(3),
                ..Default::default()
            }),
            ..Default::default()
        };

        assert_eq!(usage.web_fetch_requests(), 3);
    }

    #[test]
    fn test_usage_web_requests_no_server_use() {
        let usage = Usage::default();
        assert_eq!(usage.web_search_requests(), 0);
        assert_eq!(usage.web_fetch_requests(), 0);
    }

    #[test]
    fn test_cache_creation_details_total() {
        let details = CacheCreationDetails {
            ephemeral_5m_input_tokens: Some(100),
            ephemeral_1h_input_tokens: Some(200),
            extra: IndexMap::new(),
        };

        assert_eq!(details.total_ephemeral_tokens(), 300);
    }

    #[test]
    fn test_cache_creation_details_default() {
        let details = CacheCreationDetails::default();
        assert_eq!(details.total_ephemeral_tokens(), 0);
    }

    #[test]
    fn test_server_tool_use_total() {
        let tools = ServerToolUse {
            web_search_requests: Some(10),
            web_fetch_requests: Some(5),
            extra: IndexMap::new(),
        };

        assert_eq!(tools.total_requests(), 15);
    }

    #[test]
    fn test_server_tool_use_default() {
        let tools = ServerToolUse::default();
        assert_eq!(tools.total_requests(), 0);
    }

    #[test]
    fn test_model_pricing_opus() {
        let pricing = ModelPricing::claude_opus_4_5();
        assert!(pricing.model.contains("opus"));
        assert!(pricing.input_per_million > 0.0);
        assert!(pricing.output_per_million > 0.0);
    }

    #[test]
    fn test_model_pricing_sonnet() {
        let pricing = ModelPricing::claude_sonnet_4();
        assert!(pricing.model.contains("sonnet"));
        assert!(pricing.input_per_million > 0.0);
    }

    #[test]
    fn test_model_pricing_haiku() {
        let pricing = ModelPricing::claude_haiku_3_5();
        assert!(pricing.model.contains("haiku"));
        assert!(pricing.input_per_million > 0.0);
    }

    #[test]
    fn test_cost_estimate_struct() {
        let cost = CostEstimate {
            input_cost: 1.0,
            output_cost: 2.0,
            cache_write_cost: 0.5,
            cache_read_cost: 0.1,
            total_cost: 3.6,
            currency: "USD".to_string(),
        };

        assert_eq!(cost.currency, "USD");
        assert_eq!(cost.total_cost, 3.6);
    }

    #[test]
    fn test_usage_merge_with_cache_creation() {
        let mut usage1 = Usage::default();
        let usage2 = Usage {
            cache_creation: Some(CacheCreationDetails {
                ephemeral_5m_input_tokens: Some(100),
                ephemeral_1h_input_tokens: Some(200),
                extra: IndexMap::new(),
            }),
            ..Default::default()
        };

        usage1.merge(&usage2);
        assert!(usage1.cache_creation.is_some());
        let cache = usage1.cache_creation.unwrap();
        assert_eq!(cache.ephemeral_5m_input_tokens, Some(100));
    }

    #[test]
    fn test_usage_merge_with_server_tools() {
        let mut usage1 = Usage::default();
        let usage2 = Usage {
            server_tool_use: Some(ServerToolUse {
                web_search_requests: Some(5),
                web_fetch_requests: Some(3),
                extra: IndexMap::new(),
            }),
            ..Default::default()
        };

        usage1.merge(&usage2);
        assert!(usage1.server_tool_use.is_some());
        assert_eq!(usage1.web_search_requests(), 5);
        assert_eq!(usage1.web_fetch_requests(), 3);
    }
}
