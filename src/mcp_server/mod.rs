//! MCP (Model Context Protocol) server implementation.
//!
//! Exposes claude-snatch functionality as MCP tools for AI model integration.
//!
//! # Tools Provided
//!
//! - `list_sessions` - List agent sessions (Claude Code by default)
//! - `get_session_info` - Get detailed session information
//! - `get_stats` - Get usage statistics
//! - `get_session_messages` - Read conversation messages at different detail levels
//! - `get_session_timeline` - Get turn-by-turn narrative of a session
//! - `get_project_history` - Cross-session overview for a project
//! - `search_sessions` - Regex search across sessions (supports thinking blocks)
//! - `get_tool_calls` - Extract tool invocations with summaries
//! - `get_session_lessons` - Extract error→fix pairs and user corrections
//! - `manage_goals` - Persistent goal tracking across sessions and compactions
//! - `get_session_digest` - Compact session summary for orientation after compaction
//! - `manage_notes` - Tactical session notes that survive compaction
//! - `manage_decisions` - Persistent decision registry across sessions
//! - `get_file_history` - Reverse index: file path → sessions that modified it
//! - `thread_topic` - Cross-session topic threading with conversation context
//! - `get_project_health` - Project health dashboard: hotspots, rework, error trends
//! - `get_event_context` - Contextual zoom around a specific event by message_id or timestamp
//! - `explain_file_evolution` - Why a file changed: modification history with conversation context
//! - `suggest_priorities` - What to work on next: errors, churn, goals, decisions ranked by score

#![cfg(feature = "mcp")]
// The #[tool] handlers must be `async fn` to satisfy the mcpkit tool signature,
// even when a handler does no awaiting; the resulting unused_async is expected.
#![allow(clippy::unused_async)]

pub mod helpers;
pub mod types;

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use mcpkit::prelude::*;
use mcpkit::transport::stdio::StdioTransport;

use crate::analytics::{AnalyticsSummary, SessionAnalytics};
use crate::discovery::{chain::detect_chains, ClaudeDirectory, Session};
use crate::model::message::LogEntry;
use crate::reconstruction::Conversation;

use helpers::{
    boundary_prompt_text, extract_assistant_summary, extract_error_preview,
    extract_files_from_tools, extract_image_placeholders, extract_result_preview,
    extract_thinking_text, extract_tool_input_summary, extract_tool_names,
    extract_user_prompt_text, failed_tool_use_ids, get_claude_dir, get_model, has_thinking,
    has_tool_errors, is_human_prompt, is_prompt_boundary, main_thread_message_total,
    parse_timestamp_param, period_cutoff, queued_human_prompt, render_attachment_content,
    resolve_project, resolve_session, resolve_session_with_chain, search_entry_text,
    thinking_redaction_note, truncate_text,
};
use types::{
    ChangeEventEntry, ChunkBranchSummary, ChunkInfo, ChunkSummary, CompactionEvent,
    ContextTurnEntry, DecisionChurnEntry, DecisionEntry, ErrorFixLesson,
    ExplainFileEvolutionRequest, ExplainFileEvolutionResponse, FileEvolutionEntry,
    FileModificationEntry, GetEventContextRequest, GetEventContextResponse, GetFileHistoryRequest,
    GetFileHistoryResponse, GetProjectHealthRequest, GetProjectHealthResponse,
    GetProjectHistoryRequest, GetSessionDigestRequest, GetSessionInfoRequest,
    GetSessionLessonsRequest, GetSessionMessagesRequest, GetSessionTimelineRequest,
    GetStatsRequest, GetToolCallsRequest, GoalEntry, HotspotFileEntry, LessonsSummary,
    ListSessionsRequest, ManageDecisionsRequest, ManageDecisionsResponse, ManageGoalsRequest,
    ManageGoalsResponse, ManageNotesRequest, ManageNotesResponse, MessageEntry, NoteEntry,
    PriorityItemEntry, PrioritySourceEntry, ProjectAggregate, ProjectHistoryResponse,
    ProjectSessionEntry, ReworkFileEntry, SearchMatch, SearchSessionsRequest,
    SearchSessionsResponse, SessionDigestResponse, SessionHealthEntry, SessionInfoResponse,
    SessionLessonsResponse, SessionMessagesResponse, SessionSummary, SessionTimelineResponse,
    StatsResponse, SubagentSummary, SuggestPrioritiesRequest, SuggestPrioritiesResponse,
    ThreadExchangeEntry, ThreadTopicRequest, ThreadTopicResponse, TimelineTurn, ToolCallEntry,
    ToolCallsResponse, ToolCallsSummary, ToolDetail, ToolLifecycleEntry, UnmatchedSubagent,
    UserCorrection,
};

// ============================================================================
// MCP Server Implementation
// ============================================================================

/// Claude-snatch MCP server.
#[derive(Debug, Clone)]
pub struct SnatchServer {
    /// Claude directory path.
    claude_dir: Option<PathBuf>,
    /// Optional explicit Codex directory (embedded/test callers); normal CLI
    /// server startup leaves this unset and uses Codex discovery.
    codex_dir: Option<PathBuf>,
    /// Maximum file size for parsing.
    max_file_size: Option<u64>,
}

impl SnatchServer {
    /// Create a new MCP server instance.
    pub fn new(claude_dir: Option<PathBuf>, max_file_size: Option<u64>) -> Self {
        Self {
            claude_dir,
            codex_dir: None,
            max_file_size,
        }
    }

    /// Override the Codex data root for an embedded server.
    #[must_use]
    pub fn with_codex_dir(mut self, codex_dir: impl Into<PathBuf>) -> Self {
        self.codex_dir = Some(codex_dir.into());
        self
    }

    /// Get the Claude directory.
    /// Provider-neutral session listing for `list_sessions` with `provider`
    /// set. Project filtering uses the same unified identity as project
    /// history; subagent filtering is driven by typed Spawn lineage.
    fn provider_sessions_output(
        &self,
        flags: &[String],
        project_filter: Option<&str>,
        include_subagents: bool,
        limit: usize,
    ) -> ToolOutput {
        use crate::provider::registry::ProviderSelection;
        use crate::provider::LineageEdgeKind;
        use std::collections::BTreeSet;

        let selection = match ProviderSelection::from_flags(flags) {
            Ok(s) => s,
            Err(reason) => {
                return ToolOutput::error(format!("Invalid provider selection: {reason}"))
            }
        };
        let registry = self.provider_registry();
        let collected = match registry.collect_project_union(&selection) {
            Ok(c) => c,
            Err(e) => return ToolOutput::error(e.to_string()),
        };

        let mut spawned = BTreeSet::new();
        for edge in &collected.lineage {
            if matches!(edge.kind, LineageEdgeKind::Spawn { .. }) {
                spawned.insert(edge.to.clone());
            }
        }

        let mut rows: Vec<serde_json::Value> = collected
            .projects
            .iter()
            .filter(|project| project_filter.map_or(true, |needle| project.matches(needle)))
            .flat_map(|project| {
                project.sessions.iter().filter_map(|session| {
                    let descriptor = &session.descriptor;
                    if !include_subagents && spawned.contains(&descriptor.key) {
                        return None;
                    }
                    Some(serde_json::json!({
                        "provider": descriptor.key.provider.to_string(),
                        "qualified_id": descriptor.key.to_string(),
                        "native_id": descriptor.key.native_id,
                        "artifacts": descriptor.artifacts.len(),
                        "project_key": project.identity.to_string(),
                        "project_path": project.display_path,
                        "git_repository": project.git_repository,
                        "is_subagent": spawned.contains(&descriptor.key),
                    }))
                })
            })
            .collect();
        rows.sort_by(|a, b| a["qualified_id"].as_str().cmp(&b["qualified_id"].as_str()));
        let total = rows.len();
        // ALWAYS truncate to the requested limit — classic MCP semantics
        // (`limit: 0` means zero rows, not unlimited; round-19 blocker 3).
        rows.truncate(limit);
        let out = serde_json::json!({
            "sessions": rows,
            "total": total,
            "skipped_providers": collected
                .skipped
                .iter()
                .map(|(id, reason)| serde_json::json!({"provider": id.to_string(), "reason": reason}))
                .collect::<Vec<_>>(),
        });
        match ToolOutput::json(&out) {
            Ok(o) => o,
            Err(e) => ToolOutput::error(format!("JSON serialization error: {e}")),
        }
    }

    /// Provider-neutral session info for `get_session_info` with `provider`
    /// set or a provider-qualified session id.
    fn provider_session_info_output(
        &self,
        registry: &crate::provider::registry::ProviderRegistry,
        flags: &[String],
        reference: &str,
    ) -> ToolOutput {
        use crate::provider::registry::cached_parsed_session;
        use crate::provider::ArtifactForm;

        let resolution = match registry.resolve_with_default_policy(flags, reference) {
            Ok(r) => r,
            Err(e) => return ToolOutput::error(e.to_string()),
        };
        let provider = resolution.provider;
        let key = &resolution.key;

        // Complete bundle: provenance/semantics travel with the entries
        // through both the cache and reconstruction (round-18).
        let parsed = match cached_parsed_session(crate::cache::global_cache(), provider, key) {
            Ok(p) => p,
            Err(e) => return ToolOutput::error(format!("Failed to parse session: {e}")),
        };
        let conversation = match Conversation::from_parsed_session(parsed.clone()) {
            Ok(c) => c,
            Err(e) => return ToolOutput::error(format!("Failed to reconstruct conversation: {e}")),
        };
        let conversation_nodes = conversation.len();
        let usage = crate::analysis::usage::provider_usage_summary(
            &conversation,
            provider.capabilities().pricing,
        );
        let descriptor = &parsed.descriptor;
        let capabilities = provider.capabilities();
        let d = &parsed.diagnostics;
        let out = serde_json::json!({
            "qualified_id": key.to_string(),
            "provider": key.provider.to_string(),
            "namespace": key.namespace.0,
            "native_id": key.native_id,
            "entries": parsed.entries.len(),
            "conversation_nodes": conversation_nodes,
            "record_dispositions": {
                "mapped": d.mapped,
                "suppressed": d.suppressed,
                "unknown": d.unknown,
                "recovered": d.recovered,
                "unparseable": d.unparseable,
            },
            "semantic_annotations": parsed.semantics.len(),
            "usage": usage,
            "artifacts": descriptor
                .artifacts
                .iter()
                .map(|a| serde_json::json!({
                    "locator": a.snapshot.id.locator,
                    "form": match &a.form {
                        ArtifactForm::PlainFile => "plain",
                        ArtifactForm::CompressedFile => "compressed",
                        ArtifactForm::Database => "database",
                        ArtifactForm::Other(o) => o.as_str(),
                    },
                    "archived": a.archived,
                }))
                .collect::<Vec<_>>(),
            "capabilities": {
                "native_export": capabilities.native_export,
                "raw_jsonl": capabilities.raw_jsonl,
            },
        });
        match ToolOutput::json(&out) {
            Ok(o) => o,
            Err(e) => ToolOutput::error(format!("JSON serialization error: {e}")),
        }
    }

    /// Resolve a provider-selected/qualified session into the same complete
    /// bundle-backed shape used by the CLI. This is the MCP acquisition
    /// chokepoint for provider-aware message/timeline consumers.
    fn resolve_provider_session(
        &self,
        flags: &[String],
        reference: &str,
    ) -> Result<helpers::ResolvedSession, ToolOutput> {
        let registry = self.provider_registry();
        let resolution = registry
            .resolve_with_default_policy(flags, reference)
            .map_err(|e| ToolOutput::error(e.to_string()))?;
        let parsed = crate::provider::registry::cached_parsed_session(
            crate::cache::global_cache(),
            resolution.provider,
            &resolution.key,
        )
        .map_err(|e| ToolOutput::error(format!("Failed to parse session: {e}")))?;
        let project_path = parsed
            .entries
            .iter()
            .find_map(|entry| entry.entry.cwd().map(String::from))
            .unwrap_or_else(|| "unknown".to_string());
        let conversation = Conversation::from_parsed_session(parsed)
            .map_err(|e| ToolOutput::error(format!("Failed to reconstruct conversation: {e}")))?;
        let analytics = SessionAnalytics::from_conversation(&conversation);
        Ok(helpers::ResolvedSession {
            session_id: resolution.key.native_id.clone(),
            project_path,
            conversation,
            analytics,
            provider: resolution.key.provider.to_string(),
            qualified_id: resolution.key.to_string(),
            semantic_annotations: resolution.provider.capabilities().semantic_annotations,
        })
    }

    /// Resolve an analytical request through the compatibility provider
    /// policy: unqualified/flagless stays Claude-only; flags or a qualified
    /// id use the provider registry. This keeps every migrated MCP surface on
    /// the same no-fallback resolution contract.
    fn resolve_analytical_session(
        &self,
        provider_flags: &[String],
        reference: &str,
    ) -> Result<helpers::ResolvedSession, ToolOutput> {
        let registry = self.provider_registry();
        if !provider_flags.is_empty() || registry.looks_qualified(reference) {
            self.resolve_provider_session(provider_flags, reference)
        } else {
            resolve_session(self, reference)
        }
    }

    fn provider_thread_result(
        &self,
        request: &ThreadTopicRequest,
        regex: &regex::Regex,
        params: &crate::analysis::threading::ThreadParams,
    ) -> Result<
        (
            crate::analysis::threading::ThreadResult,
            Vec<String>,
            Vec<String>,
        ),
        ToolOutput,
    > {
        use crate::analysis::threading::{
            finish_thread_exchanges, thread_one_conversation, ThreadConversation,
        };
        use crate::provider::registry::{cached_parsed_session, ProviderSelection};
        use crate::provider::LineageEdgeKind;

        fn overlaps(
            conversation: &Conversation,
            since: Option<chrono::DateTime<chrono::Utc>>,
            until: Option<chrono::DateTime<chrono::Utc>>,
        ) -> bool {
            let mut timestamps = conversation
                .chronological_entries()
                .into_iter()
                .filter_map(crate::model::message::LogEntry::timestamp);
            let Some(first) = timestamps.next() else {
                return true;
            };
            let (start, end) = timestamps.fold((first, first), |(start, end), timestamp| {
                (start.min(timestamp), end.max(timestamp))
            });
            since.map_or(true, |bound| end >= bound) && until.map_or(true, |bound| start <= bound)
        }

        let since = request
            .since
            .as_deref()
            .map(parse_timestamp_param)
            .transpose()
            .map_err(|error| ToolOutput::error(format!("Date filter error: {error}")))?;
        let until = request
            .until
            .as_deref()
            .map(parse_timestamp_param)
            .transpose()
            .map_err(|error| ToolOutput::error(format!("Date filter error: {error}")))?;
        let no_subagents = request.no_subagents.unwrap_or(true);
        let provider_flags = request.provider.as_deref().unwrap_or(&[]);
        let registry = self.provider_registry();
        let mut exchanges = Vec::new();
        let mut warnings = Vec::new();
        let mut skipped = Vec::new();

        if let Some(session_id) = request.session_id.as_deref() {
            let resolved = self.resolve_analytical_session(provider_flags, session_id)?;
            let key: crate::provider::LogicalSessionKey =
                resolved.qualified_id.parse().map_err(ToolOutput::error)?;
            if request
                .project
                .as_deref()
                .is_some_and(|project| !resolved.project_path.contains(project))
                || !overlaps(&resolved.conversation, since, until)
            {
                return Ok((
                    finish_thread_exchanges(Vec::new(), params.limit),
                    skipped,
                    warnings,
                ));
            }
            if no_subagents {
                let provider = registry
                    .get(&key.provider)
                    .map_err(|error| ToolOutput::error(error.to_string()))?;
                let spawned = provider
                    .lineage()
                    .map_err(|error| ToolOutput::error(error.to_string()))?
                    .into_iter()
                    .any(|edge| {
                        edge.to == key && matches!(edge.kind, LineageEdgeKind::Spawn { .. })
                    });
                if spawned || key.namespace.0.starts_with("subagent:") {
                    return Ok((
                        finish_thread_exchanges(Vec::new(), params.limit),
                        skipped,
                        warnings,
                    ));
                }
            }
            exchanges.extend(thread_one_conversation(
                &ThreadConversation {
                    provider: &resolved.provider,
                    qualified_id: &resolved.qualified_id,
                    session_id: &resolved.session_id,
                    project: &resolved.project_path,
                    conversation: &resolved.conversation,
                    semantic_annotations: resolved.semantic_annotations,
                },
                regex,
                params,
            ));
        } else {
            let selection =
                ProviderSelection::from_flags(provider_flags).map_err(ToolOutput::error)?;
            let collected = registry
                .collect_project_union(&selection)
                .map_err(|error| ToolOutput::error(error.to_string()))?;
            skipped.extend(
                collected
                    .skipped
                    .iter()
                    .map(|(provider, _)| format!("{provider}: unavailable")),
            );
            let spawned: std::collections::BTreeSet<_> = collected
                .lineage
                .iter()
                .filter(|edge| matches!(edge.kind, LineageEdgeKind::Spawn { .. }))
                .map(|edge| edge.to.clone())
                .collect();
            for project in &collected.projects {
                if request
                    .project
                    .as_deref()
                    .is_some_and(|needle| !project.matches(needle))
                {
                    continue;
                }
                for session in &project.sessions {
                    let key = &session.descriptor.key;
                    if no_subagents
                        && (spawned.contains(key) || key.namespace.0.starts_with("subagent:"))
                    {
                        continue;
                    }
                    let provider = match registry.get(&key.provider) {
                        Ok(provider) => provider,
                        Err(_) => {
                            warnings.push(format!("{key}: provider unavailable"));
                            continue;
                        }
                    };
                    let parsed =
                        match cached_parsed_session(crate::cache::global_cache(), provider, key) {
                            Ok(parsed) => parsed,
                            Err(_) => {
                                warnings.push(format!("{key}: session could not be parsed"));
                                continue;
                            }
                        };
                    let conversation = match Conversation::from_parsed_session(parsed) {
                        Ok(conversation) => conversation,
                        Err(_) => {
                            warnings
                                .push(format!("{key}: conversation could not be reconstructed"));
                            continue;
                        }
                    };
                    if !overlaps(&conversation, since, until) {
                        continue;
                    }
                    let qualified = key.to_string();
                    let project_path = session
                        .context
                        .cwd
                        .as_deref()
                        .or(project.display_path.as_deref())
                        .unwrap_or("unknown");
                    exchanges.extend(thread_one_conversation(
                        &ThreadConversation {
                            provider: &key.provider.0,
                            qualified_id: &qualified,
                            session_id: &key.native_id,
                            project: project_path,
                            conversation: &conversation,
                            semantic_annotations: provider.capabilities().semantic_annotations,
                        },
                        regex,
                        params,
                    ));
                }
            }
        }

        warnings.sort();
        skipped.sort();
        Ok((
            finish_thread_exchanges(exchanges, params.limit),
            skipped,
            warnings,
        ))
    }

    fn render_thread_result(
        request: &ThreadTopicRequest,
        result: crate::analysis::threading::ThreadResult,
        skipped_providers: Vec<String>,
        warnings: Vec<String>,
    ) -> ToolOutput {
        use crate::cli::helpers::truncate;

        let max_context = request.max_context.unwrap_or(500);
        let exchanges = result
            .exchanges
            .into_iter()
            .map(|exchange| ThreadExchangeEntry {
                timestamp: exchange.timestamp.to_rfc3339(),
                session_id: exchange.session_id,
                provider: exchange.provider,
                qualified_id: exchange.qualified_id,
                project: exchange.project,
                entry_uuid: exchange.entry_uuid,
                user_text: exchange.user_text.map(|text| truncate(&text, max_context)),
                assistant_text: exchange
                    .assistant_text
                    .map(|text| truncate(&text, max_context)),
                thinking_text: exchange
                    .thinking_text
                    .map(|text| truncate(&text, max_context)),
                match_location: exchange.match_location,
                match_provenance: exchange.match_provenance,
                match_count: exchange.match_count,
            })
            .collect::<Vec<_>>();
        let response = ThreadTopicResponse {
            pattern: request.pattern.clone(),
            total_exchanges: exchanges.len(),
            session_count: result.session_count,
            total_matches: result.total_matches,
            exchanges,
            skipped_providers,
            warnings,
        };
        ToolOutput::json(&response)
            .unwrap_or_else(|error| ToolOutput::error(format!("JSON error: {error}")))
    }

    /// Build the provider registry from the server's global options — the
    /// ONE construction path for MCP surfaces (round-18 blocker 4: parsing
    /// limits must never be dropped).
    fn provider_registry(&self) -> crate::provider::registry::ProviderRegistry {
        crate::provider::registry::ProviderRegistry::with_config(
            &crate::provider::registry::RegistryConfig {
                claude_root: self.claude_dir.clone(),
                codex_root: self.codex_dir.clone(),
                max_file_size: self.max_file_size,
            },
        )
    }

    /// Provider-neutral project history used when the request explicitly
    /// selects one or more providers. The classic flagless implementation
    /// remains byte-for-byte compatible in the tool method below.
    fn provider_project_history(&self, request: &GetProjectHistoryRequest) -> ToolOutput {
        use crate::analysis::usage::provider_usage_summary;
        use crate::provider::project::{history_units, new_activity_entries};
        use crate::provider::registry::{cached_parsed_session, ProviderSelection};
        use crate::provider::{ActivityKind, LineageEdgeKind, PromptAuthorship, SessionNamespace};

        let Some(flags) = request.provider.as_ref() else {
            return ToolOutput::error("provider selection missing");
        };
        if flags.is_empty() {
            return ToolOutput::error("provider must name at least one provider or 'all'");
        }
        let selection = match ProviderSelection::from_flags(flags) {
            Ok(selection) => selection,
            Err(error) => return ToolOutput::error(error),
        };
        let atomic = matches!(selection, ProviderSelection::Explicit(_));
        let registry = self.provider_registry();
        let collected = match registry.collect_project_union(&selection) {
            Ok(collected) => collected,
            Err(error) => return ToolOutput::error(error.to_string()),
        };
        let lineage = &collected.lineage;

        let mut matches: Vec<_> = collected
            .projects
            .iter()
            .filter(|project| project.matches(&request.project))
            .collect();
        let exact: Vec<usize> = matches
            .iter()
            .enumerate()
            .filter(|(_, project)| {
                project.identity.to_string() == request.project
                    || project.display_path.as_deref() == Some(request.project.as_str())
            })
            .map(|(index, _)| index)
            .collect();
        if exact.len() == 1 {
            let project = matches.swap_remove(exact[0]);
            matches.clear();
            matches.push(project);
        }
        if matches.len() > 1 {
            let mut candidates: Vec<_> = matches
                .iter()
                .map(|project| project.identity.to_string())
                .collect();
            candidates.sort();
            candidates.truncate(5);
            return ToolOutput::error(format!(
                "project filter '{}' is ambiguous; use an exact project key ({})",
                request.project,
                candidates.join(", ")
            ));
        }

        let period = request.period.as_deref().unwrap_or("7d");
        let cutoff = match period_cutoff(period) {
            Ok(cutoff) => cutoff,
            Err(error) => return ToolOutput::error(error),
        };
        let limit = request.limit.unwrap_or(20);
        let include_summaries = request.include_summaries.unwrap_or(true);

        let Some(project) = matches.pop() else {
            let response = ProjectHistoryResponse {
                project_path: request.project.clone(),
                project_key: None,
                providers: Vec::new(),
                period: period.to_string(),
                sessions_found: 0,
                sessions: Vec::new(),
                aggregate: ProjectAggregate {
                    total_sessions: 0,
                    total_tokens: 0,
                    total_cost: Some(0.0),
                    has_unpriced_sessions: Some(false),
                    total_prompts: 0,
                    active_branches: Vec::new(),
                },
                skipped_providers: collected
                    .skipped
                    .iter()
                    .map(|(id, _)| format!("{id}: unavailable"))
                    .collect(),
                project_warnings: Vec::new(),
                activity_basis: Some("new-activity-only".into()),
            };
            return ToolOutput::json(&response)
                .unwrap_or_else(|error| ToolOutput::error(error.to_string()));
        };

        let context_by_key: HashMap<_, _> = project
            .sessions
            .iter()
            .map(|session| (&session.descriptor.key, &session.context))
            .collect();
        let fork_parent: HashMap<_, _> = lineage
            .iter()
            .filter(|edge| matches!(edge.kind, LineageEdgeKind::Fork))
            .map(|edge| (edge.to.clone(), edge.from.to_string()))
            .collect();

        let mut session_entries = Vec::new();
        let mut parse_warnings = Vec::new();
        for unit in history_units(project, lineage) {
            if let Some(cutoff) = cutoff {
                let latest = unit
                    .members
                    .iter()
                    .filter_map(|key| {
                        context_by_key
                            .get(key)
                            .and_then(|context| context.modified_at)
                    })
                    .max();
                if latest.is_some_and(|latest| latest < cutoff) {
                    continue;
                }
            }

            let mut normalized_entries = Vec::new();
            let mut semantic_human_ids = HashSet::new();
            let mut parse_failed = None;
            for key in &unit.members {
                let provider = match registry.get(&key.provider) {
                    Ok(provider) => provider,
                    Err(error) => {
                        parse_failed = Some(error.to_string());
                        break;
                    }
                };
                let parsed =
                    match cached_parsed_session(crate::cache::global_cache(), provider, key) {
                        Ok(parsed) => parsed,
                        Err(error) => {
                            parse_failed = Some(error.to_string());
                            break;
                        }
                    };
                if provider.capabilities().semantic_annotations {
                    for identified in &parsed.entries {
                        let Some(semantics) = parsed.semantics.get(&identified.id) else {
                            continue;
                        };
                        if semantics.activity == ActivityKind::New
                            && semantics
                                .prompt
                                .is_some_and(|prompt| prompt.authorship == PromptAuthorship::Human)
                        {
                            if let Some(uuid) = identified.entry.uuid() {
                                semantic_human_ids.insert(uuid.to_string());
                            }
                        }
                    }
                }
                normalized_entries.extend(new_activity_entries(&parsed));
            }
            if let Some(error) = parse_failed {
                // A broken logical unit is never partially counted. Explicit
                // selections are atomic; `all` skips it with a bounded,
                // path-free warning.
                if atomic {
                    return ToolOutput::error(format!(
                        "failed to parse selected session {}: {error}",
                        unit.root
                    ));
                }
                parse_warnings.push(format!("{}: parse failed", unit.root));
                continue;
            }
            let conversation = match Conversation::from_entries(normalized_entries) {
                Ok(conversation) => conversation,
                Err(error) if atomic => {
                    return ToolOutput::error(format!(
                        "failed to reconstruct selected session {}: {error}",
                        unit.root
                    ));
                }
                Err(_) => {
                    parse_warnings.push(format!("{}: reconstruction failed", unit.root));
                    continue;
                }
            };
            let main_entries = conversation.main_thread_entries();
            let root_provider = match registry.get(&unit.root.provider) {
                Ok(provider) => provider,
                Err(error) if atomic => {
                    return ToolOutput::error(format!(
                        "failed to resolve selected provider for {}: {error}",
                        unit.root
                    ));
                }
                Err(_) => {
                    parse_warnings.push(format!("{}: provider unavailable", unit.root));
                    continue;
                }
            };
            let semantic = root_provider.capabilities().semantic_annotations;
            let mut prompts = Vec::new();
            let mut prompt_count = 0_usize;
            for entry in &main_entries {
                let human = if semantic {
                    entry
                        .uuid()
                        .is_some_and(|uuid| semantic_human_ids.contains(uuid))
                } else {
                    is_human_prompt(entry)
                };
                if human {
                    prompt_count += 1;
                    if include_summaries && prompts.len() < 3 {
                        if let Some(text) = extract_user_prompt_text(entry) {
                            if text.len() > 20 {
                                prompts.push(truncate_text(&text, 150));
                            }
                        }
                    }
                }
            }

            let analytics = SessionAnalytics::from_conversation(&conversation);
            let usage = provider_usage_summary(&conversation, root_provider.capabilities().pricing);
            let branch = unit.members.iter().find_map(|key| {
                context_by_key
                    .get(key)
                    .and_then(|context| context.git_branch.clone())
            });
            let compaction_count = main_entries
                .iter()
                .filter(|entry| {
                    matches!(
                        entry,
                        LogEntry::System(system)
                            if matches!(system.subtype,
                                Some(crate::model::SystemSubtype::CompactBoundary
                                    | crate::model::SystemSubtype::MicrocompactBoundary))
                    )
                })
                .count();
            let files = extract_files_from_tools(&main_entries);
            let mut tool_counts = HashMap::new();
            for entry in &main_entries {
                for name in extract_tool_names(entry) {
                    *tool_counts.entry(name).or_default() += 1;
                }
            }
            let first_prompt = prompts.first().cloned();
            let member_count = unit.members.len();
            session_entries.push(ProjectSessionEntry {
                session_id: unit.root.native_id.clone(),
                provider: Some(unit.root.provider.to_string()),
                qualified_id: Some(unit.root.to_string()),
                slug: None,
                chain_id: (member_count > 1).then(|| unit.root.to_string()),
                chain_length: (member_count > 1).then_some(member_count),
                is_subagent: unit.root.namespace != SessionNamespace::global(),
                parent_session_id: fork_parent.get(&unit.root).cloned(),
                start_time: analytics.start_time.map(|time| time.to_rfc3339()),
                end_time: analytics.end_time.map(|time| time.to_rfc3339()),
                span: analytics.duration_string(),
                compaction_count,
                git_branch: branch,
                user_prompt_count: prompt_count,
                first_prompt,
                key_prompts: prompts,
                tools_summary: tool_counts,
                files_touched: files.into_iter().take(10).collect(),
                estimated_cost: usage.pricing.estimated_cost,
                total_tokens: usage.canonical.total_processed_tokens,
            });
        }

        session_entries.retain(|entry| entry.user_prompt_count > 0 || entry.total_tokens > 0);
        session_entries.sort_by(|a, b| {
            b.start_time
                .cmp(&a.start_time)
                .then_with(|| a.qualified_id.cmp(&b.qualified_id))
        });
        if limit < session_entries.len() {
            session_entries.truncate(limit);
        }
        let total_tokens = session_entries
            .iter()
            .fold(0_u64, |sum, entry| sum.saturating_add(entry.total_tokens));
        let total_prompts = session_entries
            .iter()
            .map(|entry| entry.user_prompt_count)
            .sum();
        let has_unpriced = session_entries
            .iter()
            .any(|entry| entry.estimated_cost.is_none());
        let total_cost = (!has_unpriced).then(|| {
            session_entries
                .iter()
                .filter_map(|entry| entry.estimated_cost)
                .sum()
        });
        let mut branches: Vec<_> = session_entries
            .iter()
            .filter_map(|entry| entry.git_branch.clone())
            .collect();
        branches.sort();
        branches.dedup();
        let sessions_found = session_entries.len();
        let response = ProjectHistoryResponse {
            project_path: project
                .display_path
                .clone()
                .unwrap_or_else(|| project.identity.to_string()),
            project_key: Some(project.identity.to_string()),
            providers: project.providers.iter().map(ToString::to_string).collect(),
            period: period.to_string(),
            sessions_found,
            sessions: session_entries,
            aggregate: ProjectAggregate {
                total_sessions: sessions_found,
                total_tokens,
                total_cost,
                has_unpriced_sessions: Some(has_unpriced),
                total_prompts,
                active_branches: branches,
            },
            skipped_providers: collected
                .skipped
                .iter()
                .map(|(id, _)| format!("{id}: unavailable"))
                .collect(),
            project_warnings: collected
                .context_warnings
                .iter()
                .map(|warning| format!("{}: project metadata unavailable", warning.key))
                .chain(parse_warnings)
                .collect(),
            activity_basis: Some("new-activity-only".into()),
        };
        ToolOutput::json(&response)
            .unwrap_or_else(|error| ToolOutput::error(format!("JSON serialization error: {error}")))
    }

