//! Prompts command implementation.
//!
//! Extract user prompts from Claude Code sessions with minimal friction.
//! This command provides a streamlined way to extract just the human-typed
//! prompts without tool results, system messages, or other noise.

use std::collections::HashMap;
use std::fs::File;
use std::io::{self, BufWriter, Write};

use regex::{Regex, RegexBuilder};
use serde::Serialize;

use crate::cli::{Cli, OutputFormat, PromptsArgs};
use crate::discovery::{Session, SessionFilter};
use crate::error::{Result, SnatchError};
use crate::model::LogEntry;

use super::{get_claude_dir, parse_date_filter};

/// Entry in frequency output.
#[derive(Debug, Clone, Serialize)]
struct FrequencyEntry {
    prompt: String,
    count: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    percentage: Option<f64>,
}

/// Frequency output structure.
#[derive(Debug, Clone, Serialize)]
struct FrequencyOutput {
    entries: Vec<FrequencyEntry>,
    total_prompts: usize,
    unique_prompts: usize,
}

/// Statistics about prompts.
#[derive(Debug, Clone, Serialize)]
struct PromptsStats {
    total_prompts: usize,
    unique_prompts: usize,
    single_use_prompts: usize,
    single_use_percentage: f64,
    reused_prompts: usize,
    total_characters: usize,
    average_length: f64,
    median_length: usize,
    min_length: usize,
    max_length: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    length_distribution: Option<LengthDistribution>,
    top_prompts: Vec<FrequencyEntry>,
}

/// Length distribution buckets for stats output.
#[derive(Debug, Clone, Serialize)]
struct LengthDistribution {
    under_100: BucketStats,
    from_100_to_500: BucketStats,
    from_500_to_1000: BucketStats,
    from_1000_to_5000: BucketStats,
    over_5000: BucketStats,
}

/// Stats for a single length bucket.
#[derive(Debug, Clone, Serialize)]
struct BucketStats {
    count: usize,
    percentage: f64,
}

/// Output structure for --contains phrase counting.
#[derive(Debug, Clone, Serialize)]
struct ContainsOutput {
    phrase: String,
    matching_prompts: usize,
    total_prompts: usize,
    percentage: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    session_count: Option<usize>,
}

/// Compute frequency distribution of prompts.
fn compute_frequency(prompts: &[Prompt], args: &PromptsArgs) -> Vec<FrequencyEntry> {
    let mut counts: HashMap<&str, usize> = HashMap::new();

    for prompt in prompts {
        *counts.entry(prompt.text.as_str()).or_insert(0) += 1;
    }

    let total = prompts.len();
    let mut entries: Vec<FrequencyEntry> = counts
        .into_iter()
        .map(|(text, count)| FrequencyEntry {
            prompt: text.to_string(),
            count,
            percentage: if total > 0 {
                Some((count as f64 / total as f64) * 100.0)
            } else {
                None
            },
        })
        .collect();

    // Apply min_count filter
    if let Some(min) = args.min_count {
        entries.retain(|e| e.count >= min);
    }

    // Sort by count (descending) or length (descending)
    if args.sort_by_length {
        entries.sort_by(|a, b| b.prompt.len().cmp(&a.prompt.len()));
    } else {
        entries.sort_by(|a, b| b.count.cmp(&a.count));
    }

    entries
}

