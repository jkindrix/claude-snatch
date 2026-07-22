//! Prompts command implementation.
//!
//! Extract user prompts from Claude Code sessions with minimal friction.
//! This command provides a streamlined way to extract just the human-typed
//! prompts without tool results, system messages, or other noise.

use std::collections::{BTreeSet, HashMap};
use std::fs::File;
use std::io::{self, BufWriter, Write};

use regex::{Regex, RegexBuilder};
use serde::Serialize;

use crate::analysis::extraction::{extract_user_prompt_text, is_human_prompt, is_noise_text};
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

#[derive(Debug, Clone)]
struct PromptSource {
    provider: String,
    qualified_id: String,
}

#[derive(Debug, Clone, Serialize)]
struct PromptProviderSkip {
    provider: String,
    reason: String,
}

#[derive(Debug, Clone)]
struct PromptCollectionMeta {
    providers: Vec<String>,
    session_descriptors_analyzed: usize,
    date_filter_fallback_descriptors: usize,
    skipped_providers: Vec<PromptProviderSkip>,
    warnings: Vec<String>,
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
    #[serde(skip_serializing_if = "Option::is_none")]
    provider: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    qualified_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    session_descriptors_analyzed: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    date_filter_fallback_descriptors: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    providers: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    skipped_providers: Option<Vec<PromptProviderSkip>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    warnings: Option<Vec<String>>,
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
        entries.sort_by(|a, b| {
            b.prompt
                .chars()
                .count()
                .cmp(&a.prompt.chars().count())
                .then_with(|| a.prompt.cmp(&b.prompt))
        });
    } else {
        entries.sort_by(|a, b| b.count.cmp(&a.count).then_with(|| a.prompt.cmp(&b.prompt)));
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
    let mut lengths: Vec<usize> = prompts.iter().map(|p| p.text.chars().count()).collect();
    lengths.sort_unstable();

    let total_chars: usize = lengths.iter().sum();
    let avg_len = if total > 0 {
        total_chars as f64 / total as f64
    } else {
        0.0
    };
    let median_len = if lengths.is_empty() {
        0
    } else {
        lengths[lengths.len() / 2]
    };
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
            under_100: BucketStats {
                count: under_100,
                percentage: pct(under_100),
            },
            from_100_to_500: BucketStats {
                count: from_100_to_500,
                percentage: pct(from_100_to_500),
            },
            from_500_to_1000: BucketStats {
                count: from_500_to_1000,
                percentage: pct(from_500_to_1000),
            },
            from_1000_to_5000: BucketStats {
                count: from_1000_to_5000,
                percentage: pct(from_1000_to_5000),
            },
            over_5000: BucketStats {
                count: over_5000,
                percentage: pct(over_5000),
            },
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
    freq_entries.sort_by(|a, b| b.count.cmp(&a.count).then_with(|| a.prompt.cmp(&b.prompt)));
    freq_entries.truncate(10);

    PromptsStats {
        total_prompts: total,
        unique_prompts: unique,
        single_use_prompts: single_use,
        single_use_percentage: if unique > 0 {
            (single_use as f64 / unique as f64) * 100.0
        } else {
            0.0
        },
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
    let mut seen = BTreeSet::new();
    prompts
        .into_iter()
        .filter(|prompt| seen.insert(prompt.text.clone()))
        .collect()
}

fn truncate_prompt_display(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_string();
    }
    if max_chars <= 3 {
        return text.chars().take(max_chars).collect();
    }
    let prefix: String = text.chars().take(max_chars - 3).collect();
    format!("{prefix}...")
}

