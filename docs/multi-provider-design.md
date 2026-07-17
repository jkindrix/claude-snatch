# Multi-Provider Ingestion Design

**Status:** Architecture confirmed (decision #30, 2026-07-17). Phase plan below.
**Goal:** goal #18 — extend snatch (CLI + MCP) to ingest and analyze session logs
from agentic coding tools beyond Claude Code, designed for future extensibility.
First target: OpenAI Codex CLI.

## Architecture decision

**Option C — provider seam + normalize into the existing internal model.**

1. Extract a `SourceProvider` seam at the pipeline's ends (discovery +
   parse-to-entries). Claude Code becomes provider #1 with zero behavior
   change; that refactor is its own verifiable milestone. This is the missing
   dual of the existing `Exporter` seam.
2. Normalize other providers into `LogEntry` (the crate's universal currency:
   355 match sites across 41 files), under a strict fidelity contract:
   - every `Session` carries a `provider` tag;
   - every normalized entry retains its native raw line — `raw-jsonl` export
     stays byte-faithful to the *source* format;
   - synthesized linkage fields (e.g. `parentUuid` := previous entry,
     `message.id` := `turn_id`) are documented as derived, never claimed
     native.
3. The CC-semantic gates become provider-parameterized instead of hard-coded:
   pricing table (`model/usage.rs::for_model`), `is_human_prompt` noise rules
   (`analysis/extraction.rs`), tool-name registries (`model/content.rs`),
   prompt-boundary rules (chunking), subagent matching, chain semantics.

Rejected: **A (pure adapter)** — fastest but silently bleeds CC semantics and
weakens the fidelity story; **B (trait-generic middle)** — rewrites the whole
middle (41 files) for isolation the evidence says normalization already
provides.

## Evidence base (research round, 2026-07-17)

### Codex rollout format (corpus of 222 real files, Sep 2025–Jul 2026, + source at rust-v0.144.5)

- Envelope: one JSON object per line, `{timestamp, type, payload}`
  (`RolloutLine`/`RolloutItem`, `codex-rs/protocol/src/protocol.rs`). Types:
  `session_meta`, `response_item`, `event_msg`, `turn_context`, `compacted`,
  `world_state` (new 2026-07), `inter_agent_communication(_metadata)`.
- Two parallel streams: `response_item` = model-API record (message with
  user/assistant/developer/system roles, `reasoning`, `function_call`/
  `function_call_output` joined by `call_id`, `custom_tool_call`
  (apply_patch), `local_shell_call`, `web_search_call`); `event_msg` =
  UI/runtime record (`user_message`, `agent_message`, `agent_reasoning`,
  `token_count`, `turn_started/complete/aborted`, `thread_rolled_back`,
  `context_compacted`, review-mode events). Content duplicated across both;
  dedup policy needed (lean: response_item authoritative for content,
  event_msg for user-facing text, reasoning summaries, token counts —
  validate empirically in Phase B).
- No per-line ids, no parent links, no version field. Ordering is flat append;
  `turn_id` groups turns (also on `turn_context` and via
  `internal_chat_message_metadata_passthrough`); `session_meta.cli_version`
  is the only version marker.
- Resume **appends to the same file**. Fork creates a new file (new UUIDv7
  thread id, `forked_from_id` in meta) and copies truncated history in —
  including the source's `session_meta` lines (16 such files in the corpus).
- Subagents: separate rollout files with `parent_thread_id` +
  `source: {subagent: …}`; `thread_spawn_edges` table in state DB.
- Compaction: `compacted` items carry summary + full `replacement_history` +
  UUIDv7 window-id chain (`window_id`/`previous_window_id`/`first_window_id`/
  `window_number`) — richer than CC's compact boundary.
- Reasoning: plaintext summaries always persisted (`reasoning.summary`,
  legacy `event_msg/agent_reasoning`); full CoT encrypted
  (`encrypted_content`). Better than modern CC (which persists nothing
  readable).
- Metadata: `session_meta` has session/thread id (UUIDv7 = filename uuid),
  cwd, originator, cli_version, source (cli/vscode/exec/mcp/subagent…),
  `git {commit_hash, branch, repository_url}`, model_provider,
  base_instructions, history_mode. `turn_context` per turn: cwd, model,
  approval/sandbox policy, collaboration_mode, effort.
- Layout: `$CODEX_HOME/sessions/YYYY/MM/DD/rollout-<local-ts>-<uuid>.jsonl`,
  archived twin under `archived_sessions/`, cold files as `.jsonl.zst` (since
  ~v0.137, early June 2026; plain `.jsonl` wins when both exist),
  `session_index.jsonl` (thread names), `state_5.sqlite` (threads index —
  rebuildable from JSONL; 40 sqlx migrations), `history.jsonl` (prompt-only).
