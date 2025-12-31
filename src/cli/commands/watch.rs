//! Watch command implementation.
//!
//! Watches session files for changes and displays updates in real-time.

use std::collections::HashMap;
use std::io::{self, Write};
use std::thread;
use std::time::{Duration, Instant};

use crate::analytics::SessionAnalytics;
use crate::cli::{Cli, WatchArgs};
use crate::discovery::Session;
use crate::parser::SessionState;
use crate::reconstruction::Conversation;
use crate::error::{Result, SnatchError};
use crate::model::LogEntry;

use super::get_claude_dir;

/// Run the watch command.
pub fn run(cli: &Cli, args: &WatchArgs) -> Result<()> {
    let claude_dir = get_claude_dir(cli.claude_dir.as_ref())?;
    let poll_interval = Duration::from_millis(args.interval);

    // Find sessions to watch
    let sessions = if args.all {
        // Watch all active sessions
        claude_dir
            .all_sessions()?
            .into_iter()
            .filter(|s| s.is_active().unwrap_or(false))
            .collect()
    } else if let Some(session_id) = &args.session {
        let session = claude_dir
            .find_session(session_id)?
            .ok_or_else(|| SnatchError::SessionNotFound {
                session_id: session_id.clone(),
            })?;
        vec![session]
    } else {
        // Watch most recently modified session
        let mut sessions = claude_dir.all_sessions()?;
        sessions.truncate(1);
        sessions
    };

    if sessions.is_empty() {
        println!("No sessions to watch.");
        return Ok(());
    }

    // Use live dashboard mode if requested
    if args.live {
        return run_live_dashboard(cli, &sessions, poll_interval);
    }

    println!("Watching {} session(s)... (Ctrl+C to stop)", sessions.len());
    println!();

    // Track last seen line count for each session
    let mut line_counts: HashMap<String, usize> = HashMap::new();

    // Initialize line counts
    for session in &sessions {
        let entries = session.parse_with_options(cli.max_file_size).unwrap_or_default();
        line_counts.insert(session.session_id().to_string(), entries.len());
    }

    // Main watch loop
    loop {
        for session in &sessions {
            let session_id = session.session_id().to_string();
            let current_count = *line_counts.get(&session_id).unwrap_or(&0);

            // Check for new entries
            match session.parse_with_options(cli.max_file_size) {
                Ok(entries) => {
                    if entries.len() > current_count {
                        // New entries found
                        for entry in entries.iter().skip(current_count) {
                            display_entry(cli, &session_id, entry);
                        }
                        line_counts.insert(session_id.clone(), entries.len());
                    }
                }
                Err(_) => {
                    // Skip parse errors during watch
                }
            }

            // Check session state
            if args.follow {
                match session.state() {
                    Ok(SessionState::Inactive) => {
                        if !cli.quiet {
                            println!("[{}] Session inactive", short_id(&session_id));
                        }
                    }
                    Ok(SessionState::PossiblyActive) => {
                        // Still active, continue watching
                    }
                    Ok(SessionState::RecentlyActive) => {
                        // Recently active, continue watching
                    }
                    Err(_) => {}
                }
            }
        }

        // Check if we should continue
        if !args.follow {
            // Single check mode
            break;
        }

        thread::sleep(poll_interval);
    }

    Ok(())
}

