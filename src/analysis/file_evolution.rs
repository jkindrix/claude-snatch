//! File evolution analysis — explains why a file changed over time.
//!
//! Chains file modification history with conversation context and thinking
//! blocks to produce a chronological narrative of changes to a file.
//!
//! Used by both CLI `file-evolution` and MCP `explain_file_evolution` tools.

use chrono::{DateTime, Utc};

use crate::analysis::extraction::{
    extract_assistant_summary, extract_thinking_text, extract_tool_names, extract_user_prompt_text,
    is_human_prompt, truncate_text,
};
use crate::discovery::Session;
use crate::file_index::FileIndex;
use crate::file_index::{ProviderFileIndex, ProviderFileModification};
use crate::model::message::LogEntry;
use crate::provider::{
    FileChangeEvidence, FileChangeKind, FileChangeOutcome, ParsedSession, PromptAuthorship,
};

/// Parameters for file evolution analysis.
pub struct FileEvolutionParams {
    /// File path pattern (substring match).
    pub file_pattern: String,
    /// Max changes to return.
    pub limit: usize,
    /// Max chars for text fields.
    pub max_text_len: usize,
    /// Include thinking blocks in output.
    pub include_thinking: bool,
    /// Context window (turns before/after the modification).
    pub context_window: usize,
}

impl Default for FileEvolutionParams {
    fn default() -> Self {
        Self {
            file_pattern: String::new(),
            limit: 50,
            max_text_len: 500,
            include_thinking: true,
            context_window: 1,
        }
    }
}

/// A single change event in the file's evolution.
#[derive(Debug, Clone)]
#[allow(missing_docs)]
pub struct ChangeEvent {
    pub timestamp: DateTime<Utc>,
    pub session_id: String,
    pub message_id: String,
    pub version: u32,
    pub user_prompt: Option<String>,
    pub assistant_response: Option<String>,
    pub thinking: Option<String>,
    pub tools_used: Vec<String>,
    pub had_errors: bool,
}

/// Complete file evolution result.
#[derive(Debug)]
#[allow(missing_docs)]
pub struct FileEvolutionResult {
    pub file_path: String,
    pub total_changes: usize,
    pub sessions_involved: usize,
    pub changes: Vec<ChangeEvent>,
}

/// One provider-neutral change/attempt with its nearby conversation context.
#[derive(Debug, Clone)]
#[allow(missing_docs)]
pub struct ProviderChangeEvent {
    pub timestamp: Option<DateTime<Utc>>,
    pub provider: String,
    pub qualified_id: String,
    pub session_id: String,
    pub project_path: String,
    pub entry_id: String,
    pub operation_id: String,
    pub version: Option<u32>,
    pub kind: FileChangeKind,
    pub move_path: Option<String>,
    pub evidence: FileChangeEvidence,
    pub outcome: FileChangeOutcome,
    pub coverage: String,
    pub record_ordinal: u64,
    pub outcome_record_ordinal: Option<u64>,
    pub user_prompt: Option<String>,
    pub assistant_response: Option<String>,
    pub thinking: Option<String>,
    pub tools_used: Vec<String>,
    pub had_errors: bool,
}

/// One file's source-backed provider-neutral evolution and non-applied attempts.
#[derive(Debug)]
#[allow(missing_docs)]
pub struct ProviderFileEvolutionResult {
    pub file_path: String,
    pub total_changes: usize,
    pub total_attempts: usize,
    pub sessions_involved: usize,
    pub changes: Vec<ProviderChangeEvent>,
    pub attempts: Vec<ProviderChangeEvent>,
}

