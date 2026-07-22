//! Multi-scope regex search across conversation entries.
//!
//! Searches user text, assistant text, tool inputs/results, and thinking blocks
//! depending on the requested scope.

use serde::{Deserialize, Serialize};

use crate::model::{ContentBlock, LogEntry, UserContent};

use super::extraction::extract_user_prompt_text;

/// Provider-neutral kind of one independently searchable text segment.
///
/// Search projections preserve block/emission boundaries instead of joining
/// equal text. This prevents deduplication by value from erasing legitimate
/// repeated messages and lets indexed search retain exact scope semantics.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SearchSegmentKind {
    /// Human, harness, or tool-authored text carried by a user entry. Prompt
    /// authorship/delivery are separate entry-semantic fields in the index.
    UserText,
    /// Visible assistant response text.
    AssistantText,
    /// System-event text.
    SystemText,
    /// Compaction/session summary text.
    SummaryText,
    /// Persisted reasoning/thinking summary text.
    Reasoning,
    /// Serialized tool-call input.
    ToolInput,
    /// Tool execution output.
    ToolResult,
}

impl SearchSegmentKind {
    /// Stable human-readable location label shared by exact search renderers.
    #[must_use]
    pub const fn location(self) -> &'static str {
        match self {
            Self::UserText => "user message",
            Self::AssistantText => "assistant text",
            Self::SystemText => "system",
            Self::SummaryText => "summary",
            Self::Reasoning => "thinking",
            Self::ToolInput => "tool input",
            Self::ToolResult => "tool result",
        }
    }

    /// Whether this segment belongs to the ordinary conversational-text
    /// scope.
    #[must_use]
    pub const fn is_text(self) -> bool {
        matches!(
            self,
            Self::UserText | Self::AssistantText | Self::SystemText | Self::SummaryText
        )
    }

    /// Whether this segment belongs to the tool input/output scope.
    #[must_use]
    pub const fn is_tool(self) -> bool {
        matches!(self, Self::ToolInput | Self::ToolResult)
    }
}

/// One ordered search projection from a normalized entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SearchSegment {
    /// Scope/location classification.
    pub kind: SearchSegmentKind,
    /// Exact searchable text after bounded binary-image omission.
    pub text: String,
    /// Native tool name for tool inputs, when present.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_name: Option<String>,
    /// Native call id for tool inputs/results, when present.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    /// Native explicit tool-result error state, when present.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_is_error: Option<bool>,
}

/// Machine-visible coverage limits for one entry projection.
///
/// Images and unknown blocks/entries remain preserved by the normalized
/// session and fidelity exports; they are not silently presented as indexed
/// searchable text.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SearchProjectionCoverage {
    /// Image content blocks omitted from text indexing.
    pub images_omitted: usize,
    /// Forward-compatible content block kinds without a text contract.
    pub unknown_blocks_omitted: usize,
    /// Forward-compatible top-level entries without a text contract.
    pub unknown_entries_omitted: usize,
}

/// Ordered searchable content plus explicit omission accounting for one
/// normalized entry.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct EntrySearchProjection {
    /// Searchable segments in native normalized block order.
    pub segments: Vec<SearchSegment>,
    /// Content that deliberately remained outside the text-search contract.
    pub coverage: SearchProjectionCoverage,
}

/// Provider-neutral content scope shared by direct and indexed search.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SearchScope {
    /// User, assistant, system, and summary text.
    Default,
    /// Ordinary text plus reasoning summaries.
    Thinking,
    /// Reasoning summaries only.
    ThinkingOnly,
    /// Assistant response text only.
    Assistant,
    /// User-carried text only.
    User,
    /// Tool input and result text only.
    Tools,
    /// Every searchable segment.
    All,
}

