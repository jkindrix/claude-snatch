//! Interactive session picker command.
//!
//! Provides a fuzzy-searchable interface for selecting sessions.

use std::collections::BTreeSet;
use std::fmt::Write as _;

use dialoguer::{theme::ColorfulTheme, FuzzySelect};

use crate::cli::{Cli, PickAction, PickArgs};
use crate::discovery::SessionFilter;
use crate::error::{Result, SnatchError};
use crate::provider::{LineageEdgeKind, LogicalSessionKey};
use crate::util::truncate_path;

use super::get_claude_dir;

#[derive(Debug, Clone)]
struct ProviderPickCandidate {
    key: LogicalSessionKey,
    modified_at: Option<chrono::DateTime<chrono::Utc>>,
    project: String,
    spawned: bool,
    artifact_bytes: Option<u64>,
    artifact_count: usize,
}

struct ProviderPickCollection {
    candidates: Vec<ProviderPickCandidate>,
    skipped: Vec<(crate::provider::ProviderId, String)>,
    warnings: Vec<String>,
}

fn collect_provider_candidates(
    registry: &crate::provider::registry::ProviderRegistry,
    selection: &crate::provider::registry::ProviderSelection,
    project_filter: Option<&str>,
    include_subagents: bool,
    limit: usize,
) -> Result<ProviderPickCollection> {
    let collected = registry.collect_project_union(selection)?;
    let spawned: BTreeSet<_> = collected
        .lineage
        .iter()
        .filter(|edge| matches!(edge.kind, LineageEdgeKind::Spawn { .. }))
        .map(|edge| edge.to.clone())
        .collect();
    let mut candidates = Vec::new();
    for project in &collected.projects {
        if project_filter.is_some_and(|filter| !project.matches(filter)) {
            continue;
        }
        let project_path = project
            .display_path
            .clone()
            .unwrap_or_else(|| project.identity.to_string());
        for session in &project.sessions {
            let is_spawned = spawned.contains(&session.descriptor.key);
            if is_spawned && !include_subagents {
                continue;
            }
            candidates.push(ProviderPickCandidate {
                key: session.descriptor.key.clone(),
                modified_at: session
                    .context
                    .modified_at
                    .or(session.context.ended_at)
                    .or(session.context.started_at),
                project: session
                    .context
                    .cwd
                    .clone()
                    .unwrap_or_else(|| project_path.clone()),
                spawned: is_spawned,
                artifact_bytes: (session.context.artifact_bytes > 0)
                    .then_some(session.context.artifact_bytes),
                artifact_count: session.descriptor.artifacts.len(),
            });
        }
    }
    candidates.sort_by(|a, b| {
        b.modified_at
            .cmp(&a.modified_at)
            .then_with(|| a.key.cmp(&b.key))
    });
    candidates.truncate(limit);
    Ok(ProviderPickCollection {
        candidates,
        skipped: collected.skipped,
        warnings: collected
            .context_warnings
            .into_iter()
            .map(|warning| format!("{}: project context unavailable", warning.key))
            .collect(),
    })
}

fn format_provider_session_item(candidate: &ProviderPickCandidate, show_project: bool) -> String {
    let mut line = String::new();
    write!(line, "{}", candidate.key).unwrap();
    if let Some(modified) = candidate.modified_at {
        write!(line, " | {}", modified.format("%Y-%m-%d %H:%M")).unwrap();
    }
    if show_project {
        write!(line, " | {}", truncate_path(&candidate.project, 40)).unwrap();
    }
    if candidate.spawned {
        write!(line, " [subagent]").unwrap();
    }
    if let Some(size) = candidate.artifact_bytes {
        let size = if size >= 1024 * 1024 {
            format!("{:.1}MB", size as f64 / (1024.0 * 1024.0))
        } else if size >= 1024 {
            format!("{:.1}KB", size as f64 / 1024.0)
        } else {
            format!("{size}B")
        };
        let label = if candidate.artifact_count == 1 {
            "artifact"
        } else {
            "artifacts"
        };
        write!(line, " ({size}, {} {label})", candidate.artifact_count).unwrap();
    } else {
        let label = if candidate.artifact_count == 1 {
            "artifact"
        } else {
            "artifacts"
        };
        write!(line, " ({} {label})", candidate.artifact_count).unwrap();
    }
    line
}

/// Format a session for display in the picker.
fn format_session_item(session: &crate::discovery::Session, show_project: bool) -> String {
    let mut line = String::new();

    // Session ID (short)
    let short_id = &session.session_id()[..8.min(session.session_id().len())];
    write!(line, "{short_id}").unwrap();

    // Modified time
    if let Ok(modified) = session
        .modified_time()
        .duration_since(std::time::UNIX_EPOCH)
    {
        let secs = modified.as_secs();
        let dt = chrono::DateTime::from_timestamp(secs as i64, 0);
        if let Some(dt) = dt {
            write!(line, " | {}", dt.format("%Y-%m-%d %H:%M")).unwrap();
        }
    }

    // Project path (shortened)
    if show_project {
        let project = session.project_path();
        let short_project = truncate_path(project, 40);
        write!(line, " | {short_project}").unwrap();
    }

    // Subagent indicator
    if session.is_subagent() {
        write!(line, " [subagent]").unwrap();
    }

    // File size
    let size = session.file_size();
    let size_str = if size >= 1024 * 1024 {
        format!("{:.1}MB", size as f64 / (1024.0 * 1024.0))
    } else if size >= 1024 {
        format!("{:.1}KB", size as f64 / 1024.0)
    } else {
        format!("{size}B")
    };
    write!(line, " ({size_str})").unwrap();

    line
}

