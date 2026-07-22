//! Search command implementation.
//!
//! Searches across sessions for text patterns with optional filters.

use std::collections::HashSet;
use std::io::IsTerminal;
use std::time::SystemTime;

use chrono::{DateTime, Utc};
use indicatif::{ProgressBar, ProgressStyle};
use regex::{Regex, RegexBuilder};

#[cfg(test)]
use crate::analysis::search::expand_to_word_boundaries;
use crate::analysis::search::{
    count_projection_matches, project_entry_for_search, projection_matches, search_projection,
    ExactSearchMatcher, ProjectedSearchMatch, SearchScope,
};
use crate::cli::{Cli, OutputFormat, SearchArgs};
use crate::discovery::Session;
use crate::error::{Result, SnatchError};
use crate::index::provider::{IndexedSessionManifest, ProviderSearchIndex};
use crate::index::query::{
    IndexedSearchFilters, IndexedSearchOrder, IndexedSearchRequest, IndexedSearchResponse,
};
use crate::model::{ContentBlock, LogEntry};

/// (project_path, match_count, last_modified) for a session's search matches.
type SessionMatchCount = (String, usize, Option<SystemTime>);

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
            msg.message
                .usage
                .as_ref()
                .map(|u| u.input_tokens + u.output_tokens)
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
            msg.message
                .model
                .to_lowercase()
                .contains(&model_filter_lower)
        }
        // Non-assistant entries carry no model, so they cannot match a model
        // filter. Exclude them while a model filter is active (keeps `--model`
        // an entry-level assistant filter, so
        // `--files-only` returns a session only if it has a matching assistant
        // entry rather than any user text).
        _ => false,
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
        if args.thinking_only {
            Self::ThinkingOnly
        } else if args.all || (args.thinking && args.tools) {
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
    fn search_scope(&self) -> SearchScope {
        match self {
            Self::Default => SearchScope::Default,
            Self::Thinking => SearchScope::Thinking,
            Self::ThinkingOnly => SearchScope::ThinkingOnly,
            Self::Assistant => SearchScope::Assistant,
            Self::User => SearchScope::User,
            Self::Tools => SearchScope::Tools,
            Self::All => SearchScope::All,
        }
    }
}

/// A pattern with its own scope for batch processing.
struct BatchPattern {
    label: String,
    regex: Regex,
    scope: BatchScope,
}

/// Count all matches for one pattern against one entry.
fn count_pattern_matches(entry: &LogEntry, pattern: &BatchPattern) -> usize {
    count_projection_matches(
        &project_entry_for_search(entry),
        &ExactSearchMatcher::Regex(pattern.regex.clone()),
        pattern.scope.search_scope(),
    )
}

