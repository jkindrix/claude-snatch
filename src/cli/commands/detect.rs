//! Decision detection heuristic implementation.
//!
//! Detects candidate decision points in conversations by analyzing
//! structural patterns in message exchanges:
//! 1. User asks a question (interrogative prompt)
//! 2. Assistant responds with options/analysis (enumeration patterns)
//! 3. User confirms/chooses (short affirmative response)
//! 4. Optionally: assistant implements (tool calls follow)
//!
//! Also detects explicit decision markers and reversal patterns.

use std::io::IsTerminal;
use std::time::SystemTime;

use chrono::{DateTime, Utc};
use indicatif::{ProgressBar, ProgressStyle};
use regex::Regex;

use crate::cli::{Cli, DetectArgs};
use crate::discovery::Session;
use crate::error::{Result, SnatchError};
use crate::model::{ContentBlock, LogEntry};

use super::get_claude_dir;

/// A candidate decision point detected in a conversation.
#[derive(Debug)]
struct CandidateDecision {
    /// Timestamp of the decision point.
    timestamp: DateTime<Utc>,
    /// Session ID.
    session_id: String,
    /// Short session ID (first 8 chars).
    short_id: String,
    /// The user's question/prompt that initiated the decision.
    question: String,
    /// The assistant's response (options/analysis).
    response: String,
    /// The user's confirmation/choice (if structural pattern).
    confirmation: Option<String>,
    /// Detection method that found this.
    detection_method: DetectionMethod,
    /// Confidence score (0.0 - 1.0).
    confidence: f64,
    /// UUID of the key entry.
    entry_uuid: String,
}

#[derive(Debug, Clone)]
enum DetectionMethod {
    /// Structural: question → options → confirmation
    Structural,
    /// Explicit marker: "DEF-\d+", "design decision", "we decided", etc.
    ExplicitMarker(String),
    /// Reversal: "changed my mind", "scratch that", "actually", etc.
    Reversal(String),
}

impl std::fmt::Display for DetectionMethod {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DetectionMethod::Structural => write!(f, "structural"),
            DetectionMethod::ExplicitMarker(marker) => write!(f, "explicit ({})", marker),
            DetectionMethod::Reversal(marker) => write!(f, "reversal ({})", marker),
        }
    }
}

