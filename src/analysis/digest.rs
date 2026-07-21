//! Session digest: compact summary of a session's key topics.
//!
//! Builds a high-level overview from conversation entries using existing
//! extraction primitives. Designed for hook injection after compaction
//! so Claude knows "what was discussed" without needing the full conversation.

use std::collections::HashMap;

use crate::model::message::LogEntry;
use crate::provider::ToolSemantics;
use crate::reconstruction::Conversation;

use super::lessons::{conversation_tool_semantics, count_tool_failures};

use super::extraction::{
    extract_files_from_tools, extract_thinking_text, extract_tool_names, find_compaction_events,
    is_human_prompt, orientation_text, thinking_redaction_note, truncate_text,
};

fn collapse_consecutive_prompts(prompts: &[String]) -> Vec<String> {
    let mut collapsed: Vec<(String, usize)> = Vec::new();
    for prompt in prompts {
        if let Some((_, count)) = collapsed
            .last_mut()
            .filter(|(existing, _)| existing == prompt)
        {
            *count += 1;
        } else {
            collapsed.push((prompt.clone(), 1));
        }
    }
    collapsed
        .into_iter()
        .map(|(prompt, count)| {
            if count == 1 {
                prompt
            } else {
                format!("{prompt} [repeated {count}×]")
            }
        })
        .collect()
}

/// Options for building a session digest.
#[derive(Debug, Clone)]
pub struct DigestOptions {
    /// Maximum number of key prompts to include.
    pub max_prompts: usize,
    /// Maximum number of files to include.
    pub max_files: usize,
    /// Maximum number of decision keywords to extract.
    pub max_keywords: usize,
    /// Maximum length of the formatted digest text.
    pub max_chars: usize,
}

impl Default for DigestOptions {
    fn default() -> Self {
        Self {
            max_prompts: 3,
            max_files: 10,
            max_keywords: 5,
            max_chars: 1000,
        }
    }
}

/// A compact session digest.
#[derive(Debug, Clone, serde::Serialize)]
pub struct SessionDigest {
    /// Key human prompts (first N, truncated).
    pub key_prompts: Vec<String>,
    /// Recent human prompts (last N, truncated). Shows where the session ended.
    pub recent_prompts: Vec<String>,
    /// Total number of human prompts in the session.
    pub total_prompts: usize,
    /// Files touched by Write/Edit/Read tools.
    pub files_touched: Vec<String>,
    /// Tool usage counts (sorted by frequency).
    pub top_tools: Vec<(String, usize)>,
    /// Number of tool errors in the session.
    pub error_count: usize,
    /// Failures backed by native/status/structured evidence.
    pub confirmed_tool_failures: usize,
    /// Error-like output inferred from unstructured text.
    pub inferred_failure_signals: usize,
    /// Number of compaction events.
    pub compaction_count: usize,
    /// Decision-related keywords from thinking blocks.
    pub thinking_keywords: Vec<String>,
    /// Set when thinking blocks exist but are all empty (recent Claude Code
    /// versions persist only the encrypted signature), explaining why
    /// `thinking_keywords` is empty.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thinking_note: Option<String>,
}

/// Decision-signal regex patterns.
const DECISION_PATTERNS: &[&str] = &[
    r"(?i)\bdecided\b",
    r"(?i)\bchose\b",
    r"(?i)\bbecause\b",
    r"(?i)\binstead of\b",
    r"(?i)\btrade.?off\b",
    r"(?i)\bapproach\b",
    r"(?i)\barchitecture\b",
    r"(?i)\brefactor\b",
];

/// Build a session digest from conversation entries.
pub fn build_digest(entries: &[&LogEntry], opts: &DigestOptions) -> SessionDigest {
    build_digest_with(entries, opts, &HashMap::new(), false, None)
}

/// Build a digest using provider tool semantics retained by a conversation.
#[must_use]
pub fn build_digest_from_conversation(
    conversation: &Conversation,
    opts: &DigestOptions,
    semantic_annotations: bool,
) -> SessionDigest {
    let entries = conversation.chronological_entries();
    let semantics = conversation_tool_semantics(conversation);
    build_digest_with(
        &entries,
        opts,
        &semantics,
        semantic_annotations,
        semantic_annotations.then_some(conversation),
    )
}

