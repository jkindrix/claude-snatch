# Analysis Module Extraction

**Decision:** Extract analytical logic from `mcp_server/` into a shared `src/analysis/` module so that CLI, MCP, and TUI all have access to the same capabilities.

**Why:** The MCP server contains ~20 analytical operations (lesson extraction, timeline building, scoped search, message formatting, etc.) that are inaccessible to the CLI. This means tools like `recap` can't access lessons, timelines, or summaries. The fix is the standard pattern: business logic in a library, presentation in the endpoints.

**Target state:**
```
src/analysis/          shared module
    mod.rs             re-exports                                                [DONE]
    extraction.rs      conversation text extraction, metadata, compaction events  [DONE]
    filters.rs         period parsing, session filtering                          [DONE]
    lessons.rs         error->fix pairs, user corrections, error-prone tool ranking  [DONE]
    search.rs          multi-scope regex search (text/tools/thinking)             [DONE]
    timeline.rs        turn-by-turn narrative building with tool-only collapse    [DONE]

src/mcp_server/        thin adapter: MCP request -> analysis -> MCP response     [fully rewired]
src/cli/commands/      thin adapter: CLI args -> analysis -> terminal/JSON output
```

---

## Inventory

### Phase 1: Critical (unblocks recap Accomplishments + Lessons)

| # | Operation | Source | Lines | Complexity | Tests | Status |
|---|-----------|--------|-------|------------|-------|--------|
| 1 | Error->fix pair extraction | analysis/lessons.rs | ~100 | Large | 4 | [x] |
| 2 | Soft error detection (regex) | analysis/lessons.rs | ~10 | Small | 1 | [x] |
| 3 | User correction detection | analysis/lessons.rs | ~60 | Medium | 3 | [x] |
| 4 | Error-prone tool ranking | analysis/lessons.rs | ~13 | Small | 1 | [x] |
| 5 | extract_user_prompt_text | analysis/extraction.rs | 35 | Small | 0 | [x] |
| 6 | extract_assistant_summary | analysis/extraction.rs | 14 | Small | 0 | [x] |
| 7 | Timeline turn building | analysis/timeline.rs | 48 | Medium | 4 | [x] |
| 8 | Tool-only turn collapsing | analysis/timeline.rs | 58 | Medium | (included) | [x] |

**Deliverable:** `snatch lessons <id> --json` and `snatch timeline <id> --json` CLI commands

### Phase 2: High Priority (widely used helpers, deduplication)

| # | Operation | Source | Lines | Complexity | Tests | Status |
|---|-----------|--------|-------|------------|-------|--------|
| 9 | extract_tool_names | analysis/extraction.rs | 13 | Small | 0 | [x] |
| 10 | extract_thinking_text | analysis/extraction.rs | 22 | Small | 0 | [x] |
| 11 | extract_tool_input_summary | analysis/extraction.rs | 63 | Medium | 4 | [x] |
| 12 | extract_files_from_tools | analysis/extraction.rs | 31 | Small | 0 | [x] |
| 13 | has_tool_errors | analysis/extraction.rs | 13 | Trivial | 0 | [x] |
| 14 | extract_error_preview | analysis/extraction.rs | 12 | Small | 0 | [x] |
| 15 | Session aggregation (3x duplication in get_stats) | mod.rs | ~60 | Medium | 0 | [ ] |

**Deliverable:** Deduplicated get_stats, all helpers accessible from CLI

### Phase 3: Medium Priority (specialized operations)

| # | Operation | Source | Lines | Complexity | Tests | Status |
|---|-----------|--------|-------|------------|-------|--------|
| 16 | search_entry_text (multi-scope) | analysis/search.rs | 66 | Medium | 4 | [x] |
| 17 | parse_period + period_cutoff | analysis/filters.rs | 25 | Small | 8 | [x] |
| 18 | find_compaction_events | analysis/extraction.rs | 14 | Trivial | 0 | [x] |
| 19 | get_model | analysis/extraction.rs | 13 | Trivial | 0 | [x] |
| 20 | has_thinking | analysis/extraction.rs | 6 | Trivial | 0 | [x] |

**Deliverable:** `snatch search <pattern> --json` CLI command, complete analysis module

---

## Post-Extraction: CLI Commands to Add

| Command | Analysis dependency | Fills recap gap | Status |
|---------|-------------------|-----------------|--------|
| `snatch lessons <id> --json` | Phase 1 (#1-4) | Lessons Learned | [x] |
| `snatch timeline <id> --json` | Phase 1 (#7-8) | Accomplishments | [x] |
| `snatch messages <id> --json` | Phase 2 (#5-6, 9-10) | Summaries | [x] |
| `snatch search <pattern> --json` | Phase 3 (#16) | General utility | [x] |

## Post-Extraction: recap Fixes (bash, no Rust)

| Fix | Depends on | Status |
|-----|-----------|--------|
| Filter empty sessions (0 messages) | Nothing | [x] |
| Add cost/token totals to report | Nothing (data exists) | [x] |
| Show primary model per session | Nothing (data exists) | [x] |
| Disambiguate files (project prefix in Most-Touched) | Nothing | [x] |
| Wire `snatch lessons` into Lessons section | Phase 1 CLI | [x] |
| Wire `snatch timeline` into Accomplishments | Phase 1 CLI | [x] |

---

## Execution Protocol

1. One operation at a time
2. Extract function -> add tests -> update MCP to call shared code -> verify MCP still works
3. After each phase: add CLI command, verify with recap
4. Never leave MCP broken between steps

## Key Dependencies

- All extraction functions depend on `LogEntry` from `src/model/`
- Lesson extraction + timeline depend on `Conversation` from `src/reconstruction/`
- Session aggregation depends on `SessionAnalytics` from `src/analytics/`
- `truncate_text` is in shared `analysis/extraction.rs`
- `resolve_session` stays in MCP (infrastructure wiring, not analysis)