impl SearchScope {
    /// Whether a projected segment participates in this scope.
    #[must_use]
    pub const fn includes(self, kind: SearchSegmentKind) -> bool {
        match self {
            Self::Default => kind.is_text(),
            Self::Thinking => kind.is_text() || matches!(kind, SearchSegmentKind::Reasoning),
            Self::ThinkingOnly => matches!(kind, SearchSegmentKind::Reasoning),
            Self::Assistant => matches!(kind, SearchSegmentKind::AssistantText),
            Self::User => matches!(kind, SearchSegmentKind::UserText),
            Self::Tools => kind.is_tool(),
            Self::All => true,
        }
    }
}

/// One exact line-level match over a typed search projection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectedSearchMatch {
    /// Position of the independently searchable segment in native order.
    pub segment_index: usize,
    /// Zero-based line position inside the segment.
    pub line_number: usize,
    /// Stable content location label.
    pub location: String,
    /// Complete matching line.
    pub line: String,
    /// Preceding lines from this same segment.
    pub context_before: String,
    /// First regex match, or fuzzy span expanded to word boundaries.
    pub matched_text: String,
    /// Following lines from this same segment.
    pub context_after: String,
    /// Provider-independent relevance score (0-100).
    pub score: u8,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct FuzzyMatch {
    score: u8,
    start: usize,
    end: usize,
}

/// Exact matcher shared by direct and indexed search.
#[derive(Debug, Clone)]
pub enum ExactSearchMatcher {
    /// Rust-regex matching.
    Regex(regex::Regex),
    /// Fzf-style subsequence matching.
    Fuzzy {
        /// Subsequence pattern.
        pattern: String,
        /// Compare characters without case.
        ignore_case: bool,
        /// Minimum accepted relevance score.
        threshold: u8,
    },
}

impl ExactSearchMatcher {
    /// Compile a regex matcher.
    pub fn regex(pattern: &str, ignore_case: bool) -> std::result::Result<Self, regex::Error> {
        regex::RegexBuilder::new(pattern)
            .case_insensitive(ignore_case)
            .build()
            .map(Self::Regex)
    }

    /// Construct a fuzzy matcher.
    #[must_use]
    pub fn fuzzy(pattern: impl Into<String>, ignore_case: bool, threshold: u8) -> Self {
        Self::Fuzzy {
            pattern: pattern.into(),
            ignore_case,
            threshold,
        }
    }

    /// Whether this matcher accepts any part of `text`.
    #[must_use]
    pub fn is_match(&self, text: &str) -> bool {
        match self {
            Self::Regex(regex) => regex.is_match(text),
            Self::Fuzzy {
                pattern,
                ignore_case,
                threshold,
            } => fuzzy_match(pattern, text, *ignore_case, *threshold).is_some(),
        }
    }

    /// Count exact regex occurrences. Fuzzy matching has one hit per matching
    /// line, matching the normal result cardinality.
    #[must_use]
    pub fn count_in(&self, text: &str) -> usize {
        match self {
            Self::Regex(regex) => regex.find_iter(text).count(),
            Self::Fuzzy { .. } => text.lines().filter(|line| self.is_match(line)).count(),
        }
    }

    fn find_in_segment(
        &self,
        segment: &SearchSegment,
        segment_index: usize,
        context_lines: usize,
    ) -> Vec<ProjectedSearchMatch> {
        let lines: Vec<&str> = segment.text.lines().collect();
        let location = segment_location(segment);
        lines
            .iter()
            .enumerate()
            .filter_map(|(line_number, line)| {
                let (matched_text, score) = match self {
                    Self::Regex(regex) => {
                        let found = regex.find(line)?;
                        (
                            found.as_str().to_string(),
                            calculate_regex_score(line, found.as_str(), found.start(), found.end()),
                        )
                    }
                    Self::Fuzzy {
                        pattern,
                        ignore_case,
                        threshold,
                    } => {
                        let found = fuzzy_match(pattern, line, *ignore_case, *threshold)?;
                        (
                            expand_to_word_boundaries(line, found.start, found.end),
                            found.score,
                        )
                    }
                };
                let start = line_number.saturating_sub(context_lines);
                let end = line_number
                    .saturating_add(context_lines)
                    .saturating_add(1)
                    .min(lines.len());
                Some(ProjectedSearchMatch {
                    segment_index,
                    line_number,
                    location: location.clone(),
                    line: (*line).to_string(),
                    context_before: lines[start..line_number].join("\n"),
                    matched_text,
                    context_after: lines[line_number + 1..end].join("\n"),
                    score,
                })
            })
            .collect()
    }
}

