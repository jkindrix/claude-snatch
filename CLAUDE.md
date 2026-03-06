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

This project provides an MCP server (`snatch serve-mcp`) that exposes 12 tools for querying Claude Code session history. When you need to recall what happened in previous sessions or understand the narrative of past work, use these tools:

| Need | Tool | Example |
|------|------|---------|
| What have we been working on? | `get_project_history` | project="claude-snatch", period="7d" |
| What happened in a session? | `get_session_timeline` | session_id="abc123" |
| Quick session overview | `get_session_digest` | session_id="abc123" |
| Read specific messages | `get_session_messages` | session_id="abc123", detail="standard" |
| Recover decision rationale | `get_session_messages` | session_id="abc123", include_thinking=true |
| Find where X was discussed | `search_sessions` | pattern="authentication", project="myproject" |
| Search reasoning/decisions | `search_sessions` | pattern="decided|because", scope="thinking" |
| What went wrong & how fixed? | `get_session_lessons` | session_id="abc123" |
| What files were changed? | `get_tool_calls` | session_id="abc123", tool_filter="Write,Edit" |
| Track long-term goals | `manage_goals` | operation="list", project="snatch" |
| Capture tactical work state | `manage_notes` | operation="add", project="snatch", text="..." |
| List all sessions | `list_sessions` | project="claude-snatch" |
| Session metadata | `get_session_info` | session_id="abc123" |
| Usage statistics | `get_stats` | project="claude-snatch" |

### Detail Levels for get_session_messages

- **overview**: User prompts only, truncated. Fast orientation.
- **conversation**: User prompts + assistant text responses, skipping tool-only turns. Best for understanding the dialogue.
- **standard**: User + assistant text, tool names listed. Good balance.
- **full**: Includes tool call details (file paths, commands). For deep investigation.

### Thinking Block Recovery

Compaction **always** drops thinking/reasoning blocks (100% loss rate). These contain decision rationale, evidence chains, and alternative evaluation. Two ways to recover them:

- `get_session_messages` with `include_thinking=true` — returns thinking text alongside messages
- `search_sessions` with `scope="thinking"` — search through reasoning blocks

### Goal Persistence

Goals survive compaction and sessions. Use `manage_goals` to track long-term intentions:

- `manage_goals(operation="add", project="snatch", text="Build digest tool")` — add a goal
- `manage_goals(operation="update", project="snatch", id=1, status="done", progress="Shipped")` — update progress
- `manage_goals(operation="list", project="snatch")` — see all goals
- `manage_goals(operation="remove", project="snatch", id=1)` — remove a goal

Active goals are auto-injected by the SessionStart hook on startup and compaction.
Status values: `open`, `in_progress`, `done`, `abandoned`.
Storage: `~/.claude/projects/<project>/memory/goals.json`

### Tactical Notes

Notes capture mid-work state that survives compaction. Unlike goals (strategic, multi-session), notes are tactical ("tried X, failed because Y, now doing Z").

- `manage_notes(operation="add", project="snatch", text="Tried redis caching, failed due to connection pooling")` — add a note
- `manage_notes(operation="list", project="snatch")` — see all notes
- `manage_notes(operation="remove", project="snatch", id=1)` — remove a specific note
- `manage_notes(operation="clear", project="snatch")` — clear all notes

Notes are auto-injected by the SessionStart hook on startup and compaction.
Storage: `~/.claude/projects/<project>/memory/notes.json`

### Session Digest

`get_session_digest` provides a compact summary for quick orientation:
- Key human prompts (first 3)
- Files touched (basenames from Write/Edit/Read)
- Top tools by frequency
- Error count and compaction count
- Decision keywords from thinking blocks

The digest is auto-injected after compaction via the SessionStart hook.

### Proactive Goal Management

**You MUST proactively manage goals.** Do not wait to be asked.

**When to add a goal:**
- User states a multi-step or multi-session intention ("build X", "fix all Y", "redesign Z")
- You recognize work that will span beyond the current task
- A compaction is likely before the work will finish

**When to update a goal:**
- You complete a significant milestone toward the goal
- The approach changes materially
- A goal is finished (`status="done"`) or abandoned

**When to check goals:**
- After compaction recovery (they're auto-injected, but verify they're still accurate)
- Before ending a significant work session
- When the user asks "what are we working on?" or "what's left?"

This is not optional. Goal amnesia after compaction is the #1 pain point. If you forget to track goals, the next session starts blind.

### Proactive Note-Taking

**Use tactical notes to capture work state that would be lost on compaction.**

**When to add a note:**
- You've tried an approach that failed — record what failed and why
- You're mid-way through a multi-step task — record current step and next steps
- You've discovered a non-obvious constraint or gotcha
- You're about to do something complex where losing context would force restart

**When to clear notes:**
- After a significant work unit is complete and committed
- When notes are stale and no longer relevant
- At the start of a fundamentally new task

Notes are lightweight and disposable. Don't overthink them — just write what future-you needs to know.

### Usage Guidelines

- Start with `get_project_history` or `list_sessions` to orient yourself
- Use `detail="conversation"` for reading the human-AI dialogue without tool noise
- Use `detail="overview"` for quick orientation on what was asked
- Always filter by project when possible to reduce noise
- The `search_sessions` tool supports regex patterns and scope="thinking" for reasoning
- Timeline collapses consecutive tool-only turns automatically for cleaner output
- Timeline is the best tool for understanding "what happened in order"
- All tools see content across compaction boundaries (pre/post-compact messages are both visible)
- Use `get_session_lessons` after compaction to recover operational gotchas and avoid retrying failed approaches