/// Compute statistics about prompts.
fn compute_stats(prompts: &[Prompt]) -> PromptsStats {
    let total = prompts.len();

    // Count frequencies
    let mut counts: HashMap<&str, usize> = HashMap::new();
    for prompt in prompts {
        *counts.entry(prompt.text.as_str()).or_insert(0) += 1;
    }

    let unique = counts.len();
    let single_use = counts.values().filter(|&&c| c == 1).count();
    let reused = unique - single_use;

    // Length statistics
    let mut lengths: Vec<usize> = prompts.iter().map(|p| p.text.len()).collect();
    lengths.sort_unstable();

    let total_chars: usize = lengths.iter().sum();
    let avg_len = if total > 0 { total_chars as f64 / total as f64 } else { 0.0 };
    let median_len = if lengths.is_empty() { 0 } else { lengths[lengths.len() / 2] };
    let min_len = lengths.first().copied().unwrap_or(0);
    let max_len = lengths.last().copied().unwrap_or(0);

    // Length distribution buckets
    let length_distribution = if total > 0 {
        let mut under_100 = 0;
        let mut from_100_to_500 = 0;
        let mut from_500_to_1000 = 0;
        let mut from_1000_to_5000 = 0;
        let mut over_5000 = 0;

        for &len in &lengths {
            match len {
                0..100 => under_100 += 1,
                100..500 => from_100_to_500 += 1,
                500..1000 => from_500_to_1000 += 1,
                1000..5000 => from_1000_to_5000 += 1,
                _ => over_5000 += 1,
            }
        }

        let pct = |count: usize| (count as f64 / total as f64) * 100.0;

        Some(LengthDistribution {
            under_100: BucketStats { count: under_100, percentage: pct(under_100) },
            from_100_to_500: BucketStats { count: from_100_to_500, percentage: pct(from_100_to_500) },
            from_500_to_1000: BucketStats { count: from_500_to_1000, percentage: pct(from_500_to_1000) },
            from_1000_to_5000: BucketStats { count: from_1000_to_5000, percentage: pct(from_1000_to_5000) },
            over_5000: BucketStats { count: over_5000, percentage: pct(over_5000) },
        })
    } else {
        None
    };

    // Top prompts by frequency
    let mut freq_entries: Vec<FrequencyEntry> = counts
        .into_iter()
        .map(|(text, count)| FrequencyEntry {
            prompt: text.to_string(),
            count,
            percentage: if total > 0 {
                Some((count as f64 / total as f64) * 100.0)
            } else {
                None
            },
        })
        .collect();
    freq_entries.sort_by(|a, b| b.count.cmp(&a.count));
    freq_entries.truncate(10);

    PromptsStats {
        total_prompts: total,
        unique_prompts: unique,
        single_use_prompts: single_use,
        single_use_percentage: if unique > 0 { (single_use as f64 / unique as f64) * 100.0 } else { 0.0 },
        reused_prompts: reused,
        total_characters: total_chars,
        average_length: avg_len,
        median_length: median_len,
        min_length: min_len,
        max_length: max_len,
        length_distribution,
        top_prompts: freq_entries,
    }
}

/// Deduplicate prompts, keeping only unique entries.
fn deduplicate_prompts(prompts: Vec<Prompt>) -> Vec<Prompt> {
    let mut seen: HashMap<String, Prompt> = HashMap::new();

    for prompt in prompts {
        seen.entry(prompt.text.clone()).or_insert(prompt);
    }

    seen.into_values().collect()
}

/// Write frequency output as JSON.
fn write_frequency_json<W: Write>(
    writer: &mut W,
    entries: &[FrequencyEntry],
    total_prompts: usize,
    session_count: Option<usize>,
) -> Result<()> {
    let output = FrequencyOutput {
        entries: entries.to_vec(),
        total_prompts,
        unique_prompts: entries.len(),
    };
    #[derive(Serialize)]
    struct JsonOutput {
        #[serde(flatten)]
        frequency: FrequencyOutput,
        #[serde(skip_serializing_if = "Option::is_none")]
        session_count: Option<usize>,
    }
    let json_out = JsonOutput {
        frequency: output,
        session_count,
    };
    serde_json::to_writer_pretty(&mut *writer, &json_out)?;
    writeln!(writer)?;
    Ok(())
}

/// Write frequency output as text.
fn write_frequency_text<W: Write>(
    writer: &mut W,
    entries: &[FrequencyEntry],
    total_prompts: usize,
    no_truncate: bool,
) -> Result<()> {
    writeln!(writer, "Prompt Frequency Analysis")?;
    writeln!(writer, "=========================")?;
    writeln!(writer, "Total prompts: {}", total_prompts)?;
    writeln!(writer, "Unique prompts: {}", entries.len())?;
    writeln!(writer)?;

    for entry in entries {
        let pct = entry.percentage.map(|p| format!(" ({:.1}%)", p)).unwrap_or_default();
        // Truncate long prompts for display (unless --no-truncate)
        let display_text = if !no_truncate && entry.prompt.len() > 80 {
            format!("{}...", &entry.prompt[..77])
        } else {
            entry.prompt.clone()
        };
        // Escape newlines for single-line display
        let display_text = display_text.replace('\n', "\\n");
        writeln!(writer, "{:>5}x{} {}", entry.count, pct, display_text)?;
    }
    Ok(())
}