fn build_digest_with(
    entries: &[&LogEntry],
    opts: &DigestOptions,
    semantics: &HashMap<String, ToolSemantics>,
    semantic_annotations: bool,
    semantic_conversation: Option<&Conversation>,
) -> SessionDigest {
    // 1. Collect all human prompts, then take first N and last N
    let mut all_prompts = Vec::new();
    for entry in entries {
        let human = semantic_conversation.map_or_else(
            || is_human_prompt(entry),
            |conversation| {
                matches!(entry, LogEntry::User(_))
                    && entry
                        .uuid()
                        .and_then(|uuid| conversation.semantics_for_uuid(uuid))
                        .and_then(|semantics| semantics.prompt)
                        .is_some_and(|prompt| {
                            prompt.authorship == crate::provider::PromptAuthorship::Human
                        })
            },
        );
        if human {
            if let Some(text) = orientation_text(entry) {
                all_prompts.push(truncate_text(&text, 100));
            }
        }
    }
    let total_prompts = all_prompts.len();

    let key_end = total_prompts.min(opts.max_prompts);
    let key_prompts = collapse_consecutive_prompts(&all_prompts[..key_end]);

    // Last N prompt emissions, excluding the first-N emission window. Collapse
    // adjacent identical presentation while retaining total_prompts as the
    // exact emission count; equal prompts are never treated as one event.
    let recent_prompts = if total_prompts > key_end {
        let recent_start = key_end.max(total_prompts.saturating_sub(opts.max_prompts));
        collapse_consecutive_prompts(&all_prompts[recent_start..])
    } else {
        Vec::new() // All prompts already in key_prompts
    };

    // 2. Files touched
    let mut files = extract_files_from_tools(entries);
    files.truncate(opts.max_files);

    // 3. Tool frequency
    let mut tool_counts: HashMap<String, usize> = HashMap::new();
    for entry in entries {
        for name in extract_tool_names(entry) {
            *tool_counts.entry(name).or_insert(0) += 1;
        }
    }
    let mut top_tools: Vec<(String, usize)> = tool_counts.into_iter().collect();
    top_tools.sort_by_key(|b| std::cmp::Reverse(b.1));
    top_tools.truncate(5);

    // 4. Error count
    let failures = count_tool_failures(entries, semantics, semantic_annotations);
    let error_count = failures.total();

    // 5. Compaction count
    let compaction_events = find_compaction_events(entries);
    let compaction_count = compaction_events.len();

    // 6. Thinking keywords
    let mut thinking_keywords = Vec::new();
    let patterns: Vec<regex::Regex> = DECISION_PATTERNS
        .iter()
        .filter_map(|p| regex::Regex::new(p).ok())
        .collect();

    for entry in entries {
        if thinking_keywords.len() >= opts.max_keywords {
            break;
        }
        if let Some(thinking) = extract_thinking_text(entry, 2000) {
            for pattern in &patterns {
                if thinking_keywords.len() >= opts.max_keywords {
                    break;
                }
                if let Some(m) = pattern.find(&thinking) {
                    let keyword = m.as_str().to_lowercase();
                    if !thinking_keywords.contains(&keyword) {
                        thinking_keywords.push(keyword);
                    }
                }
            }
        }
    }

    SessionDigest {
        key_prompts,
        recent_prompts,
        total_prompts,
        files_touched: files,
        top_tools,
        error_count,
        confirmed_tool_failures: failures.confirmed,
        inferred_failure_signals: failures.inferred,
        compaction_count,
        thinking_keywords,
        thinking_note: thinking_redaction_note(entries),
    }
}

