//! Provider-neutral persistent search-index commands.

use std::collections::BTreeSet;
use std::path::PathBuf;

use crate::analysis::search::{ExactSearchMatcher, SearchScope};
use crate::cli::{Cli, IndexArgs, IndexSubcommand, OutputFormat};
use crate::config::Config;
use crate::discovery::{format_count, format_number, format_size};
use crate::error::{Result, SnatchError};
use crate::index::build::{
    rebuild_provider_index, update_provider_index, ProviderIndexBuildOptions,
};
use crate::index::provider::{
    ProviderIndexStats, ProviderSearchIndex, PROVIDER_INDEX_SCHEMA_VERSION,
};
use crate::index::query::{
    IndexedProviderSelection, IndexedSearchFilters, IndexedSearchOrder, IndexedSearchRequest,
};
use crate::provider::registry::ProviderSelection;
use crate::provider::{LogicalSessionKey, ProviderId};

/// Run the index command.
pub fn run(cli: &Cli, args: &IndexArgs) -> Result<()> {
    match &args.command {
        IndexSubcommand::Build(build_args) => run_build(cli, build_args),
        IndexSubcommand::Rebuild(rebuild_args) => run_rebuild(cli, rebuild_args),
        IndexSubcommand::Status => run_status(cli),
        IndexSubcommand::Clear => run_clear(cli),
        IndexSubcommand::Search(search_args) => run_search(cli, search_args),
    }
}

pub(super) fn index_path(cli: &Cli) -> PathBuf {
    let config = match &cli.config {
        Some(path) => Config::load_from(path).unwrap_or_default(),
        None => Config::load().unwrap_or_default(),
    };
    config
        .index
        .directory
        .unwrap_or_else(ProviderSearchIndex::default_index_dir)
}

pub(super) fn provider_selection(flags: &[String]) -> Result<ProviderSelection> {
    if flags.is_empty() {
        return Ok(ProviderSelection::Explicit(vec![ProviderId::claude_code()]));
    }
    ProviderSelection::from_flags(flags).map_err(|reason| SnatchError::InvalidArgument {
        name: "provider".to_string(),
        reason,
    })
}

pub(super) fn indexed_selection(selection: &ProviderSelection) -> IndexedProviderSelection {
    match selection {
        ProviderSelection::All => IndexedProviderSelection::All,
        ProviderSelection::Explicit(ids) => {
            IndexedProviderSelection::Explicit(ids.iter().map(ToString::to_string).collect())
        }
    }
}

pub(super) fn selected_provider_names(selection: &ProviderSelection) -> Option<BTreeSet<String>> {
    match selection {
        ProviderSelection::All => None,
        ProviderSelection::Explicit(ids) => Some(ids.iter().map(ToString::to_string).collect()),
    }
}

