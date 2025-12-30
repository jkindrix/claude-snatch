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
    /// Start index of match in the text.
    start: usize,
    /// End index of match in the text.
    end: usize,
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

    // Get the match range
    let start = match_positions.first().copied().unwrap_or(0);
    let end = match_positions.last().copied().unwrap_or(0) + 1;

    Some(FuzzyMatch { score, start, end })
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

    // Check token usage filters
    if args.min_tokens.is_some() || args.max_tokens.is_some() {
        if let Some(tokens) = get_entry_tokens(entry) {
            if let Some(min) = args.min_tokens {
                if tokens < min {
                    return false;
                }
            }
            if let Some(max) = args.max_tokens {
                if tokens > max {
                    return false;
                }
            }
        } else {
            // No token info available - skip if filter is active
            return false;
        }
    }

    // Check git branch filter
    if let Some(ref branch_filter) = args.git_branch {
        if !matches_git_branch(entry, branch_filter) {
            return false;
        }
    }

    true
}

/// Check if an entry matches the git branch filter.
fn matches_git_branch(entry: &LogEntry, branch_filter: &str) -> bool {
    let filter_lower = branch_filter.to_lowercase();

    let branch: Option<&str> = match entry {
        LogEntry::User(msg) => msg.git_branch.as_deref(),
        LogEntry::Assistant(msg) => msg.git_branch.as_deref(),
        LogEntry::System(msg) => msg.git_branch.as_deref(),
        // SummaryMessage doesn't have git_branch
        LogEntry::Summary(_) => None,
        _ => None,
    };

    match branch {
        Some(b) => b.to_lowercase().contains(&filter_lower),
        None => false, // No branch info means no match
    }
}

/// Get token count from an entry (assistant messages have usage info).
fn get_entry_tokens(entry: &LogEntry) -> Option<u64> {
    match entry {
        LogEntry::Assistant(msg) => {
            // Get total tokens from usage info
            msg.message.usage.as_ref().map(|u| {
                u.input_tokens + u.output_tokens
            })
        }
        _ => None,
    }
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
    // Reject empty search patterns - they would match everything and aren't useful
    if args.pattern.is_empty() {
        return Err(SnatchError::InvalidArgument {
            name: "pattern".to_string(),
            reason: "search pattern cannot be empty".to_string(),
        });
    }

    // Reject whitespace-only patterns (also not useful)
    if args.pattern.trim().is_empty() {
        return Err(SnatchError::InvalidArgument {
            name: "pattern".to_string(),
            reason: "search pattern cannot be whitespace-only".to_string(),
        });
    }

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
                            score: m.score,
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

    // Sort results by relevance if requested
    if args.sort && !all_results.is_empty() {
        all_results.sort_by(|a, b| b.score.cmp(&a.score));
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
                println!("  [{}]", format_match_label(&result.entry_type, &result.location));

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
    /// Relevance score (0-100).
    score: u8,
}

/// A match within an entry.
struct Match {
    location: String,
    line: String,
    context_before: String,
    matched_text: String,
    context_after: String,
    /// Relevance score (0-100).
    score: u8,
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

            // Extract matched portion and calculate score
            let (matched_text, score) = if let Some(m) = regex.find(line) {
                let matched = m.as_str().to_string();
                let score = calculate_regex_score(line, &matched, m.start());
                (matched, score)
            } else {
                (line.to_string(), 50) // Base score for full-line match
            };

            matches.push(Match {
                location: location.to_string(),
                line: (*line).to_string(),
                context_before,
                matched_text,
                context_after,
                score,
            });
        }
    }

    matches
}

/// Calculate relevance score for a regex match.
fn calculate_regex_score(line: &str, matched: &str, match_start: usize) -> u8 {
    let mut score: f64 = 50.0; // Base score

    // Bonus for matches at start of line (0-15 points)
    if match_start == 0 {
        score += 15.0;
    } else if match_start < 10 {
        score += 10.0 - match_start as f64;
    }

    // Bonus for larger match coverage (0-20 points)
    let coverage = matched.len() as f64 / line.len().max(1) as f64;
    score += coverage * 20.0;

    // Bonus for word boundary matches (0-10 points)
    let at_word_start = match_start == 0 ||
        !line.chars().nth(match_start.saturating_sub(1))
            .map(|c| c.is_alphanumeric())
            .unwrap_or(false);
    let at_word_end = match_start + matched.len() >= line.len() ||
        !line.chars().nth(match_start + matched.len())
            .map(|c| c.is_alphanumeric())
            .unwrap_or(false);

    if at_word_start && at_word_end {
        score += 10.0; // Full word match
    } else if at_word_start || at_word_end {
        score += 5.0; // Partial word boundary
    }

    score.clamp(0.0, 100.0) as u8
}