    pub(crate) fn get_claude_dir(&self) -> Result<ClaudeDirectory, String> {
        let result = if let Some(ref path) = self.claude_dir {
            ClaudeDirectory::from_path(path.clone())
        } else {
            ClaudeDirectory::discover()
        };
        result.map_err(|e| format!("Failed to access Claude directory: {e}"))
    }
}

/// Resolve the persistence namespace for goals/notes/decisions. These stores
/// still live in Claude Code project memory; naming another provider must fail
/// before project resolution so callers cannot mistake them for union data.
fn claude_registry_scope(provider: Option<&str>) -> Result<bool, String> {
    match provider {
        None => Ok(false),
        Some("claude-code") => Ok(true),
        Some(other) => Err(format!(
            "registry storage is scoped to 'claude-code', not '{other}'; \
             cross-provider goals/notes/decisions are not available"
        )),
    }
}

#[mcp_server(name = "claude-snatch", version = "0.1.0")]
impl SnatchServer {
    // ========================================================================
    // Existing Tools
    // ========================================================================

    /// List Claude Code sessions with optional filtering.
    #[tool(
        description = "List agent sessions (Claude Code by default; other providers via the provider parameter) with optional filtering"
    )]
    async fn list_sessions(&self, request: ListSessionsRequest) -> ToolOutput {
        if let Some(flags) = request.provider.as_ref().filter(|f| !f.is_empty()) {
            return self.provider_sessions_output(
                flags,
                request.project.as_deref(),
                request.include_subagents.unwrap_or(false),
                request.limit.unwrap_or(50),
            );
        }

        let claude_dir = match self.get_claude_dir() {
            Ok(dir) => dir,
            Err(e) => return ToolOutput::error(e),
        };

        let mut sessions = match claude_dir.all_sessions() {
            Ok(s) => s,
            Err(e) => return ToolOutput::error(format!("Failed to list sessions: {e}")),
        };

        if let Some(ref project) = request.project {
            sessions.retain(|s| s.project_path().contains(project));
        }

        if !request.include_subagents.unwrap_or(false) {
            sessions.retain(|s| !s.is_subagent());
        }

        let limit = request.limit.unwrap_or(50);
        sessions.truncate(limit);

        // Detect chains for the sessions we're listing
        let main_sessions: Vec<_> = sessions.iter().filter(|s| !s.is_subagent()).collect();
        let chains = detect_chains(main_sessions.iter().map(|s| (s.session_id(), s.path())));
        // Build reverse lookup: file_id -> (chain_root, chain_len)
        let mut chain_lookup: HashMap<String, (String, usize)> = HashMap::new();
        for (root_id, chain) in &chains {
            for member in &chain.members {
                chain_lookup.insert(member.file_id.clone(), (root_id.clone(), chain.len()));
            }
        }

        let summaries: Vec<SessionSummary> = sessions
            .iter()
            .map(|s| {
                let (span, compaction_count, slug) = s
                    .quick_metadata_cached()
                    .map(|m| (m.duration_human(), m.compaction_count, m.slug.clone()))
                    .unwrap_or((None, 0, None));
                let chain_info = chain_lookup.get(s.session_id());
                let key = crate::provider::claude_code::logical_key(s);
                SessionSummary {
                    session_id: s.session_id().to_string(),
                    provider: key.provider.to_string(),
                    qualified_id: key.to_string(),
                    slug,
                    project_path: s.display_project_path(),
                    is_subagent: s.is_subagent(),
                    parent_session_id: s.parent_session_id().map(String::from),
                    modified_time: Some(s.modified_datetime().to_rfc3339()),
                    is_active: s.is_active().unwrap_or(false),
                    span,
                    compaction_count,
                    chain_id: chain_info.map(|(root, _)| root.clone()),
                    chain_length: chain_info.map(|(_, len)| *len),
                }
            })
            .collect();

        match ToolOutput::json(&summaries) {
            Ok(output) => output,
            Err(e) => ToolOutput::error(format!("JSON serialization error: {e}")),
        }
    }

    /// Get detailed information about a specific Claude Code session.
    #[tool(
        description = "Get detailed information about a session (Claude Code by default; provider-qualified ids and the provider parameter reach other providers)"
    )]
    async fn get_session_info(&self, request: GetSessionInfoRequest) -> ToolOutput {
        let provider_flags = request.provider.clone().unwrap_or_default();
        if !provider_flags.is_empty() || request.session_id.contains(':') {
            let registry = self.provider_registry();
            if !provider_flags.is_empty() || registry.looks_qualified(&request.session_id) {
                return self.provider_session_info_output(
                    &registry,
                    &provider_flags,
                    &request.session_id,
                );
            }
        }

        let claude_dir = match self.get_claude_dir() {
            Ok(dir) => dir,
            Err(e) => return ToolOutput::error(e),
        };

        let session = match claude_dir.find_session(&request.session_id) {
            Ok(Some(s)) => s,
            Ok(None) => {
                return ToolOutput::error(format!("Session not found: {}", request.session_id))
            }
            Err(e) => return ToolOutput::error(format!("Failed to find session: {e}")),
        };

        let entries = match session.parse_with_options(self.max_file_size) {
            Ok(e) => e,
            Err(e) => return ToolOutput::error(format!("Failed to parse session: {e}")),
        };

        let conversation = match Conversation::from_entries(entries) {
            Ok(c) => c,
            Err(e) => return ToolOutput::error(format!("Failed to reconstruct conversation: {e}")),
        };

        let analytics = SessionAnalytics::from_conversation(&conversation);
        let summary = analytics.summary_report();

        let (compaction_count, slug) = session
            .quick_metadata_cached()
            .map(|m| (m.compaction_count, m.slug.clone()))
            .unwrap_or((0, None));

        // Detect chain membership for this session
        let (chain_id, chain_members) = if !session.is_subagent() {
            if let Ok(Some(project)) = claude_dir.find_project(session.project_path()) {
                if let Ok(chains) = project.session_chains() {
                    chains
                        .values()
                        .find(|c| c.contains(session.session_id()))
                        .map(|c| {
                            (
                                Some(c.root_id.clone()),
                                Some(c.file_ids().into_iter().map(String::from).collect()),
                            )
                        })
                        .unwrap_or((None, None))
                } else {
                    (None, None)
                }
            } else {
                (None, None)
            }
        } else {
            (None, None)
        };

        // Enumerate subagents spawned by this session (empty for subagent
        // sessions). message_count parses each transcript; cached across calls.
        let mut subagents: Vec<SubagentSummary> = session
            .subagent_links()
            .into_iter()
            .map(|link| {
                let message_count = Session::from_path(&link.path, session.project_path())
                    .ok()
                    .and_then(|s| s.quick_metadata_cached().ok())
                    .map(|m| m.user_count + m.assistant_count);
                SubagentSummary {
                    agent_session_id: link.agent_session_id,
                    agent_type: link.agent_type,
                    description: link.description,
                    tool_use_id: link.tool_use_id,
                    message_count,
                }
            })
            .collect();
        subagents.sort_by(|a, b| a.agent_session_id.cmp(&b.agent_session_id));

        let key = crate::provider::claude_code::logical_key(&session);
        let info = SessionInfoResponse {
            session_id: session.session_id().to_string(),
            provider: key.provider.to_string(),
            qualified_id: key.to_string(),
            slug,
            chain_id,
            chain_members,
            project_path: session.display_project_path(),
            is_subagent: session.is_subagent(),
            parent_session_id: session.parent_session_id().map(String::from),
            is_active: session.is_active().unwrap_or(false),
            modified_time: Some(session.modified_datetime().to_rfc3339()),
            span: analytics.duration_string(),
            compaction_count,
            primary_model: analytics.primary_model().map(String::from),
            total_tokens: summary.total_tokens,
            input_tokens: summary.input_tokens,
            output_tokens: summary.output_tokens,
            cache_read_tokens: summary.cache_read_tokens,
            cache_creation_tokens: summary.cache_creation_tokens,
            total_processed_tokens: summary.total_processed_tokens,
            messages: main_thread_message_total(&conversation),
            user_messages: summary.user_messages,
            assistant_messages: summary.assistant_messages,
            tool_invocations: summary.tool_invocations,
            cache_hit_rate: summary.cache_hit_rate,
            estimated_cost: summary.estimated_cost,
            unpriced_models: summary.unpriced_models.clone(),
            subagents,
        };

        match ToolOutput::json(&info) {
            Ok(output) => output,
            Err(e) => ToolOutput::error(format!("JSON serialization error: {e}")),
        }
    }

    /// Get usage statistics for sessions, projects, or globally.
    #[tool(description = "Get usage statistics for sessions, projects, or globally")]
    async fn get_stats(&self, request: GetStatsRequest) -> ToolOutput {
        let claude_dir = match self.get_claude_dir() {
            Ok(dir) => dir,
            Err(e) => return ToolOutput::error(e),
        };

        let response = if let Some(session_id) = request.session_id {
            let session = match claude_dir.find_session(&session_id) {
                Ok(Some(s)) => s,
                Ok(None) => return ToolOutput::error(format!("Session not found: {session_id}")),
                Err(e) => return ToolOutput::error(format!("Failed to find session: {e}")),
            };

            let entries = match session.parse_with_options(self.max_file_size) {
                Ok(e) => e,
                Err(e) => return ToolOutput::error(format!("Failed to parse session: {e}")),
            };

            let conversation = match Conversation::from_entries(entries) {
                Ok(c) => c,
                Err(e) => {
                    return ToolOutput::error(format!("Failed to reconstruct conversation: {e}"))
                }
            };

            let analytics = SessionAnalytics::from_conversation(&conversation);
            let summary = analytics.summary_report();

            StatsResponse {
                scope: "session".to_string(),
                sessions: Some(1),
                total_tokens: summary.total_tokens,
                input_tokens: summary.input_tokens,
                output_tokens: summary.output_tokens,
                cache_read_tokens: summary.cache_read_tokens,
                cache_creation_tokens: summary.cache_creation_tokens,
                total_processed_tokens: summary.total_processed_tokens,
                messages: summary.total_messages,
                tool_invocations: summary.tool_invocations,
                estimated_cost: summary.estimated_cost,
            }
        } else {
            let sessions = match claude_dir.all_sessions() {
                Ok(s) => s,
                Err(e) => return ToolOutput::error(format!("Failed to list sessions: {e}")),
            };

            let (scope, target_sessions): (String, Vec<_>) = if let Some(project) = request.project
            {
                let filtered: Vec<_> = sessions
                    .iter()
                    .filter(|s| s.project_path().contains(&project))
                    .collect();
                (project, filtered)
            } else {
                ("global".to_string(), sessions.iter().collect())
            };

            let summaries: Vec<_> = target_sessions
                .iter()
                .filter_map(|session| {
                    let entries = session.parse_with_options(self.max_file_size).ok()?;
                    let conversation = Conversation::from_entries(entries).ok()?;
                    let analytics = SessionAnalytics::from_conversation(&conversation);
                    Some(analytics.summary_report())
                })
                .collect();

            let agg = AnalyticsSummary::aggregate(&summaries);

            StatsResponse {
                scope,
                sessions: Some(target_sessions.len()),
                total_tokens: agg.total_tokens,
                input_tokens: agg.input_tokens,
                output_tokens: agg.output_tokens,
                cache_read_tokens: agg.cache_read_tokens,
                cache_creation_tokens: agg.cache_creation_tokens,
                total_processed_tokens: agg.total_processed_tokens,
                messages: agg.total_messages,
                tool_invocations: agg.tool_invocations,
                estimated_cost: agg.estimated_cost,
            }
        };

        match ToolOutput::json(&response) {
            Ok(output) => output,
            Err(e) => ToolOutput::error(format!("JSON serialization error: {e}")),
        }
    }

    // ========================================================================
    // New Tool: get_session_messages
    // ========================================================================

    /// Read conversation messages from a session at different detail levels.
    /// Use detail="overview" for prompt boundaries only, "standard" for user+assistant
    /// text with tool names, or "full" for tool call details.
    #[tool(
        description = "Read conversation messages from an agent session (Claude Code by default; other providers via provider or a qualified id). Use detail='overview' for prompt boundaries, 'conversation' for user+assistant text (skipping tool-only turns), 'standard' for user+assistant text, and 'full' for tool details. Chunk selectors use provider semantics: midturn steering remains inside the active chunk. Claude-only chain/subagent controls are refused on other provider routes. Set include_thinking=true for persisted reasoning, errors_only=true to drill into failed tool calls, and max_text_len to control truncation. Supports pagination with offset/limit."
    )]
    async fn get_session_messages(&self, request: GetSessionMessagesRequest) -> ToolOutput {
        let provider_flags = request.provider.as_deref().unwrap_or(&[]);
        let registry = self.provider_registry();
        let provider_route =
            !provider_flags.is_empty() || registry.looks_qualified(&request.session_id);
        if provider_route
            && (request.chain_aware.is_some() || request.include_subagent_transcripts == Some(true))
        {
            return ToolOutput::error(
                "provider-routed messages do not support chain_aware or \
                 include_subagent_transcripts",
            );
        }
        let chain_aware = request.chain_aware.unwrap_or(true);
        let resolved = match if provider_route {
            self.resolve_provider_session(provider_flags, &request.session_id)
        } else {
            resolve_session_with_chain(self, &request.session_id, chain_aware)
        } {
            Ok(r) => r,
            Err(e) => return e,
        };
        let semantic = resolved.semantic_annotations;
        let prompt = |entry: &LogEntry| {
            entry
                .uuid()
                .and_then(|uuid| resolved.conversation.semantics_for_uuid(uuid))
                .and_then(|semantics| semantics.prompt)
        };
        let human = |entry: &LogEntry| {
            if semantic {
                matches!(entry, LogEntry::User(_))
                    && prompt(entry).is_some_and(|prompt| {
                        matches!(prompt.authorship, crate::provider::PromptAuthorship::Human)
                    })
            } else {
                is_human_prompt(entry)
            }
        };
        let boundary = |entry: &LogEntry| {
            if semantic {
                matches!(entry, LogEntry::User(_))
                    && prompt(entry).is_some_and(|prompt| {
                        matches!(
                            (prompt.authorship, prompt.delivery),
                            (
                                crate::provider::PromptAuthorship::Human,
                                crate::provider::PromptDelivery::TurnBoundary
                            )
                        )
                    })
            } else {
                is_prompt_boundary(entry)
            }
        };

        let detail = request.detail.as_deref().unwrap_or("standard");
        let msg_type_filter = request.message_type.as_deref().unwrap_or("all");
        // 0 means unlimited, matching the CLI (`messages -l 0`). Chunk
        // requests default to unlimited: a chunk is the retrieval unit, and
        // silently cutting it at 50 betrays that; an explicit limit still
        // paginates.
        let default_limit = if request.chunk.is_some() { 0 } else { 50 };
        let limit = match request.limit.unwrap_or(default_limit) {
            0 => usize::MAX,
            n => n,
        };
        let offset = request.offset.unwrap_or(0);
        let reverse = request.reverse.unwrap_or(false);
        let include_thinking = request.include_thinking.unwrap_or(false);
        let thinking_max_len = match detail {
            "overview" => 500,
            "conversation" | "standard" => 1000,
            _ => 2000,
        };
        let include_subagent_transcripts = request.include_subagent_transcripts.unwrap_or(false);

        // Match Agent/Task calls to the subagents they spawned (only "full" detail
        // renders tool details). Uses the unfiltered thread for spawn-order joining.
        let resolved_subagents: ResolvedSubagents = if detail == "full" && !semantic {
            match self.get_claude_dir() {
                Ok(dir) => match dir.find_session(&resolved.session_id) {
                    Ok(Some(session)) => resolve_subagent_renders(
                        &session,
                        &resolved.conversation.main_thread_entries(),
                        include_subagent_transcripts,
                        include_thinking,
                        self.max_file_size,
                    ),
                    _ => ResolvedSubagents::default(),
                },
                Err(_) => ResolvedSubagents::default(),
            }
        } else {
            ResolvedSubagents::default()
        };
        let subagent_renders = resolved_subagents.matched;
        let unmatched_subagents = resolved_subagents.unmatched;

        let mut entries: Vec<&LogEntry> = resolved.conversation.main_thread_entries();

        // Restrict to prompt-boundary chunk(s) when requested. Membership is
        // tree-based, so late async results belong to the chunk that spawned
        // them (appended after its main-thread members).
        let chunk_info: Option<ChunkInfo> = if let Some(ref spec) = request.chunk {
            use crate::analysis::chunking::{
                chunk_conversation, chunk_conversation_semantic, entries_for_chunk_range,
                parse_chunk_spec,
            };
            let chunking = if semantic {
                chunk_conversation_semantic(&resolved.conversation)
            } else {
                chunk_conversation(&resolved.conversation)
            };
            let (start, end) = match parse_chunk_spec(spec, chunking.len()) {
                Ok(range) => range,
                Err(message) => return ToolOutput::error(format!("Invalid chunk: {message}")),
            };
            entries = entries_for_chunk_range(&resolved.conversation, &chunking, start, end);
            Some(ChunkInfo {
                total_chunks: chunking.len(),
                start,
                end,
                chunks: chunking.chunks[start..=end]
                    .iter()
                    .map(|c| ChunkSummary {
                        index: c.index,
                        prompt: truncate_text(&c.prompt_text, 200),
                        prompt_source: c.prompt_source.as_str().to_string(),
                        start_ts: c.start_ts.map(|t| t.to_rfc3339()),
                        end_ts: c.end_ts.map(|t| t.to_rfc3339()),
                        entries: c.entry_count(),
                        attached: c.attached_uuids.len(),
                        tool_calls: c.tool_call_count,
                        errors: c.error_count,
                        branches: c
                            .branches
                            .iter()
                            .map(|b| ChunkBranchSummary {
                                root_uuid: b.root_uuid.clone(),
                                prompt: b.prompt_text.as_deref().map(|p| truncate_text(p, 100)),
                                entries: b.uuids.len(),
                            })
                            .collect(),
                    })
                    .collect(),
            })
        } else {
            None
        };

        // Error drill-down: keep failed tool results AND the assistant
        // entries that issued the failing calls (the result carries the
        // error text, the call carries the command — an audit needs both).
        if request.errors_only.unwrap_or(false) {
            let failed = failed_tool_use_ids(&entries);
            entries.retain(|e| match e {
                LogEntry::User(_) => has_tool_errors(std::slice::from_ref(e)),
                LogEntry::Assistant(a) => {
                    a.message.tool_uses().iter().any(|t| failed.contains(&t.id))
                }
                _ => false,
            });
        }

        // Surface the recent-Claude-Code redaction pattern (thinking blocks
        // present but all empty) so include_thinking never fails silently.
        let thinking_note = if include_thinking {
            thinking_redaction_note(&entries)
        } else {
            None
        };

        // Filter by message type
        match msg_type_filter {
            "user" => entries.retain(|e| human(e)),
            "assistant" => entries.retain(|e| matches!(e, LogEntry::Assistant(_))),
            "system" => entries.retain(|e| matches!(e, LogEntry::System(_))),
            _ => {} // "all" — keep everything
        }

        // Filter by timestamp window
        if request.after_timestamp.is_some() || request.before_timestamp.is_some() {
            let after = if let Some(ref ts) = request.after_timestamp {
                match parse_timestamp_param(ts) {
                    Ok(dt) => Some(dt),
                    Err(e) => return ToolOutput::error(format!("Invalid after_timestamp: {e}")),
                }
            } else {
                None
            };
            let before = if let Some(ref ts) = request.before_timestamp {
                match parse_timestamp_param(ts) {
                    Ok(dt) => Some(dt),
                    Err(e) => return ToolOutput::error(format!("Invalid before_timestamp: {e}")),
                }
            } else {
                None
            };
            entries.retain(|e| {
                if let Some(ts) = e.timestamp() {
                    if let Some(ref a) = after {
                        if ts < *a {
                            return false;
                        }
                    }
                    if let Some(ref b) = before {
                        if ts > *b {
                            return false;
                        }
                    }
                    true
                } else {
                    // Keep entries without timestamps (conservative)
                    true
                }
            });
        }

        // Pre-filter entries based on detail level. Overview uses the
        // chunker's boundary predicate (typed prompts + queued steering
        // prompts) so its indices always match chunk indices.
        match detail {
            "overview" => {
                entries.retain(|e| boundary(e));
            }
            "conversation" => {
                // Human prompts + assistant messages with text content
                // Skips tool-only assistant turns, system messages, and noise
                entries.retain(|e| match e {
                    LogEntry::User(_) => human(e),
                    LogEntry::Assistant(_) => extract_assistant_summary(e, 1).is_some(),
                    // Queued steering prompts are dialogue, not tool noise.
                    LogEntry::Attachment(_) => queued_human_prompt(e).is_some(),
                    _ => false,
                });
            }
            _ => {} // standard/full: keep everything
        }

        // Detail-independent canonical total (see main_thread_message_total),
        // so it matches get_session_info.messages regardless of detail level.
        // `returned` below conveys how many this page actually emitted.
        let total_messages = if semantic {
            resolved
                .conversation
                .main_thread_entries()
                .into_iter()
                .filter(|entry| matches!(entry, LogEntry::Assistant(_)) || human(entry))
                .count()
        } else {
            main_thread_message_total(&resolved.conversation)
        };

        // Build (original_index, entry) pairs so indices survive reordering
        let mut indexed: Vec<(usize, &LogEntry)> = entries.into_iter().enumerate().collect();

        if reverse {
            indexed.reverse();
        }

        // Size of the population offset/limit actually range over at this
        // detail level (post message_type/detail filtering). Distinct from
        // total_messages (the canonical, detail-independent count): page_total
        // is what a caller must compare against to know if more pages exist,
        // so total vs returned never has to be (mis)read as a pagination signal.
        let page_total = indexed.len();

        // Apply pagination
        let paginated: Vec<(usize, &LogEntry)> =
            indexed.into_iter().skip(offset).take(limit).collect();
        // Computed from entries consumed by this page, not rendered messages
        // (some entries can render empty at a given detail level).
        let has_more = offset.saturating_add(paginated.len()) < page_total;

        // Map tool_use_id -> (had_error, result_preview) so the "full" detail
        // builder can surface each tool's output, joined to the assistant's
        // tool_use call. Scoped to the tool_use ids actually in the rendered
        // page so we don't clone every result's content (e.g. large file reads)
        // across the whole session. Only built at "full" detail.
        let tool_result_previews: HashMap<String, (bool, Option<String>)> = if detail == "full" {
            let needed: HashSet<&str> = paginated
                .iter()
                .filter_map(|(_, e)| match e {
                    LogEntry::Assistant(a) => Some(a),
                    _ => None,
                })
                .flat_map(|a| a.message.tool_uses())
                .map(|t| t.id.as_str())
                .collect();
            let mut map = HashMap::new();
            if !needed.is_empty() {
                for entry in &resolved.conversation.main_thread_entries() {
                    if let LogEntry::User(user) = entry {
                        for result in user.message.tool_results() {
                            if !needed.contains(result.tool_use_id.as_str()) {
                                continue;
                            }
                            let is_err = result.is_error == Some(true);
                            let preview = if is_err {
                                extract_error_preview(result, 300)
                            } else {
                                extract_result_preview(result, 500)
                            };
                            map.insert(result.tool_use_id.clone(), (is_err, preview));
                        }
                    }
                }
            }
            map
        } else {
            HashMap::new()
        };

        let truncate_len = request.max_text_len.unwrap_or(match detail {
            "overview" => 200,
            "conversation" => 500,
            "standard" => 500,
            _ => 1000,
        });

        let messages: Vec<MessageEntry> = paginated
            .iter()
            .map(|(orig_idx, entry)| {
                let msg_type = entry.message_type().to_string();
                let timestamp = entry.timestamp().map(|t| t.to_rfc3339());
                let git_branch = entry.git_branch().map(String::from);

                match detail {
                    "overview" => {
                        let content =
                            boundary_prompt_text(entry).map(|t| truncate_text(&t, truncate_len));
                        MessageEntry {
                            index: *orig_idx,
                            msg_type,
                            timestamp,
                            content,
                            git_branch,
                            model: None,
                            tool_calls: None,
                            tool_details: None,
                            has_thinking: None,
                            thinking_preview: None,
                        }
                    }
                    "conversation" => {
                        // User prompts + assistant text, no tool details
                        let content = match entry {
                            LogEntry::User(_) => extract_user_prompt_text(entry)
                                .map(|t| truncate_text(&t, truncate_len)),
                            LogEntry::Assistant(_) => {
                                extract_assistant_summary(entry, truncate_len)
                            }
                            LogEntry::Attachment(_) => queued_human_prompt(entry)
                                .map(|t| format!("(queued) {}", truncate_text(t, truncate_len))),
                            _ => None,
                        };
                        let thinking = if include_thinking {
                            extract_thinking_text(entry, thinking_max_len)
                        } else {
                            None
                        };
                        MessageEntry {
                            index: *orig_idx,
                            msg_type,
                            timestamp,
                            content,
                            git_branch,
                            model: get_model(entry),
                            tool_calls: None,
                            tool_details: None,
                            has_thinking: if has_thinking(entry) {
                                Some(true)
                            } else {
                                None
                            },
                            thinking_preview: thinking,
                        }
                    }
                    "standard" => {
                        let content = match entry {
                            LogEntry::User(_) => {
                                let text = extract_user_prompt_text(entry)
                                    .map(|t| truncate_text(&t, truncate_len));
                                let images = extract_image_placeholders(entry);
                                match (text, images.is_empty()) {
                                    (Some(t), true) => Some(t),
                                    (Some(t), false) => Some(format!("{t}\n{}", images.join(" "))),
                                    (None, false) => Some(images.join(" ")),
                                    (None, true) => None,
                                }
                            }
                            LogEntry::Assistant(_) => {
                                extract_assistant_summary(entry, truncate_len)
                            }
                            LogEntry::System(sys) => sys.content.clone(),
                            LogEntry::Attachment(_) => {
                                render_attachment_content(entry, truncate_len)
                            }
                            _ => None,
                        };
                        let tool_names = extract_tool_names(entry);
                        let thinking = if include_thinking {
                            extract_thinking_text(entry, thinking_max_len)
                        } else {
                            None
                        };
                        MessageEntry {
                            index: *orig_idx,
                            msg_type,
                            timestamp,
                            content,
                            git_branch,
                            model: get_model(entry),
                            tool_calls: if tool_names.is_empty() {
                                None
                            } else {
                                Some(tool_names)
                            },
                            tool_details: None,
                            has_thinking: if has_thinking(entry) {
                                Some(true)
                            } else {
                                None
                            },
                            thinking_preview: thinking,
                        }
                    }
                    // "full" or any unrecognised detail level
                    _ => {
                        let content = match entry {
                            LogEntry::User(_) => {
                                let text = extract_user_prompt_text(entry)
                                    .map(|t| truncate_text(&t, truncate_len));
                                let images = extract_image_placeholders(entry);
                                match (text, images.is_empty()) {
                                    (Some(t), true) => Some(t),
                                    (Some(t), false) => Some(format!("{t}\n{}", images.join(" "))),
                                    (None, false) => Some(images.join(" ")),
                                    (None, true) => None,
                                }
                            }
                            LogEntry::Assistant(_) => {
                                extract_assistant_summary(entry, truncate_len)
                            }
                            LogEntry::System(sys) => sys.content.clone(),
                            LogEntry::Attachment(_) => {
                                render_attachment_content(entry, truncate_len)
                            }
                            _ => None,
                        };
                        let tool_names = extract_tool_names(entry);
                        let tool_details: Vec<ToolDetail> = if let LogEntry::Assistant(a) = entry {
                            a.message
                                .tool_uses()
                                .iter()
                                .map(|t| {
                                    let summary = extract_tool_input_summary(&t.name, &t.input);
                                    let rendered = subagent_renders.get(&t.id);
                                    let subagent_result_preview =
                                        rendered.and_then(|r| r.result_preview.clone());
                                    let (had_error, mut result_preview) =
                                        match tool_result_previews.get(&t.id) {
                                            Some((err, prev)) => (*err, prev.clone()),
                                            None => (false, None),
                                        };
                                    // Skip the generic result preview when a richer
                                    // subagent preview is already attached, to avoid
                                    // duplicating the same text.
                                    if subagent_result_preview.is_some() {
                                        result_preview = None;
                                    }
                                    // Collect input fields not surfaced as a
                                    // named field above, so bulky inputs (Edit
                                    // old/new_string, Write content, TodoWrite
                                    // todos) aren't silently dropped.
                                    const NAMED_KEYS: [&str; 6] = [
                                        "file_path",
                                        "command",
                                        "pattern",
                                        "subagent_type",
                                        "description",
                                        "prompt",
                                    ];
                                    let extra: std::collections::BTreeMap<String, String> = summary
                                        .iter()
                                        .filter(|(k, _)| !NAMED_KEYS.contains(&k.as_str()))
                                        .map(|(k, v)| (k.clone(), v.clone()))
                                        .collect();
                                    ToolDetail {
                                        tool_name: t.name.clone(),
                                        file_path: summary.get("file_path").cloned(),
                                        command: summary.get("command").cloned(),
                                        pattern: summary.get("pattern").cloned(),
                                        subagent_type: summary.get("subagent_type").cloned(),
                                        description: summary.get("description").cloned(),
                                        prompt: summary.get("prompt").cloned(),
                                        input_summary: if extra.is_empty() {
                                            None
                                        } else {
                                            Some(extra)
                                        },
                                        subagent_session_id: rendered.map(|r| r.session_id.clone()),
                                        subagent_result_preview,
                                        subagent_transcript: rendered
                                            .and_then(|r| r.transcript.clone()),
                                        had_error: if had_error { Some(true) } else { None },
                                        result_preview,
                                    }
                                })
                                .collect()
                        } else {
                            vec![]
                        };
                        let thinking = if include_thinking {
                            extract_thinking_text(entry, thinking_max_len)
                        } else {
                            None
                        };
                        MessageEntry {
                            index: *orig_idx,
                            msg_type,
                            timestamp,
                            content,
                            git_branch,
                            model: get_model(entry),
                            tool_calls: if tool_names.is_empty() {
                                None
                            } else {
                                Some(tool_names)
                            },
                            tool_details: if tool_details.is_empty() {
                                None
                            } else {
                                Some(tool_details)
                            },
                            has_thinking: if has_thinking(entry) {
                                Some(true)
                            } else {
                                None
                            },
                            thinking_preview: thinking,
                        }
                    }
                }
            })
            .collect();

        let returned = messages.len();
        let duplicate_notice = resolved.conversation.duplicate_notice();
        let response = SessionMessagesResponse {
            session_id: resolved.session_id,
            provider: resolved.provider,
            qualified_id: resolved.qualified_id,
            project_path: resolved.project_path,
            total_messages,
            returned,
            offset,
            page_total,
            has_more,
            messages,
            unmatched_subagents,
            duplicate_notice,
            thinking_note,
            chunk_info,
        };

        match ToolOutput::json(&response) {
            Ok(output) => output,
            Err(e) => ToolOutput::error(format!("JSON serialization error: {e}")),
        }
    }

    // ========================================================================
    // New Tool: get_session_timeline
    // ========================================================================

    /// Get a turn-by-turn narrative timeline of a session showing what was asked,
    /// what Claude did, and what files were touched.
    #[tool(
        description = "Get a turn-by-turn narrative timeline of an agent session (Claude Code by default; other providers via provider or a qualified id). Each turn shows the user prompt, same-turn steering prompts, assistant summary, tools used, and files touched. Also surfaces compaction events."
    )]
    async fn get_session_timeline(&self, request: GetSessionTimelineRequest) -> ToolOutput {
        let provider_flags = request.provider.as_deref().unwrap_or(&[]);
        let registry = self.provider_registry();
        let provider_route =
            !provider_flags.is_empty() || registry.looks_qualified(&request.session_id);
        if provider_route && request.chain_aware.is_some() {
            return ToolOutput::error("provider-routed timeline does not support chain_aware");
        }
        let chain_aware = request.chain_aware.unwrap_or(true);
        let resolved = match if provider_route {
            self.resolve_provider_session(provider_flags, &request.session_id)
        } else {
            resolve_session_with_chain(self, &request.session_id, chain_aware)
        } {
            Ok(r) => r,
            Err(e) => return e,
        };

        let limit = request.limit.unwrap_or(30);

        // Detect compaction events from the complete conversation so provider
        // window metadata survives into the public response.
        let main_entries = resolved.conversation.main_thread_entries();
        let main_refs: Vec<&LogEntry> = main_entries.clone();
        let compaction_events: Vec<CompactionEvent> =
            crate::analysis::timeline::compaction_events(&resolved.conversation)
                .into_iter()
                .map(|event| CompactionEvent {
                    timestamp: event.timestamp,
                    summary: event.summary,
                    kind: event.kind,
                    replacement_history_items: event.replacement_history_items,
                    window: event
                        .window
                        .map(|window| crate::mcp_server::types::CompactionWindow {
                            number: window.number,
                            first_id: window.first_id,
                            previous_id: window.previous_id,
                            id: window.id,
                            legacy_numeric_id: window.legacy_numeric_id,
                        }),
                })
                .collect();
        let error_events: Vec<crate::mcp_server::types::ErrorEvent> =
            crate::analysis::extraction::find_error_events(&main_refs)
                .into_iter()
                .map(|(ts, message)| crate::mcp_server::types::ErrorEvent {
                    timestamp: ts,
                    message,
                })
                .collect();

        // Get session time bounds and git branch
        let start_time = resolved.analytics.start_time.map(|t| t.to_rfc3339());
        let end_time = resolved.analytics.end_time.map(|t| t.to_rfc3339());
        let span = resolved.analytics.duration_string();
        let git_branch = main_entries
            .iter()
            .find_map(|e| e.git_branch().map(String::from));

        // Build timeline using shared analysis module
        let timeline_opts = crate::analysis::timeline::TimelineOptions {
            limit,
            prompt_max_len: 200,
            summary_max_len: 200,
        };
        let (total_turns, analysis_timeline) = if resolved.semantic_annotations {
            let turns = crate::analysis::timeline::semantic_turns(&resolved.conversation);
            let total = turns.len();
            (
                total,
                crate::analysis::timeline::build_semantic_timeline(&turns, &timeline_opts),
            )
        } else {
            let turns = resolved.conversation.turns();
            let total = turns.len();
            (
                total,
                crate::analysis::timeline::build_timeline(&turns, &timeline_opts),
            )
        };

        // Map analysis types to MCP response types
        let timeline: Vec<TimelineTurn> = analysis_timeline
            .into_iter()
            .map(|t| TimelineTurn {
                index: t.index,
                timestamp: t.timestamp,
                user_prompt: t.user_prompt,
                steering_prompts: t.steering_prompts,
                assistant_summary: t.assistant_summary,
                tools_used: t.tools_used,
                files_touched: t.files_touched,
                had_errors: t.had_errors,
            })
            .collect();

        let duplicate_notice = resolved.conversation.duplicate_notice();
        let response = SessionTimelineResponse {
            session_id: resolved.session_id,
            provider: resolved.provider,
            qualified_id: resolved.qualified_id,
            project_path: resolved.project_path,
            start_time,
            end_time,
            span,
            total_turns,
            git_branch,
            timeline,
            compaction_events,
            error_events,
            duplicate_notice,
        };

        match ToolOutput::json(&response) {
            Ok(output) => output,
            Err(e) => ToolOutput::error(format!("JSON serialization error: {e}")),
        }
    }

    // ========================================================================
    // New Tool: get_project_history
    // ========================================================================

    /// Get a cross-session overview for a project, showing what was worked on
    /// across sessions with key prompts, tools used, and files touched.
    #[tool(
        description = "Get cross-session history for a project. Shows sessions with key prompts, tools, files, and costs. Filter by period (24h/7d/30d/all). Omit provider for the classic Claude-only path; set provider=['all'] (or explicit providers) for a cwd/git-unified history. Provider unions exclude fork-inherited history and collapse only typed continuations."
    )]
    async fn get_project_history(&self, request: GetProjectHistoryRequest) -> ToolOutput {
        if request.provider.is_some() {
            return self.provider_project_history(&request);
        }
        let claude_dir = match get_claude_dir(self) {
            Ok(dir) => dir,
            Err(e) => return e,
        };

        let period = request.period.as_deref().unwrap_or("7d");
        let limit = request.limit.unwrap_or(20);
        let include_summaries = request.include_summaries.unwrap_or(true);

        let cutoff = match period_cutoff(period) {
            Ok(c) => c,
            Err(e) => return ToolOutput::error(e),
        };

        // Iterate per-project so we can detect chains
        let projects = match claude_dir.projects() {
            Ok(p) => p,
            Err(e) => return ToolOutput::error(format!("Failed to list projects: {e}")),
        };

        let filtered_projects: Vec<_> = projects
            .into_iter()
            .filter(|p| p.best_path().contains(&request.project))
            .collect();

        let mut project_path = String::new();
        let mut agg_tokens = 0u64;
        let mut agg_cost = 0.0f64;
        let mut agg_prompts = 0usize;
        let mut agg_branches = HashSet::new();
        let mut session_entries = Vec::new();

        for project in &filtered_projects {
            if project_path.is_empty() {
                project_path = project.best_path().clone();
            }

            let mut sessions = match project.main_sessions() {
                Ok(s) => s,
                Err(_) => continue,
            };

            // Filter by time
            if let Some(cutoff_time) = cutoff {
                sessions.retain(|s| s.modified_datetime() >= cutoff_time);
            }

            // Detect chains for this project
            let chains = project.session_chains().unwrap_or_default();

            // Build lookup: session_id → chain info
            let mut chain_lookup: HashMap<String, (&str, usize, Option<&str>)> = HashMap::new();
            let mut skip_set: HashSet<String> = HashSet::new();
            for chain in chains.values() {
                for member in &chain.members {
                    chain_lookup.insert(
                        member.file_id.clone(),
                        (&chain.root_id, chain.len(), chain.slug.as_deref()),
                    );
                    if member.file_id != chain.root_id {
                        skip_set.insert(member.file_id.clone());
                    }
                }
            }

            for session in &sessions {
                let sid = session.session_id().to_string();

                // Skip non-root chain members (they'll be included in the root entry)
                if skip_set.contains(&sid) {
                    continue;
                }

                // If this is a chain root, parse the full chain; otherwise single file
                let (entries, chain_info) = if let Some(chain) = chains.get(&sid) {
                    match project.parse_chain(chain) {
                        Ok(e) => (
                            e,
                            Some((chain.root_id.clone(), chain.len(), chain.slug.clone())),
                        ),
                        Err(_) => continue,
                    }
                } else {
                    match session.parse_with_options(self.max_file_size) {
                        Ok(e) => {
                            let slug = chain_lookup
                                .get(&sid)
                                .and_then(|(_, _, s)| s.map(String::from));
                            (e, slug.map(|s| (sid.clone(), 1, Some(s))))
                        }
                        Err(_) => continue,
                    }
                };

                let conversation = match Conversation::from_entries(entries) {
                    Ok(c) => c,
                    Err(_) => continue,
                };

                let analytics = SessionAnalytics::from_conversation(&conversation);
                let summary_report = analytics.summary_report();

                let main_entries = conversation.main_thread_entries();
                let main_refs: Vec<&LogEntry> = main_entries.clone();

                // Extract user prompts (excluding system noise)
                let mut prompts: Vec<String> = Vec::new();
                let mut prompt_count = 0usize;
                for entry in &main_refs {
                    if is_human_prompt(entry) {
                        prompt_count += 1;
                        if include_summaries && prompts.len() < 3 {
                            if let Some(text) = extract_user_prompt_text(entry) {
                                if text.len() > 20 {
                                    prompts.push(truncate_text(&text, 150));
                                }
                            }
                        }
                    }
                }

                // Extract git branch
                let branch = main_refs
                    .iter()
                    .find_map(|e| e.git_branch().map(String::from));
                if let Some(ref b) = branch {
                    agg_branches.insert(b.clone());
                }

                // Extract files
                let files = extract_files_from_tools(&main_refs);

                // Tool counts
                let mut tool_counts: HashMap<String, usize> = HashMap::new();
                for entry in &main_refs {
                    for name in extract_tool_names(entry) {
                        *tool_counts.entry(name).or_default() += 1;
                    }
                }

                let first_prompt = prompts.first().cloned();
                let start_time = analytics.start_time.map(|t| t.to_rfc3339());
                let end_time = analytics.end_time.map(|t| t.to_rfc3339());
                let span = analytics.duration_string();
                let tokens = summary_report.total_tokens;
                let cost = summary_report.estimated_cost;

                agg_tokens += tokens;
                agg_cost += cost.unwrap_or(0.0);
                agg_prompts += prompt_count;

                let compaction_count = session
                    .quick_metadata_cached()
                    .map(|m| m.compaction_count)
                    .unwrap_or(0);

                // Extract chain metadata
                let (chain_id, chain_length, slug) = match chain_info {
                    Some((root, len, s)) if len > 1 => (Some(root), Some(len), s),
                    Some((_, _, s)) => (None, None, s),
                    None => (None, None, None),
                };

                session_entries.push(ProjectSessionEntry {
                    session_id: session.session_id().to_string(),
                    provider: None,
                    qualified_id: None,
                    slug,
                    chain_id,
                    chain_length,
                    is_subagent: session.is_subagent(),
                    parent_session_id: session.parent_session_id().map(String::from),
                    start_time,
                    end_time,
                    span,
                    compaction_count,
                    git_branch: branch,
                    user_prompt_count: prompt_count,
                    first_prompt,
                    key_prompts: prompts,
                    tools_summary: tool_counts,
                    files_touched: files.into_iter().take(10).collect(),
                    estimated_cost: cost,
                    total_tokens: tokens,
                });
            }
        }

        // Filter out empty sessions (no prompts and no tokens)
        session_entries.retain(|s| s.user_prompt_count > 0 || s.total_tokens > 0);

        // Sort by start time (newest first) and truncate
        session_entries.sort_by(|a, b| b.start_time.cmp(&a.start_time));
        session_entries.truncate(limit);

        let sessions_found = session_entries.len();

        let mut branches: Vec<String> = agg_branches.into_iter().collect();
        branches.sort();

        let response = ProjectHistoryResponse {
            project_path,
            project_key: None,
            providers: Vec::new(),
            period: period.to_string(),
            sessions_found,
            sessions: session_entries,
            aggregate: ProjectAggregate {
                total_sessions: sessions_found,
                total_tokens: agg_tokens,
                total_cost: Some(agg_cost),
                has_unpriced_sessions: None,
                total_prompts: agg_prompts,
                active_branches: branches,
            },
            skipped_providers: Vec::new(),
            project_warnings: Vec::new(),
            activity_basis: None,
        };

        match ToolOutput::json(&response) {
            Ok(output) => output,
            Err(e) => ToolOutput::error(format!("JSON serialization error: {e}")),
        }
    }

    // ========================================================================
    // New Tool: search_sessions
    // ========================================================================

    /// Search across sessions for text patterns using regex.
    #[tool(
        description = "Search across sessions for text patterns (regex). Filter by project, session, scope (text/tools/thinking/all). scope='thinking' searches reasoning blocks, but matches only sessions from old Claude Code (~2.1.4x and earlier) — recent versions persist thinking as empty text, and the response notes when only empty blocks were scanned. Returns matching text with context."
    )]
    async fn search_sessions(&self, request: SearchSessionsRequest) -> ToolOutput {
        let claude_dir = match get_claude_dir(self) {
            Ok(dir) => dir,
            Err(e) => return e,
        };

        let scope = request.scope.as_deref().unwrap_or("text");
        let ignore_case = request.ignore_case.unwrap_or(true);
        let limit = request.limit.unwrap_or(20);

        let regex = match regex::RegexBuilder::new(&request.pattern)
            .case_insensitive(ignore_case)
            .build()
        {
            Ok(r) => r,
            Err(e) => return ToolOutput::error(format!("Invalid regex pattern: {e}")),
        };

        let chain_aware = request.chain_aware.unwrap_or(true);

        // Determine which sessions to search
        let sessions = if let Some(ref session_id) = request.session_id {
            let session = match claude_dir.find_session(session_id) {
                Ok(Some(s)) => s,
                Ok(None) => return ToolOutput::error(format!("Session not found: {session_id}")),
                Err(e) => return ToolOutput::error(format!("Failed to find session: {e}")),
            };
            // When chain-aware and the session is part of a multi-file resume
            // chain, expand the search to every member file (once), consistent
            // with the other chain-aware tools.
            let mut expanded: Option<Vec<Session>> = None;
            if chain_aware {
                if let Ok(projects) = claude_dir.projects() {
                    if let Some(project) = projects.iter().find(|p| {
                        p.best_path() == session.project_path()
                            || p.decoded_path() == session.project_path()
                    }) {
                        if let Ok(chains) = project.session_chains() {
                            if let Some(chain) = chains
                                .values()
                                .find(|c| c.len() > 1 && c.contains(session.session_id()))
                            {
                                let mut members = Vec::new();
                                for fid in chain.file_ids() {
                                    if let Ok(Some(s)) = claude_dir.find_session(fid) {
                                        members.push(s);
                                    }
                                }
                                if !members.is_empty() {
                                    expanded = Some(members);
                                }
                            }
                        }
                    }
                }
            }
            expanded.unwrap_or_else(|| vec![session])
        } else {
            let mut all = match claude_dir.all_sessions() {
                Ok(s) => s,
                Err(e) => return ToolOutput::error(format!("Failed to list sessions: {e}")),
            };
            if let Some(ref project) = request.project {
                all.retain(|s| s.project_path().contains(project));
            }
            all.retain(|s| !s.is_subagent());
            all
        };

        // Build chain lookup: session_id → chain root_id
        let mut chain_lookup: HashMap<String, String> = HashMap::new();
        if let Ok(projects) = claude_dir.projects() {
            for project in &projects {
                if let Some(ref proj_filter) = request.project {
                    if !project.best_path().contains(proj_filter) {
                        continue;
                    }
                }
                if let Ok(chains) = project.session_chains() {
                    for chain in chains.values() {
                        for member in &chain.members {
                            chain_lookup.insert(member.file_id.clone(), chain.root_id.clone());
                        }
                    }
                }
            }
        }

        let mut results = Vec::new();

        // Track thinking-block emptiness for scope="thinking" so zero matches
        // on redaction-era sessions (thinking persisted as empty text) is
        // explained rather than silent.
        let track_thinking = scope == "thinking";
        let mut thinking_blocks_seen = 0usize;
        let mut nonempty_thinking_seen = 0usize;

        for session in &sessions {
            let entries = match session.parse_with_options(self.max_file_size) {
                Ok(e) => e,
                Err(_) => continue,
            };

            let conversation = match Conversation::from_entries(entries) {
                Ok(c) => c,
                Err(_) => continue,
            };

            let sid = session.session_id().to_string();
            let chain_id = chain_lookup.get(&sid).cloned();

            // Search ALL entries (not just main thread) so branches,
            // sidechains, and agent sub-conversations are included.
            for entry in conversation.chronological_entries() {
                if track_thinking {
                    if let LogEntry::Assistant(assistant) = entry {
                        for block in assistant.message.thinking_blocks() {
                            thinking_blocks_seen += 1;
                            if !block.thinking.trim().is_empty() {
                                nonempty_thinking_seen += 1;
                            }
                        }
                    }
                }
                let matches = search_entry_text(entry, &regex, scope, 100);
                for (matched, context) in matches {
                    results.push(SearchMatch {
                        session_id: sid.clone(),
                        project_path: session.project_path().to_string(),
                        chain_id: chain_id.clone(),
                        timestamp: entry.timestamp().map(|t| t.to_rfc3339()),
                        message_type: entry.message_type().to_string(),
                        matched_text: truncate_text(&matched, 200),
                        context: truncate_text(&context, 300),
                    });
                }
            }
        }

        let total = results.len();
        results.truncate(limit);
        let returned = results.len();
        let note = (track_thinking && thinking_blocks_seen > 0 && nonempty_thinking_seen == 0)
            .then(|| {
                format!(
                    "searched {thinking_blocks_seen} thinking block(s) but all are empty — recent Claude Code versions do not persist thinking text, so scope=\"thinking\" cannot match in these sessions"
                )
            });
        let response = SearchSessionsResponse {
            pattern: request.pattern,
            total_matches: total,
            returned,
            results,
            note,
        };

        match ToolOutput::json(&response) {
            Ok(output) => output,
            Err(e) => ToolOutput::error(format!("JSON serialization error: {e}")),
        }
    }

    // ========================================================================
    // New Tool: get_tool_calls
    // ========================================================================

    /// Extract tool invocations from a session with input summaries and error states.
    #[tool(
        description = "Extract tool invocations from a Claude Code or Codex session (select with provider or a qualified id). Filter by tool name or errors, or scope to prompt-boundary chunk(s) with chunk='4' / chunk='2-5'. errors_only defaults to confirmed failures; failure_kind='all' also includes explicitly labeled text-inferred signals. The summary reports both counts for the main-thread scope. Use to audit claims against commands without pulling message bodies."
    )]
    async fn get_tool_calls(&self, request: GetToolCallsRequest) -> ToolOutput {
        let provider_flags = request.provider.as_deref().unwrap_or(&[]);
        let resolved = match self.resolve_analytical_session(provider_flags, &request.session_id) {
            Ok(r) => r,
            Err(e) => return e,
        };

        let limit = request.limit.unwrap_or(100);
        let offset = request.offset.unwrap_or(0);
        let errors_only = request.errors_only.unwrap_or(false);
        let failure_kind = request.failure_kind.as_deref().unwrap_or("confirmed");
        if !matches!(failure_kind, "confirmed" | "inferred" | "all") {
            return ToolOutput::error(
                "failure_kind must be one of: confirmed, inferred, all".to_string(),
            );
        }
        if request.failure_kind.is_some() && !errors_only {
            return ToolOutput::error("failure_kind requires errors_only=true".to_string());
        }

        let tool_filter: Option<HashSet<String>> = request
            .tool_filter
            .map(|f| f.split(',').map(|s| s.trim().to_string()).collect());

        // Optionally scope to prompt-boundary chunk(s) — the ground-truth
        // view of what actually ran in a chunk (attached async results
        // included via tree-based membership).
        let main_entries = if let Some(ref spec) = request.chunk {
            use crate::analysis::chunking::{
                chunk_conversation, chunk_conversation_semantic, entries_for_chunk_range,
                parse_chunk_spec,
            };
            let chunking = if resolved.semantic_annotations {
                chunk_conversation_semantic(&resolved.conversation)
            } else {
                chunk_conversation(&resolved.conversation)
            };
            let (start, end) = match parse_chunk_spec(spec, chunking.len()) {
                Ok(range) => range,
                Err(message) => return ToolOutput::error(format!("Invalid chunk: {message}")),
            };
            entries_for_chunk_range(&resolved.conversation, &chunking, start, end)
        } else {
            resolved.conversation.main_thread_entries()
        };

        // Build list of tool calls with their results
        struct ToolCallWithResult {
            timestamp: Option<String>,
            tool_name: String,
            input: serde_json::Value,
            failure: Option<crate::analysis::lessons::FailureClassification>,
            error_text: Option<String>,
            result_text: Option<String>,
            lifecycle: Option<Vec<ToolLifecycleEntry>>,
        }

        let mut all_calls: Vec<ToolCallWithResult> = Vec::new();
        let mut call_index: HashMap<String, (String, serde_json::Value)> = HashMap::new();
        for entry in &main_entries {
            if let LogEntry::Assistant(assistant) = entry {
                for tool_use in assistant.message.tool_uses() {
                    call_index.insert(
                        tool_use.id.clone(),
                        (tool_use.name.clone(), tool_use.input.clone()),
                    );
                }
            }
        }
        let semantics =
            crate::analysis::lessons::conversation_tool_semantics(&resolved.conversation);
        type ToolResultInfo = (
            Option<crate::analysis::lessons::FailureClassification>,
            Option<String>,
            Option<String>,
        );
        let mut tool_result_map: HashMap<String, ToolResultInfo> = HashMap::new();

        // First pass: collect tool results from user messages
        for entry in &main_entries {
            if let LogEntry::User(user) = entry {
                for result in user.message.tool_results() {
                    let (tool_name, input) = call_index
                        .get(&result.tool_use_id)
                        .cloned()
                        .unwrap_or_else(|| ("unknown".to_string(), serde_json::Value::Null));
                    let failure = crate::analysis::lessons::classify_tool_result(
                        &tool_name,
                        &input,
                        result,
                        semantics.get(&result.tool_use_id),
                        resolved.semantic_annotations,
                    );
                    let display = result
                        .content
                        .as_ref()
                        .map(|content| content.to_display_string(false));
                    let (err_text, res_text) = if failure.is_some() {
                        (
                            display.as_deref().map(|text| truncate_text(text, 300)),
                            None,
                        )
                    } else {
                        (None, extract_result_preview(result, 500))
                    };
                    tool_result_map
                        .insert(result.tool_use_id.clone(), (failure, err_text, res_text));
                }
            }
        }

        // Second pass: collect tool uses from assistant messages
        for entry in &main_entries {
            if let LogEntry::Assistant(assistant) = entry {
                let timestamp = entry.timestamp().map(|t| t.to_rfc3339());
                for tool_use in assistant.message.tool_uses() {
                    let (failure, error_text, result_text) = tool_result_map
                        .get(&tool_use.id)
                        .cloned()
                        .unwrap_or((None, None, None));
                    let lifecycle = semantics
                        .get(&tool_use.id)
                        .map(|tool| {
                            tool.lifecycle
                                .iter()
                                .map(|observation| ToolLifecycleEntry {
                                    kind: observation.kind.as_str().to_string(),
                                    status: observation
                                        .status
                                        .as_ref()
                                        .map(|status| status.as_str().to_string()),
                                    success: observation.success,
                                    exit_code: observation.exit_code,
                                    duration_ms: observation
                                        .duration
                                        .map(|duration| duration.as_secs_f64() * 1_000.0),
                                    source: observation.source.clone(),
                                })
                                .collect::<Vec<_>>()
                        })
                        .filter(|items| !items.is_empty());

                    all_calls.push(ToolCallWithResult {
                        timestamp: timestamp.clone(),
                        tool_name: tool_use.name.clone(),
                        input: tool_use.input.clone(),
                        failure,
                        error_text,
                        result_text,
                        lifecycle,
                    });
                }
            }
        }

        // Apply filters
        if let Some(ref filter) = tool_filter {
            all_calls.retain(|c| filter.contains(&c.tool_name));
        }
        let confirmed_tool_failures = all_calls
            .iter()
            .filter(|call| {
                call.failure.is_some_and(|failure| {
                    failure.kind == crate::analysis::lessons::FailureKind::Confirmed
                })
            })
            .count();
        let inferred_failure_signals = all_calls
            .iter()
            .filter(|call| {
                call.failure.is_some_and(|failure| {
                    failure.kind == crate::analysis::lessons::FailureKind::Inferred
                })
            })
            .count();
        if errors_only {
            all_calls.retain(|call| match (failure_kind, call.failure) {
                ("confirmed", Some(failure)) => {
                    failure.kind == crate::analysis::lessons::FailureKind::Confirmed
                }
                ("inferred", Some(failure)) => {
                    failure.kind == crate::analysis::lessons::FailureKind::Inferred
                }
                ("all", Some(_)) => true,
                _ => false,
            });
        }

        let total_tool_calls = all_calls.len();

        // Build summary before pagination
        let mut by_tool: HashMap<String, usize> = HashMap::new();
        let mut files_written = HashSet::new();
        let mut files_edited = HashSet::new();
        let mut error_count = 0usize;

        for call in &all_calls {
            *by_tool.entry(call.tool_name.clone()).or_default() += 1;
            if call.failure.is_some() {
                error_count += 1;
            }
            if let Some(fp) = call.input.get("file_path").and_then(|v| v.as_str()) {
                let basename = std::path::Path::new(fp)
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or(fp);
                match call.tool_name.as_str() {
                    "Write" => {
                        files_written.insert(basename.to_string());
                    }
                    "Edit" => {
                        files_edited.insert(basename.to_string());
                    }
                    _ => {}
                }
            }
        }

        // Paginate
        let paginated: Vec<ToolCallEntry> = all_calls
            .into_iter()
            .skip(offset)
            .take(limit)
            .enumerate()
            .map(|(i, call)| {
                let input_summary = extract_tool_input_summary(&call.tool_name, &call.input);
                ToolCallEntry {
                    index: offset + i,
                    timestamp: call.timestamp,
                    tool_name: call.tool_name,
                    input_summary,
                    had_error: call.failure.is_some(),
                    failure_kind: call.failure.map(|failure| failure.kind),
                    failure_basis: call.failure.map(|failure| failure.basis),
                    error_preview: call.error_text,
                    result_preview: call.result_text,
                    lifecycle: call.lifecycle,
                }
            })
            .collect();

        let returned = paginated.len();
        let entry_scope = match (resolved.semantic_annotations, request.chunk.is_some()) {
            (true, true) => "semantic_chunk",
            (true, false) => "semantic_main_thread",
            (false, true) => "chunk",
            (false, false) => "main_thread",
        };

        let mut written: Vec<String> = files_written.into_iter().collect();
        written.sort();
        let mut edited: Vec<String> = files_edited.into_iter().collect();
        edited.sort();

        let response = ToolCallsResponse {
            session_id: resolved.session_id,
            provider: resolved.provider,
            qualified_id: resolved.qualified_id,
            total_tool_calls,
            returned,
            tool_calls: paginated,
            summary: ToolCallsSummary {
                by_tool,
                files_written: written,
                files_edited: edited,
                error_count,
                confirmed_tool_failures,
                inferred_failure_signals,
                selected_failure_kind: failure_kind.to_string(),
                entry_scope: entry_scope.to_string(),
            },
        };

        match ToolOutput::json(&response) {
            Ok(output) => output,
            Err(e) => ToolOutput::error(format!("JSON serialization error: {e}")),
        }
    }

    // ========================================================================
    // New Tool: get_session_lessons
    // ========================================================================

    /// Extract operational lessons from a session: error→fix pairs and user corrections.
    /// Targets the most expensive compaction failure mode (negative result amnesia).
    #[tool(
        description = "Extract lessons from an agent session: error->fix pairs (what failed and how it was resolved) and high-precision human corrections, each labeled with its dialogue evidence. Claude Code is the default; select another provider with provider or a qualified id. category filters the returned lists; summary totals always describe the full session. Provider tool and prompt semantics suppress content-shaped false positives. Best for recovery and continuity; to independently judge what happened (adversarial review), read the raw transcript with get_session_messages rather than this pre-extracted summary."
    )]
    async fn get_session_lessons(&self, request: GetSessionLessonsRequest) -> ToolOutput {
        use crate::analysis::lessons::{
            extract_lessons_from_conversation, LessonCategory, LessonOptions,
        };

        let provider_flags = request.provider.as_deref().unwrap_or(&[]);
        let registry = self.provider_registry();
        let provider_route =
            !provider_flags.is_empty() || registry.looks_qualified(&request.session_id);
        let resolved = match if provider_route {
            self.resolve_provider_session(provider_flags, &request.session_id)
        } else {
            resolve_session(self, &request.session_id)
        } {
            Ok(r) => r,
            Err(e) => return e,
        };

        let category = request.category.as_deref().unwrap_or("all");
        let limit = request.limit.unwrap_or(30);

        // Use all entries (not just main thread) so lessons on branches
        // and across compaction boundaries are visible.
        let opts = LessonOptions {
            category: LessonCategory::from_str_loose(category),
            limit,
            ..LessonOptions::default()
        };

        let result = extract_lessons_from_conversation(
            &resolved.conversation,
            &opts,
            resolved.semantic_annotations,
        );

        // Convert from analysis types to MCP wire types
        let response = SessionLessonsResponse {
            session_id: resolved.session_id,
            provider: resolved.provider,
            qualified_id: resolved.qualified_id,
            project_path: resolved.project_path,
            error_fix_pairs: result
                .error_fix_pairs
                .into_iter()
                .map(|p| ErrorFixLesson {
                    timestamp: p.timestamp,
                    tool_name: p.tool_name,
                    input_summary: p.input_summary,
                    error_preview: p.error_preview,
                    failure_kind: p.failure_kind,
                    failure_basis: p.failure_basis,
                    resolution_summary: p.resolution_summary,
                    resolution_tools: p.resolution_tools,
                })
                .collect(),
            user_corrections: result
                .user_corrections
                .into_iter()
                .map(|c| UserCorrection {
                    timestamp: c.timestamp,
                    user_text: c.user_text,
                    correction_basis: c.correction_basis,
                    prior_assistant_summary: c.prior_assistant_summary,
                })
                .collect(),
            summary: LessonsSummary {
                total_errors: result.summary.total_errors,
                confirmed_tool_failures: result.summary.confirmed_tool_failures,
                inferred_failure_signals: result.summary.inferred_failure_signals,
                entry_scope: if resolved.semantic_annotations {
                    "new_activity_whole_conversation"
                } else {
                    "whole_conversation"
                }
                .to_string(),
                total_corrections: result.summary.total_corrections,
                most_error_prone_tools: result.summary.most_error_prone_tools,
            },
        };

        match ToolOutput::json(&response) {
            Ok(output) => output,
            Err(e) => ToolOutput::error(format!("JSON serialization error: {e}")),
        }
    }

    // ========================================================================
    // Goal Management
    // ========================================================================

    /// Manage persistent goals in Claude Code project-memory storage.
    #[tool(
        description = "Manage persistent goals in the Claude Code project-memory registry (not a cross-provider union). Operations: list, add, update, remove. Omit provider or use provider='claude-code'."
    )]
    async fn manage_goals(&self, request: ManageGoalsRequest) -> ToolOutput {
        use crate::goals::{load_goals, save_goals, GoalStatus};

        let explicit_scope = match claude_registry_scope(request.provider.as_deref()) {
            Ok(explicit) => explicit,
            Err(error) => return ToolOutput::error(error),
        };

        let resolved = match resolve_project(self, &request.project) {
            Ok(r) => r,
            Err(e) => return e,
        };

        let mut store = match load_goals(&resolved.project_dir) {
            Ok(s) => s,
            Err(e) => return ToolOutput::error(format!("Failed to load goals: {e}")),
        };

        match request.operation.as_str() {
            "list" => {
                let goals: Vec<GoalEntry> = store
                    .goals
                    .iter()
                    .map(|g| GoalEntry {
                        id: g.id,
                        text: g.text.clone(),
                        status: g.status.to_string(),
                        created_at: g.created_at.to_rfc3339(),
                        updated_at: g.updated_at.to_rfc3339(),
                        progress: g.progress.clone(),
                    })
                    .collect();

                let response = ManageGoalsResponse {
                    operation: "list".into(),
                    project_path: resolved.project_path,
                    storage_provider: explicit_scope.then(|| "claude-code".into()),
                    message: Some(format!("{} goal(s)", goals.len())),
                    goals: Some(goals),
                    goal: None,
                };

                match ToolOutput::json(&response) {
                    Ok(output) => output,
                    Err(e) => ToolOutput::error(format!("JSON error: {e}")),
                }
            }

            "add" => {
                let text = match request.text {
                    Some(t) if !t.trim().is_empty() => t,
                    _ => return ToolOutput::error("'text' is required for add operation"),
                };

                let id = store.add_goal(text.clone(), request.progress);

                if let Err(e) = save_goals(&resolved.project_dir, &store) {
                    return ToolOutput::error(format!("Failed to save goals: {e}"));
                }

                let goal = &store.goals.iter().find(|g| g.id == id).unwrap();
                let response = ManageGoalsResponse {
                    operation: "add".into(),
                    project_path: resolved.project_path,
                    storage_provider: explicit_scope.then(|| "claude-code".into()),
                    message: Some(format!("Added goal #{id}")),
                    goals: None,
                    goal: Some(GoalEntry {
                        id: goal.id,
                        text: goal.text.clone(),
                        status: goal.status.to_string(),
                        created_at: goal.created_at.to_rfc3339(),
                        updated_at: goal.updated_at.to_rfc3339(),
                        progress: goal.progress.clone(),
                    }),
                };

                match ToolOutput::json(&response) {
                    Ok(output) => output,
                    Err(e) => ToolOutput::error(format!("JSON error: {e}")),
                }
            }

            "update" => {
                let id = match request.id {
                    Some(id) => id,
                    None => return ToolOutput::error("'id' is required for update operation"),
                };

                let status = match request.status.as_deref() {
                    Some(s) => match GoalStatus::parse(s) {
                        Some(status) => Some(status),
                        None => {
                            return ToolOutput::error(format!(
                                "Invalid status '{s}'. Use: open, in_progress, done, abandoned"
                            ))
                        }
                    },
                    None => None,
                };

                if request.text.as_deref().is_some_and(|t| t.trim().is_empty()) {
                    return ToolOutput::error("'text' cannot be empty");
                }

                if status.is_none() && request.text.is_none() && request.progress.is_none() {
                    return ToolOutput::error(
                        "At least one of 'status', 'text', or 'progress' is required for update",
                    );
                }

                if !store.update_goal(id, status, request.text, request.progress) {
                    return ToolOutput::error(format!("Goal #{id} not found"));
                }

                if let Err(e) = save_goals(&resolved.project_dir, &store) {
                    return ToolOutput::error(format!("Failed to save goals: {e}"));
                }

                let goal = store.goals.iter().find(|g| g.id == id).unwrap();
                let response = ManageGoalsResponse {
                    operation: "update".into(),
                    project_path: resolved.project_path,
                    storage_provider: explicit_scope.then(|| "claude-code".into()),
                    message: Some(format!("Updated goal #{id}")),
                    goals: None,
                    goal: Some(GoalEntry {
                        id: goal.id,
                        text: goal.text.clone(),
                        status: goal.status.to_string(),
                        created_at: goal.created_at.to_rfc3339(),
                        updated_at: goal.updated_at.to_rfc3339(),
                        progress: goal.progress.clone(),
                    }),
                };

                match ToolOutput::json(&response) {
                    Ok(output) => output,
                    Err(e) => ToolOutput::error(format!("JSON error: {e}")),
                }
            }

            "remove" => {
                let id = match request.id {
                    Some(id) => id,
                    None => return ToolOutput::error("'id' is required for remove operation"),
                };

                if !store.remove_goal(id) {
                    return ToolOutput::error(format!("Goal #{id} not found"));
                }

                if let Err(e) = save_goals(&resolved.project_dir, &store) {
                    return ToolOutput::error(format!("Failed to save goals: {e}"));
                }

                let response = ManageGoalsResponse {
                    operation: "remove".into(),
                    project_path: resolved.project_path,
                    storage_provider: explicit_scope.then(|| "claude-code".into()),
                    message: Some(format!("Removed goal #{id}")),
                    goals: None,
                    goal: None,
                };

                match ToolOutput::json(&response) {
                    Ok(output) => output,
                    Err(e) => ToolOutput::error(format!("JSON error: {e}")),
                }
            }

            other => ToolOutput::error(format!(
                "Unknown operation '{other}'. Use: list, add, update, remove"
            )),
        }
    }

    // ========================================================================
    // Session Digest
    // ========================================================================

    /// Get a compact summary of a session's key topics, files, tools, and decisions.
    #[tool(
        description = "Get a compact structured digest of a Claude Code or Codex session (select with provider or a qualified id): key/recent prompts, files, tools, confirmed failures vs inferred signals, compactions, and decision keywords. Quoted relays are reduced to their primary preamble and adjacent repeated prompts are compacted without changing the emission total. include_formatted defaults to false (the structured fields carry the same information); set it to true only when a client needs a duplicate pre-rendered text form."
    )]
    async fn get_session_digest(&self, request: GetSessionDigestRequest) -> ToolOutput {
        use crate::analysis::digest::{
            build_digest_from_conversation, format_digest, DigestOptions,
        };

        let provider_flags = request.provider.as_deref().unwrap_or(&[]);
        let resolved = match self.resolve_analytical_session(provider_flags, &request.session_id) {
            Ok(r) => r,
            Err(e) => return e,
        };

        let opts = DigestOptions {
            max_prompts: request.max_prompts.unwrap_or(3),
            max_files: request.max_files.unwrap_or(10),
            ..DigestOptions::default()
        };

        let digest = build_digest_from_conversation(
            &resolved.conversation,
            &opts,
            resolved.semantic_annotations,
        );
        let formatted = request
            .include_formatted
            .unwrap_or(false)
            .then(|| format_digest(&digest, opts.max_chars));

        let response = SessionDigestResponse {
            session_id: resolved.session_id,
            provider: resolved.provider,
            qualified_id: resolved.qualified_id,
            project_path: resolved.project_path,
            key_prompts: digest.key_prompts,
            recent_prompts: digest.recent_prompts,
            total_prompts: digest.total_prompts,
            files_touched: digest.files_touched,
            top_tools: digest.top_tools,
            error_count: digest.error_count,
            confirmed_tool_failures: digest.confirmed_tool_failures,
            inferred_failure_signals: digest.inferred_failure_signals,
            failure_entry_scope: "whole_conversation".to_string(),
            compaction_count: digest.compaction_count,
            thinking_keywords: digest.thinking_keywords,
            thinking_note: digest.thinking_note,
            formatted,
        };

        match ToolOutput::json(&response) {
            Ok(output) => output,
            Err(e) => ToolOutput::error(format!("JSON serialization error: {e}")),
        }
    }

    // ========================================================================
    // Tactical Notes
    // ========================================================================

    /// Manage tactical notes in Claude Code project-memory storage.
    #[tool(
        description = "Manage tactical notes in the Claude Code project-memory registry (not a cross-provider union). Operations: list, add, update, remove, clear. Omit provider or use provider='claude-code'."
    )]
    async fn manage_notes(&self, request: ManageNotesRequest) -> ToolOutput {
        use crate::notes::{load_notes, save_notes};

        let explicit_scope = match claude_registry_scope(request.provider.as_deref()) {
            Ok(explicit) => explicit,
            Err(error) => return ToolOutput::error(error),
        };

        let resolved = match resolve_project(self, &request.project) {
            Ok(r) => r,
            Err(e) => return e,
        };

        let mut store = match load_notes(&resolved.project_dir) {
            Ok(s) => s,
            Err(e) => return ToolOutput::error(format!("Failed to load notes: {e}")),
        };

        match request.operation.as_str() {
            "list" => {
                let notes: Vec<NoteEntry> = store
                    .notes
                    .iter()
                    .map(|n| NoteEntry {
                        id: n.id,
                        text: n.text.clone(),
                        created_at: n.created_at.to_rfc3339(),
                        session_id: n.session_id.clone(),
                        resurface_when: n.resurface_when.clone(),
                        expires_when: n.expires_when.clone(),
                    })
                    .collect();

                let response = ManageNotesResponse {
                    operation: "list".into(),
                    project_path: resolved.project_path,
                    storage_provider: explicit_scope.then(|| "claude-code".into()),
                    message: Some(format!("{} note(s)", notes.len())),
                    notes: Some(notes),
                    note: None,
                };

                match ToolOutput::json(&response) {
                    Ok(output) => output,
                    Err(e) => ToolOutput::error(format!("JSON error: {e}")),
                }
            }

            "add" => {
                let text = match request.text {
                    Some(t) if !t.trim().is_empty() => t,
                    _ => return ToolOutput::error("'text' is required for add operation"),
                };

                let id = store.add_note(text.clone(), request.session_id);
                if request.resurface_when.is_some() || request.expires_when.is_some() {
                    store.set_note_schedule(id, request.resurface_when, request.expires_when);
                }

                if let Err(e) = save_notes(&resolved.project_dir, &store) {
                    return ToolOutput::error(format!("Failed to save notes: {e}"));
                }

                let note = store.notes.iter().find(|n| n.id == id).unwrap();
                let response = ManageNotesResponse {
                    operation: "add".into(),
                    project_path: resolved.project_path,
                    storage_provider: explicit_scope.then(|| "claude-code".into()),
                    message: Some(format!("Added note #{id}")),
                    notes: None,
                    note: Some(NoteEntry {
                        id: note.id,
                        text: note.text.clone(),
                        created_at: note.created_at.to_rfc3339(),
                        session_id: note.session_id.clone(),
                        resurface_when: note.resurface_when.clone(),
                        expires_when: note.expires_when.clone(),
                    }),
                };

                match ToolOutput::json(&response) {
                    Ok(output) => output,
                    Err(e) => ToolOutput::error(format!("JSON error: {e}")),
                }
            }

            "update" => {
                let id = match request.id {
                    Some(id) => id,
                    None => return ToolOutput::error("'id' is required for update operation"),
                };

                if request.text.as_deref().is_some_and(|t| t.trim().is_empty()) {
                    return ToolOutput::error("'text' cannot be empty");
                }

                if request.text.is_none()
                    && request.session_id.is_none()
                    && request.resurface_when.is_none()
                    && request.expires_when.is_none()
                {
                    return ToolOutput::error(
                        "At least one of 'text', 'session_id', 'resurface_when', or 'expires_when' is required for update",
                    );
                }

                if !store.update_note(id, request.text, request.session_id) {
                    return ToolOutput::error(format!("Note #{id} not found"));
                }
                if request.resurface_when.is_some() || request.expires_when.is_some() {
                    store.set_note_schedule(id, request.resurface_when, request.expires_when);
                }

                if let Err(e) = save_notes(&resolved.project_dir, &store) {
                    return ToolOutput::error(format!("Failed to save notes: {e}"));
                }

                let note = store.notes.iter().find(|n| n.id == id).unwrap();
                let response = ManageNotesResponse {
                    operation: "update".into(),
                    project_path: resolved.project_path,
                    storage_provider: explicit_scope.then(|| "claude-code".into()),
                    message: Some(format!("Updated note #{id}")),
                    notes: None,
                    note: Some(NoteEntry {
                        id: note.id,
                        text: note.text.clone(),
                        created_at: note.created_at.to_rfc3339(),
                        session_id: note.session_id.clone(),
                        resurface_when: note.resurface_when.clone(),
                        expires_when: note.expires_when.clone(),
                    }),
                };

                match ToolOutput::json(&response) {
                    Ok(output) => output,
                    Err(e) => ToolOutput::error(format!("JSON error: {e}")),
                }
            }

            "remove" => {
                let id = match request.id {
                    Some(id) => id,
                    None => return ToolOutput::error("'id' is required for remove operation"),
                };

                if !store.remove_note(id) {
                    return ToolOutput::error(format!("Note #{id} not found"));
                }

                if let Err(e) = save_notes(&resolved.project_dir, &store) {
                    return ToolOutput::error(format!("Failed to save notes: {e}"));
                }

                let response = ManageNotesResponse {
                    operation: "remove".into(),
                    project_path: resolved.project_path,
                    storage_provider: explicit_scope.then(|| "claude-code".into()),
                    message: Some(format!("Removed note #{id}")),
                    notes: None,
                    note: None,
                };

                match ToolOutput::json(&response) {
                    Ok(output) => output,
                    Err(e) => ToolOutput::error(format!("JSON error: {e}")),
                }
            }

            "clear" => {
                let removed = store.clear();

                if let Err(e) = save_notes(&resolved.project_dir, &store) {
                    return ToolOutput::error(format!("Failed to save notes: {e}"));
                }

                let response = ManageNotesResponse {
                    operation: "clear".into(),
                    project_path: resolved.project_path,
                    storage_provider: explicit_scope.then(|| "claude-code".into()),
                    message: Some(format!("Cleared {removed} note(s)")),
                    notes: None,
                    note: None,
                };

                match ToolOutput::json(&response) {
                    Ok(output) => output,
                    Err(e) => ToolOutput::error(format!("JSON error: {e}")),
                }
            }

            other => ToolOutput::error(format!(
                "Unknown operation '{other}'. Use: list, add, update, remove, clear"
            )),
        }
    }

    #[tool(
        description = "Manage the persistent Claude Code project-memory decision registry (not a cross-provider union). Operations: list, add, update, remove, supersede. Omit provider or use provider='claude-code'. For confidence auto-scoring use CLI: snatch decisions score -p <project>."
    )]
    async fn manage_decisions(&self, request: ManageDecisionsRequest) -> ToolOutput {
        use crate::decisions::{load_decisions, save_decisions, DecisionStatus, DecisionUpdate};

        let explicit_scope = match claude_registry_scope(request.provider.as_deref()) {
            Ok(explicit) => explicit,
            Err(error) => return ToolOutput::error(error),
        };

        let resolved = match resolve_project(self, &request.project) {
            Ok(r) => r,
            Err(e) => return e,
        };

        let mut store = match load_decisions(&resolved.project_dir) {
            Ok(s) => s,
            Err(e) => return ToolOutput::error(format!("Failed to load decisions: {e}")),
        };

        fn to_entry(d: &crate::decisions::Decision) -> DecisionEntry {
            DecisionEntry {
                id: d.id,
                title: d.title.clone(),
                description: d.description.clone(),
                status: d.status.to_string(),
                confidence: d.confidence,
                created_at: d.created_at.to_rfc3339(),
                updated_at: d.updated_at.to_rfc3339(),
                session_id: d.session_id.clone(),
                superseded_by: d.superseded_by,
                tags: d.tags.clone(),
                references: d.references.clone(),
                resurface_when: d.resurface_when.clone(),
                expires_when: d.expires_when.clone(),
            }
        }

        match request.operation.as_str() {
            "list" => {
                let mut decisions: Vec<&crate::decisions::Decision> = store.decisions.iter().collect();

                // Filter by status if specified
                if let Some(ref status_str) = request.status {
                    match DecisionStatus::parse(status_str) {
                        Some(status) => decisions.retain(|d| d.status == status),
                        None => {
                            return ToolOutput::error(format!(
                                "Invalid status '{status_str}'. Use: proposed, confirmed, superseded, abandoned"
                            ))
                        }
                    }
                }

                // Filter by tag if specified
                if let Some(ref tags_str) = request.tags {
                    let tag = tags_str.trim();
                    decisions.retain(|d| d.tags.iter().any(|t| t.contains(tag)));
                }

                let entries: Vec<DecisionEntry> = decisions.iter().map(|d| to_entry(d)).collect();

                let response = ManageDecisionsResponse {
                    operation: "list".into(),
                    project_path: resolved.project_path,
                    storage_provider: explicit_scope.then(|| "claude-code".into()),
                    message: Some(format!("{} decision(s)", entries.len())),
                    decisions: Some(entries),
                    decision: None,
                };

                match ToolOutput::json(&response) {
                    Ok(output) => output,
                    Err(e) => ToolOutput::error(format!("JSON error: {e}")),
                }
            }

            "add" => {
                let title = match request.title {
                    Some(t) if !t.trim().is_empty() => t,
                    _ => return ToolOutput::error("'title' is required for add operation"),
                };

                let tags: Vec<String> = request
                    .tags
                    .as_deref()
                    .map(|t| t.split(',').map(|s| s.trim().to_string()).collect())
                    .unwrap_or_default();

                let id = store.add_decision(
                    title,
                    request.description,
                    request.session_id,
                    request.confidence,
                    tags,
                );

                // Apply status if specified
                if let Some(ref status_str) = request.status {
                    match DecisionStatus::parse(status_str) {
                        Some(status) => {
                            store.update_decision(
                                id,
                                DecisionUpdate {
                                    status: Some(status),
                                    ..Default::default()
                                },
                            );
                        }
                        None => {
                            return ToolOutput::error(format!(
                                "Invalid status '{status_str}'. Use: proposed, confirmed, superseded, abandoned"
                            ))
                        }
                    }
                }
                if request.resurface_when.is_some() || request.expires_when.is_some() {
                    store.set_decision_schedule(id, request.resurface_when, request.expires_when);
                }

                if let Err(e) = save_decisions(&resolved.project_dir, &store) {
                    return ToolOutput::error(format!("Failed to save decisions: {e}"));
                }

                let decision = store.decisions.iter().find(|d| d.id == id).unwrap();
                let response = ManageDecisionsResponse {
                    operation: "add".into(),
                    project_path: resolved.project_path,
                    storage_provider: explicit_scope.then(|| "claude-code".into()),
                    message: Some(format!("Added decision #{id}")),
                    decisions: None,
                    decision: Some(to_entry(decision)),
                };

                match ToolOutput::json(&response) {
                    Ok(output) => output,
                    Err(e) => ToolOutput::error(format!("JSON error: {e}")),
                }
            }

            "update" => {
                let id = match request.id {
                    Some(id) => id,
                    None => return ToolOutput::error("'id' is required for update operation"),
                };

                let status = match request.status.as_deref() {
                    Some(s) => match DecisionStatus::parse(s) {
                        Some(status) => Some(status),
                        None => {
                            return ToolOutput::error(format!(
                                "Invalid status '{s}'. Use: proposed, confirmed, superseded, abandoned"
                            ))
                        }
                    },
                    None => None,
                };

                if request.title.as_deref().is_some_and(|t| t.trim().is_empty()) {
                    return ToolOutput::error("'title' cannot be empty");
                }

                let tags: Option<Vec<String>> = request
                    .tags
                    .as_deref()
                    .map(|t| t.split(',').map(|s| s.trim().to_string()).collect());

                let update = DecisionUpdate {
                    status,
                    title: request.title,
                    description: request.description,
                    confidence: request.confidence,
                    tags,
                    session_id: request.session_id,
                };

                if update.is_empty()
                    && request.resurface_when.is_none()
                    && request.expires_when.is_none()
                {
                    return ToolOutput::error(
                        "At least one of 'title', 'status', 'description', 'confidence', 'tags', 'session_id', 'resurface_when', or 'expires_when' is required for update",
                    );
                }

                if !store.update_decision(id, update) {
                    return ToolOutput::error(format!("Decision #{id} not found"));
                }
                if request.resurface_when.is_some() || request.expires_when.is_some() {
                    store.set_decision_schedule(id, request.resurface_when, request.expires_when);
                }

                if let Err(e) = save_decisions(&resolved.project_dir, &store) {
                    return ToolOutput::error(format!("Failed to save decisions: {e}"));
                }

                let decision = store.decisions.iter().find(|d| d.id == id).unwrap();
                let response = ManageDecisionsResponse {
                    operation: "update".into(),
                    project_path: resolved.project_path,
                    storage_provider: explicit_scope.then(|| "claude-code".into()),
                    message: Some(format!("Updated decision #{id}")),
                    decisions: None,
                    decision: Some(to_entry(decision)),
                };

                match ToolOutput::json(&response) {
                    Ok(output) => output,
                    Err(e) => ToolOutput::error(format!("JSON error: {e}")),
                }
            }

            "remove" => {
                let id = match request.id {
                    Some(id) => id,
                    None => return ToolOutput::error("'id' is required for remove operation"),
                };

                if !store.remove_decision(id) {
                    return ToolOutput::error(format!("Decision #{id} not found"));
                }

                if let Err(e) = save_decisions(&resolved.project_dir, &store) {
                    return ToolOutput::error(format!("Failed to save decisions: {e}"));
                }

                let response = ManageDecisionsResponse {
                    operation: "remove".into(),
                    project_path: resolved.project_path,
                    storage_provider: explicit_scope.then(|| "claude-code".into()),
                    message: Some(format!("Removed decision #{id}")),
                    decisions: None,
                    decision: None,
                };

                match ToolOutput::json(&response) {
                    Ok(output) => output,
                    Err(e) => ToolOutput::error(format!("JSON error: {e}")),
                }
            }

            "supersede" => {
                let id = match request.id {
                    Some(id) => id,
                    None => return ToolOutput::error("'id' is required for supersede operation"),
                };
                let by = match request.superseded_by {
                    Some(by) => by,
                    None => {
                        return ToolOutput::error(
                            "'superseded_by' is required for supersede operation",
                        )
                    }
                };

                if !store.supersede_decision(id, by) {
                    return ToolOutput::error(format!("Decision #{id} or #{by} not found"));
                }

                if let Err(e) = save_decisions(&resolved.project_dir, &store) {
                    return ToolOutput::error(format!("Failed to save decisions: {e}"));
                }

                let decision = store.decisions.iter().find(|d| d.id == id).unwrap();
                let response = ManageDecisionsResponse {
                    operation: "supersede".into(),
                    project_path: resolved.project_path,
                    storage_provider: explicit_scope.then(|| "claude-code".into()),
                    message: Some(format!("Decision #{id} superseded by #{by}")),
                    decisions: None,
                    decision: Some(to_entry(decision)),
                };

                match ToolOutput::json(&response) {
                    Ok(output) => output,
                    Err(e) => ToolOutput::error(format!("JSON error: {e}")),
                }
            }

            other => ToolOutput::error(format!(
                "Unknown operation '{other}'. Use: list, add, update, remove, supersede. For auto-scoring use CLI: snatch decisions score -p <project>"
            )),
        }
    }

    /// Look up which sessions modified a file. Returns file modification history
    /// from file-history-snapshot entries — the reverse index from file path to sessions.
    #[tool(
        description = "Look up which sessions modified a file path. Uses file-history-snapshot entries to build a reverse index. Returns session IDs, timestamps, and version numbers for each modification. Use to answer 'when was this file changed?' or 'which session introduced this code?'"
    )]
    async fn get_file_history(&self, request: GetFileHistoryRequest) -> ToolOutput {
        use crate::file_index::FileIndex;

        let claude_dir = match self.get_claude_dir() {
            Ok(dir) => dir,
            Err(e) => return ToolOutput::error(e),
        };

        let projects = match claude_dir.projects() {
            Ok(p) => p,
            Err(e) => return ToolOutput::error(format!("Failed to list projects: {e}")),
        };

        let mut sessions = Vec::new();
        for project in &projects {
            if let Some(ref filter) = request.project {
                if !project.best_path().contains(filter) {
                    continue;
                }
            }
            if let Ok(s) = project.sessions() {
                sessions.extend(s);
            }
        }

        let index = FileIndex::from_sessions(&sessions, self.max_file_size);
        let mut matches = index.search(&request.path);
        matches.sort_by_key(|(path, _)| path.to_string());

        let limit = request.limit.unwrap_or(50);
        let total_files = matches.len();
        let total_modifications: usize = matches.iter().map(|(_, m)| m.len()).sum();

        let mut modifications = Vec::new();
        for (path, mods) in &matches {
            for m in *mods {
                if modifications.len() >= limit {
                    break;
                }
                modifications.push(FileModificationEntry {
                    file_path: path.to_string(),
                    session_id: m.session_id.clone(),
                    project_path: m.project_path.clone(),
                    message_id: m.message_id.clone(),
                    timestamp: m.timestamp.to_rfc3339(),
                    version: m.version,
                });
            }
            if modifications.len() >= limit {
                break;
            }
        }

        let returned = modifications.len();
        let response = GetFileHistoryResponse {
            path_query: request.path,
            total_files,
            total_modifications,
            returned,
            modifications,
        };

        match ToolOutput::json(&response) {
            Ok(output) => output,
            Err(e) => ToolOutput::error(format!("JSON error: {e}")),
        }
    }

    /// Cross-session topic threading: search for a pattern across sessions and
    /// return chronologically-ordered exchanges with conversation context.
    #[tool(
        description = "Cross-session topic threading over Claude Code by default or explicit provider selections (provider=['all'] for a union; qualified ids route directly). Returns a chronological thread with match_provenance. When limited, primary prose is selected before quoted or harness-injected text, but secondary-only evidence is retained. Use decisions_only for decision points and include_thinking for persisted reasoning."
    )]
    async fn thread_topic(&self, request: ThreadTopicRequest) -> ToolOutput {
        use crate::analysis::threading::{thread_topic, ThreadParams};

        let pattern = &request.pattern;
        let ignore_case = true;
        let regex = match regex::RegexBuilder::new(pattern)
            .case_insensitive(ignore_case)
            .build()
        {
            Ok(r) => r,
            Err(e) => return ToolOutput::error(format!("Invalid regex pattern: {e}")),
        };

        let provider_flags = request.provider.as_deref().unwrap_or(&[]);
        let registry = self.provider_registry();
        let provider_route = !provider_flags.is_empty()
            || request
                .session_id
                .as_deref()
                .is_some_and(|session_id| registry.looks_qualified(session_id));
        if provider_route {
            let max_context = request.max_context.unwrap_or(500);
            let params = ThreadParams {
                include_thinking: request.include_thinking.unwrap_or(false),
                limit: request.limit.unwrap_or(30),
                max_user_context: max_context,
                max_assistant_context: max_context,
                max_thinking_context: max_context,
                role_filter: None,
                decisions_only: request.decisions_only.unwrap_or(false),
            };
            return match self.provider_thread_result(&request, &regex, &params) {
                Ok((result, skipped, warnings)) => {
                    Self::render_thread_result(&request, result, skipped, warnings)
                }
                Err(error) => error,
            };
        }

        let claude_dir = match self.get_claude_dir() {
            Ok(dir) => dir,
            Err(e) => return ToolOutput::error(e),
        };

        // Collect sessions with filters
        let mut sessions = if let Some(ref session_id) = request.session_id {
            match claude_dir.find_session(session_id) {
                Ok(Some(s)) => vec![s],
                Ok(None) => return ToolOutput::error(format!("Session not found: {session_id}")),
                Err(e) => return ToolOutput::error(format!("Failed to find session: {e}")),
            }
        } else {
            let mut all = match claude_dir.all_sessions() {
                Ok(s) => s,
                Err(e) => return ToolOutput::error(format!("Failed to list sessions: {e}")),
            };
            if let Some(ref project) = request.project {
                all.retain(|s| s.project_path().contains(project));
            }
            all
        };

        if request.no_subagents.unwrap_or(true) {
            sessions.retain(|s| !s.is_subagent());
        }

        // Apply date filters
        if request.since.is_some() || request.until.is_some() {
            use crate::cli::helpers::filter_sessions_by_date;
            if let Err(e) = filter_sessions_by_date(
                &mut sessions,
                request.since.as_deref(),
                request.until.as_deref(),
            ) {
                return ToolOutput::error(format!("Date filter error: {e}"));
            }
        }

        let max_context = request.max_context.unwrap_or(500);
        let params = ThreadParams {
            include_thinking: request.include_thinking.unwrap_or(false),
            limit: request.limit.unwrap_or(30),
            max_user_context: max_context,
            max_assistant_context: max_context,
            max_thinking_context: max_context,
            role_filter: None,
            decisions_only: request.decisions_only.unwrap_or(false),
        };

        let result = thread_topic(&sessions, &regex, &params, self.max_file_size);

        Self::render_thread_result(&request, result, Vec::new(), Vec::new())
    }

    // ========================================================================
    // New Tool: get_project_health
    // ========================================================================

    /// Project health dashboard: hotspot files, rework, error trends, decision stability.
    #[tool(
        description = "Project health dashboard. Shows hotspot files (most edits), rework files (edited across multiple sessions), decision stability metrics, and per-session error/tool counts. Answers 'which parts of the codebase cause the most trouble?' and 'are we improving?'"
    )]
    async fn get_project_health(&self, request: GetProjectHealthRequest) -> ToolOutput {
        use crate::analysis::project_health::{analyze_project_health, ProjectHealthParams};

        let claude_dir = match self.get_claude_dir() {
            Ok(dir) => dir,
            Err(e) => return ToolOutput::error(e),
        };

        let period = request.period.as_deref().unwrap_or("7d");
        let cutoff = match period_cutoff(period) {
            Ok(c) => c,
            Err(e) => return ToolOutput::error(format!("Invalid period: {e}")),
        };

        let mut sessions = match claude_dir.all_sessions() {
            Ok(s) => s,
            Err(e) => return ToolOutput::error(format!("Failed to list sessions: {e}")),
        };

        sessions.retain(|s| s.project_path().contains(request.project.as_str()));

        if request.no_subagents.unwrap_or(true) {
            sessions.retain(|s| !s.is_subagent());
        }

        if let Some(cutoff_dt) = cutoff {
            let cutoff_systime = std::time::SystemTime::from(cutoff_dt);
            sessions.retain(|s| s.modified_time() >= cutoff_systime);
        }

        // Try to load decision store for this project
        let projects = claude_dir.projects().unwrap_or_default();
        let decision_store = projects
            .iter()
            .find(|p| {
                p.path()
                    .to_string_lossy()
                    .contains(request.project.as_str())
            })
            .and_then(|proj| crate::decisions::load_decisions(proj.path()).ok());

        let params = ProjectHealthParams {
            max_hotspots: request.max_hotspots.unwrap_or(20),
        };

        let result = analyze_project_health(
            &sessions,
            decision_store.as_ref(),
            &params,
            self.max_file_size,
        );

        let response = GetProjectHealthResponse {
            project_path: request.project,
            period: period.to_string(),
            sessions_analyzed: result.sessions_analyzed,
            total_tool_failures: result.total_errors,
            total_tool_calls: result.total_tool_calls,
            hotspot_files: result
                .hotspot_files
                .into_iter()
                .map(|f| HotspotFileEntry {
                    path: f.path,
                    edit_count: f.edit_count,
                    session_count: f.session_count,
                })
                .collect(),
            rework_files: result
                .rework_files
                .into_iter()
                .map(|f| ReworkFileEntry {
                    path: f.path,
                    version_count: f.version_count,
                    session_count: f.session_count,
                })
                .collect(),
            decision_churn: result.decision_churn.map(|dc| DecisionChurnEntry {
                total_decisions: dc.total_decisions,
                confirmed_count: dc.confirmed_count,
                superseded_count: dc.superseded_count,
                abandoned_count: dc.abandoned_count,
                proposed_count: dc.proposed_count,
            }),
            session_stats: result
                .session_stats
                .into_iter()
                .map(|s| SessionHealthEntry {
                    session_id: s.session_id,
                    timestamp: s.timestamp,
                    tool_failure_count: s.error_count,
                    tool_count: s.tool_count,
                })
                .collect(),
        };

        match ToolOutput::json(&response) {
            Ok(output) => output,
            Err(e) => ToolOutput::error(format!("JSON error: {e}")),
        }
    }

    // ========================================================================
    // New Tool: suggest_priorities
    // ========================================================================

    /// Suggest what to work on next based on project data.
    #[tool(
        description = "Suggest priorities based on project data: recurring errors (reliability issues), high-churn files (stability concerns), open goals (committed work), and proposed decisions (unresolved uncertainty). Returns ranked items with evidence. Use at session start or when deciding what to tackle next."
    )]
    async fn suggest_priorities(&self, request: SuggestPrioritiesRequest) -> ToolOutput {
        use crate::analysis::priorities::{
            suggest_priorities as analyze_priorities, PriorityParams,
        };

        let claude_dir = match self.get_claude_dir() {
            Ok(dir) => dir,
            Err(e) => return ToolOutput::error(e),
        };

        let period = request.period.as_deref().unwrap_or("7d");
        let cutoff = match period_cutoff(period) {
            Ok(c) => c,
            Err(e) => return ToolOutput::error(format!("Invalid period: {e}")),
        };

        let mut sessions = match claude_dir.all_sessions() {
            Ok(s) => s,
            Err(e) => return ToolOutput::error(format!("Failed to list sessions: {e}")),
        };

        sessions.retain(|s| s.project_path().contains(request.project.as_str()));

        if request.no_subagents.unwrap_or(true) {
            sessions.retain(|s| !s.is_subagent());
        }

        if let Some(cutoff_dt) = cutoff {
            let cutoff_systime = std::time::SystemTime::from(cutoff_dt);
            sessions.retain(|s| s.modified_time() >= cutoff_systime);
        }

        // Load decision and goal stores
        let projects = claude_dir.projects().unwrap_or_default();
        let project_dir = projects.iter().find(|p| {
            p.path()
                .to_string_lossy()
                .contains(request.project.as_str())
        });

        let decision_store =
            project_dir.and_then(|proj| crate::decisions::load_decisions(proj.path()).ok());
        let goal_store = project_dir.and_then(|proj| crate::goals::load_goals(proj.path()).ok());

        let params = PriorityParams {
            max_priorities: request.max_priorities.unwrap_or(10),
            ..Default::default()
        };

        let result = analyze_priorities(
            &sessions,
            decision_store.as_ref(),
            goal_store.as_ref(),
            &params,
            self.max_file_size,
        );

        let response = SuggestPrioritiesResponse {
            project_path: request.project,
            period: period.to_string(),
            sessions_analyzed: result.sessions_analyzed,
            total_tool_failures: result.total_errors,
            open_goals: result.open_goals,
            proposed_decisions: result.proposed_decisions,
            priorities: result
                .priorities
                .into_iter()
                .map(|p| PriorityItemEntry {
                    rank: p.rank,
                    category: p.category,
                    summary: p.summary,
                    score: p.score,
                    sources: p
                        .sources
                        .into_iter()
                        .map(|s| {
                            let (source_type, detail) = match &s {
                                crate::analysis::priorities::PrioritySource::RecurringError {
                                    ..
                                } => ("error", s.to_string()),
                                crate::analysis::priorities::PrioritySource::FileChurn {
                                    ..
                                } => ("churn", s.to_string()),
                                crate::analysis::priorities::PrioritySource::OpenGoal {
                                    ..
                                } => ("goal", s.to_string()),
                                crate::analysis::priorities::PrioritySource::ProposedDecision {
                                    ..
                                } => ("decision", s.to_string()),
                            };
                            PrioritySourceEntry {
                                source_type: source_type.to_string(),
                                detail,
                            }
                        })
                        .collect(),
                })
                .collect(),
        };

        match ToolOutput::json(&response) {
            Ok(output) => output,
            Err(e) => ToolOutput::error(format!("JSON error: {e}")),
        }
    }

    // ========================================================================
    // New Tool: explain_file_evolution
    // ========================================================================

    /// Explain why a file changed over time.
    #[tool(
        description = "Explain how and why a file evolved across sessions. For each modification, shows the user prompt that triggered it, the assistant's response, thinking/rationale (if available), and tools used. Answers 'why did this file end up this way?' by combining file history with conversation context. Returns chronologically ordered change events."
    )]
    async fn explain_file_evolution(&self, request: ExplainFileEvolutionRequest) -> ToolOutput {
        use crate::analysis::file_evolution::{analyze_file_evolution, FileEvolutionParams};

        let claude_dir = match self.get_claude_dir() {
            Ok(dir) => dir,
            Err(e) => return ToolOutput::error(e),
        };

        let period = request.period.as_deref().unwrap_or("30d");
        let cutoff = match period_cutoff(period) {
            Ok(c) => c,
            Err(e) => return ToolOutput::error(format!("Invalid period: {e}")),
        };

        let mut sessions = match claude_dir.all_sessions() {
            Ok(s) => s,
            Err(e) => return ToolOutput::error(format!("Failed to list sessions: {e}")),
        };

        sessions.retain(|s| s.project_path().contains(request.project.as_str()));

        if request.no_subagents.unwrap_or(true) {
            sessions.retain(|s| !s.is_subagent());
        }

        if let Some(cutoff_dt) = cutoff {
            let cutoff_systime = std::time::SystemTime::from(cutoff_dt);
            sessions.retain(|s| s.modified_time() >= cutoff_systime);
        }

        let params = FileEvolutionParams {
            file_pattern: request.file_pattern.clone(),
            limit: request.limit.unwrap_or(30),
            max_text_len: 500,
            include_thinking: request.include_thinking.unwrap_or(true),
            context_window: request.context_window.unwrap_or(1),
        };

        let results = analyze_file_evolution(&sessions, &params, self.max_file_size);

        let response = ExplainFileEvolutionResponse {
            project_path: request.project,
            file_pattern: request.file_pattern,
            period: period.to_string(),
            files: results
                .into_iter()
                .map(|r| FileEvolutionEntry {
                    file_path: r.file_path,
                    total_changes: r.total_changes,
                    sessions_involved: r.sessions_involved,
                    changes: r
                        .changes
                        .into_iter()
                        .map(|c| ChangeEventEntry {
                            timestamp: c.timestamp.to_rfc3339(),
                            session_id: c.session_id,
                            message_id: c.message_id,
                            version: c.version,
                            user_prompt: c.user_prompt,
                            assistant_response: c.assistant_response,
                            thinking: c.thinking,
                            tools_used: c.tools_used,
                            had_errors: c.had_errors,
                        })
                        .collect(),
                })
                .collect(),
        };

        match ToolOutput::json(&response) {
            Ok(output) => output,
            Err(e) => ToolOutput::error(format!("JSON error: {e}")),
        }
    }

    // ========================================================================
    // New Tool: get_event_context
    // ========================================================================

    /// Get contextual zoom around a specific event in a session.
    #[tool(
        description = "Get conversation context around a specific message or timestamp in a session. Returns the target event plus surrounding turns (user prompts, assistant responses, tools, errors). Use to understand 'what was happening around this event?' after finding events via other tools. Provide either message_id or timestamp."
    )]
    async fn get_event_context(&self, request: GetEventContextRequest) -> ToolOutput {
        use crate::analysis::event_context::{find_event_context, EventContextParams};

        if request.message_id.is_none() && request.timestamp.is_none() {
            return ToolOutput::error("Either message_id or timestamp is required");
        }

        let chain_aware = request.chain_aware.unwrap_or(true);
        let resolved = match resolve_session_with_chain(self, &request.session_id, chain_aware) {
            Ok(r) => r,
            Err(e) => return e,
        };

        let entries = resolved.conversation.main_thread_entries();
        let entry_refs: Vec<&LogEntry> = entries.clone();

        let timestamp = if let Some(ref ts) = request.timestamp {
            match parse_timestamp_param(ts) {
                Ok(dt) => Some(dt),
                Err(e) => return ToolOutput::error(format!("Invalid timestamp: {e}")),
            }
        } else {
            None
        };

        let params = EventContextParams {
            message_id: request.message_id,
            timestamp,
            context_window: request.context_window.unwrap_or(2),
            max_text_len: 500,
        };

        let result = match find_event_context(&entry_refs, &params) {
            Some(r) => r,
            None => return ToolOutput::error("Event not found in session"),
        };

        let to_entry = |t: crate::analysis::event_context::ContextTurn| -> ContextTurnEntry {
            ContextTurnEntry {
                index: t.index,
                msg_type: t.message_type,
                uuid: t.uuid,
                timestamp: t.timestamp.map(|ts| ts.to_rfc3339()),
                text: t.text,
                tools: t.tools,
                had_errors: t.had_errors,
            }
        };

        let response = GetEventContextResponse {
            session_id: resolved.session_id,
            target_index: result.target_index,
            target: to_entry(result.target),
            before: result.before.into_iter().map(to_entry).collect(),
            after: result.after.into_iter().map(to_entry).collect(),
            related_files: result.related_files,
            error_count: result.error_count,
        };

        match ToolOutput::json(&response) {
            Ok(output) => output,
            Err(e) => ToolOutput::error(format!("JSON error: {e}")),
        }
    }
}