/// Whether `regex` matches any content a search in `scope` would inspect for
/// this entry.
///
/// `--exclude` must be evaluated over the SAME content set the search/count
/// paths scan. `extract_text_for_scope` alone omits tool-use/tool-result JSON
/// (all scopes) and thinking blocks (under `--all`), so an exclude built on it
/// silently fails to suppress matches inside that content. This mirrors the
/// scope coverage of `count_pattern_matches` and `search_entry`.
fn entry_matches_in_scope(entry: &LogEntry, regex: &Regex, scope: &BatchScope) -> bool {
    projection_matches(
        &project_entry_for_search(entry),
        &ExactSearchMatcher::Regex(regex.clone()),
        scope.search_scope(),
    )
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
        let regex =
            RegexBuilder::new(fields[4])
                .build()
                .map_err(|e| SnatchError::InvalidArgument {
                    name: "pattern".to_string(),
                    reason: format!(
                        "Line {}: invalid regex '{}': {}",
                        line_num + 1,
                        fields[4],
                        e
                    ),
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
        let session =
            claude_dir
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

    // Apply --since / --until date filters (content-based timestamps)
    super::helpers::filter_sessions_by_date(
        &mut sessions,
        args.since.as_deref(),
        args.until.as_deref(),
    )?;

    // Apply --recent N (most recent sessions by modification time)
    if let Some(n) = args.recent {
        sessions.sort_by_key(|b| std::cmp::Reverse(b.modified_time()));
        sessions.truncate(n);
    }

    // Apply --no-subagents filter
    if args.no_subagents {
        sessions.retain(|s| !s.is_subagent());
    }

    // `--files-only` exposes discovery order directly and stops after a
    // bounded number of distinct sessions. Make both the selected membership
    // and its rendering deterministic, including when mtimes tie.
    if args.files_only {
        sessions.sort_by(|a, b| {
            b.modified_time()
                .cmp(&a.modified_time())
                .then_with(|| a.session_id().cmp(b.session_id()))
                .then_with(|| a.path().cmp(b.path()))
        });
    }

    Ok(sessions)
}

/// Run the batch (multi-pattern, single-pass) search and output counts.
fn run_batch(cli: &Cli, args: &SearchArgs, patterns: Vec<BatchPattern>) -> Result<()> {
    let sessions = collect_sessions(cli, args)?;

    // Build exclude regex if specified, so batch honors --exclude like the
    // single-pattern path does.
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
                .template(
                    "{spinner:.cyan} [{bar:40.cyan/dim}] {pos}/{len} sessions ({eta} remaining)",
                )
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
            // Apply the general filter stack (model, tool-name, errors, token
            // bounds, branch, message-type) at entry level, matching the
            // single-pattern path. Previously batch skipped these entirely.
            if !matches_filters(entry, args) {
                continue;
            }

            for (i, pattern) in patterns.iter().enumerate() {
                // Apply --exclude per pattern using that pattern's own scope,
                // skipping the entry's contribution to this pattern when the
                // exclude regex matches any content that pattern would search.
                if let Some(ref excl) = exclude_regex {
                    if entry_matches_in_scope(entry, excl, &pattern.scope) {
                        continue;
                    }
                }

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
                let entries: Vec<serde_json::Value> = patterns
                    .iter()
                    .enumerate()
                    .map(|(i, p)| {
                        let parts: Vec<&str> = p.label.splitn(3, '\t').collect();
                        serde_json::json!({
                            "category": parts.first().unwrap_or(&""),
                            "subcategory": parts.get(1).unwrap_or(&""),
                            "label": parts.get(2).unwrap_or(&""),
                            "count": counts[i],
                        })
                    })
                    .collect();
                println!("{}", serde_json::to_string_pretty(&entries)?);
            } else {
                let map: Vec<serde_json::Value> = patterns
                    .iter()
                    .enumerate()
                    .map(|(i, p)| {
                        serde_json::json!({
                            "pattern": p.label,
                            "count": counts[i],
                        })
                    })
                    .collect();
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
                        if !prev_cat.is_empty() {
                            println!();
                        }
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
            let entries: Vec<serde_json::Value> = patterns
                .iter()
                .enumerate()
                .map(|(i, p)| {
                    let parts: Vec<&str> = p.label.splitn(3, '\t').collect();
                    let sessions: Vec<serde_json::Value> = {
                        let mut sess: Vec<_> = per_session[i].iter().collect();
                        sess.sort_by_key(|b| std::cmp::Reverse((b.1).0));
                        sess.iter()
                            .map(|(sid, (count, modified))| {
                                let mut val = serde_json::json!({
                                    "session_id": sid,
                                    "count": count,
                                });
                                if let Some(time) = modified {
                                    val["date"] = serde_json::Value::String(format_date(time));
                                }
                                val
                            })
                            .collect()
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
                })
                .collect();
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
                        if !prev_cat.is_empty() {
                            println!();
                        }
                        println!("=== {} ===", cat.to_uppercase());
                        prev_cat = cat.to_string();
                    }
                    println!("{:<7} {}", counts[i], label);
                } else {
                    println!("{:<7} {}", counts[i], p.label);
                }

                // Per-session breakdown
                let mut sess: Vec<_> = per_session[i].iter().collect();
                sess.sort_by_key(|b| std::cmp::Reverse((b.1).0));
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

fn validate_indexed_args(args: &SearchArgs) -> Result<()> {
    let SearchArgs {
        pattern: _,
        provider: _,
        project: _,
        session: _,
        ignore_case: _,
        thinking: _,
        thinking_only: _,
        tools: _,
        all: _,
        context: _,
        limit: _,
        no_limit,
        files_only: _,
        count: _,
        message_type: _,
        model: _,
        tool_name: _,
        errors,
        fuzzy: _,
        fuzzy_threshold: _,
        min_tokens: _,
        max_tokens: _,
        git_branch: _,
        sort: _,
        patterns_tsv,
        since: _,
        until: _,
        recent: _,
        match_only: _,
        exclude: _,
        with_date,
        no_subagents: _,
        aggregate_by_session,
        breakdown,
        phase,
        show_uuid,
    } = args;
    super::helpers::refuse_unsupported_flags(
        "provider-index search",
        &[
            ("multiple patterns", args.pattern.len() > 1),
            ("--patterns-tsv", patterns_tsv.is_some()),
            ("--no-limit", *no_limit),
            ("--errors", *errors),
            ("--breakdown", *breakdown),
            ("--phase", phase.is_some()),
            ("--show-uuid", *show_uuid),
            (
                "--with-date",
                *with_date && !args.count && !*aggregate_by_session,
            ),
        ],
    )
}

fn indexed_manifest_overlaps(
    manifest: &IndexedSessionManifest,
    since: Option<DateTime<Utc>>,
    until: Option<DateTime<Utc>>,
) -> bool {
    let start = manifest
        .source_started_at
        .or(manifest.source_ended_at)
        .or(manifest.source_modified_at);
    let end = manifest
        .source_ended_at
        .or(manifest.source_started_at)
        .or(manifest.source_modified_at);
    let (Some(start), Some(end)) = (start, end) else {
        return since.is_none() && until.is_none();
    };
    let after_start = match since {
        Some(bound) => end >= bound,
        None => true,
    };
    let before_end = match until {
        Some(bound) => start <= bound,
        None => true,
    };
    after_start && before_end
}

fn indexed_session_filter(
    index: &ProviderSearchIndex,
    selection: &crate::provider::registry::ProviderSelection,
    args: &SearchArgs,
) -> Result<(Vec<String>, bool, bool)> {
    let resolved_session = args
        .session
        .as_deref()
        .map(|reference| super::index::resolve_indexed_session(index, selection, reference))
        .transpose()?;
    let restrict = indexed_search_needs_session_prefilter(args, resolved_session.is_some());
    if !restrict {
        return Ok((Vec::new(), false, false));
    }
    let since = args
        .since
        .as_deref()
        .map(super::parse_date_filter)
        .transpose()?
        .map(DateTime::<Utc>::from);
    let until = args
        .until
        .as_deref()
        .map(super::parse_date_filter)
        .transpose()?
        .map(DateTime::<Utc>::from);
    let selected = super::index::selected_provider_names(selection);
    let project = args.project.as_ref().map(|value| value.to_lowercase());
    let mut manifests: Vec<_> = index
        .session_manifests()?
        .into_iter()
        .filter(|manifest| match &selected {
            Some(providers) => providers.contains(&manifest.provider),
            None => true,
        })
        .filter(|manifest| match &resolved_session {
            Some(session) => manifest.session_key == *session,
            None => true,
        })
        .filter(|manifest| {
            resolved_session.is_some()
                || match &project {
                    Some(needle) => {
                        manifest.project_path.to_lowercase().contains(needle)
                            || manifest.project_key.to_lowercase().contains(needle)
                    }
                    None => true,
                }
        })
        .filter(|manifest| indexed_manifest_overlaps(manifest, since, until))
        .collect();
    if let Some(limit) = args.recent {
        manifests.sort_by(|left, right| {
            right
                .source_modified_at
                .cmp(&left.source_modified_at)
                .then_with(|| left.session_key.cmp(&right.session_key))
        });
        manifests.truncate(limit);
    }
    let mut keys: Vec<_> = manifests
        .into_iter()
        .map(|manifest| manifest.session_key)
        .collect();
    keys.sort();
    keys.dedup();
    let scope_filter = resolved_session.is_none() || keys.is_empty();
    Ok((keys, true, scope_filter))
}

fn indexed_search_needs_session_prefilter(args: &SearchArgs, resolved_session: bool) -> bool {
    resolved_session
        || args.project.is_some()
        || args.since.is_some()
        || args.until.is_some()
        || args.recent.is_some()
}

fn indexed_matcher(args: &SearchArgs) -> Result<ExactSearchMatcher> {
    if args.pattern.len() != 1 || args.pattern[0].trim().is_empty() {
        return Err(SnatchError::InvalidArgument {
            name: "pattern".to_string(),
            reason: "provider-index search requires exactly one non-empty pattern".to_string(),
        });
    }
    if args.fuzzy {
        return Ok(ExactSearchMatcher::fuzzy(
            &args.pattern[0],
            args.ignore_case,
            args.fuzzy_threshold,
        ));
    }
    ExactSearchMatcher::regex(&args.pattern[0], args.ignore_case).map_err(|error| {
        SnatchError::InvalidArgument {
            name: "pattern".to_string(),
            reason: error.to_string(),
        }
    })
}

fn output_indexed_coverage_warning(cli: &Cli, response: &IndexedSearchResponse) {
    if response.coverage.incomplete && !cli.quiet {
        eprintln!(
            "Warning: indexed coverage is incomplete for generation {}",
            response.coverage.generation
        );
    }
}

fn output_indexed_summary(
    cli: &Cli,
    args: &SearchArgs,
    response: &IndexedSearchResponse,
) -> Result<()> {
    let use_occurrences = args.count;
    let total = if use_occurrences {
        response.total_occurrences
    } else {
        response.total_matches
    };
    let mut summaries = response.by_session.clone();
    summaries.sort_by(|left, right| {
        let left_count = if use_occurrences {
            left.occurrences
        } else {
            left.matching_lines
        };
        let right_count = if use_occurrences {
            right.occurrences
        } else {
            right.matching_lines
        };
        right_count
            .cmp(&left_count)
            .then_with(|| left.session_key.cmp(&right.session_key))
    });

    match cli.effective_output() {
        OutputFormat::Json => {
            println!(
                "{}",
                serde_json::to_string_pretty(&serde_json::json!({
                    "total": total,
                    "count_basis": if use_occurrences { "occurrences" } else { "matching_lines" },
                    "by_session": summaries,
                    "coverage": response.coverage,
                }))?
            );
        }
        _ if cli.quiet && args.count => println!("{total}"),
        _ => {
            for summary in &summaries {
                let count = if use_occurrences {
                    summary.occurrences
                } else {
                    summary.matching_lines
                };
                if args.with_date {
                    let date = summary
                        .source_modified_at
                        .map(|value| value.format("%Y-%m-%d").to_string())
                        .unwrap_or_else(|| "unknown".to_string());
                    println!(
                        "{} ({}) [{}]:{}",
                        summary.session_key, summary.project_path, date, count
                    );
                } else {
                    println!(
                        "{} ({}):{}",
                        summary.session_key, summary.project_path, count
                    );
                }
            }
            if summaries.len() > 1 {
                println!();
                println!("Total: {total}");
            }
        }
    }
    output_indexed_coverage_warning(cli, response);
    Ok(())
}

fn output_indexed_files(
    cli: &Cli,
    args: &SearchArgs,
    response: &IndexedSearchResponse,
) -> Result<()> {
    let mut summaries = response.by_session.clone();
    summaries.sort_by(|left, right| {
        right
            .source_modified_at
            .cmp(&left.source_modified_at)
            .then_with(|| left.session_key.cmp(&right.session_key))
    });
    let total = summaries.len();
    summaries.truncate(args.limit);
    match cli.effective_output() {
        OutputFormat::Json => println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "total_sessions": total,
                "sessions": summaries,
                "coverage": response.coverage,
            }))?
        ),
        _ => {
            for summary in &summaries {
                println!("{}", summary.session_key);
            }
            if total > summaries.len() && !cli.quiet {
                eprintln!(
                    "Showing {} matching sessions (limit: {})",
                    summaries.len(),
                    args.limit
                );
            }
        }
    }
    output_indexed_coverage_warning(cli, response);
    Ok(())
}

