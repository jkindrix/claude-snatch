//! Contradiction detection implementation.
//!
//! Finds potentially conflicting decisions across sessions by:
//! 1. Registry-based: comparing decisions that share tags/topics
//! 2. Search-based: finding opposing language about the same topic across sessions

use std::io::IsTerminal;

use chrono::{DateTime, Utc};
use indicatif::{ProgressBar, ProgressStyle};
use regex::{Regex, RegexBuilder};

use crate::cli::{Cli, ConflictsArgs};
use crate::decisions::{self, DecisionStatus, DecisionStore};
use crate::error::{Result, SnatchError};

use super::helpers::{
    self, extract_text, short_id, truncate, SessionCollectParams,
};

/// A pair of potentially conflicting statements.
#[derive(Debug)]
struct ConflictPair {
    earlier_time: DateTime<Utc>,
    earlier_session: String,
    earlier_text: String,
    later_time: DateTime<Utc>,
    later_session: String,
    later_text: String,
    detection: ConflictDetection,
    confidence: f64,
    topic: String,
}

#[derive(Debug, Clone)]
enum ConflictDetection {
    Registry,
    OpposingLanguage,
    SupersedeChain,
}

impl std::fmt::Display for ConflictDetection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ConflictDetection::Registry => write!(f, "registry"),
            ConflictDetection::OpposingLanguage => write!(f, "opposing-language"),
            ConflictDetection::SupersedeChain => write!(f, "supersede-chain"),
        }
    }
}

/// A conclusion extracted from a session about a topic.
#[derive(Debug)]
struct TopicConclusion {
    session_id: String,
    timestamp: DateTime<Utc>,
    text: String,
}

/// Signal word pairs that indicate opposing positions.
const OPPOSING_PAIRS: &[(&[&str], &[&str])] = &[
    (&["will use", "should use", "let's use", "we'll use", "using"],
     &["won't use", "shouldn't use", "don't use", "not use", "without", "no "]),
    (&["add", "adding", "include", "including", "enable", "enabling"],
     &["remove", "removing", "exclude", "excluding", "disable", "disabling"]),
    (&["yes", "agree", "confirmed", "correct", "right"],
     &["no", "disagree", "rejected", "incorrect", "wrong"]),
    (&["keep", "keeping", "maintain", "retain"],
     &["drop", "dropping", "eliminate", "remove"]),
    (&["implement", "support", "allow"],
     &["skip", "forbid", "prevent", "disallow"]),
    (&["explicit", "manual", "opt-in"],
     &["implicit", "automatic", "opt-out"]),
];

/// Check if two texts contain opposing signal patterns.
fn find_opposing_signals(text_a: &str, text_b: &str) -> Option<(Vec<String>, f64)> {
    let lower_a = text_a.to_lowercase();
    let lower_b = text_b.to_lowercase();

    let mut found_a_positive = Vec::new();
    let mut found_b_negative = Vec::new();
    let mut found_a_negative = Vec::new();
    let mut found_b_positive = Vec::new();

    for (positive, negative) in OPPOSING_PAIRS {
        let a_has_pos = positive.iter().any(|p| lower_a.contains(p));
        let a_has_neg = negative.iter().any(|n| lower_a.contains(n));
        let b_has_pos = positive.iter().any(|p| lower_b.contains(p));
        let b_has_neg = negative.iter().any(|n| lower_b.contains(n));

        if a_has_pos && b_has_neg {
            for p in positive.iter().filter(|p| lower_a.contains(*p)) {
                found_a_positive.push(p.to_string());
            }
            for n in negative.iter().filter(|n| lower_b.contains(*n)) {
                found_b_negative.push(n.to_string());
            }
        }
        if a_has_neg && b_has_pos {
            for n in negative.iter().filter(|n| lower_a.contains(*n)) {
                found_a_negative.push(n.to_string());
            }
            for p in positive.iter().filter(|p| lower_b.contains(*p)) {
                found_b_positive.push(p.to_string());
            }
        }
    }

    let total_signals = found_a_positive.len() + found_b_negative.len()
        + found_a_negative.len() + found_b_positive.len();

    if total_signals == 0 {
        return None;
    }

    let mut all_signals = Vec::new();
    all_signals.extend(found_a_positive);
    all_signals.extend(found_b_negative);
    all_signals.extend(found_a_negative);
    all_signals.extend(found_b_positive);
    all_signals.dedup();

    let confidence = match total_signals {
        1..=2 => 0.4,
        3..=4 => 0.6,
        5..=6 => 0.75,
        _ => 0.85,
    };

    Some((all_signals, confidence))
}

