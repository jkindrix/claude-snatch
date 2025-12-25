//! Search command implementation.
//!
//! Searches across sessions for text patterns with optional filters.

use std::collections::HashSet;

use regex::{Regex, RegexBuilder};

use crate::cli::{Cli, OutputFormat, SearchArgs};
use crate::error::{Result, SnatchError};
use crate::model::{ContentBlock, LogEntry};

/// Fuzzy matching result with score.
#[derive(Debug)]
struct FuzzyMatch {
    /// Match score (0-100).
    score: u8,
    /// Start index of match in the text (for future highlighting).
    #[allow(dead_code)]
    start: usize,
    /// End index of match in the text (for future highlighting).
    #[allow(dead_code)]
    end: usize,
    /// The matched substring.
    matched_text: String,
}

/// Perform fuzzy matching (fzf-style subsequence matching with scoring).
///
/// Returns a match result if the pattern characters appear in order in the text
/// and the calculated score meets the threshold.
fn fuzzy_match(pattern: &str, text: &str, ignore_case: bool, threshold: u8) -> Option<FuzzyMatch> {
    let pattern_chars: Vec<char> = if ignore_case {
        pattern.to_lowercase().chars().collect()
    } else {
        pattern.chars().collect()
    };

    let text_chars: Vec<char> = text.chars().collect();
    let text_lower: Vec<char> = if ignore_case {
        text.to_lowercase().chars().collect()
    } else {
        text_chars.clone()
    };

    if pattern_chars.is_empty() {
        return None;
    }

    // Find the best matching position using a simple greedy approach
    let mut pattern_idx = 0;
    let mut match_positions: Vec<usize> = Vec::new();

    for (text_idx, &ch) in text_lower.iter().enumerate() {
        if pattern_idx < pattern_chars.len() && ch == pattern_chars[pattern_idx] {
            match_positions.push(text_idx);
            pattern_idx += 1;
        }
    }

    // All pattern characters must be found
    if pattern_idx != pattern_chars.len() {
        return None;
    }

    // Calculate score based on match quality
    let score = calculate_fuzzy_score(&match_positions, &text_chars, &pattern_chars, ignore_case);

    if score < threshold {
        return None;
    }

    // Build the matched text range
    let start = match_positions.first().copied().unwrap_or(0);
    let end = match_positions.last().copied().unwrap_or(0) + 1;
    let matched_text: String = text_chars[start..end].iter().collect();

    Some(FuzzyMatch {
        score,
        start,
        end,
        matched_text,
    })
}

/// Calculate fuzzy match score (0-100).
///
/// Scoring factors:
/// - Consecutive character matches (bonus)
/// - Start of word matches (bonus)
/// - Exact case matches (bonus)
/// - Shorter match span (bonus)
fn calculate_fuzzy_score(
    positions: &[usize],
    text_chars: &[char],
    pattern_chars: &[char],
    ignore_case: bool,
) -> u8 {
    if positions.is_empty() {
        return 0;
    }

    let mut score: f64 = 50.0; // Base score for finding all characters

    // Bonus for consecutive matches
    let mut consecutive_count = 0;
    for window in positions.windows(2) {
        if window[1] == window[0] + 1 {
            consecutive_count += 1;
        }
    }
    let consecutive_ratio = consecutive_count as f64 / (positions.len().max(1) - 1).max(1) as f64;
    score += consecutive_ratio * 25.0;

    // Bonus for start of word matches
    let mut word_start_count = 0;
    for &pos in positions {
        if pos == 0 || !text_chars[pos - 1].is_alphanumeric() {
            word_start_count += 1;
        }
    }
    let word_start_ratio = word_start_count as f64 / positions.len() as f64;
    score += word_start_ratio * 15.0;

    // Bonus for exact case matches (when not ignoring case)
    if !ignore_case {
        let mut case_match_count = 0;
        for (i, &pos) in positions.iter().enumerate() {
            if i < pattern_chars.len() && text_chars[pos] == pattern_chars[i] {
                case_match_count += 1;
            }
        }
        let case_ratio = case_match_count as f64 / positions.len() as f64;
        score += case_ratio * 5.0;
    }

    // Penalty for spread-out matches (prefer compact matches)
    if positions.len() > 1 {
        let span = positions.last().unwrap() - positions.first().unwrap() + 1;
        let ideal_span = positions.len();
        let compactness = ideal_span as f64 / span as f64;
        score += (compactness - 0.5) * 10.0; // -5 to +5 adjustment
    }

    score.clamp(0.0, 100.0) as u8
}

