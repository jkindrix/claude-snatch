# Test Corpus — coverage checklist & strategy

Status: design checklist (Phase 0 of the export-hardening plan). This document
defines the **golden test corpus**: a set of small, deterministic, checked-in
JSONL fixtures that exercise every shape the parser/exporters must handle.

## Strategy

**Synthetic-as-corpus, derived from real captures.** The corpus is hand-authored
minimal fixtures (one feature per fixture, dummy content, no PII). Real sessions
under `~/.claude/projects` are the **oracle** — mined to author fixtures
correctly and diffed against to catch format drift — never the test input itself.

Why derive-from-real and not author-from-head: the export bugs this work targets
(`--only tool-results` returning nothing, `--only user` ≡ `--only prompts`) exist
because the code's *mental model* was wrong — it assumed tool results live in
assistant entries when they actually live in **user-role** entries. Fixtures
authored from that same wrong model would pass and catch nothing. Real captures
are the ground truth that keeps the corpus honest.

### Guardrails

1. **Provenance.** Each harvested shape carries a comment noting the source
   session id + the `version` it came from (or `spec-authored` for synthesized
   gaps).
2. **Minimal & PII-free.** Reproduce the *shape* with dummy content; never copy a
   real session wholesale.
3. **Shape-diff validation.** A periodic check confirms synthetic shapes still
   match a fresh sample of real sessions, so CC format drift is noticed.

### Executable native-corpus audit

The provider semantic audit is opt-in because it reads private, machine-local
session artifacts and emits aggregate counts only. Run it with:

```bash
just audit-native-corpus
```

The recipe deliberately fails when the native corpus is unavailable or empty.
Directly invoking the ignored tests retains a skip-on-absence mode for
development, so it must not be used as evidence that a real-corpus audit ran.
The conformance test independently derives expected provenance, prompt, usage,
lineage, compaction/state, tool-lifecycle, and file-change allocations from the
native record stream. Mutation tests in the normal suite prove those oracles
reject representative corruptions.

For the explicit full-provider inventory performance check, run:

```bash
just benchmark-provider-union
```

That local GNU/Linux benchmark defaults to generous ceilings of 10 seconds and
256 MiB peak RSS. Normal CI avoids machine-specific timing assertions and
instead pins the deterministic algorithmic contract: one bulk inventory call,
zero per-session rediscovery, bounded native prelude reads, and filtering
before parse.

### Version policy

Every JSONL entry carries a per-entry `"version"` stamp (the CC version that
wrote it) — this is the source of truth, not install date. Current binary:
**2.1.193**. Strict `== 2.1.193` yields only ~13 sessions (too thin for rare
shapes), so the rule is:

> **Harvest from any version; prefer the newest version that contains the shape;
> stamp each fixture's provenance with the exact source version; validate the
> shape against 2.1.193.**

Forward-compat shapes (`Unknown` variants, enum `Other(...)` fallbacks) are
**synthesized** — current versions don't emit them by definition.

## Fixtures

Existing fixtures live in `tests/fixtures/`. New ones proposed below.

| Fixture | Status | Purpose |
|---------|--------|---------|
| `simple_session.jsonl` | exists | basic user/assistant + one tool round-trip |
| `thinking_session.jsonl` | exists | thinking block + signature |
| `system_session.jsonl` | exists | bare system banner + summary — **extend** with subtypes |
| `branching_session.jsonl` | exists | sidechain marker — **extend** with real `agentId` + subagent file |
| `compaction_session.jsonl` | NEW | compaction trio (see below) |
| `subagent_session.jsonl` + `subagents/agent-*.jsonl` | NEW | real parent↔subagent linkage, exact count |
| `rich_entries_session.jsonl` | NEW | the rare top-level `LogEntry` types |
| `content_blocks_session.jsonl` | NEW | image block + image sources + MCP/server tool-use + tool-result error states |
| `forward_compat_session.jsonl` | NEW | `Unknown` entry, `Unknown` block, each enum `Other(...)` |
| `malformed_session.jsonl` | NEW | malformed lines, duplicate UUIDs |

