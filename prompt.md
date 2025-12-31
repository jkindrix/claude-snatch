# claude-snatch Code Review Prompt v2.1

Perform a comprehensive technical review of this Rust CLI/TUI tool for Claude Code conversation log extraction. This prompt is tailored to the actual codebase architecture and emphasizes modern Rust 2024/2025 best practices.

**Codebase Summary**: A high-fidelity JSONL log parser and exporter with TUI/CLI interfaces, supporting 7 export formats, full-text search indexing, and conversation tree reconstruction.

## Pre-Review Commands

Execute these commands to gather baseline metrics before analysis:

```bash
# Test suite baseline
cargo test --all-features 2>&1 | tail -5

# Clippy analysis (pedantic + nursery enabled in Cargo.toml)
cargo clippy --all-targets --all-features 2>&1 | grep -E "^(warning|error)" | head -20

# Dependency audit
cargo deny check 2>&1 | tail -10

# Build verification
cargo build --release 2>&1 | tail -3

# Benchmark suite (optional - takes ~2 min)
cargo bench --bench parser_bench -- --quick
```

---

## 1. Rust Ecosystem Alignment (Weight: 15%)

### Dependency Audit
Validate against 2025 crates.io best practices:

| Crate | Expected Version | Check |
|-------|------------------|-------|
| clap | 4.5+ | Derive macros, subcommand architecture |
| ratatui | 0.29+ | Latest widget patterns, no deprecated APIs |
| thiserror | 2.0+ | Error derive patterns |
| rusqlite | 0.33+ | Bundled SQLite, FTS5 feature |
| serde | 1.0.210+ | Derive macros, flatten usage |
| chrono | 0.4.38+ | Timezone handling, no deprecated |
| tokio | 1.42+ | If async used, verify runtime config |
| parking_lot | 0.12+ | RwLock usage patterns |
| criterion | 0.5+ | Benchmark harness |

