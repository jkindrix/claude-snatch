//! Cross-session topic threading.
//!
//! Searches sessions for a regex pattern and returns chronologically-ordered
//! exchanges with surrounding user/assistant conversation context.
//! Used by both CLI `thread` and MCP `thread_topic` tools.

use std::collections::HashSet;

use chrono::{DateTime, Utc};
use regex::Regex;
use serde::Serialize;

use crate::analysis::extraction::{
    entry_content_segments, extract_visible_text, markdown_content_segments, ContentProvenance,
};
use crate::cli::helpers::{
    extract_thinking_text, looks_like_decision, main_thread_entries, short_id,
};
use crate::discovery::Session;
use crate::reconstruction::Conversation;

/// Parameters for topic threading.
pub struct ThreadParams {
    /// Include thinking/reasoning blocks in search and output.
    pub include_thinking: bool,
    /// Maximum results to return.
    pub limit: usize,
    /// Maximum characters for user context text.
    pub max_user_context: usize,
    /// Maximum characters for assistant context text.
    pub max_assistant_context: usize,
    /// Maximum characters for thinking context text.
    pub max_thinking_context: usize,
    /// Filter to specific message role ("user" or "assistant").
    pub role_filter: Option<String>,
    /// Only include exchanges that look like decision points.
    pub decisions_only: bool,
}

impl Default for ThreadParams {
    fn default() -> Self {
        Self {
            include_thinking: false,
            limit: 30,
            max_user_context: 500,
            max_assistant_context: 500,
            max_thinking_context: 500,
            role_filter: None,
            decisions_only: false,
        }
    }
}

/// A threaded exchange: a match with surrounding conversation context.
#[derive(Debug, Serialize)]
#[allow(missing_docs)]
pub struct ThreadedExchange {
    pub timestamp: DateTime<Utc>,
    pub session_id: String,
    pub provider: String,
    pub qualified_id: String,
    pub short_id: String,
    pub project: String,
    pub entry_uuid: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user_text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub assistant_text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thinking_text: Option<String>,
    pub match_location: String,
    pub match_provenance: ContentProvenance,
    pub match_count: usize,
}

/// Identity/context for one already-parsed conversation supplied to topic
/// threading.
pub struct ThreadConversation<'a> {
    /// Owning provider.
    pub provider: &'a str,
    /// Provider-qualified logical identity.
    pub qualified_id: &'a str,
    /// Native session id retained for compatibility.
    pub session_id: &'a str,
    /// Human-readable project path.
    pub project: &'a str,
    /// Reconstructed conversation.
    pub conversation: &'a Conversation,
    /// Whether prompt semantics are complete enough to classify harness text.
    pub semantic_annotations: bool,
}

/// Result of a threading operation.
pub struct ThreadResult {
    /// Chronologically-ordered exchanges matching the pattern.
    pub exchanges: Vec<ThreadedExchange>,
    /// Number of unique sessions with matches.
    pub session_count: usize,
    /// Total match count across all exchanges.
    pub total_matches: usize,
}

fn narrow_exchanges(exchanges: &mut Vec<ThreadedExchange>, limit: usize) {
    exchanges.sort_by(|a, b| {
        a.match_provenance
            .priority()
            .cmp(&b.match_provenance.priority())
            .then_with(|| a.timestamp.cmp(&b.timestamp))
            .then_with(|| a.session_id.cmp(&b.session_id))
            .then_with(|| a.entry_uuid.cmp(&b.entry_uuid))
    });
    exchanges.truncate(limit);
    exchanges.sort_by(|a, b| {
        a.timestamp
            .cmp(&b.timestamp)
            .then_with(|| a.session_id.cmp(&b.session_id))
            .then_with(|| a.entry_uuid.cmp(&b.entry_uuid))
    });
}

struct ThreadSource<'a> {
    provider: &'a str,
    qualified_id: &'a str,
    session_id: &'a str,
    project: &'a str,
    semantic_conversation: Option<&'a Conversation>,
}

