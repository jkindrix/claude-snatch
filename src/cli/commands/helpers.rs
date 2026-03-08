//! Shared helper functions for CLI commands.
//!
//! Extracts common logic used across thread, detect, conflicts, and decisions commands.

use std::sync::LazyLock;

use chrono::{DateTime, Utc};
use regex::Regex;

use crate::cli::Cli;
use crate::discovery::{Project, Session};
use crate::error::{Result, SnatchError};
use crate::model::{ContentBlock, LogEntry};

use super::get_claude_dir;

/// Extract visible text from a LogEntry (user or assistant).
pub fn extract_text(entry: &LogEntry) -> Option<String> {
    match entry {
        LogEntry::User(user) => {
            let text = match &user.message {
                crate::model::UserContent::Simple(s) => s.content.clone(),
                crate::model::UserContent::Blocks(b) => b
                    .content
                    .iter()
                    .filter_map(|c| {
                        if let ContentBlock::Text(t) = c {
                            Some(t.text.as_str())
                        } else {
                            None
                        }
                    })
                    .collect::<Vec<_>>()
                    .join("\n"),
            };
            if text.trim().is_empty() {
                None
            } else {
                Some(text)
            }
        }
        LogEntry::Assistant(assistant) => {
            let texts: Vec<&str> = assistant
                .message
                .content
                .iter()
                .filter_map(|block| {
                    if let ContentBlock::Text(t) = block {
                        Some(t.text.as_str())
                    } else {
                        None
                    }
                })
                .collect();
            let joined = texts.join("\n");
            if joined.trim().is_empty() {
                None
            } else {
                Some(joined)
            }
        }
        _ => None,
    }
}

/// Extract thinking text from an assistant entry.
pub fn extract_thinking_text(entry: &LogEntry) -> Option<String> {
    if let LogEntry::Assistant(assistant) = entry {
        let texts: Vec<&str> = assistant
            .message
            .content
            .iter()
            .filter_map(|block| {
                if let ContentBlock::Thinking(t) = block {
                    Some(t.thinking.as_str())
                } else {
                    None
                }
            })
            .collect();
        let joined = texts.join("\n");
        if joined.trim().is_empty() {
            None
        } else {
            Some(joined)
        }
    } else {
        None
    }
}

/// Check if an assistant entry contains tool use calls.
pub fn has_tool_calls(entry: &LogEntry) -> bool {
    if let LogEntry::Assistant(assistant) = entry {
        assistant
            .message
            .content
            .iter()
            .any(|block| matches!(block, ContentBlock::ToolUse(_)))
    } else {
        false
    }
}

/// Truncate text to max_chars at a character boundary, appending "..." if truncated.
pub fn truncate(text: &str, max_chars: usize) -> String {
    if text.len() <= max_chars {
        text.to_string()
    } else {
        let boundary = text
            .char_indices()
            .nth(max_chars)
            .map(|(i, _)| i)
            .unwrap_or(text.len());
        format!("{}...", &text[..boundary])
    }
}

/// Check if text looks like a question (interrogative).
///
/// Checks for question marks (excluding those in code/URLs) and question-word starters.
pub fn is_interrogative(text: &str) -> bool {
    let trimmed = text.trim();
    let lower = trimmed.to_lowercase();

    // Check for question marks, but skip those likely in code/URLs/regex
    // A question mark at end of a line (after trimming) is likely a real question
    for line in trimmed.lines() {
        let line = line.trim();
        // Skip lines that look like code, URLs, or file paths
        if line.starts_with("```")
            || line.starts_with("//")
            || line.starts_with('#')
            || line.starts_with("http")
            || line.contains("://")
            || line.starts_with('$')
            || line.starts_with('>')
        {
            continue;
        }
        if line.ends_with('?') {
            return true;
        }
    }

    // Starts with question words (case-insensitive)
    let question_starters = [
        "what ", "how ", "should ", "can ", "could ", "would ", "will ",
        "is ", "are ", "do ", "does ", "which ", "where ", "when ", "why ",
        "shall ", "have you ", "did ",
    ];

    question_starters.iter().any(|q| lower.starts_with(q))
}