### Code Quality Gates
- [ ] `src/lib.rs:76-78` - Verify `#![deny(unsafe_code)]` present (use `deny` not `forbid` to allow mmap feature's targeted unsafe)
- [ ] `Cargo.toml` - Check `[lints.clippy]` section for pedantic/nursery
- [ ] Zero `#[allow(...)]` without justification comments (see `src/tui/state.rs` for proper pattern)
- [ ] `src/error.rs` - thiserror usage with `#[from]` and `#[error]` context
- [ ] BSD-style exit codes (0=success, 1=general error, 64-78=specific)
- [ ] `deny.toml` - cargo-deny configured for license compliance and advisory auditing

### Modern Rust Patterns
- [ ] Edition 2021 or 2024
- [ ] MSRV declared in `Cargo.toml` or `rust-toolchain.toml`
- [ ] No `unwrap()` in library code (only tests/examples)
- [ ] `?` operator over `.expect()` in fallible functions
- [ ] `impl Trait` return types where appropriate

---

## 2. JSONL Parser Correctness (Weight: 15%)

### Message Type Coverage
Verify `src/model/message.rs` LogEntry enum handles all types:

```rust
// Expected variants
User, Assistant, System, Summary,
FileHistorySnapshot, QueueOperation, TurnEnd
```

### Schema Evolution
- [ ] `src/model/*.rs` - `#[serde(flatten)] extra: HashMap<String, Value>` for forward compat
- [ ] `#[serde(default)]` on optional fields
- [ ] `#[serde(rename_all = "camelCase")]` consistency
- [ ] Version field parsed but not strictly validated (schema versions 2.0.x)

### Parser Modes
Check `src/parser/mod.rs` for:
- [ ] Strict mode: fail on first error with line context
- [ ] Lenient mode: skip malformed lines, collect errors
- [ ] Streaming: `BufReader` line-by-line without full file load
- [ ] Progress callback support for large files

### Content Block Types
Verify `src/model/content.rs` ContentBlock enum:
```rust
// Core variants
Text, Thinking, ToolUse, ToolResult, Image

// Note: ServerToolUse, McpToolUse are handled via ToolUse methods:
// - is_server_tool() - detects srvtoolu_* ID prefix
// - is_mcp_tool() - detects mcp__ naming pattern
// - mcp_server(), mcp_method() - parse MCP tool names
```

### Tool Result Types
Verify `src/model/tools.rs` ToolUseResult enum for parsed tool outputs:
```rust
Glob, Grep, Read, Edit, Write, Bash, WebFetch, WebSearch,
Task, TaskOutput, Ls, MultiEdit, NotebookEdit, NotebookRead,
TodoRead, TodoWrite, AskUserQuestion, Lsp, EnterPlanMode,
ExitPlanMode, Skill, ListMcpResources, ReadMcpResource, KillShell
```

### Test Fixtures
Examine `tests/fixtures/*.jsonl` for coverage:
- [ ] `simple_session.jsonl` - Basic user/assistant flow
- [ ] `thinking_session.jsonl` - Extended thinking blocks
- [ ] `branching_session.jsonl` - Sidechain/branch detection
- [ ] `system_session.jsonl` - System messages, summaries

---

## 3. Conversation Tree Reconstruction (Weight: 12%)

### UUID Chain Integrity
Check `src/reconstruction/tree.rs`:
- [ ] `uuid` → `parentUuid` linking builds DAG
- [ ] `logicalParentUuid` handled for compaction recovery
- [ ] Orphan detection (entries with invalid parent refs)
- [ ] Root node identification (null parentUuid)

### Branch Analysis
- [ ] `isSidechain` field drives branch classification
- [ ] Main thread = longest path from root
- [ ] Branch points identified where children > 1
- [ ] Chronological ordering within branches

### Tool Correlation
- [ ] `tool_use.id` → `tool_result.tool_use_id` mapping
- [ ] Multi-tool responses grouped correctly
- [ ] Failed tool results (is_error: true) preserved
- [ ] Nested tool calls in agent workflows

### Analytics Derivation
- [ ] Token usage aggregation (input, output, cache_read, cache_creation)
- [ ] Turn count calculation
- [ ] Session duration from first/last timestamps
- [ ] Model distribution tracking

---

## 4. Export Format Fidelity (Weight: 15%)

### JSON Export (`src/export/json.rs`)
- [ ] Lossless round-trip: parse → export → parse = identical
- [ ] Pretty-print option with configurable indentation
- [ ] Analytics metadata appended (token counts, duration)

### SQLite Export (`src/export/sqlite.rs`)
Required schema elements:
```sql
-- Core tables
CREATE TABLE sessions (...);
CREATE TABLE messages (...);
CREATE TABLE tool_calls (...);
CREATE TABLE thinking_blocks (...);

-- FTS5 full-text search
CREATE VIRTUAL TABLE messages_fts USING fts5(content, role);

-- Indexes
CREATE INDEX idx_messages_session ON messages(session_id);
CREATE INDEX idx_messages_parent ON messages(parent_uuid);

-- Foreign keys enforced
PRAGMA foreign_keys = ON;
```
- [ ] All queries use `?` parameterized statements
- [ ] Transaction wrapping for atomicity
- [ ] WAL mode for concurrent reads

### Markdown Export (`src/export/markdown.rs`)
- [ ] `<details><summary>` for collapsible thinking blocks
- [ ] Fenced code blocks with language hints
- [ ] Tool calls rendered with input/output sections
- [ ] Table of contents generation
- [ ] Session analytics summary

### HTML Export (`src/export/html.rs`)
- [ ] Inline CSS (no external dependencies)
- [ ] Syntax highlighting for code blocks
- [ ] Base64 inline images
- [ ] Responsive layout
- [ ] Print-friendly styles

### Other Formats
- [ ] CSV: Proper quoting, UTF-8 BOM option
- [ ] XML: Valid structure, CDATA for content
- [ ] Text: Configurable line width, word wrap

---

## 5. TUI Implementation (Weight: 12%)

### Architecture (`src/tui/`)
- [ ] `app.rs` - Central App state struct
- [ ] `events.rs` - Event loop with crossterm
- [ ] `ui.rs` - Render functions
- [ ] `widgets/` - Custom widget implementations
- [ ] `theme.rs` - Color schemes (dark, light, high-contrast)

### Event Handling
```rust
// Expected event patterns
KeyCode::Char('q') | KeyCode::Esc => quit,
KeyCode::Up | KeyCode::Char('k') => scroll_up,
KeyCode::Down | KeyCode::Char('j') => scroll_down,
KeyCode::Enter => select/expand,
KeyCode::Char('/') => search_mode,
```
- [ ] Vim-style navigation (hjkl, gg, G)
- [ ] Mouse scroll support
- [ ] Terminal resize handling
- [ ] Focus management between panes

### Modal System
- [ ] Help overlay (F1 or ?)
- [ ] Export dialog with format selection
- [ ] Command palette (Ctrl+P)
- [ ] Go-to-line (Ctrl+G)
- [ ] Search overlay (/)

### State Management
- [ ] Immutable state updates (or documented mutation)
- [ ] Undo/redo for navigation
- [ ] Scroll position preservation
- [ ] Filter state persistence

---

## 6. Performance Characteristics (Weight: 10%)

### Parser Benchmarks
Reference `benches/parser_bench.rs` targets:

| Metric | Target | Measurement |
|--------|--------|-------------|
| 10KB file parse | <5ms | `cargo bench` |
| 1MB file parse | <50ms | |
| 10MB file parse | <500ms | |
| Tree reconstruction (1000 msgs) | <10ms | |

### Memory Efficiency
- [ ] Streaming parser: O(1) memory for file size
- [ ] Tree construction: O(n) where n = message count
- [ ] Export: Buffered writes, not full output in memory
- [ ] Target: <2x input file size peak memory

### Caching (`src/cache/mod.rs`)
- [ ] LRU eviction policy
- [ ] mtime-based invalidation
- [ ] Thread-safe via `parking_lot::RwLock`
- [ ] Configurable max entries
- [ ] Persistent disk cache option

### Parallelization
- [ ] Session discovery: parallel directory walk
- [ ] Batch export: rayon parallel iterator
- [ ] Search: parallel file scanning
- [ ] Verify `rayon` or `tokio` usage in hot paths

---

## 7. Test Coverage (Weight: 8%)

### Test Execution
```bash
cargo test --all-features
cargo test --doc
cargo test --all-features -- --ignored  # Long-running tests
```

### Coverage Categories
- [ ] **Unit tests**: Each module has `#[cfg(test)] mod tests`
- [ ] **Integration tests**: `tests/*.rs` for end-to-end flows
- [ ] **Property tests**: `proptest` for parser fuzzing (see `tests/integration_tests.rs::property_tests`)
  - `parser_never_panics_on_arbitrary_input` - fuzz with random bytes
  - `parser_handles_json_like_strings` - structured JSON fuzzing
  - `parse_stats_are_consistent` - invariant checking
- [ ] **Snapshot tests**: `insta` for export format stability (see `tests/integration_tests.rs::snapshot_tests`)
  - Use `Settings::add_filter()` to redact timestamps
  - Accept new snapshots with `cargo insta accept`
- [ ] **Benchmark tests**: `criterion` in `benches/`

### Fixture Quality
- [ ] Real-world anonymized samples
- [ ] Edge cases: empty sessions, single message, 10k+ messages
- [ ] Unicode handling: emoji, RTL, CJK
- [ ] Malformed input: truncated JSON, invalid UTF-8

### Coverage Metrics
```bash
# CI uses cargo-llvm-cov (see .github/workflows/ci.yml)
cargo llvm-cov --all-features --workspace --lcov --output-path lcov.info

# Alternative: cargo-tarpaulin
cargo tarpaulin --out html --all-features
# Target: >80% line coverage on src/
```

---

## 8. Security Posture (Weight: 8%)

### SQL Injection Prevention
Verify `src/export/sqlite.rs`:
```rust
// REQUIRED: Parameterized queries
conn.execute("INSERT INTO messages (uuid, content) VALUES (?1, ?2)", params![uuid, content])?;

// FORBIDDEN: String interpolation
conn.execute(&format!("INSERT INTO messages VALUES ('{}')", user_input), [])?;
```

### PII Handling (`src/util/mod.rs`)
- [ ] `RedactionConfig` with pattern matching
- [ ] API key detection (sk-*, anthropic-*, etc.)
- [ ] Email/phone/SSN/credit card regex
- [ ] IP address redaction option
- [ ] Configurable replacement text

### Input Validation
- [ ] Path traversal: No `../` in user-provided paths
- [ ] File size limits: Warn on files >100MB
- [ ] Symlink handling: Follow or reject configurable
- [ ] Filename sanitization for exports

### Local-Only Operation
- [ ] No network dependencies in core
- [ ] No telemetry or analytics
- [ ] No external API calls
- [ ] Audit `Cargo.toml` for network-capable deps

---

## 9. Documentation Quality (Weight: 5%)

### README.md
- [ ] Installation (cargo install, from source, binaries)
- [ ] Quick start with common commands
- [ ] All CLI commands documented
- [ ] Configuration file format
- [ ] Screenshots/GIFs of TUI

### CHANGELOG.md
- [ ] [Keep a Changelog](https://keepachangelog.com/en/1.1.0/) format
- [ ] Semantic versioning
- [ ] Unreleased section for pending changes
- [ ] Links to releases/tags

### API Documentation
```bash
cargo doc --no-deps --open
```
- [ ] All public items documented
- [ ] Module-level `//!` docs
- [ ] Examples in doc comments (`///`)
- [ ] `#[doc(hidden)]` only for internal APIs

### Missing Documentation
Check for presence:
- [ ] `CONTRIBUTING.md` - Development setup, PR process
- [x] `SECURITY.md` - Vulnerability reporting (created - includes threat model, response timeline)
- [ ] `docs/architecture.md` - System design, data flow
- [ ] `docs/adr/` - Architecture Decision Records

---

## 10. CLI Design (Weight: Bonus)

### Command Structure
Verify `src/cli/` implements:
```
snatch list      # Browse sessions
snatch export    # Multi-format export
snatch search    # Full-text search
snatch info      # Session metadata
snatch stats     # Usage analytics
snatch diff      # Session comparison
snatch validate  # JSONL integrity
snatch watch     # Real-time monitor
snatch extract   # Beyond-JSONL data
snatch cleanup   # Session pruning
snatch cache     # Cache management
snatch index     # Search indexing
snatch config    # Configuration
snatch tag       # Session tagging
snatch prompts   # Prompt extraction
```

### UX Patterns
- [ ] `--help` on every subcommand
- [ ] `--version` flag
- [ ] `--quiet` and `--verbose` flags
- [ ] `--output` for file destination
- [ ] `--format` for export type
- [ ] Shell completions (bash, zsh, fish, PowerShell)
- [ ] Color auto-detection (NO_COLOR, TERM)

---

## Output Format

### Ratings Table
| Dimension | Score (1-10) | Grade | Key Finding |
|-----------|--------------|-------|-------------|
| 1. Rust Ecosystem | | | |
| 2. Parser Correctness | | | |
| 3. Tree Reconstruction | | | |
| 4. Export Fidelity | | | |
| 5. TUI Implementation | | | |
| 6. Performance | | | |
| 7. Test Coverage | | | |
| 8. Security | | | |
| 9. Documentation | | | |
| **Overall** | | | |

### Required Deliverables
1. **Ratings table** with 1-10 scores and letter grades (A/B/C/D/F)
2. **Code citations** in `file:line` format for notable patterns
3. **Verified claims** with hyperlinks to crates.io, docs.rs, or RFCs
4. **Prioritized recommendations** grouped by effort (Quick Win / Medium / Major)
5. **Executive summary** (3-5 sentences) with overall grade

### Grade Scale
| Score | Grade | Meaning |
|-------|-------|---------|
| 9-10 | A | Exceptional, production-ready |
| 8-8.9 | B+ | Strong, minor improvements needed |
| 7-7.9 | B | Good, some gaps to address |
| 6-6.9 | C | Adequate, significant work needed |
| <6 | D/F | Below standard, major issues |

---

## Reviewer Notes

- Run all pre-review commands before starting analysis
- Use `cargo expand` to verify macro-generated code if needed
- Check `target/doc` for API documentation completeness
- Reference `benches/` output for performance validation
- Cross-reference `CHANGELOG.md` claims against actual code

---

## Known Issues & Lessons Learned (v2.1)

### Fixed in Latest Review
1. **UTF-8 boundary bug** (`src/parser/mod.rs:257`) - `truncate_preview()` panicked on multi-byte chars; fixed with `is_char_boundary()` check
2. **Doctest error** (`src/export/mod.rs:27`) - Missing `?` on `Conversation::from_entries()` which returns `Result`
3. **Snapshot timestamp instability** - Use `insta::Settings::add_filter()` to redact `exported_at` fields

### Clippy Warnings to Address
The codebase has ~900 clippy warnings with `-D warnings`. Common categories:
- `unnested_or_patterns` - Pattern matching style in TUI event handlers
- `similar_names` - Variable naming (e.g., `entry` vs `entries`)
- `redundant_else` - Empty else blocks after early return
- `unreadable_literal` - Numbers like `10000` should be `10_000`

These are cosmetic and don't affect functionality. Consider batch-fixing with `cargo clippy --fix`.

### Best Practices Applied
1. Use `#![deny(unsafe_code)]` instead of `#![forbid]` to allow feature-gated unsafe (mmap)
2. Add `filters` feature to insta for timestamp redaction in snapshots
3. Property tests with proptest effectively find edge cases (UTF-8, empty input, etc.)
4. `deny.toml` should only list licenses actually used by dependencies