fn collect_exchanges(
    main: &[&crate::model::message::LogEntry],
    source: &ThreadSource<'_>,
    regex: &Regex,
    params: &ThreadParams,
    exchanges: &mut Vec<ThreadedExchange>,
) {
    let mut seen_uuids: HashSet<String> = HashSet::new();

    for (idx, entry) in main.iter().enumerate() {
        let mut match_location = String::new();
        let entry_text = extract_visible_text(entry);
        let thinking_text = params
            .include_thinking
            .then(|| extract_thinking_text(entry))
            .flatten();
        let mut match_count = 0;
        let mut match_provenance: Option<ContentProvenance> = None;

        let forced_injected = matches!(entry, crate::model::message::LogEntry::User(_))
            && source.semantic_conversation.is_some_and(|conversation| {
                entry
                    .uuid()
                    .and_then(|uuid| conversation.semantics_for_uuid(uuid))
                    .and_then(|semantics| semantics.prompt)
                    .is_some_and(|prompt| {
                        prompt.authorship == crate::provider::PromptAuthorship::Harness
                    })
            });
        for mut segment in entry_content_segments(entry) {
            if forced_injected {
                segment.provenance = ContentProvenance::Injected;
            }
            let count = regex.find_iter(&segment.text).count();
            if count > 0 {
                match_count += count;
                match_location = entry.message_type().to_string();
                if match_provenance.map_or(true, |current| {
                    segment.provenance.priority() < current.priority()
                }) {
                    match_provenance = Some(segment.provenance);
                }
            }
        }

        if let Some(ref text) = thinking_text {
            for segment in markdown_content_segments(text) {
                let count = regex.find_iter(&segment.text).count();
                if count > 0 {
                    match_count += count;
                    if match_location.is_empty() {
                        match_location = "thinking".to_string();
                    }
                    if match_provenance.map_or(true, |current| {
                        segment.provenance.priority() < current.priority()
                    }) {
                        match_provenance = Some(segment.provenance);
                    }
                }
            }
        }

        if match_count == 0 {
            continue;
        }
        if params
            .role_filter
            .as_ref()
            .is_some_and(|role| entry.message_type() != role.as_str())
        {
            continue;
        }
        if params.decisions_only {
            let is_decision = entry_text.as_ref().is_some_and(|t| looks_like_decision(t));
            let paired_is_decision = !is_decision
                && entry.message_type() == "user"
                && ((idx + 1)..main.len())
                    .find(|&i| main[i].message_type() == "assistant")
                    .and_then(|i| extract_visible_text(main[i]))
                    .as_ref()
                    .is_some_and(|text| looks_like_decision(text));
            if !is_decision && !paired_is_decision {
                continue;
            }
        }

        let uuid = entry.uuid().unwrap_or("").to_string();
        if !uuid.is_empty() && !seen_uuids.insert(uuid.clone()) {
            continue;
        }
        let timestamp = entry.timestamp().unwrap_or_else(Utc::now);
        let user_text = if entry.message_type() == "user" {
            extract_visible_text(entry)
        } else {
            (0..idx)
                .rev()
                .find(|&i| main[i].message_type() == "user")
                .and_then(|i| extract_visible_text(main[i]))
        };
        let assistant_text = if entry.message_type() == "assistant" {
            extract_visible_text(entry)
        } else {
            ((idx + 1)..main.len())
                .find(|&i| main[i].message_type() == "assistant")
                .and_then(|i| extract_visible_text(main[i]))
        };
        let thinking_text = if params.include_thinking {
            if entry.message_type() == "assistant" {
                extract_thinking_text(entry)
            } else {
                ((idx + 1)..main.len())
                    .find(|&i| main[i].message_type() == "assistant")
                    .and_then(|i| extract_thinking_text(main[i]))
            }
        } else {
            None
        };

        exchanges.push(ThreadedExchange {
            timestamp,
            session_id: source.session_id.to_string(),
            provider: source.provider.to_string(),
            qualified_id: source.qualified_id.to_string(),
            short_id: short_id(source.session_id).to_string(),
            project: source.project.to_string(),
            entry_uuid: uuid,
            user_text,
            assistant_text,
            thinking_text,
            match_location,
            match_provenance: match_provenance.expect("a matching exchange has a provenance"),
            match_count,
        });
    }
}

/// Apply evidence-priority narrowing and compute result totals.
#[must_use]
pub fn finish_thread_exchanges(mut exchanges: Vec<ThreadedExchange>, limit: usize) -> ThreadResult {
    narrow_exchanges(&mut exchanges, limit);
    let session_ids: HashSet<&str> = exchanges.iter().map(|e| e.qualified_id.as_str()).collect();
    let total_matches = exchanges.iter().map(|e| e.match_count).sum();
    ThreadResult {
        session_count: session_ids.len(),
        total_matches,
        exchanges,
    }
}