/// Expand a substring to word boundaries within the given text.
///
/// Given start/end indices that may be mid-word, expand them to include
/// complete words at both ends for better readability.
fn expand_to_word_boundaries(text: &str, start: usize, end: usize) -> String {
    let chars: Vec<char> = text.chars().collect();
    if chars.is_empty() || start >= chars.len() {
        return text.to_string();
    }

    // Expand start backwards to word boundary (or start of string)
    let mut expanded_start = start;
    while expanded_start > 0 && chars[expanded_start - 1].is_alphanumeric() {
        expanded_start -= 1;
    }

    // Expand end forwards to word boundary (or end of string)
    let mut expanded_end = end.min(chars.len());
    while expanded_end < chars.len() && chars[expanded_end].is_alphanumeric() {
        expanded_end += 1;
    }

    chars[expanded_start..expanded_end].iter().collect()
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

                // Expand matched_text to word boundaries for readability
                let expanded_text = expand_to_word_boundaries(
                    line,
                    fuzzy_result.start,
                    fuzzy_result.end,
                );

                matches.push(Match {
                    location: location.to_string(),
                    line: (*line).to_string(),
                    context_before,
                    matched_text: expanded_text,
                    context_after,
                    score: fuzzy_result.score,
                });
            }
        }
    }

    matches
}

