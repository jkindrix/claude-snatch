//! Messages command implementation.
//!
//! Reads conversation messages from a session at different detail levels,
//! mirroring the MCP `get_session_messages` tool for CLI use.

use std::collections::HashMap;

use crate::analysis::extraction::{
    extract_assistant_summary, extract_thinking_text, extract_tool_input_summary,
    extract_tool_names, extract_user_prompt_text, get_model, has_thinking, is_human_prompt,
    truncate_text,
};
use crate::analysis::subagents::{match_subagents, SubagentMatch, SubagentMatches};
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
    /// Subagents present on disk but not joinable to a specific spawn call.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    unmatched_subagents: Vec<UnmatchedSubagentOutput>,
}

#[derive(serde::Serialize)]
struct UnmatchedSubagentOutput {
    session_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    message_count: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    result_preview: Option<String>,
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
    git_branch: Option<String>,
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
    #[serde(skip_serializing_if = "Option::is_none")]
    subagent_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    prompt: Option<String>,
    /// For Agent/Task calls: the spawned subagent's session id, when matched.
    #[serde(skip_serializing_if = "Option::is_none")]
    subagent_session_id: Option<String>,
    /// Preview of the subagent's final assistant message (its result), truncated.
    #[serde(skip_serializing_if = "Option::is_none")]
    subagent_result_preview: Option<String>,
    /// Full subagent transcript, present only with --subagent-transcripts.
    #[serde(skip_serializing_if = "Option::is_none")]
    subagent_transcript: Option<Vec<MessageOutput>>,
}