/// Find the project directory for a given project filter.
fn find_project_dir(cli: &Cli, project_filter: &str) -> Result<Option<std::path::PathBuf>> {
    let claude_dir = super::get_claude_dir(cli.claude_dir.as_ref())?;
    let projects = claude_dir.projects()?;
    for project in projects {
        if project.decoded_path().contains(project_filter) {
            return Ok(Some(project.path().to_path_buf()));
        }
    }
    Ok(None)
}

/// Run the conflicts command.
pub fn run(cli: &Cli, args: &ConflictsArgs) -> Result<()> {
    let mut conflicts: Vec<ConflictPair> = Vec::new();

    // === Approach 1: Registry-based detection ===
    if let Some(ref project) = args.project {
        if let Some(project_dir) = find_project_dir(cli, project)? {
            let store = decisions::load_decisions(&project_dir)?;
            detect_registry_conflicts(&store, &args.topic, &mut conflicts);
        }
    }

    // === Approach 2: Search-based detection ===
    if let Some(ref topic) = args.topic {
        detect_search_conflicts(cli, args, topic, &mut conflicts)?;
    }

    conflicts.retain(|c| c.confidence >= args.min_confidence);

    conflicts.sort_by(|a, b| {
        b.confidence
            .partial_cmp(&a.confidence)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.earlier_time.cmp(&b.earlier_time))
    });

    let limit = if args.no_limit {
        conflicts.len()
    } else {
        args.limit.min(conflicts.len())
    };
    conflicts.truncate(limit);

    if conflicts.is_empty() {
        if !cli.quiet {
            if args.topic.is_some() {
                println!("No conflicts detected for the specified topic.");
            } else {
                println!("No conflicts detected in the decision registry.");
                if args.project.is_some() {
                    println!("Tip: use --topic <pattern> to search for opposing language across sessions.");
                }
            }
        }
        return Ok(());
    }

    match cli.effective_output() {
        crate::cli::OutputFormat::Json => output_json(&conflicts),
        _ => output_text(cli, &conflicts),
    }

    Ok(())
}

/// Detect conflicts from the decision registry.
fn detect_registry_conflicts(
    store: &DecisionStore,
    topic_filter: &Option<String>,
    conflicts: &mut Vec<ConflictPair>,
) {
    let decisions = &store.decisions;

    // Find supersede chains (explicit conflicts)
    for d in decisions {
        if d.status == DecisionStatus::Superseded {
            if let Some(new_id) = d.superseded_by {
                if let Some(new_d) = decisions.iter().find(|dd| dd.id == new_id) {
                    if let Some(ref topic) = topic_filter {
                        let topic_lower = topic.to_lowercase();
                        let matches = d.title.to_lowercase().contains(&topic_lower)
                            || d.tags.iter().any(|t| t.to_lowercase().contains(&topic_lower))
                            || new_d.title.to_lowercase().contains(&topic_lower)
                            || new_d.tags.iter().any(|t| t.to_lowercase().contains(&topic_lower));
                        if !matches {
                            continue;
                        }
                    }

                    conflicts.push(ConflictPair {
                        earlier_time: d.created_at,
                        earlier_session: d.session_id.clone().unwrap_or_default(),
                        earlier_text: format!("[{}] #{}: {}", d.status, d.id, d.title),
                        later_time: new_d.created_at,
                        later_session: new_d.session_id.clone().unwrap_or_default(),
                        later_text: format!("[{}] #{}: {}", new_d.status, new_d.id, new_d.title),
                        detection: ConflictDetection::SupersedeChain,
                        confidence: 0.95,
                        topic: d.tags.first().cloned().unwrap_or_else(|| d.title.clone()),
                    });
                }
            }
        }
    }

    // Find decisions with shared tags but different conclusions
    for (i, d1) in decisions.iter().enumerate() {
        if !d1.status.is_active() && d1.status != DecisionStatus::Superseded {
            continue;
        }
        for d2 in decisions.iter().skip(i + 1) {
            if !d2.status.is_active() && d2.status != DecisionStatus::Superseded {
                continue;
            }
            if d1.id == d2.id {
                continue;
            }

            let shared_tags: Vec<&String> = d1.tags.iter()
                .filter(|t| d2.tags.contains(t))
                .collect();

            if shared_tags.is_empty() {
                continue;
            }

            if let Some(ref topic) = topic_filter {
                let topic_lower = topic.to_lowercase();
                if !shared_tags.iter().any(|t| t.to_lowercase().contains(&topic_lower)) {
                    continue;
                }
            }

            let d1_text = format!("{} {}", d1.title,
                d1.description.as_deref().unwrap_or(""));
            let d2_text = format!("{} {}", d2.title,
                d2.description.as_deref().unwrap_or(""));

            if let Some((_, confidence)) = find_opposing_signals(&d1_text, &d2_text) {
                let (earlier, later) = if d1.created_at <= d2.created_at {
                    (d1, d2)
                } else {
                    (d2, d1)
                };

                conflicts.push(ConflictPair {
                    earlier_time: earlier.created_at,
                    earlier_session: earlier.session_id.clone().unwrap_or_default(),
                    earlier_text: format!("[{}] #{}: {}", earlier.status, earlier.id, earlier.title),
                    later_time: later.created_at,
                    later_session: later.session_id.clone().unwrap_or_default(),
                    later_text: format!("[{}] #{}: {}", later.status, later.id, later.title),
                    detection: ConflictDetection::Registry,
                    confidence,
                    topic: shared_tags.first().map(|s| s.to_string()).unwrap_or_default(),
                });
            }
        }
    }
}

