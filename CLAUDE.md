# Claude-Snatch

Rust CLI tool for extracting, analyzing, and searching Claude Code conversation logs.

## Build

```bash
cargo build                    # standard build
cargo build --features mcp     # with MCP server
cargo build --features tui     # with TUI
cargo test                     # run tests
```

## Session History Recall (snatch MCP)

This project provides an MCP server (`snatch serve-mcp`) that exposes 8 tools for querying Claude Code session history. When you need to recall what happened in previous sessions or understand the narrative of past work, use these tools:

| Need | Tool | Example |
|------|------|---------|
| What have we been working on? | `get_project_history` | project="claude-snatch", period="7d" |
| What happened in a session? | `get_session_timeline` | session_id="abc123" |
| Read specific messages | `get_session_messages` | session_id="abc123", detail="standard" |
| Find where X was discussed | `search_sessions` | pattern="authentication", project="myproject" |
| What files were changed? | `get_tool_calls` | session_id="abc123", tool_filter="Write,Edit" |
| List all sessions | `list_sessions` | project="claude-snatch" |
| Session metadata | `get_session_info` | session_id="abc123" |
| Usage statistics | `get_stats` | project="claude-snatch" |

### Detail Levels for get_session_messages

- **overview**: User prompts only, truncated. Fast orientation.
- **conversation**: User prompts + assistant text responses, skipping tool-only turns. Best for understanding the dialogue.
- **standard**: User + assistant text, tool names listed. Good balance.
- **full**: Includes tool call details (file paths, commands). For deep investigation.

### Usage Guidelines

- Start with `get_project_history` or `list_sessions` to orient yourself
- Use `detail="conversation"` for reading the human-AI dialogue without tool noise
- Use `detail="overview"` for quick orientation on what was asked
- Always filter by project when possible to reduce noise
- The `search_sessions` tool supports regex patterns
- Timeline collapses consecutive tool-only turns automatically for cleaner output
- Timeline is the best tool for understanding "what happened in order"
- All tools see content across compaction boundaries (pre/post-compact messages are both visible)