use super::get_claude_dir;

/// Check if an entry matches the search filters.
fn matches_filters(entry: &LogEntry, args: &SearchArgs) -> bool {
    // Check message type filter
    if let Some(ref type_filter) = args.message_type {
        if !matches_message_type(entry, type_filter) {
            return false;
        }
    }

    // Check model filter
    if let Some(ref model_filter) = args.model {
        if !matches_model(entry, model_filter) {
            return false;
        }
    }

    // Check tool name filter
    if let Some(ref tool_filter) = args.tool_name {
        if !contains_tool(entry, tool_filter) {
            return false;
        }
    }

    // Check error filter
    if args.errors && !is_error_message(entry) {
        return false;
    }

    true
}

/// Check if an entry matches the model filter.
fn matches_model(entry: &LogEntry, model_filter: &str) -> bool {
    let model_filter_lower = model_filter.to_lowercase();
    match entry {
        LogEntry::Assistant(msg) => {
            // Check if assistant message has model info in the message
            msg.message.model.to_lowercase().contains(&model_filter_lower)
        }
        _ => true, // Non-assistant messages don't have model info, so don't filter them out
    }
}

/// Check if an entry contains a specific tool use.
fn contains_tool(entry: &LogEntry, tool_filter: &str) -> bool {
    let tool_filter_lower = tool_filter.to_lowercase();
    match entry {
        LogEntry::Assistant(msg) => {
            for block in &msg.message.content {
                if let ContentBlock::ToolUse(tool) = block {
                    if tool.name.to_lowercase().contains(&tool_filter_lower) {
                        return true;
                    }
                }
            }
            false
        }
        LogEntry::User(_) => {
            // Tool results don't have the tool name directly, so we can't filter by tool name here
            // This would require tracking the parent tool_use to get the name
            false
        }
        _ => false,
    }
}

/// Check if an entry is an error message.
fn is_error_message(entry: &LogEntry) -> bool {
    use crate::model::message::SystemSubtype;

    match entry {
        LogEntry::Assistant(msg) => msg.is_api_error_message.unwrap_or(false),
        LogEntry::System(msg) => {
            // Check for api_error subtype
            matches!(msg.subtype, Some(SystemSubtype::ApiError))
        }
        _ => false,
    }
}