/// Write frequency output as JSON.
fn write_frequency_json<W: Write>(
    writer: &mut W,
    entries: &[FrequencyEntry],
    total_prompts: usize,
    session_count: Option<usize>,
    source: Option<&PromptSource>,
    collection: Option<&PromptCollectionMeta>,
) -> Result<()> {
    let output = FrequencyOutput {
        entries: entries.to_vec(),
        total_prompts,
        unique_prompts: entries.len(),
    };
    #[derive(Serialize)]
    struct JsonOutput<'a> {
        #[serde(flatten)]
        frequency: FrequencyOutput,
        #[serde(skip_serializing_if = "Option::is_none")]
        session_count: Option<usize>,
        #[serde(skip_serializing_if = "Option::is_none")]
        provider: Option<&'a str>,
        #[serde(skip_serializing_if = "Option::is_none")]
        qualified_id: Option<&'a str>,
        #[serde(skip_serializing_if = "Option::is_none")]
        session_descriptors_analyzed: Option<usize>,
        #[serde(skip_serializing_if = "Option::is_none")]
        date_filter_fallback_descriptors: Option<usize>,
        #[serde(skip_serializing_if = "Option::is_none")]
        providers: Option<&'a [String]>,
        #[serde(skip_serializing_if = "Option::is_none")]
        skipped_providers: Option<&'a [PromptProviderSkip]>,
        #[serde(skip_serializing_if = "Option::is_none")]
        warnings: Option<&'a [String]>,
    }
    let json_out = JsonOutput {
        frequency: output,
        session_count,
        provider: source.map(|source| source.provider.as_str()),
        qualified_id: source.map(|source| source.qualified_id.as_str()),
        session_descriptors_analyzed: collection.map(|meta| meta.session_descriptors_analyzed),
        date_filter_fallback_descriptors: collection
            .map(|meta| meta.date_filter_fallback_descriptors),
        providers: collection.map(|meta| meta.providers.as_slice()),
        skipped_providers: collection.map(|meta| meta.skipped_providers.as_slice()),
        warnings: collection.map(|meta| meta.warnings.as_slice()),
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
        let pct = entry
            .percentage
            .map(|p| format!(" ({:.1}%)", p))
            .unwrap_or_default();
        // Truncate long prompts for display (unless --no-truncate)
        let display_text = if !no_truncate && entry.prompt.chars().count() > 80 {
            truncate_prompt_display(&entry.prompt, 80)
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
    source: Option<&PromptSource>,
    collection: Option<&PromptCollectionMeta>,
) -> Result<()> {
    #[derive(Serialize)]
    struct JsonOutput<'a> {
        #[serde(flatten)]
        stats: &'a PromptsStats,
        #[serde(skip_serializing_if = "Option::is_none")]
        session_count: Option<usize>,
        #[serde(skip_serializing_if = "Option::is_none")]
        provider: Option<&'a str>,
        #[serde(skip_serializing_if = "Option::is_none")]
        qualified_id: Option<&'a str>,
        #[serde(skip_serializing_if = "Option::is_none")]
        session_descriptors_analyzed: Option<usize>,
        #[serde(skip_serializing_if = "Option::is_none")]
        date_filter_fallback_descriptors: Option<usize>,
        #[serde(skip_serializing_if = "Option::is_none")]
        providers: Option<&'a [String]>,
        #[serde(skip_serializing_if = "Option::is_none")]
        skipped_providers: Option<&'a [PromptProviderSkip]>,
        #[serde(skip_serializing_if = "Option::is_none")]
        warnings: Option<&'a [String]>,
    }
    let json_out = JsonOutput {
        stats,
        session_count,
        provider: source.map(|source| source.provider.as_str()),
        qualified_id: source.map(|source| source.qualified_id.as_str()),
        session_descriptors_analyzed: collection.map(|meta| meta.session_descriptors_analyzed),
        date_filter_fallback_descriptors: collection
            .map(|meta| meta.date_filter_fallback_descriptors),
        providers: collection.map(|meta| meta.providers.as_slice()),
        skipped_providers: collection.map(|meta| meta.skipped_providers.as_slice()),
        warnings: collection.map(|meta| meta.warnings.as_slice()),
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
    writeln!(
        writer,
        "  Single-use:         {} ({:.1}%)",
        stats.single_use_prompts, stats.single_use_percentage
    )?;
    writeln!(writer, "  Reused:             {}", stats.reused_prompts)?;
    writeln!(writer)?;
    writeln!(writer, "Length (characters):")?;
    writeln!(writer, "  Total:              {}", stats.total_characters)?;
    writeln!(writer, "  Average:            {:.1}", stats.average_length)?;
    writeln!(writer, "  Median:             {}", stats.median_length)?;
    writeln!(
        writer,
        "  Range:              {} - {}",
        stats.min_length, stats.max_length
    )?;

    // Length distribution
    if let Some(ref dist) = stats.length_distribution {
        writeln!(writer)?;
        writeln!(writer, "Length Distribution:")?;
        writeln!(
            writer,
            "  <100 chars:         {:>5} ({:>5.1}%)",
            dist.under_100.count, dist.under_100.percentage
        )?;
        writeln!(
            writer,
            "  100-500:            {:>5} ({:>5.1}%)",
            dist.from_100_to_500.count, dist.from_100_to_500.percentage
        )?;
        writeln!(
            writer,
            "  500-1000:           {:>5} ({:>5.1}%)",
            dist.from_500_to_1000.count, dist.from_500_to_1000.percentage
        )?;
        writeln!(
            writer,
            "  1000-5000:          {:>5} ({:>5.1}%)",
            dist.from_1000_to_5000.count, dist.from_1000_to_5000.percentage
        )?;
        writeln!(
            writer,
            "  >5000:              {:>5} ({:>5.1}%)",
            dist.over_5000.count, dist.over_5000.percentage
        )?;
    }

    writeln!(writer)?;
    writeln!(writer, "Top Prompts by Frequency:")?;
    for (i, entry) in stats.top_prompts.iter().enumerate() {
        // Truncate long prompts for display (unless --no-truncate)
        let display_text = if !no_truncate && entry.prompt.chars().count() > 60 {
            truncate_prompt_display(&entry.prompt, 60)
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
        prompts
            .iter()
            .filter(|p| p.text.to_lowercase().contains(&phrase_lower))
            .count()
    } else {
        prompts.iter().filter(|p| p.text.contains(phrase)).count()
    }
}

/// Write contains output as JSON.
fn write_contains_json<W: Write>(writer: &mut W, output: &ContainsOutput) -> Result<()> {
    serde_json::to_writer_pretty(&mut *writer, output)?;
    writeln!(writer)?;
    Ok(())
}

/// Write contains output as text.
fn write_contains_text<W: Write>(writer: &mut W, output: &ContainsOutput) -> Result<()> {
    writeln!(writer, "Phrase Search Results")?;
    writeln!(writer, "=====================")?;
    if let Some(count) = output.session_count {
        writeln!(writer, "Sessions analyzed: {}", count)?;
    }
    writeln!(writer)?;
    writeln!(writer, "Phrase: \"{}\"", output.phrase)?;
    writeln!(
        writer,
        "Matching prompts: {} ({:.1}% of {} total)",
        output.matching_prompts, output.percentage, output.total_prompts
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
fn filter_prompts_by_grep(
    prompts: Vec<Prompt>,
    grep_regex: Option<&Regex>,
    invert: bool,
) -> Vec<Prompt> {
    match grep_regex {
        Some(regex) => prompts
            .into_iter()
            .filter(|p| {
                let matches = regex.is_match(&p.text);
                if invert {
                    !matches
                } else {
                    matches
                }
            })
            .collect(),
        None => prompts,
    }
}

/// Prompts-specific noise patterns beyond the shared `is_noise_text()`.
///
/// These are patterns that are useful to exclude from prompt frequency analysis
/// but are arguably real user input (e.g., session continuation summaries that
/// the user pastes, or caveat text that appears outside XML tags).
const PROMPTS_EXTRA_NOISE: &[&str] = &[
    "Caveat: The messages below",
    "This session is being continued from a previous conversation",
];

/// Filter out system messages that pollute prompt analysis.
///
/// Uses the shared `is_noise_text()` for common patterns (XML-tagged system
/// messages, interrupt markers) plus prompts-specific extras.
fn filter_system_messages(prompts: Vec<Prompt>) -> Vec<Prompt> {
    prompts
        .into_iter()
        .filter(|p| {
            !is_noise_text(&p.text)
                && !PROMPTS_EXTRA_NOISE
                    .iter()
                    .any(|pattern| p.text.starts_with(pattern))
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

    if !args.provider.is_empty() {
        return extract_provider_multiple_sessions(cli, args);
    }

    // Otherwise, extract from multiple sessions
    extract_multiple_sessions(cli, args)
}

/// Extract prompts from a single session.
fn extract_single_session(cli: &Cli, args: &PromptsArgs, session_id: &str) -> Result<()> {
    let registry = (!args.provider.is_empty() || session_id.contains(':'))
        .then(|| super::helpers::provider_registry(cli));
    let provider_route = !args.provider.is_empty()
        || registry
            .as_ref()
            .is_some_and(|registry| registry.looks_qualified(session_id));
    let (mut prompts, source) = if provider_route {
        // Complete classification: a future PromptsArgs field must be
        // consciously supported or refused on the provider route.
        let PromptsArgs {
            session: _,
            provider: _,
            all,
            project,
            since,
            until,
            min_length: _,
            limit: _,
            subagents,
            output_file: _,
            separators,
            timestamps: _,
            numbered: _,
            grep: _,
            ignore_case: _,
            invert_match: _,
            exclude_system: _,
            frequency: _,
            stats: _,
            unique: _,
            sort_by_length: _,
            min_count: _,
            no_truncate: _,
            contains: _,
        } = args;
        super::helpers::refuse_unsupported_flags(
            "provider-routed single-session prompts",
            &[
                ("--all", *all),
                ("--project", project.is_some()),
                ("--since", since.is_some()),
                ("--until", until.is_some()),
                ("--subagents", *subagents),
                ("--separators", *separators),
            ],
        )?;
        let registry =
            registry.expect("provider flags or qualified reference constructed registry");
        let resolution = registry.resolve_with_default_policy(&args.provider, session_id)?;
        let parsed = crate::provider::registry::cached_parsed_session(
            crate::cache::global_cache(),
            resolution.provider,
            &resolution.key,
        )?;
        let prompts = extract_prompts_from_parsed_session(
            &parsed,
            args,
            resolution.provider.capabilities().semantic_annotations,
            false,
        );
        (
            prompts,
            Some(PromptSource {
                provider: resolution.key.provider.to_string(),
                qualified_id: resolution.key.to_string(),
            }),
        )
    } else {
        let claude_dir = get_claude_dir(cli.claude_dir.as_ref())?;
        let session =
            claude_dir
                .find_session(session_id)?
                .ok_or_else(|| SnatchError::SessionNotFound {
                    session_id: session_id.to_string(),
                })?;
        (
            extract_prompts_from_session(&session, args, cli.max_file_size)?,
            None,
        )
    };

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
            SnatchError::io(
                format!("Failed to create output file: {}", path.display()),
                e,
            )
        })?;
        Box::new(BufWriter::new(file))
    } else {
        Box::new(io::stdout())
    };

    // Handle special output modes: stats, frequency, contains, unique
    if args.stats {
        let stats = compute_stats(&prompts);
        if use_json {
            write_stats_json(&mut writer, &stats, None, source.as_ref(), None)?;
        } else {
            write_stats_text(&mut writer, &stats, None, args.no_truncate)?;
        }
    } else if args.frequency {
        let entries = compute_frequency(&prompts, args);
        if use_json {
            write_frequency_json(
                &mut writer,
                &entries,
                total_prompts,
                None,
                source.as_ref(),
                None,
            )?;
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
            provider: source.as_ref().map(|source| source.provider.clone()),
            qualified_id: source.as_ref().map(|source| source.qualified_id.clone()),
            session_descriptors_analyzed: None,
            date_filter_fallback_descriptors: None,
            providers: None,
            skipped_providers: None,
            warnings: None,
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
            write_prompts_json(
                &mut writer,
                &prompts,
                total_before_limit,
                None,
                source.as_ref(),
                None,
            )?;
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

fn context_overlaps_date_range(
    context: &crate::provider::project::SessionProjectContext,
    since: Option<std::time::SystemTime>,
    until: Option<std::time::SystemTime>,
) -> (bool, bool) {
    if since.is_none() && until.is_none() {
        return (true, false);
    }

    let start = context.started_at.map(std::time::SystemTime::from);
    let native_end = if context.native_tail_unresolved {
        None
    } else {
        context.ended_at
    };
    let end = if native_end.is_some() {
        native_end
    } else {
        [context.ended_at, context.started_at, context.modified_at]
            .into_iter()
            .flatten()
            .max()
    }
    .map(std::time::SystemTime::from);

    let used_fallback = (since.is_some() && native_end.is_none())
        || (until.is_some() && context.started_at.is_none());
    if since.is_some_and(|cutoff| end.is_some_and(|timestamp| timestamp < cutoff)) {
        return (false, used_fallback);
    }
    if until.is_some_and(|cutoff| start.is_some_and(|timestamp| timestamp > cutoff)) {
        return (false, used_fallback);
    }
    (true, used_fallback)
}

fn sort_prompts_chronologically(prompts: &mut [Prompt]) {
    prompts.sort_by(|a, b| {
        a.sort_timestamp
            .cmp(&b.sort_timestamp)
            .then_with(|| a.sort_tiebreak.cmp(&b.sort_tiebreak))
    });
}

/// Extract prompts from a provider-selected session/project union.
fn extract_provider_multiple_sessions(cli: &Cli, args: &PromptsArgs) -> Result<()> {
    use crate::provider::registry::ProviderSelection;

    let selection = ProviderSelection::from_flags(&args.provider).map_err(|reason| {
        SnatchError::InvalidArgument {
            name: "--provider".to_string(),
            reason,
        }
    })?;
    let since = args.since.as_deref().map(parse_date_filter).transpose()?;
    let until = args.until.as_deref().map(parse_date_filter).transpose()?;
    let grep_regex = build_grep_regex(args)?;
    let registry = super::helpers::provider_registry(cli);
    let mut providers: BTreeSet<_> = registry
        .select(&selection)?
        .providers
        .into_iter()
        .map(|provider| provider.id().to_string())
        .collect();
    let mut prompts = Vec::new();
    let mut sessions_with_prompts = BTreeSet::new();
    let mut unique_prompts = BTreeSet::new();
    let mut date_filter_fallbacks = BTreeSet::new();
    let mut session_descriptors_analyzed = 0_usize;
    let mut total_matching_prompts = 0_usize;
    let use_json = matches!(cli.effective_output(), OutputFormat::Json);

    let report = registry.visit_filtered_parsed_project_sessions(
        &selection,
        crate::cache::global_cache(),
        args.project.as_deref(),
        args.subagents,
        |_, session| {
            let (include, used_fallback) =
                context_overlaps_date_range(&session.context, since, until);
            if include && used_fallback {
                date_filter_fallbacks.insert(session.descriptor.key.clone());
            }
            include
        },
        |project, session, logical_root, parsed| {
            session_descriptors_analyzed += 1;
            let semantic_annotations = registry
                .get(&session.descriptor.key.provider)
                .expect("visited session came from a registered provider")
                .capabilities()
                .semantic_annotations;
            let mut session_prompts =
                extract_prompts_from_parsed_session(&parsed, args, semantic_annotations, true);
            if args.exclude_system {
                session_prompts = filter_system_messages(session_prompts);
            }
            session_prompts =
                filter_prompts_by_grep(session_prompts, grep_regex.as_ref(), args.invert_match);
            if session_prompts.is_empty() {
                return;
            }

            sessions_with_prompts.insert(logical_root.clone());
            total_matching_prompts = total_matching_prompts.saturating_add(session_prompts.len());
            let qualified_id = session.descriptor.key.to_string();
            let provider = session.descriptor.key.provider.to_string();
            let project_key = project.identity.to_string();
            let project_path = project
                .display_path
                .clone()
                .unwrap_or_else(|| project_key.clone());
            for (index, mut prompt) in session_prompts.into_iter().enumerate() {
                if args.unique && !args.stats && !args.frequency && args.contains.is_none() {
                    unique_prompts.insert(prompt.text.clone());
                }
                prompt.sort_tiebreak = format!("{qualified_id}:{index:020}");
                if args.separators || use_json {
                    prompt.session_id = Some(qualified_id.clone());
                    prompt.project_path = Some(project_path.clone());
                }
                if use_json {
                    prompt.provider = Some(provider.clone());
                    prompt.qualified_id = Some(qualified_id.clone());
                    prompt.project_key = Some(project_key.clone());
                }
                prompts.push(prompt);
            }

            // The global limit bounds retained prompt memory without being
            // multiplied by the number of sessions. Every session is still
            // scanned so total_count and coverage metadata remain exact.
            if let Some(limit) = args.limit {
                sort_prompts_chronologically(&mut prompts);
                if args.unique && !args.stats && !args.frequency && args.contains.is_none() {
                    prompts = deduplicate_prompts(std::mem::take(&mut prompts));
                }
                prompts.truncate(limit);
            }
        },
    )?;

    sort_prompts_chronologically(&mut prompts);
    for (provider, _) in &report.skipped {
        providers.remove(&provider.to_string());
    }
    let mut warnings = report.warnings;
    if !date_filter_fallbacks.is_empty() {
        warnings.push(format!(
            "{} session descriptors used conservative source-time evidence for date filtering",
            date_filter_fallbacks.len()
        ));
    }
    warnings.sort();
    warnings.dedup();
    let metadata = PromptCollectionMeta {
        providers: providers.into_iter().collect(),
        session_descriptors_analyzed,
        date_filter_fallback_descriptors: date_filter_fallbacks.len(),
        skipped_providers: report
            .skipped
            .into_iter()
            .map(|(provider, reason)| PromptProviderSkip {
                provider: provider.to_string(),
                reason,
            })
            .collect(),
        warnings,
    };
    let standard_total_count = if args.unique {
        unique_prompts.len()
    } else {
        total_matching_prompts
    };
    let result = render_multiple_prompts(
        cli,
        args,
        prompts,
        sessions_with_prompts.len(),
        Some(&metadata),
        Some(standard_total_count),
    );
    for skipped in &metadata.skipped_providers {
        eprintln!(
            "warning: provider '{}' skipped: {}",
            skipped.provider, skipped.reason
        );
    }
    for warning in &metadata.warnings {
        eprintln!("warning: {warning}");
    }
    result
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
                    eprintln!(
                        "Warning: Failed to extract from {}: {}",
                        session.session_id(),
                        e
                    );
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

    render_multiple_prompts(cli, args, all_prompts, session_count, None, None)
}

fn render_multiple_prompts(
    cli: &Cli,
    args: &PromptsArgs,
    mut all_prompts: Vec<Prompt>,
    session_count: usize,
    collection: Option<&PromptCollectionMeta>,
    standard_total_count: Option<usize>,
) -> Result<()> {
    let total_prompts = all_prompts.len();
    let use_json = matches!(cli.effective_output(), OutputFormat::Json);

    // Write output
    let mut writer: Box<dyn Write> = if let Some(ref path) = args.output_file {
        let file = File::create(path).map_err(|e| {
            SnatchError::io(
                format!("Failed to create output file: {}", path.display()),
                e,
            )
        })?;
        Box::new(BufWriter::new(file))
    } else {
        Box::new(io::stdout())
    };

    // Handle special output modes: stats, frequency, contains, unique
    if args.stats {
        let stats = compute_stats(&all_prompts);
        if use_json {
            write_stats_json(&mut writer, &stats, Some(session_count), None, collection)?;
        } else {
            write_stats_text(&mut writer, &stats, Some(session_count), args.no_truncate)?;
        }
    } else if args.frequency {
        let entries = compute_frequency(&all_prompts, args);
        if use_json {
            write_frequency_json(
                &mut writer,
                &entries,
                total_prompts,
                Some(session_count),
                None,
                collection,
            )?;
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
            provider: None,
            qualified_id: None,
            session_descriptors_analyzed: collection.map(|meta| meta.session_descriptors_analyzed),
            date_filter_fallback_descriptors: collection
                .map(|meta| meta.date_filter_fallback_descriptors),
            providers: collection.map(|meta| meta.providers.clone()),
            skipped_providers: collection.map(|meta| meta.skipped_providers.clone()),
            warnings: collection.map(|meta| meta.warnings.clone()),
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
        let total_before_limit = standard_total_count.unwrap_or(all_prompts.len());
        if let Some(limit) = args.limit {
            all_prompts.truncate(limit);
        }

        if use_json {
            write_prompts_json(
                &mut writer,
                &all_prompts,
                total_before_limit,
                Some(session_count),
                None,
                collection,
            )?;
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
                        project_path: session_prompts[0].project_path.clone().unwrap_or_default(),
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
    #[serde(skip_serializing_if = "Option::is_none")]
    provider: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    qualified_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    project_key: Option<String>,
    #[serde(skip)]
    sort_timestamp: chrono::DateTime<chrono::Utc>,
    #[serde(skip)]
    sort_tiebreak: String,
}

/// Collection of prompts for JSON output.
#[derive(Debug, Clone, Serialize)]
struct PromptsOutput {
    prompts: Vec<Prompt>,
    total_count: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    session_count: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    provider: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    qualified_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    session_descriptors_analyzed: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    date_filter_fallback_descriptors: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    providers: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    skipped_providers: Option<Vec<PromptProviderSkip>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    warnings: Option<Vec<String>>,
}

/// Extract prompts from a session.
fn extract_prompts_from_session(
    session: &Session,
    args: &PromptsArgs,
    max_file_size: Option<u64>,
) -> Result<Vec<Prompt>> {
    let entries = session.parse_with_options(max_file_size)?;
    Ok(entries
        .iter()
        .filter(|entry| is_human_prompt(entry))
        .filter_map(|entry| prompt_from_entry(entry, args))
        .collect())
}

/// Extract prompts from a complete provider bundle. On providers that declare
/// semantic coverage, native authorship is authoritative: harness context and
/// tool output stay out while both turn-boundary and mid-turn human input are
/// retained. The complete text of each proven human prompt is preserved,
/// including quotes and fenced code; provenance is a classification signal,
/// not permission to rewrite what the user supplied.
fn extract_prompts_from_parsed_session(
    parsed: &crate::provider::ParsedSession,
    args: &PromptsArgs,
    semantic_annotations: bool,
    new_activity_only: bool,
) -> Vec<Prompt> {
    parsed
        .entries
        .iter()
        .filter(|identified| {
            if new_activity_only
                && parsed
                    .semantics
                    .get(&identified.id)
                    .is_some_and(|semantics| {
                        semantics.activity != crate::provider::ActivityKind::New
                    })
            {
                return false;
            }
            if !semantic_annotations {
                return is_human_prompt(&identified.entry);
            }
            parsed
                .semantics
                .get(&identified.id)
                .and_then(|semantics| semantics.prompt)
                .is_some_and(|prompt| prompt.authorship == crate::provider::PromptAuthorship::Human)
        })
        .filter_map(|identified| prompt_from_entry(&identified.entry, args))
        .collect()
}

fn prompt_from_entry(entry: &LogEntry, args: &PromptsArgs) -> Option<Prompt> {
    let LogEntry::User(user) = entry else {
        return None;
    };
    let text = extract_user_prompt_text(entry)?;
    let trimmed = text.trim();
    if trimmed.chars().count() < args.min_length {
        return None;
    }
    Some(Prompt {
        text: trimmed.to_string(),
        timestamp: args
            .timestamps
            .then(|| user.timestamp.format("%Y-%m-%d %H:%M:%S UTC").to_string()),
        session_id: None,
        project_path: None,
        provider: None,
        qualified_id: None,
        project_key: None,
        sort_timestamp: user.timestamp,
        sort_tiebreak: String::new(),
    })
}

/// Write prompts as JSON to output.
fn write_prompts_json<W: Write>(
    writer: &mut W,
    prompts: &[Prompt],
    total_count: usize,
    session_count: Option<usize>,
    source: Option<&PromptSource>,
    collection: Option<&PromptCollectionMeta>,
) -> Result<()> {
    let output = PromptsOutput {
        prompts: prompts.to_vec(),
        total_count,
        session_count,
        provider: source.map(|source| source.provider.clone()),
        qualified_id: source.map(|source| source.qualified_id.clone()),
        session_descriptors_analyzed: collection.map(|meta| meta.session_descriptors_analyzed),
        date_filter_fallback_descriptors: collection
            .map(|meta| meta.date_filter_fallback_descriptors),
        providers: collection.map(|meta| meta.providers.clone()),
        skipped_providers: collection.map(|meta| meta.skipped_providers.clone()),
        warnings: collection.map(|meta| meta.warnings.clone()),
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
        writeln!(
            writer,
            "# Session: {} ({})",
            &info.session_id[..8.min(info.session_id.len())],
            info.project_path
        )?;
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
    #![allow(clippy::trivial_regex)]
    use super::*;

    fn make_prompt(text: &str) -> Prompt {
        Prompt {
            text: text.to_string(),
            timestamp: None,
            session_id: None,
            project_path: None,
            provider: None,
            qualified_id: None,
            project_key: None,
            sort_timestamp: chrono::DateTime::parse_from_rfc3339("2026-01-01T00:00:00Z")
                .unwrap()
                .with_timezone(&chrono::Utc),
            sort_tiebreak: String::new(),
        }
    }

    #[test]
    fn test_prompt_struct() {
        let mut prompt = make_prompt("Hello world");
        prompt.timestamp = Some("2025-12-30 12:00:00 UTC".to_string());
        assert_eq!(prompt.text, "Hello world");
        assert!(prompt.timestamp.is_some());
    }

    #[test]
    fn deduplication_keeps_first_occurrence_order() {
        let prompts = vec![
            make_prompt("zebra"),
            make_prompt("alpha"),
            make_prompt("zebra"),
            make_prompt("beta"),
        ];
        let texts: Vec<_> = deduplicate_prompts(prompts)
            .into_iter()
            .map(|prompt| prompt.text)
            .collect();
        assert_eq!(texts, ["zebra", "alpha", "beta"]);
    }

    #[test]
    fn frequency_ties_are_deterministic() {
        let args = PromptsArgs {
            session: None,
            provider: Vec::new(),
            all: true,
            project: None,
            since: None,
            until: None,
            min_length: 0,
            limit: None,
            subagents: false,
            output_file: None,
            separators: false,
            timestamps: false,
            numbered: false,
            grep: None,
            ignore_case: false,
            invert_match: false,
            exclude_system: false,
            frequency: true,
            stats: false,
            unique: false,
            sort_by_length: false,
            min_count: None,
            no_truncate: false,
            contains: None,
        };
        let prompts = vec![make_prompt("zebra"), make_prompt("alpha")];
        let entries = compute_frequency(&prompts, &args);
        assert_eq!(entries[0].prompt, "alpha");
        assert_eq!(entries[1].prompt, "zebra");
    }

    #[test]
    fn text_analysis_truncates_unicode_at_a_character_boundary() {
        let prompt = make_prompt(&"🙂".repeat(100));
        let entry = FrequencyEntry {
            prompt: prompt.text,
            count: 1,
            percentage: Some(100.0),
        };
        let mut frequency = Vec::new();
        write_frequency_text(&mut frequency, std::slice::from_ref(&entry), 1, false).unwrap();
        assert!(String::from_utf8(frequency).unwrap().contains("..."));

        let stats = compute_stats(&[make_prompt(&"界".repeat(100))]);
        assert_eq!(stats.total_characters, 100);
        assert!((stats.average_length - 100.0).abs() < f64::EPSILON);
        assert_eq!(stats.min_length, 100);
        assert_eq!(stats.max_length, 100);
        let mut rendered = Vec::new();
        write_stats_text(&mut rendered, &stats, None, false).unwrap();
        assert!(String::from_utf8(rendered).unwrap().contains("..."));
    }

    #[test]
    fn date_overlap_never_invents_a_start_from_the_end() {
        let ended_at = chrono::DateTime::parse_from_rfc3339("2026-07-20T00:00:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc);
        let until = std::time::SystemTime::from(
            chrono::DateTime::parse_from_rfc3339("2026-07-10T00:00:00Z")
                .unwrap()
                .with_timezone(&chrono::Utc),
        );
        let context = crate::provider::project::SessionProjectContext {
            ended_at: Some(ended_at),
            modified_at: Some(ended_at),
            ..Default::default()
        };
        assert_eq!(
            context_overlaps_date_range(&context, None, Some(until)),
            (true, true),
            "without native start evidence the session must be included conservatively"
        );
    }

    #[test]
    fn date_overlap_prefers_a_complete_native_end_over_fresh_mtime() {
        let ended_at = chrono::DateTime::parse_from_rfc3339("2020-01-01T00:00:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc);
        let modified_at = chrono::DateTime::parse_from_rfc3339("2026-07-22T00:00:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc);
        let since = std::time::SystemTime::from(
            chrono::DateTime::parse_from_rfc3339("2026-07-17T00:00:00Z")
                .unwrap()
                .with_timezone(&chrono::Utc),
        );
        let complete = crate::provider::project::SessionProjectContext {
            ended_at: Some(ended_at),
            modified_at: Some(modified_at),
            ..Default::default()
        };
        assert_eq!(
            context_overlaps_date_range(&complete, Some(since), None),
            (false, false)
        );

        let unresolved = crate::provider::project::SessionProjectContext {
            native_tail_unresolved: true,
            ..complete
        };
        assert_eq!(
            context_overlaps_date_range(&unresolved, Some(since), None),
            (true, true)
        );
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
        let prompts = vec![make_prompt("Hello world"), make_prompt("Goodbye world")];
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
        let prompts = vec![make_prompt("Hello world"), make_prompt("Goodbye world")];
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
