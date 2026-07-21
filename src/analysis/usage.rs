//! Provider-aware usage presentation.
//!
//! Canonical normalized usage and native observations are deliberately
//! separate: `Call/Delta` contributes to canonical totals, while
//! `Session/Cumulative` is a reconciliation observation and must never be
//! summed as another model call.

use std::collections::BTreeMap;

use serde::Serialize;

use crate::analytics::SessionAnalytics;
use crate::provider::{
    ProviderPricing, UsageAggregation, UsageBasis, UsageObservationKind, UsageScope,
};
use crate::reconstruction::Conversation;

/// Canonical normalized usage totals consumed by existing analytics.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct CanonicalUsageSummary {
    /// Fresh (non-cached) input tokens.
    pub input_tokens: u64,
    /// Re-served cached input tokens.
    pub cache_read_tokens: u64,
    /// Cache-creation input tokens.
    pub cache_creation_tokens: u64,
    /// Generated output tokens.
    pub output_tokens: u64,
    /// All tokens processed, including cache reads and writes.
    pub total_processed_tokens: u64,
}

/// Pricing result under the provider's declared policy.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct UsagePricingSummary {
    /// `known-model-rates` or `unpriced`.
    pub policy: &'static str,
    /// Estimated USD cost. `None` for an unpriced provider or when every
    /// observed model lacks a rate.
    pub estimated_cost: Option<f64>,
    /// Models excluded from the estimate.
    pub unpriced_models: Vec<String>,
}

/// Provider-aware usage summary for public info surfaces.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct ProviderUsageSummary {
    /// Normalized totals. These are the only values consumers may sum.
    pub canonical: CanonicalUsageSummary,
    /// Native observation cardinality keyed by `scope/aggregation`.
    pub observation_counts: BTreeMap<&'static str, usize>,
    /// Native observation cardinality by measurement kind. Context-window
    /// occupancy is deliberately separate from model-token usage.
    pub observation_kind_counts: BTreeMap<&'static str, usize>,
    /// Native observation cardinality by input/cache basis.
    pub basis_counts: BTreeMap<&'static str, usize>,
    /// Observations whose fresh-input contribution was ambiguous.
    pub ambiguous_observations: usize,
    /// Provider policy and estimate coverage.
    pub pricing: UsagePricingSummary,
}

