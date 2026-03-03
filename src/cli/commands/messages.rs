//! Messages command implementation.
//!
//! Reads conversation messages from a session at different detail levels,
//! mirroring the MCP `get_session_messages` tool for CLI use.

use crate::analysis::extraction::{
    extract_assistant_summary, extract_thinking_text, extract_tool_input_summary,
    extract_tool_names, extract_user_prompt_text, get_model, has_thinking, truncate_text,
};
use crate::cli::{Cli, MessagesArgs, OutputFormat};
use crate::error::{Result, SnatchError};
use crate::model::message::LogEntry;
use crate::reconstruction::Conversation;

use super::get_claude_dir;

/// JSON output types.
#[derive(serde::Serialize)]
struct MessagesOutput {
    session_id: String,
    project_path: String,
    total_messages: usize,
    returned: usize,
    offset: usize,
    messages: Vec<MessageOutput>,
}

#[derive(serde::Serialize)]
struct MessageOutput {
    index: usize,
    msg_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    timestamp: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_calls: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_details: Option<Vec<ToolDetailOutput>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    has_thinking: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    thinking_preview: Option<String>,
}

#[derive(serde::Serialize)]
struct ToolDetailOutput {
    tool_name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    file_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    command: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pattern: Option<String>,
}

/// Run the messages command.
pub fn run(cli: &Cli, args: &MessagesArgs) -> Result<()> {
    let claude_dir = get_claude_dir(cli.claude_dir.as_ref())?;

    let session = claude_dir
        .find_session(&args.session_id)?
        .ok_or_else(|| SnatchError::SessionNotFound {
            session_id: args.session_id.clone(),
        })?;

    let project_path = session.project_path().to_string();
    let entries = session.parse_with_options(cli.max_file_size)?;
    let conversation = Conversation::from_entries(entries)?;

    let detail = args.detail.as_deref().unwrap_or("standard");
    let msg_type_filter = args.message_type.as_deref().unwrap_or("all");
    let limit = args.limit;
    let offset = args.offset;
    let include_thinking = args.include_thinking;

    let thinking_max_len = match detail {
        "overview" => 500,
        "conversation" | "standard" => 1000,
        _ => 2000,
    };

    let truncate_len = match detail {
        "overview" => 200,
        "conversation" | "standard" => 500,
        _ => 1000,
    };

    let mut main_entries: Vec<&LogEntry> = conversation.main_thread_entries();

    // Filter by message type
    match msg_type_filter {
        "user" => main_entries.retain(|e| {
            matches!(e, LogEntry::User(u) if u.message.has_visible_text())
        }),
        "assistant" => main_entries.retain(|e| matches!(e, LogEntry::Assistant(_))),
        "system" => main_entries.retain(|e| matches!(e, LogEntry::System(_))),
        _ => {}
    }

    // Pre-filter by detail level
    match detail {
        "overview" => {
            main_entries.retain(|e| {
                if let LogEntry::User(u) = e {
                    u.message.has_visible_text()
                } else {
                    false
                }
            });
        }
        "conversation" => {
            main_entries.retain(|e| match e {
                LogEntry::User(u) => u.message.has_visible_text(),
                LogEntry::Assistant(_) => extract_assistant_summary(e, 1).is_some(),
                _ => false,
            });
        }
        _ => {}
    }

    let total_messages = main_entries.len();

    // Build indexed pairs
    let mut indexed: Vec<(usize, &LogEntry)> = main_entries.into_iter().enumerate().collect();

    if args.reverse {
        indexed.reverse();
    }

    // Paginate
    let paginated: Vec<(usize, &LogEntry)> = indexed.into_iter().skip(offset).take(limit).collect();

    match cli.effective_output() {
        OutputFormat::Json => {
            let messages: Vec<MessageOutput> = paginated
                .iter()
                .filter_map(|(orig_idx, entry)| {
                    build_message_output(*orig_idx, entry, detail, truncate_len, include_thinking, thinking_max_len)
                })
                .collect();

            let output = MessagesOutput {
                session_id: args.session_id.clone(),
                project_path,
                total_messages,
                returned: messages.len(),
                offset,
                messages,
            };
            println!("{}", serde_json::to_string_pretty(&output)?);
        }
        _ => {
            // Text output
            if paginated.is_empty() {
                println!("No messages found.");
                return Ok(());
            }

            println!(
                "Session {} ({} messages, showing {}-{})\n",
                &args.session_id[..8.min(args.session_id.len())],
                total_messages,
                offset + 1,
                (offset + paginated.len()).min(total_messages),
            );

            for (orig_idx, entry) in &paginated {
                let msg_type = entry.message_type();
                let timestamp = entry
                    .timestamp()
                    .map(|t| t.format("%H:%M:%S").to_string())
                    .unwrap_or_default();

                match detail {
                    "overview" => {
                        if let Some(text) = extract_user_prompt_text(entry) {
                            println!("[{orig_idx}] {timestamp} user: {}", truncate_text(&text, truncate_len));
                        }
                    }
                    "conversation" => {
                        let content = match entry {
                            LogEntry::User(_) => extract_user_prompt_text(entry)
                                .map(|t| truncate_text(&t, truncate_len)),
                            LogEntry::Assistant(_) => extract_assistant_summary(entry, truncate_len),
                            _ => None,
                        };
                        if let Some(text) = content {
                            println!("[{orig_idx}] {timestamp} {msg_type}: {text}");
                        }
                        if include_thinking {
                            if let Some(thinking) = extract_thinking_text(entry, thinking_max_len) {
                                println!("    thinking: {}", truncate_text(&thinking, 200));
                            }
                        }
                    }
                    "standard" => {
                        let content = match entry {
                            LogEntry::User(_) => extract_user_prompt_text(entry)
                                .map(|t| truncate_text(&t, truncate_len)),
                            LogEntry::Assistant(_) => extract_assistant_summary(entry, truncate_len),
                            LogEntry::System(sys) => sys.content.clone(),
                            _ => None,
                        };
                        let model_str = get_model(entry)
                            .map(|m| format!(" ({m})"))
                            .unwrap_or_default();
                        println!("[{orig_idx}] {timestamp} {msg_type}{model_str}:");
                        if let Some(text) = content {
                            println!("    {text}");
                        }
                        let tools = extract_tool_names(entry);
                        if !tools.is_empty() {
                            println!("    tools: {}", tools.join(", "));
                        }
                        if include_thinking {
                            if let Some(thinking) = extract_thinking_text(entry, thinking_max_len) {
                                println!("    thinking: {}", truncate_text(&thinking, 200));
                            }
                        }
                        println!();
                    }
                    _ => {
                        // "full"
                        let content = match entry {
                            LogEntry::User(_) => extract_user_prompt_text(entry)
                                .map(|t| truncate_text(&t, truncate_len)),
                            LogEntry::Assistant(_) => extract_assistant_summary(entry, truncate_len),
                            LogEntry::System(sys) => sys.content.clone(),
                            _ => None,
                        };
                        let model_str = get_model(entry)
                            .map(|m| format!(" ({m})"))
                            .unwrap_or_default();
                        println!("[{orig_idx}] {timestamp} {msg_type}{model_str}:");
                        if let Some(text) = content {
                            println!("    {text}");
                        }
                        // Show tool details
                        if let LogEntry::Assistant(a) = entry {
                            for t in a.message.tool_uses() {
                                let summary = extract_tool_input_summary(&t.name, &t.input);
                                let detail_str: Vec<String> = summary
                                    .iter()
                                    .map(|(k, v)| format!("{k}={v}"))
                                    .collect();
                                println!("    > {} {}", t.name, detail_str.join(" "));
                            }
                        }
                        if include_thinking {
                            if let Some(thinking) = extract_thinking_text(entry, thinking_max_len) {
                                println!("    thinking: {}", truncate_text(&thinking, 300));
                            }
                        }
                        println!();
                    }
                }
            }
        }
    }

    Ok(())
}

