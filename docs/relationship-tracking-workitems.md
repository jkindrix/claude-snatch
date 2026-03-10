# Relationship Tracking: Work Items

**Goal:** Make snatch aware of the relationships already encoded in Claude Code's raw data.

**Core discovery:** Claude Code already tracks session continuation (via `sessionId` field), session metadata (via `sessions-index.json`), human-readable names (via `slug`), and subagent provenance (via `progress` entries). Snatch ignores all of these.

---

## Work Items

### 1. Read `sessions-index.json`

Claude Code maintains a per-project index at `~/.claude/projects/<encoded>/sessions-index.json` with fields: `sessionId`, `fullPath`, `fileMtime`, `firstPrompt`, `summary`, `messageCount`, `created`, `modified`, `gitBranch`, `projectPath`, `isSidechain`.

**What to do:**
- Add a `SessionIndex` struct in `src/discovery/` to deserialize the index
- Load it in `ProjectDirectory` during session discovery
- Attach index metadata (created, firstPrompt, summary, slug) to `Session` structs
- Fall back gracefully when index is absent or stale

**Files affected:** `src/discovery/project.rs`, `src/discovery/session.rs`

**Why first:** Everything else builds on having richer session metadata available.

---

### 2. Detect session chains from `sessionId` field

When `claude --resume` or `claude --continue` creates a new file, the new file's internal `sessionId` points to the previous file's UUID. Chain rule: if `file_uuid != entry.sessionId`, then `entry.sessionId` is the parent file.

**What to do:**
- During discovery, read the first entry of each JSONL to extract the internal `sessionId`
- Compare to filename UUID — if different, record as continuation link
- Build chain graph: `HashMap<root_uuid, Vec<file_uuid>>` ordered by creation time
- Add to `Session`: `logical_session_id: Option<String>` (the root UUID), `chain_position: Option<usize>`
- Add `SessionChain` struct: `root_id`, `member_ids: Vec<String>`, `slug`, `created`, `modified`

**Files affected:** `src/discovery/session.rs`, `src/discovery/project.rs`, new `src/discovery/chain.rs`

**Key constraint:** Must not break existing single-file session behavior. Unchained sessions remain unchanged.

---

### 3. Surface `slug` as human-readable session name

Every message carries a `slug` field (adjective-adjective-noun pattern, e.g., "valiant-twirling-biscuit"). Stable across continuations. Already parsed by snatch but not surfaced.

**What to do:**
- Extract `slug` from first entry during quick metadata scan (already in `QuickMetadata` path)
- Add `slug: Option<String>` to `Session` struct
- Display slug in CLI output (`snatch list`, `snatch info`)
- Include slug in MCP `list_sessions` and `get_session_info` responses
- Support `find_session()` lookup by slug (exact or substring match)

**Files affected:** `src/discovery/session.rs`, `src/analytics/mod.rs`, `src/mcp_server/mod.rs`, `src/cli/commands/info.rs`, `src/cli/commands/list.rs`

---

### 4. Update `Session::parse()` to support chain-aware parsing

Currently each file is parsed independently. For chained sessions, we need to parse all files in order and produce a unified entry list.

