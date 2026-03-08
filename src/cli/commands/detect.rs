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

use chrono::{DateTime, Utc};
use indicatif::{ProgressBar, ProgressStyle};
use regex::Regex;

use crate::cli::{Cli, DetectArgs};
use crate::decisions::{load_decisions, save_decisions};
use crate::error::{Result, SnatchError};

use super::helpers::{
    self, extract_text, has_options_pattern, has_tool_calls, is_affirmative, is_interrogative,
    short_id, truncate, SessionCollectParams,
};

/// A candidate decision point detected in a conversation.
#[derive(Debug)]
struct CandidateDecision {
    timestamp: DateTime<Utc>,
    session_id: String,
    short_id: String,
    question: String,
    response: String,
    confirmation: Option<String>,
    detection_method: DetectionMethod,
    confidence: f64,
    entry_uuid: String,
}

#[derive(Debug, Clone)]
enum DetectionMethod {
    Structural,
    ExplicitMarker(String),
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

/// Explicit decision marker patterns.
fn find_explicit_markers(text: &str) -> Vec<String> {
    let mut markers = Vec::new();
    let lower = text.to_lowercase();

    let patterns: &[(&str, &str)] = &[
        (r"DEF-\d+", "DEF-marker"),
        (r"(?i)design decision", "design-decision"),
        (r"(?i)we decided", "we-decided"),
        (r"(?i)the decision is", "the-decision-is"),
        (r"(?i)(?:we|they|team) decided to", "decided-to"),
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

    if lower.contains("after discussion") || lower.contains("after considering") {
        markers.push("after-deliberation".to_string());
    }

    markers
}

/// Extract the first prose sentence containing a decision keyword from text.
/// Skips markdown formatting lines (headers, tables, rules, code fences).
fn extract_decision_sentence(text: &str) -> Option<String> {
    let keywords = [
        "decided to", "design decision", "we decided", "the decision is",
        "agreed to", "agreed that", "agreed on", "final decision",
    ];
    for line in text.lines() {
        let trimmed = line.trim();
        // Skip markdown formatting
        if trimmed.starts_with('#')
            || trimmed.starts_with('|')
            || trimmed.starts_with("---")
            || trimmed.starts_with("```")
            || trimmed.starts_with("**") && trimmed.ends_with("**")
        {
            continue;
        }
        let lower = trimmed.to_lowercase();
        if keywords.iter().any(|k| lower.contains(k)) {
            if trimmed.len() > 10 {
                return Some(truncate(trimmed, 120));
            }
        }
    }
    None
}

/// Extract the first non-markdown prose line from text (for title fallback).
fn extract_first_prose_line(text: &str) -> Option<String> {
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        // Skip markdown formatting
        if trimmed.starts_with('#')
            || trimmed.starts_with('|')
            || trimmed.starts_with("---")
            || trimmed.starts_with("```")
            || (trimmed.starts_with("**") && trimmed.ends_with("**"))
            || trimmed.starts_with("- ")
            || trimmed.starts_with("* ")
        {
            continue;
        }
        if trimmed.len() > 10 {
            return Some(truncate(trimmed, 120));
        }
    }
    None
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

/// Run the detect command.
pub fn run(cli: &Cli, args: &DetectArgs) -> Result<()> {
    let sessions = helpers::collect_sessions(cli, &SessionCollectParams {
        session: args.session.as_deref(),
        project: args.project.as_deref(),
        since: args.since.as_deref(),
        until: args.until.as_deref(),
        recent: args.recent,
        no_subagents: args.no_subagents,
    })?;

    let topic_regex = if let Some(ref topic) = args.topic {
        Some(Regex::new(topic).map_err(|e| SnatchError::InvalidArgument {
            name: "topic".into(),
            reason: format!("Invalid topic regex: {e}"),
        })?)
    } else {
        None
    };

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

        let main_entries = helpers::main_thread_entries(&entries);
        let sid = short_id(session.session_id()).to_string();

        for i in 0..main_entries.len() {
            let entry = main_entries[i];

            // === Check for explicit markers in any entry ===
            if let Some(text) = extract_text(entry) {
                let explicit = find_explicit_markers(&text);
                for marker in &explicit {
                    let timestamp = entry.timestamp().unwrap_or_else(Utc::now);
                    let confidence = 0.85;
                    if confidence >= min_confidence {
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

                        // Skip empty or continuation/notification entries
                        if question.trim().is_empty() && response.trim().is_empty() {
                            continue;
                        }
                        let q_lower = question.to_lowercase();
                        if q_lower.starts_with("this session is being continued")
                            || q_lower.starts_with("<task-notification")
                            || q_lower.starts_with("<system-reminder")
                        {
                            continue;
                        }

                        candidates.push(CandidateDecision {
                            timestamp,
                            session_id: session.session_id().to_string(),
                            short_id: sid.clone(),
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
                                short_id: sid.clone(),
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

            let mut confidence: f64 = 0.5;
            if confirmation.is_some() {
                confidence += 0.25;
            }
            let impl_idx = if confirmation.is_some() { i + 3 } else { i + 2 };
            if impl_idx < main_entries.len() && has_tool_calls(main_entries[impl_idx]) {
                confidence += 0.1;
            }
            if assistant_text.len() > 500 {
                confidence += 0.05;
            }

            confidence = confidence.min(1.0);

            if confidence >= min_confidence {
                let timestamp = entry.timestamp().unwrap_or_else(Utc::now);
                candidates.push(CandidateDecision {
                    timestamp,
                    session_id: session.session_id().to_string(),
                    short_id: sid.clone(),
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

    // Filter by topic if specified
    if let Some(ref topic_re) = topic_regex {
        candidates.retain(|c| {
            topic_re.is_match(&c.question) || topic_re.is_match(&c.response)
        });
    }

    candidates.sort_by(|a, b| {
        b.confidence
            .partial_cmp(&a.confidence)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.timestamp.cmp(&b.timestamp))
    });

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

    // Register confirmed candidates to the decision registry
    if args.register || args.dry_run {
        let project_filter = args.project.as_deref().ok_or_else(|| SnatchError::InvalidArgument {
            name: "project".into(),
            reason: "--project is required with --register".into(),
        })?;

        let project = super::helpers::resolve_single_project(cli, project_filter)?;

        let project_dir = project.path();
        let mut store = load_decisions(project_dir)?;
        let mut registered = 0u32;

        // Only register confirmed structural decisions and explicit markers
        for c in &candidates {
            let should_register = match &c.detection_method {
                DetectionMethod::Structural => c.confirmation.is_some(),
                DetectionMethod::ExplicitMarker(_) => true,
                DetectionMethod::Reversal(_) => false,
            };
            if !should_register {
                continue;
            }

            // Skip candidates with continuation/notification prompts
            let q_lower = c.question.to_lowercase();
            if q_lower.starts_with("this session is being continued")
                || q_lower.starts_with("<task-notification")
                || q_lower.starts_with("<system-reminder")
                || c.question.trim().is_empty()
            {
                continue;
            }

            // Build title: extract a decision sentence from the assistant response,
            // falling back to the first prose line, then the user question.
            let title = extract_decision_sentence(&c.response)
                .or_else(|| extract_first_prose_line(&c.response))
                .unwrap_or_else(|| truncate(&c.question, 120))
            .trim_end_matches("...")
            .trim()
            .to_string();
            if title.is_empty() {
                continue;
            }

            if args.dry_run {
                eprintln!("  [dry-run] \"{}\"", title);
                eprintln!("            session: {} | confidence: {:.0}%", c.short_id, c.confidence * 100.0);
                eprintln!();
            } else {
                store.add_decision(
                    title,
                    Some(truncate(&c.response, 500)),
                    Some(c.session_id.clone()),
                    Some(c.confidence),
                    vec![],
                );
            }
            registered += 1;
        }

        if !args.dry_run {
            save_decisions(project_dir, &store)?;
        }

        if !cli.quiet {
            if args.dry_run {
                eprintln!("Would register {registered} decision(s) (dry run).");
            } else {
                eprintln!("Registered {registered} decision(s) to the registry.");
            }
        }
    }

    match cli.effective_output() {
        crate::cli::OutputFormat::Json => output_json(&candidates),
        _ => output_text(cli, &candidates),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_find_explicit_markers() {
        assert!(!find_explicit_markers("DEF-001: No Drop trait").is_empty());
        assert!(!find_explicit_markers("We decided to use Rust").is_empty());
        assert!(!find_explicit_markers("The design decision is clear").is_empty());
        assert!(find_explicit_markers("Just a regular sentence").is_empty());
    }

    #[test]
    fn test_find_reversal_markers() {
        assert!(!find_reversal_markers("Actually, let's go with option B").is_empty());
        assert!(!find_reversal_markers("I changed my mind about this").is_empty());
        assert!(!find_reversal_markers("On second thought, use enums").is_empty());
        assert!(find_reversal_markers("This is a regular message").is_empty());
    }
}
