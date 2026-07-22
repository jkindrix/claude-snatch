//! Usage statistics for Claude Code JSONL logs.
//!
//! This module defines token usage structures including:
//! - Input/output tokens
//! - Cache creation/read tokens
//! - Ephemeral cache tokens (5m and 1h)
//! - Server tool usage (web_search, web_fetch)

use chrono::{DateTime, NaiveDate, Utc};
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

    /// Calculate total tokens processed, including cache reads and writes
    /// (`total_input_tokens` + output). This is the all-in figure.
    ///
    /// Note: this method is the all-in total; the `total_tokens` *field* on
    /// reporting structs (e.g. `AnalyticsSummary`) holds the real-work figure
    /// from [`Self::work_tokens`]. Keep the two straight when wiring outputs.
    #[must_use]
    pub fn total_tokens(&self) -> u64 {
        self.total_input_tokens() + self.output_tokens
    }

    /// Calculate real-work tokens: everything the model processed as new
    /// content — fresh input + cache-creation (first-time-processed) input +
    /// output. Excludes only `cache_read`, the re-served component.
    #[must_use]
    pub fn work_tokens(&self) -> u64 {
        self.input_tokens + self.cache_creation_input_tokens.unwrap_or(0) + self.output_tokens
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
            let cache = self
                .cache_creation
                .get_or_insert_with(CacheCreationDetails::default);
            if let Some(tokens) = other_cache.ephemeral_5m_input_tokens {
                *cache.ephemeral_5m_input_tokens.get_or_insert(0) += tokens;
            }
            if let Some(tokens) = other_cache.ephemeral_1h_input_tokens {
                *cache.ephemeral_1h_input_tokens.get_or_insert(0) += tokens;
            }
        }

        if let Some(other_tools) = &other.server_tool_use {
            let tools = self
                .server_tool_use
                .get_or_insert_with(ServerToolUse::default);
            if let Some(count) = other_tools.web_search_requests {
                *tools.web_search_requests.get_or_insert(0) += count;
            }
            if let Some(count) = other_tools.web_fetch_requests {
                *tools.web_fetch_requests.get_or_insert(0) += count;
            }
        }
    }

    /// Fold another `Usage` into this one by taking the field-wise maximum.
    ///
    /// Used to deduplicate the streaming-chunk JSONL nodes that share one
    /// `message.id`: every chunk repeats the (constant) input/cache totals and
    /// carries a running output total, so the maximum across chunks recovers
    /// the single billed value per field. Summing them (see [`Self::merge`])
    /// would multiplicatively over-count one API turn.
    pub fn merge_max(&mut self, other: &Self) {
        self.input_tokens = self.input_tokens.max(other.input_tokens);
        self.output_tokens = self.output_tokens.max(other.output_tokens);

        if let Some(other_cache_creation) = other.cache_creation_input_tokens {
            let slot = self.cache_creation_input_tokens.get_or_insert(0);
            *slot = (*slot).max(other_cache_creation);
        }

        if let Some(other_cache_read) = other.cache_read_input_tokens {
            let slot = self.cache_read_input_tokens.get_or_insert(0);
            *slot = (*slot).max(other_cache_read);
        }

        if let Some(other_cache) = &other.cache_creation {
            let cache = self
                .cache_creation
                .get_or_insert_with(CacheCreationDetails::default);
            if let Some(tokens) = other_cache.ephemeral_5m_input_tokens {
                let slot = cache.ephemeral_5m_input_tokens.get_or_insert(0);
                *slot = (*slot).max(tokens);
            }
            if let Some(tokens) = other_cache.ephemeral_1h_input_tokens {
                let slot = cache.ephemeral_1h_input_tokens.get_or_insert(0);
                *slot = (*slot).max(tokens);
            }
        }

        if let Some(other_tools) = &other.server_tool_use {
            let tools = self
                .server_tool_use
                .get_or_insert_with(ServerToolUse::default);
            if let Some(count) = other_tools.web_search_requests {
                let slot = tools.web_search_requests.get_or_insert(0);
                *slot = (*slot).max(count);
            }
            if let Some(count) = other_tools.web_fetch_requests {
                let slot = tools.web_fetch_requests.get_or_insert(0);
                *slot = (*slot).max(count);
            }
        }

        // These fields affect pricing qualifications. Preserve their first
        // observed values while deliberately avoiding wholesale cloning of
        // open-ended `extra` data such as large per-iteration payloads.
        if self.service_tier.is_none() {
            self.service_tier.clone_from(&other.service_tier);
        }
        for field in ["speed", "inference_geo"] {
            if !self.extra.contains_key(field) {
                if let Some(value) = other.extra.get(field) {
                    self.extra.insert(field.to_string(), value.clone());
                }
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
    /// Cost for cache writes with a five-minute TTL.
    pub cache_write_5m_cost: f64,
    /// Cost for cache writes with a one-hour TTL.
    pub cache_write_1h_cost: f64,
    /// Cost for cache writes whose TTL was not recorded. These are priced at
    /// the five-minute rate and surfaced as a qualification by the aggregate.
    pub cache_write_unclassified_cost: f64,
    /// Cache-write tokens whose TTL was not recorded.
    pub unclassified_cache_write_tokens: u64,
    /// Whether the TTL detail exceeded the aggregate cache-write count. When
    /// true, the aggregate is conservatively priced at the five-minute rate.
    pub cache_write_breakdown_mismatch: bool,
    /// Cost for cache reads (typically cheaper).
    pub cache_read_cost: f64,
    /// Additional usage-based charges for server-side tools.
    pub server_tool_cost: f64,
    /// Total cost.
    pub total_cost: f64,
    /// Currency (typically "USD").
    pub currency: String,
}

impl CostEstimate {
    fn merge(&mut self, other: &Self) {
        self.input_cost += other.input_cost;
        self.output_cost += other.output_cost;
        self.cache_write_cost += other.cache_write_cost;
        self.cache_write_5m_cost += other.cache_write_5m_cost;
        self.cache_write_1h_cost += other.cache_write_1h_cost;
        self.cache_write_unclassified_cost += other.cache_write_unclassified_cost;
        self.unclassified_cache_write_tokens = self
            .unclassified_cache_write_tokens
            .saturating_add(other.unclassified_cache_write_tokens);
        self.cache_write_breakdown_mismatch |= other.cache_write_breakdown_mismatch;
        self.cache_read_cost += other.cache_read_cost;
        self.server_tool_cost += other.server_tool_cost;
        self.total_cost += other.total_cost;
        if self.currency.is_empty() {
            self.currency.clone_from(&other.currency);
        }
    }
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
    /// Cost per million five-minute cache write tokens.
    ///
    /// Kept under its original public field name for API compatibility.
    pub cache_write_per_million: f64,
    /// Cost per million one-hour cache write tokens.
    pub cache_write_1h_per_million: f64,
    /// Cost per million cache read tokens.
    pub cache_read_per_million: f64,
    /// Stable identifier for the matched rate card.
    pub rate_card: &'static str,
    /// Human-readable effective period for the rate card.
    pub effective_period: &'static str,
    /// Primary source for the rates.
    pub source_url: &'static str,
    /// Date on which the source was last verified for this release.
    pub source_checked: &'static str,
}

/// One pricing bucket used to preserve effective-date and modifier context
/// while token totals continue to aggregate by model for compatibility.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CostBucketKey {
    /// Native model identifier.
    pub model: String,
    /// Matched rate-card identifier, or `None` when no rate is known.
    pub rate_card: Option<String>,
    /// Native modifiers which may change the public base rate and are not
    /// modeled by the estimator.
    pub unmodeled_modifiers: Vec<String>,
}

const ANTHROPIC_PRICING_URL: &str = "https://platform.claude.com/docs/en/about-claude/pricing";
const PRICING_SOURCE_CHECKED: &str = "2026-07-22";

/// Strip an optional trailing `-YYYYMMDD` snapshot suffix from a model ID so a
/// dated snapshot (e.g. `claude-opus-4-5-20251101`) maps to the same rate as
/// its base ID (`claude-opus-4-5`).
fn normalize_model_id(model: &str) -> &str {
    if let Some(idx) = model.rfind('-') {
        let suffix = &model[idx + 1..];
        if suffix.len() == 8 && suffix.bytes().all(|b| b.is_ascii_digit()) {
            return &model[..idx];
        }
    }
    model
}

impl ModelPricing {
    /// Create pricing for Claude Opus 4.5.
    #[must_use]
    pub fn claude_opus_4_5() -> Self {
        Self {
            model: "claude-opus-4-5-20251101".to_string(),
            input_per_million: 5.0,
            output_per_million: 25.0,
            cache_write_per_million: 6.25,
            cache_write_1h_per_million: 10.0,
            cache_read_per_million: 0.5,
            rate_card: "anthropic-api-opus-4.5",
            effective_period: "standard API list rate",
            source_url: ANTHROPIC_PRICING_URL,
            source_checked: PRICING_SOURCE_CHECKED,
        }
    }

    /// Create pricing for Claude Sonnet 4.
    #[must_use]
    pub fn claude_sonnet_4() -> Self {
        Self {
            model: "claude-sonnet-4-20250514".to_string(),
            input_per_million: 3.0,        // $3/M input
            output_per_million: 15.0,      // $15/M output
            cache_write_per_million: 3.75, // 1.25x input
            cache_write_1h_per_million: 6.0,
            cache_read_per_million: 0.3, // 0.1x input
            rate_card: "anthropic-api-sonnet-4",
            effective_period: "standard API list rate",
            source_url: ANTHROPIC_PRICING_URL,
            source_checked: PRICING_SOURCE_CHECKED,
        }
    }

    /// Create pricing for Claude Haiku 3.5.
    #[must_use]
    pub fn claude_haiku_3_5() -> Self {
        Self {
            model: "claude-3-5-haiku-20241022".to_string(),
            input_per_million: 0.8,
            output_per_million: 4.0,
            cache_write_per_million: 1.0,
            cache_write_1h_per_million: 1.6,
            cache_read_per_million: 0.08,
            rate_card: "anthropic-api-haiku-3.5",
            effective_period: "standard API list rate",
            source_url: ANTHROPIC_PRICING_URL,
            source_checked: PRICING_SOURCE_CHECKED,
        }
    }

    /// Create pricing for the current Claude Opus tier (Opus 4.6–4.8).
    ///
    /// Rates as published 2026-06: $5/M input, $25/M output.
    #[must_use]
    pub fn claude_opus_4_8() -> Self {
        Self {
            model: "claude-opus-4-8".to_string(),
            input_per_million: 5.0,        // $5/M input
            output_per_million: 25.0,      // $25/M output
            cache_write_per_million: 6.25, // 1.25x input
            cache_write_1h_per_million: 10.0,
            cache_read_per_million: 0.5, // 0.1x input
            rate_card: "anthropic-api-opus-4.6-4.8",
            effective_period: "standard API list rate",
            source_url: ANTHROPIC_PRICING_URL,
            source_checked: PRICING_SOURCE_CHECKED,
        }
    }

    /// Create pricing for Claude Fable 5 (and Claude Mythos 5 — same rates).
    #[must_use]
    pub fn claude_fable_5() -> Self {
        Self {
            model: "claude-fable-5".to_string(),
            input_per_million: 10.0,       // $10/M input
            output_per_million: 50.0,      // $50/M output
            cache_write_per_million: 12.5, // 1.25x input
            cache_write_1h_per_million: 20.0,
            cache_read_per_million: 1.0, // 0.1x input
            rate_card: "anthropic-api-fable-mythos-5",
            effective_period: "standard API list rate",
            source_url: ANTHROPIC_PRICING_URL,
            source_checked: PRICING_SOURCE_CHECKED,
        }
    }

    /// Create standard pricing for Claude Sonnet 5, effective 2026-09-01.
    #[must_use]
    pub fn claude_sonnet_5() -> Self {
        Self {
            model: "claude-sonnet-5".to_string(),
            input_per_million: 3.0,        // $3/M input
            output_per_million: 15.0,      // $15/M output
            cache_write_per_million: 3.75, // 1.25x input
            cache_write_1h_per_million: 6.0,
            cache_read_per_million: 0.3, // 0.1x input
            rate_card: "anthropic-api-sonnet-5-standard",
            effective_period: "starting 2026-09-01",
            source_url: ANTHROPIC_PRICING_URL,
            source_checked: PRICING_SOURCE_CHECKED,
        }
    }

    /// Create introductory pricing for Claude Sonnet 5, through 2026-08-31.
    #[must_use]
    pub fn claude_sonnet_5_intro() -> Self {
        Self {
            model: "claude-sonnet-5".to_string(),
            input_per_million: 2.0,
            output_per_million: 10.0,
            cache_write_per_million: 2.5,
            cache_write_1h_per_million: 4.0,
            cache_read_per_million: 0.2,
            rate_card: "anthropic-api-sonnet-5-intro",
            effective_period: "through 2026-08-31",
            source_url: ANTHROPIC_PRICING_URL,
            source_checked: PRICING_SOURCE_CHECKED,
        }
    }

    /// Create pricing for Claude Haiku 4.5.
    ///
    /// Rates as published 2026-06: $1/M input, $5/M output.
    #[must_use]
    pub fn claude_haiku_4_5() -> Self {
        Self {
            model: "claude-haiku-4-5".to_string(),
            input_per_million: 1.0,        // $1/M input
            output_per_million: 5.0,       // $5/M output
            cache_write_per_million: 1.25, // 1.25x input
            cache_write_1h_per_million: 2.0,
            cache_read_per_million: 0.1, // 0.1x input
            rate_card: "anthropic-api-haiku-4.5",
            effective_period: "standard API list rate",
            source_url: ANTHROPIC_PRICING_URL,
            source_checked: PRICING_SOURCE_CHECKED,
        }
    }

    /// Calculate cost for given usage.
    #[must_use]
    pub fn calculate_cost(&self, usage: &Usage) -> CostEstimate {
        let input_cost = (usage.input_tokens as f64 / 1_000_000.0) * self.input_per_million;
        let output_cost = (usage.output_tokens as f64 / 1_000_000.0) * self.output_per_million;
        let cache_write_5m_tokens = usage
            .cache_creation
            .as_ref()
            .and_then(|detail| detail.ephemeral_5m_input_tokens)
            .unwrap_or(0);
        let cache_write_1h_tokens = usage
            .cache_creation
            .as_ref()
            .and_then(|detail| detail.ephemeral_1h_input_tokens)
            .unwrap_or(0);
        let classified_cache_write = cache_write_5m_tokens.saturating_add(cache_write_1h_tokens);
        // Some older/future envelopes may omit the aggregate while retaining
        // the more specific TTL fields. In that shape the detail is sufficient
        // evidence; an explicitly contradictory aggregate is handled below.
        let aggregate_cache_write = usage
            .cache_creation_input_tokens
            .unwrap_or(classified_cache_write);
        let cache_write_breakdown_mismatch = classified_cache_write > aggregate_cache_write;

        // A missing TTL cannot be reconstructed after the fact. Price the
        // unexplained aggregate at the cheaper five-minute rate and carry an
        // explicit qualification rather than fabricating precision. If native
        // detail exceeds the aggregate, distrust the detail and conservatively
        // treat the entire aggregate as unclassified.
        let (priced_5m_tokens, priced_1h_tokens, unclassified_cache_write_tokens) =
            if cache_write_breakdown_mismatch {
                (0, 0, aggregate_cache_write)
            } else {
                (
                    cache_write_5m_tokens,
                    cache_write_1h_tokens,
                    aggregate_cache_write - classified_cache_write,
                )
            };
        let cache_write_5m_cost =
            (priced_5m_tokens as f64 / 1_000_000.0) * self.cache_write_per_million;
        let cache_write_1h_cost =
            (priced_1h_tokens as f64 / 1_000_000.0) * self.cache_write_1h_per_million;
        let cache_write_unclassified_cost =
            (unclassified_cache_write_tokens as f64 / 1_000_000.0) * self.cache_write_per_million;
        let cache_write_cost =
            cache_write_5m_cost + cache_write_1h_cost + cache_write_unclassified_cost;
        let cache_read_cost = (usage.cache_read_input_tokens.unwrap_or(0) as f64 / 1_000_000.0)
            * self.cache_read_per_million;
        // Web search is $10 per 1,000 searches; web fetch has no additional
        // charge beyond the tokens it adds to context.
        let server_tool_cost = f64::from(usage.web_search_requests()) * 0.01;

        let total_cost =
            input_cost + output_cost + cache_write_cost + cache_read_cost + server_tool_cost;

        CostEstimate {
            input_cost,
            output_cost,
            cache_write_cost,
            cache_write_5m_cost,
            cache_write_1h_cost,
            cache_write_unclassified_cost,
            unclassified_cache_write_tokens,
            cache_write_breakdown_mismatch,
            cache_read_cost,
            server_tool_cost,
            total_cost,
            currency: "USD".to_string(),
        }
    }

    /// Get pricing for a model by its exact identifier.
    ///
    /// Matches the exact model ID (snapshot date suffixes are normalized away)
    /// against a rate table, so each version is priced at its own tier rather
    /// than a single per-family guess. Returns `None` for any unrecognized
    /// model — callers must treat that as "rate unavailable", never as $0.
    #[must_use]
    pub fn for_model(model: &str) -> Option<Self> {
        Self::for_model_at(model, Utc::now())
    }

    /// Get pricing for a model at the time the usage was observed.
    ///
    /// This matters for temporary public rates such as Sonnet 5's introductory
    /// period. Other entries currently have one verified model-lifetime rate.
    #[must_use]
    pub fn for_model_at(model: &str, observed_at: DateTime<Utc>) -> Option<Self> {
        match normalize_model_id(model) {
            "claude-fable-5" | "claude-mythos-5" => Some(Self::claude_fable_5()),
            "claude-opus-4-8" | "claude-opus-4-7" | "claude-opus-4-6" => {
                Some(Self::claude_opus_4_8())
            }
            "claude-opus-4-5" => Some(Self::claude_opus_4_5()),
            "claude-sonnet-5" => {
                let intro_end =
                    NaiveDate::from_ymd_opt(2026, 8, 31).expect("hard-coded pricing date is valid");
                if observed_at.date_naive() <= intro_end {
                    Some(Self::claude_sonnet_5_intro())
                } else {
                    Some(Self::claude_sonnet_5())
                }
            }
            "claude-sonnet-4-6" | "claude-sonnet-4-5" | "claude-sonnet-4" => {
                Some(Self::claude_sonnet_4())
            }
            "claude-haiku-4-5" => Some(Self::claude_haiku_4_5()),
            _ => None,
        }
    }

    /// Resolve a stable rate-card identifier previously selected by
    /// [`Self::for_model_at`].
    #[must_use]
    pub fn for_rate_card(rate_card: &str) -> Option<Self> {
        match rate_card {
            "anthropic-api-fable-mythos-5" => Some(Self::claude_fable_5()),
            "anthropic-api-opus-4.6-4.8" => Some(Self::claude_opus_4_8()),
            "anthropic-api-opus-4.5" => Some(Self::claude_opus_4_5()),
            "anthropic-api-sonnet-5-intro" => Some(Self::claude_sonnet_5_intro()),
            "anthropic-api-sonnet-5-standard" => Some(Self::claude_sonnet_5()),
            "anthropic-api-sonnet-4" => Some(Self::claude_sonnet_4()),
            "anthropic-api-haiku-4.5" => Some(Self::claude_haiku_4_5()),
            "anthropic-api-haiku-3.5" => Some(Self::claude_haiku_3_5()),
            _ => None,
        }
    }

    /// Primary-source citation shared by human-readable cost reports.
    #[must_use]
    pub fn source_summary() -> &'static str {
        "Anthropic API list rates, checked 2026-07-22; https://platform.claude.com/docs/en/about-claude/pricing"
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
    /// Billing totals split by effective rate card and unmodeled modifier set.
    /// This preserves the context that `by_model` intentionally collapses.
    pub cost_buckets: IndexMap<CostBucketKey, Usage>,
    /// Calculated cost breakdown by model using `cost_buckets`.
    pub cost_by_model: IndexMap<String, CostEstimate>,
    /// Total tool invocations.
    pub tool_invocations: usize,
    /// Tool invocations by name.
    pub tools_by_name: IndexMap<String, usize>,
    /// Total errors encountered.
    pub error_count: usize,
    /// Estimated total cost. When `by_model` contains models with no known
    /// rate (see `unpriced_models`), this sum covers only the priced models.
    pub estimated_cost: Option<f64>,
    /// Models present in `by_model` that have no known rate, so their cost is
    /// excluded from `estimated_cost`. Non-empty means the estimate is partial.
    pub unpriced_models: Vec<String>,
    /// Stable rate-card identifiers used by the estimate.
    pub pricing_rate_cards: Vec<String>,
    /// Assumptions or unsupported modifiers which qualify the estimate.
    pub pricing_qualifications: Vec<String>,
}

impl AggregatedUsage {
    /// Add usage from a single message.
    pub fn add_usage(&mut self, model: &str, usage: &Usage) {
        self.add_usage_at(model, usage, Utc::now());
    }

    /// Add usage observed at a specific time, preserving the effective rate
    /// card selected for that API turn.
    pub fn add_usage_at(&mut self, model: &str, usage: &Usage, observed_at: DateTime<Utc>) {
        self.message_count += 1;
        self.usage.merge(usage);

        let model_usage = self.by_model.entry(model.to_string()).or_default();
        model_usage.merge(usage);

        let rate_card = ModelPricing::for_model_at(model, observed_at)
            .map(|pricing| pricing.rate_card.to_string());
        let mut unmodeled_modifiers = Vec::new();
        if let Some(service_tier) = usage.service_tier.as_deref() {
            if service_tier != "standard" {
                unmodeled_modifiers.push(format!("service_tier={service_tier}"));
            }
        }
        for field in ["speed", "inference_geo"] {
            let Some(value) = usage.extra.get(field).and_then(Value::as_str) else {
                continue;
            };
            let is_default = match field {
                "speed" => value.is_empty() || value == "standard" || value == "not_available",
                "inference_geo" => {
                    value.is_empty() || value == "global" || value == "not_available"
                }
                _ => false,
            };
            if !is_default {
                unmodeled_modifiers.push(format!("{field}={value}"));
            }
        }
        let standard_long_context = matches!(
            normalize_model_id(model),
            "claude-fable-5"
                | "claude-mythos-5"
                | "claude-opus-4-8"
                | "claude-opus-4-7"
                | "claude-opus-4-6"
                | "claude-sonnet-5"
                | "claude-sonnet-4-6"
        );
        if usage.total_input_tokens() > 200_000 && !standard_long_context {
            unmodeled_modifiers.push("context_window=>200k".to_string());
        }
        unmodeled_modifiers.sort();
        let bucket = CostBucketKey {
            model: model.to_string(),
            rate_card,
            unmodeled_modifiers,
        };
        self.cost_buckets.entry(bucket).or_default().merge(usage);
    }

    /// Record a tool invocation.
    pub fn record_tool(&mut self, tool_name: &str) {
        self.tool_invocations += 1;
        *self.tools_by_name.entry(tool_name.to_string()).or_insert(0) += 1;
    }

    /// Calculate estimated cost based on model usage.
    pub fn calculate_cost(&mut self) {
        let mut total = 0.0;
        let mut priced = false;
        let mut unpriced = std::collections::BTreeSet::new();
        let mut rate_cards = std::collections::BTreeSet::new();
        let mut qualifications = std::collections::BTreeSet::new();
        let mut cost_by_model: IndexMap<String, CostEstimate> = IndexMap::new();

        for (bucket, usage) in &self.cost_buckets {
            let Some(rate_card) = bucket.rate_card.as_deref() else {
                unpriced.insert(bucket.model.clone());
                continue;
            };
            let Some(pricing) = ModelPricing::for_rate_card(rate_card) else {
                unpriced.insert(bucket.model.clone());
                continue;
            };
            let cost = pricing.calculate_cost(usage);
            total += cost.total_cost;
            priced = true;
            rate_cards.insert(format!(
                "{} ({})",
                pricing.rate_card, pricing.effective_period
            ));
            if cost.unclassified_cache_write_tokens > 0 {
                qualifications.insert(format!(
                    "{}: {} cache-creation tokens lacked TTL detail and were assumed 5m",
                    bucket.model, cost.unclassified_cache_write_tokens
                ));
            }
            if cost.cache_write_breakdown_mismatch {
                qualifications.insert(format!(
                    "{}: cache-creation TTL detail exceeded the aggregate; the aggregate was assumed 5m",
                    bucket.model
                ));
            }
            if !bucket.unmodeled_modifiers.is_empty() {
                qualifications.insert(format!(
                    "{}: base list rates do not model {}",
                    bucket.model,
                    bucket.unmodeled_modifiers.join(", ")
                ));
            }
            cost_by_model
                .entry(bucket.model.clone())
                .or_default()
                .merge(&cost);
        }

        self.cost_by_model = cost_by_model;
        self.unpriced_models = unpriced.into_iter().collect();
        self.pricing_rate_cards = rate_cards.into_iter().collect();
        self.pricing_qualifications = qualifications.into_iter().collect();
        // If no model in the breakdown has a known rate, the cost is
        // unavailable rather than a misleading $0. When some are priced, the
        // sum is partial — `unpriced_models` flags what was excluded.
        self.estimated_cost = if priced { Some(total) } else { None };
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
    // Test assertions compare exactly-representable float values (0.0, integer-valued
    // costs/scores); the float_cmp lint is a false positive for these.
    #![allow(clippy::float_cmp)]
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
        // work_tokens = fresh input + cache_creation + output (excludes cache_read).
        assert_eq!(usage.work_tokens(), 100 + 200 + 50);
    }

    #[test]
    fn test_work_tokens_excludes_only_cache_read() {
        let usage = Usage {
            input_tokens: 14_852,
            output_tokens: 115_560,
            cache_creation_input_tokens: Some(2_331_483),
            cache_read_input_tokens: Some(35_214_896),
            ..Default::default()
        };

        // Real work = fresh + cache_creation + output; cache_read is re-served.
        assert_eq!(usage.work_tokens(), 14_852 + 2_331_483 + 115_560);
        // All-in total still available and includes cache_read.
        assert_eq!(
            usage.total_tokens(),
            14_852 + 2_331_483 + 35_214_896 + 115_560
        );
        // Headline (work) is far below the all-in figure, but well above fresh+output.
        assert!(usage.work_tokens() < usage.total_tokens());
        assert!(usage.work_tokens() > usage.input_tokens + usage.output_tokens);
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
    fn test_usage_merge_max_dedups_whole_struct() {
        // Models the streaming chunks of one message.id: input/cache constant,
        // output a running total, ephemeral + server_tool_use constant. Folding
        // by field-wise max must recover the single billed value per field.
        let chunk = |output: u64| Usage {
            input_tokens: 100,
            output_tokens: output,
            cache_creation_input_tokens: Some(2000),
            cache_read_input_tokens: Some(5000),
            cache_creation: Some(CacheCreationDetails {
                ephemeral_5m_input_tokens: Some(1500),
                ephemeral_1h_input_tokens: Some(500),
                extra: IndexMap::new(),
            }),
            server_tool_use: Some(ServerToolUse {
                web_search_requests: Some(2),
                web_fetch_requests: Some(1),
                extra: IndexMap::new(),
            }),
            service_tier: Some("priority".to_string()),
            extra: IndexMap::from([
                ("speed".to_string(), Value::String("fast".to_string())),
                ("inference_geo".to_string(), Value::String("us".to_string())),
            ]),
        };

        let mut folded = Usage::default();
        for output in [8, 40, 88] {
            folded.merge_max(&chunk(output));
        }

        // Constant fields: max == the single repeated value, not the sum.
        assert_eq!(folded.input_tokens, 100);
        assert_eq!(folded.cache_creation_input_tokens, Some(2000));
        assert_eq!(folded.cache_read_input_tokens, Some(5000));
        // Output: max == last (cumulative running total).
        assert_eq!(folded.output_tokens, 88);
        // Ephemeral cache_creation breakdown deduped (max per field).
        let cache = folded.cache_creation.unwrap();
        assert_eq!(cache.ephemeral_5m_input_tokens, Some(1500));
        assert_eq!(cache.ephemeral_1h_input_tokens, Some(500));
        // server_tool_use deduped (max per field), not summed.
        let tools = folded.server_tool_use.unwrap();
        assert_eq!(tools.web_search_requests, Some(2));
        assert_eq!(tools.web_fetch_requests, Some(1));
        assert_eq!(folded.service_tier.as_deref(), Some("priority"));
        assert_eq!(folded.extra["speed"], "fast");
        assert_eq!(folded.extra["inference_geo"], "us");
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
    fn test_for_model_prices_by_exact_id() {
        // Fable 5 (and Mythos 5) price at their own tier above Opus.
        let fable = ModelPricing::for_model("claude-fable-5").unwrap();
        assert_eq!(fable.input_per_million, 10.0);
        assert_eq!(fable.output_per_million, 50.0);
        assert_eq!(
            ModelPricing::for_model("claude-mythos-5")
                .unwrap()
                .input_per_million,
            10.0
        );

        // Sonnet 5 selects its temporary rate by observation date.
        let intro = DateTime::parse_from_rfc3339("2026-07-22T00:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let standard = DateTime::parse_from_rfc3339("2026-09-01T00:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let sonnet5_intro = ModelPricing::for_model_at("claude-sonnet-5", intro).unwrap();
        assert_eq!(sonnet5_intro.input_per_million, 2.0);
        assert_eq!(sonnet5_intro.output_per_million, 10.0);
        let sonnet5_standard = ModelPricing::for_model_at("claude-sonnet-5", standard).unwrap();
        assert_eq!(sonnet5_standard.input_per_million, 3.0);
        assert_eq!(sonnet5_standard.output_per_million, 15.0);

        // Current Opus tier prices at its own rate, not the older 4.5 tier.
        let opus48 = ModelPricing::for_model("claude-opus-4-8").unwrap();
        assert_eq!(opus48.input_per_million, 5.0);
        assert_eq!(opus48.output_per_million, 25.0);

        // Opus 4.5 uses the verified $5/$25 tier; dated snapshots normalize.
        let opus45 = ModelPricing::for_model("claude-opus-4-5-20251101").unwrap();
        assert_eq!(opus45.input_per_million, 5.0);
        assert_eq!(opus45.output_per_million, 25.0);

        // Sonnet and Haiku resolve to their own tiers (dated form included).
        assert_eq!(
            ModelPricing::for_model("claude-sonnet-4-6")
                .unwrap()
                .input_per_million,
            3.0
        );
        assert_eq!(
            ModelPricing::for_model("claude-haiku-4-5-20251001")
                .unwrap()
                .input_per_million,
            1.0
        );
        let haiku35 = ModelPricing::claude_haiku_3_5();
        assert_eq!(haiku35.input_per_million, 0.8);
        assert_eq!(haiku35.output_per_million, 4.0);

        // Unknown / fabricated IDs are unavailable, never a wrong-tier guess.
        assert!(ModelPricing::for_model("claude-made-up-9").is_none());
        assert!(ModelPricing::for_model("<synthetic>").is_none());
    }

    #[test]
    fn test_unpriced_usage_reports_unavailable_not_zero() {
        let mut agg = AggregatedUsage::default();
        agg.add_usage(
            "<synthetic>",
            &Usage {
                input_tokens: 1000,
                output_tokens: 500,
                ..Default::default()
            },
        );
        agg.calculate_cost();
        // A session whose only model has no known rate yields no estimate,
        // rather than a misleading $0.00.
        assert_eq!(agg.estimated_cost, None);
        assert_eq!(agg.unpriced_models, vec!["<synthetic>".to_string()]);
    }

    #[test]
    fn test_mixed_pricing_reports_partial_with_signal() {
        let mut agg = AggregatedUsage::default();
        agg.add_usage(
            "claude-opus-4-8",
            &Usage {
                input_tokens: 1_000_000,
                output_tokens: 0,
                ..Default::default()
            },
        );
        agg.add_usage(
            "retired-3-x",
            &Usage {
                input_tokens: 1000,
                output_tokens: 500,
                ..Default::default()
            },
        );
        agg.calculate_cost();
        // The priced model's cost is preserved ($5/M input * 1M = $5)...
        assert_eq!(agg.estimated_cost, Some(5.0));
        // ...and the unpriceable model is flagged rather than silently dropped.
        assert_eq!(agg.unpriced_models, vec!["retired-3-x".to_string()]);
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
            ..Default::default()
        };

        assert_eq!(cost.currency, "USD");
        assert_eq!(cost.total_cost, 3.6);
    }

    #[test]
    fn cache_write_cost_uses_each_recorded_ttl() {
        let usage = Usage {
            cache_creation_input_tokens: Some(3_000_000),
            cache_creation: Some(CacheCreationDetails {
                ephemeral_5m_input_tokens: Some(1_000_000),
                ephemeral_1h_input_tokens: Some(2_000_000),
                ..Default::default()
            }),
            ..Default::default()
        };
        let cost = ModelPricing::claude_sonnet_4().calculate_cost(&usage);

        assert_eq!(cost.cache_write_5m_cost, 3.75);
        assert_eq!(cost.cache_write_1h_cost, 12.0);
        assert_eq!(cost.cache_write_unclassified_cost, 0.0);
        assert_eq!(cost.cache_write_cost, 15.75);
        assert_eq!(cost.unclassified_cache_write_tokens, 0);
        assert!(!cost.cache_write_breakdown_mismatch);
    }

    #[test]
    fn missing_cache_ttl_is_priced_conservatively_and_qualified() {
        let observed_at = DateTime::parse_from_rfc3339("2026-07-22T00:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let mut aggregate = AggregatedUsage::default();
        aggregate.add_usage_at(
            "claude-fable-5",
            &Usage {
                cache_creation_input_tokens: Some(20_756),
                ..Default::default()
            },
            observed_at,
        );
        aggregate.calculate_cost();

        let cost = &aggregate.cost_by_model["claude-fable-5"];
        assert_eq!(cost.unclassified_cache_write_tokens, 20_756);
        assert!((cost.cache_write_unclassified_cost - 0.25945).abs() < 0.000_001);
        assert_eq!(aggregate.pricing_qualifications.len(), 1);
        assert!(aggregate.pricing_qualifications[0].contains("assumed 5m"));
    }

    #[test]
    fn contradictory_cache_ttl_detail_cannot_inflate_the_aggregate() {
        let usage = Usage {
            cache_creation_input_tokens: Some(1_000_000),
            cache_creation: Some(CacheCreationDetails {
                ephemeral_5m_input_tokens: Some(1_000_000),
                ephemeral_1h_input_tokens: Some(1_000_000),
                ..Default::default()
            }),
            ..Default::default()
        };
        let cost = ModelPricing::claude_sonnet_4().calculate_cost(&usage);

        assert!(cost.cache_write_breakdown_mismatch);
        assert_eq!(cost.unclassified_cache_write_tokens, 1_000_000);
        assert_eq!(cost.cache_write_cost, 3.75);
    }

    #[test]
    fn sonnet_five_costs_preserve_both_effective_rate_periods() {
        let intro = DateTime::parse_from_rfc3339("2026-08-31T23:59:59Z")
            .unwrap()
            .with_timezone(&Utc);
        let standard = DateTime::parse_from_rfc3339("2026-09-01T00:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let usage = Usage {
            input_tokens: 1_000_000,
            ..Default::default()
        };
        let mut aggregate = AggregatedUsage::default();
        aggregate.add_usage_at("claude-sonnet-5", &usage, intro);
        aggregate.add_usage_at("claude-sonnet-5", &usage, standard);
        aggregate.calculate_cost();

        assert_eq!(aggregate.estimated_cost, Some(5.0));
        assert_eq!(aggregate.cost_buckets.len(), 2);
        assert_eq!(aggregate.pricing_rate_cards.len(), 2);
    }

    #[test]
    fn unmodeled_pricing_modifiers_are_never_silent() {
        let observed_at = DateTime::parse_from_rfc3339("2026-07-22T00:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let mut extra = IndexMap::new();
        extra.insert("speed".to_string(), Value::String("fast".to_string()));
        extra.insert("inference_geo".to_string(), Value::String("us".to_string()));
        let mut aggregate = AggregatedUsage::default();
        aggregate.add_usage_at(
            "claude-fable-5",
            &Usage {
                input_tokens: 100,
                service_tier: Some("priority".to_string()),
                extra,
                ..Default::default()
            },
            observed_at,
        );
        aggregate.calculate_cost();

        assert_eq!(aggregate.pricing_qualifications.len(), 1);
        let note = &aggregate.pricing_qualifications[0];
        assert!(note.contains("inference_geo=us"));
        assert!(note.contains("service_tier=priority"));
        assert!(note.contains("speed=fast"));
    }

    #[test]
    fn web_search_charge_is_included_but_web_fetch_is_token_only() {
        let usage = Usage {
            server_tool_use: Some(ServerToolUse {
                web_search_requests: Some(3),
                web_fetch_requests: Some(7),
                ..Default::default()
            }),
            ..Default::default()
        };
        let cost = ModelPricing::claude_fable_5().calculate_cost(&usage);

        assert_eq!(cost.server_tool_cost, 0.03);
        assert_eq!(cost.total_cost, 0.03);
    }

    #[test]
    fn unsupported_long_context_premium_is_qualified_by_turn_shape() {
        let observed_at = DateTime::parse_from_rfc3339("2026-03-01T00:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let mut aggregate = AggregatedUsage::default();
        aggregate.add_usage_at(
            "claude-opus-4-5",
            &Usage {
                input_tokens: 250_000,
                ..Default::default()
            },
            observed_at,
        );
        aggregate.calculate_cost();

        assert!(aggregate.pricing_qualifications[0].contains("context_window=>200k"));
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
