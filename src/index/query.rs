//! Exact provider-index query execution.
//!
//! Tantivy narrows only exact typed fields. Regex/fuzzy matching, scope,
//! context, cardinality, and relevance are computed over the stored ordered
//! projections by the same matcher used by direct CLI search.

use std::cmp::Ordering;
use std::collections::{BTreeSet, BinaryHeap, HashSet};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::analysis::search::{
    count_projection_matches, projection_matches, search_projection, ExactSearchMatcher,
    ProjectedSearchMatch, SearchScope, SearchSegmentKind,
};
use crate::error::{Result, SnatchError};
use crate::provider::LogicalSessionKey;

use super::provider::{
    IndexedEntryCandidateFilter, IndexedSearchEntry, IndexedSkip, ProviderSearchIndex,
    PROVIDER_INDEX_SCHEMA_VERSION,
};

/// Maximum number of results returned in one page.
pub const MAX_INDEXED_SEARCH_PAGE_SIZE: usize = 10_000;
/// Maximum ordered result window retained while producing a deterministic page.
pub const MAX_INDEXED_SEARCH_WINDOW: usize = 100_000;

/// Provider partitions selected from the committed index snapshot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IndexedProviderSelection {
    /// Search only these providers. A missing indexed provider is an error.
    Explicit(Vec<String>),
    /// Search every represented provider partition and report incomplete
    /// coverage rather than consulting live source discovery.
    All,
}

/// Typed entry filters. Exact identity/type/time/activity fields narrow the
/// Tantivy candidate set; substring and semantic filters are verified against
/// each stored payload.
#[derive(Debug, Clone, Default)]
pub struct IndexedSearchFilters {
    /// Complete qualified source-session keys.
    pub session_keys: Vec<String>,
    /// Complete qualified continuation-root keys.
    pub logical_roots: Vec<String>,
    /// Exact unified-project identities.
    pub project_keys: Vec<String>,
    /// Case-insensitive substring of the project display path.
    pub project_contains: Option<String>,
    /// Exact normalized entry discriminator(s).
    pub message_types: Vec<String>,
    /// Case-insensitive assistant-model substring.
    pub model_contains: Option<String>,
    /// Case-insensitive native tool-name substring.
    pub tool_name_contains: Option<String>,
    /// Exact canonical tool kinds; any matching call accepts the entry.
    pub tool_kinds: Vec<String>,
    /// Case-insensitive git-branch substring.
    pub git_branch_contains: Option<String>,
    /// Exact prompt-authorship labels.
    pub prompt_authorship: Vec<String>,
    /// Exact prompt-delivery labels.
    pub prompt_delivery: Vec<String>,
    /// Minimum canonical processed tokens on the entry.
    pub min_processed_tokens: Option<u64>,
    /// Maximum canonical processed tokens on the entry.
    pub max_processed_tokens: Option<u64>,
    /// Inclusive lower timestamp bound.
    pub timestamp_from: Option<DateTime<Utc>>,
    /// Inclusive upper timestamp bound.
    pub timestamp_until: Option<DateTime<Utc>>,
    /// Require a tool result with this explicit native error state.
    pub tool_error: Option<bool>,
    /// Include fork-inherited entries in cross-session search. A selected
    /// source session is content-complete regardless of this value.
    pub include_inherited: bool,
    /// Include typed spawned sessions in cross-session search. A selected
    /// source session is content-complete regardless of this value.
    pub include_spawned: bool,
}

/// Stable result order independent of Tantivy segment layout.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord)]
pub enum IndexedSearchOrder {
    /// Qualified session, normalized entry order, segment, then line.
    #[default]
    Source,
    /// Score descending with source order as a deterministic tie-breaker.
    Relevance,
}

/// One exact indexed-search request.
#[derive(Debug, Clone)]
pub struct IndexedSearchRequest {
    /// Provider partitions to search.
    pub selection: IndexedProviderSelection,
    /// Exact positive matcher.
    pub matcher: ExactSearchMatcher,
    /// Optional same-scope entry exclusion matcher.
    pub exclude: Option<ExactSearchMatcher>,
    /// Projected content scope.
    pub scope: SearchScope,
    /// Typed candidate and semantic filters.
    pub filters: IndexedSearchFilters,
    /// Lines of context retained inside each segment.
    pub context_lines: usize,
    /// Deterministic result ordering.
    pub order: IndexedSearchOrder,
    /// Zero-based result offset after ordering.
    pub offset: usize,
    /// Maximum returned results.
    pub limit: usize,
}

/// One independently identified matching line.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IndexedSearchMatch {
    /// Source provider id.
    pub provider: String,
    /// Complete qualified source-session key.
    pub session_key: String,
    /// Complete qualified continuation root.
    pub logical_root: String,
    /// Unified-project identity.
    pub project_key: String,
    /// Project display path captured at build time.
    pub project_path: String,
    /// Deterministic normalized entry id.
    pub entry_id: String,
    /// Normalized entry order within the source session.
    pub entry_order: usize,
    /// Entry timestamp, when available.
    pub timestamp: Option<DateTime<Utc>>,
    /// Normalized entry discriminator.
    pub message_type: String,
    /// Assistant model, when available.
    pub model: Option<String>,
    /// `new` or `inherited-history`.
    pub activity: String,
    /// Whether typed lineage identifies this as a spawned session.
    pub spawned: bool,
    /// Native-order projected-segment position.
    pub segment_index: usize,
    /// Zero-based line within that segment.
    pub line_number: usize,
    /// Stable human-readable segment location.
    pub location: String,
    /// Complete matching line.
    pub line: String,
    /// Preceding same-segment context.
    pub context_before: String,
    /// First regex match or fuzzy word span.
    pub matched_text: String,
    /// Following same-segment context.
    pub context_after: String,
    /// Matcher relevance score (0-100).
    pub score: u8,
}

