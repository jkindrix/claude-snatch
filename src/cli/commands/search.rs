//! Search command implementation.
//!
//! Searches across sessions for text patterns with optional filters.

use std::collections::HashSet;
use std::io::IsTerminal;
use std::time::SystemTime;

use chrono::{DateTime, Utc};
use indicatif::{ProgressBar, ProgressStyle};
use regex::{Regex, RegexBuilder};

use crate::cli::{Cli, OutputFormat, SearchArgs};
use crate::discovery::Session;
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
        // Safety: positions.len() > 1 guarantees first/last return Some
        let span = positions.last().expect("len > 1") - positions.first().expect("len > 1") + 1;
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

// ─── Batch (multi-pattern) types and helpers ────────────────────────────────

/// Which parts of a log entry to search in batch mode.
#[derive(Debug, Clone, PartialEq, Eq)]
enum BatchScope {
    /// Default: user text + assistant text (no flags).
    Default,
    /// Additive: user text + assistant text + thinking blocks (--thinking).
    Thinking,
    /// Thinking blocks only, exclusive (--thinking-only).
    ThinkingOnly,
    /// Assistant text only (-t assistant).
    Assistant,
    /// User text only (-t user).
    User,
    /// Tool use/result blocks (--tools).
    Tools,
    /// Everything (-a / --all).
    All,
}

impl BatchScope {
    fn from_tsv_flag(s: &str) -> Result<Self> {
        match s.trim() {
            "--thinking" => Ok(Self::Thinking),
            "--thinking-only" => Ok(Self::ThinkingOnly),
            "-t assistant" => Ok(Self::Assistant),
            "-t user" => Ok(Self::User),
            "--tools" => Ok(Self::Tools),
            "-a" | "--all" => Ok(Self::All),
            _ => Err(SnatchError::InvalidArgument {
                name: "scope".to_string(),
                reason: format!(
                    "Unknown scope '{}'. Expected --thinking, --thinking-only, -t assistant, -t user, --tools, or -a",
                    s
                ),
            }),
        }
    }

    /// Build from SearchArgs flags (for positional multi-pattern).
    fn from_search_args(args: &SearchArgs) -> Self {
        if args.all {
            Self::All
        } else if args.thinking_only {
            Self::ThinkingOnly
        } else if args.thinking && args.tools {
            Self::All
        } else if args.thinking {
            Self::Thinking
        } else if args.tools {
            Self::Tools
        } else if let Some(ref t) = args.message_type {
            match t.as_str() {
                "user" => Self::User,
                "assistant" => Self::Assistant,
                _ => Self::Default,
            }
        } else {
            Self::Default
        }
    }
}

/// A pattern with its own scope for batch processing.
struct BatchPattern {
    label: String,
    regex: Regex,
    scope: BatchScope,
}

/// Extract searchable text from an entry for a given scope.
fn extract_text_for_scope<'a>(entry: &'a LogEntry, scope: &BatchScope) -> Vec<&'a str> {
    match (entry, scope) {
        // Default scope: user text + assistant text
        (LogEntry::User(user), BatchScope::Default | BatchScope::User | BatchScope::All) => {
            match &user.message {
                crate::model::UserContent::Simple(s) => vec![s.content.as_str()],
                crate::model::UserContent::Blocks(b) => {
                    b.content.iter().filter_map(|c| {
                        if let ContentBlock::Text(t) = c { Some(t.text.as_str()) } else { None }
                    }).collect()
                }
            }
        }
        // Thinking (additive): user text is included
        (LogEntry::User(user), BatchScope::Thinking) => {
            match &user.message {
                crate::model::UserContent::Simple(s) => vec![s.content.as_str()],
                crate::model::UserContent::Blocks(b) => {
                    b.content.iter().filter_map(|c| {
                        if let ContentBlock::Text(t) = c { Some(t.text.as_str()) } else { None }
                    }).collect()
                }
            }
        }
        (LogEntry::Assistant(assistant), BatchScope::Default | BatchScope::Assistant | BatchScope::All) => {
            assistant.message.content.iter().filter_map(|block| {
                if let ContentBlock::Text(t) = block { Some(t.text.as_str()) } else { None }
            }).collect()
        }
        // Thinking (additive): assistant text + thinking blocks
        (LogEntry::Assistant(assistant), BatchScope::Thinking) => {
            assistant.message.content.iter().filter_map(|block| {
                match block {
                    ContentBlock::Text(t) => Some(t.text.as_str()),
                    ContentBlock::Thinking(t) => Some(t.thinking.as_str()),
                    _ => None,
                }
            }).collect()
        }
        // ThinkingOnly (exclusive): thinking blocks only
        (LogEntry::Assistant(assistant), BatchScope::ThinkingOnly) => {
            assistant.message.content.iter().filter_map(|block| {
                if let ContentBlock::Thinking(t) = block { Some(t.thinking.as_str()) } else { None }
            }).collect()
        }
        // Tools scope tool-use inputs are owned strings — handled separately
        (LogEntry::Assistant(_), BatchScope::Tools) => vec![],
        (LogEntry::System(sys), BatchScope::Default | BatchScope::Thinking | BatchScope::All) => {
            sys.content.as_deref().into_iter().collect()
        }
        (LogEntry::Summary(summary), BatchScope::Default | BatchScope::Thinking | BatchScope::All) => {
            vec![summary.summary.as_str()]
        }
        _ => vec![],
    }
}