/// Run the pick command.
pub fn run(cli: &Cli, args: &PickArgs) -> Result<()> {
    if !args.provider.is_empty() {
        return run_provider(cli, args);
    }
    let claude_dir = get_claude_dir(cli.claude_dir.as_ref())?;

    // Build session filter
    let mut filter = SessionFilter::new();

    // By default exclude subagents unless explicitly included
    if !args.subagents {
        filter = filter.main_only();
    }

    // Get all sessions
    let all_sessions = claude_dir.all_sessions()?;

    // Filter and sort sessions
    let mut sessions: Vec<_> = all_sessions
        .iter()
        .filter(|s| {
            // Apply project filter
            if let Some(ref project) = args.project {
                if !s.project_path().contains(project) {
                    return false;
                }
            }

            // Apply session filter
            filter.matches(s).unwrap_or_default()
        })
        .collect();

    if sessions.is_empty() {
        return Err(SnatchError::ConfigError {
            message: "No sessions found matching the specified filters".to_string(),
        });
    }

    // Sort by modification time (newest first)
    sessions.sort_by_key(|s| std::cmp::Reverse(s.modified_time()));

    // Limit the number of sessions in the picker
    let limit = args.limit.unwrap_or(100);
    if sessions.len() > limit {
        sessions.truncate(limit);
    }

    // Show project paths if there are multiple projects
    let unique_projects: std::collections::HashSet<_> =
        sessions.iter().map(|s| s.project_path()).collect();
    let show_project = unique_projects.len() > 1;

    // Create display items
    let items: Vec<String> = sessions
        .iter()
        .map(|s| format_session_item(s, show_project))
        .collect();

    // Show the fuzzy selector
    let selection = FuzzySelect::with_theme(&ColorfulTheme::default())
        .with_prompt("Select a session (type to filter)")
        .items(&items)
        .default(0)
        .interact_opt()
        .map_err(|e| SnatchError::ConfigError {
            message: format!("Failed to show interactive picker: {e}"),
        })?;

    let Some(idx) = selection else {
        // User cancelled
        if !cli.quiet {
            eprintln!("Selection cancelled.");
        }
        return Ok(());
    };

    let selected_session = sessions[idx];
    let session_id = selected_session.session_id();

    // Perform the requested action
    match args.action {
        PickAction::Export => {
            // Just print the session ID for piping
            println!("{session_id}");
            if !cli.quiet {
                eprintln!("Selected session: {session_id}");
                eprintln!("Tip: Use `snatch export {session_id}` to export this session.");
            }
        }
        PickAction::Info => {
            // Run info command
            let info_args = crate::cli::InfoArgs {
                target: Some(session_id.to_string()),
                provider: Vec::new(),
                no_chain: false,
                tree: false,
                raw: false,
                entry: None,
                paths: false,
                messages: None,
                files: false,
            };
            crate::cli::commands::info::run(cli, &info_args)?;
        }
        PickAction::Stats => {
            // Run stats command
            let stats_args = crate::cli::StatsArgs {
                project: None,
                session: Some(session_id.to_string()),
                provider: Vec::new(),
                global: false,
                tools: true,
                models: false,
                costs: false,
                blocks: false,
                token_limit: None,
                all: false,
                sparkline: false,
                history: false,
                days: 30,
                record: false,
                weekly: false,
                monthly: false,
                csv: false,
                clear_history: false,
                timeline: false,
                granularity: "daily".to_string(),
                graph: false,
                graph_width: 60,
            };
            crate::cli::commands::stats::run(cli, &stats_args)?;
        }
        PickAction::Open => {
            // Print session file path
            println!("{}", selected_session.path().display());
            if !cli.quiet {
                eprintln!("Session file: {}", selected_session.path().display());
            }
        }
    }

    Ok(())
}