/// Analyze the evolution of a file across sessions.
pub fn analyze_file_evolution(
    sessions: &[Session],
    params: &FileEvolutionParams,
    max_file_size: Option<u64>,
) -> Vec<FileEvolutionResult> {
    let file_index = FileIndex::from_sessions(sessions, max_file_size);

    // Find matching files
    let matches = file_index.search(&params.file_pattern);

    if matches.is_empty() {
        return Vec::new();
    }

    // Build a session lookup for quick access
    let session_map: std::collections::HashMap<&str, &Session> =
        sessions.iter().map(|s| (s.session_id(), s)).collect();

    let mut results = Vec::new();

    for (file_path, modifications) in matches {
        let total_changes = modifications.len();
        let mut unique_sessions: Vec<&str> = modifications
            .iter()
            .map(|m| m.session_id.as_str())
            .collect();
        unique_sessions.sort_unstable();
        unique_sessions.dedup();
        let sessions_involved = unique_sessions.len();

        let mut changes = Vec::new();

        for modification in modifications.iter().take(params.limit) {
            let session = match session_map.get(modification.session_id.as_str()) {
                Some(s) => s,
                None => continue,
            };

            let entries = match session.parse_with_options(max_file_size) {
                Ok(e) => e,
                Err(_) => continue,
            };

            // Find the entry matching the message_id
            let target_idx = entries.iter().position(|e| {
                e.uuid()
                    .map(|u| {
                        u == modification.message_id || u.starts_with(&modification.message_id)
                    })
                    .unwrap_or(false)
            });

            let change = if let Some(idx) = target_idx {
                extract_change_context(&entries, idx, modification, params)
            } else {
                // Message not found — still record the change with minimal info
                ChangeEvent {
                    timestamp: modification.timestamp,
                    session_id: modification.session_id.clone(),
                    message_id: modification.message_id.clone(),
                    version: modification.version,
                    user_prompt: None,
                    assistant_response: None,
                    thinking: None,
                    tools_used: Vec::new(),
                    had_errors: false,
                }
            };

            changes.push(change);
        }

        results.push(FileEvolutionResult {
            file_path: file_path.to_string(),
            total_changes,
            sessions_involved,
            changes,
        });
    }

    results
}

/// Analyze provider-normalized file evidence.
///
/// This does not assume snapshot versions or Claude-shaped message ids. Only
/// source-proven `Applied` observations count as changes; failed, declined,
/// and unknown attempts are returned separately.
pub fn analyze_provider_file_evolution(
    sessions: &[(&str, &ParsedSession)],
    params: &FileEvolutionParams,
) -> Vec<ProviderFileEvolutionResult> {
    let index = ProviderFileIndex::from_parsed_sessions(sessions.iter().copied());
    let session_map: std::collections::BTreeMap<_, _> = sessions
        .iter()
        .map(|(_, parsed)| (&parsed.descriptor.key, *parsed))
        .collect();
    let mut results = Vec::new();

    for (file_path, observations) in index.search(&params.file_pattern) {
        let mut applied: Vec<&ProviderFileModification> = observations
            .iter()
            .filter(|change| change.outcome == FileChangeOutcome::Applied)
            .collect();
        let mut attempts: Vec<&ProviderFileModification> = observations
            .iter()
            .filter(|change| change.outcome != FileChangeOutcome::Applied)
            .collect();
        let total_changes = applied.len();
        let total_attempts = attempts.len();
        let sessions_involved = observations
            .iter()
            .map(|change| &change.session)
            .collect::<std::collections::BTreeSet<_>>()
            .len();
        applied.truncate(params.limit);
        attempts.truncate(params.limit);

        let build = |change: &ProviderFileModification| {
            let parsed = session_map.get(&change.session).copied();
            let context = parsed
                .and_then(|parsed| {
                    parsed
                        .entries
                        .iter()
                        .position(|entry| entry.id == change.entry_id)
                        .map(|index| extract_provider_change_context(parsed, index, params))
                })
                .unwrap_or_default();
            ProviderChangeEvent {
                timestamp: change.timestamp,
                provider: change.session.provider.to_string(),
                qualified_id: change.session.to_string(),
                session_id: change.session.native_id.clone(),
                project_path: change.project_path.clone(),
                entry_id: change.entry_id.to_string(),
                operation_id: change.operation_id.clone(),
                version: change.version,
                kind: change.kind,
                move_path: change.move_path.clone(),
                evidence: change.evidence,
                outcome: change.outcome,
                coverage: change.coverage.clone(),
                record_ordinal: change.record.ordinal,
                outcome_record_ordinal: change.outcome_record.as_ref().map(|r| r.ordinal),
                user_prompt: context.user_prompt,
                assistant_response: context.assistant_response,
                thinking: context.thinking,
                tools_used: context.tools_used,
                had_errors: context.had_errors,
            }
        };
        results.push(ProviderFileEvolutionResult {
            file_path: file_path.to_string(),
            total_changes,
            total_attempts,
            sessions_involved,
            changes: applied.into_iter().map(build).collect(),
            attempts: attempts.into_iter().map(build).collect(),
        });
    }
    results
}