fn output_indexed_match_only(cli: &Cli, response: &IndexedSearchResponse) -> Result<()> {
    match cli.effective_output() {
        OutputFormat::Json => println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "total_matches": response.total_matches,
                "matches": response.matches.iter().map(|hit| &hit.matched_text).collect::<Vec<_>>(),
                "coverage": response.coverage,
            }))?
        ),
        _ => {
            for hit in &response.matches {
                println!("{}", hit.matched_text);
            }
        }
    }
    output_indexed_coverage_warning(cli, response);
    Ok(())
}

fn run_indexed(cli: &Cli, args: &SearchArgs) -> Result<()> {
    validate_indexed_args(args)?;
    let selection = if args.provider.is_empty() {
        let reference = args
            .session
            .as_deref()
            .ok_or_else(|| SnatchError::InvalidArgument {
                name: "provider".to_string(),
                reason: "provider-index search requires --provider or a qualified session"
                    .to_string(),
            })?;
        let key: crate::provider::LogicalSessionKey =
            reference
                .parse()
                .map_err(|reason: String| SnatchError::InvalidArgument {
                    name: "session".to_string(),
                    reason,
                })?;
        crate::provider::registry::ProviderSelection::Explicit(vec![key.provider])
    } else {
        super::index::provider_selection(&args.provider)?
    };
    let index = ProviderSearchIndex::open_read_only(super::index::index_path(cli))?;
    let (session_keys, restrict_session_keys, session_keys_are_scope_filter) =
        indexed_session_filter(&index, &selection, args)?;
    let exclude = args
        .exclude
        .as_deref()
        .map(|pattern| ExactSearchMatcher::regex(pattern, args.ignore_case))
        .transpose()
        .map_err(|error| SnatchError::InvalidArgument {
            name: "exclude".to_string(),
            reason: error.to_string(),
        })?;
    let summary_only = args.files_only || args.count || args.aggregate_by_session;
    let response = index.query(&IndexedSearchRequest {
        selection: super::index::indexed_selection(&selection),
        matcher: indexed_matcher(args)?,
        exclude,
        scope: BatchScope::from_search_args(args).search_scope(),
        filters: IndexedSearchFilters {
            session_keys,
            restrict_session_keys,
            session_keys_are_scope_filter,
            project_contains: args.project.clone(),
            message_types: args.message_type.iter().cloned().collect(),
            model_contains: args.model.clone(),
            tool_name_contains: args.tool_name.clone(),
            git_branch_contains: args.git_branch.clone(),
            min_processed_tokens: args.min_tokens,
            max_processed_tokens: args.max_tokens,
            include_spawned: !args.no_subagents,
            ..Default::default()
        },
        context_lines: args.context,
        order: if args.sort {
            IndexedSearchOrder::Relevance
        } else {
            IndexedSearchOrder::Source
        },
        offset: 0,
        limit: if summary_only { 0 } else { args.limit },
    })?;
    if args.files_only {
        output_indexed_files(cli, args, &response)
    } else if args.aggregate_by_session || args.count {
        output_indexed_summary(cli, args, &response)
    } else if args.match_only {
        output_indexed_match_only(cli, &response)
    } else {
        super::index::output_search_response(cli, &response)
    }
}

