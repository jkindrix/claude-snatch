//! Thread command implementation.
//!
//! Cross-session topic threading: searches all sessions for a pattern,
//! then presents matches with surrounding conversation context, ordered
//! chronologically across sessions.

use std::io::IsTerminal;

use indicatif::{ProgressBar, ProgressStyle};
use regex::{Regex, RegexBuilder};

use crate::analysis::threading::{thread_topic, ThreadParams, ThreadedExchange};
use crate::cli::{Cli, ThreadArgs};
use crate::error::{Result, SnatchError};

use super::helpers::{self, truncate, SessionCollectParams};

/// Run the thread command.
pub fn run(cli: &Cli, args: &ThreadArgs) -> Result<()> {
    if args.pattern.trim().is_empty() {
        return Err(SnatchError::InvalidArgument {
            name: "pattern".to_string(),
            reason: "thread pattern cannot be empty".to_string(),
        });
    }

    let regex = RegexBuilder::new(&args.pattern)
        .case_insensitive(args.ignore_case)
        .build()
        .map_err(|e| SnatchError::InvalidArgument {
            name: "pattern".to_string(),
            reason: e.to_string(),
        })?;

    let limit = if args.no_limit {
        usize::MAX
    } else {
        args.limit
    };

    let params = ThreadParams {
        include_thinking: args.thinking,
        limit,
        max_user_context: args.max_user_context.unwrap_or(args.max_context),
        max_assistant_context: args.max_assistant_context.unwrap_or(args.max_context),
        max_thinking_context: args.max_context,
        role_filter: args.role.clone(),
        decisions_only: args.decisions_only,
    };
    let registry = helpers::provider_registry(cli);
    let provider_route = !args.provider.is_empty()
        || args
            .session
            .as_deref()
            .is_some_and(|session| registry.looks_qualified(session));
    let result = if provider_route {
        provider_thread_topic(cli, args, &regex, &params)?
    } else {
        let sessions = helpers::collect_sessions(
            cli,
            &SessionCollectParams {
                session: args.session.as_deref(),
                project: args.project.as_deref(),
                since: args.since.as_deref(),
                until: args.until.as_deref(),
                recent: args.recent,
                no_subagents: args.no_subagents,
            },
        )?;

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
        thread_topic(&sessions, &regex, &params, cli.max_file_size)
    };

    if result.exchanges.is_empty() {
        if cli.effective_output() == crate::cli::OutputFormat::Json {
            println!("[]");
        } else if !cli.quiet {
            println!("No matches found for pattern: {}", args.pattern);
        }
        return Ok(());
    }

    match cli.effective_output() {
        crate::cli::OutputFormat::Json => output_json(&result.exchanges),
        _ => {
            output_text(
                cli,
                &result.exchanges,
                &args.pattern,
                result.session_count,
                params.max_user_context,
                params.max_assistant_context,
                params.max_thinking_context,
            );
            if args.summary && !cli.quiet {
                output_summary(&result.exchanges, result.session_count);
            }
        }
    }

    Ok(())
}