/// Extract visible text from an entry.
fn extract_text(entry: &LogEntry) -> Option<String> {
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

/// Check if an assistant entry contains tool use calls.
fn has_tool_calls(entry: &LogEntry) -> bool {
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

/// Check if text looks like a question (interrogative).
fn is_interrogative(text: &str) -> bool {
    let trimmed = text.trim();

    // Contains question marks
    if trimmed.contains('?') {
        return true;
    }

    // Starts with question words (case-insensitive)
    let lower = trimmed.to_lowercase();
    let question_starters = [
        "what ", "how ", "should ", "can ", "could ", "would ", "will ",
        "is ", "are ", "do ", "does ", "which ", "where ", "when ", "why ",
        "shall ", "have you ", "did ",
    ];

    question_starters.iter().any(|q| lower.starts_with(q))
}

/// Check if assistant response contains enumeration/options patterns.
fn has_options_pattern(text: &str) -> bool {
    let lower = text.to_lowercase();

    // Numbered lists: "1.", "2.", "3." etc.
    let numbered = Regex::new(r"(?m)^\s*\d+[\.\)]\s+").unwrap();
    let numbered_count = numbered.find_iter(text).count();
    if numbered_count >= 2 {
        return true;
    }

    // Option A/B or approach 1/2
    if lower.contains("option a") && lower.contains("option b")
        || lower.contains("approach 1") && lower.contains("approach 2")
        || lower.contains("option 1") && lower.contains("option 2")
    {
        return true;
    }

    // Pros/cons patterns
    if lower.contains("pros:") && lower.contains("cons:")
        || lower.contains("advantages") && lower.contains("disadvantages")
        || lower.contains("trade-off") || lower.contains("tradeoff")
    {
        return true;
    }

    // Bullet lists with enough items
    let bullets = Regex::new(r"(?m)^\s*[-*]\s+").unwrap();
    if bullets.find_iter(text).count() >= 3 {
        // Only count as options if also contains comparison-like language
        if lower.contains("alternatively")
            || lower.contains("or we could")
            || lower.contains("another approach")
            || lower.contains("we could also")
            || lower.contains("versus")
            || lower.contains(" vs ")
        {
            return true;
        }
    }

    false
}

/// Check if user response is a short affirmative (decision confirmation).
fn is_affirmative(text: &str) -> bool {
    let trimmed = text.trim();
    let lower = trimmed.to_lowercase();

    // Very short responses are more likely confirmations
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
        "use ", "i choose", "i pick",
    ];
    if choice_patterns.iter().any(|p| lower.starts_with(p)) {
        return true;
    }

    // Short responses (under 30 words) that aren't questions
    if word_count <= 30 && !trimmed.contains('?') {
        // Contains positive language
        if lower.contains("agree") || lower.contains("go with")
            || lower.contains("let's") || lower.contains("sounds")
            || lower.contains("perfect") || lower.contains("great")
        {
            return true;
        }
    }

    false
}

/// Explicit decision marker patterns.
fn find_explicit_markers(text: &str) -> Vec<String> {
    let mut markers = Vec::new();
    let lower = text.to_lowercase();

    let patterns: &[(&str, &str)] = &[
        (r"DEF-\d+", "DEF-marker"),
        (r"(?i)design decision", "design-decision"),
        (r"(?i)we decided", "we-decided"),
        (r"(?i)the decision is", "the-decision-is"),
        (r"(?i)decided to", "decided-to"),
        (r"(?i)final decision", "final-decision"),
        (r"(?i)agreed (?:to|that|on)", "agreed-to"),
    ];

    for (pattern, label) in patterns {
        if let Ok(re) = Regex::new(pattern) {
            if re.is_match(text) {
                markers.push(label.to_string());
            }
        }
    }

    // Additional context-free markers
    if lower.contains("after discussion") || lower.contains("after considering") {
        markers.push("after-deliberation".to_string());
    }

    markers
}

/// Reversal pattern detection.
fn find_reversal_markers(text: &str) -> Vec<String> {
    let mut markers = Vec::new();
    let lower = text.to_lowercase();

    let reversal_phrases = [
        ("changed my mind", "changed-mind"),
        ("scratch that", "scratch-that"),
        ("actually, let", "actually-let"),
        ("actually let's", "actually-lets"),
        ("let's go back to", "go-back-to"),
        ("on second thought", "second-thought"),
        ("i take that back", "take-back"),
        ("never mind", "never-mind"),
        ("nevermind", "nevermind"),
        ("reverse that", "reverse"),
        ("undo that", "undo"),
        ("instead, let", "instead-let"),
        ("wait, ", "wait"),
    ];

    for (phrase, label) in &reversal_phrases {
        if lower.contains(phrase) {
            markers.push(label.to_string());
        }
    }

    markers
}

/// Collect sessions matching detect args.
fn collect_sessions(cli: &Cli, args: &DetectArgs) -> Result<Vec<Session>> {
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
        let mut sess = Vec::new();
        for project in projects {
            if project.decoded_path().contains(project_filter) {
                sess.extend(project.sessions()?);
            }
        }
        sess
    } else {
        claude_dir.all_sessions()?
    };

    // Date filters
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

    if let Some(n) = args.recent {
        sessions.sort_by(|a, b| b.modified_time().cmp(&a.modified_time()));
        sessions.truncate(n);
    }

    if args.no_subagents {
        sessions.retain(|s| !s.is_subagent());
    }

    Ok(sessions)
}