fn segment_location(segment: &SearchSegment) -> String {
    if segment.kind == SearchSegmentKind::ToolInput {
        if let Some(name) = &segment.tool_name {
            return format!("tool:{name}");
        }
    }
    segment.kind.location().to_string()
}

/// Find line-level matches in native segment order.
#[must_use]
pub fn search_projection(
    projection: &EntrySearchProjection,
    matcher: &ExactSearchMatcher,
    scope: SearchScope,
    context_lines: usize,
) -> Vec<ProjectedSearchMatch> {
    projection
        .segments
        .iter()
        .enumerate()
        .filter(|(_, segment)| scope.includes(segment.kind))
        .flat_map(|(index, segment)| matcher.find_in_segment(segment, index, context_lines))
        .collect()
}

/// Count matching occurrences without merging equal segments.
#[must_use]
pub fn count_projection_matches(
    projection: &EntrySearchProjection,
    matcher: &ExactSearchMatcher,
    scope: SearchScope,
) -> usize {
    projection
        .segments
        .iter()
        .filter(|segment| scope.includes(segment.kind))
        .map(|segment| matcher.count_in(&segment.text))
        .sum()
}

/// Whether any segment in scope matches.
#[must_use]
pub fn projection_matches(
    projection: &EntrySearchProjection,
    matcher: &ExactSearchMatcher,
    scope: SearchScope,
) -> bool {
    !search_projection(projection, matcher, scope, 0).is_empty()
}

fn char_key(ch: char, ignore_case: bool) -> String {
    if ignore_case {
        ch.to_lowercase().collect()
    } else {
        ch.to_string()
    }
}

fn fuzzy_match(pattern: &str, text: &str, ignore_case: bool, threshold: u8) -> Option<FuzzyMatch> {
    let pattern_chars: Vec<char> = pattern.chars().collect();
    let text_chars: Vec<char> = text.chars().collect();
    if pattern_chars.is_empty() {
        return None;
    }
    let pattern_keys: Vec<String> = pattern_chars
        .iter()
        .map(|ch| char_key(*ch, ignore_case))
        .collect();
    let mut pattern_index = 0;
    let mut positions = Vec::new();
    for (text_index, ch) in text_chars.iter().enumerate() {
        if pattern_index < pattern_keys.len()
            && char_key(*ch, ignore_case) == pattern_keys[pattern_index]
        {
            positions.push(text_index);
            pattern_index += 1;
        }
    }
    if pattern_index != pattern_keys.len() {
        return None;
    }
    let score = calculate_fuzzy_score(&positions, &text_chars, &pattern_chars, ignore_case);
    (score >= threshold).then(|| FuzzyMatch {
        score,
        start: positions.first().copied().unwrap_or(0),
        end: positions.last().copied().unwrap_or(0).saturating_add(1),
    })
}

fn calculate_fuzzy_score(
    positions: &[usize],
    text_chars: &[char],
    pattern_chars: &[char],
    ignore_case: bool,
) -> u8 {
    if positions.is_empty() {
        return 0;
    }
    let consecutive = positions
        .windows(2)
        .filter(|window| window[1] == window[0] + 1)
        .count();
    let consecutive_ratio = consecutive as f64 / (positions.len().max(1) - 1).max(1) as f64;
    let word_starts = positions
        .iter()
        .filter(|&&position| position == 0 || !text_chars[position - 1].is_alphanumeric())
        .count();
    let mut score = 50.0 + consecutive_ratio * 25.0;
    score += word_starts as f64 / positions.len() as f64 * 15.0;
    if !ignore_case {
        let exact_case = positions
            .iter()
            .enumerate()
            .filter(|&(index, &position)| text_chars[position] == pattern_chars[index])
            .count();
        score += exact_case as f64 / positions.len() as f64 * 5.0;
    }
    if positions.len() > 1 {
        let span =
            positions.last().copied().unwrap_or(0) - positions.first().copied().unwrap_or(0) + 1;
        score += (positions.len() as f64 / span as f64 - 0.5) * 10.0;
    }
    score.clamp(0.0, 100.0) as u8
}