/// Build a JSON message output for a single entry.
fn build_message_output(
    index: usize,
    entry: &LogEntry,
    detail: &str,
    truncate_len: usize,
    include_thinking: bool,
    thinking_max_len: usize,
) -> Option<MessageOutput> {
    let msg_type = entry.message_type().to_string();
    let timestamp = entry.timestamp().map(|t| t.to_rfc3339());

    match detail {
        "overview" => {
            let content = extract_user_prompt_text(entry)
                .map(|t| truncate_text(&t, truncate_len));
            Some(MessageOutput {
                index,
                msg_type,
                timestamp,
                content,
                model: None,
                tool_calls: None,
                tool_details: None,
                has_thinking: None,
                thinking_preview: None,
            })
        }
        "conversation" => {
            let content = match entry {
                LogEntry::User(_) => extract_user_prompt_text(entry)
                    .map(|t| truncate_text(&t, truncate_len)),
                LogEntry::Assistant(_) => extract_assistant_summary(entry, truncate_len),
                _ => None,
            };
            let thinking = if include_thinking {
                extract_thinking_text(entry, thinking_max_len)
            } else {
                None
            };
            Some(MessageOutput {
                index,
                msg_type,
                timestamp,
                content,
                model: get_model(entry),
                tool_calls: None,
                tool_details: None,
                has_thinking: if has_thinking(entry) { Some(true) } else { None },
                thinking_preview: thinking,
            })
        }
        "standard" => {
            let content = match entry {
                LogEntry::User(_) => extract_user_prompt_text(entry)
                    .map(|t| truncate_text(&t, truncate_len)),
                LogEntry::Assistant(_) => extract_assistant_summary(entry, truncate_len),
                LogEntry::System(sys) => sys.content.clone(),
                _ => None,
            };
            let tool_names = extract_tool_names(entry);
            let thinking = if include_thinking {
                extract_thinking_text(entry, thinking_max_len)
            } else {
                None
            };
            Some(MessageOutput {
                index,
                msg_type,
                timestamp,
                content,
                model: get_model(entry),
                tool_calls: if tool_names.is_empty() { None } else { Some(tool_names) },
                tool_details: None,
                has_thinking: if has_thinking(entry) { Some(true) } else { None },
                thinking_preview: thinking,
            })
        }
        _ => {
            // "full"
            let content = match entry {
                LogEntry::User(_) => extract_user_prompt_text(entry)
                    .map(|t| truncate_text(&t, truncate_len)),
                LogEntry::Assistant(_) => extract_assistant_summary(entry, truncate_len),
                LogEntry::System(sys) => sys.content.clone(),
                _ => None,
            };
            let tool_names = extract_tool_names(entry);
            let tool_details: Vec<ToolDetailOutput> = if let LogEntry::Assistant(a) = entry {
                a.message
                    .tool_uses()
                    .iter()
                    .map(|t| {
                        let summary = extract_tool_input_summary(&t.name, &t.input);
                        ToolDetailOutput {
                            tool_name: t.name.clone(),
                            file_path: summary.get("file_path").cloned(),
                            command: summary.get("command").cloned(),
                            pattern: summary.get("pattern").cloned(),
                        }
                    })
                    .collect()
            } else {
                vec![]
            };
            let thinking = if include_thinking {
                extract_thinking_text(entry, thinking_max_len)
            } else {
                None
            };
            Some(MessageOutput {
                index,
                msg_type,
                timestamp,
                content,
                model: get_model(entry),
                tool_calls: if tool_names.is_empty() { None } else { Some(tool_names) },
                tool_details: if tool_details.is_empty() { None } else { Some(tool_details) },
                has_thinking: if has_thinking(entry) { Some(true) } else { None },
                thinking_preview: thinking,
            })
        }
    }
}