/// Write stats output as JSON.
fn write_stats_json<W: Write>(
    writer: &mut W,
    stats: &PromptsStats,
    session_count: Option<usize>,
) -> Result<()> {
    #[derive(Serialize)]
    struct JsonOutput<'a> {
        #[serde(flatten)]
        stats: &'a PromptsStats,
        #[serde(skip_serializing_if = "Option::is_none")]
        session_count: Option<usize>,
    }
    let json_out = JsonOutput {
        stats,
        session_count,
    };
    serde_json::to_writer_pretty(&mut *writer, &json_out)?;
    writeln!(writer)?;
    Ok(())
}

/// Write stats output as text.
fn write_stats_text<W: Write>(
    writer: &mut W,
    stats: &PromptsStats,
    session_count: Option<usize>,
    no_truncate: bool,
) -> Result<()> {
    writeln!(writer, "Prompt Statistics")?;
    writeln!(writer, "=================")?;
    if let Some(count) = session_count {
        writeln!(writer, "Sessions analyzed: {}", count)?;
    }
    writeln!(writer)?;
    writeln!(writer, "Counts:")?;
    writeln!(writer, "  Total prompts:      {}", stats.total_prompts)?;
    writeln!(writer, "  Unique prompts:     {}", stats.unique_prompts)?;
    writeln!(writer, "  Single-use:         {} ({:.1}%)", stats.single_use_prompts, stats.single_use_percentage)?;
    writeln!(writer, "  Reused:             {}", stats.reused_prompts)?;
    writeln!(writer)?;
    writeln!(writer, "Length (characters):")?;
    writeln!(writer, "  Total:              {}", stats.total_characters)?;
    writeln!(writer, "  Average:            {:.1}", stats.average_length)?;
    writeln!(writer, "  Median:             {}", stats.median_length)?;
    writeln!(writer, "  Range:              {} - {}", stats.min_length, stats.max_length)?;

    // Length distribution
    if let Some(ref dist) = stats.length_distribution {
        writeln!(writer)?;
        writeln!(writer, "Length Distribution:")?;
        writeln!(writer, "  <100 chars:         {:>5} ({:>5.1}%)", dist.under_100.count, dist.under_100.percentage)?;
        writeln!(writer, "  100-500:            {:>5} ({:>5.1}%)", dist.from_100_to_500.count, dist.from_100_to_500.percentage)?;
        writeln!(writer, "  500-1000:           {:>5} ({:>5.1}%)", dist.from_500_to_1000.count, dist.from_500_to_1000.percentage)?;
        writeln!(writer, "  1000-5000:          {:>5} ({:>5.1}%)", dist.from_1000_to_5000.count, dist.from_1000_to_5000.percentage)?;
        writeln!(writer, "  >5000:              {:>5} ({:>5.1}%)", dist.over_5000.count, dist.over_5000.percentage)?;
    }

    writeln!(writer)?;
    writeln!(writer, "Top Prompts by Frequency:")?;
    for (i, entry) in stats.top_prompts.iter().enumerate() {
        // Truncate long prompts for display (unless --no-truncate)
        let display_text = if !no_truncate && entry.prompt.len() > 60 {
            format!("{}...", &entry.prompt[..57])
        } else {
            entry.prompt.clone()
        };
        // Escape newlines for single-line display
        let display_text = display_text.replace('\n', "\\n");
        writeln!(writer, "  {}. {}x: {}", i + 1, entry.count, display_text)?;
    }
    Ok(())
}

/// Count prompts containing a phrase.
fn count_contains(prompts: &[Prompt], phrase: &str, ignore_case: bool) -> usize {
    if ignore_case {
        let phrase_lower = phrase.to_lowercase();
        prompts.iter().filter(|p| p.text.to_lowercase().contains(&phrase_lower)).count()
    } else {
        prompts.iter().filter(|p| p.text.contains(phrase)).count()
    }
}