/// Display an entry in watch mode.
fn display_entry(_cli: &Cli, session_id: &str, entry: &LogEntry) {
    let timestamp = entry
        .timestamp()
        .map(|t| t.format("%H:%M:%S").to_string())
        .unwrap_or_else(|| "??:??:??".to_string());

    let type_str = entry.message_type();
    let uuid = entry.uuid().unwrap_or("unknown");

    match entry {
        LogEntry::User(user) => {
            let content = match &user.message {
                crate::model::UserContent::Simple(s) => truncate(&s.content, 80),
                crate::model::UserContent::Blocks(b) => {
                    let texts: Vec<_> = b.content.iter().filter_map(|c| {
                        if let crate::model::ContentBlock::Text(t) = c {
                            Some(t.text.as_str())
                        } else {
                            None
                        }
                    }).collect();
                    truncate(&texts.join(" "), 80)
                }
            };
            println!("[{} {}] 👤 USER: {}", timestamp, short_id(session_id), content);
        }
        LogEntry::Assistant(assistant) => {
            let has_thinking = assistant.message.content.iter().any(|c| matches!(c, crate::model::ContentBlock::Thinking(_)));
            let has_tools = assistant.message.content.iter().any(|c| matches!(c, crate::model::ContentBlock::ToolUse(_)));

            let mut text_content = String::new();
            for block in &assistant.message.content {
                if let crate::model::ContentBlock::Text(t) = block {
                    text_content.push_str(&t.text);
                    text_content.push(' ');
                }
            }

            let mut markers = Vec::new();
            if has_thinking { markers.push("💭"); }
            if has_tools { markers.push("🔧"); }

            let markers_str = if markers.is_empty() {
                String::new()
            } else {
                format!("{} ", markers.join(""))
            };

            println!("[{} {}] 🤖 ASST: {}{}",
                timestamp,
                short_id(session_id),
                markers_str,
                truncate(&text_content, 80)
            );
        }
        LogEntry::System(system) => {
            let subtype = system.subtype.as_ref()
                .map(|s| format!("{s:?}"))
                .unwrap_or_else(|| "system".to_string());
            println!("[{} {}] ⚙️  SYS: {}", timestamp, short_id(session_id), subtype);
        }
        LogEntry::Summary(_) => {
            println!("[{} {}] 📋 SUMMARY", timestamp, short_id(session_id));
        }
        _ => {
            println!("[{} {}] {} {}", timestamp, short_id(session_id), type_str, short_id(uuid));
        }
    }
}

/// Get short ID.
fn short_id(id: &str) -> String {
    if id.len() > 8 {
        id[..8].to_string()
    } else {
        id.to_string()
    }
}

/// Truncate string with ellipsis.
fn truncate(s: &str, max_len: usize) -> String {
    let s = s.replace('\n', " ").replace('\r', "");
    if s.len() <= max_len {
        s
    } else {
        format!("{}...", &s[..max_len.saturating_sub(3)])
    }
}

