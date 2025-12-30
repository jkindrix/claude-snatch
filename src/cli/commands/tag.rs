//! Tag command implementation.
//!
//! Manage session tags, names, and bookmarks for easier session discovery.

use crate::cli::{Cli, OutputFormat, TagAction, TagArgs};
use crate::error::Result;
use crate::tags::TagStore;

use super::get_claude_dir;

/// Run the tag command.
pub fn run(cli: &Cli, args: &TagArgs) -> Result<()> {
    let mut store = TagStore::load()?;

    match &args.action {
        TagAction::Add { session, tag } => {
            let session_id = resolve_session_id(cli, session)?;
            if store.add_tag(&session_id, tag) {
                store.save()?;
                println!("Added tag '{}' to session {}", tag, short_id(&session_id));
            } else {
                println!(
                    "Session {} already has tag '{}'",
                    short_id(&session_id),
                    tag
                );
            }
        }

        TagAction::Remove { session, tag } => {
            let session_id = resolve_session_id(cli, session)?;
            if store.remove_tag(&session_id, tag) {
                store.save()?;
                println!(
                    "Removed tag '{}' from session {}",
                    tag,
                    short_id(&session_id)
                );
            } else {
                println!(
                    "Session {} does not have tag '{}'",
                    short_id(&session_id),
                    tag
                );
            }
        }

        TagAction::Name { session, name } => {
            let session_id = resolve_session_id(cli, session)?;
            if let Some(name) = name {
                store.set_name(&session_id, Some(name.clone()));
                store.save()?;
                println!(
                    "Set name '{}' for session {}",
                    name,
                    short_id(&session_id)
                );
            } else {
                store.set_name(&session_id, None);
                store.save()?;
                println!("Cleared name for session {}", short_id(&session_id));
            }
        }

        TagAction::List { session } => {
            if let Some(session_prefix) = session {
                // Show tags for a specific session
                let session_id = resolve_session_id(cli, session_prefix)?;
                if let Some(meta) = store.get(&session_id) {
                    match cli.effective_output() {
                        OutputFormat::Json => {
                            println!("{}", serde_json::to_string_pretty(meta)?);
                        }
                        OutputFormat::Tsv => {
                            println!("session_id\tname\ttags\tbookmarked");
                            println!(
                                "{}\t{}\t{}\t{}",
                                session_id,
                                meta.name.as_deref().unwrap_or(""),
                                meta.tags.join(","),
                                meta.bookmarked
                            );
                        }
                        OutputFormat::Compact => {
                            for tag in &meta.tags {
                                println!("{}", tag);
                            }
                        }
                        OutputFormat::Text => {
                            println!("Session: {}", short_id(&session_id));
                            if let Some(name) = &meta.name {
                                println!("  Name: {}", name);
                            }
                            if !meta.tags.is_empty() {
                                println!("  Tags: {}", meta.tags.join(", "));
                            }
                            if meta.bookmarked {
                                println!("  Bookmarked: yes");
                            }
                        }
                    }
                } else {
                    println!("No tags or metadata for session {}", short_id(&session_id));
                }
            } else {
                // List all unique tags
                let tags = store.all_tags();
                if tags.is_empty() {
                    println!("No tags defined.");
                    return Ok(());
                }

                match cli.effective_output() {
                    OutputFormat::Json => {
                        println!("{}", serde_json::to_string_pretty(&tags)?);
                    }
                    OutputFormat::Tsv => {
                        println!("tag\tcount");
                        for tag in &tags {
                            let count = store.sessions_with_tag(tag).len();
                            println!("{}\t{}", tag, count);
                        }
                    }
                    OutputFormat::Compact => {
                        for tag in &tags {
                            println!("{}", tag);
                        }
                    }
                    OutputFormat::Text => {
                        println!("Tags ({} unique):", tags.len());
                        for tag in &tags {
                            let count = store.sessions_with_tag(tag).len();
                            println!(
                                "  {} ({} session{})",
                                tag,
                                count,
                                if count == 1 { "" } else { "s" }
                            );
                        }
                    }
                }
            }
        }

        TagAction::Bookmark { session } => {
            let session_id = resolve_session_id(cli, session)?;
            store.set_bookmark(&session_id, true);
            store.save()?;
            println!("Bookmarked session {}", short_id(&session_id));
        }

        TagAction::Unbookmark { session } => {
            let session_id = resolve_session_id(cli, session)?;
            store.set_bookmark(&session_id, false);
            store.save()?;
            println!("Removed bookmark from session {}", short_id(&session_id));
        }

        TagAction::Bookmarks => {
            let bookmarked = store.bookmarked_sessions();
            if bookmarked.is_empty() {
                println!("No bookmarked sessions.");
                return Ok(());
            }

            match cli.effective_output() {
                OutputFormat::Json => {
                    println!("{}", serde_json::to_string_pretty(&bookmarked)?);
                }
                OutputFormat::Tsv => {
                    println!("session_id\tname");
                    for id in &bookmarked {
                        let name = store
                            .get(id)
                            .and_then(|m| m.name.as_deref())
                            .unwrap_or("");
                        println!("{}\t{}", id, name);
                    }
                }
                OutputFormat::Compact => {
                    for id in &bookmarked {
                        println!("{}", short_id(id));
                    }
                }
                OutputFormat::Text => {
                    println!("Bookmarked sessions ({}):", bookmarked.len());
                    for id in &bookmarked {
                        let name = store
                            .get(id)
                            .and_then(|m| m.name.as_deref())
                            .map(|n| format!(" - {}", n))
                            .unwrap_or_default();
                        println!("  {}{}", short_id(id), name);
                    }
                }
            }
        }

        TagAction::Find { tag } => {
            let sessions = store.sessions_with_tag(tag);
            if sessions.is_empty() {
                println!("No sessions with tag '{}'.", tag);
                return Ok(());
            }

            match cli.effective_output() {
                OutputFormat::Json => {
                    println!("{}", serde_json::to_string_pretty(&sessions)?);
                }
                OutputFormat::Tsv => {
                    println!("session_id\tname");
                    for id in &sessions {
                        let name = store
                            .get(id)
                            .and_then(|m| m.name.as_deref())
                            .unwrap_or("");
                        println!("{}\t{}", id, name);
                    }
                }
                OutputFormat::Compact => {
                    for id in &sessions {
                        println!("{}", short_id(id));
                    }
                }
                OutputFormat::Text => {
                    println!(
                        "Sessions with tag '{}' ({}):",
                        tag,
                        sessions.len()
                    );
                    for id in &sessions {
                        let name = store
                            .get(id)
                            .and_then(|m| m.name.as_deref())
                            .map(|n| format!(" - {}", n))
                            .unwrap_or_default();
                        println!("  {}{}", short_id(id), name);
                    }
                }
            }
        }

        TagAction::Outcome { session, outcome } => {
            let session_id = resolve_session_id(cli, session)?;
            if let Some(outcome) = outcome {
                store.set_outcome(&session_id, Some(*outcome));
                store.save()?;
                println!(
                    "Set outcome '{}' for session {}",
                    outcome,
                    short_id(&session_id)
                );
            } else {
                store.set_outcome(&session_id, None);
                store.save()?;
                println!("Cleared outcome for session {}", short_id(&session_id));
            }
        }

        TagAction::Outcomes { outcome } => {
            if let Some(outcome) = outcome {
                // List sessions with specific outcome
                let sessions = store.sessions_with_outcome(*outcome);
                if sessions.is_empty() {
                    println!("No sessions with outcome '{}'.", outcome);
                    return Ok(());
                }

                match cli.effective_output() {
                    OutputFormat::Json => {
                        println!("{}", serde_json::to_string_pretty(&sessions)?);
                    }
                    OutputFormat::Tsv => {
                        println!("session_id\tname\toutcome");
                        for id in &sessions {
                            let name = store
                                .get(id)
                                .and_then(|m| m.name.as_deref())
                                .unwrap_or("");
                            println!("{}\t{}\t{}", id, name, outcome);
                        }
                    }
                    OutputFormat::Compact => {
                        for id in &sessions {
                            println!("{}", short_id(id));
                        }
                    }
                    OutputFormat::Text => {
                        println!(
                            "Sessions with outcome '{}' ({}):",
                            outcome,
                            sessions.len()
                        );
                        for id in &sessions {
                            let name = store
                                .get(id)
                                .and_then(|m| m.name.as_deref())
                                .map(|n| format!(" - {}", n))
                                .unwrap_or_default();
                            println!("  {}{}", short_id(id), name);
                        }
                    }
                }
            } else {
                // Show outcome statistics
                let stats = store.outcome_stats();

                match cli.effective_output() {
                    OutputFormat::Json => {
                        println!("{}", serde_json::to_string_pretty(&stats)?);
                    }
                    OutputFormat::Tsv => {
                        println!("outcome\tcount");
                        println!("success\t{}", stats.success);
                        println!("partial\t{}", stats.partial);
                        println!("failed\t{}", stats.failed);
                        println!("abandoned\t{}", stats.abandoned);
                        println!("unclassified\t{}", stats.unclassified);
                    }
                    OutputFormat::Compact => {
                        println!("{} {} {} {} {}",
                            stats.success, stats.partial, stats.failed,
                            stats.abandoned, stats.unclassified);
                    }
                    OutputFormat::Text => {
                        println!("Session Outcome Statistics");
                        println!("==========================");
                        println!("  Success:      {:>5}", stats.success);
                        println!("  Partial:      {:>5}", stats.partial);
                        println!("  Failed:       {:>5}", stats.failed);
                        println!("  Abandoned:    {:>5}", stats.abandoned);
                        println!("  Unclassified: {:>5}", stats.unclassified);
                        println!();
                        println!("  Classified:   {:>5}", stats.classified());
                        println!("  Success Rate: {:>5.1}%", stats.success_rate());
                    }
                }
            }
        }
    }

    Ok(())
}