/// Runtime configuration for the MCP server.
///
/// `max_concurrent_requests` is pinned to 1 so the server processes requests
/// sequentially. mcpkit 0.6 defaults to 100 concurrent requests, but the
/// persistence tools (`manage_goals`/`manage_notes`/`manage_decisions`) do an
/// unsynchronized load -> mutate -> save with `next_id` allocation and hold no
/// lock across it; two concurrent mutations would race and lose an update or
/// allocate duplicate IDs. Serializing restores mcpkit 0.5's sequential
/// behavior, which this local single-client server relied on.
fn serialized_runtime_config() -> mcpkit::server::RuntimeConfig {
    mcpkit::server::RuntimeConfig {
        max_concurrent_requests: 1,
        ..mcpkit::server::RuntimeConfig::default()
    }
}

/// Run the MCP server.
pub async fn run_server(
    claude_dir: Option<PathBuf>,
    max_file_size: Option<u64>,
) -> crate::error::Result<()> {
    let server = SnatchServer::new(claude_dir, max_file_size);
    let transport = StdioTransport::new();

    mcpkit::server::ServerRuntime::with_config(
        server.into_server(),
        transport,
        serialized_runtime_config(),
    )
    .run()
    .await
    .map_err(|e| crate::error::SnatchError::ExportError {
        message: format!("MCP server error: {e}"),
        source: None,
    })
}

