# Fixture provenance

JSONL has no comment syntax, so per-fixture provenance lives here. Each entry
records the real session shape a fixture was derived from (or `spec-authored`
for synthesized forward-compat shapes). See `docs/test-corpus.md` for strategy.

Fixtures reproduce *shapes* with dummy content — they contain no real session
data or PII.

| Fixture | Source | Version | Shapes exercised |
|---------|--------|---------|------------------|
| `simple_session.jsonl` | hand-authored (pre-existing) | 2.0.74 | user/assistant text, tool_use, tool_result |
| `thinking_session.jsonl` | hand-authored (pre-existing) | — | thinking block + signature |
| `system_session.jsonl` | hand-authored (pre-existing) | — | bare system banner, summary |
| `branching_session.jsonl` | hand-authored (pre-existing) | — | isSidechain marker (orphan) |
| `compaction_session.jsonl` | shape derived from real `compact_boundary` session (project `-tmp-rust-mssql-driver`, session `961711c0`, captured v2.1.170) | 2.1.193 | `compact_boundary` system entry, `compactMetadata{trigger,preTokens,postTokens,durationMs}`, `logicalParentUuid`, `isCompactSummary` user entry, cross-boundary chain |
| `subagent_session/` (directory tree) | layout derived from real subagent session (project `-tmp-rust-mssql-driver`, session `85a67f74`, captured v2.1.170) | 2.1.193 | on-disk `<uuid>/subagents/agent-*.jsonl` transcripts + `agent-*.meta.json` sidecars (`agentType`/`description`/`toolUseId`), parent `Task` tool_use spawn, `isSidechain`/`agentId`, partial sidecar (agent-3 omits `toolUseId`) |
| `rich_entries_session.jsonl` | shapes harvested across real sessions (mode/permission-mode/last-prompt/ai-title/queue-operation/attachment/progress/file-history-snapshot), `todos` shape from `model::Todo` | 2.1.193 | `file-history-snapshot`, `queue-operation`, `attachment`, `progress`, `last-prompt`, `mode`, `permission-mode`, `ai-title`, user `todos` |
| `content_blocks_session.jsonl` | image/MCP shapes harvested from real sessions; `ImageSource::Url`/`File` and `srvtoolu_` server tool spec-authored (absent from disk) | 2.1.193 | `Image` block with `ImageSource` base64/url/file, MCP tool-use (`mcp__server__method`), server tool-use (`srvtoolu_`), tool-result 3-state error (true/false/absent), array-variant tool-result carrying a base64 image (for image-payload handling, issue 0014) |
| `forward_compat_session.jsonl` | spec-authored (forward-compat shapes never emitted by current CC) | 2.1.193 | `LogEntry::Unknown` (future entry type), `ContentBlock::Unknown` (`redacted_thinking` + future block), `SystemSubtype::Other`, `CompactTrigger::Other`, `StopReason::Other` (blocked by issue 0015) |
| `malformed_session.jsonl` | spec-authored | 2.1.193 | truncated/invalid JSONL lines (lenient skip + diagnostic retention), duplicate-UUID pair (for future reconstruction tests) |
| `redaction_session.jsonl` | spec-authored (planted dummy secret) | 2.1.193 | a planted email in assistant text, for the issue 0001 redaction guard |
| `tool_render_session.jsonl` | tool-use input shapes (`Edit`/`MultiEdit`/`Bash`/`Write`/`TodoWrite`) from `model` types, dummy content | 2.1.193 | readable tool rendering (issue 0020): `Edit`→diff, `MultiEdit`→multi-diff, `Bash`→shell+description, `Write`→code, `TodoWrite`→checklist, plus an unmodeled `Read` tool for the JSON fallback |

## Notes

- The real `compactMetadata` carries more fields than the model's typed
  `CompactMetadata` (`preCompactDiscoveredTools`, `preservedSegment`,
  `preservedMessages`); those ride in the struct's `extra` map. The fixture
  includes the typed subset plus `durationMs` to exercise `extra` preservation.