/// Check if assistant response contains enumeration/options patterns.
///
/// Requires comparison/alternative language alongside lists to reduce false positives
/// from simple step-by-step instructions.
pub fn has_options_pattern(text: &str) -> bool {
    let lower = text.to_lowercase();

    // Comparison/deliberation language that distinguishes options from instructions
    let has_deliberation = lower.contains("alternatively")
        || lower.contains("or we could")
        || lower.contains("another approach")
        || lower.contains("another option")
        || lower.contains("we could also")
        || lower.contains("you could also")
        || lower.contains("versus")
        || lower.contains(" vs ")
        || lower.contains(" vs.")
        || lower.contains("trade-off")
        || lower.contains("tradeoff")
        || lower.contains("on the other hand")
        || lower.contains("the downside")
        || lower.contains("the upside")
        || lower.contains("compared to")
        || lower.contains("either way")
        || lower.contains("pros:")
        || lower.contains("cons:")
        || lower.contains("however,")
        || lower.contains("recommend")
        || lower.contains("i'd suggest")
        || lower.contains("i would suggest")
        || lower.contains("the best approach")
        || lower.contains("prefer");

    // Option A/B or approach 1/2 — these are strong signals on their own
    if (lower.contains("option a") && lower.contains("option b"))
        || (lower.contains("approach 1") && lower.contains("approach 2"))
        || (lower.contains("option 1") && lower.contains("option 2"))
    {
        return true;
    }

    // Pros/cons patterns — strong signal
    if (lower.contains("pros:") && lower.contains("cons:"))
        || (lower.contains("advantages") && lower.contains("disadvantages"))
    {
        return true;
    }

    // Numbered lists: 2 items + deliberation, or 3+ items on their own
    static NUMBERED: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"(?m)^\s*\d+[\.\)]\s+").unwrap());
    let numbered_count = NUMBERED.find_iter(text).count();
    if numbered_count >= 3 || (numbered_count >= 2 && has_deliberation) {
        return true;
    }

    // Bullet lists: 3 items + deliberation, or 4+ items on their own
    static BULLETS: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"(?m)^\s*[-*]\s+").unwrap());
    let bullet_count = BULLETS.find_iter(text).count();
    if (bullet_count >= 4) || (bullet_count >= 3 && has_deliberation) {
        return true;
    }

    false
}

/// Check if user response is a short affirmative (decision confirmation).
pub fn is_affirmative(text: &str) -> bool {
    let trimmed = text.trim();
    let lower = trimmed.to_lowercase();
    let word_count = trimmed.split_whitespace().count();

    // Direct affirmatives
    let affirmatives = [
        "yes", "yeah", "yep", "yup", "sure", "ok", "okay", "sounds good",
        "go for it", "do it", "let's do it", "let's go", "perfect",
        "exactly", "agreed", "correct", "right", "absolutely",
        "that works", "makes sense", "go ahead", "proceed",
        "i agree", "i like", "i think so", "definitely",
    ];
    if affirmatives.iter().any(|a| lower.starts_with(a)) {
        return true;
    }

    // "Option A/B/1/2" or "let's go with" patterns
    let choice_patterns = [
        "option ", "approach ", "let's go with", "go with ",
        "i prefer", "i'd prefer", "i'll go with", "let's use",
        "i choose", "i pick",
    ];
    if choice_patterns.iter().any(|p| lower.starts_with(p)) {
        return true;
    }

    // Short responses (under 30 words) that aren't questions
    if word_count <= 30 && !trimmed.contains('?') {
        if lower.contains("agree") || lower.contains("go with")
            || lower.contains("let's") || lower.contains("sounds")
            || lower.contains("perfect") || lower.contains("great")
        {
            return true;
        }
    }

    false
}