/// Truncate text for display.
fn truncate(text: &str, max_chars: usize) -> String {
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

/// Run the detect command.
pub fn run(cli: &Cli, args: &DetectArgs) -> Result<()> {
    let sessions = collect_sessions(cli, args)?;

    let session_count = sessions.len();
    let show_progress = session_count > 10 && std::io::stderr().is_terminal() && !cli.quiet;
    let progress = if show_progress {
        let pb = ProgressBar::new(session_count as u64);
        pb.set_style(
            ProgressStyle::default_bar()
                .template("{spinner:.cyan} [{bar:40.cyan/dim}] {pos}/{len} sessions")
                .unwrap()
                .progress_chars("█▓░"),
        );
        Some(pb)
    } else {
        None
    };

    let min_confidence = args.min_confidence;
    let mut candidates: Vec<CandidateDecision> = Vec::new();

    for session in &sessions {
        if let Some(ref pb) = progress {
            pb.inc(1);
        }

        let entries = match session.parse_with_options(cli.max_file_size) {
            Ok(e) => e,
            Err(_) => continue,
        };

        // Filter to main thread
        let main_entries: Vec<&LogEntry> = entries
            .iter()
            .filter(|e| !e.is_sidechain())
            .filter(|e| matches!(e, LogEntry::User(_) | LogEntry::Assistant(_)))
            .collect();

        let short_id = if session.session_id().len() >= 8 {
            session.session_id()[..8].to_string()
        } else {
            session.session_id().to_string()
        };

        // Scan for structural pattern: user question → assistant options → user confirmation
        for i in 0..main_entries.len() {
            let entry = main_entries[i];

            // === Check for explicit markers in any entry ===
            if let Some(text) = extract_text(entry) {
                let explicit = find_explicit_markers(&text);
                for marker in &explicit {
                    let timestamp = entry.timestamp().unwrap_or_else(Utc::now);
                    let confidence = 0.85;
                    if confidence >= min_confidence {
                        // Get surrounding context
                        let (question, response) = if entry.message_type() == "user" {
                            let resp = if i + 1 < main_entries.len() {
                                extract_text(main_entries[i + 1]).unwrap_or_default()
                            } else {
                                String::new()
                            };
                            (text.clone(), resp)
                        } else {
                            let q = if i > 0 {
                                extract_text(main_entries[i - 1]).unwrap_or_default()
                            } else {
                                String::new()
                            };
                            (q, text.clone())
                        };

                        candidates.push(CandidateDecision {
                            timestamp,
                            session_id: session.session_id().to_string(),
                            short_id: short_id.clone(),
                            question,
                            response,
                            confirmation: None,
                            detection_method: DetectionMethod::ExplicitMarker(marker.clone()),
                            confidence,
                            entry_uuid: entry.uuid().unwrap_or("").to_string(),
                        });
                    }
                }

                // === Check for reversal markers in user messages ===
                if entry.message_type() == "user" {
                    let reversals = find_reversal_markers(&text);
                    for marker in &reversals {
                        let timestamp = entry.timestamp().unwrap_or_else(Utc::now);
                        let confidence = 0.7;
                        if confidence >= min_confidence {
                            let response = if i + 1 < main_entries.len() {
                                extract_text(main_entries[i + 1]).unwrap_or_default()
                            } else {
                                String::new()
                            };

                            candidates.push(CandidateDecision {
                                timestamp,
                                session_id: session.session_id().to_string(),
                                short_id: short_id.clone(),
                                question: text.clone(),
                                response,
                                confirmation: None,
                                detection_method: DetectionMethod::Reversal(marker.clone()),
                                confidence,
                                entry_uuid: entry.uuid().unwrap_or("").to_string(),
                            });
                        }
                    }
                }
            }

            // === Structural pattern: question → options → confirmation ===
            if entry.message_type() != "user" {
                continue;
            }

            let user_text = match extract_text(entry) {
                Some(t) => t,
                None => continue,
            };

            if !is_interrogative(&user_text) {
                continue;
            }

            // Next entry should be assistant with options
            if i + 1 >= main_entries.len() {
                continue;
            }
            let next = main_entries[i + 1];
            if next.message_type() != "assistant" {
                continue;
            }

            let assistant_text = match extract_text(next) {
                Some(t) => t,
                None => continue,
            };

            if !has_options_pattern(&assistant_text) {
                continue;
            }

            // Look for user confirmation
            let confirmation = if i + 2 < main_entries.len() {
                let confirm_entry = main_entries[i + 2];
                if confirm_entry.message_type() == "user" {
                    extract_text(confirm_entry).filter(|t| is_affirmative(t))
                } else {
                    None
                }
            } else {
                None
            };

            // Score confidence
            let mut confidence: f64 = 0.5; // Base for question + options
            if confirmation.is_some() {
                confidence += 0.25; // User confirmed
            }
            // Check if implementation followed (tool calls after confirmation)
            let impl_idx = if confirmation.is_some() { i + 3 } else { i + 2 };
            if impl_idx < main_entries.len() && has_tool_calls(main_entries[impl_idx]) {
                confidence += 0.1; // Implementation followed
            }
            // Longer assistant response with more options = higher confidence
            if assistant_text.len() > 500 {
                confidence += 0.05;
            }

            confidence = confidence.min(1.0);

            if confidence >= min_confidence {
                let timestamp = entry.timestamp().unwrap_or_else(Utc::now);
                candidates.push(CandidateDecision {
                    timestamp,
                    session_id: session.session_id().to_string(),
                    short_id: short_id.clone(),
                    question: user_text,
                    response: assistant_text,
                    confirmation,
                    detection_method: DetectionMethod::Structural,
                    confidence,
                    entry_uuid: entry.uuid().unwrap_or("").to_string(),
                });
            }
        }
    }

    if let Some(pb) = progress {
        pb.finish_and_clear();
    }

    // Sort by confidence (descending), then by timestamp
    candidates.sort_by(|a, b| {
        b.confidence
            .partial_cmp(&a.confidence)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.timestamp.cmp(&b.timestamp))
    });

    // Apply limit
    let limit = if args.no_limit {
        candidates.len()
    } else {
        args.limit.min(candidates.len())
    };
    candidates.truncate(limit);

    if candidates.is_empty() {
        if !cli.quiet {
            println!("No candidate decisions detected.");
        }
        return Ok(());
    }

    match cli.effective_output() {
        crate::cli::OutputFormat::Json => {
            output_json(&candidates);
        }
        _ => {
            output_text(cli, &candidates);
        }
    }

    Ok(())
}