fn calculate_regex_score(line: &str, matched: &str, match_start: usize, match_end: usize) -> u8 {
    let mut score = 50.0;
    if match_start == 0 {
        score += 15.0;
    } else if match_start < 10 {
        score += 10.0 - match_start as f64;
    }
    score += matched.len() as f64 / line.len().max(1) as f64 * 20.0;
    let at_word_start = line[..match_start]
        .chars()
        .next_back()
        .is_none_or(|ch| !ch.is_alphanumeric());
    let at_word_end = line[match_end..]
        .chars()
        .next()
        .is_none_or(|ch| !ch.is_alphanumeric());
    if at_word_start && at_word_end {
        score += 10.0;
    } else if at_word_start || at_word_end {
        score += 5.0;
    }
    score.clamp(0.0, 100.0) as u8
}

pub(crate) fn expand_to_word_boundaries(text: &str, start: usize, end: usize) -> String {
    let chars: Vec<char> = text.chars().collect();
    if chars.is_empty() || start >= chars.len() {
        return text.to_string();
    }
    let mut expanded_start = start;
    while expanded_start > 0 && chars[expanded_start - 1].is_alphanumeric() {
        expanded_start -= 1;
    }
    let mut expanded_end = end.min(chars.len());
    while expanded_end < chars.len() && chars[expanded_end].is_alphanumeric() {
        expanded_end += 1;
    }
    chars[expanded_start..expanded_end].iter().collect()
}

fn text_segment(kind: SearchSegmentKind, text: &str) -> Option<SearchSegment> {
    (!text.is_empty()).then(|| SearchSegment {
        kind,
        text: text.to_string(),
        tool_name: None,
        tool_call_id: None,
        tool_is_error: None,
    })
}

fn push_content_block(
    projection: &mut EntrySearchProjection,
    block: &ContentBlock,
    text_kind: SearchSegmentKind,
) {
    match block {
        ContentBlock::Text(text) => {
            if let Some(segment) = text_segment(text_kind, &text.text) {
                projection.segments.push(segment);
            }
        }
        ContentBlock::Thinking(thinking) => {
            if let Some(segment) = text_segment(SearchSegmentKind::Reasoning, &thinking.thinking) {
                projection.segments.push(segment);
            }
        }
        ContentBlock::ToolUse(tool) => {
            let text = serde_json::to_string(&tool.input).unwrap_or_default();
            if !text.is_empty() {
                projection.segments.push(SearchSegment {
                    kind: SearchSegmentKind::ToolInput,
                    text,
                    tool_name: Some(tool.name.clone()),
                    tool_call_id: Some(tool.id.clone()),
                    tool_is_error: None,
                });
            }
        }
        ContentBlock::ToolResult(result) => {
            if let Some(content) = &result.content {
                let text = content.to_display_string(false);
                if !text.is_empty() {
                    projection.segments.push(SearchSegment {
                        kind: SearchSegmentKind::ToolResult,
                        text,
                        tool_name: None,
                        tool_call_id: Some(result.tool_use_id.clone()),
                        tool_is_error: result.is_error,
                    });
                }
            }
        }
        ContentBlock::Image(_) => {
            projection.coverage.images_omitted =
                projection.coverage.images_omitted.saturating_add(1);
        }
        ContentBlock::Unknown { .. } => {
            projection.coverage.unknown_blocks_omitted =
                projection.coverage.unknown_blocks_omitted.saturating_add(1);
        }
    }
}

