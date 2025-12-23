//! Watch command implementation.
//!
//! Watches session files for changes and displays updates in real-time.

use std::collections::HashMap;
use std::thread;
use std::time::Duration;

use crate::cli::{Cli, WatchArgs};
use crate::parser::SessionState;
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

    println!("Watching {} session(s)... (Ctrl+C to stop)", sessions.len());
    println!();

    // Track last seen line count for each session
    let mut line_counts: HashMap<String, usize> = HashMap::new();

    // Initialize line counts
    for session in &sessions {
        let entries = session.parse().unwrap_or_default();
        line_counts.insert(session.session_id().to_string(), entries.len());
    }

    // Main watch loop
    loop {
        for session in &sessions {
            let session_id = session.session_id().to_string();
            let current_count = *line_counts.get(&session_id).unwrap_or(&0);

            // Check for new entries
            match session.parse() {
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