- Known format hazards:
  1. Files from Codex ≤0.31.0 (before 2025-09-10) use a bare pre-envelope
     format (naked ResponseItem lines, minimal meta, `record_type: state`
     markers). This corpus is entirely envelope-era; others won't be.
  2. `history_mode: "paginated"` (per-thread, default legacy) changes the
     persisted vocabulary to `item_completed` TurnItems; a merged post-0.144.5
     PR adds ordinals. Schema is actively moving.
  3. session_meta field sets drift heavily across 10 months (base_instructions
     82/238, thread_source 77/238, history_mode/context_window 19/238);
     2 unparseable lines exist in the corpus. Doctor-style drift detection is
     mandatory from day one.

### snatch coupling map (chokepoints)

Pipeline: discover → parse → `LogEntry` → `Conversation` → analyses/export.
Coupling concentrated at the ends. Chokepoints: `LogEntry`
(model/message.rs), `JsonlParser` (parser/mod.rs — salvage anchors are
CC-shaped), `Conversation::from_entries` (reconstruction — needs
uuid/parentUuid/message.id), `Session`+`ClaudeDirectory` (discovery — layout,
lossy path decode, sidecars, chains), `is_human_prompt`/`is_prompt_boundary`,
`ModelPricing::for_model`, `SessionAnalytics::from_conversation`, `Exporter`
(the one existing seam, provider-neutral already), `ContentBlock`+tool_names.
`LogEntry` is already format-tolerant (Unknown variants, WithUnknown flatten,
Other(String) enums) — the fidelity-hardening work is what makes Option C
cheap.

### Tool landscape (July 2026)

Format families: (a) CC-style per-project JSONL with uuid/parentUuid trees —
Qwen Code (near-clone of CC incl. path sanitization), Pi (only tool with an
officially documented, versioned format), Gemini CLI post-2026-04 (JSONL),
Copilot CLI `events.jsonl` + SQLite index, Mistral Vibe, Kimi; (b) SQLite with
JSON payload columns — opencode ≥1.2, Goose ≥1.10, Cursor (closed,
part-protobuf), Crush, Warp, Devin; (c) single JSON per session — Amp (uses
Anthropic-style content blocks), Cline, old Gemini; (d) markdown — Aider.
Four tools migrated storage formats mid-life within ten months; almost nothing
is versioned in-band. No interchange standard fit for internal use: ATIF
(Harbor, v1.7) is the closest semantic match but young/lossy on DAG lineage,
compaction, queued-prompt provenance; OTel GenAI semconv is span-shaped and
experimental. Both are candidate *export* targets later. Conclusion: own
internal model, native logs as source of truth, adapters per provider — and
the `SourceProvider` trait must support DB-backed discovery, not just
directory walks.

## Phase plan

- **Phase A — seam extraction (refactor, zero behavior change).**
  `SourceProvider` trait over discovery + parse; `ClaudeCodeProvider` is the
  only impl; full test suite + snapshot exports must be byte-identical before
  and after. Session gains a `provider` field (always "claude-code").
- **Phase B — Codex provider (read-only ingestion).** Discovery
  ($CODEX_HOME/sessions + archived_sessions, zstd, filename parsing,
  session_index/state_5 as optional accelerators), envelope parser, normalizer
  into LogEntry (turn_id grouping, call_id joins, synthesized parent links,
  native-raw retention), fork/subagent linking, drift detection (doctor
  coverage for unknown types/fields). Empirically settle the two-stream dedup
  policy and steered-prompt persistence. Milestone: list/info/messages/
  timeline/export work on real Codex sessions.
- **Phase C — semantic tuning.** Prompt-boundary chunking for Codex, lessons
  noise filters for Codex tools, OpenAI model pricing (or explicit unpriced),
  compaction/window-chain surfacing, fork lineage in chain views.
- **Phase D — cross-provider UX.** Unified project history across providers
  (same cwd/git identity), provider filters in CLI/MCP, and the long-deferred
  union view (note #4). Candidate export targets: ATIF, OTel GenAI.

## Open questions (to settle in-phase)

1. Two-stream dedup policy (Phase B, empirical).
2. Steered/queued prompt persisted shape (Phase B, empirical — inferred from
   inject.rs code paths only).
3. `world_state` / `ghost_snapshot` semantics (Phase B).
4. GPT-5.x pricing rates (Phase C — until sourced, Codex sessions report as
   unpriced, which the analytics already handle).
5. Pre-envelope (≤0.31.0) file support: parse or explicitly report as
   unsupported-legacy (Phase B decision).
6. How provider selection surfaces in CLI/MCP UX (Phase D).
