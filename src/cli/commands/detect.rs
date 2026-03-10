//! Decision detection heuristic implementation.
//!
//! Detects candidate decision points in conversations using the shared
//! analysis::decision_detection module. Supports registration to the
//! decision registry.

use std::io::IsTerminal;

use indicatif::{ProgressBar, ProgressStyle};
use regex::Regex;

use crate::analysis::decision_detection::{
    detect_decisions, extract_decision_sentence, extract_first_prose_line,
    CandidateDecision, DetectParams,
};
use crate::cli::{Cli, DetectArgs};
use crate::decisions::{load_decisions, save_decisions};
use crate::error::{Result, SnatchError};

use super::helpers::{self, truncate, SessionCollectParams};

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
    if show_progress {
        let pb = ProgressBar::new(session_count as u64);
        pb.set_style(
            ProgressStyle::default_bar()
                .template("{spinner:.cyan} [{bar:40.cyan/dim}] {pos}/{len} sessions")
                .unwrap()
                .progress_chars("█▓░"),
        );
        pb.finish_and_clear();
    }

    let limit = if args.no_limit { usize::MAX } else { args.limit };

    let params = DetectParams {
        min_confidence: args.min_confidence,
        limit,
        topic_filter: topic_regex,
    };

    let result = detect_decisions(&sessions, &params, cli.max_file_size);
    let candidates = result.candidates;

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

        for c in &candidates {
            let should_register = match &c.detection_method {
                crate::analysis::decision_detection::DetectionMethod::Structural => {
                    c.confirmation.is_some()
                }
                crate::analysis::decision_detection::DetectionMethod::ExplicitMarker(_) => true,
                crate::analysis::decision_detection::DetectionMethod::Reversal(_) => false,
            };
            if !should_register {
                continue;
            }

            if c.response.trim().is_empty() {
                continue;
            }
            let q_lower = c.question.to_lowercase();
            if q_lower.starts_with("this session is being continued")
                || q_lower.starts_with("in the last session")
                || q_lower.starts_with("<task-notification")
                || q_lower.starts_with("<system-reminder")
                || c.question.trim().is_empty()
            {
                continue;
            }

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
                eprintln!(
                    "            session: {} | confidence: {:.0}%",
                    c.short_id,
                    c.confidence * 100.0
                );
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
            crate::analysis::decision_detection::DetectionMethod::Structural => "?->!",
            crate::analysis::decision_detection::DetectionMethod::ExplicitMarker(_) => "DEF",
            crate::analysis::decision_detection::DetectionMethod::Reversal(_) => "REV",
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