/// Check if an exchange looks like a decision point.
///
/// Returns true if the assistant text contains structural decision patterns
/// (options/tradeoffs) or explicit decision markers.
pub fn looks_like_decision(text: &str) -> bool {
    // Check structural pattern: options/tradeoffs discussed
    if has_options_pattern(text) {
        return true;
    }

    // Check explicit decision markers
    let lower = text.to_lowercase();
    let decision_markers = [
        "we decided", "design decision", "the decision is",
        "final decision", "agreed to", "agreed that", "agreed on",
        "after discussion", "after considering",
    ];
    if decision_markers.iter().any(|m| lower.contains(m)) {
        return true;
    }

    // Check "decided to" with subject (tighter pattern)
    static DECIDED_TO_RE: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"(?i)(?:we|they|team) decided to").unwrap());
    if DECIDED_TO_RE.is_match(text) {
        return true;
    }

    false
}

/// Filter projects by a filter string with smart matching.
///
/// If the filter exactly matches a decoded path or its last segment,
/// returns only that project. Otherwise falls back to substring matching
/// across decoded paths and encoded names.
pub fn filter_projects(projects: Vec<Project>, filter: &str) -> Vec<Project> {
    // Exact full-path match
    let exact: Vec<_> = projects
        .iter()
        .enumerate()
        .filter(|(_, p)| p.decoded_path() == filter)
        .map(|(i, _)| i)
        .collect();
    if exact.len() == 1 {
        let idx = exact[0];
        return vec![projects.into_iter().nth(idx).unwrap()];
    }

    // Exact trailing segment match: path ends with "/<filter>"
    let suffix = format!("/{filter}");
    let trailing: Vec<_> = projects
        .iter()
        .enumerate()
        .filter(|(_, p)| p.decoded_path().ends_with(&suffix))
        .map(|(i, _)| i)
        .collect();
    if trailing.len() == 1 {
        let idx = trailing[0];
        return vec![projects.into_iter().nth(idx).unwrap()];
    }

    // Fall back to substring match
    projects
        .into_iter()
        .filter(|p| {
            p.decoded_path().contains(filter) || p.encoded_name().contains(filter)
        })
        .collect()
}

/// Resolve a single project from a filter string.
///
/// Uses `filter_projects` for smart matching, then requires exactly one result.
/// Returns an error if zero or multiple projects match.
pub fn resolve_single_project(cli: &Cli, filter: &str) -> Result<crate::discovery::Project> {
    let claude_dir = get_claude_dir(cli.claude_dir.as_ref())?;
    let projects = claude_dir.projects()?;
    let mut matches = filter_projects(projects, filter);

    match matches.len() {
        0 => Err(SnatchError::ProjectNotFound {
            project_path: format!("No project matching '{filter}'"),
        }),
        1 => Ok(matches.remove(0)),
        n => {
            let names: Vec<_> = matches.iter().map(|p| p.decoded_path().to_string()).collect();
            Err(SnatchError::InvalidArgument {
                name: "project".into(),
                reason: format!(
                    "Ambiguous filter '{filter}' matches {n} projects: {}",
                    names.join(", ")
                ),
            })
        }
    }
}

/// Common session collection parameters.
pub struct SessionCollectParams<'a> {
    /// Filter to a single session by ID.
    pub session: Option<&'a str>,
    /// Filter to sessions matching this project path substring.
    pub project: Option<&'a str>,
    /// Only sessions modified after this date/duration string.
    pub since: Option<&'a str>,
    /// Only sessions modified before this date/duration string.
    pub until: Option<&'a str>,
    /// Take the N most recently modified sessions.
    pub recent: Option<usize>,
    /// Exclude subagent sessions.
    pub no_subagents: bool,
}