#[derive(Default)]
struct ProviderChangeContext {
    user_prompt: Option<String>,
    assistant_response: Option<String>,
    thinking: Option<String>,
    tools_used: Vec<String>,
    had_errors: bool,
}

fn extract_provider_change_context(
    parsed: &ParsedSession,
    target_idx: usize,
    params: &FileEvolutionParams,
) -> ProviderChangeContext {
    let (start, end) = provider_context_bounds(parsed, target_idx, params.context_window);
    let mut context = ProviderChangeContext::default();
    for identified in &parsed.entries[start..end] {
        let entry = &identified.entry;
        match entry {
            LogEntry::User(user) => {
                let semantic_human = parsed
                    .semantics
                    .get(&identified.id)
                    .and_then(|semantics| semantics.prompt)
                    .is_some_and(|prompt| prompt.authorship == PromptAuthorship::Human);
                if context.user_prompt.is_none() && (semantic_human || is_human_prompt(entry)) {
                    context.user_prompt = extract_user_prompt_text(entry)
                        .map(|text| truncate_text(&text, params.max_text_len));
                }
                context.had_errors |= user
                    .message
                    .tool_results()
                    .iter()
                    .any(|result| result.is_error == Some(true));
            }
            LogEntry::Assistant(_) => {
                if context.assistant_response.is_none() {
                    context.assistant_response =
                        extract_assistant_summary(entry, params.max_text_len);
                }
                if params.include_thinking && context.thinking.is_none() {
                    context.thinking = extract_thinking_text(entry, params.max_text_len);
                }
                for name in extract_tool_names(entry) {
                    if !context.tools_used.contains(&name) {
                        context.tools_used.push(name);
                    }
                }
            }
            _ => {}
        }
    }
    context
}

fn provider_context_bounds(
    parsed: &ParsedSession,
    target_idx: usize,
    context_window: usize,
) -> (usize, usize) {
    let Some(target_turn) = parsed
        .entries
        .get(target_idx)
        .and_then(|entry| parsed.semantics.get(&entry.id))
        .and_then(|semantics| semantics.turn_id.as_deref())
    else {
        return (
            target_idx.saturating_sub(context_window),
            (target_idx + 1 + context_window).min(parsed.entries.len()),
        );
    };
    let mut turns = Vec::new();
    for entry in &parsed.entries {
        let Some(turn) = parsed
            .semantics
            .get(&entry.id)
            .and_then(|semantics| semantics.turn_id.as_deref())
        else {
            continue;
        };
        if !turns.contains(&turn) {
            turns.push(turn);
        }
    }
    let Some(target_turn_index) = turns.iter().position(|turn| *turn == target_turn) else {
        return (target_idx, target_idx + 1);
    };
    let selected_start = target_turn_index.saturating_sub(context_window);
    let selected_end = (target_turn_index + 1 + context_window).min(turns.len());
    let selected: std::collections::BTreeSet<_> = turns[selected_start..selected_end]
        .iter()
        .copied()
        .collect();
    let mut matching = parsed
        .entries
        .iter()
        .enumerate()
        .filter_map(|(index, entry)| {
            parsed
                .semantics
                .get(&entry.id)
                .and_then(|semantics| semantics.turn_id.as_deref())
                .filter(|turn| selected.contains(turn))
                .map(|_| index)
        });
    let Some(start) = matching.next() else {
        return (target_idx, target_idx + 1);
    };
    let end = matching.next_back().unwrap_or(start).saturating_add(1);
    (start, end)
}