/// Write contains output as JSON.
fn write_contains_json<W: Write>(
    writer: &mut W,
    output: &ContainsOutput,
) -> Result<()> {
    serde_json::to_writer_pretty(&mut *writer, output)?;
    writeln!(writer)?;
    Ok(())
}

/// Write contains output as text.
fn write_contains_text<W: Write>(
    writer: &mut W,
    output: &ContainsOutput,
) -> Result<()> {
    writeln!(writer, "Phrase Search Results")?;
    writeln!(writer, "=====================")?;
    if let Some(count) = output.session_count {
        writeln!(writer, "Sessions analyzed: {}", count)?;
    }
    writeln!(writer)?;
    writeln!(writer, "Phrase: \"{}\"", output.phrase)?;
    writeln!(writer, "Matching prompts: {} ({:.1}% of {} total)",
        output.matching_prompts,
        output.percentage,
        output.total_prompts
    )?;
    Ok(())
}

/// Build a regex from the grep pattern if provided.
fn build_grep_regex(args: &PromptsArgs) -> Result<Option<Regex>> {
    match &args.grep {
        Some(pattern) => {
            let regex = RegexBuilder::new(pattern)
                .case_insensitive(args.ignore_case)
                .build()
                .map_err(|e| SnatchError::InvalidArgument {
                    name: "grep".to_string(),
                    reason: format!("Invalid regex pattern: {}", e),
                })?;
            Ok(Some(regex))
        }
        None => Ok(None),
    }
}

/// Filter prompts by grep pattern if specified.
fn filter_prompts_by_grep(prompts: Vec<Prompt>, grep_regex: Option<&Regex>, invert: bool) -> Vec<Prompt> {
    match grep_regex {
        Some(regex) => prompts
            .into_iter()
            .filter(|p| {
                let matches = regex.is_match(&p.text);
                if invert { !matches } else { matches }
            })
            .collect(),
        None => prompts,
    }
}

/// Common system message patterns to exclude.
const SYSTEM_MESSAGE_PATTERNS: &[&str] = &[
    "[Request interrupted",
    "Caveat: The messages below",
    "<command-name>",
    "<local-command-stdout>",
    "<local-command-stderr>",
    "This session is being continued from a previous conversation",
];

/// Filter out system messages that pollute prompt analysis.
fn filter_system_messages(prompts: Vec<Prompt>) -> Vec<Prompt> {
    prompts
        .into_iter()
        .filter(|p| {
            !SYSTEM_MESSAGE_PATTERNS.iter().any(|pattern| p.text.starts_with(pattern))
        })
        .collect()
}

/// Run the prompts command.
pub fn run(cli: &Cli, args: &PromptsArgs) -> Result<()> {
    // Validate arguments
    if args.session.is_none() && !args.all && args.project.is_none() {
        return Err(SnatchError::InvalidArgument {
            name: "session".to_string(),
            reason: "Specify a session ID, use --all, or use -p/--project to filter".to_string(),
        });
    }

    // If a specific session is provided
    if let Some(ref session_id) = args.session {
        return extract_single_session(cli, args, session_id);
    }

    // Otherwise, extract from multiple sessions
    extract_multiple_sessions(cli, args)
}