/// A subagent's rendered output, attached to its spawning Agent/Task call.
struct RenderedSubagent {
    session_id: String,
    result_preview: Option<String>,
    transcript: Option<Vec<MessageEntry>>,
}

/// Result of resolving subagents for a messages response: the confident joins
/// (keyed by spawning tool_use id, attached inline) plus any subagents present
/// on disk that could not be joined (surfaced separately so they never silently
/// vanish — the same fix the CLI `messages` renderer carries).
#[derive(Default)]
struct ResolvedSubagents {
    matched: HashMap<String, RenderedSubagent>,
    unmatched: Vec<UnmatchedSubagent>,
}

/// Match each `Agent`/`Task` call in `ordered_entries` to the subagent it spawned
/// (via the shared conservative join) and render it for the messages response,
/// keyed by the spawning tool_use id. The full transcript is built only when
/// `include_transcripts` is set.
fn resolve_subagent_renders(
    session: &Session,
    ordered_entries: &[&LogEntry],
    include_transcripts: bool,
    include_thinking: bool,
    max_file_size: Option<u64>,
) -> ResolvedSubagents {
    let matches =
        crate::analysis::subagents::match_subagents(session, ordered_entries, max_file_size);
    let matched = matches
        .matched
        .into_iter()
        .map(|(id, m)| {
            let transcript = if include_transcripts {
                let entries = Session::from_path(&m.path, session.project_path())
                    .ok()
                    .and_then(|s| s.parse_with_options(max_file_size).ok())
                    .unwrap_or_default();
                let conversation = Conversation::from_entries(entries).ok();
                let main: Vec<&LogEntry> = conversation
                    .as_ref()
                    .map(Conversation::main_thread_entries)
                    .unwrap_or_default();
                Some(render_subagent_transcript(&main, include_thinking))
            } else {
                None
            };
            (
                id,
                RenderedSubagent {
                    session_id: m.session_id,
                    result_preview: m.result_preview,
                    transcript,
                },
            )
        })
        .collect();
    let unmatched = matches
        .unmatched
        .into_iter()
        .map(|m| UnmatchedSubagent {
            session_id: m.session_id,
            message_count: m.message_count,
            result_preview: m.result_preview,
        })
        .collect();
    ResolvedSubagents { matched, unmatched }
}