/// Count regex matches across text chunks.
fn count_regex_in_texts(regex: &Regex, texts: &[&str]) -> usize {
    texts.iter().map(|t| regex.find_iter(t).count()).sum()
}

/// Count matches in tool-use/tool-result blocks (needs owned strings).
fn count_tool_matches(entry: &LogEntry, regex: &Regex) -> usize {
    match entry {
        LogEntry::Assistant(assistant) => {
            let mut count = 0;
            for block in &assistant.message.content {
                match block {
                    ContentBlock::ToolUse(tool) => {
                        let input_str = serde_json::to_string(&tool.input).unwrap_or_default();
                        count += regex.find_iter(&input_str).count();
                    }
                    ContentBlock::ToolResult(result) => {
                        if let Some(content) = &result.content {
                            if let crate::model::content::ToolResultContent::String(text) = content {
                                count += regex.find_iter(text).count();
                            }
                        }
                    }
                    _ => {}
                }
            }
            count
        }
        // Note: single-pattern search_entry does NOT search User tool results,
        // only Assistant ToolUse/ToolResult blocks. Keep batch path consistent.
        _ => 0,
    }
}

/// Count all matches for one pattern against one entry.
fn count_pattern_matches(entry: &LogEntry, pattern: &BatchPattern) -> usize {
    let texts = extract_text_for_scope(entry, &pattern.scope);
    let mut count = count_regex_in_texts(&pattern.regex, &texts);

    // Tool content requires owned strings, handled separately
    if pattern.scope == BatchScope::Tools || pattern.scope == BatchScope::All {
        count += count_tool_matches(entry, &pattern.regex);
    }

    // All scope on assistant also includes thinking
    if pattern.scope == BatchScope::All {
        if let LogEntry::Assistant(assistant) = entry {
            for block in &assistant.message.content {
                if let ContentBlock::Thinking(t) = block {
                    count += pattern.regex.find_iter(&t.thinking).count();
                }
            }
        }
    }

    count
}

/// Parse a TSV patterns file into batch patterns.
fn parse_patterns_tsv(path: &std::path::Path) -> Result<Vec<BatchPattern>> {
    let content = std::fs::read_to_string(path)
        .map_err(|e| SnatchError::io(format!("reading patterns file {}", path.display()), e))?;

    let mut patterns = Vec::new();
    for (line_num, line) in content.lines().enumerate() {
        if line.trim().is_empty() || line.starts_with('#') {
            continue;
        }
        let fields: Vec<&str> = line.split('\t').collect();
        if fields.first() == Some(&"category") {
            continue; // header
        }
        if fields.len() < 5 {
            return Err(SnatchError::InvalidArgument {
                name: "patterns_tsv".to_string(),
                reason: format!(
                    "Line {} has {} fields, expected 5 (category, subcategory, label, scope, pattern)",
                    line_num + 1, fields.len()
                ),
            });
        }
        let scope = BatchScope::from_tsv_flag(fields[3])?;
        let regex = RegexBuilder::new(fields[4])
            .build()
            .map_err(|e| SnatchError::InvalidArgument {
                name: "pattern".to_string(),
                reason: format!("Line {}: invalid regex '{}': {}", line_num + 1, fields[4], e),
            })?;
        patterns.push(BatchPattern {
            label: format!("{}\t{}\t{}", fields[0], fields[1], fields[2]),
            regex,
            scope,
        });
    }
    Ok(patterns)
}