/// Format a digest as compact text for hook injection.
///
/// Recent prompts are placed first because they are the most important
/// context after compaction (showing where work ended). If truncation
/// occurs, first prompts and metadata get cut rather than recent context.
pub fn format_digest(digest: &SessionDigest, max_chars: usize) -> String {
    let mut lines = Vec::new();

    // Recent prompts first — most important for post-compaction orientation
    if !digest.recent_prompts.is_empty() {
        lines.push(format!("Recent prompts ({} total):", digest.total_prompts));
        for prompt in &digest.recent_prompts {
            // Presentation compaction can collapse adjacent equal emissions;
            // bullets avoid inventing an ordinal after that collapse.
            lines.push(format!("  - {prompt}"));
        }
    }

    // First prompts second — provide origin context
    if !digest.key_prompts.is_empty() {
        let header = if digest.recent_prompts.is_empty() {
            format!("Prompts ({} total):", digest.total_prompts)
        } else {
            "First prompts:".to_string()
        };
        lines.push(header);
        for prompt in &digest.key_prompts {
            lines.push(format!("  - {prompt}"));
        }
    }

    if !digest.files_touched.is_empty() {
        lines.push(format!("Files: {}", digest.files_touched.join(", ")));
    }

    if !digest.top_tools.is_empty() {
        let tools: Vec<String> = digest
            .top_tools
            .iter()
            .map(|(name, count)| format!("{name}({count})"))
            .collect();
        lines.push(format!("Tools: {}", tools.join(", ")));
    }

    if digest.confirmed_tool_failures > 0 {
        lines.push(format!(
            "Confirmed tool failures: {}",
            digest.confirmed_tool_failures
        ));
    }
    if digest.inferred_failure_signals > 0 {
        lines.push(format!(
            "Inferred failure signals: {}",
            digest.inferred_failure_signals
        ));
    }

    if digest.compaction_count > 0 {
        lines.push(format!("Compactions: {}", digest.compaction_count));
    }

    if !digest.thinking_keywords.is_empty() {
        lines.push(format!(
            "Decisions: {}",
            digest.thinking_keywords.join(", ")
        ));
    } else if let Some(ref note) = digest.thinking_note {
        lines.push(format!("Thinking: {note}"));
    }

    let mut result = lines.join("\n");
    if result.len() > max_chars {
        result = truncate_text(&result, max_chars);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    fn user_entry(uuid: &str, content: &str) -> LogEntry {
        serde_json::from_value(serde_json::json!({
            "uuid": uuid,
            "parentUuid": null,
            "type": "user",
            "timestamp": "2026-01-01T00:00:00Z",
            "sessionId": "s",
            "version": "2.0",
            "isSidechain": false,
            "message": {"role": "user", "content": content}
        }))
        .unwrap()
    }

    #[test]
    fn test_default_options() {
        let opts = DigestOptions::default();
        assert_eq!(opts.max_prompts, 3);
        assert_eq!(opts.max_files, 10);
        assert_eq!(opts.max_keywords, 5);
        assert_eq!(opts.max_chars, 1000);
    }

    #[test]
    fn test_build_digest_empty() {
        let entries: Vec<&LogEntry> = vec![];
        let digest = build_digest(&entries, &DigestOptions::default());
        assert!(digest.key_prompts.is_empty());
        assert!(digest.files_touched.is_empty());
        assert!(digest.top_tools.is_empty());
        assert_eq!(digest.error_count, 0);
        assert_eq!(digest.compaction_count, 0);
    }

    #[test]
    fn test_build_digest_excludes_compact_summaries() {
        // Two genuine human prompts plus one compaction continuation summary.
        let human1: LogEntry = serde_json::from_str(
            r#"{"uuid":"1","parentUuid":null,"type":"user","timestamp":"2026-01-01T00:00:00Z","sessionId":"s","version":"2.0","isSidechain":false,"message":{"role":"user","content":"fix the parser"}}"#,
        )
        .unwrap();
        let summary: LogEntry = serde_json::from_str(
            r#"{"uuid":"2","parentUuid":null,"type":"user","timestamp":"2026-01-01T00:00:01Z","sessionId":"s","version":"2.0","isSidechain":false,"isCompactSummary":true,"message":{"role":"user","content":"This session is being continued from a previous conversation."}}"#,
        )
        .unwrap();
        let human2: LogEntry = serde_json::from_str(
            r#"{"uuid":"3","parentUuid":null,"type":"user","timestamp":"2026-01-01T00:00:02Z","sessionId":"s","version":"2.0","isSidechain":false,"message":{"role":"user","content":"add tests"}}"#,
        )
        .unwrap();
        let entries: Vec<&LogEntry> = vec![&human1, &summary, &human2];
        let digest = build_digest(&entries, &DigestOptions::default());
        // Only the two genuine prompts count; the compaction summary leaks out.
        assert_eq!(digest.total_prompts, 2);
        assert!(digest.key_prompts.iter().all(|p| !p.contains("continued")));
    }

    #[test]
    fn digest_summarizes_relay_preamble_not_the_quoted_review() {
        let entry = user_entry(
            "1",
            "I shared the work with a reviewer:\n```\nwrong wrong wrong provider boilerplate\n```\nPlease investigate.",
        );
        let digest = build_digest(&[&entry], &DigestOptions::default());
        assert_eq!(digest.total_prompts, 1);
        assert_eq!(digest.key_prompts.len(), 1);
        assert!(digest.key_prompts[0].contains("I shared the work"));
        assert!(!digest.key_prompts[0].contains("provider boilerplate"));
    }

    #[test]
    fn digest_compacts_adjacent_repetition_without_merging_emissions() {
        let entries = [
            user_entry("1", "first"),
            user_entry("2", "second"),
            user_entry("3", "third"),
            user_entry("4", "fourth"),
            user_entry("5", "same prompt"),
            user_entry("6", "same prompt"),
            user_entry("7", "same prompt"),
        ];
        let refs: Vec<&LogEntry> = entries.iter().collect();
        let digest = build_digest(
            &refs,
            &DigestOptions {
                max_prompts: 3,
                ..Default::default()
            },
        );
        assert_eq!(digest.total_prompts, 7, "emission count remains exact");
        assert_eq!(digest.key_prompts, vec!["first", "second", "third"]);
        assert_eq!(digest.recent_prompts, vec!["same prompt [repeated 3×]"]);
    }

    #[test]
    fn digest_first_and_recent_windows_do_not_overlap() {
        let entries = [
            user_entry("1", "one"),
            user_entry("2", "two"),
            user_entry("3", "three"),
            user_entry("4", "four"),
        ];
        let refs: Vec<&LogEntry> = entries.iter().collect();
        let digest = build_digest(
            &refs,
            &DigestOptions {
                max_prompts: 3,
                ..Default::default()
            },
        );
        assert_eq!(digest.key_prompts, vec!["one", "two", "three"]);
        assert_eq!(digest.recent_prompts, vec!["four"]);
    }

    #[test]
    fn test_format_digest_empty() {
        let digest = SessionDigest {
            key_prompts: vec![],
            recent_prompts: vec![],
            total_prompts: 0,
            files_touched: vec![],
            top_tools: vec![],
            error_count: 0,
            confirmed_tool_failures: 0,
            inferred_failure_signals: 0,
            compaction_count: 0,
            thinking_keywords: vec![],
            thinking_note: None,
        };
        let formatted = format_digest(&digest, 500);
        assert!(formatted.is_empty());
    }

    #[test]
    fn test_format_digest_full() {
        let digest = SessionDigest {
            key_prompts: vec!["Fix the auth bug".into(), "Add tests".into()],
            recent_prompts: vec!["Ship it".into()],
            total_prompts: 10,
            files_touched: vec!["auth.rs".into(), "tests.rs".into()],
            top_tools: vec![("Edit".into(), 5), ("Read".into(), 3)],
            error_count: 2,
            confirmed_tool_failures: 1,
            inferred_failure_signals: 1,
            compaction_count: 1,
            thinking_keywords: vec!["decided".into(), "because".into()],
            thinking_note: None,
        };
        let formatted = format_digest(&digest, 1000);
        // Recent prompts come first (most important for post-compaction)
        assert!(formatted.contains("Recent prompts (10 total):"));
        assert!(formatted.contains("Ship it"));
        // First prompts come second
        assert!(formatted.contains("First prompts:"));
        assert!(formatted.contains("Fix the auth bug"));
        // Recent appears before First in the output
        let recent_pos = formatted.find("Recent prompts").unwrap();
        let first_pos = formatted.find("First prompts").unwrap();
        assert!(recent_pos < first_pos);
        assert!(formatted.contains("Files: auth.rs, tests.rs"));
        assert!(formatted.contains("Tools: Edit(5), Read(3)"));
        assert!(formatted.contains("Confirmed tool failures: 1"));
        assert!(formatted.contains("Inferred failure signals: 1"));
        assert!(formatted.contains("Compactions: 1"));
        assert!(formatted.contains("Decisions: decided, because"));
    }

    #[test]
    fn test_format_digest_no_recent_when_few_prompts() {
        let digest = SessionDigest {
            key_prompts: vec!["Only prompt".into()],
            recent_prompts: vec![], // No recent when total <= max_prompts
            total_prompts: 1,
            files_touched: vec![],
            top_tools: vec![],
            error_count: 0,
            confirmed_tool_failures: 0,
            inferred_failure_signals: 0,
            compaction_count: 0,
            thinking_keywords: vec![],
            thinking_note: None,
        };
        let formatted = format_digest(&digest, 1000);
        // When no recent prompts, key_prompts header includes total
        assert!(formatted.contains("Prompts (1 total):"));
        assert!(!formatted.contains("Recent prompts:"));
    }

    #[test]
    fn test_format_digest_truncation() {
        let digest = SessionDigest {
            key_prompts: vec![
                "A very long prompt that goes on and on and on and on and on and on".into(),
            ],
            recent_prompts: vec![],
            total_prompts: 1,
            files_touched: vec![
                "a.rs".into(),
                "b.rs".into(),
                "c.rs".into(),
                "d.rs".into(),
                "e.rs".into(),
            ],
            top_tools: vec![("Edit".into(), 100)],
            error_count: 0,
            confirmed_tool_failures: 0,
            inferred_failure_signals: 0,
            compaction_count: 0,
            thinking_keywords: vec![],
            thinking_note: None,
        };
        let formatted = format_digest(&digest, 50);
        assert!(formatted.len() <= 53); // 50 + "..."
    }
}