pub(super) fn resolve_indexed_session(
    index: &ProviderSearchIndex,
    selection: &ProviderSelection,
    reference: &str,
) -> Result<String> {
    let selected = selected_provider_names(selection);
    let manifests = index.session_manifests()?;
    let qualified = reference.contains(':');
    let requested_key = if qualified {
        Some(
            reference
                .parse::<LogicalSessionKey>()
                .map_err(|reason: String| SnatchError::InvalidArgument {
                    name: "session".to_string(),
                    reason,
                })?,
        )
    } else {
        None
    };
    if let (Some(selected), Some(key)) = (&selected, &requested_key) {
        if !selected.contains(&key.provider.to_string()) {
            return Err(SnatchError::InvalidArgument {
                name: "session".to_string(),
                reason: format!(
                    "qualified session {reference} belongs to unselected provider {}",
                    key.provider
                ),
            });
        }
    }

    let mut candidates: Vec<LogicalSessionKey> = manifests
        .into_iter()
        .filter_map(|manifest| manifest.session_key.parse().ok())
        .filter(|key: &LogicalSessionKey| match &selected {
            Some(providers) => providers.contains(&key.provider.to_string()),
            None => true,
        })
        .filter(|key| {
            requested_key.as_ref().map_or_else(
                || key.native_id.starts_with(reference),
                |requested| {
                    key.provider == requested.provider
                        && key.namespace == requested.namespace
                        && key.native_id.starts_with(&requested.native_id)
                },
            )
        })
        .collect();
    let exact_native = requested_key
        .as_ref()
        .map_or(reference, |key| &key.native_id);
    let exact: Vec<_> = candidates
        .iter()
        .filter(|key| key.native_id == exact_native)
        .cloned()
        .collect();
    if !exact.is_empty() {
        candidates = exact;
    }
    candidates.sort();
    candidates.dedup();
    match candidates.as_slice() {
        [] => Err(SnatchError::SessionNotFound {
            session_id: reference.to_string(),
        }),
        [key] => Ok(key.to_string()),
        many => Err(SnatchError::InvalidArgument {
            name: "session".to_string(),
            reason: format!(
                "ambiguous indexed session reference '{reference}'; candidates: {}",
                many.iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        }),
    }
}

fn run_build(cli: &Cli, args: &crate::cli::IndexBuildArgs) -> Result<()> {
    let selection = provider_selection(&args.provider)?;
    let registry = super::helpers::provider_registry(cli);
    let index = ProviderSearchIndex::open(index_path(cli))?;
    let options = ProviderIndexBuildOptions::new(&selection, args.project.as_deref());
    let report = update_provider_index(&index, &registry, &options)?;

    match cli.effective_output() {
        OutputFormat::Json => println!("{}", serde_json::to_string_pretty(&report)?),
        _ => {
            println!(
                "Indexed {} entries from {} changed sessions ({} unchanged, {} removed)",
                format_count(report.entries_replaced),
                format_count(report.sessions_replaced),
                format_count(report.sessions_unchanged),
                format_count(report.sessions_removed)
            );
            println!("Generation: {}", report.generation);
            if report.skipped > 0 || report.warnings > 0 || !report.removal_coverage_complete {
                println!(
                    "Coverage: {} skipped, {} warnings{}",
                    format_count(report.skipped),
                    format_count(report.warnings),
                    if report.removal_coverage_complete {
                        ""
                    } else {
                        " (removal coverage incomplete)"
                    }
                );
            }
        }
    }
    Ok(())
}

fn run_rebuild(cli: &Cli, args: &crate::cli::IndexRebuildArgs) -> Result<()> {
    if args.project.is_some() {
        return Err(SnatchError::InvalidArgument {
            name: "project".to_string(),
            reason: "provider-index rebuild requires complete provider coverage; use 'index build --project' for an upsert-only filtered update".to_string(),
        });
    }
    let selection = provider_selection(&args.provider)?;
    let registry = super::helpers::provider_registry(cli);
    let options = ProviderIndexBuildOptions::new(&selection, None);
    let report = rebuild_provider_index(index_path(cli), &registry, &options)?;

    match cli.effective_output() {
        OutputFormat::Json => println!("{}", serde_json::to_string_pretty(&report)?),
        _ => {
            println!(
                "Rebuilt index with {} entries from {} sessions",
                format_count(report.build.entries_replaced),
                format_count(report.build.sessions_replaced)
            );
            println!("Generation: {}", report.build.generation);
            if let Some(backup) = report.retained_backup {
                println!("Previous index retained at: {}", backup.display());
            }
        }
    }
    Ok(())
}

fn run_status(cli: &Cli) -> Result<()> {
    let path = index_path(cli);
    let stats = if path.try_exists().map_err(|error| {
        SnatchError::io(
            format!("failed to inspect provider index path: {}", path.display()),
            error,
        )
    })? {
        ProviderSearchIndex::open_read_only(&path)?.stats()?
    } else {
        ProviderIndexStats {
            schema_version: PROVIDER_INDEX_SCHEMA_VERSION,
            document_count: 0,
            session_count: 0,
            entry_count: 0,
            size_bytes: 0,
            build: None,
        }
    };
    match cli.effective_output() {
        OutputFormat::Json => println!("{}", serde_json::to_string_pretty(&stats)?),
        _ => {
            println!("Search Index Status");
            println!("===================");
            println!("Path: {}", path.display());
            println!("Schema: {}", stats.schema_version);
            println!("Documents: {}", format_number(stats.document_count));
            println!("Sessions: {}", format_count(stats.session_count));
            println!("Entries: {}", format_count(stats.entry_count));
            println!("Size: {}", format_size(stats.size_bytes));
            if let Some(build) = stats.build {
                println!("Generation: {}", build.generation);
                println!("Built: {}", build.built_at.to_rfc3339());
                println!(
                    "Complete providers: {}",
                    if build.complete_providers.is_empty() {
                        "none".to_string()
                    } else {
                        build.complete_providers.join(", ")
                    }
                );
                if !build.removal_coverage_complete || !build.skipped.is_empty() {
                    println!("Coverage: incomplete");
                }
            } else {
                println!();
                println!("Index is empty. Run 'snatch index build' to create it.");
            }
        }
    }
    Ok(())
}

fn run_clear(cli: &Cli) -> Result<()> {
    let index = ProviderSearchIndex::open(index_path(cli))?;
    index.clear()?;
    match cli.effective_output() {
        OutputFormat::Json => println!(r#"{{"status": "cleared"}}"#),
        _ => println!("Search index cleared."),
    }
    Ok(())
}

fn run_search(cli: &Cli, args: &crate::cli::IndexSearchArgs) -> Result<()> {
    if args.query.trim().is_empty() {
        return Err(SnatchError::InvalidArgument {
            name: "query".to_string(),
            reason: "search query cannot be empty or whitespace-only".to_string(),
        });
    }
    let selection = provider_selection(&args.provider)?;
    let index = ProviderSearchIndex::open_read_only(index_path(cli))?;
    let session_keys = match &args.session {
        Some(reference) => vec![resolve_indexed_session(&index, &selection, reference)?],
        None => Vec::new(),
    };
    let matcher = if args.fuzzy {
        ExactSearchMatcher::fuzzy(&args.query, args.ignore_case, args.fuzzy_threshold)
    } else {
        ExactSearchMatcher::regex(&args.query, args.ignore_case).map_err(|error| {
            SnatchError::InvalidArgument {
                name: "query".to_string(),
                reason: error.to_string(),
            }
        })?
    };
    let exclude = args
        .exclude
        .as_deref()
        .map(|pattern| ExactSearchMatcher::regex(pattern, args.ignore_case))
        .transpose()
        .map_err(|error| SnatchError::InvalidArgument {
            name: "exclude".to_string(),
            reason: error.to_string(),
        })?;
    let request = IndexedSearchRequest {
        selection: indexed_selection(&selection),
        matcher,
        exclude,
        scope: if args.thinking {
            SearchScope::Thinking
        } else {
            SearchScope::Default
        },
        filters: IndexedSearchFilters {
            session_keys,
            message_types: args.message_type.iter().cloned().collect(),
            model_contains: args.model.clone(),
            tool_name_contains: args.tool_name.clone(),
            include_spawned: true,
            ..Default::default()
        },
        context_lines: args.context,
        order: if args.sort {
            IndexedSearchOrder::Relevance
        } else {
            IndexedSearchOrder::Source
        },
        offset: args.offset,
        limit: args.limit.unwrap_or(100),
    };
    let response = index.query(&request)?;
    output_search_response(cli, &response)
}

pub(super) fn output_search_response(
    cli: &Cli,
    response: &crate::index::query::IndexedSearchResponse,
) -> Result<()> {
    match cli.effective_output() {
        OutputFormat::Json => println!("{}", serde_json::to_string_pretty(response)?),
        OutputFormat::Tsv => {
            println!("provider\tsession\tproject\tentry\ttype\tlocation\tscore\tline");
            for hit in &response.matches {
                println!(
                    "{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}",
                    hit.provider,
                    hit.session_key,
                    hit.project_path.replace('\t', " "),
                    hit.entry_id,
                    hit.message_type,
                    hit.location,
                    hit.score,
                    hit.line.replace(['\t', '\n'], " ")
                );
            }
        }
        OutputFormat::Compact => {
            for hit in &response.matches {
                println!(
                    "{}:{}:{}: {}",
                    hit.session_key, hit.message_type, hit.location, hit.matched_text
                );
            }
        }
        OutputFormat::Text => {
            if response.matches.is_empty() {
                println!("No matches found.");
            } else {
                println!(
                    "Found {} matches ({} occurrences across {} sessions); showing {}:",
                    format_count(response.total_matches),
                    format_count(response.total_occurrences),
                    format_count(response.sessions_matched),
                    format_count(response.returned)
                );
                let mut current = "";
                for hit in &response.matches {
                    if hit.session_key != current {
                        current = &hit.session_key;
                        println!();
                        println!("Session: {} ({})", hit.session_key, hit.project_path);
                    }
                    println!();
                    println!(
                        "  [{} - score: {}] {}",
                        hit.message_type, hit.score, hit.location
                    );
                    if !hit.context_before.is_empty() {
                        println!("    {}", hit.context_before.replace('\n', "\n    "));
                    }
                    println!("  > {}", hit.line);
                    if !hit.context_after.is_empty() {
                        println!("    {}", hit.context_after.replace('\n', "\n    "));
                    }
                }
            }
        }
    }
    if cli.effective_output() != OutputFormat::Json && response.coverage.incomplete && !cli.quiet {
        eprintln!(
            "Warning: indexed coverage is incomplete for generation {}",
            response.coverage.generation
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provider_selection_defaults_to_claude_and_rejects_mixed_all() {
        assert_eq!(
            provider_selection(&[]).unwrap(),
            ProviderSelection::Explicit(vec![ProviderId::claude_code()])
        );
        assert!(provider_selection(&["all".into(), "claude-code".into()]).is_err());
    }
}