/// Machine-readable snapshot and completeness statement attached to every
/// indexed query.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IndexedQueryCoverage {
    /// Provider-index schema version.
    pub schema_version: u64,
    /// Latest committed build generation.
    pub generation: String,
    /// Latest committed build time.
    pub built_at: DateTime<Utc>,
    /// Provider ids requested by the caller or derived for `all`.
    pub requested_providers: Vec<String>,
    /// Provider ids with retained partitions or a current complete empty inventory.
    pub represented_providers: Vec<String>,
    /// Provider ids actually included in candidate filtering.
    pub searched_providers: Vec<String>,
    /// Requested providers established complete by the latest build.
    pub complete_providers: Vec<String>,
    /// Whether the latest build proved disappearance coverage for its selection.
    pub removal_coverage_complete: bool,
    /// Whether any requested coverage remains unverified, skipped, or upsert-only.
    pub incomplete: bool,
    /// Requested-provider failures from the latest generation.
    pub skipped: Vec<IndexedSkip>,
    /// Requested-provider nonfatal coverage warnings.
    pub warnings: Vec<IndexedSkip>,
    /// Human-readable, non-sensitive coverage qualifications.
    pub notes: Vec<String>,
}

/// Exact results and distinct cardinalities. Normal result cardinality is one
/// per matching line; `total_occurrences` preserves grep-count semantics.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IndexedSearchResponse {
    /// Matching-line cardinality before pagination.
    pub total_matches: usize,
    /// Exact occurrence cardinality before pagination.
    pub total_occurrences: usize,
    /// Distinct qualified source sessions with matches.
    pub sessions_matched: usize,
    /// Results returned on this page.
    pub returned: usize,
    /// Applied result offset.
    pub offset: usize,
    /// Applied page limit.
    pub limit: usize,
    /// Deterministically ordered result page.
    pub matches: Vec<IndexedSearchMatch>,
    /// Snapshot generation and coverage statement.
    pub coverage: IndexedQueryCoverage,
}

fn normalize_values(values: &[String], label: &str) -> Result<Vec<String>> {
    let normalized: BTreeSet<_> = values.iter().cloned().collect();
    if normalized.len() != values.len() || normalized.contains("") {
        return Err(SnatchError::InvalidArgument {
            name: label.to_string(),
            reason: "values must be non-empty and unique".to_string(),
        });
    }
    Ok(normalized.into_iter().collect())
}

fn provider_of(session_key: &str) -> Result<String> {
    let key: LogicalSessionKey = session_key.parse().map_err(|error: String| {
        SnatchError::IndexError(format!(
            "indexed session key '{session_key}' is invalid: {error}"
        ))
    })?;
    if key.to_string() != session_key {
        return Err(SnatchError::IndexError(format!(
            "indexed session key '{session_key}' is not canonical"
        )));
    }
    Ok(key.provider.to_string())
}

fn contains_folded(value: &str, needle: Option<&str>) -> bool {
    needle.map_or(true, |needle| {
        value.to_lowercase().contains(&needle.to_lowercase())
    })
}

fn any_value(values: &[String], actual: Option<&str>) -> bool {
    values.is_empty() || actual.is_some_and(|actual| values.iter().any(|value| value == actual))
}

fn entry_matches_filters(entry: &IndexedSearchEntry, filters: &IndexedSearchFilters) -> bool {
    if !contains_folded(&entry.project_path, filters.project_contains.as_deref())
        || !contains_folded(
            entry.model.as_deref().unwrap_or(""),
            filters.model_contains.as_deref(),
        )
        || !contains_folded(
            entry.git_branch.as_deref().unwrap_or(""),
            filters.git_branch_contains.as_deref(),
        )
        || !any_value(
            &filters.prompt_authorship,
            entry.prompt_authorship.as_deref(),
        )
        || !any_value(&filters.prompt_delivery, entry.prompt_delivery.as_deref())
    {
        return false;
    }
    if filters.min_processed_tokens.is_some_and(|minimum| {
        entry
            .processed_tokens
            .map_or(true, |tokens| tokens < minimum)
    }) || filters.max_processed_tokens.is_some_and(|maximum| {
        entry
            .processed_tokens
            .map_or(true, |tokens| tokens > maximum)
    }) {
        return false;
    }
    if !filters.tool_kinds.is_empty()
        && !entry
            .tool_kinds
            .values()
            .any(|kind| filters.tool_kinds.iter().any(|wanted| wanted == kind))
    {
        return false;
    }
    if let Some(needle) = &filters.tool_name_contains {
        let needle = needle.to_lowercase();
        if !entry.projection.segments.iter().any(|segment| {
            segment
                .tool_name
                .as_ref()
                .is_some_and(|name| name.to_lowercase().contains(&needle))
        }) {
            return false;
        }
    }
    if let Some(is_error) = filters.tool_error {
        if !entry.projection.segments.iter().any(|segment| {
            segment.kind == SearchSegmentKind::ToolResult && segment.tool_is_error == Some(is_error)
        }) {
            return false;
        }
    }
    true
}