/// Format match label in a non-redundant way.
///
/// Simplifies labels like "user in user message" to just "user".
fn format_match_label(entry_type: &str, location: &str) -> String {
    // Handle cases where entry_type and location are redundant
    match (entry_type, location) {
        // Simple message types - just show the type
        ("user", "user message") => "user".to_string(),
        ("summary", "summary") => "summary".to_string(),
        ("system", "system") => "system".to_string(),
        // Assistant text - just show "assistant"
        ("assistant", "assistant text") => "assistant".to_string(),
        // Assistant thinking - show "assistant/thinking"
        ("assistant", "thinking") => "assistant/thinking".to_string(),
        // Tool use - show "tool: name"
        ("assistant", loc) if loc.starts_with("tool:") => loc.to_string(),
        // Tool result - show "tool result"
        ("assistant", "tool result") => "tool result".to_string(),
        // Default: show "type/location" if different, or just type
        (t, l) if t == l => t.to_string(),
        (t, l) => format!("{t}/{l}"),
    }
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
        let text = "hello world";
        let result = fuzzy_match("hello", text, false, 60);
        assert!(result.is_some());
        let m = result.unwrap();
        let matched = expand_to_word_boundaries(text, m.start, m.end);
        assert_eq!(matched, "hello");
        assert!(m.score >= 80); // High score for exact match
    }

    #[test]
    fn test_fuzzy_match_subsequence() {
        // Characters in order but not consecutive: "hlo" in "hello"
        let text = "hello";
        let result = fuzzy_match("hlo", text, false, 50);
        assert!(result.is_some());
        let m = result.unwrap();
        let matched = expand_to_word_boundaries(text, m.start, m.end);
        assert_eq!(matched, "hello");
    }

    #[test]
    fn test_fuzzy_match_case_insensitive() {
        let text = "hello world";
        let result = fuzzy_match("HELLO", text, true, 60);
        assert!(result.is_some());
        let m = result.unwrap();
        let matched = expand_to_word_boundaries(text, m.start, m.end);
        assert_eq!(matched, "hello");
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

    #[test]
    fn test_regex_score_word_boundary() {
        // Full word match at start should score high
        let score1 = calculate_regex_score("hello world", "hello", 0);
        assert!(score1 >= 75, "Start + word boundary should score >= 75, got {}", score1);

        // Match in middle without word boundary should score lower
        let score2 = calculate_regex_score("the hello world", "ello", 5);
        assert!(score2 < score1, "Middle match should score lower");

        // Match at word boundary in middle
        let score3 = calculate_regex_score("the hello world", "hello", 4);
        assert!(score3 > score2, "Word boundary match should score higher than partial");
    }

    #[test]
    fn test_regex_score_coverage() {
        // Larger coverage should score higher
        let score_full = calculate_regex_score("hello", "hello", 0);
        let score_partial = calculate_regex_score("hello world", "hello", 0);

        assert!(score_full > score_partial, "Full coverage should score higher");
    }

    #[test]
    fn test_matches_git_branch_exact() {
        use crate::model::{UserMessage, UserContent, UserSimpleContent};
        use chrono::Utc;
        use indexmap::IndexMap;

        let msg = UserMessage {
            uuid: "test".to_string(),
            parent_uuid: None,
            timestamp: Utc::now(),
            session_id: "test-session".to_string(),
            version: "2.0.74".to_string(),
            cwd: None,
            git_branch: Some("feature/user-auth".to_string()),
            user_type: None,
            is_sidechain: false,
            is_teammate: None,
            agent_id: None,
            slug: None,
            is_meta: None,
            is_visible_in_transcript_only: None,
            thinking_metadata: None,
            todos: Vec::new(),
            tool_use_result: None,
            message: UserContent::Simple(UserSimpleContent {
                role: "user".to_string(),
                content: "Hello".to_string(),
            }),
            extra: IndexMap::new(),
        };
        let entry = LogEntry::User(msg);

        // Exact match
        assert!(matches_git_branch(&entry, "feature/user-auth"));
        // Partial match
        assert!(matches_git_branch(&entry, "user-auth"));
        assert!(matches_git_branch(&entry, "feature"));
        // Case insensitive
        assert!(matches_git_branch(&entry, "FEATURE"));
        // No match
        assert!(!matches_git_branch(&entry, "develop"));
    }

    #[test]
    fn test_matches_git_branch_none() {
        use crate::model::{UserMessage, UserContent, UserSimpleContent};
        use chrono::Utc;
        use indexmap::IndexMap;

        let msg = UserMessage {
            uuid: "test".to_string(),
            parent_uuid: None,
            timestamp: Utc::now(),
            session_id: "test-session".to_string(),
            version: "2.0.74".to_string(),
            cwd: None,
            git_branch: None, // No branch
            user_type: None,
            is_sidechain: false,
            is_teammate: None,
            agent_id: None,
            slug: None,
            is_meta: None,
            is_visible_in_transcript_only: None,
            thinking_metadata: None,
            todos: Vec::new(),
            tool_use_result: None,
            message: UserContent::Simple(UserSimpleContent {
                role: "user".to_string(),
                content: "Hello".to_string(),
            }),
            extra: IndexMap::new(),
        };
        let entry = LogEntry::User(msg);

        // Should not match when no branch is present
        assert!(!matches_git_branch(&entry, "main"));
    }

    #[test]
    fn test_expand_to_word_boundaries_mid_word() {
        // "ient" within "orient yourself" - should expand to "orient"
        // Indices 2-6 are "ient" in "orient"
        let result = expand_to_word_boundaries("orient yourself", 2, 6);
        assert_eq!(result, "orient");
    }

    #[test]
    fn test_expand_to_word_boundaries_multiple_words() {
        // Spanning from mid-"hello" to mid-"world"
        // "llo wor" should expand to "hello world"
        let result = expand_to_word_boundaries("hello world", 2, 9);
        assert_eq!(result, "hello world");
    }

    #[test]
    fn test_expand_to_word_boundaries_at_word_start() {
        // Already at word start, should expand end only
        let result = expand_to_word_boundaries("hello world", 0, 3);
        assert_eq!(result, "hello");
    }

    #[test]
    fn test_expand_to_word_boundaries_at_word_end() {
        // Already at word end, should expand start only
        let result = expand_to_word_boundaries("hello world", 3, 5);
        assert_eq!(result, "hello");
    }

    #[test]
    fn test_expand_to_word_boundaries_full_word() {
        // Already a complete word
        let result = expand_to_word_boundaries("hello world", 0, 5);
        assert_eq!(result, "hello");
    }

    #[test]
    fn test_expand_to_word_boundaries_with_punctuation() {
        // Word followed by punctuation
        let result = expand_to_word_boundaries("hello, world!", 1, 4);
        assert_eq!(result, "hello");
    }

    #[test]
    fn test_expand_to_word_boundaries_empty() {
        let result = expand_to_word_boundaries("", 0, 0);
        assert_eq!(result, "");
    }

}
