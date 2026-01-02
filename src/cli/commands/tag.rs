//! Tag command implementation.
//!
//! Manage session tags, names, and bookmarks for easier session discovery.

use crate::cli::{Cli, OutputFormat, TagAction, TagArgs};
use crate::error::Result;
use crate::tags::TagStore;

use super::{get_claude_dir, parse_date_filter};

/// Run the tag command.
pub fn run(cli: &Cli, args: &TagArgs) -> Result<()> {
    let mut store = TagStore::load()?;

    match &args.action {
        TagAction::Add { tag, session, since, until, project, preview } => {
            // If session is specified, use single-session mode
            if let Some(session_prefix) = session {
                let session_id = resolve_session_id(cli, session_prefix)?;
                if *preview {
                    println!("Would add tag '{}' to session {}", tag, short_id(&session_id));
                } else if store.add_tag(&session_id, tag) {
                    store.save()?;
                    println!("Added tag '{}' to session {}", tag, short_id(&session_id));
                } else {
                    println!("Session {} already has tag '{}'", short_id(&session_id), tag);
                }
            } else if since.is_some() || until.is_some() || project.is_some() {
                // Bulk mode with filters
                let sessions = get_filtered_sessions(cli, since.as_deref(), until.as_deref(), project.as_deref())?;
                if sessions.is_empty() {
                    println!("No sessions match the specified filters.");
                    return Ok(());
                }

                if *preview {
                    println!("Would add tag '{}' to {} sessions:", tag, sessions.len());
                    for id in &sessions {
                        println!("  {}", short_id(id));
                    }
                } else {
                    let mut added = 0;
                    for session_id in &sessions {
                        if store.add_tag(session_id, tag) {
                            added += 1;
                        }
                    }
                    store.save()?;
                    println!("Added tag '{}' to {} sessions ({} already had it)", tag, added, sessions.len() - added);
                }
            } else {
                eprintln!("Error: Either --session or date/project filters are required.");
                eprintln!("Examples:");
                eprintln!("  snatch tag add sprint-42 -s 24ce6088");
                eprintln!("  snatch tag add sprint-42 --since 1week");
                eprintln!("  snatch tag add sprint-42 --since 1week -p myproject");
            }
        }

        TagAction::Remove { tag, session, since, until, project, preview } => {
            // If session is specified, use single-session mode
            if let Some(session_prefix) = session {
                let session_id = resolve_session_id(cli, session_prefix)?;
                if *preview {
                    println!("Would remove tag '{}' from session {}", tag, short_id(&session_id));
                } else if store.remove_tag(&session_id, tag) {
                    store.save()?;
                    println!("Removed tag '{}' from session {}", tag, short_id(&session_id));
                } else {
                    println!("Session {} does not have tag '{}'", short_id(&session_id), tag);
                }
            } else if since.is_some() || until.is_some() || project.is_some() {
                // Bulk mode with filters
                let sessions = get_filtered_sessions(cli, since.as_deref(), until.as_deref(), project.as_deref())?;
                if sessions.is_empty() {
                    println!("No sessions match the specified filters.");
                    return Ok(());
                }

                if *preview {
                    println!("Would remove tag '{}' from {} sessions:", tag, sessions.len());
                    for id in &sessions {
                        println!("  {}", short_id(id));
                    }
                } else {
                    let mut removed = 0;
                    for session_id in &sessions {
                        if store.remove_tag(session_id, tag) {
                            removed += 1;
                        }
                    }
                    store.save()?;
                    println!("Removed tag '{}' from {} sessions ({} didn't have it)", tag, removed, sessions.len() - removed);
                }
            } else {
                eprintln!("Error: Either --session or date/project filters are required.");
                eprintln!("Examples:");
                eprintln!("  snatch tag remove sprint-42 -s 24ce6088");
                eprintln!("  snatch tag remove sprint-42 --since 1week");
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

        TagAction::Note { session, text, label } => {
            let session_id = resolve_session_id(cli, session)?;
            store.add_note(&session_id, text, label.as_deref());
            store.save()?;
            let label_str = label.as_ref().map(|l| format!(" [{}]", l)).unwrap_or_default();
            println!("Added note{} to session {}", label_str, short_id(&session_id));
        }

        TagAction::Notes { session } => {
            let session_id = resolve_session_id(cli, session)?;
            if let Some(notes) = store.get_notes(&session_id) {
                if notes.is_empty() {
                    println!("No notes for session {}.", short_id(&session_id));
                    return Ok(());
                }

                match cli.effective_output() {
                    OutputFormat::Json => {
                        println!("{}", serde_json::to_string_pretty(&notes)?);
                    }
                    OutputFormat::Tsv => {
                        println!("index\tlabel\tcreated\ttext");
                        for (i, note) in notes.iter().enumerate() {
                            println!(
                                "{}\t{}\t{}\t{}",
                                i,
                                note.label.as_deref().unwrap_or(""),
                                note.created_at.format("%Y-%m-%d %H:%M"),
                                note.text.replace('\n', "\\n")
                            );
                        }
                    }
                    OutputFormat::Compact => {
                        for note in notes {
                            println!("{}", note.text.lines().next().unwrap_or(""));
                        }
                    }
                    OutputFormat::Text => {
                        println!("Notes for session {} ({} total):", short_id(&session_id), notes.len());
                        println!();
                        for (i, note) in notes.iter().enumerate() {
                            let label_str = note.label.as_ref().map(|l| format!(" [{}]", l)).unwrap_or_default();
                            println!("[{}]{} - {}", i, label_str, note.created_at.format("%Y-%m-%d %H:%M"));
                            for line in note.text.lines() {
                                println!("    {}", line);
                            }
                            println!();
                        }
                    }
                }
            } else {
                println!("No notes for session {}.", short_id(&session_id));
            }
        }

        TagAction::Unnote { session, index } => {
            let session_id = resolve_session_id(cli, session)?;
            if store.remove_note(&session_id, *index) {
                store.save()?;
                println!("Removed note {} from session {}", index, short_id(&session_id));
            } else {
                println!("No note at index {} for session {}", index, short_id(&session_id));
            }
        }

        TagAction::ClearNotes { session } => {
            let session_id = resolve_session_id(cli, session)?;
            store.clear_notes(&session_id);
            store.save()?;
            println!("Cleared all notes from session {}", short_id(&session_id));
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

/// Get session IDs matching date/project filters for bulk operations.
fn get_filtered_sessions(
    cli: &Cli,
    since: Option<&str>,
    until: Option<&str>,
    project: Option<&str>,
) -> Result<Vec<String>> {
    let claude_dir = get_claude_dir(cli.claude_dir.as_ref())?;
    let sessions = claude_dir.all_sessions()?;

    // Parse date filters
    let since_time = since.map(parse_date_filter).transpose()?;
    let until_time = until.map(parse_date_filter).transpose()?;

    let filtered: Vec<String> = sessions
        .iter()
        .filter(|s| {
            // Apply project filter
            if let Some(ref proj) = project {
                if !s.project_path().contains(proj) {
                    return false;
                }
            }

            // Apply date filters
            let modified = s.modified_time();
            if let Some(since) = since_time {
                if modified < since {
                    return false;
                }
            }
            if let Some(until) = until_time {
                if modified > until {
                    return false;
                }
            }

            true
        })
        .map(|s| s.session_id().to_string())
        .collect();

    Ok(filtered)
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