/// Extract prompts from a single session.
fn extract_single_session(cli: &Cli, args: &PromptsArgs, session_id: &str) -> Result<()> {
    let claude_dir = get_claude_dir(cli.claude_dir.as_ref())?;

    let session = claude_dir
        .find_session(session_id)?
        .ok_or_else(|| SnatchError::SessionNotFound {
            session_id: session_id.to_string(),
        })?;

    let mut prompts = extract_prompts_from_session(&session, args, cli.max_file_size)?;

    // Apply system message filter if requested
    if args.exclude_system {
        prompts = filter_system_messages(prompts);
    }

    // Build grep regex if pattern specified
    let grep_regex = build_grep_regex(args)?;

    // Apply grep filter if specified (with optional inversion)
    prompts = filter_prompts_by_grep(prompts, grep_regex.as_ref(), args.invert_match);

    let total_prompts = prompts.len();

    // Check if JSON output is requested
    let use_json = matches!(cli.effective_output(), OutputFormat::Json);

    // Create writer
    let mut writer: Box<dyn Write> = if let Some(ref path) = args.output_file {
        let file = File::create(path).map_err(|e| {
            SnatchError::io(format!("Failed to create output file: {}", path.display()), e)
        })?;
        Box::new(BufWriter::new(file))
    } else {
        Box::new(io::stdout())
    };

    // Handle special output modes: stats, frequency, contains, unique
    if args.stats {
        let stats = compute_stats(&prompts);
        if use_json {
            write_stats_json(&mut writer, &stats, None)?;
        } else {
            write_stats_text(&mut writer, &stats, None, args.no_truncate)?;
        }
    } else if args.frequency {
        let entries = compute_frequency(&prompts, args);
        if use_json {
            write_frequency_json(&mut writer, &entries, total_prompts, None)?;
        } else {
            write_frequency_text(&mut writer, &entries, total_prompts, args.no_truncate)?;
        }
    } else if let Some(ref phrase) = args.contains {
        let matching = count_contains(&prompts, phrase, args.ignore_case);
        let output = ContainsOutput {
            phrase: phrase.clone(),
            matching_prompts: matching,
            total_prompts,
            percentage: if total_prompts > 0 {
                (matching as f64 / total_prompts as f64) * 100.0
            } else {
                0.0
            },
            session_count: None,
        };
        if use_json {
            write_contains_json(&mut writer, &output)?;
        } else {
            write_contains_text(&mut writer, &output)?;
        }
    } else {
        // Apply unique filter if requested
        if args.unique {
            prompts = deduplicate_prompts(prompts);
        }

        // Apply limit if specified
        let total_before_limit = prompts.len();
        if let Some(limit) = args.limit {
            prompts.truncate(limit);
        }

        if use_json {
            write_prompts_json(&mut writer, &prompts, total_before_limit, None)?;
        } else {
            write_prompts(&mut writer, &prompts, args, None)?;
        }
    }

    // Flush and report if writing to file
    if let Some(ref path) = args.output_file {
        drop(writer);
        if !cli.quiet {
            let mode = if args.stats {
                "stats"
            } else if args.frequency {
                "frequency data"
            } else if args.contains.is_some() {
                "phrase search results"
            } else {
                "prompts"
            };
            eprintln!("Wrote {} to {}", mode, path.display());
        }
    }

    Ok(())
}