The `tests/generators/` builder should be **rebuilt on the real `src/model`
types** (it currently uses private structs with only 3 block kinds) so synthetic
fixtures are type-safe and round-trip-verified.

---

## Coverage checklist

Legend — **Cov:** ✅ existing fixture · ⚠️ partial · ❌ gap.
**Source:** `harvest (N)` = N sessions contain it (scan 2026-06-26, 12,353 files) ·
`synth` = spec-authored.

### Top-level `LogEntry` variants — `src/model/message.rs:28-75`

| Variant | Wire `type` | Cov | Source | Target fixture |
|---------|-------------|-----|--------|----------------|
| `User` | `user` | ✅ | harvest | simple |
| `Assistant` | `assistant` | ✅ | harvest | simple |
| `System` | `system` | ⚠️ bare only | harvest | system (extend) |
| `Summary` | `summary` | ✅ | harvest (423) | system |
| `FileHistorySnapshot` | `file-history-snapshot` | ❌ | harvest (2431) | rich_entries |
| `QueueOperation` | `queue-operation` | ❌ | harvest (697) | rich_entries |
| `TurnEnd` | `turn_end` | ❌ | synth (0 on disk) | forward_compat |
| `Progress` | `progress` | ❌ | harvest (5212) | subagent / rich_entries |
| `Attachment` | `attachment` | ❌ | harvest (1544) | rich_entries |
| `LastPrompt` | `last-prompt` | ❌ | harvest (200+) | rich_entries |
| `Mode` | `mode` | ❌ | harvest (200+) | rich_entries |
| `PermissionMode` | `permission-mode` | ❌ | harvest (200+) | rich_entries |
| `AiTitle` | `ai-title` | ❌ | harvest (289) | rich_entries |
| `Unknown` | (unknown/absent `type`) | ❌ | synth | forward_compat |

### `ContentBlock` variants — `src/model/content.rs:216-241`

| Variant | Wire `type` | Cov | Source | Target |
|---------|-------------|-----|--------|--------|
| `Text` | `text` | ✅ | harvest | simple |
| `ToolUse` | `tool_use` | ✅ | harvest | simple |
| `ToolResult` | `tool_result` | ✅ | harvest | simple |
| `Thinking` | `thinking` | ✅ | harvest | thinking |
| `Image` | `image` | ❌ | harvest (31) | content_blocks |
| `Unknown { kind, raw }` | e.g. `redacted_thinking`, `server_tool_use`, `fallback` | ❌ | synth (`redacted_thinking` = 0 on disk) | forward_compat |

### Forward-compat string enums (`Other(String)` fallback) — synthesize all `Other` arms

| Enum | File:line | Known wire values | `Other` source |
|------|-----------|-------------------|----------------|
| `StopReason` | content.rs:128 | tool_use, end_turn, max_tokens, stop_sequence | synth + harvest known (max_tokens = 34) |
| `ThinkingLevel` | metadata.rs:36 | high, medium, low | synth |
| `TodoStatus` | metadata.rs:79 | pending, in_progress, completed | synth |
| `CompactTrigger` | metadata.rs:175 | manual, auto | synth |
| `QueueOperationType` | message.rs:981 | enqueue, dequeue, remove, popAll | synth |
| `SystemSubtype` | message.rs:839 | compact_boundary, stop_hook_summary, api_error, local_command, checkpoint, rewind, rename, init, resume, permission, tool | harvest known + synth `Other` |

### Special structures