**What to do:**
- Add `Session::parse_chain(chain: &SessionChain) -> Vec<LogEntry>` that concatenates entries from all member files in chain order
- Ensure `logicalParentUuid` bridging in `Conversation::from_entries()` works across file boundaries (likely already does since it's UUID-based, but verify)
- Add source file tracking to entries if needed for diagnostics

**Files affected:** `src/discovery/session.rs`, `src/reconstruction/mod.rs`

**Risk:** Large chains could produce very large entry vectors. Consider lazy/streaming approach if needed.

---

### 5. Update analytics to aggregate across chains

**No code changes needed.** `SessionAnalytics::from_conversation()` already works on any
`Conversation` regardless of source. Chain parsing (Item 4) produces a unified entry list
that builds into a single `Conversation`, so analytics automatically span the full chain
including correct duration, token totals, and message counts.

---

### 6. Update MCP tools to expose chain information

The MCP server is the primary consumer interface. It needs to know about chains.

**What to do:**
- `list_sessions`: Add `chain_id`, `chain_length`, `slug` fields to `SessionSummary`. Group chain members under their root by default, with option to expand.
- `get_session_info`: If session is part of a chain, include chain metadata (members, total duration, total messages). Resolve by slug.
- `get_session_messages`: Accept chain-aware mode — when requesting a chained session, optionally return messages across all member files.
- `get_project_history`: Use chain grouping so a 4-file chain shows as 1 session in project history.
- `search_sessions`: Search across chain members and report chain_id in results.
- `get_session_timeline`: Support chain-spanning timeline.

**Files affected:** `src/mcp_server/mod.rs`, `src/mcp_server/types.rs`

---

### 7. Update CLI commands to expose chain information

**What to do:**
- `snatch list`: Show slug, chain indicator (e.g., `[3 parts]`), use chain duration/dates
- `snatch info <session>`: Show chain info section when session is part of chain. Support lookup by slug.
- `snatch sessions` (if separate from list): Chain-grouped view
- Add `snatch chain` subcommand: `list` (all chains), `info <id>` (chain details), `members <id>` (file list)
- All session-accepting commands: resolve slug → session, resolve chain member → chain root

**Files affected:** `src/cli/commands/list.rs`, `src/cli/commands/info.rs`, new `src/cli/commands/chain.rs`, `src/cli/mod.rs`

---

### 8. Parse `progress` entries for subagent provenance

`progress` entries (type `"progress"`) contain `parentToolUseID` and `agentId` fields that link subagent sessions to the specific tool call that spawned them. Currently dropped entirely.

**What to do:**
- Add `Progress` variant to `LogEntry` enum with key fields: `parent_tool_use_id`, `tool_use_id`, `agent_id`, `data.type` (hook_progress/agent_progress/tool_progress)
- During hierarchy building, use `parentToolUseID` to link subagent sessions to their invoking tool call (not just temporal proximity)
- Surface in `get_session_info` / `snatch info`: "spawned by tool call X in parent session Y"

**Files affected:** `src/model/message.rs`, `src/parser/mod.rs`, `src/discovery/hierarchy.rs`

---

### 9. Index file-session relationships from `file-history-snapshot` entries

`file-history-snapshot` entries track which messages modified which files. This gives us a file→session index for free.

**What to do:**
- Parse `file-history-snapshot` entries (already deserialized but not indexed)
- Build reverse index: `file_path → Vec<(session_id, message_id, timestamp)>`
- Add MCP tool or extend `get_tool_calls`: "which sessions touched this file?"
- Add CLI: `snatch file-history <path>` — shows all sessions that modified a file

**Files affected:** `src/model/message.rs` (verify snapshot parsing), new `src/file_index.rs`, `src/mcp_server/mod.rs`, `src/cli/commands/`

---

### 10. Update session hook to leverage chain awareness

The `snatch-recall.sh` hook fires on startup/compact/resume. It should use chain context.

**What to do:**
- When injecting session context, use the logical session (full chain) not just the current file
- Show slug in hook output for human readability
- On compact injection, include chain context: "this is part N of session <slug>"

**Files affected:** `~/.claude/hooks/snatch-recall.sh`

---

## Execution Order

Items are ordered by dependency. Each builds on the previous.

| # | Item | Depends On | Scope | Status |
|---|------|-----------|-------|--------|
| 1 | Read `sessions-index.json` | — | Discovery | Done (438646d) |
| 2 | Detect session chains | 1 | Discovery | Done (438646d) |
| 3 | Surface `slug` | 1 | Discovery + Display | Done (438646d) |
| 4 | Chain-aware parsing | 2 | Parser + Reconstruction | Done (96e5cfb) |
| 5 | Chain-aware analytics | 4 | Analytics | Done (no changes needed) |
| 6 | MCP tool updates | 2, 3, 5 | MCP Server | Done (5a7776b, 6dd9e66) |
| 7 | CLI command updates | 2, 3, 5 | CLI | Done (10a3247, 6dd9e66) |
| 8 | Parse `progress` entries | — | Model + Hierarchy | Done (deb8fc9) |
| 9 | File-session index | — | New module | Done (c9a8c36) |
| 10 | Hook updates | 2, 3 | Hook script | Done |

All items complete.