/// Project one normalized entry into ordered provider-neutral search text.
///
/// This function deliberately performs no provider inference. Prompt
/// authorship/delivery, activity, canonical tool kind, lineage, and source
/// identity come from the surrounding [`crate::provider::ParsedSession`] and
/// are attached by the index builder. Native raw records remain at the source.
#[must_use]
pub fn project_entry_for_search(entry: &LogEntry) -> EntrySearchProjection {
    let mut projection = EntrySearchProjection::default();
    match entry {
        LogEntry::User(user) => match &user.message {
            UserContent::Simple(content) => {
                if let Some(segment) = text_segment(SearchSegmentKind::UserText, &content.content) {
                    projection.segments.push(segment);
                }
            }
            UserContent::Blocks(content) => {
                for block in &content.content {
                    push_content_block(&mut projection, block, SearchSegmentKind::UserText);
                }
            }
        },
        LogEntry::Assistant(assistant) => {
            for block in &assistant.message.content {
                push_content_block(&mut projection, block, SearchSegmentKind::AssistantText);
            }
        }
        LogEntry::System(system) => {
            if let Some(content) = system
                .content
                .as_deref()
                .and_then(|text| text_segment(SearchSegmentKind::SystemText, text))
            {
                projection.segments.push(content);
            }
        }
        LogEntry::Summary(summary) => {
            if let Some(segment) = text_segment(SearchSegmentKind::SummaryText, &summary.summary) {
                projection.segments.push(segment);
            }
        }
        LogEntry::Unknown(_) => {
            projection.coverage.unknown_entries_omitted = 1;
        }
        _ => {}
    }
    projection
}

/// Search a single entry for a regex pattern match.
///
/// Returns a list of `(matched_text, context_snippet)` pairs. The `scope` parameter
/// controls which parts of the entry are searched:
///
/// - `"text"`: User prompt text and assistant response text
/// - `"tools"`: Tool result content (user entries) and tool use inputs (assistant entries)
/// - `"thinking"`: Assistant thinking/reasoning blocks
/// - `"all"`: All of the above
///
/// `max_context` controls how many characters of surrounding context to include.
pub fn search_entry_text(
    entry: &LogEntry,
    regex: &regex::Regex,
    scope: &str,
    max_context: usize,
) -> Vec<(String, String)> {
    let mut matches = Vec::new();

    let mut search_text = |text: &str| {
        for mat in regex.find_iter(text) {
            let start = mat.start().saturating_sub(max_context);
            let end = (mat.end() + max_context).min(text.len());
            // Snap to char boundaries
            let start = snap_char_boundary_left(text, start);
            let end = snap_char_boundary_right(text, end);
            let context = &text[start..end];
            matches.push((mat.as_str().to_string(), context.to_string()));
        }
    };

    match entry {
        LogEntry::User(user) => {
            if scope == "text" || scope == "all" {
                if let Some(text) = extract_user_prompt_text(&LogEntry::User(user.clone())) {
                    search_text(&text);
                }
            }
            if scope == "tools" || scope == "all" {
                for result in user.message.tool_results() {
                    if let Some(ref content) = result.content {
                        let text = format!("{content:?}");
                        search_text(&text);
                    }
                }
            }
        }
        LogEntry::Assistant(assistant) => {
            if scope == "text" || scope == "all" {
                let text = assistant.message.combined_text();
                if !text.is_empty() {
                    search_text(&text);
                }
            }
            if scope == "tools" || scope == "all" {
                for tool in assistant.message.tool_uses() {
                    let input_str = tool.input.to_string();
                    search_text(&input_str);
                }
            }
            if scope == "thinking" || scope == "all" {
                for block in assistant.message.thinking_blocks() {
                    if !block.thinking.is_empty() {
                        search_text(&block.thinking);
                    }
                }
            }
        }
        LogEntry::System(sys) => {
            if scope == "text" || scope == "all" {
                if let Some(ref content) = sys.content {
                    search_text(content);
                }
            }
        }
        _ => {}
    }

    matches
}

/// Snap a byte index left to the nearest char boundary.
fn snap_char_boundary_left(s: &str, idx: usize) -> usize {
    let mut i = idx;
    while i > 0 && !s.is_char_boundary(i) {
        i -= 1;
    }
    i
}