/// Run the messages command.
pub fn run(cli: &Cli, args: &MessagesArgs) -> Result<()> {
    let claude_dir = get_claude_dir(cli.claude_dir.as_ref())?;

    let session =
        claude_dir
            .find_session(&args.session_id)?
            .ok_or_else(|| SnatchError::SessionNotFound {
                session_id: args.session_id.clone(),
            })?;

    let project_path = session.project_path().to_string();
    let (entries, unparsed) = session.parse_with_options_counted(cli.max_file_size)?;
    let conversation = Conversation::from_entries(entries)?;

    let detail = args.detail.as_str();
    let msg_type_filter = args.message_type.as_str();
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

    // Match Agent/Task calls to spawned subagents (only "full" detail renders tool
    // details). Uses the unfiltered thread for spawn-order joining.
    let subagent_matches: SubagentMatches = if detail == "full" {
        match_subagents(
            &session,
            &conversation.main_thread_entries(),
            cli.max_file_size,
        )
    } else {
        SubagentMatches::default()
    };

    let mut main_entries: Vec<&LogEntry> = conversation.main_thread_entries();

    // Filter by message type
    match msg_type_filter {
        "user" => main_entries.retain(|e| is_human_prompt(e)),
        "assistant" => main_entries.retain(|e| matches!(e, LogEntry::Assistant(_))),
        "system" => main_entries.retain(|e| matches!(e, LogEntry::System(_))),
        _ => {}
    }

    // Filter by timestamp window
    if args.after.is_some() || args.before.is_some() {
        use chrono::{DateTime, Utc};
        let after = if let Some(ref ts) = args.after {
            let systime = super::parse_date_filter(ts)?;
            Some(DateTime::<Utc>::from(systime))
        } else {
            None
        };
        let before = if let Some(ref ts) = args.before {
            let systime = super::parse_date_filter(ts)?;
            Some(DateTime::<Utc>::from(systime))
        } else {
            None
        };
        main_entries.retain(|e| {
            if let Some(ts) = e.timestamp() {
                if let Some(ref a) = after {
                    if ts < *a {
                        return false;
                    }
                }
                if let Some(ref b) = before {
                    if ts > *b {
                        return false;
                    }
                }
                true
            } else {
                true
            }
        });
    }

    // Pre-filter by detail level
    match detail {
        "overview" => {
            main_entries.retain(|e| is_human_prompt(e));
        }
        "conversation" => {
            main_entries.retain(|e| match e {
                LogEntry::User(_) => is_human_prompt(e),
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
                    build_message_output(
                        *orig_idx,
                        entry,
                        detail,
                        truncate_len,
                        include_thinking,
                        thinking_max_len,
                        &subagent_matches.matched,
                        args.subagent_transcripts,
                        cli.max_file_size,
                    )
                })
                .collect();

            let output = MessagesOutput {
                session_id: args.session_id.clone(),
                project_path,
                total_messages,
                returned: messages.len(),
                offset,
                messages,
                unmatched_subagents: subagent_matches
                    .unmatched
                    .iter()
                    .map(|m| UnmatchedSubagentOutput {
                        session_id: m.session_id.clone(),
                        message_count: m.message_count,
                        result_preview: m.result_preview.clone(),
                    })
                    .collect(),
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
            if unparsed > 0 {
                println!(
                    "⚠ {unparsed} line{} could not be parsed (dropped from this view)\n",
                    if unparsed == 1 { "" } else { "s" }
                );
            }

            for (orig_idx, entry) in &paginated {
                let msg_type = entry.message_type();
                let timestamp = entry
                    .timestamp()
                    .map(|t| t.format("%H:%M:%S").to_string())
                    .unwrap_or_default();

                match detail {
                    "overview" => {
                        if let Some(text) = extract_user_prompt_text(entry) {
                            println!(
                                "[{orig_idx}] {timestamp} user: {}",
                                truncate_text(&text, truncate_len)
                            );
                        }
                    }
                    "conversation" => {
                        let content = match entry {
                            LogEntry::User(_) => extract_user_prompt_text(entry)
                                .map(|t| truncate_text(&t, truncate_len)),
                            LogEntry::Assistant(_) => {
                                extract_assistant_summary(entry, truncate_len)
                            }
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
                            LogEntry::Assistant(_) => {
                                extract_assistant_summary(entry, truncate_len)
                            }
                            LogEntry::System(sys) => sys.content.clone(),
                            _ => None,
                        };
                        let tools = extract_tool_names(entry);
                        let thinking = if include_thinking {
                            extract_thinking_text(entry, thinking_max_len)
                        } else {
                            None
                        };

                        // Skip entries with nothing to show in text mode
                        if content.is_none() && tools.is_empty() && thinking.is_none() {
                            continue;
                        }

                        let model_str = get_model(entry)
                            .map(|m| format!(" ({m})"))
                            .unwrap_or_default();
                        println!("[{orig_idx}] {timestamp} {msg_type}{model_str}:");
                        if let Some(text) = content {
                            println!("    {text}");
                        }
                        if !tools.is_empty() {
                            println!("    tools: {}", tools.join(", "));
                        }
                        if let Some(thinking) = thinking {
                            println!("    thinking: {}", truncate_text(&thinking, 200));
                        }
                        println!();
                    }
                    _ => {
                        // "full"
                        let content = match entry {
                            LogEntry::User(_) => extract_user_prompt_text(entry)
                                .map(|t| truncate_text(&t, truncate_len)),
                            LogEntry::Assistant(_) => {
                                extract_assistant_summary(entry, truncate_len)
                            }
                            LogEntry::System(sys) => sys.content.clone(),
                            _ => None,
                        };
                        let tool_uses: Vec<_> = if let LogEntry::Assistant(a) = entry {
                            a.message.tool_uses()
                        } else {
                            vec![]
                        };
                        let thinking = if include_thinking {
                            extract_thinking_text(entry, thinking_max_len)
                        } else {
                            None
                        };

                        // Skip entries with nothing to show in text mode
                        if content.is_none() && tool_uses.is_empty() && thinking.is_none() {
                            continue;
                        }

                        let model_str = get_model(entry)
                            .map(|m| format!(" ({m})"))
                            .unwrap_or_default();
                        println!("[{orig_idx}] {timestamp} {msg_type}{model_str}:");
                        if let Some(text) = content {
                            println!("    {text}");
                        }
                        for t in &tool_uses {
                            let summary = extract_tool_input_summary(&t.name, &t.input);
                            let detail_str: Vec<String> =
                                summary.iter().map(|(k, v)| format!("{k}={v}")).collect();
                            println!("    > {} {}", t.name, detail_str.join(" "));

                            // Attach the spawned subagent's work to its Agent/Task call.
                            if let Some(m) = subagent_matches.matched.get(&t.id) {
                                let msgs = m
                                    .message_count
                                    .map(|n| format!(" ({n} msgs)"))
                                    .unwrap_or_default();
                                println!("      -> subagent {}{}", m.session_id, msgs);
                                if let Some(preview) = &m.result_preview {
                                    println!("         result: {}", truncate_text(preview, 200));
                                }
                                if args.subagent_transcripts {
                                    for sub in render_subagent_transcript_cli(
                                        &m.path,
                                        include_thinking,
                                        cli.max_file_size,
                                    ) {
                                        let c = sub.content.unwrap_or_default();
                                        println!(
                                            "         [{}] {}: {}",
                                            sub.index,
                                            sub.msg_type,
                                            truncate_text(&c, 200)
                                        );
                                    }
                                }
                            }
                        }
                        if let Some(thinking) = thinking {
                            println!("    thinking: {}", truncate_text(&thinking, 300));
                        }
                        println!();
                    }
                }
            }

            // Subagents present on disk but not joinable to a specific spawn call.
            // Emitting a marker keeps a present subagent from vanishing silently.
            for m in &subagent_matches.unmatched {
                let msgs = m
                    .message_count
                    .map(|n| format!("{n} msgs"))
                    .unwrap_or_else(|| "? msgs".to_string());
                println!("[subagent {}: {}, unlinked]", m.session_id, msgs);
                if let Some(preview) = &m.result_preview {
                    println!("    result: {}", truncate_text(preview, 200));
                }
                println!();
            }
        }
    }

    Ok(())
}

/// Render a subagent's main thread as message outputs (standard detail: user and
/// assistant text plus tool names; tool details are not expanded recursively).
fn render_subagent_transcript_cli(
    path: &std::path::Path,
    include_thinking: bool,
    max_file_size: Option<u64>,
) -> Vec<MessageOutput> {
    let entries = crate::discovery::Session::from_path(path, "")
        .ok()
        .and_then(|s| s.parse_with_options(max_file_size).ok())
        .unwrap_or_default();
    let Ok(conversation) = Conversation::from_entries(entries) else {
        return vec![];
    };
    conversation
        .main_thread_entries()
        .iter()
        .enumerate()
        .map(|(i, entry)| {
            let content = match entry {
                LogEntry::User(_) => {
                    extract_user_prompt_text(entry).map(|t| truncate_text(&t, 500))
                }
                LogEntry::Assistant(_) => extract_assistant_summary(entry, 500),
                LogEntry::System(sys) => sys.content.clone(),
                _ => None,
            };
            let tool_names = extract_tool_names(entry);
            let thinking = if include_thinking {
                extract_thinking_text(entry, 1000)
            } else {
                None
            };
            MessageOutput {
                index: i,
                msg_type: entry.message_type().to_string(),
                timestamp: entry.timestamp().map(|t| t.to_rfc3339()),
                content,
                git_branch: entry.git_branch().map(|s| s.to_string()),
                model: get_model(entry),
                tool_calls: if tool_names.is_empty() {
                    None
                } else {
                    Some(tool_names)
                },
                tool_details: None,
                has_thinking: if has_thinking(entry) {
                    Some(true)
                } else {
                    None
                },
                thinking_preview: thinking,
            }
        })
        .collect()
}

/// Build a JSON message output for a single entry.
#[allow(clippy::too_many_arguments)]
fn build_message_output(
    index: usize,
    entry: &LogEntry,
    detail: &str,
    truncate_len: usize,
    include_thinking: bool,
    thinking_max_len: usize,
    subagent_matches: &HashMap<String, SubagentMatch>,
    include_subagent_transcripts: bool,
    max_file_size: Option<u64>,
) -> Option<MessageOutput> {
    let msg_type = entry.message_type().to_string();
    let timestamp = entry.timestamp().map(|t| t.to_rfc3339());
    let git_branch = entry.git_branch().map(|s| s.to_string());

    match detail {
        "overview" => {
            let content = extract_user_prompt_text(entry).map(|t| truncate_text(&t, truncate_len));
            Some(MessageOutput {
                index,
                msg_type,
                timestamp,
                content,
                git_branch,
                model: None,
                tool_calls: None,
                tool_details: None,
                has_thinking: None,
                thinking_preview: None,
            })
        }
        "conversation" => {
            let content = match entry {
                LogEntry::User(_) => {
                    extract_user_prompt_text(entry).map(|t| truncate_text(&t, truncate_len))
                }
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
                git_branch,
                model: get_model(entry),
                tool_calls: None,
                tool_details: None,
                has_thinking: if has_thinking(entry) {
                    Some(true)
                } else {
                    None
                },
                thinking_preview: thinking,
            })
        }
        "standard" => {
            let content = match entry {
                LogEntry::User(_) => {
                    extract_user_prompt_text(entry).map(|t| truncate_text(&t, truncate_len))
                }
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
                git_branch,
                model: get_model(entry),
                tool_calls: if tool_names.is_empty() {
                    None
                } else {
                    Some(tool_names)
                },
                tool_details: None,
                has_thinking: if has_thinking(entry) {
                    Some(true)
                } else {
                    None
                },
                thinking_preview: thinking,
            })
        }
        _ => {
            // "full"
            let content = match entry {
                LogEntry::User(_) => {
                    extract_user_prompt_text(entry).map(|t| truncate_text(&t, truncate_len))
                }
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
                        let matched = subagent_matches.get(&t.id);
                        ToolDetailOutput {
                            tool_name: t.name.clone(),
                            file_path: summary.get("file_path").cloned(),
                            command: summary.get("command").cloned(),
                            pattern: summary.get("pattern").cloned(),
                            subagent_type: summary.get("subagent_type").cloned(),
                            description: summary.get("description").cloned(),
                            prompt: summary.get("prompt").cloned(),
                            subagent_session_id: matched.map(|m| m.session_id.clone()),
                            subagent_result_preview: matched.and_then(|m| m.result_preview.clone()),
                            subagent_transcript: matched
                                .filter(|_| include_subagent_transcripts)
                                .map(|m| {
                                    render_subagent_transcript_cli(
                                        &m.path,
                                        include_thinking,
                                        max_file_size,
                                    )
                                }),
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
                git_branch,
                model: get_model(entry),
                tool_calls: if tool_names.is_empty() {
                    None
                } else {
                    Some(tool_names)
                },
                tool_details: if tool_details.is_empty() {
                    None
                } else {
                    Some(tool_details)
                },
                has_thinking: if has_thinking(entry) {
                    Some(true)
                } else {
                    None
                },
                thinking_preview: thinking,
            })
        }
    }
}
