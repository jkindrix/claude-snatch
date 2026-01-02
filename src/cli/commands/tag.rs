//! Tag command implementation.
//!
//! Manage session tags, names, and bookmarks for easier session discovery.

use std::collections::HashSet;

use crate::analytics::SessionAnalytics;
use crate::cli::{Cli, OutputFormat, TagAction, TagArgs};
use crate::discovery::Session;
use crate::error::Result;
use crate::reconstruction::Conversation;
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

        TagAction::Link { session_a, session_b } => {
            let id_a = resolve_session_id(cli, session_a)?;
            let id_b = resolve_session_id(cli, session_b)?;

            if id_a == id_b {
                println!("Cannot link a session to itself.");
                return Ok(());
            }

            if store.link_sessions(&id_a, &id_b) {
                store.save()?;
                println!(
                    "Linked sessions {} <-> {}",
                    short_id(&id_a),
                    short_id(&id_b)
                );
            } else {
                println!(
                    "Sessions {} and {} are already linked",
                    short_id(&id_a),
                    short_id(&id_b)
                );
            }
        }

        TagAction::Unlink { session_a, session_b } => {
            let id_a = resolve_session_id(cli, session_a)?;
            let id_b = resolve_session_id(cli, session_b)?;

            if store.unlink_sessions(&id_a, &id_b) {
                store.save()?;
                println!(
                    "Unlinked sessions {} <-> {}",
                    short_id(&id_a),
                    short_id(&id_b)
                );
            } else {
                println!(
                    "Sessions {} and {} were not linked",
                    short_id(&id_a),
                    short_id(&id_b)
                );
            }
        }

        TagAction::Links { session } => {
            if let Some(session_prefix) = session {
                // Show links for specific session
                let session_id = resolve_session_id(cli, session_prefix)?;
                let linked = store.get_linked_sessions(&session_id);

                if linked.is_empty() {
                    println!("Session {} has no linked sessions.", short_id(&session_id));
                    return Ok(());
                }

                match cli.effective_output() {
                    OutputFormat::Json => {
                        println!("{}", serde_json::to_string_pretty(&linked)?);
                    }
                    OutputFormat::Tsv => {
                        println!("session_id\tname");
                        for id in &linked {
                            let name = store
                                .get(id)
                                .and_then(|m| m.name.as_deref())
                                .unwrap_or("");
                            println!("{}\t{}", id, name);
                        }
                    }
                    OutputFormat::Compact => {
                        for id in &linked {
                            println!("{}", short_id(id));
                        }
                    }
                    OutputFormat::Text => {
                        println!(
                            "Sessions linked to {} ({}):",
                            short_id(&session_id),
                            linked.len()
                        );
                        for id in &linked {
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
                // Show all sessions with links
                let sessions = store.sessions_with_links();
                if sessions.is_empty() {
                    println!("No sessions have links.");
                    return Ok(());
                }

                match cli.effective_output() {
                    OutputFormat::Json => {
                        let data: Vec<_> = sessions
                            .iter()
                            .map(|id| {
                                let linked = store.get_linked_sessions(id);
                                serde_json::json!({
                                    "session_id": id,
                                    "name": store.get(id).and_then(|m| m.name.as_ref()),
                                    "linked_count": linked.len(),
                                    "linked": linked
                                })
                            })
                            .collect();
                        println!("{}", serde_json::to_string_pretty(&data)?);
                    }
                    OutputFormat::Tsv => {
                        println!("session_id\tname\tlinked_count");
                        for id in &sessions {
                            let name = store
                                .get(id)
                                .and_then(|m| m.name.as_deref())
                                .unwrap_or("");
                            let linked = store.get_linked_sessions(id);
                            println!("{}\t{}\t{}", id, name, linked.len());
                        }
                    }
                    OutputFormat::Compact => {
                        for id in &sessions {
                            println!("{}", short_id(id));
                        }
                    }
                    OutputFormat::Text => {
                        println!("Sessions with links ({}):", sessions.len());
                        for id in &sessions {
                            let name = store
                                .get(id)
                                .and_then(|m| m.name.as_deref())
                                .map(|n| format!(" \"{}\"", n))
                                .unwrap_or_default();
                            let linked = store.get_linked_sessions(id);
                            println!(
                                "  {}{} -> {} linked session{}",
                                short_id(id),
                                name,
                                linked.len(),
                                if linked.len() == 1 { "" } else { "s" }
                            );
                        }
                    }
                }
            }
        }

        TagAction::Similar {
            session,
            limit,
            threshold,
            tool_weight,
            project_weight,
            time_weight,
            tag_weight,
            token_weight,
        } => {
            let session_id = resolve_session_id(cli, session)?;
            let claude_dir = get_claude_dir(cli.claude_dir.as_ref())?;
            let all_sessions = claude_dir.all_sessions()?;

            // Find the source session
            let source_session = all_sessions
                .iter()
                .find(|s| s.session_id() == session_id)
                .ok_or_else(|| crate::error::SnatchError::SessionNotFound {
                    session_id: session_id.clone(),
                })?;

            // Get source session metadata
            let source_meta = get_session_similarity_meta(source_session, &store, cli.max_file_size)?;

            // Calculate similarity for all other sessions
            let weights = SimilarityWeights {
                tool: *tool_weight,
                project: *project_weight,
                time: *time_weight,
                tag: *tag_weight,
                token: *token_weight,
            };

            let mut similarities: Vec<(String, SimilarityScore)> = all_sessions
                .iter()
                .filter(|s| s.session_id() != session_id)
                .filter_map(|s| {
                    let meta = get_session_similarity_meta(s, &store, cli.max_file_size).ok()?;
                    let score = calculate_similarity(&source_meta, &meta, &weights);
                    if score.total >= *threshold as f64 {
                        Some((s.session_id().to_string(), score))
                    } else {
                        None
                    }
                })
                .collect();

            // Sort by similarity score (highest first)
            similarities.sort_by(|a, b| b.1.total.partial_cmp(&a.1.total).unwrap_or(std::cmp::Ordering::Equal));

            // Limit results
            similarities.truncate(*limit);

            if similarities.is_empty() {
                println!("No similar sessions found for {} with threshold >= {}%", short_id(&session_id), threshold);
                return Ok(());
            }

            match cli.effective_output() {
                OutputFormat::Json => {
                    let data: Vec<_> = similarities
                        .iter()
                        .map(|(id, score)| {
                            serde_json::json!({
                                "session_id": id,
                                "name": store.get(id).and_then(|m| m.name.as_ref()),
                                "similarity": {
                                    "total": score.total,
                                    "tool_overlap": score.tool_overlap,
                                    "project_match": score.project_match,
                                    "time_proximity": score.time_proximity,
                                    "tag_overlap": score.tag_overlap,
                                    "token_similarity": score.token_similarity,
                                }
                            })
                        })
                        .collect();
                    println!("{}", serde_json::to_string_pretty(&serde_json::json!({
                        "source_session": session_id,
                        "similar_sessions": data,
                    }))?);
                }
                OutputFormat::Tsv => {
                    println!("session_id\tname\ttotal\ttool\tproject\ttime\ttag\ttoken");
                    for (id, score) in &similarities {
                        let name = store
                            .get(id)
                            .and_then(|m| m.name.as_deref())
                            .unwrap_or("");
                        println!(
                            "{}\t{}\t{:.1}\t{:.1}\t{:.1}\t{:.1}\t{:.1}\t{:.1}",
                            id, name, score.total, score.tool_overlap,
                            score.project_match, score.time_proximity,
                            score.tag_overlap, score.token_similarity
                        );
                    }
                }
                OutputFormat::Compact => {
                    for (id, score) in &similarities {
                        println!("{}:{:.0}%", short_id(id), score.total);
                    }
                }
                OutputFormat::Text => {
                    let source_name = store
                        .get(&session_id)
                        .and_then(|m| m.name.as_deref())
                        .map(|n| format!(" \"{}\"", n))
                        .unwrap_or_default();

                    println!("Sessions similar to {}{}", short_id(&session_id), source_name);
                    println!("{}", "=".repeat(40));
                    println!();

                    for (i, (id, score)) in similarities.iter().enumerate() {
                        let name = store
                            .get(id)
                            .and_then(|m| m.name.as_deref())
                            .map(|n| format!(" - {}", n))
                            .unwrap_or_default();

                        // Similarity bar
                        let bar_len = (score.total / 100.0 * 20.0) as usize;
                        let bar = format!(
                            "[{}{}]",
                            "█".repeat(bar_len),
                            "░".repeat(20 - bar_len)
                        );

                        println!(
                            "  {}. {}{} {} {:.0}%",
                            i + 1,
                            short_id(id),
                            name,
                            bar,
                            score.total
                        );

                        // Show breakdown on verbose
                        if *threshold < 50 || similarities.len() <= 5 {
                            println!(
                                "      Tools: {:.0}%  Project: {:.0}%  Time: {:.0}%  Tags: {:.0}%  Tokens: {:.0}%",
                                score.tool_overlap,
                                score.project_match,
                                score.time_proximity,
                                score.tag_overlap,
                                score.token_similarity
                            );
                        }
                    }

                    println!();
                    println!(
                        "Found {} similar session{} (threshold: {}%)",
                        similarities.len(),
                        if similarities.len() == 1 { "" } else { "s" },
                        threshold
                    );
                }
            }
        }
    }

    Ok(())
}

/// Metadata used for similarity comparison.
#[derive(Debug, Clone)]
struct SessionSimilarityMeta {
    #[allow(dead_code)]
    session_id: String,
    project_path: String,
    modified_time: chrono::DateTime<chrono::Utc>,
    tools_used: HashSet<String>,
    tags: HashSet<String>,
    total_tokens: u64,
}

/// Weights for similarity calculation.
#[derive(Debug, Clone)]
struct SimilarityWeights {
    tool: u8,
    project: u8,
    time: u8,
    tag: u8,
    token: u8,
}

/// Similarity score breakdown.
#[derive(Debug, Clone)]
struct SimilarityScore {
    total: f64,
    tool_overlap: f64,
    project_match: f64,
    time_proximity: f64,
    tag_overlap: f64,
    token_similarity: f64,
}

/// Get session metadata for similarity comparison.
fn get_session_similarity_meta(
    session: &Session,
    store: &TagStore,
    max_file_size: Option<u64>,
) -> Result<SessionSimilarityMeta> {
    let session_id = session.session_id().to_string();
    let project_path = session.project_path().to_string();
    let modified_time: chrono::DateTime<chrono::Utc> = session.modified_time().into();

    // Get tools and tokens from session content
    let (tools_used, total_tokens) = if let Ok(entries) = session.parse_with_options(max_file_size) {
        if let Ok(conv) = Conversation::from_entries(entries) {
            let analytics = SessionAnalytics::from_conversation(&conv);
            let tools: HashSet<String> = analytics.tool_counts.keys().cloned().collect();
            let tokens = analytics.usage.usage.total_tokens();
            (tools, tokens)
        } else {
            (HashSet::new(), 0)
        }
    } else {
        (HashSet::new(), 0)
    };

    // Get tags from store
    let tags: HashSet<String> = store
        .get(&session_id)
        .map(|m| m.tags.iter().cloned().collect())
        .unwrap_or_default();

    Ok(SessionSimilarityMeta {
        session_id,
        project_path,
        modified_time,
        tools_used,
        tags,
        total_tokens,
    })
}

/// Calculate similarity between two sessions.
fn calculate_similarity(
    source: &SessionSimilarityMeta,
    target: &SessionSimilarityMeta,
    weights: &SimilarityWeights,
) -> SimilarityScore {
    // Normalize weights
    let total_weight = weights.tool as f64
        + weights.project as f64
        + weights.time as f64
        + weights.tag as f64
        + weights.token as f64;

    // Tool overlap (Jaccard similarity)
    let tool_overlap = if source.tools_used.is_empty() && target.tools_used.is_empty() {
        100.0
    } else if source.tools_used.is_empty() || target.tools_used.is_empty() {
        0.0
    } else {
        let intersection = source.tools_used.intersection(&target.tools_used).count() as f64;
        let union = source.tools_used.union(&target.tools_used).count() as f64;
        (intersection / union) * 100.0
    };

    // Project match (exact or partial path match)
    let project_match = if source.project_path == target.project_path {
        100.0
    } else {
        // Check for partial match (shared path components)
        let source_parts: Vec<&str> = source.project_path.split('/').collect();
        let target_parts: Vec<&str> = target.project_path.split('/').collect();
        let common = source_parts
            .iter()
            .zip(target_parts.iter())
            .take_while(|(a, b)| a == b)
            .count();
        let max_len = source_parts.len().max(target_parts.len());
        if max_len > 0 {
            (common as f64 / max_len as f64) * 100.0
        } else {
            0.0
        }
    };

    // Time proximity (exponential decay, sessions within same day = 100%)
    let time_diff = (source.modified_time - target.modified_time)
        .num_seconds()
        .abs() as f64;
    let day_seconds = 86400.0;
    let time_proximity = ((-time_diff / (7.0 * day_seconds)).exp()) * 100.0; // Week decay

    // Tag overlap (Jaccard similarity)
    let tag_overlap = if source.tags.is_empty() && target.tags.is_empty() {
        50.0 // Neutral if both have no tags
    } else if source.tags.is_empty() || target.tags.is_empty() {
        0.0
    } else {
        let intersection = source.tags.intersection(&target.tags).count() as f64;
        let union = source.tags.union(&target.tags).count() as f64;
        (intersection / union) * 100.0
    };

    // Token similarity (relative difference)
    let token_similarity = if source.total_tokens == 0 && target.total_tokens == 0 {
        100.0
    } else if source.total_tokens == 0 || target.total_tokens == 0 {
        0.0
    } else {
        let diff = (source.total_tokens as f64 - target.total_tokens as f64).abs();
        let max_tokens = source.total_tokens.max(target.total_tokens) as f64;
        let ratio = 1.0 - (diff / max_tokens);
        ratio * 100.0
    };

    // Calculate weighted total
    let total = if total_weight > 0.0 {
        (tool_overlap * weights.tool as f64
            + project_match * weights.project as f64
            + time_proximity * weights.time as f64
            + tag_overlap * weights.tag as f64
            + token_similarity * weights.token as f64)
            / total_weight
    } else {
        0.0
    };

    SimilarityScore {
        total,
        tool_overlap,
        project_match,
        time_proximity,
        tag_overlap,
        token_similarity,
    }
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