fn run_provider(cli: &Cli, args: &PickArgs) -> Result<()> {
    use crate::provider::registry::ProviderSelection;

    let selection = ProviderSelection::from_flags(&args.provider).map_err(|reason| {
        SnatchError::InvalidArgument {
            name: "--provider".to_string(),
            reason,
        }
    })?;
    let registry = super::helpers::provider_registry(cli);
    registry.select(&selection)?;
    if args.action == PickAction::Open {
        return Err(SnatchError::InvalidArgument {
            name: "--action open".to_string(),
            reason: "provider-routed sessions do not promise a local source path; omit --provider for the classic filesystem picker, or export the native/archive tier"
                .to_string(),
        });
    }
    let mut collection = collect_provider_candidates(
        &registry,
        &selection,
        args.project.as_deref(),
        args.subagents,
        args.limit.unwrap_or(100),
    )?;
    if collection.candidates.is_empty() {
        return Err(SnatchError::ConfigError {
            message: "No provider sessions found matching the specified filters".to_string(),
        });
    }
    collection.warnings.sort();
    collection.warnings.dedup();
    for (provider, _) in &collection.skipped {
        eprintln!("warning: provider '{provider}' unavailable");
    }
    for warning in &collection.warnings {
        eprintln!("warning: {warning}");
    }

    let show_project = collection
        .candidates
        .iter()
        .map(|candidate| candidate.project.as_str())
        .collect::<BTreeSet<_>>()
        .len()
        > 1;
    let items: Vec<_> = collection
        .candidates
        .iter()
        .map(|candidate| format_provider_session_item(candidate, show_project))
        .collect();
    let selection = FuzzySelect::with_theme(&ColorfulTheme::default())
        .with_prompt("Select a provider session (type to filter)")
        .items(&items)
        .default(0)
        .interact_opt()
        .map_err(|error| SnatchError::ConfigError {
            message: format!("Failed to show interactive picker: {error}"),
        })?;
    let Some(index) = selection else {
        if !cli.quiet {
            eprintln!("Selection cancelled.");
        }
        return Ok(());
    };
    let selected = &collection.candidates[index];
    let qualified_id = selected.key.to_string();
    let provider_flag = vec![selected.key.provider.to_string()];
    match args.action {
        PickAction::Export => {
            println!("{qualified_id}");
            if !cli.quiet {
                eprintln!("Selected session: {qualified_id}");
                eprintln!("Tip: Use `snatch export '{qualified_id}'` to export this session.");
            }
        }
        PickAction::Info => {
            crate::cli::commands::info::run(
                cli,
                &crate::cli::InfoArgs {
                    target: Some(qualified_id),
                    provider: provider_flag,
                    no_chain: false,
                    tree: false,
                    raw: false,
                    entry: None,
                    paths: false,
                    messages: None,
                    files: false,
                },
            )?;
        }
        PickAction::Stats => {
            crate::cli::commands::stats::run(
                cli,
                &crate::cli::StatsArgs {
                    project: None,
                    session: Some(qualified_id),
                    provider: provider_flag,
                    global: false,
                    tools: true,
                    models: false,
                    costs: false,
                    blocks: false,
                    token_limit: None,
                    all: false,
                    sparkline: false,
                    history: false,
                    days: 30,
                    record: false,
                    weekly: false,
                    monthly: false,
                    csv: false,
                    clear_history: false,
                    timeline: false,
                    granularity: "daily".to_string(),
                    graph: false,
                    graph_width: 60,
                },
            )?;
        }
        PickAction::Open => unreachable!("provider open is rejected before discovery"),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_format_session_item_short() {
        // Basic test - just ensure formatting doesn't panic
        let formatted = "12345678 | 2024-12-24 10:00 | /project (1.0KB)";
        assert!(formatted.contains("12345678"));
    }

    #[test]
    fn provider_candidates_are_qualified_deterministic_and_globally_limited() {
        let mut registry = crate::provider::registry::ProviderRegistry::new();
        registry
            .register(crate::provider::registry::RegisteredProvider {
                id: crate::provider::ProviderId("fake".to_string()),
                root: None,
                provider: Ok(Box::new(crate::provider::fake::FakeProvider)),
            })
            .unwrap();
        let selection =
            crate::provider::registry::ProviderSelection::from_flags(&["fake".to_string()])
                .unwrap();
        let all = collect_provider_candidates(&registry, &selection, None, false, 10).unwrap();
        assert_eq!(all.candidates.len(), 2);
        assert!(all
            .candidates
            .windows(2)
            .all(|pair| pair[0].key < pair[1].key));
        assert!(all
            .candidates
            .iter()
            .all(|candidate| candidate.key.to_string().starts_with("fake:")));
        let labels: BTreeSet<_> = all
            .candidates
            .iter()
            .map(|candidate| format_provider_session_item(candidate, false))
            .collect();
        assert_eq!(labels.len(), 2, "namespace collisions must stay visible");
        let limited = collect_provider_candidates(&registry, &selection, None, false, 1).unwrap();
        assert_eq!(limited.candidates.len(), 1);
        assert_eq!(limited.candidates[0].key, all.candidates[0].key);
    }

    #[test]
    fn provider_picker_display_is_unicode_safe() {
        let candidate = ProviderPickCandidate {
            key: LogicalSessionKey {
                provider: crate::provider::ProviderId("fake".to_string()),
                namespace: crate::provider::SessionNamespace::global(),
                native_id: "ééééééééééééé".to_string(),
            },
            modified_at: None,
            project: "/work/example".to_string(),
            spawned: false,
            artifact_bytes: Some(12),
            artifact_count: 2,
        };
        let rendered = format_provider_session_item(&candidate, true);
        assert!(rendered.starts_with(&candidate.key.to_string()));
        assert!(rendered.contains("2 artifacts"));
    }
}