/// Detect conflicts via search-based opposing language detection.
fn detect_search_conflicts(
    cli: &Cli,
    args: &ConflictsArgs,
    topic: &str,
    conflicts: &mut Vec<ConflictPair>,
) -> Result<()> {
    let regex = RegexBuilder::new(topic)
        .case_insensitive(true)
        .build()
        .map_err(|e| SnatchError::InvalidArgument {
            name: "topic".to_string(),
            reason: e.to_string(),
        })?;

    let sessions = helpers::collect_sessions(cli, &SessionCollectParams {
        session: None,
        project: args.project.as_deref(),
        since: args.since.as_deref(),
        until: args.until.as_deref(),
        recent: None,
        no_subagents: args.no_subagents,
    })?;

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

    let mut conclusions: Vec<TopicConclusion> = Vec::new();

    for session in &sessions {
        if let Some(ref pb) = progress {
            pb.inc(1);
        }

        let entries = match session.parse_with_options(cli.max_file_size) {
            Ok(e) => e,
            Err(_) => continue,
        };

        let main_entries = helpers::main_thread_entries(&entries);

        let mut last_matching_assistant: Option<(DateTime<Utc>, String)> = None;

        for entry in &main_entries {
            if entry.message_type() != "assistant" {
                continue;
            }
            if let Some(text) = extract_text(entry) {
                if regex.is_match(&text) {
                    let timestamp = entry.timestamp().unwrap_or_else(Utc::now);
                    let conclusion = extract_conclusion_around_match(&text, &regex);
                    last_matching_assistant = Some((timestamp, conclusion));
                }
            }
        }

        if let Some((timestamp, text)) = last_matching_assistant {
            conclusions.push(TopicConclusion {
                session_id: session.session_id().to_string(),
                timestamp,
                text,
            });
        }
    }

    if let Some(pb) = progress {
        pb.finish_and_clear();
    }

    for i in 0..conclusions.len() {
        for j in (i + 1)..conclusions.len() {
            let c1 = &conclusions[i];
            let c2 = &conclusions[j];

            if let Some((_, confidence)) = find_opposing_signals(&c1.text, &c2.text) {
                let (earlier, later) = if c1.timestamp <= c2.timestamp {
                    (c1, c2)
                } else {
                    (c2, c1)
                };

                conflicts.push(ConflictPair {
                    earlier_time: earlier.timestamp,
                    earlier_session: earlier.session_id.clone(),
                    earlier_text: earlier.text.clone(),
                    later_time: later.timestamp,
                    later_session: later.session_id.clone(),
                    later_text: later.text.clone(),
                    detection: ConflictDetection::OpposingLanguage,
                    confidence,
                    topic: topic.to_string(),
                });
            }
        }
    }

    Ok(())
}