fn provider_thread_topic(
    cli: &Cli,
    args: &ThreadArgs,
    regex: &Regex,
    params: &ThreadParams,
) -> Result<crate::analysis::threading::ThreadResult> {
    use crate::analysis::threading::{
        finish_thread_exchanges, thread_one_conversation, ThreadConversation,
    };
    use crate::provider::registry::{cached_parsed_session, ProviderSelection};
    use crate::provider::LineageEdgeKind;
    use crate::reconstruction::Conversation;

    fn context_overlaps(
        context: &crate::provider::project::SessionProjectContext,
        since: Option<chrono::DateTime<chrono::Utc>>,
        until: Option<chrono::DateTime<chrono::Utc>>,
    ) -> bool {
        let start = context.started_at.or(context.modified_at);
        let end = context.ended_at.or(context.modified_at).or(start);
        since.map_or(true, |bound| end.map_or(true, |end| end >= bound))
            && until.map_or(true, |bound| start.map_or(true, |start| start <= bound))
    }

    let since = args
        .since
        .as_deref()
        .map(super::parse_date_filter)
        .transpose()?
        .map(chrono::DateTime::<chrono::Utc>::from);
    let until = args
        .until
        .as_deref()
        .map(super::parse_date_filter)
        .transpose()?
        .map(chrono::DateTime::<chrono::Utc>::from);
    let registry = helpers::provider_registry(cli);
    let mut exchanges = Vec::new();

    if let Some(session_id) = args.session.as_deref() {
        let resolution = registry.resolve_with_default_policy(&args.provider, session_id)?;
        let key = resolution.key.clone();
        if args.no_subagents {
            let spawned =
                resolution.provider.lineage()?.into_iter().any(|edge| {
                    edge.to == key && matches!(edge.kind, LineageEdgeKind::Spawn { .. })
                });
            if spawned || key.namespace.0.starts_with("subagent:") {
                return Ok(finish_thread_exchanges(Vec::new(), params.limit));
            }
        }
        let parsed = cached_parsed_session(
            crate::cache::global_cache(),
            resolution.provider,
            &resolution.key,
        )?;
        let context = crate::provider::project::SessionProjectContext::from_parsed(&parsed);
        if args.project.as_deref().is_some_and(|project| {
            !context
                .cwd
                .as_deref()
                .is_some_and(|cwd| cwd.contains(project))
        }) || !context_overlaps(&context, since, until)
        {
            return Ok(finish_thread_exchanges(Vec::new(), params.limit));
        }
        let conversation = Conversation::from_parsed_session(parsed)?;
        let qualified = resolution.key.to_string();
        exchanges.extend(thread_one_conversation(
            &ThreadConversation {
                provider: &resolution.key.provider.0,
                qualified_id: &qualified,
                session_id: &resolution.key.native_id,
                project: context.cwd.as_deref().unwrap_or("unknown"),
                conversation: &conversation,
                semantic_annotations: resolution.provider.capabilities().semantic_annotations,
            },
            regex,
            params,
        ));
        return Ok(finish_thread_exchanges(exchanges, params.limit));
    }

    let selection = ProviderSelection::from_flags(&args.provider).map_err(|reason| {
        SnatchError::InvalidArgument {
            name: "--provider".to_string(),
            reason,
        }
    })?;
    let collected = registry.collect_project_union(&selection)?;
    let spawned: std::collections::BTreeSet<_> = collected
        .lineage
        .iter()
        .filter(|edge| matches!(edge.kind, LineageEdgeKind::Spawn { .. }))
        .map(|edge| edge.to.clone())
        .collect();
    let mut candidates = Vec::new();
    for project in &collected.projects {
        if args
            .project
            .as_deref()
            .is_some_and(|needle| !project.matches(needle))
        {
            continue;
        }
        for session in &project.sessions {
            let key = &session.descriptor.key;
            if args.no_subagents
                && (spawned.contains(key) || key.namespace.0.starts_with("subagent:"))
            {
                continue;
            }
            if context_overlaps(&session.context, since, until) {
                candidates.push((
                    session.clone(),
                    project
                        .display_path
                        .clone()
                        .unwrap_or_else(|| "unknown".to_string()),
                ));
            }
        }
    }
    candidates.sort_by(|(left, _), (right, _)| {
        right
            .context
            .modified_at
            .or(right.context.ended_at)
            .or(right.context.started_at)
            .cmp(
                &left
                    .context
                    .modified_at
                    .or(left.context.ended_at)
                    .or(left.context.started_at),
            )
            .then_with(|| left.descriptor.key.cmp(&right.descriptor.key))
    });
    if let Some(recent) = args.recent {
        candidates.truncate(recent);
    }

    for (session, display_path) in candidates {
        let key = &session.descriptor.key;
        let provider = match registry.get(&key.provider) {
            Ok(provider) => provider,
            Err(_) => continue,
        };
        let parsed = match cached_parsed_session(crate::cache::global_cache(), provider, key) {
            Ok(parsed) => parsed,
            Err(_) => {
                if !cli.quiet {
                    eprintln!("warning: {key}: session could not be parsed");
                }
                continue;
            }
        };
        let conversation = match Conversation::from_parsed_session(parsed) {
            Ok(conversation) => conversation,
            Err(_) => {
                if !cli.quiet {
                    eprintln!("warning: {key}: conversation could not be reconstructed");
                }
                continue;
            }
        };
        let qualified = key.to_string();
        let project = session.context.cwd.as_deref().unwrap_or(&display_path);
        exchanges.extend(thread_one_conversation(
            &ThreadConversation {
                provider: &key.provider.0,
                qualified_id: &qualified,
                session_id: &key.native_id,
                project,
                conversation: &conversation,
                semantic_annotations: provider.capabilities().semantic_annotations,
            },
            regex,
            params,
        ));
    }
    if !cli.quiet {
        for (provider, _) in collected.skipped {
            eprintln!("warning: provider {provider} unavailable");
        }
    }
    Ok(finish_thread_exchanges(exchanges, params.limit))
}

