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

## Notes

- The real `compactMetadata` carries more fields than the model's typed
  `CompactMetadata` (`preCompactDiscoveredTools`, `preservedSegment`,
  `preservedMessages`); those ride in the struct's `extra` map. The fixture
  includes the typed subset plus `durationMs` to exercise `extra` preservation.