/// Collect sessions matching common filter parameters.
pub fn collect_sessions(cli: &Cli, params: &SessionCollectParams) -> Result<Vec<Session>> {
    let claude_dir = get_claude_dir(cli.claude_dir.as_ref())?;

    let mut sessions = if let Some(session_id) = params.session {
        let session = claude_dir
            .find_session(session_id)?
            .ok_or_else(|| SnatchError::SessionNotFound {
                session_id: session_id.to_string(),
            })?;
        vec![session]
    } else if let Some(project_filter) = params.project {
        let projects = claude_dir.projects()?;
        let matched = filter_projects(projects, project_filter);
        let mut sess = Vec::new();
        for project in matched {
            sess.extend(project.sessions()?);
        }
        sess
    } else {
        claude_dir.all_sessions()?
    };

    // Date filters — use content timestamps (not file mtime)
    filter_sessions_by_date(&mut sessions, params.since, params.until)?;

    if let Some(n) = params.recent {
        sessions.sort_by(|a, b| b.modified_time().cmp(&a.modified_time()));
        sessions.truncate(n);
    }

    if params.no_subagents {
        sessions.retain(|s| !s.is_subagent());
    }

    Ok(sessions)
}

/// Filter sessions by date range using content-based timestamps.
///
/// Uses `quick_metadata_cached()` to get the session's actual start/end times
/// from JSONL content rather than file modification time. This correctly handles
/// compacted sessions whose file mtime reflects the compaction time, not when
/// the conversation actually occurred.
///
/// A session is retained if its time range overlaps [since, until]:
/// - `session.end_time >= since` (session has content at or after `since`)
/// - `session.start_time <= until` (session has content at or before `until`)
pub fn filter_sessions_by_date(
    sessions: &mut Vec<Session>,
    since: Option<&str>,
    until: Option<&str>,
) -> Result<()> {
    let since_dt: Option<DateTime<Utc>> = if let Some(since) = since {
        Some(DateTime::from(super::parse_date_filter(since)?))
    } else {
        None
    };
    let until_dt: Option<DateTime<Utc>> = if let Some(until) = until {
        Some(DateTime::from(super::parse_date_filter(until)?))
    } else {
        None
    };
    if since_dt.is_some() || until_dt.is_some() {
        sessions.retain(|s| {
            let (start, end) = match s.quick_metadata_cached() {
                Ok(meta) => {
                    let start = meta.start_time.unwrap_or_else(|| DateTime::from(s.modified_time()));
                    let end = meta.end_time.unwrap_or_else(|| DateTime::from(s.modified_time()));
                    (start, end)
                }
                Err(_) => {
                    let mt = DateTime::from(s.modified_time());
                    (mt, mt)
                }
            };
            if let Some(since) = since_dt {
                if end < since {
                    return false;
                }
            }
            if let Some(until) = until_dt {
                if start > until {
                    return false;
                }
            }
            true
        });
    }
    Ok(())
}

/// Short session ID (first 8 chars). Safe for ASCII hex UUIDs.
pub fn short_id(id: &str) -> &str {
    if id.len() > 8 { &id[..8] } else { id }
}