/// Collect matches from one provider-resolved conversation.
///
/// The global result limit is applied later, so corpus callers can parse and
/// drop one session at a time instead of retaining every conversation.
#[must_use]
pub fn thread_one_conversation(
    session: &ThreadConversation<'_>,
    regex: &Regex,
    params: &ThreadParams,
) -> Vec<ThreadedExchange> {
    let mut exchanges = Vec::new();
    collect_exchanges(
        &session.conversation.main_thread_entries(),
        &ThreadSource {
            provider: session.provider,
            qualified_id: session.qualified_id,
            session_id: session.session_id,
            project: session.project,
            semantic_conversation: session.semantic_annotations.then_some(session.conversation),
        },
        regex,
        params,
        &mut exchanges,
    );
    exchanges
}

/// Thread a topic across provider-resolved conversations.
#[must_use]
pub fn thread_conversations(
    sessions: &[ThreadConversation<'_>],
    regex: &Regex,
    params: &ThreadParams,
) -> ThreadResult {
    let mut exchanges = Vec::new();
    for session in sessions {
        exchanges.extend(thread_one_conversation(session, regex, params));
    }
    finish_thread_exchanges(exchanges, params.limit)
}

/// Run topic threading across a set of sessions.
///
/// Searches each session's main thread for the regex pattern, collects
/// matching exchanges with their surrounding user/assistant context,
/// and returns them in chronological order.
pub fn thread_topic(
    sessions: &[Session],
    regex: &Regex,
    params: &ThreadParams,
    max_file_size: Option<u64>,
) -> ThreadResult {
    let mut exchanges: Vec<ThreadedExchange> = Vec::new();

    for session in sessions {
        let entries = match session.parse_with_options(max_file_size) {
            Ok(e) => e,
            Err(_) => continue,
        };

        let main = main_thread_entries(&entries);
        let qualified = crate::provider::claude_code::logical_key(session).to_string();
        collect_exchanges(
            &main,
            &ThreadSource {
                provider: "claude-code",
                qualified_id: &qualified,
                session_id: session.session_id(),
                project: session.project_path(),
                semantic_conversation: None,
            },
            regex,
            params,
            &mut exchanges,
        );
    }
    finish_thread_exchanges(exchanges, params.limit)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{Datelike, TimeZone};

    fn exchange(day: u32, provenance: ContentProvenance) -> ThreadedExchange {
        ThreadedExchange {
            timestamp: Utc.with_ymd_and_hms(2026, 7, day, 0, 0, 0).unwrap(),
            session_id: format!("s{day}"),
            provider: "claude-code".to_string(),
            qualified_id: format!("claude-code:global:s{day}"),
            short_id: format!("s{day}"),
            project: "project".to_string(),
            entry_uuid: format!("u{day}"),
            user_text: None,
            assistant_text: None,
            thinking_text: None,
            match_location: "user".to_string(),
            match_provenance: provenance,
            match_count: 1,
        }
    }

    #[test]
    fn limiting_prefers_primary_evidence_then_restores_chronology() {
        let mut exchanges = vec![
            exchange(1, ContentProvenance::Injected),
            exchange(2, ContentProvenance::Primary),
            exchange(3, ContentProvenance::Quoted),
            exchange(4, ContentProvenance::Primary),
        ];
        narrow_exchanges(&mut exchanges, 3);
        assert_eq!(
            exchanges
                .iter()
                .map(|exchange| exchange.timestamp.day())
                .collect::<Vec<_>>(),
            vec![2, 3, 4]
        );
        assert!(exchanges
            .iter()
            .all(|exchange| exchange.match_provenance != ContentProvenance::Injected));
    }

    #[test]
    fn secondary_evidence_is_retained_when_it_is_all_that_exists() {
        let mut exchanges = vec![
            exchange(1, ContentProvenance::Injected),
            exchange(2, ContentProvenance::Quoted),
        ];
        narrow_exchanges(&mut exchanges, 5);
        assert_eq!(exchanges.len(), 2);
    }
}
