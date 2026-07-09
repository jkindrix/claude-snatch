---
name: session-audit
description: Audits a past Claude Code session chunk-by-chunk using snatch prompt-boundary retrieval — maps where time, tool calls, errors, and corrections concentrated, reads the narrative per chunk, and verifies claims against the commands that actually ran. Use when the user asks to audit, review, walk through, replay, or analyze a previous session or conversation, verify what an agent really did versus what it claimed, find where a session went wrong, or asks "what happened in session X".
when_to_use: Trigger phrases like "audit that session", "walk through the session", "what happened in", "did it actually do X", "review yesterday's session", "session post-mortem". Requires the snatch CLI. For auditing code changes instead of session history, use audit-changes.
---

# Session Audit

Audit unit: a **chunk** = one prompt boundary (typed user prompt, or queued
mid-turn steering prompt) plus everything it produced, up to the next prompt.
Ladder: **map → narrative → evidence** — spend tokens only where the map says.

All commands are `snatch` CLI via Bash. The MCP server `snatch` mirrors them
(`get_session_messages` with `chunk`/`errors_only`/`max_text_len`,
`get_tool_calls` with `chunk`) — but a long-running MCP server may predate the
installed binary, so prefer the CLI.

## Workflow

Copy this checklist and track progress:

```
Audit Progress:
- [ ] 1. Locate the session
- [ ] 2. Map it
- [ ] 3. Pick targets from map signals
- [ ] 4. Read narrative per target chunk
- [ ] 5. Verify claims against evidence
- [ ] 6. Report (commentary + notes)
```

**1. Locate** — `snatch list sessions -p <project-substring> -n 10`
(add `--subagents` for agent sidecars; resume chains collapse to one row).

**2. Map** — `snatch chunks <session-id>`
One line per chunk: index, start time, wall-clock span, entry/tool counts,
`⚠N errors`, `(+N attached)` async results, `(queued)` steering prompts,
`└ abandoned branch` rewinds.

**3. Pick targets.** Read the map before pulling any chunk:
- `⚠ errors` → failure sites
- long span + high tool count → where the work (or churn) happened
- `(queued)` → mid-turn corrections; the user redirected the agent
- abandoned branches → the user rewound; compare with what replaced them
- many tiny chunks → back-and-forth; possible thrash

**4. Narrative** — `snatch messages <id> --chunk <N|A-B> -D conversation`
Dialogue only; ~500-char truncation. This is the agent's *self-report*.

**5. Evidence** — never grade on the narrative alone:
- Errors: `snatch messages <id> --chunk <N> --errors-only -D full`
  (each failing command with its `✗ error` text)
- Claims vs reality: MCP `get_tool_calls` with `chunk="N"` — every command,
  file path, and error state that actually ran; no narrative
- Anything else: re-pull the chunk at `-D full`

**6. Report.** Lead with the arc (what the session accomplished), then
per-chunk commentary anchored to indices, then verified findings (claim →
evidence). Persist durable lessons via `manage_notes` / `manage_decisions`
only if the user wants them kept.

## Audit types

| Ask | Route |
|---|---|
| "what went wrong" | map errors → step 5 error drill-down → was it diagnosed or papered over? |
| "did it really do X" | find the claim chunk → `get_tool_calls chunk=N` → match commands to claim |
| "how did it handle my corrections" | `(queued)` chunks + abandoned branches → read chunk at conversation detail |
| "where did the time go" | span column → skim heavy chunks with `--max-text-len 100` |
| whole-session walkthrough | chunks in order at conversation detail; evidence-check anything surprising |

## Token control

- `--max-text-len <chars>` overrides truncation: 80–150 to skim, 2000+ to read fully
- `-l 0` = unlimited messages (default 50); `-D overview` lists prompts at chunk indices
- Accumulate ranges (`--chunk 2-5`) instead of whole-session pulls

## Pitfalls

- The conversation view is the agent's self-report; a session that narrates
  confidently can still have failed. Evidence-check before concluding.
- Thinking blocks are empty in sessions from recent Claude Code versions —
  reasoning is recoverable only from what the agent said aloud.
- Filtered views renumber indices; cross-reference chunks by timestamp.
- Sessions with zero chunks (no prompts) are snapshot/metadata files, not
  conversations — say so rather than forcing an audit.

## Skill feedback

If a step here didn't match reality (wrong flag, missing capability,
misleading guidance), say so to the user in one line at the end of the audit.
Do not edit this skill unprompted.
