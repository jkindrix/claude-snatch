//! Decision point detection heuristic.
//!
//! Detects candidate decision points in conversations by analyzing:
//! 1. Structural patterns: question → options → confirmation
//! 2. Explicit markers: "DEF-\d+", "we decided", "design decision"
//! 3. Reversal patterns: "changed my mind", "scratch that"
//!
//! Used by both CLI `detect` and MCP `detect_decisions` tools.

use chrono::{DateTime, Utc};
use regex::Regex;
use serde::Serialize;

use crate::cli::helpers::{
    extract_text, has_options_pattern, has_tool_calls, is_affirmative, is_interrogative,
    main_thread_entries, short_id, truncate,
};
use crate::discovery::Session;

/// Parameters for decision detection.
pub struct DetectParams {
    /// Minimum confidence threshold (0.0-1.0).
    pub min_confidence: f64,
    /// Maximum candidates to return.
    pub limit: usize,
    /// Topic filter regex (applied after detection).
    pub topic_filter: Option<Regex>,
}

impl Default for DetectParams {
    fn default() -> Self {
        Self {
            min_confidence: 0.5,
            limit: 50,
            topic_filter: None,
        }
    }
}

/// How a decision was detected.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DetectionMethod {
    /// Question → options → confirmation pattern.
    Structural,
    /// Explicit marker like "DEF-001" or "we decided".
    ExplicitMarker(String),
    /// Reversal pattern like "changed my mind".
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

/// A candidate decision point detected in a conversation.
#[derive(Debug, Serialize)]
#[allow(missing_docs)]
pub struct CandidateDecision {
    pub timestamp: DateTime<Utc>,
    pub session_id: String,
    pub short_id: String,
    pub question: String,
    pub response: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub confirmation: Option<String>,
    pub detection_method: DetectionMethod,
    pub confidence: f64,
    pub entry_uuid: String,
}

/// Result of decision detection.
pub struct DetectResult {
    /// Detected candidate decisions, sorted by confidence (desc) then timestamp.
    pub candidates: Vec<CandidateDecision>,
}

/// Explicit decision marker patterns.
pub fn find_explicit_markers(text: &str) -> Vec<String> {
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

/// Reversal pattern detection.
pub fn find_reversal_markers(text: &str) -> Vec<String> {
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

/// Extract the first prose sentence containing a decision keyword.
pub fn extract_decision_sentence(text: &str) -> Option<String> {
    let keywords = [
        "decided to", "design decision", "we decided", "the decision is",
        "agreed to", "agreed that", "agreed on", "final decision",
    ];
    for line in text.lines() {
        let trimmed = line.trim();
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

/// Extract the first non-markdown prose line (title fallback).
pub fn extract_first_prose_line(text: &str) -> Option<String> {
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
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

/// Run decision detection across a set of sessions.
pub fn detect_decisions(
    sessions: &[Session],
    params: &DetectParams,
    max_file_size: Option<u64>,
) -> DetectResult {
    let mut candidates: Vec<CandidateDecision> = Vec::new();

    for session in sessions {
        let entries = match session.parse_with_options(max_file_size) {
            Ok(e) => e,
            Err(_) => continue,
        };

        let main = main_thread_entries(&entries);
        let sid = short_id(session.session_id()).to_string();

        for i in 0..main.len() {
            let entry = main[i];

            // === Explicit markers in any entry ===
            if let Some(text) = extract_text(entry) {
                let explicit = find_explicit_markers(&text);
                for marker in &explicit {
                    let timestamp = entry.timestamp().unwrap_or_else(Utc::now);
                    let confidence = 0.85;
                    if confidence >= params.min_confidence {
                        let (question, response) = if entry.message_type() == "user" {
                            let resp = if i + 1 < main.len() {
                                extract_text(main[i + 1]).unwrap_or_default()
                            } else {
                                String::new()
                            };
                            (text.clone(), resp)
                        } else {
                            let q = if i > 0 {
                                extract_text(main[i - 1]).unwrap_or_default()
                            } else {
                                String::new()
                            };
                            (q, text.clone())
                        };

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

                // === Reversal markers in user messages ===
                if entry.message_type() == "user" {
                    let reversals = find_reversal_markers(&text);
                    for marker in &reversals {
                        let timestamp = entry.timestamp().unwrap_or_else(Utc::now);
                        let confidence = 0.7;
                        if confidence >= params.min_confidence {
                            let response = if i + 1 < main.len() {
                                extract_text(main[i + 1]).unwrap_or_default()
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

            if i + 1 >= main.len() {
                continue;
            }
            let next = main[i + 1];
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

            let confirmation = if i + 2 < main.len() {
                let confirm_entry = main[i + 2];
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
            if impl_idx < main.len() && has_tool_calls(main[impl_idx]) {
                confidence += 0.1;
            }
            if assistant_text.len() > 500 {
                confidence += 0.05;
            }
            confidence = confidence.min(1.0);

            if confidence >= params.min_confidence {
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

    // Filter by topic
    if let Some(ref topic_re) = params.topic_filter {
        candidates.retain(|c| {
            topic_re.is_match(&c.question) || topic_re.is_match(&c.response)
        });
    }

    // Sort by confidence desc, then timestamp
    candidates.sort_by(|a, b| {
        b.confidence
            .partial_cmp(&a.confidence)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.timestamp.cmp(&b.timestamp))
    });

    candidates.truncate(params.limit);

    DetectResult { candidates }
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