/// Collect and filter sessions based on search args (shared by single-pattern and batch paths).
fn collect_sessions(cli: &Cli, args: &SearchArgs) -> Result<Vec<Session>> {
    let claude_dir = get_claude_dir(cli.claude_dir.as_ref())?;

    let mut sessions = if let Some(session_id) = &args.session {
        let session = claude_dir
            .find_session(session_id)?
            .ok_or_else(|| SnatchError::SessionNotFound {
                session_id: session_id.clone(),
            })?;
        vec![session]
    } else if let Some(project_filter) = &args.project {
        let projects = claude_dir.projects()?;
        let matched = super::helpers::filter_projects(projects, project_filter);
        let mut sess = Vec::new();
        for project in matched {
            sess.extend(project.sessions()?);
        }
        sess
    } else {
        claude_dir.all_sessions()?
    };

    // Apply --since / --until date filters
    let since_time: Option<SystemTime> = if let Some(ref since) = args.since {
        Some(super::parse_date_filter(since)?)
    } else {
        None
    };
    let until_time: Option<SystemTime> = if let Some(ref until) = args.until {
        Some(super::parse_date_filter(until)?)
    } else {
        None
    };
    if since_time.is_some() || until_time.is_some() {
        sessions.retain(|s| {
            let modified = s.modified_time();
            if let Some(since) = since_time {
                if modified < since {
                    return false;
                }
            }
            if let Some(until) = until_time {
                if modified > until {
                    return false;
                }
            }
            true
        });
    }

    // Apply --recent N (most recent sessions by modification time)
    if let Some(n) = args.recent {
        sessions.sort_by(|a, b| b.modified_time().cmp(&a.modified_time()));
        sessions.truncate(n);
    }

    // Apply --no-subagents filter
    if args.no_subagents {
        sessions.retain(|s| !s.is_subagent());
    }

    Ok(sessions)
}

/// Run the batch (multi-pattern, single-pass) search and output counts.
fn run_batch(cli: &Cli, args: &SearchArgs, patterns: Vec<BatchPattern>) -> Result<()> {
    let sessions = collect_sessions(cli, args)?;

    let mut counts: Vec<usize> = vec![0; patterns.len()];
    // Per-pattern per-session breakdown (only allocated when --breakdown)
    let mut per_session: Vec<std::collections::HashMap<String, (usize, Option<SystemTime>)>> =
        if args.breakdown {
            vec![std::collections::HashMap::new(); patterns.len()]
        } else {
            Vec::new()
        };

    let session_count = sessions.len();
    let show_progress = session_count > 10 && std::io::stderr().is_terminal() && !cli.quiet;
    let progress = if show_progress {
        let pb = ProgressBar::new(session_count as u64);
        pb.set_style(
            ProgressStyle::default_bar()
                .template("{spinner:.cyan} [{bar:40.cyan/dim}] {pos}/{len} sessions ({eta} remaining)")
                .unwrap()
                .progress_chars("█▓░"),
        );
        Some(pb)
    } else {
        None
    };

    for session in &sessions {
        if let Some(ref pb) = progress {
            pb.inc(1);
        }
        let entries = match session.parse_with_options(cli.max_file_size) {
            Ok(e) => e,
            Err(_) => continue,
        };

        // Track per-pattern session counts for breakdown mode
        let mut session_counts: Vec<usize> = if args.breakdown {
            vec![0; patterns.len()]
        } else {
            Vec::new()
        };

        for entry in &entries {
            for (i, pattern) in patterns.iter().enumerate() {
                let c = count_pattern_matches(entry, pattern);
                counts[i] += c;
                if args.breakdown {
                    session_counts[i] += c;
                }
            }
        }

        if args.breakdown {
            let sid = session.session_id().to_string();
            let modified = Some(session.modified_time());
            for (i, &sc) in session_counts.iter().enumerate() {
                if sc > 0 {
                    per_session[i].insert(sid.clone(), (sc, modified));
                }
            }
        }
    }

    if let Some(pb) = progress {
        pb.finish_and_clear();
    }

    // Output
    let is_tsv_mode = args.patterns_tsv.is_some();

    if args.breakdown {
        output_batch_breakdown(cli, &patterns, &counts, &per_session, is_tsv_mode)?;
    } else {
        output_batch_aggregate(cli, &patterns, &counts, is_tsv_mode)?;
    }

    Ok(())
}