/// Run live dashboard mode with real-time stats display.
fn run_live_dashboard(cli: &Cli, sessions: &[Session], poll_interval: Duration) -> Result<()> {
    let start_time = Instant::now();

    // Clear screen and hide cursor
    print!("\x1b[2J\x1b[H\x1b[?25l");
    io::stdout().flush()?;

    // Note: Ctrl+C will naturally terminate the process; cursor will be restored by terminal

    loop {
        // Move cursor to top-left
        print!("\x1b[H");

        // Build dashboard
        let mut output = String::new();

        // Header
        let elapsed = start_time.elapsed();
        output.push_str("╔══════════════════════════════════════════════════════════════════════════════╗\n");
        output.push_str("║           🔍  CLAUDE-SNATCH LIVE MONITOR                                     ║\n");
        output.push_str("╠══════════════════════════════════════════════════════════════════════════════╣\n");
        output.push_str(&format!(
            "║  ⏱️  Uptime: {:02}:{:02}:{:02}  │  Sessions: {:>3}  │  Interval: {:>4}ms                  ║\n",
            elapsed.as_secs() / 3600,
            (elapsed.as_secs() % 3600) / 60,
            elapsed.as_secs() % 60,
            sessions.len(),
            poll_interval.as_millis()
        ));
        output.push_str("╠══════════════════════════════════════════════════════════════════════════════╣\n");

        // Aggregate stats across sessions
        let mut total_input_tokens: u64 = 0;
        let mut total_output_tokens: u64 = 0;
        let mut total_cache_read: u64 = 0;
        let mut total_cache_creation: u64 = 0;
        let mut total_messages: usize = 0;
        let mut total_cost: f64 = 0.0;
        let mut active_count = 0;

        for session in sessions {
            let entries = session.parse_with_options(cli.max_file_size).unwrap_or_default();
            if let Ok(conversation) = Conversation::from_entries(entries) {
                let analytics = SessionAnalytics::from_conversation(&conversation);

                total_input_tokens += analytics.usage.usage.input_tokens;
                total_output_tokens += analytics.usage.usage.output_tokens;
                total_cache_read += analytics.usage.usage.cache_read_input_tokens.unwrap_or(0);
                total_cache_creation += analytics.usage.usage.cache_creation_input_tokens.unwrap_or(0);
                total_messages += analytics.usage.message_count;
                total_cost += analytics.usage.estimated_cost.unwrap_or(0.0);
            }

            if session.is_active().unwrap_or(false) {
                active_count += 1;
            }
        }

        // Token usage section
        output.push_str("║  📊 TOKEN USAGE                                                              ║\n");
        output.push_str("║  ────────────────────────────────────────────────────────────────────────    ║\n");
        output.push_str(&format!(
            "║    Input Tokens:     {:>12} │ Cache Read:     {:>12}              ║\n",
            format_number(total_input_tokens),
            format_number(total_cache_read)
        ));
        output.push_str(&format!(
            "║    Output Tokens:    {:>12} │ Cache Creation: {:>12}              ║\n",
            format_number(total_output_tokens),
            format_number(total_cache_creation)
        ));
        output.push_str(&format!(
            "║    Total Messages:   {:>12} │ Active Sessions:{:>12}              ║\n",
            total_messages,
            active_count
        ));
        output.push_str("╠══════════════════════════════════════════════════════════════════════════════╣\n");

        // Cost section
        output.push_str("║  💰 COST ESTIMATE                                                            ║\n");
        output.push_str("║  ────────────────────────────────────────────────────────────────────────    ║\n");
        output.push_str(&format!(
            "║    Estimated Cost:   ${:<12.4}                                            ║\n",
            total_cost
        ));

        // Calculate hourly rate if running for more than 1 minute
        if elapsed.as_secs() > 60 {
            let hourly_rate = total_cost / (elapsed.as_secs_f64() / 3600.0);
            output.push_str(&format!(
                "║    Projected Rate:   ${:<12.2}/hour                                       ║\n",
                hourly_rate
            ));
        } else {
            output.push_str("║    Projected Rate:   (calculating...)                                        ║\n");
        }
        output.push_str("╠══════════════════════════════════════════════════════════════════════════════╣\n");

        // Recent activity section
        output.push_str("║  📝 RECENT ACTIVITY                                                          ║\n");
        output.push_str("║  ────────────────────────────────────────────────────────────────────────    ║\n");

        // Get last 5 messages across all sessions
        let mut recent_entries: Vec<(String, LogEntry)> = Vec::new();
        for session in sessions {
            let entries = session.parse_with_options(cli.max_file_size).unwrap_or_default();
            for entry in entries.into_iter().rev().take(5) {
                recent_entries.push((session.session_id().to_string(), entry));
            }
        }
        recent_entries.sort_by(|a, b| {
            let ts_a = a.1.timestamp();
            let ts_b = b.1.timestamp();
            ts_b.cmp(&ts_a) // Reverse order (newest first)
        });
        recent_entries.truncate(5);

        for (session_id, entry) in &recent_entries {
            let timestamp = entry
                .timestamp()
                .map(|t| t.format("%H:%M:%S").to_string())
                .unwrap_or_else(|| "??:??:??".to_string());

            let (icon, preview) = match entry {
                LogEntry::User(user) => {
                    let content = match &user.message {
                        crate::model::UserContent::Simple(s) => s.content.clone(),
                        crate::model::UserContent::Blocks(b) => {
                            b.content.iter().filter_map(|c| {
                                if let crate::model::ContentBlock::Text(t) = c {
                                    Some(t.text.as_str())
                                } else {
                                    None
                                }
                            }).collect::<Vec<_>>().join(" ")
                        }
                    };
                    ("👤", truncate(&content, 50))
                }
                LogEntry::Assistant(_) => ("🤖", "Assistant response...".to_string()),
                LogEntry::System(sys) => {
                    let subtype = sys.subtype.as_ref()
                        .map(|s| format!("{s:?}"))
                        .unwrap_or_else(|| "system".to_string());
                    ("⚙️ ", subtype)
                }
                _ => ("📋", entry.message_type().to_string()),
            };

            output.push_str(&format!(
                "║    {} {} [{}] {:<50} ║\n",
                timestamp,
                icon,
                &session_id[..8.min(session_id.len())],
                truncate(&preview, 48)
            ));
        }

        // Pad with empty lines if fewer than 5 entries
        for _ in recent_entries.len()..5 {
            output.push_str("║                                                                              ║\n");
        }

        output.push_str("╚══════════════════════════════════════════════════════════════════════════════╝\n");
        output.push_str("  Press Ctrl+C to exit\n");

        print!("{output}");
        io::stdout().flush()?;

        thread::sleep(poll_interval);
    }
}

/// Format large numbers with comma separators.
fn format_number(n: u64) -> String {
    let s = n.to_string();
    let mut result = String::new();
    let chars: Vec<char> = s.chars().collect();
    for (i, c) in chars.iter().enumerate() {
        if i > 0 && (chars.len() - i) % 3 == 0 {
            result.push(',');
        }
        result.push(*c);
    }
    result
}