| Structure | Where | Cov | Source | Target |
|-----------|-------|-----|--------|--------|
| Compaction: `compact_boundary` + `compactMetadata{trigger,pre_tokens}` | message.rs:869, metadata.rs:159 | ❌ | harvest (592) | compaction |
| Compaction: `logicalParentUuid` (cross-boundary link) | message.rs:710 | ❌ | harvest (592) | compaction |
| Compaction: `isCompactSummary` (user + summary) | message.rs:548, 893 | ❌ | harvest (611) | compaction |
| Subagent sidechain: `isSidechain` + `agentId` | message.rs:412, 419 | ⚠️ orphan marker | harvest (9733 / 1197 dirs) | subagent |
| Subagent provenance: `tool_use_id`, `parentToolUseID`, `agent_progress` | message.rs:1089 | ❌ | harvest (progress 5212) | subagent |
| On-disk subagent files: `subagents/agent-*.jsonl` | discovery/paths.rs:11 | ❌ | harvest (1197 dirs) — synth for exact count | subagent |
| `FileHistorySnapshot.snapshot.tracked_file_backups` | message.rs:902 | ❌ | harvest (2431) | rich_entries |
| System api_error retry fields (`error`, `retry_in_ms`, …) | message.rs:761 | ❌ | harvest (63) | system (extend) |
| System `stop_hook_summary` (`hook_infos`) | message.rs:782 | ❌ | harvest (3) | system (extend) |
| System checkpoint/rewind/rename/init/resume/permission/tool subtypes | message.rs:807 | ❌ | synth (0 on disk) | forward_compat |
| `Todo` / `todos` on user message | message.rs:563, metadata.rs:56 | ❌ | harvest (200+) | rich_entries |
| MCP tool-use (`mcp__server__method`) / server tool (`srvtoolu_`) | content.rs:380 | ❌ | harvest (200+) | content_blocks |
| Tool-result 3-state error (`is_error` true/false/absent) | content.rs:438 | ⚠️ absent only | harvest | content_blocks |
| `ImageSource` Base64 / Url / File (no `Other` fallback) | content.rs:592 | ❌ | base64 harvest (31) + synth url/file | content_blocks |
| `ToolResultContent::Array` (image-bearing, redaction path) | content.rs:475 | ❌ | harvest (31) | content_blocks |
| Duplicate-UUID reconstruction (exact vs conflict) | reconstruction | ❌ | synth | malformed |
| Malformed / truncated JSONL line | parser | ❌ | synth | malformed |
| Unknown-field preservation (`extra` IndexMap round-trip) | all structs | ⚠️ unit tests only | synth | forward_compat |

---

## Gaps that MUST be synthesized (cannot be harvested)

These never appear in real current-version logs, so they are authored from the
API/format spec:

- `LogEntry::Unknown` and `ContentBlock::Unknown` (forward-compat) — incl.
  `redacted_thinking` (0 on disk).
- `LogEntry::TurnEnd` (`turn_end`) and the `SystemSubtype` values
  `checkpoint` / `rewind` / `rename` / `init` / `resume` / `permission` / `tool`
  — all modeled but **absent from all 12,353 current sessions** (scan 2026-06-26).
  Synthesize from the model definitions. (Note: that the model defines subtypes
  current CC doesn't emit is itself a fidelity observation worth a follow-up.)
- Every enum `Other(...)` arm (fake-future values).
- `ImageSource::Url` / `ImageSource::File` (if absent from harvest).
- Malformed lines, truncated lines, duplicate UUIDs.
- Exact-count subagent fixtures (N synthetic `agent-*.jsonl` files for
  deterministic `subagent_count == N` assertions).

## Next steps (after this checklist)

1. ~~Run the "harvest — verify" scans~~ — done (scan 2026-06-26; all rows
   resolved to harvest/synth above).
2. Author the new fixtures (harvested shapes first, synth gaps second), each with
   provenance.
3. Rebuild `tests/generators/` on real `src/model` types.
4. Add behavioral assertions (issue 0013): redaction removes a planted secret;
   each `--only` filter includes/excludes correctly; `--combine-agents` count
   matches on-disk subagent files.