/// Summarize canonical usage and the native observations that justify it.
#[must_use]
pub fn provider_usage_summary(
    conversation: &Conversation,
    pricing: ProviderPricing,
) -> ProviderUsageSummary {
    let mut analytics = SessionAnalytics::from_conversation(conversation);
    let mut observation_counts = BTreeMap::new();
    let mut observation_kind_counts = BTreeMap::new();
    let mut basis_counts = BTreeMap::new();
    let mut ambiguous_observations = 0;

    if let Some(bundle) = conversation.provider_bundle() {
        for semantics in bundle.semantics.values() {
            for observation in &semantics.usage {
                let kind = match observation.kind {
                    UsageObservationKind::ModelTokens => "model-tokens",
                    UsageObservationKind::ContextWindow => "context-window",
                };
                *observation_kind_counts.entry(kind).or_default() += 1;
                let axis = match (observation.scope, observation.aggregation) {
                    (UsageScope::Call, UsageAggregation::Delta) => "call/delta",
                    (UsageScope::Call, UsageAggregation::Cumulative) => "call/cumulative",
                    (UsageScope::Turn, UsageAggregation::Delta) => "turn/delta",
                    (UsageScope::Turn, UsageAggregation::Cumulative) => "turn/cumulative",
                    (UsageScope::Session, UsageAggregation::Delta) => "session/delta",
                    (UsageScope::Session, UsageAggregation::Cumulative) => "session/cumulative",
                };
                *observation_counts.entry(axis).or_default() += 1;
                let basis = match observation.basis {
                    UsageBasis::InputIncludesCached => "input-includes-cached",
                    UsageBasis::InputExcludesCached => "input-excludes-cached",
                    UsageBasis::Unknown => "unknown",
                };
                *basis_counts.entry(basis).or_default() += 1;
                ambiguous_observations += usize::from(observation.ambiguous);
            }
        }
    }

    let policy = match pricing {
        ProviderPricing::KnownModelRates => "known-model-rates",
        ProviderPricing::Unpriced => {
            analytics.usage.estimated_cost = None;
            analytics.usage.unpriced_models = analytics.usage.by_model.keys().cloned().collect();
            "unpriced"
        }
    };
    analytics.usage.unpriced_models.sort();
    analytics.usage.unpriced_models.dedup();
    let usage = &analytics.usage.usage;

    ProviderUsageSummary {
        canonical: CanonicalUsageSummary {
            input_tokens: usage.input_tokens,
            cache_read_tokens: usage.cache_read_input_tokens.unwrap_or(0),
            cache_creation_tokens: usage.cache_creation_input_tokens.unwrap_or(0),
            output_tokens: usage.output_tokens,
            total_processed_tokens: usage.total_tokens(),
        },
        observation_counts,
        observation_kind_counts,
        basis_counts,
        ambiguous_observations,
        pricing: UsagePricingSummary {
            policy,
            estimated_cost: analytics.usage.estimated_cost,
            unpriced_models: analytics.usage.unpriced_models,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::fake::{multi_artifact_key, FakeProvider};
    use crate::provider::SourceProvider;

    #[test]
    fn native_observation_axes_are_counted_without_becoming_canonical_usage() {
        let parsed = FakeProvider.parse(&multi_artifact_key()).unwrap();
        let conversation = Conversation::from_parsed_session(std::sync::Arc::new(parsed)).unwrap();
        let report = provider_usage_summary(&conversation, ProviderPricing::Unpriced);
        assert_eq!(report.observation_counts.get("call/delta"), Some(&1));
        assert_eq!(
            report.observation_counts.get("session/cumulative"),
            Some(&1)
        );
        assert_eq!(report.observation_kind_counts.get("model-tokens"), Some(&2));
        assert_eq!(report.ambiguous_observations, 0);
        // The fake intentionally carries native observations but no normalized
        // message usage. Observation values are never summed by this consumer.
        assert_eq!(report.canonical.total_processed_tokens, 0);
    }

    fn priced_model_conversation() -> Conversation {
        let entry: crate::model::LogEntry = serde_json::from_value(serde_json::json!({
            "type": "assistant",
            "uuid": "a1",
            "parentUuid": null,
            "timestamp": "2026-01-01T00:00:00Z",
            "sessionId": "s1",
            "version": "1.0.0",
            "message": {
                "id": "m1",
                "type": "message",
                "role": "assistant",
                "model": "claude-sonnet-5",
                "content": [{"type": "text", "text": "done"}],
                "usage": {"input_tokens": 100, "output_tokens": 50}
            }
        }))
        .unwrap();
        Conversation::from_entries(vec![entry]).unwrap()
    }

    #[test]
    fn unpriced_policy_overrides_even_a_known_model_name() {
        let report =
            provider_usage_summary(&priced_model_conversation(), ProviderPricing::Unpriced);
        assert_eq!(report.pricing.policy, "unpriced");
        assert_eq!(report.pricing.estimated_cost, None);
        assert_eq!(report.pricing.unpriced_models, ["claude-sonnet-5"]);
    }

    #[test]
    fn known_rate_policy_preserves_existing_claude_pricing() {
        let report = provider_usage_summary(
            &priced_model_conversation(),
            ProviderPricing::KnownModelRates,
        );
        assert_eq!(report.pricing.policy, "known-model-rates");
        assert!(report.pricing.estimated_cost.is_some());
        assert!(report.pricing.unpriced_models.is_empty());
    }
}