/// Run the search command.
pub fn run(cli: &Cli, args: &SearchArgs) -> Result<()> {
    let claude_dir = get_claude_dir(cli.claude_dir.as_ref())?;

    // Build regex
    let regex = RegexBuilder::new(&args.pattern)
        .case_insensitive(args.ignore_case)
        .build()
        .map_err(|e| SnatchError::InvalidArgument {
            name: "pattern".to_string(),
            reason: e.to_string(),
        })?;

    // Collect sessions to search
    let sessions = if let Some(session_id) = &args.session {
        let session = claude_dir
            .find_session(session_id)?
            .ok_or_else(|| SnatchError::SessionNotFound {
                session_id: session_id.clone(),
            })?;
        vec![session]
    } else if let Some(project_filter) = &args.project {
        let projects = claude_dir.projects()?;
        let mut sessions = Vec::new();
        for project in projects {
            if project.decoded_path().contains(project_filter) {
                sessions.extend(project.sessions()?);
            }
        }
        sessions
    } else {
        claude_dir.all_sessions()?
    };

    let mut total_matches = 0;
    let mut all_results = Vec::new();
    let mut sessions_with_matches: HashSet<String> = HashSet::new();
    let mut match_counts: std::collections::HashMap<String, usize> = std::collections::HashMap::new();

    // Search each session
    for session in sessions {
        let entries = match session.parse() {
            Ok(e) => e,
            Err(_) => continue, // Skip unparseable sessions
        };

        let mut session_match_count = 0;

        for entry in &entries {
            // Apply all filters
            if !matches_filters(entry, args) {
                continue;
            }

            let matches = search_entry(entry, &regex, args);

            if !matches.is_empty() {
                sessions_with_matches.insert(session.session_id().to_string());

                for m in matches {
                    total_matches += 1;
                    session_match_count += 1;

                    // For files_only or count mode, we don't need to store full results
                    if !args.files_only && !args.count {
                        let result = SearchResult {
                            session_id: session.session_id().to_string(),
                            project: session.project_path().to_string(),
                            uuid: entry.uuid().unwrap_or("").to_string(),
                            entry_type: entry.message_type().to_string(),
                            location: m.location,
                            line: m.line,
                            context_before: m.context_before,
                            matched_text: m.matched_text,
                            context_after: m.context_after,
                        };

                        all_results.push(result);
                    }

                    if let Some(limit) = args.limit {
                        if total_matches >= limit {
                            break;
                        }
                    }
                }
            }
        }

        if session_match_count > 0 {
            match_counts.insert(session.session_id().to_string(), session_match_count);
        }

        if let Some(limit) = args.limit {
            if total_matches >= limit {
                break;
            }
        }
    }

    // Output results based on mode
    if args.files_only {
        output_files_only(cli, &sessions_with_matches)?;
    } else if args.count {
        output_count(cli, &match_counts, total_matches)?;
    } else {
        output_full_results(cli, args, &all_results, total_matches)?;
    }

    Ok(())
}

/// Check if an entry matches the message type filter.
fn matches_message_type(entry: &LogEntry, type_filter: &str) -> bool {
    let entry_type = entry.message_type().to_lowercase();
    match type_filter {
        "user" => entry_type == "user",
        "assistant" => entry_type == "assistant",
        "system" => entry_type == "system",
        "summary" => entry_type == "summary",
        _ => entry_type.contains(type_filter),
    }
}

/// Output only session IDs with matches.
fn output_files_only(cli: &Cli, sessions: &HashSet<String>) -> Result<()> {
    match cli.effective_output() {
        OutputFormat::Json => {
            let sessions_vec: Vec<&String> = sessions.iter().collect();
            println!("{}", serde_json::to_string_pretty(&sessions_vec)?);
        }
        _ => {
            for session_id in sessions {
                println!("{}", session_id);
            }
        }
    }
    Ok(())
}

/// Output match counts.
fn output_count(
    cli: &Cli,
    match_counts: &std::collections::HashMap<String, usize>,
    total: usize,
) -> Result<()> {
    match cli.effective_output() {
        OutputFormat::Json => {
            let output = serde_json::json!({
                "total": total,
                "by_session": match_counts,
            });
            println!("{}", serde_json::to_string_pretty(&output)?);
        }
        _ => {
            if match_counts.len() == 1 {
                // Single session - just show count
                println!("{}", total);
            } else {
                // Multiple sessions - show per-session counts
                let mut counts: Vec<(&String, &usize)> = match_counts.iter().collect();
                counts.sort_by(|a, b| b.1.cmp(a.1));

                for (session_id, count) in counts {
                    println!("{}:{}", &session_id[..8.min(session_id.len())], count);
                }
                println!();
                println!("Total: {}", total);
            }
        }
    }
    Ok(())
}