/// Filter main-thread user+assistant entries from a parsed session.
pub fn main_thread_entries(entries: &[LogEntry]) -> Vec<&LogEntry> {
    entries
        .iter()
        .filter(|e| !e.is_sidechain())
        .filter(|e| matches!(e, LogEntry::User(_) | LogEntry::Assistant(_)))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    // ─── is_interrogative ───────────────────────────────────────────

    #[test]
    fn test_interrogative_question_mark() {
        assert!(is_interrogative("Should we use Drop?"));
        assert!(is_interrogative("what do you think?"));
    }

    #[test]
    fn test_interrogative_question_word() {
        assert!(is_interrogative("How should we handle this"));
        assert!(is_interrogative("What is the best approach"));
        assert!(is_interrogative("Should we proceed"));
    }

    #[test]
    fn test_interrogative_code_false_positive() {
        // Question marks in code/URLs should not trigger
        assert!(!is_interrogative("```\nfoo?.bar\n```"));
        assert!(!is_interrogative("https://example.com?q=test"));
        assert!(!is_interrogative("// is this a comment?"));
    }

    #[test]
    fn test_interrogative_not_question() {
        assert!(!is_interrogative("The implementation is ready."));
        assert!(!is_interrogative("Let's proceed with the refactor."));
        assert!(!is_interrogative("Build and deploy the service."));
    }

    // ─── is_affirmative ─────────────────────────────────────────────

    #[test]
    fn test_affirmative_direct() {
        assert!(is_affirmative("yes"));
        assert!(is_affirmative("Yeah, let's do that"));
        assert!(is_affirmative("Sounds good to me"));
        assert!(is_affirmative("Go for it"));
        assert!(is_affirmative("Absolutely"));
    }

    #[test]
    fn test_affirmative_choice() {
        assert!(is_affirmative("Option A"));
        assert!(is_affirmative("let's go with approach 2"));
        assert!(is_affirmative("I prefer the first one"));
    }

    #[test]
    fn test_affirmative_short_positive() {
        assert!(is_affirmative("I agree with that approach"));
        assert!(is_affirmative("great, let's do it"));
    }

    #[test]
    fn test_affirmative_not_affirmative() {
        assert!(!is_affirmative("No, I don't think so"));
        assert!(!is_affirmative("What about the other approach?"));
        assert!(!is_affirmative("I need to think about this more. There are several factors to consider and I'm not sure which direction we should go. Let me review the options again and get back to you with my thoughts."));
    }

    // ─── has_options_pattern ────────────────────────────────────────

    #[test]
    fn test_options_explicit_ab() {
        assert!(has_options_pattern("Option A: use traits\nOption B: use structs"));
    }

    #[test]
    fn test_options_pros_cons() {
        assert!(has_options_pattern("Pros: fast\nCons: complex"));
        assert!(has_options_pattern("Advantages: simple\nDisadvantages: slow"));
    }

    #[test]
    fn test_options_numbered_with_deliberation() {
        let text = "1. Use traits\n2. Use structs\nAlternatively, we could use enums.";
        assert!(has_options_pattern(text));
    }

    #[test]
    fn test_options_numbered_three_plus_matches() {
        // 3+ numbered items match even without deliberation (may be options)
        let text = "1. Use traits\n2. Use structs\n3. Use enums";
        assert!(has_options_pattern(text));
    }

    #[test]
    fn test_options_numbered_two_without_deliberation_rejected() {
        // Only 2 numbered items without deliberation should NOT match
        let text = "1. Read the file\n2. Edit the function";
        assert!(!has_options_pattern(text));
    }

    #[test]
    fn test_options_bullet_instructions_rejected() {
        // Bullet lists that are instructions, not options
        let text = "- First, install the package\n- Then configure it\n- Finally run the tests";
        assert!(!has_options_pattern(text));
    }

    #[test]
    fn test_options_bullet_with_alternatives() {
        let text = "- Use traits for polymorphism\n- Use enums for closed sets\n- Or we could use generics";
        assert!(has_options_pattern(text));
    }

    // ─── has_tool_calls ─────────────────────────────────────────────

    // (requires constructing LogEntry which is complex; tested via integration)

    // ─── truncate ───────────────────────────────────────────────────

    #[test]
    fn test_truncate_short() {
        assert_eq!(truncate("hello", 10), "hello");
    }

    #[test]
    fn test_truncate_exact() {
        assert_eq!(truncate("hello", 5), "hello");
    }

    #[test]
    fn test_truncate_long() {
        assert_eq!(truncate("hello world", 5), "hello...");
    }

    #[test]
    fn test_truncate_unicode() {
        // Em dash is 3 bytes — should not panic
        let text = "hello — world";
        let result = truncate(text, 6);
        assert!(result.ends_with("..."));
        // Should not panic
        let _ = truncate(text, 7);
    }

    #[test]
    fn test_truncate_multibyte() {
        let text = "café résumé naïve";
        let result = truncate(text, 4);
        assert_eq!(result, "café...");
    }

    // ─── short_id ───────────────────────────────────────────────────

    #[test]
    fn test_short_id() {
        assert_eq!(short_id("abcdef1234567890"), "abcdef12");
        assert_eq!(short_id("abc"), "abc");
        assert_eq!(short_id(""), "");
    }
}