/// Output batch results as aggregate counts (original behavior).
fn output_batch_aggregate(
    cli: &Cli,
    patterns: &[BatchPattern],
    counts: &[usize],
    is_tsv_mode: bool,
) -> Result<()> {
    match cli.effective_output() {
        OutputFormat::Json => {
            if is_tsv_mode {
                let entries: Vec<serde_json::Value> = patterns.iter().enumerate().map(|(i, p)| {
                    let parts: Vec<&str> = p.label.splitn(3, '\t').collect();
                    serde_json::json!({
                        "category": parts.first().unwrap_or(&""),
                        "subcategory": parts.get(1).unwrap_or(&""),
                        "label": parts.get(2).unwrap_or(&""),
                        "count": counts[i],
                    })
                }).collect();
                println!("{}", serde_json::to_string_pretty(&entries)?);
            } else {
                let map: Vec<serde_json::Value> = patterns.iter().enumerate().map(|(i, p)| {
                    serde_json::json!({
                        "pattern": p.label,
                        "count": counts[i],
                    })
                }).collect();
                println!("{}", serde_json::to_string_pretty(&map)?);
            }
        }
        OutputFormat::Tsv => {
            if is_tsv_mode {
                println!("category\tsubcategory\tlabel\tcount");
                for (i, p) in patterns.iter().enumerate() {
                    println!("{}\t{}", p.label, counts[i]);
                }
            } else {
                println!("pattern\tcount");
                for (i, p) in patterns.iter().enumerate() {
                    println!("{}\t{}", p.label, counts[i]);
                }
            }
        }
        _ => {
            if is_tsv_mode {
                let mut prev_cat = String::new();
                for (i, p) in patterns.iter().enumerate() {
                    let parts: Vec<&str> = p.label.splitn(3, '\t').collect();
                    let cat = parts.first().unwrap_or(&"");
                    let label = parts.get(2).unwrap_or(&"");
                    if *cat != prev_cat {
                        if !prev_cat.is_empty() { println!(); }
                        println!("=== {} ===", cat.to_uppercase());
                        prev_cat = cat.to_string();
                    }
                    println!("{:<7} {}", counts[i], label);
                }
            } else {
                for (i, p) in patterns.iter().enumerate() {
                    println!("{:<7} {}", counts[i], p.label);
                }
            }
        }
    }
    Ok(())
}

/// Output batch results with per-session breakdown (--breakdown).
fn output_batch_breakdown(
    cli: &Cli,
    patterns: &[BatchPattern],
    counts: &[usize],
    per_session: &[std::collections::HashMap<String, (usize, Option<SystemTime>)>],
    is_tsv_mode: bool,
) -> Result<()> {
    match cli.effective_output() {
        OutputFormat::Json => {
            let entries: Vec<serde_json::Value> = patterns.iter().enumerate().map(|(i, p)| {
                let parts: Vec<&str> = p.label.splitn(3, '\t').collect();
                let sessions: Vec<serde_json::Value> = {
                    let mut sess: Vec<_> = per_session[i].iter().collect();
                    sess.sort_by(|a, b| (b.1).0.cmp(&(a.1).0));
                    sess.iter().map(|(sid, (count, modified))| {
                        let mut val = serde_json::json!({
                            "session_id": sid,
                            "count": count,
                        });
                        if let Some(time) = modified {
                            val["date"] = serde_json::Value::String(format_date(time));
                        }
                        val
                    }).collect()
                };
                if is_tsv_mode {
                    serde_json::json!({
                        "category": parts.first().unwrap_or(&""),
                        "subcategory": parts.get(1).unwrap_or(&""),
                        "label": parts.get(2).unwrap_or(&""),
                        "count": counts[i],
                        "sessions": sessions,
                    })
                } else {
                    serde_json::json!({
                        "pattern": p.label,
                        "count": counts[i],
                        "sessions": sessions,
                    })
                }
            }).collect();
            println!("{}", serde_json::to_string_pretty(&entries)?);
        }
        _ => {
            let mut prev_cat = String::new();
            for (i, p) in patterns.iter().enumerate() {
                if counts[i] == 0 {
                    continue;
                }

                if is_tsv_mode {
                    let parts: Vec<&str> = p.label.splitn(3, '\t').collect();
                    let cat = parts.first().unwrap_or(&"");
                    let label = parts.get(2).unwrap_or(&"");
                    if *cat != prev_cat {
                        if !prev_cat.is_empty() { println!(); }
                        println!("=== {} ===", cat.to_uppercase());
                        prev_cat = cat.to_string();
                    }
                    println!("{:<7} {}", counts[i], label);
                } else {
                    println!("{:<7} {}", counts[i], p.label);
                }

                // Per-session breakdown
                let mut sess: Vec<_> = per_session[i].iter().collect();
                sess.sort_by(|a, b| (b.1).0.cmp(&(a.1).0));
                for (sid, (count, modified)) in &sess {
                    let short_id = &sid[..8.min(sid.len())];
                    let date_str = modified
                        .as_ref()
                        .map(format_date)
                        .unwrap_or_else(|| "unknown".to_string());
                    println!("  {}  {}  {}", date_str, short_id, count);
                }
            }
        }
    }
    Ok(())
}