/// Resolve a session ID prefix to a full session ID.
fn resolve_session_id(cli: &Cli, prefix: &str) -> Result<String> {
    // First try to find in existing tag store
    let store = TagStore::load()?;
    if let Some(full_id) = store.resolve_id(prefix) {
        return Ok(full_id.to_string());
    }

    // Then try to find in actual session files
    let claude_dir = get_claude_dir(cli.claude_dir.as_ref())?;
    let sessions = claude_dir.all_sessions()?;

    let matches: Vec<_> = sessions
        .iter()
        .filter(|s| s.session_id().starts_with(prefix))
        .collect();

    match matches.len() {
        0 => {
            // Allow using the prefix as-is if no match found
            // (user might be tagging a session that's been deleted)
            Ok(prefix.to_string())
        }
        1 => Ok(matches[0].session_id().to_string()),
        _ => {
            eprintln!("Multiple sessions match prefix '{}':", prefix);
            for session in &matches[..5.min(matches.len())] {
                eprintln!("  {} ({})", session.session_id(), session.project_path());
            }
            if matches.len() > 5 {
                eprintln!("  ... and {} more", matches.len() - 5);
            }
            // Return the prefix and let the user be more specific
            Ok(prefix.to_string())
        }
    }
}

/// Get short ID (first 8 chars).
fn short_id(id: &str) -> String {
    if id.len() > 8 {
        id[..8].to_string()
    } else {
        id.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_short_id() {
        assert_eq!(short_id("40afc8a7-3fcb-4d29-b1ee-100b81b8c6c0"), "40afc8a7");
        assert_eq!(short_id("short"), "short");
    }
}