/// Output full search results.
fn output_full_results(
    cli: &Cli,
    args: &SearchArgs,
    all_results: &[SearchResult],
    total_matches: usize,
) -> Result<()> {
    match cli.effective_output() {
        OutputFormat::Json => {
            println!("{}", serde_json::to_string_pretty(&all_results)?);
        }
        OutputFormat::Tsv => {
            println!("session\tproject\tuuid\ttype\tlocation\tline");
            for result in all_results {
                println!(
                    "{}\t{}\t{}\t{}\t{}\t{}",
                    &result.session_id[..8.min(result.session_id.len())],
                    result.project,
                    &result.uuid[..8.min(result.uuid.len())],
                    result.entry_type,
                    result.location,
                    result.line.replace('\t', " ")
                );
            }
        }
        OutputFormat::Compact => {
            for result in all_results {
                println!("{}:{}: {}",
                    &result.session_id[..8.min(result.session_id.len())],
                    result.location,
                    result.matched_text
                );
            }
        }
        OutputFormat::Text => {
            if all_results.is_empty() {
                println!("No matches found.");
                return Ok(());
            }

            println!("Found {} matches:", total_matches);
            println!();

            let mut current_session = String::new();

            for result in all_results {
                if result.session_id != current_session {
                    current_session = result.session_id.clone();
                    println!("Session: {} ({})",
                        &result.session_id[..8.min(result.session_id.len())],
                        result.project
                    );
                }

                println!();
                println!("  [{} in {}]", result.entry_type, result.location);

                if args.context > 0 && !result.context_before.is_empty() {
                    for line in result.context_before.lines() {
                        println!("  | {}", line);
                    }
                }

                println!("  > {}", result.matched_text);

                if args.context > 0 && !result.context_after.is_empty() {
                    for line in result.context_after.lines() {
                        println!("  | {}", line);
                    }
                }
            }
        }
    }

    Ok(())
}

/// Search result.
#[derive(Debug, serde::Serialize)]
struct SearchResult {
    session_id: String,
    project: String,
    uuid: String,
    entry_type: String,
    location: String,
    line: String,
    context_before: String,
    matched_text: String,
    context_after: String,
}

/// A match within an entry.
struct Match {
    location: String,
    line: String,
    context_before: String,
    matched_text: String,
    context_after: String,
}

/// Matcher enum to support both regex and fuzzy matching.
enum Matcher<'a> {
    Regex(&'a Regex),
    Fuzzy {
        pattern: &'a str,
        ignore_case: bool,
        threshold: u8,
    },
}

impl Matcher<'_> {
    fn is_match(&self, text: &str) -> bool {
        match self {
            Matcher::Regex(regex) => regex.is_match(text),
            Matcher::Fuzzy { pattern, ignore_case, threshold } => {
                fuzzy_match(pattern, text, *ignore_case, *threshold).is_some()
            }
        }
    }

    fn find_matches_in(&self, text: &str, location: &str, context: usize) -> Vec<Match> {
        match self {
            Matcher::Regex(regex) => find_matches(text, regex, location, context),
            Matcher::Fuzzy { pattern, ignore_case, threshold } => {
                find_fuzzy_matches(text, pattern, location, context, *ignore_case, *threshold)
            }
        }
    }
}