fn source_cmp(left: &IndexedSearchMatch, right: &IndexedSearchMatch) -> Ordering {
    left.session_key
        .cmp(&right.session_key)
        .then_with(|| left.entry_order.cmp(&right.entry_order))
        .then_with(|| left.entry_id.cmp(&right.entry_id))
        .then_with(|| left.segment_index.cmp(&right.segment_index))
        .then_with(|| left.line_number.cmp(&right.line_number))
}

fn result_cmp(
    left: &IndexedSearchMatch,
    right: &IndexedSearchMatch,
    order: IndexedSearchOrder,
) -> Ordering {
    match order {
        IndexedSearchOrder::Source => source_cmp(left, right),
        IndexedSearchOrder::Relevance => right
            .score
            .cmp(&left.score)
            .then_with(|| source_cmp(left, right)),
    }
}

#[derive(Debug)]
struct RetainedSearchMatch {
    order: IndexedSearchOrder,
    value: IndexedSearchMatch,
}

impl PartialEq for RetainedSearchMatch {
    fn eq(&self, other: &Self) -> bool {
        self.order == other.order
            && result_cmp(&self.value, &other.value, self.order) == Ordering::Equal
    }
}

impl Eq for RetainedSearchMatch {}

impl PartialOrd for RetainedSearchMatch {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for RetainedSearchMatch {
    fn cmp(&self, other: &Self) -> Ordering {
        self.order
            .cmp(&other.order)
            .then_with(|| result_cmp(&self.value, &other.value, self.order))
    }
}

fn retain_match(
    retained: &mut BinaryHeap<RetainedSearchMatch>,
    value: IndexedSearchMatch,
    order: IndexedSearchOrder,
    window: usize,
) {
    if window == 0 {
        return;
    }
    let candidate = RetainedSearchMatch { order, value };
    if retained.len() < window {
        retained.push(candidate);
    } else if retained
        .peek()
        .is_some_and(|worst| candidate.cmp(worst) == Ordering::Less)
    {
        retained.pop();
        retained.push(candidate);
    }
}

fn make_result(
    entry: &IndexedSearchEntry,
    provider: &str,
    matched: ProjectedSearchMatch,
) -> IndexedSearchMatch {
    IndexedSearchMatch {
        provider: provider.to_string(),
        session_key: entry.session_key.clone(),
        logical_root: entry.logical_root.clone(),
        project_key: entry.project_key.clone(),
        project_path: entry.project_path.clone(),
        entry_id: entry.entry_id.clone(),
        entry_order: entry.entry_order,
        timestamp: entry.timestamp,
        message_type: entry.message_type.clone(),
        model: entry.model.clone(),
        activity: entry.activity.clone(),
        spawned: entry.spawned,
        segment_index: matched.segment_index,
        line_number: matched.line_number,
        location: matched.location,
        line: matched.line,
        context_before: matched.context_before,
        matched_text: matched.matched_text,
        context_after: matched.context_after,
        score: matched.score,
    }
}

impl ProviderSearchIndex {
    /// Execute an exact query against only the committed index snapshot.
    /// This method has no provider registry and cannot perform source
    /// discovery or parsing as a side effect.
    pub fn query(&self, request: &IndexedSearchRequest) -> Result<IndexedSearchResponse> {
        for matcher in std::iter::once(&request.matcher).chain(request.exclude.iter()) {
            if let ExactSearchMatcher::Fuzzy { threshold, .. } = matcher {
                if *threshold > 100 {
                    return Err(SnatchError::InvalidArgument {
                        name: "fuzzy_threshold".to_string(),
                        reason: "must be between 0 and 100".to_string(),
                    });
                }
            }
        }
        if request
            .filters
            .min_processed_tokens
            .zip(request.filters.max_processed_tokens)
            .is_some_and(|(minimum, maximum)| minimum > maximum)
        {
            return Err(SnatchError::InvalidArgument {
                name: "tokens".to_string(),
                reason: "minimum cannot exceed maximum".to_string(),
            });
        }
        if request
            .filters
            .timestamp_from
            .zip(request.filters.timestamp_until)
            .is_some_and(|(from, until)| from > until)
        {
            return Err(SnatchError::InvalidArgument {
                name: "time".to_string(),
                reason: "start cannot be after end".to_string(),
            });
        }
        if request.limit > MAX_INDEXED_SEARCH_PAGE_SIZE {
            return Err(SnatchError::InvalidArgument {
                name: "limit".to_string(),
                reason: format!("must not exceed {MAX_INDEXED_SEARCH_PAGE_SIZE}"),
            });
        }
        let window = request.offset.checked_add(request.limit).ok_or_else(|| {
            SnatchError::InvalidArgument {
                name: "offset".to_string(),
                reason: "offset + limit overflows".to_string(),
            }
        })?;
        if window > MAX_INDEXED_SEARCH_WINDOW {
            return Err(SnatchError::InvalidArgument {
                name: "offset".to_string(),
                reason: format!("offset + limit must not exceed {MAX_INDEXED_SEARCH_WINDOW}"),
            });
        }

        let build = self.build_manifest()?.ok_or_else(|| {
            SnatchError::IndexError(
                "provider search index has no committed build; run an index build first"
                    .to_string(),
            )
        })?;
        let manifests = self.session_manifests()?;
        let partition_providers: BTreeSet<_> = manifests
            .iter()
            .map(|manifest| manifest.provider.clone())
            .collect();
        let represented: BTreeSet<_> = partition_providers
            .iter()
            .cloned()
            .chain(build.complete_providers.iter().cloned())
            .collect();
        let requested = match &request.selection {
            IndexedProviderSelection::Explicit(values) => normalize_values(values, "provider")?,
            IndexedProviderSelection::All => represented.iter().cloned().collect(),
        };
        if requested.is_empty() {
            return Err(SnatchError::IndexError(
                "provider search index represents no provider partitions".to_string(),
            ));
        }
        if matches!(request.selection, IndexedProviderSelection::Explicit(_)) {
            let missing: Vec<_> = requested
                .iter()
                .filter(|provider| !represented.contains(*provider))
                .cloned()
                .collect();
            if !missing.is_empty() {
                return Err(SnatchError::IndexError(format!(
                    "requested provider(s) are absent from the committed index: {}",
                    missing.join(", ")
                )));
            }
        }

        let session_keys = normalize_values(&request.filters.session_keys, "session")?;
        for session_key in &session_keys {
            let provider = provider_of(session_key)?;
            if !requested.iter().any(|selected| selected == &provider) {
                return Err(SnatchError::InvalidArgument {
                    name: "session".to_string(),
                    reason: format!(
                        "qualified session {session_key} belongs to unselected provider {provider}"
                    ),
                });
            }
            if !manifests
                .iter()
                .any(|manifest| manifest.session_key == *session_key)
            {
                return Err(SnatchError::IndexError(format!(
                    "requested session is absent from the committed index: {session_key}"
                )));
            }
        }
        let selected_session = !session_keys.is_empty();
        let logical_roots = normalize_values(&request.filters.logical_roots, "logical_root")?;
        for logical_root in &logical_roots {
            let provider = provider_of(logical_root)?;
            if !requested.iter().any(|selected| selected == &provider) {
                return Err(SnatchError::InvalidArgument {
                    name: "logical_root".to_string(),
                    reason: format!(
                        "qualified root {logical_root} belongs to unselected provider {provider}"
                    ),
                });
            }
        }
        let project_keys = normalize_values(&request.filters.project_keys, "project_key")?;
        let message_types = normalize_values(&request.filters.message_types, "message_type")?;
        let activities = if selected_session || request.filters.include_inherited {
            Vec::new()
        } else {
            vec!["new".to_string()]
        };
        let spawned = if selected_session || request.filters.include_spawned {
            None
        } else {
            Some(false)
        };
        let candidate_filter = IndexedEntryCandidateFilter {
            providers: requested.clone(),
            session_keys,
            logical_roots,
            project_keys,
            message_types,
            activities,
            spawned,
            timestamp_from_millis: request
                .filters
                .timestamp_from
                .map(|value| value.timestamp_millis()),
            timestamp_until_millis: request
                .filters
                .timestamp_until
                .map(|value| value.timestamp_millis()),
        };

        let mut total_matches = 0_usize;
        let mut total_occurrences = 0_usize;
        let mut sessions_matched = HashSet::new();
        let mut retained = BinaryHeap::with_capacity(window.min(MAX_INDEXED_SEARCH_PAGE_SIZE));
        self.visit_candidate_entries(&candidate_filter, |entry| {
            if !entry_matches_filters(&entry, &request.filters)
                || request.exclude.as_ref().is_some_and(|exclude| {
                    projection_matches(&entry.projection, exclude, request.scope)
                })
            {
                return Ok(());
            }
            total_occurrences = total_occurrences.saturating_add(count_projection_matches(
                &entry.projection,
                &request.matcher,
                request.scope,
            ));
            let provider = provider_of(&entry.session_key)?;
            for matched in search_projection(
                &entry.projection,
                &request.matcher,
                request.scope,
                request.context_lines,
            ) {
                total_matches = total_matches.saturating_add(1);
                sessions_matched.insert(entry.session_key.clone());
                retain_match(
                    &mut retained,
                    make_result(&entry, &provider, matched),
                    request.order,
                    window,
                );
            }
            Ok(())
        })?;
        let mut retained: Vec<_> = retained
            .into_iter()
            .map(|retained| retained.value)
            .collect();
        retained.sort_by(|left, right| result_cmp(left, right, request.order));
        retained = retained
            .into_iter()
            .skip(request.offset)
            .take(request.limit)
            .collect();

        let requested_set: BTreeSet<_> = requested.iter().cloned().collect();
        let complete_set: BTreeSet<_> = build.complete_providers.iter().cloned().collect();
        let skipped: Vec<_> = build
            .skipped
            .iter()
            .filter(|skip| {
                skip.provider
                    .as_ref()
                    .is_some_and(|provider| requested_set.contains(provider))
                    || skip.session_key.as_ref().is_some_and(|key| {
                        provider_of(key).is_ok_and(|provider| requested_set.contains(&provider))
                    })
            })
            .cloned()
            .collect();
        let warnings: Vec<_> = build
            .warnings
            .iter()
            .filter(|warning| {
                warning
                    .provider
                    .as_ref()
                    .is_some_and(|provider| requested_set.contains(provider))
                    || warning.session_key.as_ref().is_some_and(|key| {
                        provider_of(key).is_ok_and(|provider| requested_set.contains(&provider))
                    })
            })
            .cloned()
            .collect();
        let complete: Vec<_> = requested
            .iter()
            .filter(|provider| complete_set.contains(*provider))
            .cloned()
            .collect();
        let unverified: Vec<_> = requested
            .iter()
            .filter(|provider| !complete_set.contains(*provider))
            .cloned()
            .collect();
        let mut notes = Vec::new();
        if !unverified.is_empty() {
            notes.push(format!(
                "latest build did not establish complete coverage for: {}",
                unverified.join(", ")
            ));
        }
        if !build.removal_coverage_complete {
            notes.push(
                "latest build was upsert-only or partial; disappeared sessions may remain indexed"
                    .to_string(),
            );
        }
        let incomplete = complete.len() != requested.len()
            || !skipped.is_empty()
            || !build.removal_coverage_complete;
        let matches = retained;
        Ok(IndexedSearchResponse {
            total_matches,
            total_occurrences,
            sessions_matched: sessions_matched.len(),
            returned: matches.len(),
            offset: request.offset,
            limit: request.limit,
            matches,
            coverage: IndexedQueryCoverage {
                schema_version: PROVIDER_INDEX_SCHEMA_VERSION,
                generation: build.generation,
                built_at: build.built_at,
                requested_providers: requested.clone(),
                represented_providers: represented.into_iter().collect(),
                searched_providers: requested,
                complete_providers: complete,
                removal_coverage_complete: build.removal_coverage_complete,
                incomplete,
                skipped,
                warnings,
                notes,
            },
        })
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use chrono::TimeZone as _;
    use tempfile::tempdir;

    use super::*;
    use crate::analysis::search::{EntrySearchProjection, SearchProjectionCoverage, SearchSegment};
    use crate::index::provider::{
        IndexedSessionBatch, IndexedSessionManifest, ProviderIndexBuildManifest,
    };
    use crate::provider::{ProviderId, SessionNamespace};

    fn key(provider: &str, native: &str) -> LogicalSessionKey {
        LogicalSessionKey {
            provider: ProviderId(provider.to_string()),
            namespace: SessionNamespace::global(),
            native_id: native.to_string(),
        }
    }

    fn segment(kind: SearchSegmentKind, text: &str) -> SearchSegment {
        SearchSegment {
            kind,
            text: text.to_string(),
            tool_name: None,
            tool_call_id: None,
            tool_is_error: None,
        }
    }

    fn entry(
        session: &LogicalSessionKey,
        order: usize,
        kind: SearchSegmentKind,
        text: &str,
    ) -> IndexedSearchEntry {
        IndexedSearchEntry {
            session_key: session.to_string(),
            logical_root: session.to_string(),
            project_key: format!("project:{}", session.provider),
            project_path: format!("/work/{}", session.provider),
            entry_id: format!(
                "{}:global:{}:{order}:0",
                session.provider, session.native_id
            ),
            entry_order: order,
            timestamp: Some(
                Utc.timestamp_opt(1_750_000_000 + i64::try_from(order).unwrap(), 0)
                    .unwrap(),
            ),
            message_type: match kind {
                SearchSegmentKind::UserText => "user",
                SearchSegmentKind::AssistantText
                | SearchSegmentKind::Reasoning
                | SearchSegmentKind::ToolInput
                | SearchSegmentKind::ToolResult => "assistant",
                SearchSegmentKind::SystemText => "system",
                SearchSegmentKind::SummaryText => "summary",
            }
            .to_string(),
            model: None,
            git_branch: None,
            activity: "new".to_string(),
            spawned: false,
            prompt_authorship: None,
            prompt_delivery: None,
            tool_kinds: BTreeMap::new(),
            processed_tokens: None,
            projection: EntrySearchProjection {
                segments: vec![segment(kind, text)],
                coverage: SearchProjectionCoverage::default(),
            },
        }
    }

    fn batch(
        session: &LogicalSessionKey,
        generation: &str,
        entries: Vec<IndexedSearchEntry>,
    ) -> IndexedSessionBatch {
        let segment_count = entries
            .iter()
            .map(|entry| entry.projection.segments.len())
            .sum();
        IndexedSessionBatch {
            manifest: IndexedSessionManifest {
                schema_version: PROVIDER_INDEX_SCHEMA_VERSION,
                provider: session.provider.to_string(),
                session_key: session.to_string(),
                logical_root: session.to_string(),
                project_key: format!("project:{}", session.provider),
                project_path: format!("/work/{}", session.provider),
                spawned: entries.first().is_some_and(|entry| entry.spawned),
                revision_token: format!("revision-{generation}"),
                metadata_fingerprint: format!("metadata-{generation}"),
                generation: generation.to_string(),
                indexed_at: Utc.timestamp_opt(1_750_000_000, 0).unwrap(),
                source_started_at: None,
                source_ended_at: None,
                source_modified_at: None,
                entry_count: entries.len(),
                segment_count,
                coverage: SearchProjectionCoverage::default(),
            },
            entries,
        }
    }

    fn build(
        generation: &str,
        selected: &[&str],
        complete: &[&str],
        skipped: Vec<IndexedSkip>,
    ) -> ProviderIndexBuildManifest {
        ProviderIndexBuildManifest {
            schema_version: PROVIDER_INDEX_SCHEMA_VERSION,
            generation: generation.to_string(),
            built_at: Utc.timestamp_opt(1_750_000_000, 0).unwrap(),
            selected_providers: selected.iter().map(ToString::to_string).collect(),
            complete_providers: complete.iter().map(ToString::to_string).collect(),
            removal_coverage_complete: skipped.is_empty() && selected == complete,
            skipped,
            warnings: Vec::new(),
        }
    }

    fn request(provider: &str, pattern: &str) -> IndexedSearchRequest {
        IndexedSearchRequest {
            selection: IndexedProviderSelection::Explicit(vec![provider.to_string()]),
            matcher: ExactSearchMatcher::regex(pattern, false).unwrap(),
            exclude: None,
            scope: SearchScope::Default,
            filters: IndexedSearchFilters::default(),
            context_lines: 1,
            order: IndexedSearchOrder::Source,
            offset: 0,
            limit: 50,
        }
    }

    #[test]
    fn exact_query_preserves_emissions_occurrences_context_and_pagination() {
        let dir = tempdir().unwrap();
        let index = ProviderSearchIndex::open(dir.path().join("index")).unwrap();
        let session = key("alpha", "one");
        let mut first = entry(
            &session,
            0,
            SearchSegmentKind::AssistantText,
            "😀 before\nneedle needle\n世界 after",
        );
        first
            .projection
            .segments
            .push(first.projection.segments[0].clone());
        let reasoning = entry(&session, 1, SearchSegmentKind::Reasoning, "private needle");
        index
            .apply_generation(
                &[batch(&session, "g1", vec![first, reasoning])],
                &[],
                &build("g1", &["alpha"], &["alpha"], Vec::new()),
            )
            .unwrap();

        let mut query = request("alpha", "needle");
        query.limit = 1;
        let first_page = index.query(&query).unwrap();
        assert_eq!(first_page.total_matches, 2);
        assert_eq!(first_page.total_occurrences, 4);
        assert_eq!(first_page.returned, 1);
        assert_eq!(first_page.matches[0].segment_index, 0);
        assert_eq!(first_page.matches[0].context_before, "😀 before");
        assert_eq!(first_page.matches[0].context_after, "世界 after");

        query.offset = 1;
        let second_page = index.query(&query).unwrap();
        assert_eq!(second_page.matches[0].segment_index, 1);
        query.scope = SearchScope::Thinking;
        query.offset = 0;
        query.limit = 50;
        assert_eq!(index.query(&query).unwrap().total_matches, 3);
        query.scope = SearchScope::ThinkingOnly;
        assert_eq!(index.query(&query).unwrap().total_matches, 1);

        query.matcher = ExactSearchMatcher::fuzzy("ndle", false, 0);
        query.scope = SearchScope::Default;
        assert_eq!(index.query(&query).unwrap().total_matches, 2);
    }

    #[test]
    fn cross_session_defaults_exclude_inherited_and_spawned_but_selected_sessions_are_complete() {
        let dir = tempdir().unwrap();
        let index = ProviderSearchIndex::open(dir.path().join("index")).unwrap();
        let main = key("alpha", "main");
        let fork = key("alpha", "fork");
        let child = key("alpha", "child");
        let fresh = entry(&main, 0, SearchSegmentKind::AssistantText, "needle fresh");
        let mut inherited = entry(
            &fork,
            0,
            SearchSegmentKind::AssistantText,
            "needle inherited",
        );
        inherited.activity = "inherited-history".to_string();
        let mut spawned = entry(
            &child,
            0,
            SearchSegmentKind::AssistantText,
            "needle spawned",
        );
        spawned.spawned = true;
        index
            .apply_generation(
                &[
                    batch(&main, "g1", vec![fresh]),
                    batch(&fork, "g1", vec![inherited]),
                    batch(&child, "g1", vec![spawned]),
                ],
                &[],
                &build("g1", &["alpha"], &["alpha"], Vec::new()),
            )
            .unwrap();

        let mut query = request("alpha", "needle");
        assert_eq!(index.query(&query).unwrap().total_matches, 1);
        query.filters.include_inherited = true;
        query.filters.include_spawned = true;
        assert_eq!(index.query(&query).unwrap().total_matches, 3);

        query.filters = IndexedSearchFilters {
            session_keys: vec![fork.to_string()],
            ..IndexedSearchFilters::default()
        };
        assert_eq!(index.query(&query).unwrap().total_matches, 1);
        query.filters.session_keys = vec![child.to_string()];
        assert_eq!(index.query(&query).unwrap().total_matches, 1);
    }

    #[test]
    fn typed_filters_each_reject_a_near_miss_and_accept_the_exact_entry() {
        let dir = tempdir().unwrap();
        let index = ProviderSearchIndex::open(dir.path().join("index")).unwrap();
        let session = key("alpha", "filtered");
        let mut matching = entry(&session, 0, SearchSegmentKind::ToolInput, "needle payload");
        matching.model = Some("model-pro".to_string());
        matching.git_branch = Some("feature/search".to_string());
        matching.prompt_authorship = Some("human".to_string());
        matching.prompt_delivery = Some("turn-boundary".to_string());
        matching.processed_tokens = Some(42);
        matching
            .tool_kinds
            .insert("call-1".to_string(), "file-read".to_string());
        matching.projection.segments[0].tool_name = Some("ReadFile".to_string());
        matching.projection.segments.push(SearchSegment {
            kind: SearchSegmentKind::ToolResult,
            text: "needle result".to_string(),
            tool_name: None,
            tool_call_id: Some("call-1".to_string()),
            tool_is_error: Some(true),
        });
        let timestamp = matching.timestamp.unwrap();
        index
            .apply_generation(
                &[batch(&session, "g1", vec![matching])],
                &[],
                &build("g1", &["alpha"], &["alpha"], Vec::new()),
            )
            .unwrap();

        let mut query = request("alpha", "needle");
        query.scope = SearchScope::Tools;
        query.filters = IndexedSearchFilters {
            session_keys: vec![session.to_string()],
            logical_roots: vec![session.to_string()],
            project_keys: vec!["project:alpha".to_string()],
            project_contains: Some("/WORK/ALPHA".to_string()),
            message_types: vec!["assistant".to_string()],
            model_contains: Some("PRO".to_string()),
            tool_name_contains: Some("read".to_string()),
            tool_kinds: vec!["file-read".to_string()],
            git_branch_contains: Some("SEARCH".to_string()),
            prompt_authorship: vec!["human".to_string()],
            prompt_delivery: vec!["turn-boundary".to_string()],
            min_processed_tokens: Some(42),
            max_processed_tokens: Some(42),
            timestamp_from: Some(timestamp),
            timestamp_until: Some(timestamp),
            tool_error: Some(true),
            include_inherited: false,
            include_spawned: false,
        };
        assert_eq!(index.query(&query).unwrap().total_matches, 2);

        let mutations = [
            ("project", "missing"),
            ("message", "user"),
            ("model", "basic"),
            ("tool-name", "write"),
            ("tool-kind", "file-write"),
            ("branch", "main"),
            ("authorship", "harness"),
            ("delivery", "mid-turn"),
        ];
        for (field, wrong) in mutations {
            let mut changed = query.clone();
            match field {
                "project" => changed.filters.project_contains = Some(wrong.to_string()),
                "message" => changed.filters.message_types = vec![wrong.to_string()],
                "model" => changed.filters.model_contains = Some(wrong.to_string()),
                "tool-name" => changed.filters.tool_name_contains = Some(wrong.to_string()),
                "tool-kind" => changed.filters.tool_kinds = vec![wrong.to_string()],
                "branch" => changed.filters.git_branch_contains = Some(wrong.to_string()),
                "authorship" => changed.filters.prompt_authorship = vec![wrong.to_string()],
                "delivery" => changed.filters.prompt_delivery = vec![wrong.to_string()],
                _ => unreachable!(),
            }
            assert_eq!(index.query(&changed).unwrap().total_matches, 0, "{field}");
        }
        let mut wrong = query.clone();
        wrong.filters.min_processed_tokens = Some(43);
        wrong.filters.max_processed_tokens = None;
        assert_eq!(index.query(&wrong).unwrap().total_matches, 0);
        let mut wrong = query.clone();
        wrong.filters.timestamp_from = Some(timestamp + chrono::Duration::seconds(1));
        wrong.filters.timestamp_until = None;
        assert_eq!(index.query(&wrong).unwrap().total_matches, 0);
        let mut wrong = query;
        wrong.filters.tool_error = Some(false);
        assert_eq!(index.query(&wrong).unwrap().total_matches, 0);
    }

    #[test]
    fn partial_generation_searches_retained_partitions_but_never_claims_complete_coverage() {
        let dir = tempdir().unwrap();
        let index = ProviderSearchIndex::open(dir.path().join("index")).unwrap();
        let alpha = key("alpha", "one");
        let beta = key("beta", "one");
        index
            .apply_generation(
                &[
                    batch(
                        &alpha,
                        "g1",
                        vec![entry(
                            &alpha,
                            0,
                            SearchSegmentKind::AssistantText,
                            "needle alpha old",
                        )],
                    ),
                    batch(
                        &beta,
                        "g1",
                        vec![entry(
                            &beta,
                            0,
                            SearchSegmentKind::AssistantText,
                            "needle beta retained",
                        )],
                    ),
                ],
                &[],
                &build("g1", &["alpha", "beta"], &["alpha", "beta"], Vec::new()),
            )
            .unwrap();
        let skipped = IndexedSkip {
            provider: Some("beta".to_string()),
            session_key: None,
            reason: "details withheld".to_string(),
        };
        index
            .apply_generation(
                &[batch(
                    &alpha,
                    "g2",
                    vec![entry(
                        &alpha,
                        0,
                        SearchSegmentKind::AssistantText,
                        "needle alpha new",
                    )],
                )],
                &[],
                &build("g2", &["alpha", "beta"], &["alpha"], vec![skipped]),
            )
            .unwrap();

        let mut query = request("alpha", "needle");
        query.selection = IndexedProviderSelection::All;
        let all = index.query(&query).unwrap();
        assert_eq!(all.total_matches, 2);
        assert_eq!(all.coverage.searched_providers, vec!["alpha", "beta"]);
        assert_eq!(all.coverage.complete_providers, vec!["alpha"]);
        assert!(all.coverage.incomplete);
        assert_eq!(all.coverage.skipped.len(), 1);
        assert!(all.coverage.notes.iter().any(|note| note.contains("beta")));

        query.selection = IndexedProviderSelection::Explicit(vec!["beta".to_string()]);
        let beta_only = index.query(&query).unwrap();
        assert_eq!(beta_only.total_matches, 1);
        assert!(beta_only.coverage.incomplete);

        query.selection = IndexedProviderSelection::Explicit(vec!["missing".to_string()]);
        assert!(index
            .query(&query)
            .unwrap_err()
            .to_string()
            .contains("absent from the committed index"));
    }

    #[test]
    fn relevance_order_is_deterministic_and_exclusion_uses_the_same_line_scope() {
        let dir = tempdir().unwrap();
        let index = ProviderSearchIndex::open(dir.path().join("index")).unwrap();
        let first = key("alpha", "a");
        let second = key("alpha", "b");
        index
            .apply_generation(
                &[
                    batch(
                        &first,
                        "g1",
                        vec![entry(
                            &first,
                            0,
                            SearchSegmentKind::AssistantText,
                            "prefix needle\nexclude on another line",
                        )],
                    ),
                    batch(
                        &second,
                        "g1",
                        vec![entry(
                            &second,
                            0,
                            SearchSegmentKind::AssistantText,
                            "needle",
                        )],
                    ),
                ],
                &[],
                &build("g1", &["alpha"], &["alpha"], Vec::new()),
            )
            .unwrap();
        let mut query = request("alpha", "needle");
        query.order = IndexedSearchOrder::Relevance;
        let result = index.query(&query).unwrap();
        assert_eq!(result.matches[0].session_key, second.to_string());
        assert_eq!(result.matches[1].session_key, first.to_string());

        query.exclude = Some(ExactSearchMatcher::regex("exclude", false).unwrap());
        let excluded = index.query(&query).unwrap();
        assert_eq!(excluded.total_matches, 1);
        assert_eq!(excluded.matches[0].session_key, second.to_string());
    }

    #[test]
    fn empty_complete_provider_and_request_bounds_are_unambiguous() {
        let dir = tempdir().unwrap();
        let index = ProviderSearchIndex::open(dir.path().join("index")).unwrap();
        index
            .apply_generation(&[], &[], &build("g1", &["empty"], &["empty"], Vec::new()))
            .unwrap();
        let mut query = request("empty", "needle");
        query.limit = 0;
        let empty = index.query(&query).unwrap();
        assert_eq!(empty.total_matches, 0);
        assert_eq!(empty.returned, 0);
        assert!(!empty.coverage.incomplete);

        query.limit = MAX_INDEXED_SEARCH_PAGE_SIZE + 1;
        assert!(index.query(&query).is_err());
        query.limit = 1;
        query.matcher = ExactSearchMatcher::fuzzy("needle", false, 101);
        assert!(index.query(&query).is_err());
        query.matcher = ExactSearchMatcher::regex("needle", false).unwrap();
        query.filters.min_processed_tokens = Some(2);
        query.filters.max_processed_tokens = Some(1);
        assert!(index.query(&query).is_err());
        query.filters.min_processed_tokens = None;
        query.filters.max_processed_tokens = None;
        query.filters.timestamp_from = Some(Utc.timestamp_opt(20, 0).unwrap());
        query.filters.timestamp_until = Some(Utc.timestamp_opt(10, 0).unwrap());
        assert!(index.query(&query).is_err());
        query.filters.timestamp_from = None;
        query.filters.timestamp_until = None;
        query.offset = MAX_INDEXED_SEARCH_WINDOW;
        query.limit = 1;
        assert!(index.query(&query).is_err());
    }

    #[test]
    fn streaming_candidates_never_resurrect_replaced_documents() {
        let dir = tempdir().unwrap();
        let index = ProviderSearchIndex::open(dir.path().join("index")).unwrap();
        let session = key("alpha", "replaced");
        index
            .apply_generation(
                &[batch(
                    &session,
                    "g1",
                    vec![entry(
                        &session,
                        0,
                        SearchSegmentKind::AssistantText,
                        "stale needle",
                    )],
                )],
                &[],
                &build("g1", &["alpha"], &["alpha"], Vec::new()),
            )
            .unwrap();
        index
            .apply_generation(
                &[batch(
                    &session,
                    "g2",
                    vec![entry(
                        &session,
                        0,
                        SearchSegmentKind::AssistantText,
                        "fresh needle",
                    )],
                )],
                &[],
                &build("g2", &["alpha"], &["alpha"], Vec::new()),
            )
            .unwrap();

        assert_eq!(
            index
                .query(&request("alpha", "stale"))
                .unwrap()
                .total_matches,
            0
        );
        assert_eq!(
            index
                .query(&request("alpha", "fresh"))
                .unwrap()
                .total_matches,
            1
        );
    }

    #[test]
    fn invalid_normalized_entry_order_is_rejected_before_index_mutation() {
        let dir = tempdir().unwrap();
        let index = ProviderSearchIndex::open(dir.path().join("index")).unwrap();
        let session = key("alpha", "ordered");
        let mut wrong = entry(&session, 0, SearchSegmentKind::AssistantText, "needle");
        wrong.entry_order = 7;
        let error = index
            .apply_generation(
                &[batch(&session, "g1", vec![wrong])],
                &[],
                &build("g1", &["alpha"], &["alpha"], Vec::new()),
            )
            .unwrap_err()
            .to_string();
        assert!(error.contains("has order 7, expected 0"));
        assert!(index.is_empty());
    }
}