/// Extract conversation context around a file modification.
fn extract_change_context(
    entries: &[LogEntry],
    target_idx: usize,
    modification: &crate::file_index::FileModification,
    params: &FileEvolutionParams,
) -> ChangeEvent {
    let window = params.context_window;
    let start = target_idx.saturating_sub(window);
    let end = (target_idx + 1 + window).min(entries.len());

    let mut user_prompt = None;
    let mut assistant_response = None;
    let mut thinking = None;
    let mut tools_used = Vec::new();
    let mut had_errors = false;

    // Scan the window for context
    #[allow(clippy::needless_range_loop)]
    for i in start..end {
        let entry = &entries[i];

        match entry {
            LogEntry::User(_) => {
                if user_prompt.is_none() && is_human_prompt(entry) {
                    user_prompt = extract_user_prompt_text(entry)
                        .map(|t| truncate_text(&t, params.max_text_len));
                }
                // Check for tool errors in user messages (tool results)
                if let LogEntry::User(u) = entry {
                    for result in u.message.tool_results() {
                        if result.is_error == Some(true) {
                            had_errors = true;
                        }
                    }
                }
            }
            LogEntry::Assistant(_) => {
                if assistant_response.is_none() {
                    assistant_response = extract_assistant_summary(entry, params.max_text_len);
                }
                if params.include_thinking && thinking.is_none() {
                    thinking = extract_thinking_text(entry, params.max_text_len);
                }
                let names = extract_tool_names(entry);
                for name in names {
                    if !tools_used.contains(&name) {
                        tools_used.push(name);
                    }
                }
            }
            _ => {}
        }
    }

    ChangeEvent {
        timestamp: modification.timestamp,
        session_id: modification.session_id.clone(),
        message_id: modification.message_id.clone(),
        version: modification.version,
        user_prompt,
        assistant_response,
        thinking,
        tools_used,
        had_errors,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::fake::{multi_artifact_key, FakeProvider};
    use crate::provider::{
        EntryId, FileChangeDetail, FileChangeDiagnostics, FileChangeObservation, RecordRef,
        SourceProvider,
    };

    fn change(
        owner: &EntryId,
        record: &RecordRef,
        change_index: u32,
        path: &str,
        outcome: FileChangeOutcome,
    ) -> FileChangeObservation {
        FileChangeObservation {
            owner: owner.clone(),
            operation_id: "call-7".into(),
            change_index,
            record: record.clone(),
            outcome_record: Some(record.clone()),
            path: path.into(),
            move_path: None,
            kind: FileChangeKind::Update,
            detail: FileChangeDetail::Patch("@@\n-old\n+new\n".into()),
            evidence: FileChangeEvidence::StructuredLifecycle,
            outcome,
            observed_at: Some("2026-01-01T00:00:01Z".parse().unwrap()),
            native_version: None,
        }
    }

    #[test]
    fn provider_evolution_separates_attempts_and_uses_semantic_prompt_context() {
        let provider = FakeProvider;
        let mut parsed = provider.parse(&multi_artifact_key()).unwrap();
        parsed.entries[0].entry = serde_json::from_value(serde_json::json!({
            "type": "user",
            "uuid": "fake-u1",
            "parentUuid": null,
            "timestamp": "2026-01-01T00:00:00Z",
            "sessionId": "42",
            "version": "0.0.0",
            "message": {"role": "user", "content": "please update the file"}
        }))
        .unwrap();
        let owner = parsed.entries[2].id.clone();
        let record = parsed.entry_origins[&owner][0].clone();
        parsed
            .semantics
            .get_mut(&parsed.entries[0].id)
            .unwrap()
            .turn_id = Some("turn-1".into());
        parsed.semantics.get_mut(&owner).unwrap().turn_id = Some("turn-1".into());
        parsed.file_changes = vec![
            change(&owner, &record, 0, "src/lib.rs", FileChangeOutcome::Applied),
            change(&owner, &record, 1, "src/lib.rs", FileChangeOutcome::Failed),
        ];
        parsed.file_change_diagnostics = FileChangeDiagnostics {
            patch_calls: 1,
            calls_with_changes: 1,
            structured_changes: 2,
            ..Default::default()
        };
        assert!(parsed.validate_provenance().is_empty());
        let params = FileEvolutionParams {
            file_pattern: "lib.rs".into(),
            context_window: 0,
            ..Default::default()
        };
        let results = analyze_provider_file_evolution(&[("/work", &parsed)], &params);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].total_changes, 1);
        assert_eq!(results[0].total_attempts, 1);
        assert_eq!(results[0].changes[0].provider, "fake");
        assert_eq!(results[0].changes[0].operation_id, "call-7");
        assert!(results[0].changes[0].user_prompt.is_some());
        assert_eq!(results[0].attempts[0].outcome, FileChangeOutcome::Failed);
    }
}