/// Search an entry for matches.
fn search_entry(entry: &LogEntry, regex: &Regex, args: &SearchArgs) -> Vec<Match> {
    // Create the appropriate matcher
    let matcher = if args.fuzzy {
        Matcher::Fuzzy {
            pattern: &args.pattern,
            ignore_case: args.ignore_case,
            threshold: args.fuzzy_threshold,
        }
    } else {
        Matcher::Regex(regex)
    };

    let mut matches = Vec::new();

    match entry {
        LogEntry::User(user) => {
            // Search user content
            let text = match &user.message {
                crate::model::UserContent::Simple(s) => s.content.clone(),
                crate::model::UserContent::Blocks(b) => {
                    b.content.iter().filter_map(|c| {
                        match c {
                            ContentBlock::Text(t) => Some(t.text.as_str()),
                            _ => None,
                        }
                    }).collect::<Vec<_>>().join("\n")
                }
            };

            if matcher.is_match(&text) {
                matches.extend(matcher.find_matches_in(&text, "user message", args.context));
            }
        }
        LogEntry::Assistant(assistant) => {
            for block in &assistant.message.content {
                match block {
                    ContentBlock::Text(text) => {
                        if matcher.is_match(&text.text) {
                            matches.extend(matcher.find_matches_in(&text.text, "assistant text", args.context));
                        }
                    }
                    ContentBlock::Thinking(thinking) if args.thinking || args.all => {
                        if matcher.is_match(&thinking.thinking) {
                            matches.extend(matcher.find_matches_in(&thinking.thinking, "thinking", args.context));
                        }
                    }
                    ContentBlock::ToolUse(tool) if args.tools || args.all => {
                        let input_str = serde_json::to_string(&tool.input).unwrap_or_default();
                        if matcher.is_match(&input_str) {
                            matches.extend(matcher.find_matches_in(&input_str, &format!("tool:{}", tool.name), args.context));
                        }
                    }
                    ContentBlock::ToolResult(result) if args.tools || args.all => {
                        if let Some(content) = &result.content {
                            if let crate::model::content::ToolResultContent::String(text) = content {
                                if matcher.is_match(text) {
                                    matches.extend(matcher.find_matches_in(text, "tool result", args.context));
                                }
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
        LogEntry::System(system) => {
            if let Some(content) = &system.content {
                if matcher.is_match(content) {
                    matches.extend(matcher.find_matches_in(content, "system", args.context));
                }
            }
        }
        LogEntry::Summary(summary) => {
            if matcher.is_match(&summary.summary) {
                matches.extend(matcher.find_matches_in(&summary.summary, "summary", args.context));
            }
        }
        _ => {}
    }

    matches
}

/// Find matches with context in text.
fn find_matches(text: &str, regex: &Regex, location: &str, context_lines: usize) -> Vec<Match> {
    let lines: Vec<&str> = text.lines().collect();
    let mut matches = Vec::new();
    let mut seen_lines = std::collections::HashSet::new();

    for (line_num, line) in lines.iter().enumerate() {
        if regex.is_match(line) && !seen_lines.contains(&line_num) {
            seen_lines.insert(line_num);

            // Get context
            let start = line_num.saturating_sub(context_lines);
            let end = (line_num + context_lines + 1).min(lines.len());

            let context_before = lines[start..line_num].join("\n");
            let context_after = lines[(line_num + 1)..end].join("\n");

            // Extract matched portion
            let matched_text = if let Some(m) = regex.find(line) {
                m.as_str().to_string()
            } else {
                line.to_string()
            };

            matches.push(Match {
                location: location.to_string(),
                line: (*line).to_string(),
                context_before,
                matched_text,
                context_after,
            });
        }
    }

    matches
}

/// Find fuzzy matches with context in text.
fn find_fuzzy_matches(
    text: &str,
    pattern: &str,
    location: &str,
    context_lines: usize,
    ignore_case: bool,
    threshold: u8,
) -> Vec<Match> {
    let lines: Vec<&str> = text.lines().collect();
    let mut matches = Vec::new();
    let mut seen_lines = std::collections::HashSet::new();

    for (line_num, line) in lines.iter().enumerate() {
        if let Some(fuzzy_result) = fuzzy_match(pattern, line, ignore_case, threshold) {
            if !seen_lines.contains(&line_num) {
                seen_lines.insert(line_num);

                // Get context
                let start = line_num.saturating_sub(context_lines);
                let end = (line_num + context_lines + 1).min(lines.len());

                let context_before = lines[start..line_num].join("\n");
                let context_after = lines[(line_num + 1)..end].join("\n");

                matches.push(Match {
                    location: format!("{} (score:{})", location, fuzzy_result.score),
                    line: (*line).to_string(),
                    context_before,
                    matched_text: fuzzy_result.matched_text,
                    context_after,
                });
            }
        }
    }

    matches
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_find_matches_single_line() {
        let regex = Regex::new("hello").unwrap();
        let matches = find_matches("hello world", &regex, "test", 0);
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].matched_text, "hello");
    }

    #[test]
    fn test_find_matches_with_context() {
        let regex = Regex::new("target").unwrap();
        let text = "line 1\nline 2\ntarget line\nline 4\nline 5";
        let matches = find_matches(text, &regex, "test", 1);
        assert_eq!(matches.len(), 1);
        assert!(matches[0].context_before.contains("line 2"));
        assert!(matches[0].context_after.contains("line 4"));
    }

    #[test]
    fn test_find_matches_no_match() {
        let regex = Regex::new("notfound").unwrap();
        let matches = find_matches("hello world", &regex, "test", 0);
        assert!(matches.is_empty());
    }

    // Fuzzy matching tests

    #[test]
    fn test_fuzzy_match_exact() {
        // Exact match should have very high score
        let result = fuzzy_match("hello", "hello world", false, 60);
        assert!(result.is_some());
        let m = result.unwrap();
        assert_eq!(m.matched_text, "hello");
        assert!(m.score >= 80); // High score for exact match
    }

    #[test]
    fn test_fuzzy_match_subsequence() {
        // Characters in order but not consecutive: "hlo" in "hello"
        let result = fuzzy_match("hlo", "hello", false, 50);
        assert!(result.is_some());
        let m = result.unwrap();
        assert_eq!(m.matched_text, "hello");
    }

    #[test]
    fn test_fuzzy_match_case_insensitive() {
        let result = fuzzy_match("HELLO", "hello world", true, 60);
        assert!(result.is_some());
        let m = result.unwrap();
        assert_eq!(m.matched_text, "hello");
    }

    #[test]
    fn test_fuzzy_match_case_sensitive_fail() {
        // Should fail if case doesn't match and ignore_case is false
        let result = fuzzy_match("HELLO", "hello world", false, 60);
        assert!(result.is_none());
    }

    #[test]
    fn test_fuzzy_match_threshold() {
        // With very high threshold, scattered matches should fail
        let result = fuzzy_match("abc", "a___b___c", false, 90);
        assert!(result.is_none()); // Scattered match should have low score
    }

    #[test]
    fn test_fuzzy_match_no_match() {
        let result = fuzzy_match("xyz", "hello world", false, 50);
        assert!(result.is_none());
    }

    #[test]
    fn test_fuzzy_match_partial_pattern() {
        // Only partial pattern found
        let result = fuzzy_match("abc", "ab", false, 50);
        assert!(result.is_none()); // 'c' is not found
    }

    #[test]
    fn test_fuzzy_match_word_boundary_bonus() {
        // "hw" matching "hello world" should find h at start, w at word start
        let result = fuzzy_match("hw", "hello world", false, 50);
        assert!(result.is_some());
        let m = result.unwrap();
        // Should have decent score due to word boundary matches
        assert!(m.score >= 60);
    }

    #[test]
    fn test_find_fuzzy_matches_single_line() {
        // text comes first, then pattern
        let matches = find_fuzzy_matches("hello world", "hello", "test", 0, false, 60);
        assert_eq!(matches.len(), 1);
        assert!(matches[0].matched_text.contains("hello"));
    }

    #[test]
    fn test_find_fuzzy_matches_multiline() {
        let text = "first line\nhello world\nlast line";
        // text comes first, then pattern
        let matches = find_fuzzy_matches(text, "hello", "test", 1, false, 60);
        assert_eq!(matches.len(), 1);
        assert!(matches[0].context_before.contains("first"));
        assert!(matches[0].context_after.contains("last"));
    }

    #[test]
    fn test_fuzzy_score_consecutive_bonus() {
        // "ab" in "ab" should score higher than "ab" in "a_b"
        let result_consecutive = fuzzy_match("ab", "ab", false, 0);
        let result_scattered = fuzzy_match("ab", "a_b", false, 0);

        assert!(result_consecutive.is_some());
        assert!(result_scattered.is_some());

        let score_consecutive = result_consecutive.unwrap().score;
        let score_scattered = result_scattered.unwrap().score;

        assert!(score_consecutive > score_scattered);
    }
}