/// Extract prompts from multiple sessions.
fn extract_multiple_sessions(cli: &Cli, args: &PromptsArgs) -> Result<()> {
    let claude_dir = get_claude_dir(cli.claude_dir.as_ref())?;

    // Build grep regex if pattern specified (do this early to fail fast on invalid regex)
    let grep_regex = build_grep_regex(args)?;

    // Build session filter
    let mut filter = SessionFilter::new();

    if !args.subagents {
        filter = filter.main_only();
    }

    if let Some(ref since) = args.since {
        let since_time = parse_date_filter(since)?;
        filter.modified_after = Some(since_time);
    }

    if let Some(ref until) = args.until {
        let until_time = parse_date_filter(until)?;
        filter.modified_before = Some(until_time);
    }

    // Get all sessions
    let all_sessions = claude_dir.all_sessions()?;

    // Filter sessions
    let mut sessions: Vec<&Session> = all_sessions
        .iter()
        .filter(|s| {
            if let Some(ref project) = args.project {
                if !s.project_path().contains(project) {
                    return false;
                }
            }
            filter.matches(s).unwrap_or_default()
        })
        .collect();

    if sessions.is_empty() {
        if !cli.quiet {
            eprintln!("No sessions match the specified filters");
        }
        return Ok(());
    }

    // Sort by modification time (oldest first for chronological order)
    sessions.sort_by_key(|s| s.modified_time());

    // Check if JSON output is requested
    let use_json = matches!(cli.effective_output(), OutputFormat::Json);

    // Collect all prompts (needed for JSON output and limit application)
    let mut all_prompts: Vec<Prompt> = Vec::new();
    let mut session_count = 0;

    for session in &sessions {
        let prompts = match extract_prompts_from_session(session, args, cli.max_file_size) {
            Ok(p) => p,
            Err(e) => {
                if !cli.quiet {
                    eprintln!("Warning: Failed to extract from {}: {}", session.session_id(), e);
                }
                continue;
            }
        };

        // Apply system message filter if requested
        let prompts = if args.exclude_system {
            filter_system_messages(prompts)
        } else {
            prompts
        };

        // Apply grep filter if specified (with optional inversion)
        let prompts = filter_prompts_by_grep(prompts, grep_regex.as_ref(), args.invert_match);

        if prompts.is_empty() {
            continue;
        }

        session_count += 1;

        // Add session metadata to prompts if separators are enabled
        let prompts_with_meta: Vec<Prompt> = prompts
            .into_iter()
            .map(|mut p| {
                if args.separators || use_json {
                    p.session_id = Some(session.session_id().to_string());
                    p.project_path = Some(session.project_path().to_string());
                }
                p
            })
            .collect();

        all_prompts.extend(prompts_with_meta);

        // Check if we've reached the limit
        if let Some(limit) = args.limit {
            if all_prompts.len() >= limit {
                all_prompts.truncate(limit);
                break;
            }
        }
    }

    let total_prompts = all_prompts.len();

    // Write output
    let mut writer: Box<dyn Write> = if let Some(ref path) = args.output_file {
        let file = File::create(path).map_err(|e| {
            SnatchError::io(format!("Failed to create output file: {}", path.display()), e)
        })?;
        Box::new(BufWriter::new(file))
    } else {
        Box::new(io::stdout())
    };

    // Handle special output modes: stats, frequency, contains, unique
    if args.stats {
        let stats = compute_stats(&all_prompts);
        if use_json {
            write_stats_json(&mut writer, &stats, Some(session_count))?;
        } else {
            write_stats_text(&mut writer, &stats, Some(session_count), args.no_truncate)?;
        }
    } else if args.frequency {
        let entries = compute_frequency(&all_prompts, args);
        if use_json {
            write_frequency_json(&mut writer, &entries, total_prompts, Some(session_count))?;
        } else {
            write_frequency_text(&mut writer, &entries, total_prompts, args.no_truncate)?;
        }
    } else if let Some(ref phrase) = args.contains {
        let matching = count_contains(&all_prompts, phrase, args.ignore_case);
        let output = ContainsOutput {
            phrase: phrase.clone(),
            matching_prompts: matching,
            total_prompts,
            percentage: if total_prompts > 0 {
                (matching as f64 / total_prompts as f64) * 100.0
            } else {
                0.0
            },
            session_count: Some(session_count),
        };
        if use_json {
            write_contains_json(&mut writer, &output)?;
        } else {
            write_contains_text(&mut writer, &output)?;
        }
    } else {
        // Apply unique filter if requested
        if args.unique {
            all_prompts = deduplicate_prompts(all_prompts);
        }

        // Apply limit for standard output (not already applied for stats/frequency which use all data)
        let total_before_limit = all_prompts.len();
        if let Some(limit) = args.limit {
            all_prompts.truncate(limit);
        }

        if use_json {
            write_prompts_json(&mut writer, &all_prompts, total_before_limit, Some(session_count))?;
        } else {
            // Group prompts by session for text output if separators are enabled
            if args.separators {
                let mut current_session: Option<String> = None;
                let mut session_prompts: Vec<Prompt> = Vec::new();

                for prompt in &all_prompts {
                    let prompt_session = prompt.session_id.clone();
                    if current_session.as_ref() != prompt_session.as_ref() {
                        // Write previous session's prompts
                        if !session_prompts.is_empty() {
                            let info = current_session.as_ref().map(|sid| SessionInfo {
                                session_id: sid.clone(),
                                project_path: session_prompts[0]
                                    .project_path
                                    .clone()
                                    .unwrap_or_default(),
                            });
                            write_prompts(&mut writer, &session_prompts, args, info.as_ref())?;
                        }
                        session_prompts.clear();
                        current_session = prompt_session;
                    }
                    session_prompts.push(prompt.clone());
                }

                // Write final session's prompts
                if !session_prompts.is_empty() {
                    let info = current_session.as_ref().map(|sid| SessionInfo {
                        session_id: sid.clone(),
                        project_path: session_prompts[0]
                            .project_path
                            .clone()
                            .unwrap_or_default(),
                    });
                    write_prompts(&mut writer, &session_prompts, args, info.as_ref())?;
                }
            } else {
                write_prompts(&mut writer, &all_prompts, args, None)?;
            }
        }
    }

    // Finalize atomic file if writing to file
    if let Some(ref path) = args.output_file {
        drop(writer);
        if !cli.quiet {
            let mode = if args.stats {
                "stats"
            } else if args.frequency {
                "frequency data"
            } else if args.contains.is_some() {
                "phrase search results"
            } else {
                "prompts"
            };
            eprintln!(
                "Wrote {} from {} sessions to {}",
                mode,
                session_count,
                path.display()
            );
        }
    }

    Ok(())
}