// ─── Main entry point ───────────────────────────────────────────────────────

/// Run the search command.
pub fn run(cli: &Cli, args: &SearchArgs) -> Result<()> {
    // ── TSV batch mode ──────────────────────────────────────────────────
    // Design note: --patterns-tsv changes search's semantics from "find and display results"
    // to "batch count across heterogeneous queries." This is coherent for counting but may not
    // remain coherent if analytical features are needed (ratios, trend comparison, aggregation).
    // Revisit signal: if this path needs output modes or processing logic that conflicts with
    // search's primary purpose, extract to a dedicated `snatch analyze` command.
    if let Some(ref tsv_path) = args.patterns_tsv {
        let patterns = parse_patterns_tsv(tsv_path)?;
        if patterns.is_empty() {
            println!("No patterns found in {}", tsv_path.display());
            return Ok(());
        }
        return run_batch(cli, args, patterns);
    }

    // ── Multi-pattern positional mode (single-pass batch) ───────────────
    // Multi-pattern positional: general-purpose single-pass batch search.
    // All patterns share the same scope flags from the CLI.
    if args.pattern.len() > 1 {
        let scope = BatchScope::from_search_args(args);
        let mut patterns = Vec::new();
        for pat_str in &args.pattern {
            let regex = RegexBuilder::new(pat_str)
                .case_insensitive(args.ignore_case)
                .build()
                .map_err(|e| SnatchError::InvalidArgument {
                    name: "pattern".to_string(),
                    reason: format!("invalid regex '{}': {}", pat_str, e),
                })?;
            patterns.push(BatchPattern {
                label: pat_str.clone(),
                regex,
                scope: scope.clone(),
            });
        }
        return run_batch(cli, args, patterns);
    }

    // ── Single-pattern mode (original behavior) ─────────────────────────
    if args.pattern.is_empty() {
        return Err(SnatchError::InvalidArgument {
            name: "pattern".to_string(),
            reason: "search pattern cannot be empty".to_string(),
        });
    }

    let pattern = &args.pattern[0];

    if pattern.trim().is_empty() {
        return Err(SnatchError::InvalidArgument {
            name: "pattern".to_string(),
            reason: "search pattern cannot be whitespace-only".to_string(),
        });
    }

    // Build regex
    let regex = RegexBuilder::new(pattern)
        .case_insensitive(args.ignore_case)
        .build()
        .map_err(|e| SnatchError::InvalidArgument {
            name: "pattern".to_string(),
            reason: e.to_string(),
        })?;

    // Build exclude regex if specified
    let exclude_regex = if let Some(ref exclude_pattern) = args.exclude {
        Some(
            RegexBuilder::new(exclude_pattern)
                .case_insensitive(args.ignore_case)
                .build()
                .map_err(|e| SnatchError::InvalidArgument {
                    name: "exclude".to_string(),
                    reason: e.to_string(),
                })?,
        )
    } else {
        None
    };

    // Collect sessions to search
    let sessions = collect_sessions(cli, args)?;

    let mut total_matches = 0;
    let mut all_results = Vec::new();
    let mut sessions_with_matches: HashSet<String> = HashSet::new();
    // Maps session_id -> (project_path, match_count, modified_time)
    let mut match_counts: std::collections::HashMap<String, (String, usize, Option<SystemTime>)> =
        std::collections::HashMap::new();

    // Create progress bar for interactive sessions with many sessions
    let session_count = sessions.len();
    let show_progress = session_count > 10 && std::io::stderr().is_terminal() && !cli.quiet;
    let progress = if show_progress {
        let pb = ProgressBar::new(session_count as u64);
        pb.set_style(
            ProgressStyle::default_bar()
                .template("{spinner:.cyan} [{bar:40.cyan/dim}] {pos}/{len} sessions searched")
                .unwrap()
                .progress_chars("█▓░"),
        );
        Some(pb)
    } else {
        None
    };

    // In count mode, use occurrence-based counting (consistent with batch path).
    // Build a BatchPattern to reuse the same counting logic.
    let count_mode_pattern = if args.count {
        let scope = BatchScope::from_search_args(args);
        Some(BatchPattern {
            label: pattern.clone(),
            regex: regex.clone(),
            scope,
        })
    } else {
        None
    };

    // Search each session
    for session in &sessions {
        if let Some(ref pb) = progress {
            pb.inc(1);
        }
        let entries = match session.parse_with_options(cli.max_file_size) {
            Ok(e) => e,
            Err(_) => continue, // Skip unparseable sessions
        };

        // Compute phase context for this session
        let phase_ctx = PhaseContext::from_entries(&entries);

        let mut session_match_count = 0;

        for entry in &entries {
            // Apply all filters
            if !matches_filters(entry, args) {
                continue;
            }

            // Apply --exclude: skip entries where exclude pattern matches
            if let Some(ref excl) = exclude_regex {
                let scope = BatchScope::from_search_args(args);
                let texts = extract_text_for_scope(entry, &scope);
                if texts.iter().any(|t| excl.is_match(t)) {
                    continue;
                }
            }

            if let Some(ref bp) = count_mode_pattern {
                // Count mode: use occurrence-based counting (same as batch path)
                let entry_count = count_pattern_matches(entry, bp);
                if entry_count > 0 {
                    sessions_with_matches.insert(session.session_id().to_string());
                    total_matches += entry_count;
                    session_match_count += entry_count;
                }
            } else {
                // Normal mode: use line-based matching with context
                let matches = search_entry(entry, &regex, args);

                if !matches.is_empty() {
                    sessions_with_matches.insert(session.session_id().to_string());

                    for m in matches {
                        total_matches += 1;
                        session_match_count += 1;

                        if !args.files_only {
                            let (phase, minutes_in, post_compaction) =
                                phase_ctx.classify(entry);

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
                                phase: Some(phase),
                                minutes_in,
                                post_compaction: Some(post_compaction),
                            };

                            all_results.push(result);
                        }

                        // Check limit (unless --no-limit is set)
                        if !args.no_limit && total_matches >= args.limit {
                            break;
                        }
                    }
                }
            }

            // Check limit after processing entry (unless --no-limit is set)
            if !args.no_limit && total_matches >= args.limit {
                break;
            }
        }

        if session_match_count > 0 {
            match_counts.insert(
                session.session_id().to_string(),
                (
                    session.project_path().to_string(),
                    session_match_count,
                    Some(session.modified_time()),
                ),
            );
        }

        // Check limit (unless --no-limit is set)
        if !args.no_limit && total_matches >= args.limit {
            break;
        }
    }

    // Finish progress bar
    if let Some(pb) = progress {
        pb.finish_and_clear();
    }

    // Apply --phase filter
    if let Some(ref phase_filter) = args.phase {
        let target_phase = match phase_filter.to_lowercase().as_str() {
            "early" => Some(SessionPhase::Early),
            "middle" | "mid" => Some(SessionPhase::Middle),
            "late" => Some(SessionPhase::Late),
            _ => None,
        };
        if let Some(target) = target_phase {
            all_results.retain(|r| r.phase == Some(target));
            total_matches = all_results.len();
        }
    }

    // Sort results by relevance if requested
    if args.sort && !all_results.is_empty() {
        all_results.sort_by(|a, b| b.score.cmp(&a.score));
    }

    // Output results based on mode
    if args.files_only {
        output_files_only(cli, &sessions_with_matches)?;
    } else if args.aggregate_by_session {
        output_aggregate(cli, &match_counts, total_matches)?;
    } else if args.count {
        output_count(cli, args, &match_counts, total_matches)?;
    } else if args.match_only {
        output_match_only(cli, &all_results)?;
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

/// Format a SystemTime as YYYY-MM-DD.
fn format_date(time: &SystemTime) -> String {
    let dt: DateTime<Utc> = (*time).into();
    dt.format("%Y-%m-%d").to_string()
}

/// Output match counts.
fn output_count(
    cli: &Cli,
    args: &SearchArgs,
    match_counts: &std::collections::HashMap<String, (String, usize, Option<SystemTime>)>,
    total: usize,
) -> Result<()> {
    // --quiet with --count: output only the total
    if cli.quiet {
        match cli.effective_output() {
            OutputFormat::Json => {
                println!("{}", serde_json::json!({ "total": total }));
            }
            _ => {
                println!("{}", total);
            }
        }
        return Ok(());
    }

    match cli.effective_output() {
        OutputFormat::Json => {
            // Build a structured JSON with project info
            let by_session: std::collections::HashMap<&str, serde_json::Value> = match_counts
                .iter()
                .map(|(session_id, (project, count, modified))| {
                    let mut val = serde_json::json!({
                        "project": project,
                        "count": count
                    });
                    if args.with_date {
                        if let Some(time) = modified {
                            val["date"] = serde_json::Value::String(format_date(time));
                        }
                    }
                    (session_id.as_str(), val)
                })
                .collect();
            let output = serde_json::json!({
                "total": total,
                "by_session": by_session,
            });
            println!("{}", serde_json::to_string_pretty(&output)?);
        }
        _ => {
            if match_counts.len() == 1 {
                // Single session - just show count
                println!("{}", total);
            } else {
                // Multiple sessions - show per-session counts with project
                let mut counts: Vec<(&String, &(String, usize, Option<SystemTime>))> =
                    match_counts.iter().collect();
                counts.sort_by(|a, b| (b.1).1.cmp(&(a.1).1));

                for (session_id, (project, count, modified)) in counts {
                    let short_id = &session_id[..8.min(session_id.len())];
                    if args.with_date {
                        let date_str = modified
                            .as_ref()
                            .map(format_date)
                            .unwrap_or_else(|| "unknown".to_string());
                        println!("{} ({}) [{}]:{}", short_id, project, date_str, count);
                    } else {
                        println!("{} ({}):{}", short_id, project, count);
                    }
                }
                println!();
                println!("Total: {}", total);
            }
        }
    }
    Ok(())
}

/// Output one line per session with match count (--aggregate-by-session).
fn output_aggregate(
    cli: &Cli,
    match_counts: &std::collections::HashMap<String, (String, usize, Option<SystemTime>)>,
    total: usize,
) -> Result<()> {
    if match_counts.is_empty() {
        println!("No matches found.");
        return Ok(());
    }

    match cli.effective_output() {
        OutputFormat::Json => {
            let mut entries: Vec<serde_json::Value> = match_counts
                .iter()
                .map(|(session_id, (project, count, modified))| {
                    let mut val = serde_json::json!({
                        "session_id": session_id,
                        "project": project,
                        "count": count,
                    });
                    if let Some(time) = modified {
                        val["date"] = serde_json::Value::String(format_date(time));
                    }
                    val
                })
                .collect();
            entries.sort_by(|a, b| {
                b["count"].as_u64().unwrap_or(0).cmp(&a["count"].as_u64().unwrap_or(0))
            });
            let output = serde_json::json!({
                "total": total,
                "sessions": entries,
            });
            println!("{}", serde_json::to_string_pretty(&output)?);
        }
        _ => {
            let mut counts: Vec<(&String, &(String, usize, Option<SystemTime>))> =
                match_counts.iter().collect();
            counts.sort_by(|a, b| (b.1).1.cmp(&(a.1).1));

            for (session_id, (_project, count, modified)) in &counts {
                let short_id = &session_id[..8.min(session_id.len())];
                let date_str = modified
                    .as_ref()
                    .map(format_date)
                    .unwrap_or_else(|| "unknown".to_string());
                println!("{}  {}  {} matches", date_str, short_id, count);
            }
            println!("\nTotal: {} matches across {} sessions", total, counts.len());
        }
    }
    Ok(())
}

/// Output only matched text (--match-only, like grep -o).
fn output_match_only(cli: &Cli, results: &[SearchResult]) -> Result<()> {
    match cli.effective_output() {
        OutputFormat::Json => {
            let matches: Vec<&str> = results.iter().map(|r| r.matched_text.as_str()).collect();
            println!("{}", serde_json::to_string_pretty(&matches)?);
        }
        _ => {
            for result in results {
                println!("{}", result.matched_text);
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
                println!("{} ({}):{}: {}",
                    &result.session_id[..8.min(result.session_id.len())],
                    result.project,
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

            // Show appropriate message based on whether limit was applied
            if !args.no_limit && total_matches >= args.limit {
                println!(
                    "Showing {} matches (limit: {}, use --no-limit for all):",
                    total_matches, args.limit
                );
            } else {
                println!("Found {} matches:", total_matches);
            }
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
                let phase_info = match (&result.phase, &result.minutes_in, &result.post_compaction) {
                    (Some(phase), Some(mins), Some(post_c)) => {
                        let compact_marker = if *post_c { " post-compact" } else { "" };
                        format!(" ({phase}, {mins}m in{compact_marker})")
                    }
                    (Some(phase), Some(mins), _) => format!(" ({phase}, {mins}m in)"),
                    (Some(phase), _, _) => format!(" ({phase})"),
                    _ => String::new(),
                };
                let uuid_suffix = if args.show_uuid && !result.uuid.is_empty() {
                    format!(" [{}]", &result.uuid[..result.uuid.len().min(12)])
                } else {
                    String::new()
                };
                println!("  [{}]{}{}", format_match_label(&result.entry_type, &result.location), phase_info, uuid_suffix);

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

/// Precomputed session phase context for classifying entries.
struct PhaseContext {
    session_start: Option<DateTime<Utc>>,
    session_end: Option<DateTime<Utc>>,
    compaction_times: Vec<DateTime<Utc>>,
}

impl PhaseContext {
    fn from_entries(entries: &[LogEntry]) -> Self {
        let mut session_start: Option<DateTime<Utc>> = None;
        let mut session_end: Option<DateTime<Utc>> = None;
        let mut compaction_times = Vec::new();

        for entry in entries {
            if let Some(ts) = entry.timestamp() {
                if session_start.is_none() || ts < session_start.unwrap() {
                    session_start = Some(ts);
                }
                if session_end.is_none() || ts > session_end.unwrap() {
                    session_end = Some(ts);
                }
            }

            // Detect compaction boundaries
            if let LogEntry::System(sys) = entry {
                if sys.subtype == Some(crate::model::SystemSubtype::CompactBoundary) {
                    if let Some(ts) = entry.timestamp() {
                        compaction_times.push(ts);
                    }
                }
            }
        }

        compaction_times.sort();

        Self {
            session_start,
            session_end,
            compaction_times,
        }
    }

    /// Classify an entry into phase, minutes_in, and post_compaction.
    fn classify(&self, entry: &LogEntry) -> (SessionPhase, Option<u64>, bool) {
        let ts = entry.timestamp();

        let phase = match (ts, self.session_start, self.session_end) {
            (Some(ts), Some(start), Some(end)) => {
                let total = (end - start).num_seconds().max(1) as f64;
                let elapsed = (ts - start).num_seconds().max(0) as f64;
                let position = elapsed / total;
                if position < 0.33 {
                    SessionPhase::Early
                } else if position < 0.67 {
                    SessionPhase::Middle
                } else {
                    SessionPhase::Late
                }
            }
            _ => SessionPhase::Middle,
        };

        let minutes_in = match (ts, self.session_start) {
            (Some(ts), Some(start)) => {
                let mins = (ts - start).num_minutes().max(0) as u64;
                Some(mins)
            }
            _ => None,
        };

        let post_compaction = match ts {
            Some(ts) => self.compaction_times.iter().any(|ct| ts > *ct),
            None => false,
        };

        (phase, minutes_in, post_compaction)
    }
}

/// Session phase position.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
enum SessionPhase {
    Early,
    Middle,
    Late,
}

impl std::fmt::Display for SessionPhase {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SessionPhase::Early => write!(f, "early"),
            SessionPhase::Middle => write!(f, "middle"),
            SessionPhase::Late => write!(f, "late"),
        }
    }
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
    /// Session phase (early/middle/late).
    #[serde(skip_serializing_if = "Option::is_none")]
    phase: Option<SessionPhase>,
    /// Minutes into session when this match occurred.
    #[serde(skip_serializing_if = "Option::is_none")]
    minutes_in: Option<u64>,
    /// Whether this match is after a compaction event.
    #[serde(skip_serializing_if = "Option::is_none")]
    post_compaction: Option<bool>,
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
    // Safety: search_entry is only called from the single-pattern path
    let matcher = if args.fuzzy {
        Matcher::Fuzzy {
            pattern: &args.pattern[0],
            ignore_case: args.ignore_case,
            threshold: args.fuzzy_threshold,
        }
    } else {
        Matcher::Regex(regex)
    };

    let mut matches = Vec::new();

    match entry {
        LogEntry::User(user) if !args.thinking_only => {
            // Search user content (skip when --thinking-only is set)
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
                    ContentBlock::Text(text) if !args.thinking_only => {
                        // Skip assistant text when --thinking-only is set
                        if matcher.is_match(&text.text) {
                            matches.extend(matcher.find_matches_in(&text.text, "assistant text", args.context));
                        }
                    }
                    ContentBlock::Thinking(thinking) if args.thinking || args.thinking_only || args.all => {
                        if matcher.is_match(&thinking.thinking) {
                            matches.extend(matcher.find_matches_in(&thinking.thinking, "thinking", args.context));
                        }
                    }
                    ContentBlock::ToolUse(tool) if !args.thinking_only && (args.tools || args.all) => {
                        let input_str = serde_json::to_string(&tool.input).unwrap_or_default();
                        if matcher.is_match(&input_str) {
                            matches.extend(matcher.find_matches_in(&input_str, &format!("tool:{}", tool.name), args.context));
                        }
                    }
                    ContentBlock::ToolResult(result) if !args.thinking_only && (args.tools || args.all) => {
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
        LogEntry::System(system) if !args.thinking_only => {
            if let Some(content) = &system.content {
                if matcher.is_match(content) {
                    matches.extend(matcher.find_matches_in(content, "system", args.context));
                }
            }
        }
        LogEntry::Summary(summary) if !args.thinking_only => {
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
