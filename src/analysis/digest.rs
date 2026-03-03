//! Session digest: compact summary of a session's key topics.
//!
//! Builds a high-level overview from conversation entries using existing
//! extraction primitives. Designed for hook injection after compaction
//! so Claude knows "what was discussed" without needing the full conversation.

use std::collections::HashMap;

use crate::model::message::LogEntry;

use super::extraction::{
    extract_files_from_tools, extract_thinking_text, extract_tool_names, extract_user_prompt_text,
    find_compaction_events, has_tool_errors, is_human_prompt, truncate_text,
};

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
            max_chars: 500,
        }
    }
}

/// A compact session digest.
#[derive(Debug, Clone, serde::Serialize)]
pub struct SessionDigest {
    /// Key human prompts (first N, truncated).
    pub key_prompts: Vec<String>,
    /// Files touched by Write/Edit/Read tools.
    pub files_touched: Vec<String>,
    /// Tool usage counts (sorted by frequency).
    pub top_tools: Vec<(String, usize)>,
    /// Number of tool errors in the session.
    pub error_count: usize,
    /// Number of compaction events.
    pub compaction_count: usize,
    /// Decision-related keywords from thinking blocks.
    pub thinking_keywords: Vec<String>,
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
    // 1. Key prompts: first N human prompts, truncated
    let mut key_prompts = Vec::new();
    for entry in entries {
        if key_prompts.len() >= opts.max_prompts {
            break;
        }
        if is_human_prompt(entry) {
            if let Some(text) = extract_user_prompt_text(entry) {
                key_prompts.push(truncate_text(&text, 100));
            }
        }
    }

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
    top_tools.sort_by(|a, b| b.1.cmp(&a.1));
    top_tools.truncate(5);

    // 4. Error count
    let mut error_count = 0;
    // Check consecutive pairs (each user entry may contain tool results)
    for window in entries.windows(2) {
        if has_tool_errors(&[window[0]]) {
            error_count += 1;
        }
    }
    // Check last entry too
    if let Some(last) = entries.last() {
        if has_tool_errors(&[*last]) {
            error_count += 1;
        }
    }

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
        files_touched: files,
        top_tools,
        error_count,
        compaction_count,
        thinking_keywords,
    }
}

/// Format a digest as compact text for hook injection.
pub fn format_digest(digest: &SessionDigest, max_chars: usize) -> String {
    let mut lines = Vec::new();

    if !digest.key_prompts.is_empty() {
        lines.push("Prompts:".to_string());
        for (i, p) in digest.key_prompts.iter().enumerate() {
            lines.push(format!("  {}. {p}", i + 1));
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

    if digest.error_count > 0 {
        lines.push(format!("Errors: {}", digest.error_count));
    }

    if digest.compaction_count > 0 {
        lines.push(format!("Compactions: {}", digest.compaction_count));
    }

    if !digest.thinking_keywords.is_empty() {
        lines.push(format!(
            "Decisions: {}",
            digest.thinking_keywords.join(", ")
        ));
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

    #[test]
    fn test_default_options() {
        let opts = DigestOptions::default();
        assert_eq!(opts.max_prompts, 3);
        assert_eq!(opts.max_files, 10);
        assert_eq!(opts.max_keywords, 5);
        assert_eq!(opts.max_chars, 500);
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
    fn test_format_digest_empty() {
        let digest = SessionDigest {
            key_prompts: vec![],
            files_touched: vec![],
            top_tools: vec![],
            error_count: 0,
            compaction_count: 0,
            thinking_keywords: vec![],
        };
        let formatted = format_digest(&digest, 500);
        assert!(formatted.is_empty());
    }

    #[test]
    fn test_format_digest_full() {
        let digest = SessionDigest {
            key_prompts: vec!["Fix the auth bug".into(), "Add tests".into()],
            files_touched: vec!["auth.rs".into(), "tests.rs".into()],
            top_tools: vec![("Edit".into(), 5), ("Read".into(), 3)],
            error_count: 2,
            compaction_count: 1,
            thinking_keywords: vec!["decided".into(), "because".into()],
        };
        let formatted = format_digest(&digest, 500);
        assert!(formatted.contains("Prompts:"));
        assert!(formatted.contains("Fix the auth bug"));
        assert!(formatted.contains("Files: auth.rs, tests.rs"));
        assert!(formatted.contains("Tools: Edit(5), Read(3)"));
        assert!(formatted.contains("Errors: 2"));
        assert!(formatted.contains("Compactions: 1"));
        assert!(formatted.contains("Decisions: decided, because"));
    }

    #[test]
    fn test_format_digest_truncation() {
        let digest = SessionDigest {
            key_prompts: vec!["A very long prompt that goes on and on and on and on and on and on".into()],
            files_touched: vec!["a.rs".into(), "b.rs".into(), "c.rs".into(), "d.rs".into(), "e.rs".into()],
            top_tools: vec![("Edit".into(), 100)],
            error_count: 0,
            compaction_count: 0,
            thinking_keywords: vec![],
        };
        let formatted = format_digest(&digest, 50);
        assert!(formatted.len() <= 53); // 50 + "..."
    }
}