/// Session information for separators.
#[derive(Debug, Clone, Serialize)]
struct SessionInfo {
    session_id: String,
    project_path: String,
}

/// Extracted prompt with metadata.
#[derive(Debug, Clone, Serialize)]
struct Prompt {
    text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    timestamp: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    session_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    project_path: Option<String>,
}

/// Collection of prompts for JSON output.
#[derive(Debug, Clone, Serialize)]
struct PromptsOutput {
    prompts: Vec<Prompt>,
    total_count: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    session_count: Option<usize>,
}

/// Extract prompts from a session.
fn extract_prompts_from_session(session: &Session, args: &PromptsArgs, max_file_size: Option<u64>) -> Result<Vec<Prompt>> {
    let entries = session.parse_with_options(max_file_size)?;
    let mut prompts = Vec::new();

    for entry in entries {
        if let LogEntry::User(user) = entry {
            // Get text content from user message
            let text = match &user.message {
                crate::model::message::UserContent::Simple(simple) => {
                    simple.content.clone()
                }
                crate::model::message::UserContent::Blocks(blocks) => {
                    // Extract text blocks only (skip tool results)
                    blocks
                        .content
                        .iter()
                        .filter_map(|block| {
                            if let crate::model::ContentBlock::Text(t) = block {
                                Some(t.text.as_str())
                            } else {
                                None
                            }
                        })
                        .collect::<Vec<_>>()
                        .join("\n")
                }
            };

            // Apply minimum length filter
            let trimmed = text.trim();
            if trimmed.len() >= args.min_length {
                let timestamp = if args.timestamps {
                    Some(user.timestamp.format("%Y-%m-%d %H:%M:%S UTC").to_string())
                } else {
                    None
                };

                prompts.push(Prompt {
                    text: trimmed.to_string(),
                    timestamp,
                    session_id: None,    // Will be filled in by caller if needed
                    project_path: None,  // Will be filled in by caller if needed
                });
            }
        }
    }

    Ok(prompts)
}

/// Write prompts as JSON to output.
fn write_prompts_json<W: Write>(
    writer: &mut W,
    prompts: &[Prompt],
    total_count: usize,
    session_count: Option<usize>,
) -> Result<()> {
    let output = PromptsOutput {
        prompts: prompts.to_vec(),
        total_count,
        session_count,
    };
    serde_json::to_writer_pretty(&mut *writer, &output)?;
    writeln!(writer)?;
    Ok(())
}