/// Extract the paragraph/section around a regex match for context.
fn extract_conclusion_around_match(text: &str, regex: &Regex) -> String {
    if let Some(m) = regex.find(text) {
        let start = m.start();
        let end = m.end();

        let para_start = text[..start]
            .rfind("\n\n")
            .map(|i| i + 2)
            .unwrap_or(0);
        let para_end = text[end..]
            .find("\n\n")
            .map(|i| end + i)
            .unwrap_or(text.len());

        let paragraph = &text[para_start..para_end];
        truncate(paragraph, 500)
    } else {
        truncate(text, 500)
    }
}

fn output_json(conflicts: &[ConflictPair]) {
    let entries: Vec<serde_json::Value> = conflicts
        .iter()
        .map(|c| {
            serde_json::json!({
                "topic": c.topic,
                "detection": format!("{}", c.detection),
                "confidence": c.confidence,
                "earlier": {
                    "timestamp": c.earlier_time.to_rfc3339(),
                    "session_id": c.earlier_session,
                    "text": c.earlier_text,
                },
                "later": {
                    "timestamp": c.later_time.to_rfc3339(),
                    "session_id": c.later_session,
                    "text": c.later_text,
                }
            })
        })
        .collect();

    println!("{}", serde_json::to_string_pretty(&entries).unwrap_or_default());
}

fn output_text(cli: &Cli, conflicts: &[ConflictPair]) {
    if !cli.quiet {
        println!(
            "Detected {} potential conflict{}:\n",
            conflicts.len(),
            if conflicts.len() == 1 { "" } else { "s" }
        );
    }

    for (i, conflict) in conflicts.iter().enumerate() {
        let conf_pct = (conflict.confidence * 100.0) as u32;
        let earlier_date = conflict.earlier_time.format("%Y-%m-%d");
        let later_date = conflict.later_time.format("%Y-%m-%d");

        println!(
            "  [{:>3}%] {} | topic: {}",
            conf_pct, conflict.detection, conflict.topic
        );
        println!();
        println!(
            "    EARLIER ({} [{}]):",
            earlier_date, short_id(&conflict.earlier_session)
        );
        for line in truncate(&conflict.earlier_text, 300).lines() {
            println!("      {}", line);
        }
        println!();
        println!(
            "    LATER ({} [{}]):",
            later_date, short_id(&conflict.later_session)
        );
        for line in truncate(&conflict.later_text, 300).lines() {
            println!("      {}", line);
        }

        if i < conflicts.len() - 1 {
            println!();
            println!("  ─────────────────────────────────────────");
            println!();
        }
    }

    println!();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_find_opposing_signals_basic() {
        let (signals, confidence) = find_opposing_signals(
            "We will use traits for polymorphism",
            "We won't use traits, using enums instead",
        ).unwrap();
        assert!(!signals.is_empty());
        assert!(confidence > 0.0);
    }

    #[test]
    fn test_find_opposing_signals_no_conflict() {
        assert!(find_opposing_signals(
            "The weather is nice today",
            "I had lunch at noon",
        ).is_none());
    }

    #[test]
    fn test_find_opposing_signals_add_remove() {
        let result = find_opposing_signals(
            "We should add logging to this module",
            "Remove the logging, it's too noisy",
        );
        assert!(result.is_some());
    }

    #[test]
    fn test_extract_conclusion_unicode() {
        // Should not panic on multi-byte characters
        let text = "This is a test with em dash — and more text that goes on for quite a while to ensure we test truncation properly with unicode characters like café and résumé and naïve and other such words that contain multi-byte characters. We need enough text here to trigger the 500-character truncation limit so let me keep writing more text. The quick brown fox jumps over the lazy dog. Pack my box with five dozen liquor jugs. How vexingly quick daft zebras jump.";
        let regex = Regex::new("test").unwrap();
        let result = extract_conclusion_around_match(text, &regex);
        // Should not panic, should end with "..."
        assert!(result.len() > 0);
    }
}