fn output_json(candidates: &[CandidateDecision]) {
    let entries: Vec<serde_json::Value> = candidates
        .iter()
        .map(|c| {
            let mut obj = serde_json::json!({
                "timestamp": c.timestamp.to_rfc3339(),
                "session_id": c.session_id,
                "entry_uuid": c.entry_uuid,
                "detection_method": format!("{}", c.detection_method),
                "confidence": c.confidence,
                "question": c.question,
                "response": c.response,
            });
            if let Some(ref conf) = c.confirmation {
                obj["confirmation"] = serde_json::Value::String(conf.clone());
            }
            obj
        })
        .collect();

    println!("{}", serde_json::to_string_pretty(&entries).unwrap_or_default());
}

fn output_text(cli: &Cli, candidates: &[CandidateDecision]) {
    if !cli.quiet {
        println!(
            "Detected {} candidate decision{}:\n",
            candidates.len(),
            if candidates.len() == 1 { "" } else { "s" }
        );
    }

    for (i, candidate) in candidates.iter().enumerate() {
        let date = candidate.timestamp.format("%Y-%m-%d %H:%M");
        let conf_pct = (candidate.confidence * 100.0) as u32;

        let method_icon = match &candidate.detection_method {
            DetectionMethod::Structural => "?->!",
            DetectionMethod::ExplicitMarker(_) => "DEF",
            DetectionMethod::Reversal(_) => "REV",
        };

        println!(
            "  [{:>3}%] [{}] {} | {} | {}",
            conf_pct, method_icon, date, candidate.short_id, candidate.detection_method
        );

        println!();
        println!("    Q: {}", truncate(&candidate.question, 200));
        println!();
        println!("    A: {}", truncate(&candidate.response, 300));

        if let Some(ref conf) = candidate.confirmation {
            println!();
            println!("    CONFIRMED: {}", truncate(conf, 150));
        }

        if i < candidates.len() - 1 {
            println!();
            println!("  ─────────────────────────────────────────");
        }
    }

    println!();
}