/// Render a subagent's main thread as message entries (standard detail: user and
/// assistant text plus tool names; tool details are not expanded recursively).
fn render_subagent_transcript(entries: &[&LogEntry], include_thinking: bool) -> Vec<MessageEntry> {
    entries
        .iter()
        .enumerate()
        .map(|(i, entry)| {
            let content = match entry {
                LogEntry::User(_) => {
                    extract_user_prompt_text(entry).map(|t| truncate_text(&t, 500))
                }
                LogEntry::Assistant(_) => extract_assistant_summary(entry, 500),
                LogEntry::System(sys) => sys.content.clone(),
                _ => None,
            };
            let tool_names = extract_tool_names(entry);
            let thinking = if include_thinking {
                extract_thinking_text(entry, 1000)
            } else {
                None
            };
            MessageEntry {
                index: i,
                msg_type: entry.message_type().to_string(),
                timestamp: entry.timestamp().map(|t| t.to_rfc3339()),
                content,
                git_branch: entry.git_branch().map(String::from),
                model: get_model(entry),
                tool_calls: if tool_names.is_empty() {
                    None
                } else {
                    Some(tool_names)
                },
                tool_details: None,
                has_thinking: if has_thinking(entry) {
                    Some(true)
                } else {
                    None
                },
                thinking_preview: thinking,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::discovery::encode_project_path;
    use tempfile::TempDir;

    const PROJECT_PATH: &str = "/home/user/test-project";

    #[test]
    fn serialized_runtime_config_processes_one_request_at_a_time() {
        // Regression guard for the mcpkit 0.6 concurrency race: the persistence
        // tools (manage_goals/notes/decisions) do an unsynchronized
        // load -> mutate -> save with next_id allocation and hold no lock across
        // it. The server must therefore process requests sequentially; if this
        // is ever reverted to mcpkit's default of 100, concurrent mutations can
        // lose updates or allocate duplicate IDs.
        assert_eq!(serialized_runtime_config().max_concurrent_requests, 1);
    }

    #[test]
    fn test_resolve_subagent_renders_description_fallback_and_ambiguity() {
        // Three Agent calls in one assistant turn: a unique description and two
        // sharing a description. Sidecars carry no toolUseId, forcing the
        // description fallback. The unique one attaches; the ambiguous pair does not.
        let tmp = tempfile::tempdir().unwrap();
        let project = tmp.path();
        let uuid = "85a67f74-54a8-49dd-89c1-b5e0c47ab3a7";

        let main_jsonl = format!(
            r#"{{"type":"assistant","uuid":"a1","parentUuid":null,"timestamp":"2026-06-09T18:00:00Z","sessionId":"{uuid}","version":"2.1.0","isSidechain":false,"message":{{"id":"m1","type":"message","role":"assistant","model":"claude","content":[{{"type":"tool_use","id":"toolu_AAA","name":"Agent","input":{{"description":"Review X","subagent_type":"Explore","prompt":"p"}}}},{{"type":"tool_use","id":"toolu_BBB","name":"Agent","input":{{"description":"Dup desc","subagent_type":"Explore","prompt":"p"}}}},{{"type":"tool_use","id":"toolu_CCC","name":"Agent","input":{{"description":"Dup desc","subagent_type":"Explore","prompt":"p"}}}}]}}}}"#
        );
        let main_path = project.join(format!("{uuid}.jsonl"));
        std::fs::write(&main_path, format!("{main_jsonl}\n")).unwrap();

        let subagents = project.join(uuid).join("subagents");
        std::fs::create_dir_all(&subagents).unwrap();
        let agent_body = |sid: &str| {
            format!(
                "{{\"type\":\"assistant\",\"uuid\":\"{sid}\",\"parentUuid\":null,\"timestamp\":\"2026-06-09T18:01:00Z\",\"sessionId\":\"{uuid}\",\"version\":\"2.1.0\",\"isSidechain\":true,\"message\":{{\"id\":\"{sid}m\",\"type\":\"message\",\"role\":\"assistant\",\"model\":\"claude\",\"content\":[{{\"type\":\"text\",\"text\":\"found\"}}]}}}}\n"
            )
        };
        for (file, desc) in [
            ("agent-x", "Review X"),
            ("agent-y", "Dup desc"),
            ("agent-z", "Dup desc"),
        ] {
            std::fs::write(subagents.join(format!("{file}.jsonl")), agent_body(file)).unwrap();
            std::fs::write(
                subagents.join(format!("{file}.meta.json")),
                format!(r#"{{"agentType":"Explore","description":"{desc}"}}"#),
            )
            .unwrap();
        }

        let session = Session::from_path(&main_path, PROJECT_PATH).unwrap();
        let entries = session.parse().unwrap();
        let refs: Vec<&LogEntry> = entries.iter().collect();
        let out = resolve_subagent_renders(&session, &refs, false, false, None);

        // Unique description attaches; ambiguous pair is left unattached.
        assert_eq!(
            out.matched.get("toolu_AAA").map(|r| r.session_id.as_str()),
            Some("agent-x")
        );
        assert!(!out.matched.contains_key("toolu_BBB"));
        assert!(!out.matched.contains_key("toolu_CCC"));

        // The ambiguous pair is surfaced as unmatched rather than silently
        // dropped — the bug this fix closes on the MCP surface.
        let mut unmatched_ids: Vec<&str> = out
            .unmatched
            .iter()
            .map(|u| u.session_id.as_str())
            .collect();
        unmatched_ids.sort_unstable();
        assert_eq!(unmatched_ids, vec!["agent-y", "agent-z"]);
    }

    fn setup_claude_dir(session_id: &str, project_path: &str, jsonl: &str) -> TempDir {
        let tmp = TempDir::new().expect("failed to create temp dir");
        let encoded = encode_project_path(project_path);
        let project_dir = tmp.path().join("projects").join(&encoded);
        std::fs::create_dir_all(&project_dir).unwrap();
        std::fs::write(project_dir.join(format!("{session_id}.jsonl")), jsonl).unwrap();
        tmp
    }

    fn minimal_session_jsonl(session_id: &str) -> String {
        let user_line = format!(
            r#"{{"type":"user","uuid":"11111111-1111-1111-1111-111111111111","parentUuid":null,"timestamp":"2025-01-15T10:00:00.000Z","sessionId":"{session_id}","version":"2.0.74","message":{{"role":"user","content":"Hello, Claude!"}}}}"#
        );
        let assistant_line = format!(
            r#"{{"type":"assistant","uuid":"22222222-2222-2222-2222-222222222222","parentUuid":"11111111-1111-1111-1111-111111111111","timestamp":"2025-01-15T10:00:01.000Z","sessionId":"{session_id}","version":"2.0.74","message":{{"id":"msg_001","type":"message","role":"assistant","content":[{{"type":"text","text":"Hello! How can I help you today?"}}],"model":"claude-sonnet-4-20250514","stop_reason":"end_turn","usage":{{"input_tokens":10,"output_tokens":15,"cache_creation_input_tokens":0,"cache_read_input_tokens":0}}}}}}"#
        );
        format!("{user_line}\n{assistant_line}\n")
    }

    fn unwrap_output(output: ToolOutput) -> String {
        match output {
            ToolOutput::Success(result) => result
                .content
                .iter()
                .filter_map(|c| c.as_text())
                .collect::<Vec<_>>()
                .join("\n"),
            ToolOutput::RecoverableError { message, .. } => {
                panic!("Expected success but got error: {message}");
            }
        }
    }

    fn assert_error(output: ToolOutput) {
        assert!(
            matches!(output, ToolOutput::RecoverableError { .. }),
            "Expected error but got success"
        );
    }

    /// A 9-entry linear session engineered so the canonical message total
    /// (`main_thread_message_total`) diverges from the detail-filtered
    /// pagination population in BOTH directions:
    ///   - 2 human prompts (U1, U2) + 4 distinct assistant turn ids
    ///     (msg_001, msg_002, msg_003, msg_004) => canonical total = 6.
    ///   - `standard` detail keeps all 9 entries => page_total 9 > total 6.
    ///   - `conversation` detail keeps only the 5 text/prompt entries
    ///     (msg_002 is tool-only via A2 but has a text sibling A3; msg_004 is
    ///     tool-only with no text sibling, so it is dropped) => page_total 5
    ///     < total 6 — the reviewer's "total > returned looks like more pages"
    ///     trap direction.
    fn pagination_fixture_jsonl(session_id: &str) -> String {
        const LINES: &str = concat!(
            r#"{"type":"user","uuid":"bbbbbbbb-0000-0000-0000-000000000001","parentUuid":null,"timestamp":"2025-02-01T10:00:00.000Z","sessionId":"__SID__","version":"2.0.74","message":{"role":"user","content":"First question"}}"#,
            "\n",
            r#"{"type":"assistant","uuid":"bbbbbbbb-0000-0000-0000-000000000002","parentUuid":"bbbbbbbb-0000-0000-0000-000000000001","timestamp":"2025-02-01T10:00:01.000Z","sessionId":"__SID__","version":"2.0.74","message":{"id":"msg_001","type":"message","role":"assistant","content":[{"type":"text","text":"Answer one"}],"model":"claude-sonnet-4-20250514","stop_reason":"end_turn","usage":{"input_tokens":10,"output_tokens":5,"cache_creation_input_tokens":0,"cache_read_input_tokens":0}}}"#,
            "\n",
            r#"{"type":"assistant","uuid":"bbbbbbbb-0000-0000-0000-000000000003","parentUuid":"bbbbbbbb-0000-0000-0000-000000000002","timestamp":"2025-02-01T10:00:02.000Z","sessionId":"__SID__","version":"2.0.74","message":{"id":"msg_002","type":"message","role":"assistant","content":[{"type":"tool_use","id":"toolu_a","name":"Bash","input":{"command":"ls"}}],"model":"claude-sonnet-4-20250514","stop_reason":"tool_use","usage":{"input_tokens":10,"output_tokens":5,"cache_creation_input_tokens":0,"cache_read_input_tokens":0}}}"#,
            "\n",
            r#"{"type":"user","uuid":"bbbbbbbb-0000-0000-0000-000000000004","parentUuid":"bbbbbbbb-0000-0000-0000-000000000003","timestamp":"2025-02-01T10:00:03.000Z","sessionId":"__SID__","version":"2.0.74","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"toolu_a","content":"ok"}]}}"#,
            "\n",
            r#"{"type":"assistant","uuid":"bbbbbbbb-0000-0000-0000-000000000005","parentUuid":"bbbbbbbb-0000-0000-0000-000000000004","timestamp":"2025-02-01T10:00:04.000Z","sessionId":"__SID__","version":"2.0.74","message":{"id":"msg_002","type":"message","role":"assistant","content":[{"type":"text","text":"Interpreted result"}],"model":"claude-sonnet-4-20250514","stop_reason":"end_turn","usage":{"input_tokens":10,"output_tokens":5,"cache_creation_input_tokens":0,"cache_read_input_tokens":0}}}"#,
            "\n",
            r#"{"type":"assistant","uuid":"bbbbbbbb-0000-0000-0000-000000000006","parentUuid":"bbbbbbbb-0000-0000-0000-000000000005","timestamp":"2025-02-01T10:00:05.000Z","sessionId":"__SID__","version":"2.0.74","message":{"id":"msg_004","type":"message","role":"assistant","content":[{"type":"tool_use","id":"toolu_b","name":"Read","input":{"file_path":"/x"}}],"model":"claude-sonnet-4-20250514","stop_reason":"tool_use","usage":{"input_tokens":10,"output_tokens":5,"cache_creation_input_tokens":0,"cache_read_input_tokens":0}}}"#,
            "\n",
            r#"{"type":"user","uuid":"bbbbbbbb-0000-0000-0000-000000000007","parentUuid":"bbbbbbbb-0000-0000-0000-000000000006","timestamp":"2025-02-01T10:00:06.000Z","sessionId":"__SID__","version":"2.0.74","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"toolu_b","content":"data"}]}}"#,
            "\n",
            r#"{"type":"user","uuid":"bbbbbbbb-0000-0000-0000-000000000008","parentUuid":"bbbbbbbb-0000-0000-0000-000000000007","timestamp":"2025-02-01T10:00:07.000Z","sessionId":"__SID__","version":"2.0.74","message":{"role":"user","content":"Second question"}}"#,
            "\n",
            r#"{"type":"assistant","uuid":"bbbbbbbb-0000-0000-0000-000000000009","parentUuid":"bbbbbbbb-0000-0000-0000-000000000008","timestamp":"2025-02-01T10:00:08.000Z","sessionId":"__SID__","version":"2.0.74","message":{"id":"msg_003","type":"message","role":"assistant","content":[{"type":"text","text":"Second answer"}],"model":"claude-sonnet-4-20250514","stop_reason":"end_turn","usage":{"input_tokens":10,"output_tokens":5,"cache_creation_input_tokens":0,"cache_read_input_tokens":0}}}"#,
            "\n",
        );
        LINES.replace("__SID__", session_id)
    }

    async fn page_value(
        server: &SnatchServer,
        id: &str,
        detail: &str,
        limit: Option<usize>,
        offset: Option<usize>,
    ) -> serde_json::Value {
        let out = unwrap_output(
            server
                .get_session_messages(GetSessionMessagesRequest {
                    session_id: id.to_string(),
                    provider: None,
                    detail: Some(detail.to_string()),
                    message_type: None,
                    limit,
                    offset,
                    reverse: None,
                    include_thinking: None,
                    chain_aware: None,
                    after_timestamp: None,
                    before_timestamp: None,
                    include_subagent_transcripts: None,
                    chunk: None,
                    errors_only: None,
                    max_text_len: None,
                })
                .await,
        );
        serde_json::from_str(&out).unwrap()
    }

    /// Regression guard for the pagination-signal fix: `total_messages` is the
    /// canonical, detail-independent count (equal to get_session_info.messages)
    /// and is NOT the pagination bound; `page_total`/`has_more` are. Neither the
    /// "total > returned looks like more pages" trap (conversation detail) nor
    /// its inverse (standard detail, where the paginable set exceeds total) may
    /// mislead a caller.
    #[tokio::test]
    async fn total_messages_is_canonical_and_page_total_drives_pagination() {
        let id = "bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb";
        let tmp = setup_claude_dir(id, PROJECT_PATH, &pagination_fixture_jsonl(id));
        let server = make_server(&tmp);

        // Canonical total is detail-independent and equals info.messages.
        let info: serde_json::Value = serde_json::from_str(&unwrap_output(
            server
                .get_session_info(GetSessionInfoRequest {
                    session_id: id.to_string(),
                    provider: None,
                })
                .await,
        ))
        .unwrap();
        assert_eq!(info["messages"], 6, "canonical count");
        for detail in ["overview", "conversation", "standard", "full"] {
            let page = page_value(&server, id, detail, Some(0), Some(0)).await;
            assert_eq!(
                page["total_messages"], 6,
                "total_messages must be detail-independent ({detail})"
            );
            assert_eq!(
                page["total_messages"], info["messages"],
                "total_messages must equal get_session_info.messages ({detail})"
            );
            // A full (unlimited) page from offset 0 reaches the end: no more.
            assert_eq!(
                page["has_more"], false,
                "unlimited page has_more ({detail})"
            );
        }

        // conversation: page_total (5) < total_messages (6) — the trap
        // direction. has_more must say "stop", not defer to total > returned.
        let conv = page_value(&server, id, "conversation", Some(0), Some(0)).await;
        assert_eq!(conv["page_total"], 5);
        assert_eq!(conv["returned"], 5);
        assert_eq!(conv["has_more"], false);

        // standard: page_total (9) > total_messages (6) — the inverse. returned
        // may exceed total_messages without meaning "past the end".
        let std = page_value(&server, id, "standard", Some(0), Some(0)).await;
        assert_eq!(std["page_total"], 9);
        assert_eq!(std["returned"], 9);
        assert_eq!(std["has_more"], false);

        // A partial page reports more remain; the last page does not.
        let partial = page_value(&server, id, "standard", Some(3), Some(0)).await;
        assert_eq!(partial["returned"], 3);
        assert_eq!(partial["has_more"], true);
        let last = page_value(&server, id, "standard", Some(3), Some(6)).await;
        assert_eq!(last["returned"], 3);
        assert_eq!(last["has_more"], false);

        // offset at/after page_total yields an empty, terminal page.
        let at_end = page_value(&server, id, "conversation", Some(0), Some(5)).await;
        assert_eq!(at_end["returned"], 0);
        assert_eq!(at_end["has_more"], false);
        let beyond = page_value(&server, id, "standard", Some(10), Some(99)).await;
        assert_eq!(beyond["returned"], 0);
        assert_eq!(beyond["has_more"], false);
    }

    #[tokio::test]
    async fn persistent_registries_are_explicitly_claude_storage_scoped() {
        let sid = "abababab-1111-2222-3333-444444444444";
        let tmp = setup_claude_dir(sid, PROJECT_PATH, &minimal_session_jsonl(sid));
        let server = make_server(&tmp);

        let goals = server
            .manage_goals(ManageGoalsRequest {
                operation: "list".into(),
                project: PROJECT_PATH.into(),
                provider: Some("codex".into()),
                text: None,
                id: None,
                status: None,
                progress: None,
            })
            .await;
        let notes = server
            .manage_notes(ManageNotesRequest {
                operation: "list".into(),
                project: PROJECT_PATH.into(),
                provider: Some("all".into()),
                text: None,
                session_id: None,
                id: None,
                resurface_when: None,
                expires_when: None,
            })
            .await;
        let decisions = server
            .manage_decisions(ManageDecisionsRequest {
                operation: "list".into(),
                project: PROJECT_PATH.into(),
                provider: Some("codex".into()),
                title: None,
                description: None,
                id: None,
                status: None,
                confidence: None,
                tags: None,
                superseded_by: None,
                session_id: None,
                resurface_when: None,
                expires_when: None,
            })
            .await;
        for output in [goals, notes, decisions] {
            let message = format!("{output:?}");
            assert!(
                message.contains("scoped to 'claude-code'"),
                "got: {message}"
            );
        }

        let explicit = unwrap_output(
            server
                .manage_goals(ManageGoalsRequest {
                    operation: "list".into(),
                    project: PROJECT_PATH.into(),
                    provider: Some("claude-code".into()),
                    text: None,
                    id: None,
                    status: None,
                    progress: None,
                })
                .await,
        );
        let explicit: serde_json::Value = serde_json::from_str(&explicit).unwrap();
        assert_eq!(explicit["storage_provider"], "claude-code");
    }

    fn make_server(tmp: &TempDir) -> SnatchServer {
        SnatchServer::new(Some(tmp.path().to_path_buf()), None)
    }

    #[cfg(feature = "codex")]
    fn setup_codex_dir_with_cwd(cwd: &str) -> (TempDir, String) {
        let tmp = TempDir::new().unwrap();
        let day = tmp.path().join("sessions/2026/07/17");
        std::fs::create_dir_all(&day).unwrap();
        let thread = "019f7000-0000-7000-8000-000000000088";
        let line = |kind: &str, payload: serde_json::Value| {
            serde_json::json!({
                "timestamp": "2026-07-17T00:00:00Z",
                "type": kind,
                "payload": payload,
            })
            .to_string()
        };
        let content = [
            line(
                "session_meta",
                serde_json::json!({"id": thread, "cwd": cwd}),
            ),
            line(
                "turn_context",
                serde_json::json!({"turn_id": "turn-1", "model": "gpt-test"}),
            ),
            line(
                "response_item",
                serde_json::json!({"type": "message", "role": "user",
                    "content": [{"type": "input_text", "text": "start task"}]}),
            ),
            line(
                "event_msg",
                serde_json::json!({"type": "user_message", "message": "start task",
                    "images": [], "local_images": [], "text_elements": []}),
            ),
            line(
                "response_item",
                serde_json::json!({"type": "message", "role": "assistant",
                    "content": [{"type": "output_text", "text": "working"}]}),
            ),
            line(
                "event_msg",
                serde_json::json!({"type": "user_message", "message": "also inspect tests",
                    "images": [], "local_images": [], "text_elements": []}),
            ),
            line(
                "response_item",
                serde_json::json!({"type": "message", "role": "assistant",
                    "content": [{"type": "output_text", "text": "done"}]}),
            ),
            line(
                "event_msg",
                serde_json::json!({"type": "token_count", "info": {
                    "last_token_usage": {"input_tokens": 100, "cached_input_tokens": 60,
                        "output_tokens": 25, "total_tokens": 125},
                    "total_token_usage": {"input_tokens": 100, "cached_input_tokens": 60,
                        "output_tokens": 25, "total_tokens": 125}}}),
            ),
            line(
                "compacted",
                serde_json::json!({"message": "compact summary", "window_id": 1}),
            ),
        ]
        .join("\n")
            + "\n";
        std::fs::write(
            day.join(format!("rollout-2026-07-17T00-00-00-{thread}.jsonl")),
            content,
        )
        .unwrap();
        (tmp, thread.to_string())
    }

    #[cfg(feature = "codex")]
    fn setup_codex_dir() -> (TempDir, String) {
        setup_codex_dir_with_cwd("/tmp/mcp-codex")
    }

    #[cfg(feature = "codex")]
    fn setup_codex_tool_dir() -> (TempDir, String) {
        let tmp = TempDir::new().unwrap();
        let day = tmp.path().join("sessions/2026/07/17");
        std::fs::create_dir_all(&day).unwrap();
        let thread = "019f7000-0000-7000-8000-000000000099";
        let line = |kind: &str, payload: serde_json::Value| {
            serde_json::json!({
                "timestamp": "2026-07-17T00:00:00Z",
                "type": kind,
                "payload": payload,
            })
            .to_string()
        };
        let content = [
            line(
                "session_meta",
                serde_json::json!({"id": thread, "cwd": "/tmp/mcp-codex-tool"}),
            ),
            line(
                "turn_context",
                serde_json::json!({"turn_id": "turn-1", "model": "gpt-test"}),
            ),
            line(
                "response_item",
                serde_json::json!({"type": "message", "role": "user",
                    "content": [{"type": "input_text", "text": "injected provider instructions"}]}),
            ),
            line(
                "response_item",
                serde_json::json!({"type": "message", "role": "user",
                    "content": [{"type": "input_text", "text": "run the failing check"}]}),
            ),
            line(
                "event_msg",
                serde_json::json!({"type": "user_message", "message": "run the failing check",
                    "images": [], "local_images": [], "text_elements": []}),
            ),
            line(
                "response_item",
                serde_json::json!({"type": "function_call", "name": "exec_command",
                    "call_id": "call-1", "arguments": "{\"cmd\":\"cargo check\"}"}),
            ),
            line(
                "event_msg",
                serde_json::json!({"type": "exec_command_end", "call_id": "call-1",
                    "turn_id": "turn-1", "command": ["cargo", "check"], "cwd": "/tmp",
                    "parsed_cmd": [], "source": "unified_exec_startup", "stdout": "",
                    "stderr": "compile failed", "aggregated_output": "compile failed",
                    "formatted_output": "", "exit_code": 101,
                    "duration": {"secs": 2, "nanos": 500_000_000}, "status": "failed"}),
            ),
            line(
                "response_item",
                serde_json::json!({"type": "function_call_output", "call_id": "call-1",
                    "output": "Process exited with code 101\nFinal output:\ncompile failed"}),
            ),
            line(
                "response_item",
                serde_json::json!({"type": "message", "role": "assistant",
                    "content": [{"type": "output_text", "text": "the check failed"}]}),
            ),
        ]
        .join("\n")
            + "\n";
        std::fs::write(
            day.join(format!("rollout-2026-07-17T00-00-00-{thread}.jsonl")),
            content,
        )
        .unwrap();
        (tmp, thread.to_string())
    }

    #[cfg(feature = "codex")]
    fn setup_codex_fork_dir_with_cwd(cwd: &str) -> (TempDir, String, String) {
        let tmp = TempDir::new().unwrap();
        let day = tmp.path().join("sessions/2026/07/17");
        std::fs::create_dir_all(&day).unwrap();
        let parent = "019f7000-0000-7000-8000-000000000101";
        let child = "019f7000-0000-7000-8000-000000000102";
        let envelope = |timestamp: &str, kind: &str, payload: serde_json::Value| {
            serde_json::json!({
                "timestamp": timestamp,
                "type": kind,
                "payload": payload,
            })
        };
        let usage = |input: u64, output: u64| {
            serde_json::json!({
                "input_tokens": input,
                "cached_input_tokens": 0,
                "output_tokens": output,
                "total_tokens": input + output,
            })
        };
        let parent_records = vec![
            envelope(
                "2026-07-17T00:00:00Z",
                "session_meta",
                serde_json::json!({"id": parent, "cwd": cwd}),
            ),
            envelope(
                "2026-07-17T00:00:01Z",
                "turn_context",
                serde_json::json!({"turn_id": "parent-turn", "model": "gpt-test"}),
            ),
            envelope(
                "2026-07-17T00:00:02Z",
                "response_item",
                serde_json::json!({"type": "message", "role": "user",
                    "content": [{"type": "input_text", "text": "parent prompt long enough"}]}),
            ),
            envelope(
                "2026-07-17T00:00:03Z",
                "event_msg",
                serde_json::json!({"type": "user_message", "message": "parent prompt long enough",
                    "images": [], "local_images": [], "text_elements": []}),
            ),
            envelope(
                "2026-07-17T00:00:04Z",
                "response_item",
                serde_json::json!({"type": "message", "role": "assistant",
                    "content": [{"type": "output_text", "text": "parent answer"}]}),
            ),
            envelope(
                "2026-07-17T00:00:05Z",
                "event_msg",
                serde_json::json!({"type": "token_count", "info": {
                    "last_token_usage": usage(10, 2),
                    "total_token_usage": usage(10, 2)}}),
            ),
        ];
        let serialize = |records: &[serde_json::Value]| {
            records
                .iter()
                .map(serde_json::Value::to_string)
                .collect::<Vec<_>>()
                .join("\n")
                + "\n"
        };
        std::fs::write(
            day.join(format!("rollout-2026-07-17T00-00-00-{parent}.jsonl")),
            serialize(&parent_records),
        )
        .unwrap();

        let mut child_records = vec![envelope(
            "2026-07-17T01:00:00Z",
            "session_meta",
            serde_json::json!({"id": child, "cwd": cwd}),
        )];
        child_records.extend(parent_records.iter().cloned().map(|mut record| {
            record["timestamp"] = serde_json::json!("2026-07-17T01:00:01Z");
            record
        }));
        child_records.extend([
            envelope(
                "2026-07-17T01:00:10Z",
                "turn_context",
                serde_json::json!({"turn_id": "child-turn", "model": "gpt-test"}),
            ),
            envelope(
                "2026-07-17T01:00:11Z",
                "response_item",
                serde_json::json!({"type": "message", "role": "user",
                    "content": [{"type": "input_text", "text": "child prompt long enough"}]}),
            ),
            envelope(
                "2026-07-17T01:00:12Z",
                "event_msg",
                serde_json::json!({"type": "user_message", "message": "child prompt long enough",
                    "images": [], "local_images": [], "text_elements": []}),
            ),
            envelope(
                "2026-07-17T01:00:13Z",
                "response_item",
                serde_json::json!({"type": "message", "role": "assistant",
                    "content": [{"type": "output_text", "text": "child answer"}]}),
            ),
            envelope(
                "2026-07-17T01:00:14Z",
                "event_msg",
                serde_json::json!({"type": "token_count", "info": {
                    "last_token_usage": usage(10, 2),
                    "total_token_usage": usage(20, 4)}}),
            ),
        ]);
        std::fs::write(
            day.join(format!("rollout-2026-07-17T01-00-00-{child}.jsonl")),
            serialize(&child_records),
        )
        .unwrap();
        (tmp, parent.to_string(), child.to_string())
    }

    #[cfg(feature = "codex")]
    #[tokio::test]
    async fn provider_project_history_unifies_cwd_and_reports_unpriced_cost() {
        let claude_id = "aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa";
        let claude = setup_claude_dir(claude_id, PROJECT_PATH, &minimal_session_jsonl(claude_id));
        let (codex, thread) = setup_codex_dir_with_cwd(PROJECT_PATH);
        let server = make_server(&claude).with_codex_dir(codex.path());
        let output = unwrap_output(
            server
                .get_project_history(GetProjectHistoryRequest {
                    project: PROJECT_PATH.to_string(),
                    provider: Some(vec!["all".to_string()]),
                    period: Some("all".to_string()),
                    limit: None,
                    include_summaries: Some(true),
                })
                .await,
        );
        let value: serde_json::Value = serde_json::from_str(&output).unwrap();
        assert_eq!(value["sessions_found"], 2);
        assert_eq!(value["aggregate"]["total_sessions"], 2);
        assert!(value["aggregate"]["total_cost"].is_null());
        assert_eq!(value["aggregate"]["has_unpriced_sessions"], true);
        assert_eq!(value["activity_basis"], "new-activity-only");
        assert_eq!(
            value["providers"],
            serde_json::json!(["claude-code", "codex"])
        );
        let ids: std::collections::BTreeSet<_> = value["sessions"]
            .as_array()
            .unwrap()
            .iter()
            .map(|session| session["qualified_id"].as_str().unwrap())
            .collect();
        assert_eq!(
            ids,
            [
                format!("claude-code:{claude_id}"),
                format!("codex:{thread}"),
            ]
            .iter()
            .map(String::as_str)
            .collect()
        );
    }

    #[cfg(feature = "codex")]
    #[tokio::test]
    async fn provider_project_history_never_double_counts_fork_inherited_activity() {
        let claude = setup_claude_dir(
            "aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa",
            "/tmp/unrelated-claude-project",
            &minimal_session_jsonl("aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa"),
        );
        let project = "/tmp/mcp-codex-fork-project";
        let (codex, parent, child) = setup_codex_fork_dir_with_cwd(project);
        let server = make_server(&claude).with_codex_dir(codex.path());
        let output = unwrap_output(
            server
                .get_project_history(GetProjectHistoryRequest {
                    project: project.to_string(),
                    provider: Some(vec!["codex".to_string()]),
                    period: Some("all".to_string()),
                    limit: None,
                    include_summaries: Some(false),
                })
                .await,
        );
        let value: serde_json::Value = serde_json::from_str(&output).unwrap();
        let sessions = value["sessions"].as_array().unwrap();
        assert_eq!(
            sessions.len(),
            2,
            "fork remains an independent history unit"
        );
        assert_eq!(value["aggregate"]["total_prompts"], 2);
        assert_eq!(value["aggregate"]["total_tokens"], 24);
        let by_id: std::collections::BTreeMap<_, _> = sessions
            .iter()
            .map(|session| (session["session_id"].as_str().unwrap(), session))
            .collect();
        assert_eq!(by_id[parent.as_str()]["user_prompt_count"], 1);
        assert_eq!(by_id[child.as_str()]["user_prompt_count"], 1);
        assert_eq!(by_id[parent.as_str()]["total_tokens"], 12);
        assert_eq!(by_id[child.as_str()]["total_tokens"], 12);
        assert_eq!(
            by_id[child.as_str()]["parent_session_id"],
            format!("codex:{parent}")
        );
    }

    #[cfg(feature = "codex")]
    #[tokio::test]
    async fn provider_messages_and_timeline_route_codex_with_semantics() {
        let claude = setup_claude_dir(
            "aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa",
            PROJECT_PATH,
            &minimal_session_jsonl("aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa"),
        );
        let (codex, thread) = setup_codex_dir();
        let server = make_server(&claude).with_codex_dir(codex.path());
        let qualified = format!("codex:{thread}");

        let messages = unwrap_output(
            server
                .get_session_messages(GetSessionMessagesRequest {
                    session_id: qualified.clone(),
                    provider: None,
                    detail: Some("conversation".to_string()),
                    message_type: None,
                    limit: None,
                    offset: None,
                    reverse: None,
                    include_thinking: None,
                    chain_aware: None,
                    after_timestamp: None,
                    before_timestamp: None,
                    include_subagent_transcripts: None,
                    chunk: None,
                    errors_only: None,
                    max_text_len: None,
                })
                .await,
        );
        let messages: serde_json::Value = serde_json::from_str(&messages).unwrap();
        assert_eq!(messages["provider"], "codex");
        assert_eq!(messages["qualified_id"], qualified);
        let rendered = messages["messages"].to_string();
        assert!(rendered.contains("start task"));
        assert!(rendered.contains("also inspect tests"));

        let info = unwrap_output(
            server
                .get_session_info(GetSessionInfoRequest {
                    session_id: qualified.clone(),
                    provider: None,
                })
                .await,
        );
        let info: serde_json::Value = serde_json::from_str(&info).unwrap();
        assert_eq!(info["usage"]["observation_counts"]["call/delta"], 1);
        assert_eq!(info["usage"]["observation_counts"]["session/cumulative"], 1);
        assert_eq!(info["usage"]["canonical"]["total_processed_tokens"], 125);
        assert_eq!(info["usage"]["pricing"]["policy"], "unpriced");
        assert!(info["usage"]["pricing"]["estimated_cost"].is_null());

        let lessons = unwrap_output(
            server
                .get_session_lessons(GetSessionLessonsRequest {
                    session_id: qualified.clone(),
                    provider: None,
                    category: None,
                    limit: None,
                })
                .await,
        );
        let lessons: serde_json::Value = serde_json::from_str(&lessons).unwrap();
        assert_eq!(lessons["provider"], "codex");
        assert_eq!(lessons["qualified_id"], qualified);

        let timeline = unwrap_output(
            server
                .get_session_timeline(GetSessionTimelineRequest {
                    session_id: format!("codex:{thread}"),
                    provider: None,
                    limit: None,
                    chain_aware: None,
                })
                .await,
        );
        let timeline: serde_json::Value = serde_json::from_str(&timeline).unwrap();
        assert_eq!(timeline["provider"], "codex");
        assert_eq!(timeline["total_turns"], 1);
        assert_eq!(
            timeline["timeline"][0]["steering_prompts"],
            serde_json::json!(["also inspect tests"])
        );
        assert_eq!(timeline["compaction_events"].as_array().unwrap().len(), 1);
        assert_eq!(timeline["compaction_events"][0]["kind"], "full");
        assert_eq!(timeline["compaction_events"][0]["window"]["number"], 1);
        assert_eq!(
            timeline["compaction_events"][0]["window"]["legacy_numeric_id"],
            true
        );

        let chunked = unwrap_output(
            server
                .get_session_messages(GetSessionMessagesRequest {
                    session_id: format!("codex:{thread}"),
                    provider: None,
                    detail: None,
                    message_type: None,
                    limit: None,
                    offset: None,
                    reverse: None,
                    include_thinking: None,
                    chain_aware: None,
                    after_timestamp: None,
                    before_timestamp: None,
                    include_subagent_transcripts: None,
                    chunk: Some("0".to_string()),
                    errors_only: None,
                    max_text_len: None,
                })
                .await,
        );
        let chunked: serde_json::Value = serde_json::from_str(&chunked).unwrap();
        assert_eq!(chunked["chunk_info"]["total_chunks"], 1);
        assert_eq!(chunked["chunk_info"]["chunks"][0]["prompt"], "start task");
        assert!(chunked["messages"]
            .to_string()
            .contains("also inspect tests"));

        let overview = unwrap_output(
            server
                .get_session_messages(GetSessionMessagesRequest {
                    session_id: format!("codex:{thread}"),
                    provider: None,
                    detail: Some("overview".to_string()),
                    message_type: None,
                    limit: None,
                    offset: None,
                    reverse: None,
                    include_thinking: None,
                    chain_aware: None,
                    after_timestamp: None,
                    before_timestamp: None,
                    include_subagent_transcripts: None,
                    chunk: None,
                    errors_only: None,
                    max_text_len: None,
                })
                .await,
        );
        let overview: serde_json::Value = serde_json::from_str(&overview).unwrap();
        assert_eq!(overview["returned"], 1);
        assert!(overview["messages"].to_string().contains("start task"));
        assert!(!overview["messages"]
            .to_string()
            .contains("also inspect tests"));
    }

    #[cfg(feature = "codex")]
    #[tokio::test]
    async fn advanced_retrieval_routes_codex_and_labels_failure_basis() {
        let claude_id = "aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa";
        let claude = setup_claude_dir(claude_id, PROJECT_PATH, &minimal_session_jsonl(claude_id));
        let (codex, thread) = setup_codex_tool_dir();
        let server = make_server(&claude).with_codex_dir(codex.path());
        let qualified = format!("codex:{thread}");

        let resolved = match server.resolve_analytical_session(&[], &qualified) {
            Ok(resolved) => resolved,
            Err(ToolOutput::RecoverableError { message, .. }) => {
                panic!("Codex fixture should resolve: {message}")
            }
            Err(ToolOutput::Success(_)) => panic!("resolver returned a success as an error"),
        };
        let chronological_text = resolved
            .conversation
            .chronological_entries()
            .into_iter()
            .filter_map(crate::analysis::extraction::extract_visible_text)
            .collect::<Vec<_>>();
        assert!(
            chronological_text
                .iter()
                .any(|text| text == "injected provider instructions"),
            "harness entry must remain content-complete: {chronological_text:?}"
        );
        let main_text = resolved
            .conversation
            .main_thread_entries()
            .into_iter()
            .filter_map(crate::analysis::extraction::extract_visible_text)
            .collect::<Vec<_>>();
        assert!(
            main_text
                .iter()
                .any(|text| text == "injected provider instructions"),
            "harness entry unexpectedly left the main thread: {main_text:?}"
        );

        let tool_calls = unwrap_output(
            server
                .get_tool_calls(GetToolCallsRequest {
                    session_id: qualified.clone(),
                    provider: None,
                    tool_filter: None,
                    errors_only: Some(true),
                    failure_kind: None,
                    chunk: None,
                    limit: None,
                    offset: None,
                })
                .await,
        );
        let tool_calls: serde_json::Value = serde_json::from_str(&tool_calls).unwrap();
        assert_eq!(tool_calls["provider"], "codex");
        assert_eq!(tool_calls["qualified_id"], qualified);
        assert_eq!(tool_calls["summary"]["confirmed_tool_failures"], 1);
        assert_eq!(tool_calls["summary"]["inferred_failure_signals"], 0);
        assert_eq!(tool_calls["tool_calls"][0]["failure_kind"], "confirmed");
        assert_eq!(tool_calls["tool_calls"][0]["failure_basis"], "process_exit");
        assert_eq!(
            tool_calls["tool_calls"][0]["lifecycle"][0]["kind"],
            "command"
        );
        assert_eq!(
            tool_calls["tool_calls"][0]["lifecycle"][0]["status"],
            "failed"
        );
        assert_eq!(
            tool_calls["tool_calls"][0]["lifecycle"][0]["exit_code"],
            101
        );
        assert_eq!(
            tool_calls["tool_calls"][0]["lifecycle"][0]["duration_ms"],
            2500.0
        );

        let scoped = unwrap_output(
            server
                .get_tool_calls(GetToolCallsRequest {
                    session_id: qualified.clone(),
                    provider: None,
                    tool_filter: None,
                    errors_only: None,
                    failure_kind: None,
                    chunk: Some("0".into()),
                    limit: None,
                    offset: None,
                })
                .await,
        );
        let scoped: serde_json::Value = serde_json::from_str(&scoped).unwrap();
        assert_eq!(scoped["total_tool_calls"], 1);
        assert_eq!(
            scoped["summary"]["entry_scope"],
            "semantic_chunk",
            "tool-call chunking must use the provider's prompt semantics rather than treating harness context as a human boundary"
        );

        let digest = unwrap_output(
            server
                .get_session_digest(GetSessionDigestRequest {
                    session_id: qualified.clone(),
                    provider: None,
                    max_prompts: None,
                    max_files: None,
                    include_formatted: None,
                })
                .await,
        );
        let digest: serde_json::Value = serde_json::from_str(&digest).unwrap();
        assert_eq!(digest["provider"], "codex");
        assert_eq!(digest["confirmed_tool_failures"], 1);
        assert_eq!(digest["total_prompts"], 1);
        assert!(
            !digest["key_prompts"]
                .to_string()
                .contains("injected provider instructions"),
            "provider-authored harness context must not become a human prompt"
        );
        assert!(
            digest.get("formatted").is_none(),
            "structured is the lean default"
        );

        let formatted = unwrap_output(
            server
                .get_session_digest(GetSessionDigestRequest {
                    session_id: qualified.clone(),
                    provider: None,
                    max_prompts: None,
                    max_files: None,
                    include_formatted: Some(true),
                })
                .await,
        );
        let formatted: serde_json::Value = serde_json::from_str(&formatted).unwrap();
        assert!(formatted["formatted"]
            .as_str()
            .is_some_and(|text| !text.is_empty()));

        let thread_output = unwrap_output(
            server
                .thread_topic(ThreadTopicRequest {
                    pattern: "failing check".to_string(),
                    provider: None,
                    project: None,
                    session_id: Some(qualified.clone()),
                    include_thinking: None,
                    no_subagents: Some(false),
                    since: None,
                    until: None,
                    decisions_only: None,
                    limit: None,
                    max_context: None,
                })
                .await,
        );
        let thread_output: serde_json::Value = serde_json::from_str(&thread_output).unwrap();
        assert_eq!(thread_output["total_exchanges"], 1);
        assert_eq!(thread_output["exchanges"][0]["provider"], "codex");
        assert_eq!(thread_output["exchanges"][0]["match_provenance"], "primary");

        let injected_thread = unwrap_output(
            server
                .thread_topic(ThreadTopicRequest {
                    pattern: "injected provider instructions".to_string(),
                    provider: None,
                    project: None,
                    session_id: Some(qualified),
                    include_thinking: None,
                    no_subagents: Some(false),
                    since: None,
                    until: None,
                    decisions_only: None,
                    limit: None,
                    max_context: None,
                })
                .await,
        );
        let injected_thread: serde_json::Value = serde_json::from_str(&injected_thread).unwrap();
        assert_eq!(
            injected_thread["exchanges"][0]["match_provenance"], "injected",
            "{injected_thread}"
        );
    }

    #[tokio::test]
    async fn test_list_sessions_returns_fixture() {
        let sid = "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee";
        let tmp = setup_claude_dir(sid, PROJECT_PATH, &minimal_session_jsonl(sid));
        let server = make_server(&tmp);
        let text = unwrap_output(
            server
                .list_sessions(ListSessionsRequest {
                    project: None,
                    limit: None,
                    include_subagents: None,
                    provider: None,
                })
                .await,
        );
        assert!(text.contains(sid));
    }

    #[tokio::test]
    async fn test_list_sessions_project_filter() {
        let sid = "bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb";
        let tmp = setup_claude_dir(sid, PROJECT_PATH, &minimal_session_jsonl(sid));
        let server = make_server(&tmp);
        let text = unwrap_output(
            server
                .list_sessions(ListSessionsRequest {
                    project: Some("test-project".to_string()),
                    limit: None,
                    include_subagents: None,
                    provider: None,
                })
                .await,
        );
        assert!(text.contains(sid));
    }

    #[tokio::test]
    async fn test_get_session_info_valid() {
        let sid = "eeeeeeee-eeee-eeee-eeee-eeeeeeeeeeee";
        let tmp = setup_claude_dir(sid, PROJECT_PATH, &minimal_session_jsonl(sid));
        let server = make_server(&tmp);
        let text = unwrap_output(
            server
                .get_session_info(GetSessionInfoRequest {
                    session_id: sid.to_string(),
                    provider: None,
                })
                .await,
        );
        assert!(text.contains(sid));
    }

    #[tokio::test]
    async fn test_get_session_info_nonexistent() {
        let tmp = TempDir::new().unwrap();
        std::fs::create_dir_all(tmp.path().join("projects")).unwrap();
        let server = make_server(&tmp);
        assert_error(
            server
                .get_session_info(GetSessionInfoRequest {
                    session_id: "ffffffff-ffff-ffff-ffff-ffffffffffff".to_string(),
                    provider: None,
                })
                .await,
        );
    }

    #[tokio::test]
    async fn test_search_sessions_match() {
        let sid = "11111111-aaaa-bbbb-cccc-dddddddddddd";
        let tmp = setup_claude_dir(sid, PROJECT_PATH, &minimal_session_jsonl(sid));
        let server = make_server(&tmp);
        let text = unwrap_output(
            server
                .search_sessions(SearchSessionsRequest {
                    pattern: "Hello, Claude!".to_string(),
                    project: None,
                    session_id: None,
                    scope: None,
                    ignore_case: None,
                    limit: None,
                    chain_aware: None,
                })
                .await,
        );
        assert!(text.contains(sid));
    }

    /// Build a temp Claude dir with a two-file resume chain. The continuation's
    /// internal `sessionId` points at the root file's UUID.
    fn setup_chain_claude_dir(
        root_id: &str,
        cont_id: &str,
        project_path: &str,
        root_text: &str,
        cont_text: &str,
    ) -> TempDir {
        let tmp = TempDir::new().expect("failed to create temp dir");
        let encoded = encode_project_path(project_path);
        let project_dir = tmp.path().join("projects").join(&encoded);
        std::fs::create_dir_all(&project_dir).unwrap();
        let root_line = format!(
            r#"{{"type":"user","uuid":"c1111111-1111-1111-1111-111111111111","parentUuid":null,"timestamp":"2025-01-15T10:00:00.000Z","sessionId":"{root_id}","version":"2.0.74","message":{{"role":"user","content":"{root_text}"}}}}"#
        );
        std::fs::write(
            project_dir.join(format!("{root_id}.jsonl")),
            format!("{root_line}\n"),
        )
        .unwrap();
        let cont_line = format!(
            r#"{{"type":"user","uuid":"c2222222-2222-2222-2222-222222222222","parentUuid":null,"timestamp":"2025-01-15T11:00:00.000Z","sessionId":"{root_id}","version":"2.0.74","message":{{"role":"user","content":"{cont_text}"}}}}"#
        );
        std::fs::write(
            project_dir.join(format!("{cont_id}.jsonl")),
            format!("{cont_line}\n"),
        )
        .unwrap();
        tmp
    }

    #[tokio::test]
    async fn test_search_sessions_chain_aware_covers_whole_chain() {
        let root = "aaaaaaaa-1111-1111-1111-111111111111";
        let cont = "bbbbbbbb-2222-2222-2222-222222222222";
        let tmp = setup_chain_claude_dir(
            root,
            cont,
            PROJECT_PATH,
            "alpha_root_marker only here",
            "beta_cont_marker only here",
        );
        let server = make_server(&tmp);

        // Default (chain-aware): searching the continuation finds text that
        // lives only in the root file.
        let text = unwrap_output(
            server
                .search_sessions(SearchSessionsRequest {
                    pattern: "alpha_root_marker".to_string(),
                    project: None,
                    session_id: Some(cont.to_string()),
                    scope: None,
                    ignore_case: None,
                    limit: None,
                    chain_aware: None,
                })
                .await,
        );
        assert!(
            text.contains("alpha_root_marker"),
            "chain-aware search should reach the root file: {text}"
        );

        // chain_aware=false restricts to the single continuation file, which
        // does not contain the root-only text.
        let text = unwrap_output(
            server
                .search_sessions(SearchSessionsRequest {
                    pattern: "alpha_root_marker".to_string(),
                    project: None,
                    session_id: Some(cont.to_string()),
                    scope: None,
                    ignore_case: None,
                    limit: None,
                    chain_aware: Some(false),
                })
                .await,
        );
        assert!(
            text.contains("\"total_matches\": 0"),
            "single-file search should not reach the root file: {text}"
        );
    }

    #[tokio::test]
    async fn test_search_sessions_no_match() {
        let sid = "22222222-aaaa-bbbb-cccc-dddddddddddd";
        let tmp = setup_claude_dir(sid, PROJECT_PATH, &minimal_session_jsonl(sid));
        let server = make_server(&tmp);
        let text = unwrap_output(
            server
                .search_sessions(SearchSessionsRequest {
                    pattern: "xyzzy_nonexistent".to_string(),
                    project: None,
                    session_id: None,
                    scope: None,
                    ignore_case: None,
                    limit: None,
                    chain_aware: None,
                })
                .await,
        );
        assert!(!text.contains(sid));
    }

    #[tokio::test]
    async fn test_get_session_timeline() {
        let sid = "33333333-aaaa-bbbb-cccc-dddddddddddd";
        let tmp = setup_claude_dir(sid, PROJECT_PATH, &minimal_session_jsonl(sid));
        let server = make_server(&tmp);
        let text = unwrap_output(
            server
                .get_session_timeline(GetSessionTimelineRequest {
                    session_id: sid.to_string(),
                    provider: None,
                    limit: None,
                    chain_aware: None,
                })
                .await,
        );
        assert!(!text.is_empty());
    }

    #[tokio::test]
    async fn test_get_session_digest() {
        let sid = "44444444-aaaa-bbbb-cccc-dddddddddddd";
        let tmp = setup_claude_dir(sid, PROJECT_PATH, &minimal_session_jsonl(sid));
        let server = make_server(&tmp);
        let text = unwrap_output(
            server
                .get_session_digest(GetSessionDigestRequest {
                    session_id: sid.to_string(),
                    provider: None,
                    max_prompts: None,
                    max_files: None,
                    include_formatted: None,
                })
                .await,
        );
        assert!(!text.is_empty());
    }

    #[tokio::test]
    async fn test_get_session_lessons_no_errors() {
        let sid = "55555555-aaaa-bbbb-cccc-dddddddddddd";
        let tmp = setup_claude_dir(sid, PROJECT_PATH, &minimal_session_jsonl(sid));
        let server = make_server(&tmp);
        let _text = unwrap_output(
            server
                .get_session_lessons(GetSessionLessonsRequest {
                    session_id: sid.to_string(),
                    provider: None,
                    category: None,
                    limit: None,
                })
                .await,
        );
    }

    #[tokio::test]
    async fn test_get_stats() {
        let sid = "66666666-aaaa-bbbb-cccc-dddddddddddd";
        let tmp = setup_claude_dir(sid, PROJECT_PATH, &minimal_session_jsonl(sid));
        let server = make_server(&tmp);
        let text = unwrap_output(
            server
                .get_stats(GetStatsRequest {
                    session_id: Some(sid.to_string()),
                    project: None,
                })
                .await,
        );
        assert!(!text.is_empty());
    }

    #[tokio::test]
    async fn test_get_session_messages() {
        let sid = "77777777-aaaa-bbbb-cccc-dddddddddddd";
        let tmp = setup_claude_dir(sid, PROJECT_PATH, &minimal_session_jsonl(sid));
        let server = make_server(&tmp);
        let text = unwrap_output(
            server
                .get_session_messages(GetSessionMessagesRequest {
                    session_id: sid.to_string(),
                    provider: None,
                    detail: None,
                    message_type: None,
                    limit: None,
                    offset: None,
                    reverse: None,
                    include_thinking: None,
                    chain_aware: None,
                    after_timestamp: None,
                    before_timestamp: None,
                    include_subagent_transcripts: None,
                    chunk: None,
                    errors_only: None,
                    max_text_len: None,
                })
                .await,
        );
        assert!(text.contains("Hello"));
    }

    #[tokio::test]
    async fn test_get_project_history() {
        let sid = "88888888-aaaa-bbbb-cccc-dddddddddddd";
        let tmp = setup_claude_dir(sid, PROJECT_PATH, &minimal_session_jsonl(sid));
        let server = make_server(&tmp);
        let text = unwrap_output(
            server
                .get_project_history(GetProjectHistoryRequest {
                    project: "test-project".to_string(),
                    provider: None,
                    period: Some("all".to_string()),
                    limit: None,
                    include_summaries: None,
                })
                .await,
        );
        assert!(!text.is_empty());
    }

    #[tokio::test]
    async fn test_get_tool_calls() {
        let sid = "99999999-aaaa-bbbb-cccc-dddddddddddd";
        let tmp = setup_claude_dir(sid, PROJECT_PATH, &minimal_session_jsonl(sid));
        let server = make_server(&tmp);
        let _text = unwrap_output(
            server
                .get_tool_calls(GetToolCallsRequest {
                    session_id: sid.to_string(),
                    provider: None,
                    tool_filter: None,
                    errors_only: None,
                    failure_kind: None,
                    chunk: None,
                    limit: None,
                    offset: None,
                })
                .await,
        );
    }

    /// Session with a successful Read, an errored Bash, and an Agent call whose
    /// result arrives as an array (no sidecar, so it stays unmatched).
    fn tool_result_session_jsonl(session_id: &str) -> String {
        let assistant = format!(
            r#"{{"type":"assistant","uuid":"a1","parentUuid":null,"timestamp":"2026-06-09T18:00:00Z","sessionId":"{session_id}","version":"2.1.0","isSidechain":false,"message":{{"id":"m1","type":"message","role":"assistant","model":"claude","content":[{{"type":"tool_use","id":"toolu_R","name":"Read","input":{{"file_path":"/x.rs"}}}},{{"type":"tool_use","id":"toolu_B","name":"Bash","input":{{"command":"false"}}}},{{"type":"tool_use","id":"toolu_A","name":"Agent","input":{{"description":"d","subagent_type":"Explore","prompt":"p"}}}}]}}}}"#
        );
        let results = format!(
            r#"{{"type":"user","uuid":"u2","parentUuid":"a1","timestamp":"2026-06-09T18:00:01Z","sessionId":"{session_id}","version":"2.1.0","isSidechain":false,"message":{{"role":"user","content":[{{"type":"tool_result","tool_use_id":"toolu_R","content":"FILE_CONTENTS_MARKER"}},{{"type":"tool_result","tool_use_id":"toolu_B","is_error":true,"content":"BASH_ERROR_MARKER"}},{{"type":"tool_result","tool_use_id":"toolu_A","content":[{{"type":"text","text":"AGENT_RESULT_MARKER"}}]}}]}}}}"#
        );
        format!("{assistant}\n{results}\n")
    }

    /// One native-confirmed failure and one text-inferred failure. Keeping
    /// both in one main-thread fixture makes cross-tool count comparisons
    /// meaningful rather than accidentally comparing different scopes.
    fn failure_taxonomy_session_jsonl(session_id: &str) -> String {
        let assistant = format!(
            r#"{{"type":"assistant","uuid":"a1","parentUuid":null,"timestamp":"2026-06-09T18:00:00Z","sessionId":"{session_id}","version":"2.1.0","isSidechain":false,"message":{{"id":"m1","type":"message","role":"assistant","model":"claude","content":[{{"type":"tool_use","id":"toolu_confirmed","name":"Bash","input":{{"command":"false"}}}},{{"type":"tool_use","id":"toolu_inferred","name":"Bash","input":{{"command":"cargo test"}}}}]}}}}"#
        );
        let results = format!(
            r#"{{"type":"user","uuid":"u2","parentUuid":"a1","timestamp":"2026-06-09T18:00:01Z","sessionId":"{session_id}","version":"2.1.0","isSidechain":false,"message":{{"role":"user","content":[{{"type":"tool_result","tool_use_id":"toolu_confirmed","is_error":true,"content":"native failure"}},{{"type":"tool_result","tool_use_id":"toolu_inferred","content":"thread worker panicked at assertion"}}]}}}}"#
        );
        format!("{assistant}\n{results}\n")
    }

    fn attachment_image_session_jsonl(session_id: &str) -> String {
        // A user turn with a top-level image block plus text.
        let user = format!(
            r#"{{"type":"user","uuid":"u1","parentUuid":null,"timestamp":"2026-06-09T18:00:00Z","sessionId":"{session_id}","version":"2.1.0","isSidechain":false,"message":{{"role":"user","content":[{{"type":"image","source":{{"type":"base64","media_type":"image/png","data":"x"}}}},{{"type":"text","text":"IMAGE_PROMPT_MARKER"}}]}}}}"#
        );
        // A content-bearing attachment (injected file): payload should surface.
        let file = format!(
            r#"{{"type":"attachment","uuid":"at1","parentUuid":"u1","timestamp":"2026-06-09T18:00:01Z","sessionId":"{session_id}","attachment":{{"type":"file","displayPath":"../CLAUDE.md","content":"FILE_BODY_MARKER"}}}}"#
        );
        // An operational attachment (noise): marker only, payload suppressed.
        let noise = format!(
            r#"{{"type":"attachment","uuid":"at2","parentUuid":"at1","timestamp":"2026-06-09T18:00:02Z","sessionId":"{session_id}","attachment":{{"type":"task_reminder","content":"NOISE_SHOULD_NOT_APPEAR","itemCount":1}}}}"#
        );
        format!("{user}\n{file}\n{noise}\n")
    }

    #[tokio::test]
    async fn test_get_session_messages_renders_attachments_and_images() {
        let sid = "cccccccc-1111-2222-3333-444444444444";
        let tmp = setup_claude_dir(sid, PROJECT_PATH, &attachment_image_session_jsonl(sid));
        let server = make_server(&tmp);
        let text = unwrap_output(
            server
                .get_session_messages(GetSessionMessagesRequest {
                    session_id: sid.to_string(),
                    provider: None,
                    detail: Some("full".to_string()),
                    message_type: None,
                    limit: None,
                    offset: None,
                    reverse: None,
                    include_thinking: None,
                    chain_aware: None,
                    after_timestamp: None,
                    before_timestamp: None,
                    include_subagent_transcripts: None,
                    chunk: None,
                    errors_only: None,
                    max_text_len: None,
                })
                .await,
        );
        // Image block renders a placeholder alongside the prompt text.
        assert!(
            text.contains("[image: image/png]"),
            "image placeholder missing"
        );
        assert!(
            text.contains("IMAGE_PROMPT_MARKER"),
            "image prompt text missing"
        );
        // Content-bearing attachment surfaces its marker and payload.
        assert!(text.contains("[attachment: file]"), "file marker missing");
        assert!(text.contains("FILE_BODY_MARKER"), "file payload missing");
        // Operational attachment is marker-only — payload suppressed.
        assert!(
            text.contains("[attachment: task_reminder]"),
            "noise marker missing"
        );
        assert!(
            !text.contains("NOISE_SHOULD_NOT_APPEAR"),
            "noise payload should not be surfaced"
        );
    }

    #[tokio::test]
    async fn test_get_session_messages_full_surfaces_tool_output() {
        let sid = "aaaaaaaa-1111-2222-3333-444444444444";
        let tmp = setup_claude_dir(sid, PROJECT_PATH, &tool_result_session_jsonl(sid));
        let server = make_server(&tmp);
        let text = unwrap_output(
            server
                .get_session_messages(GetSessionMessagesRequest {
                    session_id: sid.to_string(),
                    provider: None,
                    detail: Some("full".to_string()),
                    message_type: None,
                    limit: None,
                    offset: None,
                    reverse: None,
                    include_thinking: None,
                    chain_aware: None,
                    after_timestamp: None,
                    before_timestamp: None,
                    include_subagent_transcripts: None,
                    chunk: None,
                    errors_only: None,
                    max_text_len: None,
                })
                .await,
        );
        // Successful Read output is surfaced.
        assert!(text.contains("FILE_CONTENTS_MARKER"), "Read output missing");
        // Errored Bash output is surfaced and flagged.
        assert!(text.contains("BASH_ERROR_MARKER"), "Bash error missing");
        assert!(text.contains("had_error"), "had_error flag missing");
        // An unmatched Agent's array result falls back to the tool_result block.
        assert!(text.contains("AGENT_RESULT_MARKER"), "Agent result missing");
    }

    #[tokio::test]
    async fn test_get_tool_calls_surfaces_result_preview() {
        let sid = "bbbbbbbb-1111-2222-3333-444444444444";
        let tmp = setup_claude_dir(sid, PROJECT_PATH, &tool_result_session_jsonl(sid));
        let server = make_server(&tmp);
        let text = unwrap_output(
            server
                .get_tool_calls(GetToolCallsRequest {
                    session_id: sid.to_string(),
                    provider: None,
                    tool_filter: None,
                    errors_only: None,
                    failure_kind: None,
                    chunk: None,
                    limit: None,
                    offset: None,
                })
                .await,
        );
        assert!(
            text.contains("result_preview"),
            "result_preview field missing"
        );
        assert!(
            text.contains("FILE_CONTENTS_MARKER"),
            "success output missing"
        );
        assert!(text.contains("BASH_ERROR_MARKER"), "error preview missing");
    }

    #[tokio::test]
    async fn failure_taxonomy_is_consistent_and_filterable_across_tools() {
        let sid = "eeeeeeee-1111-2222-3333-444444444444";
        let tmp = setup_claude_dir(sid, PROJECT_PATH, &failure_taxonomy_session_jsonl(sid));
        let server = make_server(&tmp);

        let confirmed = unwrap_output(
            server
                .get_tool_calls(GetToolCallsRequest {
                    session_id: sid.to_string(),
                    provider: None,
                    tool_filter: None,
                    errors_only: Some(true),
                    failure_kind: None,
                    chunk: None,
                    limit: None,
                    offset: None,
                })
                .await,
        );
        let confirmed: serde_json::Value = serde_json::from_str(&confirmed).unwrap();
        assert_eq!(confirmed["returned"], 1);
        assert_eq!(confirmed["tool_calls"][0]["failure_kind"], "confirmed");
        assert_eq!(confirmed["summary"]["confirmed_tool_failures"], 1);
        assert_eq!(confirmed["summary"]["inferred_failure_signals"], 1);
        assert_eq!(confirmed["summary"]["entry_scope"], "main_thread");

        let all = unwrap_output(
            server
                .get_tool_calls(GetToolCallsRequest {
                    session_id: sid.to_string(),
                    provider: None,
                    tool_filter: None,
                    errors_only: Some(true),
                    failure_kind: Some("all".to_string()),
                    chunk: None,
                    limit: None,
                    offset: None,
                })
                .await,
        );
        let all: serde_json::Value = serde_json::from_str(&all).unwrap();
        assert_eq!(all["returned"], 2);
        assert!(all["tool_calls"]
            .as_array()
            .unwrap()
            .iter()
            .any(|call| call["failure_kind"] == "inferred"
                && call["failure_basis"] == "text_signature"));

        let lessons = unwrap_output(
            server
                .get_session_lessons(GetSessionLessonsRequest {
                    session_id: sid.to_string(),
                    provider: None,
                    category: Some("errors".to_string()),
                    limit: None,
                })
                .await,
        );
        let lessons: serde_json::Value = serde_json::from_str(&lessons).unwrap();
        assert_eq!(lessons["summary"]["confirmed_tool_failures"], 1);
        assert_eq!(lessons["summary"]["inferred_failure_signals"], 1);
        assert_eq!(lessons["summary"]["entry_scope"], "whole_conversation");

        let correction_projection = unwrap_output(
            server
                .get_session_lessons(GetSessionLessonsRequest {
                    session_id: sid.to_string(),
                    provider: None,
                    category: Some("corrections".to_string()),
                    limit: None,
                })
                .await,
        );
        let correction_projection: serde_json::Value =
            serde_json::from_str(&correction_projection).unwrap();
        assert_eq!(correction_projection["summary"]["total_errors"], 2);
        assert_eq!(
            correction_projection["summary"]["confirmed_tool_failures"],
            1
        );
        assert_eq!(
            correction_projection["summary"]["inferred_failure_signals"],
            1
        );
        assert!(correction_projection["error_fix_pairs"]
            .as_array()
            .unwrap()
            .is_empty());

        let invalid = server
            .get_tool_calls(GetToolCallsRequest {
                session_id: sid.to_string(),
                provider: None,
                tool_filter: None,
                errors_only: Some(false),
                failure_kind: Some("all".to_string()),
                chunk: None,
                limit: None,
                offset: None,
            })
            .await;
        assert!(matches!(
            invalid,
            ToolOutput::RecoverableError { message, .. }
                if message.contains("requires errors_only=true")
        ));
    }

    #[tokio::test]
    async fn test_full_detail_surfaces_result_when_result_turn_outside_window() {
        // limit=1 pages in only the assistant turn (idx 0); the tool_result turn
        // (idx 1) is outside the window. The full-thread scan must still find it.
        let sid = "dddddddd-1111-2222-3333-444444444444";
        let tmp = setup_claude_dir(sid, PROJECT_PATH, &tool_result_session_jsonl(sid));
        let server = make_server(&tmp);
        let text = unwrap_output(
            server
                .get_session_messages(GetSessionMessagesRequest {
                    session_id: sid.to_string(),
                    provider: None,
                    detail: Some("full".to_string()),
                    message_type: None,
                    limit: Some(1),
                    offset: None,
                    reverse: None,
                    include_thinking: None,
                    chain_aware: None,
                    after_timestamp: None,
                    before_timestamp: None,
                    include_subagent_transcripts: None,
                    chunk: None,
                    errors_only: None,
                    max_text_len: None,
                })
                .await,
        );
        assert_eq!(
            text.matches("\"index\"").count(),
            1,
            "expected a 1-entry page"
        );
        assert!(
            text.contains("FILE_CONTENTS_MARKER"),
            "result not surfaced when its turn is outside the window"
        );
    }

    #[tokio::test]
    async fn test_full_detail_dedups_matched_subagent_preview() {
        // One Agent call whose sidecar carries the spawning toolUseId, so it
        // matches exactly and gets a subagent_result_preview. The generic
        // result_preview (from the parent tool_result) must then be suppressed.
        let sid = "cccccccc-1111-2222-3333-444444444444";
        let assistant = format!(
            r#"{{"type":"assistant","uuid":"a1","parentUuid":null,"timestamp":"2026-06-09T18:00:00Z","sessionId":"{sid}","version":"2.1.0","isSidechain":false,"message":{{"id":"m1","type":"message","role":"assistant","model":"claude","content":[{{"type":"tool_use","id":"toolu_A","name":"Agent","input":{{"description":"d","subagent_type":"Explore","prompt":"p"}}}}]}}}}"#
        );
        let user = format!(
            r#"{{"type":"user","uuid":"u2","parentUuid":"a1","timestamp":"2026-06-09T18:00:02Z","sessionId":"{sid}","version":"2.1.0","isSidechain":false,"message":{{"role":"user","content":[{{"type":"tool_result","tool_use_id":"toolu_A","content":[{{"type":"text","text":"PARENT_RESULT_MARKER"}}]}}]}}}}"#
        );
        let main = format!("{assistant}\n{user}\n");
        let tmp = setup_claude_dir(sid, PROJECT_PATH, &main);

        // Sidecar transcript + meta with the exact toolUseId for a Pass-1 match.
        let sub_dir = tmp
            .path()
            .join("projects")
            .join(encode_project_path(PROJECT_PATH))
            .join(sid)
            .join("subagents");
        std::fs::create_dir_all(&sub_dir).unwrap();
        std::fs::write(
            sub_dir.join("agent-1.jsonl"),
            format!(
                "{}\n",
                r#"{"type":"assistant","uuid":"s1","parentUuid":null,"timestamp":"2026-06-09T18:00:01Z","sessionId":"agent-1","version":"2.1.0","isSidechain":true,"message":{"id":"sm1","type":"message","role":"assistant","model":"claude","content":[{"type":"text","text":"SUBAGENT_REPORT_MARKER"}]}}"#
            ),
        )
        .unwrap();
        std::fs::write(
            sub_dir.join("agent-1.meta.json"),
            r#"{"agentType":"Explore","description":"d","toolUseId":"toolu_A"}"#,
        )
        .unwrap();

        let server = make_server(&tmp);
        let text = unwrap_output(
            server
                .get_session_messages(GetSessionMessagesRequest {
                    session_id: sid.to_string(),
                    provider: None,
                    detail: Some("full".to_string()),
                    message_type: None,
                    limit: None,
                    offset: None,
                    reverse: None,
                    include_thinking: None,
                    chain_aware: None,
                    after_timestamp: None,
                    before_timestamp: None,
                    include_subagent_transcripts: None,
                    chunk: None,
                    errors_only: None,
                    max_text_len: None,
                })
                .await,
        );
        let v: serde_json::Value = serde_json::from_str(&text).unwrap();
        let messages = v["messages"].as_array().unwrap();
        let td = messages
            .iter()
            .flat_map(|m| m["tool_details"].as_array().into_iter().flatten())
            .find(|td| td["tool_name"] == "Agent")
            .expect("Agent tool_detail not found");
        // The richer subagent preview is present...
        assert!(
            td["subagent_result_preview"]
                .as_str()
                .unwrap_or("")
                .contains("SUBAGENT_REPORT_MARKER"),
            "subagent_result_preview missing for matched Agent"
        );
        // ...and the generic result_preview is suppressed (no duplication).
        assert!(
            td.get("result_preview").is_none() || td["result_preview"].is_null(),
            "result_preview should be deduped when subagent preview is present"
        );
    }

    // ========================================================================
    // Provider routing (B2)
    // ========================================================================

    #[tokio::test]
    async fn classic_list_sessions_carries_provider_and_qualified_id() {
        let sid = "abcdabcd-1111-2222-3333-444455556666";
        let tmp = setup_claude_dir(sid, PROJECT_PATH, &minimal_session_jsonl(sid));
        let server = make_server(&tmp);
        let text = unwrap_output(
            server
                .list_sessions(ListSessionsRequest {
                    project: None,
                    limit: None,
                    include_subagents: None,
                    provider: None,
                })
                .await,
        );
        let v: serde_json::Value = serde_json::from_str(&text).unwrap();
        let row = v.as_array().unwrap().first().expect("one session").clone();
        assert_eq!(row["provider"], "claude-code");
        assert_eq!(row["qualified_id"], format!("claude-code:{sid}"));
    }

    #[tokio::test]
    async fn provider_list_sessions_neutral_rows_and_unknown_provider_error() {
        let sid = "abcdabcd-aaaa-bbbb-cccc-444455556666";
        let tmp = setup_claude_dir(sid, PROJECT_PATH, &minimal_session_jsonl(sid));
        let server = make_server(&tmp);
        let text = unwrap_output(
            server
                .list_sessions(ListSessionsRequest {
                    project: None,
                    limit: None,
                    include_subagents: None,
                    provider: Some(vec!["claude-code".to_string()]),
                })
                .await,
        );
        let v: serde_json::Value = serde_json::from_str(&text).unwrap();
        let rows = v["sessions"].as_array().unwrap();
        assert!(rows
            .iter()
            .any(|r| r["qualified_id"] == format!("claude-code:{sid}")));

        // Unknown provider: an error naming the known set, never a fallback.
        let err = server
            .list_sessions(ListSessionsRequest {
                project: None,
                limit: None,
                include_subagents: None,
                provider: Some(vec!["gemini".to_string()]),
            })
            .await;
        let msg = format!("{err:?}");
        assert!(
            msg.contains("gemini") && msg.contains("claude-code"),
            "got: {msg}"
        );
    }

    #[tokio::test]
    async fn classic_get_session_info_carries_qualified_id() {
        let sid = "abcdabcd-1234-5678-9abc-def012345678";
        let tmp = setup_claude_dir(sid, PROJECT_PATH, &minimal_session_jsonl(sid));
        let server = make_server(&tmp);
        let text = unwrap_output(
            server
                .get_session_info(GetSessionInfoRequest {
                    session_id: sid.to_string(),
                    provider: None,
                })
                .await,
        );
        let v: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(v["provider"], "claude-code");
        assert_eq!(v["qualified_id"], format!("claude-code:{sid}"));
    }

    /// Contract pin for the approved pre-1.0 exception to invariant #8:
    /// `get_session_digest`'s `formatted` field is opt-in. All three inputs
    /// are pinned so the default can never silently flip.
    #[tokio::test]
    async fn get_session_digest_formatted_is_opt_in_across_all_three_inputs() {
        let sid = "dddddddd-1111-2222-3333-444455556666";
        let tmp = setup_claude_dir(sid, PROJECT_PATH, &minimal_session_jsonl(sid));
        let server = make_server(&tmp);
        let digest = |include_formatted: Option<bool>| GetSessionDigestRequest {
            session_id: sid.to_string(),
            provider: None,
            max_prompts: None,
            max_files: None,
            include_formatted,
        };

        // omitted -> absent
        let omitted: serde_json::Value = serde_json::from_str(&unwrap_output(
            server.get_session_digest(digest(None)).await,
        ))
        .unwrap();
        assert!(
            omitted.get("formatted").is_none(),
            "omitted include_formatted must leave formatted absent"
        );

        // explicit false -> absent
        let off: serde_json::Value = serde_json::from_str(&unwrap_output(
            server.get_session_digest(digest(Some(false))).await,
        ))
        .unwrap();
        assert!(
            off.get("formatted").is_none(),
            "include_formatted=false must leave formatted absent"
        );

        // explicit true -> populated string
        let on: serde_json::Value = serde_json::from_str(&unwrap_output(
            server.get_session_digest(digest(Some(true))).await,
        ))
        .unwrap();
        assert!(
            on["formatted"].as_str().is_some_and(|t| !t.is_empty()),
            "include_formatted=true must populate a non-empty formatted string"
        );
    }

    #[tokio::test]
    async fn qualified_get_session_info_routes_to_provider_neutral_view() {
        let sid = "abcdabcd-9999-8888-7777-def012345678";
        let tmp = setup_claude_dir(sid, PROJECT_PATH, &minimal_session_jsonl(sid));
        let server = make_server(&tmp);
        let text = unwrap_output(
            server
                .get_session_info(GetSessionInfoRequest {
                    session_id: format!("claude-code:{sid}"),
                    provider: None,
                })
                .await,
        );
        let v: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(v["provider"], "claude-code");
        assert_eq!(v["qualified_id"], format!("claude-code:{sid}"));
        assert!(v["entries"].as_u64().unwrap() > 0);
        assert!(v["capabilities"]["raw_jsonl"].as_bool().unwrap());
    }

    #[tokio::test]
    async fn provider_list_sessions_honors_project_and_subagent_scope() {
        let sid = "abcdabcd-5555-6666-7777-888899990000";
        let tmp = setup_claude_dir(sid, PROJECT_PATH, &minimal_session_jsonl(sid));
        let subagents = tmp
            .path()
            .join("projects")
            .join(encode_project_path(PROJECT_PATH))
            .join(sid)
            .join("subagents");
        std::fs::create_dir_all(&subagents).unwrap();
        std::fs::write(
            subagents.join("agent-scope.jsonl"),
            format!(
                "{}\n",
                r#"{"type":"assistant","uuid":"scope-a","parentUuid":null,"timestamp":"2026-06-09T18:00:01Z","sessionId":"agent-scope","version":"2.1.0","isSidechain":true,"message":{"id":"scope-m","type":"message","role":"assistant","model":"claude","content":[{"type":"text","text":"scoped subagent"}]}}"#
            ),
        )
        .unwrap();
        std::fs::write(
            subagents.join("agent-scope.meta.json"),
            r#"{"agentType":"Explore","description":"scope test"}"#,
        )
        .unwrap();
        let server = make_server(&tmp);

        let filtered = unwrap_output(
            server
                .list_sessions(ListSessionsRequest {
                    project: Some("test-project".to_string()),
                    limit: None,
                    include_subagents: Some(false),
                    provider: Some(vec!["claude-code".to_string()]),
                })
                .await,
        );
        let filtered: serde_json::Value = serde_json::from_str(&filtered).unwrap();
        assert_eq!(filtered["sessions"].as_array().unwrap().len(), 1);
        assert_eq!(
            filtered["sessions"][0]["qualified_id"],
            format!("claude-code:{sid}")
        );
        assert_eq!(filtered["sessions"][0]["is_subagent"], false);
        assert!(filtered["sessions"][0]["project_key"]
            .as_str()
            .unwrap()
            .contains("test-project"));

        let included = unwrap_output(
            server
                .list_sessions(ListSessionsRequest {
                    project: Some("does-not-match".to_string()),
                    limit: None,
                    include_subagents: Some(true),
                    provider: Some(vec!["claude-code".to_string()]),
                })
                .await,
        );
        let included: serde_json::Value = serde_json::from_str(&included).unwrap();
        assert_eq!(included["sessions"].as_array().unwrap().len(), 0);

        let included = unwrap_output(
            server
                .list_sessions(ListSessionsRequest {
                    project: Some("test-project".to_string()),
                    limit: None,
                    include_subagents: Some(true),
                    provider: Some(vec!["claude-code".to_string()]),
                })
                .await,
        );
        let included: serde_json::Value = serde_json::from_str(&included).unwrap();
        assert_eq!(included["sessions"].as_array().unwrap().len(), 2);
        assert!(included["sessions"]
            .as_array()
            .unwrap()
            .iter()
            .any(|row| row["is_subagent"] == true));
    }

    #[tokio::test]
    async fn provider_listing_limit_semantics_match_classic() {
        // Round-19 blocker 3: `limit: 0` must mean ZERO rows on the
        // provider route exactly as on the classic route — never unlimited.
        let sid = "abcdabcd-2222-3333-4444-555566667777";
        let tmp = setup_claude_dir(sid, PROJECT_PATH, &minimal_session_jsonl(sid));
        let server = make_server(&tmp);
        for limit in [0usize, 1, 999] {
            let classic = unwrap_output(
                server
                    .list_sessions(ListSessionsRequest {
                        project: None,
                        limit: Some(limit),
                        include_subagents: None,
                        provider: None,
                    })
                    .await,
            );
            let classic: serde_json::Value = serde_json::from_str(&classic).unwrap();
            let classic_count = classic.as_array().unwrap().len();

            let routed = unwrap_output(
                server
                    .list_sessions(ListSessionsRequest {
                        project: None,
                        limit: Some(limit),
                        include_subagents: None,
                        provider: Some(vec!["claude-code".to_string()]),
                    })
                    .await,
            );
            let routed: serde_json::Value = serde_json::from_str(&routed).unwrap();
            let routed_count = routed["sessions"].as_array().unwrap().len();
            assert_eq!(
                classic_count, routed_count,
                "limit {limit}: classic returned {classic_count}, provider route {routed_count}"
            );
        }
    }
}