/// Snap a byte index right to the nearest char boundary.
fn snap_char_boundary_right(s: &str, idx: usize) -> usize {
    let mut i = idx;
    while i < s.len() && !s.is_char_boundary(i) {
        i += 1;
    }
    i
}

#[cfg(test)]
mod tests {
    #![allow(clippy::trivial_regex)]
    use super::*;

    /// Build a System LogEntry from JSON for testing.
    fn system_entry(content: &str) -> LogEntry {
        serde_json::from_value(serde_json::json!({
            "type": "system",
            "uuid": "test-uuid",
            "timestamp": "2025-01-01T00:00:00Z",
            "content": content,
        }))
        .unwrap()
    }

    #[test]
    fn test_search_no_match() {
        let entry = system_entry("hello world");
        let re = regex::Regex::new("foobar").unwrap();
        let results = search_entry_text(&entry, &re, "text", 20);
        assert!(results.is_empty());
    }

    #[test]
    fn test_search_system_text() {
        let entry = system_entry("the quick brown fox jumps");
        let re = regex::Regex::new("brown").unwrap();
        let results = search_entry_text(&entry, &re, "text", 5);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].0, "brown");
        assert!(results[0].1.contains("brown"));
    }

    #[test]
    fn test_search_scope_filtering() {
        let entry = system_entry("hello world");
        let re = regex::Regex::new("hello").unwrap();

        // "text" scope should find it
        assert_eq!(search_entry_text(&entry, &re, "text", 10).len(), 1);
        // "tools" scope should not
        assert_eq!(search_entry_text(&entry, &re, "tools", 10).len(), 0);
        // "thinking" scope should not
        assert_eq!(search_entry_text(&entry, &re, "thinking", 10).len(), 0);
        // "all" scope should find it
        assert_eq!(search_entry_text(&entry, &re, "all", 10).len(), 1);
    }

    #[test]
    fn test_snap_char_boundary() {
        let s = "hello 😀 world";
        // The emoji is 4 bytes. Make sure we snap correctly.
        let emoji_start = s.find('😀').unwrap();
        assert_eq!(snap_char_boundary_left(s, emoji_start + 1), emoji_start);
        assert_eq!(
            snap_char_boundary_right(s, emoji_start + 1),
            emoji_start + 4
        );
    }

    #[test]
    fn search_projection_preserves_order_identity_and_tool_metadata() {
        let entry: LogEntry = serde_json::from_value(serde_json::json!({
            "type": "assistant",
            "uuid": "assistant-1",
            "timestamp": "2026-07-22T00:00:00Z",
            "sessionId": "session-1",
            "version": "2.1.193",
            "message": {
                "id": "message-1",
                "type": "message",
                "role": "assistant",
                "model": "test-model",
                "content": [
                    {"type": "text", "text": "repeat"},
                    {"type": "text", "text": "repeat"},
                    {"type": "thinking", "thinking": "reason", "signature": "sig"},
                    {"type": "tool_use", "id": "call-1", "name": "Read", "input": {"file_path": "src/lib.rs"}},
                    {"type": "tool_result", "tool_use_id": "call-1", "content": "result", "is_error": false}
                ]
            }
        }))
        .unwrap();

        let projection = project_entry_for_search(&entry);
        assert_eq!(projection.segments.len(), 5);
        assert_eq!(
            projection.segments[0].kind,
            SearchSegmentKind::AssistantText
        );
        assert_eq!(
            projection.segments[1].kind,
            SearchSegmentKind::AssistantText
        );
        assert_eq!(projection.segments[0].text, "repeat");
        assert_eq!(projection.segments[1].text, "repeat");
        assert_eq!(projection.segments[2].kind, SearchSegmentKind::Reasoning);
        assert_eq!(projection.segments[3].kind, SearchSegmentKind::ToolInput);
        assert_eq!(projection.segments[3].tool_name.as_deref(), Some("Read"));
        assert_eq!(
            projection.segments[3].tool_call_id.as_deref(),
            Some("call-1")
        );
        assert_eq!(projection.segments[4].kind, SearchSegmentKind::ToolResult);
        assert_eq!(projection.segments[4].tool_is_error, Some(false));
        assert_eq!(projection.coverage, SearchProjectionCoverage::default());
    }

    #[test]
    fn search_projection_includes_user_tool_results_without_indexing_image_payloads() {
        let secret_base64 = "A".repeat(512);
        let entry: LogEntry = serde_json::from_value(serde_json::json!({
            "type": "user",
            "uuid": "user-1",
            "timestamp": "2026-07-22T00:00:00Z",
            "sessionId": "session-1",
            "version": "2.1.193",
            "message": {
                "role": "user",
                "content": [
                    {"type": "text", "text": "prompt"},
                    {
                        "type": "tool_result",
                        "tool_use_id": "call-1",
                        "is_error": true,
                        "content": [{
                            "type": "image",
                            "source": {"type": "base64", "media_type": "image/png", "data": secret_base64}
                        }]
                    },
                    {
                        "type": "image",
                        "source": {"type": "url", "url": "https://example.invalid/image.png"}
                    },
                    {"type": "future_block", "payload": "preserved outside search"}
                ]
            }
        }))
        .unwrap();

        let projection = project_entry_for_search(&entry);
        assert_eq!(projection.segments.len(), 2);
        assert_eq!(projection.segments[0].kind, SearchSegmentKind::UserText);
        assert_eq!(projection.segments[1].kind, SearchSegmentKind::ToolResult);
        assert_eq!(
            projection.segments[1].tool_call_id.as_deref(),
            Some("call-1")
        );
        assert_eq!(projection.segments[1].tool_is_error, Some(true));
        assert!(projection.segments[1].text.contains("base64 image omitted"));
        assert!(!projection.segments[1].text.contains(&"A".repeat(128)));
        assert_eq!(projection.coverage.images_omitted, 1);
        assert_eq!(projection.coverage.unknown_blocks_omitted, 1);
        assert_eq!(projection.coverage.unknown_entries_omitted, 0);
    }

    #[test]
    fn search_projection_reports_unknown_entry_coverage() {
        let entry: LogEntry = serde_json::from_value(serde_json::json!({
            "type": "future-entry",
            "uuid": "unknown-1",
            "content": "not silently claimed as searchable"
        }))
        .unwrap();
        let projection = project_entry_for_search(&entry);
        assert!(projection.segments.is_empty());
        assert_eq!(projection.coverage.unknown_entries_omitted, 1);
    }

    #[test]
    fn exact_matcher_preserves_duplicate_segments_but_one_result_per_line() {
        let projection = EntrySearchProjection {
            segments: vec![
                SearchSegment {
                    kind: SearchSegmentKind::AssistantText,
                    text: "repeat repeat".to_string(),
                    tool_name: None,
                    tool_call_id: None,
                    tool_is_error: None,
                },
                SearchSegment {
                    kind: SearchSegmentKind::AssistantText,
                    text: "repeat repeat".to_string(),
                    tool_name: None,
                    tool_call_id: None,
                    tool_is_error: None,
                },
            ],
            coverage: SearchProjectionCoverage::default(),
        };
        let matcher = ExactSearchMatcher::regex("repeat", false).unwrap();
        let matches = search_projection(&projection, &matcher, SearchScope::Default, 0);
        assert_eq!(matches.len(), 2, "equal emissions must remain distinct");
        assert_eq!(matches[0].segment_index, 0);
        assert_eq!(matches[1].segment_index, 1);
        assert_eq!(
            count_projection_matches(&projection, &matcher, SearchScope::Default),
            4,
            "count mode counts occurrences rather than matching lines"
        );
    }

    #[test]
    fn shared_scope_contract_is_additive_and_exclusive_where_documented() {
        let projection = EntrySearchProjection {
            segments: vec![
                SearchSegment {
                    kind: SearchSegmentKind::UserText,
                    text: "needle".to_string(),
                    tool_name: None,
                    tool_call_id: None,
                    tool_is_error: None,
                },
                SearchSegment {
                    kind: SearchSegmentKind::Reasoning,
                    text: "needle".to_string(),
                    tool_name: None,
                    tool_call_id: None,
                    tool_is_error: None,
                },
                SearchSegment {
                    kind: SearchSegmentKind::ToolInput,
                    text: "needle".to_string(),
                    tool_name: Some("Read".to_string()),
                    tool_call_id: Some("call-1".to_string()),
                    tool_is_error: None,
                },
            ],
            coverage: SearchProjectionCoverage::default(),
        };
        let matcher = ExactSearchMatcher::regex("needle", false).unwrap();
        assert_eq!(
            search_projection(&projection, &matcher, SearchScope::Default, 0).len(),
            1
        );
        assert_eq!(
            search_projection(&projection, &matcher, SearchScope::Thinking, 0).len(),
            2
        );
        assert_eq!(
            search_projection(&projection, &matcher, SearchScope::ThinkingOnly, 0).len(),
            1
        );
        let tools = search_projection(&projection, &matcher, SearchScope::Tools, 0);
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].location, "tool:Read");
        assert_eq!(
            search_projection(&projection, &matcher, SearchScope::All, 0).len(),
            3
        );
    }

    #[test]
    fn exact_matcher_context_and_scoring_are_unicode_safe() {
        let projection = EntrySearchProjection {
            segments: vec![SearchSegment {
                kind: SearchSegmentKind::AssistantText,
                text: "😀 first\nélan NEEDLE 世界\n😀 last".to_string(),
                tool_name: None,
                tool_call_id: None,
                tool_is_error: None,
            }],
            coverage: SearchProjectionCoverage::default(),
        };
        let regex = ExactSearchMatcher::regex("needle", true).unwrap();
        let matches = search_projection(&projection, &regex, SearchScope::Default, 1);
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].matched_text, "NEEDLE");
        assert_eq!(matches[0].context_before, "😀 first");
        assert_eq!(matches[0].context_after, "😀 last");

        let fuzzy = ExactSearchMatcher::fuzzy("éN界", true, 0);
        let matches = search_projection(&projection, &fuzzy, SearchScope::Default, 0);
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].line_number, 1);
    }

    #[test]
    fn fuzzy_matcher_preserves_scoring_and_case_contracts() {
        let exact = fuzzy_match("hello", "hello world", false, 60).unwrap();
        assert!(exact.score >= 80);
        assert_eq!(
            expand_to_word_boundaries("hello world", exact.start, exact.end),
            "hello"
        );

        let subsequence = fuzzy_match("hlo", "hello", false, 50).unwrap();
        assert_eq!(
            expand_to_word_boundaries("hello", subsequence.start, subsequence.end),
            "hello"
        );
        assert!(fuzzy_match("HELLO", "hello world", true, 60).is_some());
        assert!(fuzzy_match("HELLO", "hello world", false, 60).is_none());
        assert!(fuzzy_match("abc", "a___b___c", false, 90).is_none());
        assert!(fuzzy_match("xyz", "hello world", false, 50).is_none());
        assert!(fuzzy_match("abc", "ab", false, 50).is_none());
        assert!(
            fuzzy_match("hw", "hello world", false, 50).is_some_and(|matched| matched.score >= 60)
        );
        assert!(
            fuzzy_match("ab", "ab", false, 0).unwrap().score
                > fuzzy_match("ab", "a_b", false, 0).unwrap().score
        );
    }

    #[test]
    fn regex_relevance_preserves_boundary_and_coverage_contracts() {
        let start = calculate_regex_score("hello world", "hello", 0, 5);
        let middle = calculate_regex_score("the hello world", "ello", 5, 9);
        let boundary = calculate_regex_score("the hello world", "hello", 4, 9);
        assert!(start >= 75);
        assert!(middle < start);
        assert!(boundary > middle);
        assert!(
            calculate_regex_score("hello", "hello", 0, 5)
                > calculate_regex_score("hello world", "hello", 0, 5)
        );
    }
}