/// Write prompts to output.
fn write_prompts<W: Write>(
    writer: &mut W,
    prompts: &[Prompt],
    args: &PromptsArgs,
    session_info: Option<&SessionInfo>,
) -> Result<()> {
    // Write session separator if requested
    if let Some(info) = session_info {
        writeln!(writer)?;
        writeln!(writer, "# Session: {} ({})", &info.session_id[..8.min(info.session_id.len())], info.project_path)?;
        writeln!(writer)?;
    }

    for (i, prompt) in prompts.iter().enumerate() {
        if args.numbered {
            write!(writer, "{}. ", i + 1)?;
        }

        if let Some(ref ts) = prompt.timestamp {
            writeln!(writer, "[{}]", ts)?;
        }

        writeln!(writer, "{}", prompt.text)?;

        // Add blank line between prompts for readability
        if i < prompts.len() - 1 {
            writeln!(writer)?;
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_prompt(text: &str) -> Prompt {
        Prompt {
            text: text.to_string(),
            timestamp: None,
            session_id: None,
            project_path: None,
        }
    }

    #[test]
    fn test_prompt_struct() {
        let prompt = Prompt {
            text: "Hello world".to_string(),
            timestamp: Some("2025-12-30 12:00:00 UTC".to_string()),
            session_id: None,
            project_path: None,
        };
        assert_eq!(prompt.text, "Hello world");
        assert!(prompt.timestamp.is_some());
    }

    #[test]
    fn test_session_info_struct() {
        let info = SessionInfo {
            session_id: "abc12345-1234-5678-9abc-def012345678".to_string(),
            project_path: "/home/user/project".to_string(),
        };
        assert!(info.session_id.starts_with("abc12345"));
    }

    #[test]
    fn test_filter_prompts_by_grep_no_filter() {
        let prompts = vec![
            make_prompt("Hello world"),
            make_prompt("Goodbye world"),
        ];
        let filtered = filter_prompts_by_grep(prompts.clone(), None, false);
        assert_eq!(filtered.len(), 2);
    }

    #[test]
    fn test_filter_prompts_by_grep_simple_pattern() {
        let prompts = vec![
            make_prompt("implement a feature"),
            make_prompt("fix the bug"),
            make_prompt("implement another thing"),
        ];
        let regex = Regex::new("implement").unwrap();
        let filtered = filter_prompts_by_grep(prompts, Some(&regex), false);
        assert_eq!(filtered.len(), 2);
        assert!(filtered[0].text.contains("implement"));
        assert!(filtered[1].text.contains("implement"));
    }

    #[test]
    fn test_filter_prompts_by_grep_regex_pattern() {
        let prompts = vec![
            make_prompt("TODO: fix this"),
            make_prompt("FIXME: broken"),
            make_prompt("normal prompt"),
        ];
        let regex = Regex::new("TODO|FIXME").unwrap();
        let filtered = filter_prompts_by_grep(prompts, Some(&regex), false);
        assert_eq!(filtered.len(), 2);
    }

    #[test]
    fn test_filter_prompts_by_grep_case_insensitive() {
        let prompts = vec![
            make_prompt("Hello World"),
            make_prompt("hello there"),
            make_prompt("goodbye"),
        ];
        let regex = RegexBuilder::new("hello")
            .case_insensitive(true)
            .build()
            .unwrap();
        let filtered = filter_prompts_by_grep(prompts, Some(&regex), false);
        assert_eq!(filtered.len(), 2);
    }

    #[test]
    fn test_filter_prompts_by_grep_no_matches() {
        let prompts = vec![
            make_prompt("Hello world"),
            make_prompt("Goodbye world"),
        ];
        let regex = Regex::new("nonexistent").unwrap();
        let filtered = filter_prompts_by_grep(prompts, Some(&regex), false);
        assert!(filtered.is_empty());
    }

    #[test]
    fn test_filter_prompts_by_grep_invert_match() {
        let prompts = vec![
            make_prompt("implement a feature"),
            make_prompt("fix the bug"),
            make_prompt("implement another thing"),
        ];
        let regex = Regex::new("implement").unwrap();
        // Invert: exclude prompts matching "implement"
        let filtered = filter_prompts_by_grep(prompts, Some(&regex), true);
        assert_eq!(filtered.len(), 1);
        assert!(filtered[0].text.contains("fix the bug"));
    }

    #[test]
    fn test_filter_system_messages() {
        let prompts = vec![
            make_prompt("[Request interrupted by user]"),
            make_prompt("Caveat: The messages below were generated..."),
            make_prompt("<command-name>/compact</command-name>"),
            make_prompt("<local-command-stdout>output</local-command-stdout>"),
            make_prompt("Let's implement a feature"),
            make_prompt("How confident are you?"),
        ];
        let filtered = filter_system_messages(prompts);
        assert_eq!(filtered.len(), 2);
        assert!(filtered[0].text.starts_with("Let's"));
        assert!(filtered[1].text.starts_with("How"));
    }
}