fn output_json(exchanges: &[ThreadedExchange]) {
    let entries: Vec<serde_json::Value> = exchanges
        .iter()
        .map(|e| {
            let mut obj = serde_json::json!({
                "timestamp": e.timestamp.to_rfc3339(),
                "session_id": e.session_id,
                "provider": e.provider,
                "qualified_id": e.qualified_id,
                "entry_uuid": e.entry_uuid,
                "project": e.project,
                "match_location": e.match_location,
                "match_provenance": e.match_provenance,
                "match_count": e.match_count,
            });
            if let Some(ref text) = e.user_text {
                obj["user_text"] = serde_json::Value::String(text.clone());
            }
            if let Some(ref text) = e.assistant_text {
                obj["assistant_text"] = serde_json::Value::String(text.clone());
            }
            if let Some(ref text) = e.thinking_text {
                obj["thinking_text"] = serde_json::Value::String(text.clone());
            }
            obj
        })
        .collect();

    println!(
        "{}",
        serde_json::to_string_pretty(&entries).unwrap_or_default()
    );
}

fn output_text(
    cli: &Cli,
    exchanges: &[ThreadedExchange],
    pattern: &str,
    session_count: usize,
    max_user_context: usize,
    max_assistant_context: usize,
    max_thinking_context: usize,
) {
    if !cli.quiet {
        println!(
            "Thread: \"{}\" - {} exchanges across {} sessions\n",
            pattern,
            exchanges.len(),
            session_count,
        );
    }

    let mut last_session_id = String::new();

    for (i, exchange) in exchanges.iter().enumerate() {
        let date = exchange.timestamp.format("%Y-%m-%d %H:%M");

        if exchange.session_id != last_session_id {
            if i > 0 {
                println!();
            }
            println!(
                "--- {} [{}] ---",
                date.to_string().split(' ').next().unwrap_or(""),
                exchange.short_id
            );
            last_session_id = exchange.session_id.clone();
        }

        println!();
        println!(
            "  {} | {} ({}) in {} ({} match{})",
            date,
            exchange.match_location,
            exchange.match_provenance,
            exchange.short_id,
            exchange.match_count,
            if exchange.match_count == 1 { "" } else { "es" }
        );

        if let Some(ref text) = exchange.user_text {
            println!();
            println!("  USER:");
            for line in truncate(text, max_user_context).lines() {
                println!("    {}", line);
            }
        }

        if let Some(ref text) = exchange.assistant_text {
            println!();
            println!("  ASSISTANT:");
            for line in truncate(text, max_assistant_context).lines() {
                println!("    {}", line);
            }
        }

        if let Some(ref text) = exchange.thinking_text {
            println!();
            println!("  THINKING:");
            for line in truncate(text, max_thinking_context).lines() {
                println!("    {}", line);
            }
        }

        if i < exchanges.len() - 1 {
            println!();
            println!("  ─────────────────────────────────────────");
        }
    }

    println!();
}

fn output_summary(exchanges: &[ThreadedExchange], session_count: usize) {
    if exchanges.is_empty() {
        return;
    }

    println!("═══════════════════════════════════════════");
    println!("  SUMMARY");
    println!("═══════════════════════════════════════════");

    let first = &exchanges[0];
    let last = &exchanges[exchanges.len() - 1];
    let total_matches: usize = exchanges.iter().map(|e| e.match_count).sum();

    let first_date = first.timestamp.format("%Y-%m-%d");
    let last_date = last.timestamp.format("%Y-%m-%d");
    if first_date.to_string() == last_date.to_string() {
        println!("  Date: {first_date}");
    } else {
        println!("  Span: {first_date} → {last_date}");
    }
    println!(
        "  {} exchange(s) across {} session(s), {} total match(es)",
        exchanges.len(),
        session_count,
        total_matches,
    );

    if let Some(ref text) = first.assistant_text {
        let snippet = truncate(text, 200);
        let first_line = snippet.lines().next().unwrap_or("");
        println!("\n  First ({}):", first.timestamp.format("%Y-%m-%d %H:%M"));
        println!("    {first_line}");
    }

    if exchanges.len() > 1 {
        if let Some(ref text) = last.assistant_text {
            let snippet = truncate(text, 200);
            let first_line = snippet.lines().next().unwrap_or("");
            println!("\n  Latest ({}):", last.timestamp.format("%Y-%m-%d %H:%M"));
            println!("    {first_line}");
        }
    }

    println!();
}