/// Run the search command.
pub fn run(cli: &Cli, args: &SearchArgs) -> Result<()> {
    let qualified_session = args
        .session
        .as_deref()
        .is_some_and(|reference| super::helpers::provider_registry(cli).looks_qualified(reference));
    if !args.provider.is_empty() || qualified_session {
        return run_indexed(cli, args);
    }
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
    let mut sessions_with_matches = Vec::new();
    let mut seen_session_ids: HashSet<String> = HashSet::new();
    // Maps session_id -> (project_path, match_count, modified_time)
    let mut match_counts: std::collections::HashMap<String, SessionMatchCount> =
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

    // In files-only mode, `--limit` counts distinct session IDs rather than
    // matching lines. Scan one result beyond the visible limit so the
    // truncation notice is based on evidence rather than an exact-boundary
    // guess. `usize::MAX` cannot have a lookahead, but is effectively
    // unbounded in practice.
    let files_only_scan_limit = if args.files_only && !args.no_limit {
        Some(args.limit.saturating_add(1))
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

            // Apply --exclude: skip entries where exclude pattern matches any
            // content the search would inspect for this scope.
            if let Some(ref excl) = exclude_regex {
                let scope = BatchScope::from_search_args(args);
                if entry_matches_in_scope(entry, excl, &scope) {
                    continue;
                }
            }

            if args.files_only {
                let entry_matches = if let Some(ref bp) = count_mode_pattern {
                    count_pattern_matches(entry, bp) > 0
                } else {
                    !search_entry(entry, &regex, args).is_empty()
                };

                if entry_matches {
                    let session_id = session.session_id().to_string();
                    if seen_session_ids.insert(session_id.clone()) {
                        sessions_with_matches.push(session_id);
                    }
                    // grep -l semantics: after the first match, nothing else
                    // in this physical session can affect the result.
                    break;
                }
                continue;
            }

            if let Some(ref bp) = count_mode_pattern {
                // Count mode: use occurrence-based counting (same as batch path)
                let entry_count = count_pattern_matches(entry, bp);
                if entry_count > 0 {
                    total_matches += entry_count;
                    session_match_count += entry_count;
                }
            } else {
                // Normal mode: use line-based matching with context
                let matches = search_entry(entry, &regex, args);

                if !matches.is_empty() {
                    for m in matches {
                        total_matches += 1;
                        session_match_count += 1;

                        if !args.files_only {
                            let (phase, minutes_in, post_compaction) = phase_ctx.classify(entry);

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

        if args.files_only {
            if files_only_scan_limit.is_some_and(|limit| sessions_with_matches.len() >= limit) {
                break;
            }
            continue;
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
        all_results.sort_by_key(|b| std::cmp::Reverse(b.score));
    }

    // Output results based on mode
    if args.files_only {
        let files_only_truncated = !args.no_limit && sessions_with_matches.len() > args.limit;
        if files_only_truncated {
            sessions_with_matches.truncate(args.limit);
        }
        output_files_only(cli, &sessions_with_matches)?;
        if files_only_truncated && !cli.quiet {
            eprintln!(
                "Showing {} matching sessions (limit: {}, use --no-limit for all)",
                sessions_with_matches.len(),
                args.limit
            );
        }
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
fn output_files_only(cli: &Cli, sessions: &[String]) -> Result<()> {
    match cli.effective_output() {
        OutputFormat::Json => {
            println!("{}", serde_json::to_string_pretty(sessions)?);
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
    match_counts: &std::collections::HashMap<String, SessionMatchCount>,
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
                let mut counts: Vec<(&String, &SessionMatchCount)> = match_counts.iter().collect();
                counts.sort_by_key(|b| std::cmp::Reverse((b.1).1));

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
    match_counts: &std::collections::HashMap<String, SessionMatchCount>,
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
                b["count"]
                    .as_u64()
                    .unwrap_or(0)
                    .cmp(&a["count"].as_u64().unwrap_or(0))
            });
            let output = serde_json::json!({
                "total": total,
                "sessions": entries,
            });
            println!("{}", serde_json::to_string_pretty(&output)?);
        }
        _ => {
            let mut counts: Vec<(&String, &SessionMatchCount)> = match_counts.iter().collect();
            counts.sort_by_key(|b| std::cmp::Reverse((b.1).1));

            for (session_id, (_project, count, modified)) in &counts {
                let short_id = &session_id[..8.min(session_id.len())];
                let date_str = modified
                    .as_ref()
                    .map(format_date)
                    .unwrap_or_else(|| "unknown".to_string());
                println!("{}  {}  {} matches", date_str, short_id, count);
            }
            println!(
                "\nTotal: {} matches across {} sessions",
                total,
                counts.len()
            );
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
                println!(
                    "{} ({}):{}: {}",
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
                    println!(
                        "Session: {} ({})",
                        &result.session_id[..8.min(result.session_id.len())],
                        result.project
                    );
                }

                println!();
                let phase_info = match (&result.phase, &result.minutes_in, &result.post_compaction)
                {
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
                println!(
                    "  [{}]{}{}",
                    format_match_label(&result.entry_type, &result.location),
                    phase_info,
                    uuid_suffix
                );

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
                if matches!(
                    sys.subtype,
                    Some(
                        crate::model::SystemSubtype::CompactBoundary
                            | crate::model::SystemSubtype::MicrocompactBoundary
                    )
                ) {
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
                let total_mins = (end - start).num_minutes();
                let elapsed_mins = (ts - start).num_minutes();
                let remaining_mins = (end - ts).num_minutes();

                if total_mins <= 120 {
                    // Short sessions: use ratio
                    let position = elapsed_mins as f64 / total_mins.max(1) as f64;
                    if position < 0.33 {
                        SessionPhase::Early
                    } else if position < 0.67 {
                        SessionPhase::Middle
                    } else {
                        SessionPhase::Late
                    }
                } else {
                    // Long sessions: absolute thresholds
                    if elapsed_mins < 30 {
                        SessionPhase::Early
                    } else if remaining_mins < 30 {
                        SessionPhase::Late
                    } else {
                        SessionPhase::Middle
                    }
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

type Match = ProjectedSearchMatch;

/// Search an entry for matches.
fn search_entry(entry: &LogEntry, regex: &Regex, args: &SearchArgs) -> Vec<Match> {
    let matcher = if args.fuzzy {
        ExactSearchMatcher::fuzzy(
            args.pattern[0].clone(),
            args.ignore_case,
            args.fuzzy_threshold,
        )
    } else {
        ExactSearchMatcher::Regex(regex.clone())
    };
    let scope = BatchScope::from_search_args(args).search_scope();
    search_projection(
        &project_entry_for_search(entry),
        &matcher,
        scope,
        args.context,
    )
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
    #![allow(clippy::trivial_regex)]
    use clap::Parser as _;

    use super::*;

    #[test]
    fn project_alone_activates_indexed_session_prefilter() {
        let cli = crate::cli::Cli::try_parse_from([
            "snatch",
            "search",
            "needle",
            "--provider",
            "all",
            "--project",
            "target-project",
        ])
        .unwrap();
        let Some(crate::cli::Commands::Search(args)) = cli.command else {
            panic!("expected search command");
        };
        assert!(indexed_search_needs_session_prefilter(&args, false));
    }

    #[test]
    fn test_matches_git_branch_exact() {
        use crate::model::{UserContent, UserMessage, UserSimpleContent};
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
            is_compact_summary: None,
            is_visible_in_transcript_only: None,
            thinking_metadata: None,
            todos: Vec::new(),
            tool_use_result: None,
            message: UserContent::Simple(UserSimpleContent {
                role: "user".to_string(),
                content: "Hello".to_string(),
                extra: IndexMap::new(),
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
        use crate::model::{UserContent, UserMessage, UserSimpleContent};
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
            is_compact_summary: None,
            is_visible_in_transcript_only: None,
            thinking_metadata: None,
            todos: Vec::new(),
            tool_use_result: None,
            message: UserContent::Simple(UserSimpleContent {
                role: "user".to_string(),
                content: "Hello".to_string(),
                extra: IndexMap::new(),
            }),
            extra: IndexMap::new(),
        };
        let entry = LogEntry::User(msg);

        // Should not match when no branch is present
        assert!(!matches_git_branch(&entry, "main"));
    }

    #[test]
    fn test_matches_model_semantics() {
        // Regression for #23: --model is an entry-level assistant-message filter.
        // Assistant entries match by substring; non-assistant entries never match
        // while a model filter is active (they carry no model), so `--files-only`
        // returns a session only via a matching assistant entry.
        use crate::model::content::AssistantContent;
        use crate::model::message::AssistantMessage;
        use crate::model::{UserContent, UserMessage, UserSimpleContent};
        use chrono::Utc;
        use indexmap::IndexMap;

        let assistant = LogEntry::Assistant(AssistantMessage {
            uuid: "a".to_string(),
            parent_uuid: None,
            timestamp: Utc::now(),
            session_id: "s".to_string(),
            version: "2.0".to_string(),
            cwd: None,
            git_branch: None,
            user_type: None,
            is_sidechain: false,
            is_teammate: None,
            agent_id: None,
            slug: None,
            request_id: None,
            is_api_error_message: None,
            message: AssistantContent {
                model: "claude-opus-4-8".to_string(),
                ..Default::default()
            },
            extra: IndexMap::new(),
        });

        // Substring match, case-insensitive.
        assert!(matches_model(&assistant, "opus"));
        assert!(matches_model(&assistant, "claude-opus-4-8"));
        assert!(matches_model(&assistant, "OPUS"));
        // Non-matching model excludes the assistant entry (was the silent no-op).
        assert!(!matches_model(&assistant, "fable"));
        assert!(!matches_model(&assistant, "definitely-not-a-model"));

        // Non-assistant entries never match while a model filter is active.
        let user = LogEntry::User(UserMessage {
            uuid: "u".to_string(),
            parent_uuid: None,
            timestamp: Utc::now(),
            session_id: "s".to_string(),
            version: "2.0".to_string(),
            cwd: None,
            git_branch: None,
            user_type: None,
            is_sidechain: false,
            is_teammate: None,
            agent_id: None,
            slug: None,
            is_meta: None,
            is_compact_summary: None,
            is_visible_in_transcript_only: None,
            thinking_metadata: None,
            todos: Vec::new(),
            tool_use_result: None,
            message: UserContent::Simple(UserSimpleContent {
                role: "user".to_string(),
                content: "the quick brown fox".to_string(),
                extra: IndexMap::new(),
            }),
            extra: IndexMap::new(),
        });
        assert!(!matches_model(&user, "opus"));
        assert!(!matches_model(&user, "claude-opus-4-8"));
    }

    #[test]
    fn test_exclude_sees_tool_and_thinking_content() {
        // Regression for #27: --exclude must be evaluated over the same content
        // a search inspects. Tool-use/tool-result JSON (Tools and All scopes) and
        // thinking blocks (under --all) are searched/counted, but were invisible
        // to extract_text_for_scope, so exclude could not suppress them. Both the
        // single-pattern and batch exclude paths now route through
        // entry_matches_in_scope, so exercising it covers both.
        use crate::model::content::{
            AssistantContent, TextBlock, ThinkingBlock, ToolResult, ToolResultContent, ToolUse,
        };
        use crate::model::message::AssistantMessage;
        use chrono::Utc;
        use indexmap::IndexMap;
        use serde_json::json;

        let entry = LogEntry::Assistant(AssistantMessage {
            uuid: "a".to_string(),
            parent_uuid: None,
            timestamp: Utc::now(),
            session_id: "s".to_string(),
            version: "2.0".to_string(),
            cwd: None,
            git_branch: None,
            user_type: None,
            is_sidechain: false,
            is_teammate: None,
            agent_id: None,
            slug: None,
            request_id: None,
            is_api_error_message: None,
            message: AssistantContent {
                model: "claude-opus-4-8".to_string(),
                content: vec![
                    ContentBlock::Text(TextBlock {
                        text: "hello world".to_string(),
                        extra: IndexMap::new(),
                    }),
                    ContentBlock::Thinking(ThinkingBlock {
                        thinking: "let me reconsider the approach".to_string(),
                        signature: String::new(),
                        extra: IndexMap::new(),
                    }),
                    ContentBlock::ToolUse(ToolUse {
                        id: "t1".to_string(),
                        name: "Bash".to_string(),
                        input: json!({ "command": "ls /tmp" }),
                        extra: IndexMap::new(),
                    }),
                    ContentBlock::ToolResult(ToolResult {
                        tool_use_id: "t1".to_string(),
                        content: Some(ToolResultContent::String("file: README.md".to_string())),
                        is_error: None,
                        extra: IndexMap::new(),
                    }),
                ],
                ..Default::default()
            },
            extra: IndexMap::new(),
        });

        let hits = |pat: &str, scope: BatchScope| {
            entry_matches_in_scope(&entry, &Regex::new(pat).unwrap(), &scope)
        };

        // The bug: exclude can now see tool content (Tools and All) ...
        assert!(hits("ls", BatchScope::Tools), "tool-use input, Tools scope");
        assert!(
            hits("README", BatchScope::Tools),
            "tool-result, Tools scope"
        );
        assert!(hits("ls", BatchScope::All), "tool-use input, All scope");
        // ... and thinking under --all (and the thinking scopes).
        assert!(hits("reconsider", BatchScope::All), "thinking under --all");
        assert!(hits("reconsider", BatchScope::Thinking), "thinking scope");
        // Plain text still works under Default/All.
        assert!(hits("hello", BatchScope::Default));
        assert!(hits("hello", BatchScope::All));

        // Scope is not over-broadened: Tools sees neither text nor thinking, and
        // Default sees neither tool content nor thinking.
        assert!(!hits("hello", BatchScope::Tools));
        assert!(!hits("reconsider", BatchScope::Tools));
        assert!(!hits("ls", BatchScope::Default));
        assert!(!hits("reconsider", BatchScope::Default));
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
