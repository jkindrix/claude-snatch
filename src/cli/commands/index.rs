//! Index command implementation.
//!
//! Manages the full-text search index for fast searching across sessions.

use crate::cli::{Cli, IndexArgs, IndexSubcommand, OutputFormat};
use crate::error::Result;
use crate::index::SearchIndex;

use super::get_claude_dir;

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

/// Build or update the search index.
fn run_build(cli: &Cli, args: &crate::cli::IndexBuildArgs) -> Result<()> {
    let claude_dir = get_claude_dir(cli.claude_dir.as_ref())?;
    let index = SearchIndex::open_default()?;

    // Get sessions to index
    let sessions = if let Some(ref project_filter) = args.project {
        let projects = claude_dir.projects()?;
        let mut sessions = Vec::new();
        for project in projects {
            if project.decoded_path().contains(project_filter) {
                sessions.extend(project.sessions()?);
            }
        }
        sessions
    } else {
        claude_dir.all_sessions()?
    };

    if cli.verbose {
        eprintln!("Indexing {} sessions...", sessions.len());
    }

    let result = index.index_sessions(&sessions)?;
    index.commit()?;

    match cli.effective_output() {
        OutputFormat::Json => {
            println!("{}", serde_json::to_string_pretty(&result)?);
        }
        _ => {
            println!(
                "Indexed {} documents from {} sessions",
                result.documents_indexed, result.sessions_indexed
            );
            if !result.errors.is_empty() {
                println!("Errors: {}", result.errors.len());
                if cli.verbose {
                    for (session, error) in &result.errors {
                        eprintln!("  {}: {}", &session[..8.min(session.len())], error);
                    }
                }
            }
        }
    }

    Ok(())
}

/// Rebuild the index from scratch.
fn run_rebuild(cli: &Cli, args: &crate::cli::IndexRebuildArgs) -> Result<()> {
    let claude_dir = get_claude_dir(cli.claude_dir.as_ref())?;
    let index = SearchIndex::open_default()?;

    // Clear existing index
    if cli.verbose {
        eprintln!("Clearing existing index...");
    }
    index.clear()?;

    // Get all sessions
    let sessions = if let Some(ref project_filter) = args.project {
        let projects = claude_dir.projects()?;
        let mut sessions = Vec::new();
        for project in projects {
            if project.decoded_path().contains(project_filter) {
                sessions.extend(project.sessions()?);
            }
        }
        sessions
    } else {
        claude_dir.all_sessions()?
    };

    if cli.verbose {
        eprintln!("Rebuilding index from {} sessions...", sessions.len());
    }

    let result = index.index_sessions(&sessions)?;
    index.commit()?;

    match cli.effective_output() {
        OutputFormat::Json => {
            println!("{}", serde_json::to_string_pretty(&result)?);
        }
        _ => {
            println!(
                "Rebuilt index: {} documents from {} sessions",
                result.documents_indexed, result.sessions_indexed
            );
            if !result.errors.is_empty() {
                println!("Errors: {}", result.errors.len());
                if cli.verbose {
                    for (session, error) in &result.errors {
                        eprintln!("  {}: {}", &session[..8.min(session.len())], error);
                    }
                }
            }
        }
    }

    Ok(())
}

/// Show index status.
fn run_status(cli: &Cli) -> Result<()> {
    let index = SearchIndex::open_default()?;
    let stats = index.stats()?;

    match cli.effective_output() {
        OutputFormat::Json => {
            println!("{}", serde_json::to_string_pretty(&stats)?);
        }
        _ => {
            println!("Search Index Status");
            println!("==================");
            println!("Path: {}", index.path().display());
            println!("Documents: {}", stats.document_count);
            println!(
                "Size: {} KB",
                stats.size_bytes / 1024
            );
            if stats.document_count == 0 {
                println!();
                println!("Index is empty. Run 'snatch index build' to create it.");
            }
        }
    }

    Ok(())
}

/// Clear the search index.
fn run_clear(cli: &Cli) -> Result<()> {
    let index = SearchIndex::open_default()?;
    index.clear()?;

    match cli.effective_output() {
        OutputFormat::Json => {
            println!(r#"{{"status": "cleared"}}"#);
        }
        _ => {
            println!("Search index cleared.");
        }
    }

    Ok(())
}

/// Search the index.
fn run_search(cli: &Cli, args: &crate::cli::IndexSearchArgs) -> Result<()> {
    let index = SearchIndex::open_default()?;

    if index.is_empty() {
        eprintln!("Index is empty. Run 'snatch index build' first.");
        return Ok(());
    }

    let options = crate::index::SearchOptions {
        query: args.query.clone(),
        message_type: args.message_type.clone(),
        model: args.model.clone(),
        session_id: args.session.clone(),
        tool_name: args.tool_name.clone(),
        include_thinking: args.thinking,
        limit: Some(args.limit.unwrap_or(100)),
    };

    let results = index.search_advanced(&options)?;

    match cli.effective_output() {
        OutputFormat::Json => {
            println!("{}", serde_json::to_string_pretty(&results)?);
        }
        OutputFormat::Tsv => {
            println!("session\tproject\tuuid\ttype\tscore\tsnippet");
            for hit in &results {
                println!(
                    "{}\t{}\t{}\t{}\t{:.3}\t{}",
                    &hit.session_id[..8.min(hit.session_id.len())],
                    hit.project,
                    &hit.uuid[..8.min(hit.uuid.len())],
                    hit.message_type,
                    hit.score,
                    hit.content_snippet
                        .chars()
                        .take(50)
                        .collect::<String>()
                        .replace('\t', " ")
                        .replace('\n', " ")
                );
            }
        }
        OutputFormat::Compact => {
            for hit in &results {
                println!(
                    "{}:{} ({:.2}) {}",
                    &hit.session_id[..8.min(hit.session_id.len())],
                    hit.message_type,
                    hit.score,
                    hit.content_snippet
                        .chars()
                        .take(80)
                        .collect::<String>()
                        .replace('\n', " ")
                );
            }
        }
        OutputFormat::Text => {
            if results.is_empty() {
                println!("No matches found.");
                return Ok(());
            }

            println!("Found {} matches:", results.len());
            println!();

            let mut current_session = String::new();

            for hit in &results {
                if hit.session_id != current_session {
                    current_session = hit.session_id.clone();
                    println!(
                        "Session: {} ({})",
                        &hit.session_id[..8.min(hit.session_id.len())],
                        hit.project
                    );
                }

                println!();
                println!("  [{} - score: {:.2}]", hit.message_type, hit.score);
                if let Some(ref model) = hit.model {
                    println!("  Model: {}", model);
                }
                println!("  > {}", hit.content_snippet.replace('\n', "\n    "));
            }
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn test_run_status() {
        // Just verify the function compiles and can be called
        // Real tests would mock the index
    }
}
