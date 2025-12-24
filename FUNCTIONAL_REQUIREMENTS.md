# claude-snatch: Functional Requirements Specification

> **Version:** 1.2.1
> **Status:** Draft (Synchronized with JSONL reference documentation)
> **Target:** Maximum Achievable Extraction Fidelity
> **Language:** Rust
> **Last Updated:** 2025-12-23
> **Claude Code Baseline:** v2.0.74+ (December 2025)

---

## Table of Contents

1. [Executive Summary](#1-executive-summary)
   - [1.1 Key Differentiators](#11-key-differentiators)
   - [1.2 MVP Scope Definition](#12-mvp-scope-definition)
2. [Project Vision & Objectives](#2-project-vision--objectives)
3. [Competitive Analysis Summary](#3-competitive-analysis-summary)
4. [Data Element Extraction Requirements](#4-data-element-extraction-requirements)
5. [Core Feature Requirements](#5-core-feature-requirements)
6. [Output Format Requirements](#6-output-format-requirements)
7. [CLI Interface Requirements](#7-cli-interface-requirements)
8. [TUI Interface Requirements](#8-tui-interface-requirements)
9. [Performance Requirements](#9-performance-requirements)
10. [Architecture Requirements](#10-architecture-requirements)
11. [Quality & Reliability Requirements](#11-quality--reliability-requirements)
12. [Security & Privacy Requirements](#12-security--privacy-requirements)
13. [Documentation Requirements](#13-documentation-requirements)
14. [Testing Requirements](#14-testing-requirements)
15. [Distribution Requirements](#15-distribution-requirements)
16. [Future Considerations](#16-future-considerations)

---

## 1. Executive Summary

**claude-snatch** is a high-performance, Rust-based CLI/TUI tool for extracting, analyzing, and exporting Claude Code conversation logs with **maximum achievable data fidelity**. It aims to be the most performant, reliable, and complete extraction tool in this space, addressing gaps in all 49 existing competitors by capturing every extractable data element from Claude Code's JSONL logs and supplementary data sources.

### 1.1 Key Differentiators

- [ ] **Maximum JSONL Fidelity**: Extract all 77+ documented data elements (vs. current best: 62%)
- [ ] **Beyond-JSONL Extraction**: Capture all 21+ supplementary data elements
- [ ] **Rust Performance**: Native speed, 10-100x faster than Python/Node alternatives
- [ ] **Production-Grade Reliability**: Comprehensive error handling, graceful degradation, schema versioning
- [ ] **Lossless Round-Trip**: JSONL → Export → Reconstruct (within same Claude Code version)
- [ ] **Dual Interface**: Both CLI (scriptable) and TUI (interactive) modes
- [ ] **Forward-Compatible**: Unknown field preservation for future Claude Code versions
- [ ] **Cross-Platform**: Linux, macOS, Windows (including WSL) with proper path handling

### 1.2 MVP Scope Definition

The Minimum Viable Product (MVP) encompasses all P0 (Critical) requirements. This section provides explicit enumeration and completion criteria.

#### 1.2.1 MVP Summary by Category

| Category | P0 Count | Description |
|----------|----------|-------------|
| Data Elements (Sections 4.1-4.8) | 46 | Core content, identity, usage, environment, agent, error, system, specialized |
| Beyond-JSONL (Section 4.9) | 1 | Subagent JSONL files only |
| Parsing & Extraction (Section 5.1) | 8 | Core JSONL parsing capabilities |
| Session Discovery (Section 5.2) | 6 | Project/session enumeration |
| Conversation Reconstruction (Section 5.3) | 8 | Tree building and linking |
| Analytics (Section 5.4) | 4 | Basic statistics |
| Search & Filtering (Section 5.5) | 3 | Essential filtering |
| File Backup (Section 5.6) | 1 | Snapshot event parsing |
| Subagent Correlation (Section 5.7) | 4 | Agent detection and linking |
| Markdown Export (Section 6.1) | 6 | Core markdown output |
| JSON Export (Section 6.2) | 4 | Core JSON output |
| CLI Commands (Section 7.1) | 5 | Essential commands |
| CLI Options (Sections 7.2-7.3) | 12 | Core flags and options |
| CLI Output (Section 7.5) | 3 | Terminal formatting |
| TUI Core (Section 8.1) | 7 | Basic TUI layout |
| TUI Navigation (Section 8.2) | 8 | Keyboard navigation |
| TUI Search (Section 8.3) | 2 | Basic search |
| TUI Display (Section 8.4) | 1 | Syntax highlighting |
| TUI Actions (Section 8.5) | 2 | Essential actions |
| TUI Visual (Section 8.6) | 2 | Dark theme, unicode |
| Performance (Section 9) | 0 | Targets only, not requirements |
| Concurrency (Section 9.3) | 1 | Non-blocking TUI |
| Architecture (Section 10.1-10.2) | 15 | Core modules and data model |
| Schema Versioning (Section 10.3) | 4 | Version detection and handling |
| Error Handling (Section 10.6) | 4 | Core error types |
| Code Quality (Section 11.1) | 4 | Clippy, rustfmt, no unsafe |
| Reliability (Section 11.2) | 3 | Graceful handling |
| Compatibility (Section 11.3) | 4 | Primary platforms |
| Security (Section 12.1-12.2) | 7 | Core security |
| Documentation (Section 13) | 5 | Essential docs |
| Testing (Section 14) | 9 | Core test coverage |
| Distribution (Section 15) | 7 | Essential releases |
| **TOTAL** | **~190** | **MVP Scope** |

#### 1.2.2 MVP Completion Criteria

The MVP is considered **complete** when ALL of the following are satisfied:

1. **Parsing Fidelity**: Successfully extract ≥95% of P0 data elements from valid JSONL files
2. **CLI Functional**: All 5 core commands (`list`, `export`, `search`, `stats`, `info`) operational
3. **TUI Functional**: Basic three-panel layout with keyboard navigation working
4. **Export Capability**: Markdown and JSON export with ≥90% element coverage
5. **Cross-Platform**: Builds and runs on Linux x86_64, macOS x86_64, macOS aarch64
6. **Test Coverage**: ≥60% code coverage, all P0 integration tests passing
7. **Documentation**: README, installation guide, and CLI reference complete
8. **Performance**: Meets PERF-001 (<50ms for 1MB JSONL) and PERF-002 (<500ms for 10MB)
9. **Quality Gates**: Zero Clippy warnings, rustfmt compliant, no panics on user input

#### 1.2.3 MVP Target Metrics

| Metric | MVP Target | v1.0 Target | Current Best Competitor |
|--------|------------|-------------|------------------------|
| JSONL Element Coverage | ≥60/77 (78%) | 77/77 (100%) | ~48/77 (62%) |
| Parse Speed (1MB) | <100ms | <50ms | ~500ms |
| CLI Commands | 5/10 | 10/10 | N/A |
| Export Formats | 2 (MD, JSON) | 7 | 1-3 (typical) |
| Platform Support | 3 | 6 | 1-2 (typical) |

#### 1.2.4 MVP Exclusions (Deferred to v1.0+)

The following are explicitly **NOT** in MVP scope:

- **P1 Requirements**: All P1 items deferred to v1.0 release
- **TUI Advanced Features**: Mouse support, resizable panels, themes
- **Beyond-JSONL Sources**: File backup contents, settings, rules (except subagent JSONL)
- **Additional Export Formats**: HTML, Plain Text, XML, SQLite, CSV
- **Advanced Analytics**: Cost estimates, cache efficiency, trends
- **Shell Completions**: Bash, Zsh, Fish, PowerShell
- **Package Manager Support**: Homebrew, AUR, Nix, Scoop

---

## 2. Project Vision & Objectives

### 2.1 Vision Statement

To create the most comprehensive, performant, and reliable tool for extracting and preserving Claude Code conversation data, enabling users to maintain complete archives, perform advanced analytics, and integrate with external systems.

### 2.2 Primary Objectives

- [ ] **OBJ-001**: Achieve maximum extraction fidelity for all documented JSONL elements (64+ elements)
- [ ] **OBJ-002**: Support extraction from all Claude Code data sources (not just JSONL)
- [ ] **OBJ-003**: Provide sub-second performance for typical session files (<10MB)
- [ ] **OBJ-004**: Offer both interactive (TUI) and scriptable (CLI) interfaces
- [ ] **OBJ-005**: Support all major output formats with format-specific optimizations
- [ ] **OBJ-006**: Enable lossless round-trip within same Claude Code version; best-effort across versions
- [ ] **OBJ-007**: Provide comprehensive analytics and reporting capabilities
- [ ] **OBJ-008**: Maintain cross-platform compatibility (Linux, macOS, Windows, WSL)
- [ ] **OBJ-009**: Implement schema versioning for forward/backward compatibility
- [ ] **OBJ-010**: Preserve unknown fields for future Claude Code version support

### 2.3 Success Metrics

| Metric | Target | Current Best Competitor |
|--------|--------|------------------------|
| JSONL Element Coverage | 77/77 (100%) | ~50/77 (~65%) - daaain/claude-code-log v0.9.0 |
| Beyond-JSONL Coverage | 21/21 (100%) | ~4/21 (~19%) - various tools |
| Parse Speed (10MB file) | <500ms | ~2-5s (Python tools) |
| Memory Efficiency | <2x file size | 5-10x (typical) |
| Startup Time | <50ms | 200-500ms (Python/Node) |

> **Note:** Element counts updated December 2025 to include LSP tools (13 elements) and output styles.

---

## 3. Competitive Analysis Summary

> **Assessment Date:** December 23, 2025
> **Tools Analyzed:** 49+ existing tools
> **Next Review:** Quarterly (March 2026)

### 3.1 Current Landscape Gaps

Based on analysis of 49+ existing tools, the following elements are **not extracted by ANY tool**:

#### Agent & Hierarchy (2 elements)
- [x] `isTeammate` — Teammate mode flag ✓ (`is_teammate` in CommonFields)
- [x] `slug` — Human-readable session identifier ✓ (`slug` in CommonFields)

#### Error & Recovery (5 elements)
- [x] `error.status` — HTTP status code from api_error ✓ (`ApiErrorDetails.status`)
- [x] `retryAttempt` — Current retry number ✓ (`retry_attempt` in SystemMessage)
- [ ] `maxRetries` — Maximum retry attempts
- [x] `retryInMs` — Milliseconds until retry ✓ (`retry_in_ms` in SystemMessage)
- [ ] `cause` — Error cause chain

#### System & Metadata (5 elements)
- [ ] `message.container` — Code execution container info
- [ ] `message.context_management` — Context editing info (beta)
- [x] `thinkingMetadata.level` — Thinking budget level ✓ (`ThinkingMetadata.level`)
- [x] `thinkingMetadata.disabled` — Whether thinking is disabled ✓ (`ThinkingMetadata.disabled`)
- [x] `thinkingMetadata.triggers` — Trigger conditions array ✓ (`ThinkingMetadata.triggers`)

#### Specialized Messages (14 elements)
- [x] `snapshot.trackedFileBackups` — Full file backup metadata ✓
- [x] `trackedFileBackups[].backupFileName` — Backup file reference ✓
- [x] `trackedFileBackups[].version` — File version number ✓
- [x] `trackedFileBackups[].backupTime` — Backup creation timestamp ✓
- [x] `queue-operation` (enqueue) — Input buffering enqueue ✓ (`QueueOperationType::Enqueue`)
- [x] `queue-operation` (dequeue) — Input buffering dequeue ✓ (`QueueOperationType::Dequeue`)
- [x] `queue-operation` (remove) — Input buffering remove ✓ (`QueueOperationType::Remove`)
- [x] `queue-operation` (popAll) — Input buffering popAll ✓ (`QueueOperationType::PopAll`)
- [x] `local_command` content — CLI slash command data ✓ (`LocalCommandContent`)
- [x] `toolUseResult.structuredPatch` — Edit tool unified diff hunks ✓ (`PatchHunk`)
- [x] `toolUseResult.structuredPatch[].oldStart` — Hunk old start line ✓
- [x] `toolUseResult.structuredPatch[].newStart` — Hunk new start line ✓
- [x] `toolUseResult.structuredPatch[].lines` — Diff lines with prefixes ✓
- [x] Complete `toolUseResult` for all 24+ tools ✓

### 3.2 Competitor Fidelity Scores

| Tool | Version | Stars | Score | Grade | Primary Gap |
|------|---------|-------|-------|-------|-------------|
| [daaain/claude-code-log](https://github.com/daaain/claude-code-log) | v0.9.0 | 575 | ~65% | D+ | Missing LSP, queue ops, error recovery |
| [d-kimuson/claude-code-viewer](https://github.com/d-kimuson/claude-code-viewer) | - | - | ~55% | D | Missing thinking metadata, backups |
| [adewale/claude-history-explorer](https://github.com/adewale/claude-history-explorer) | - | - | ~45% | F | Missing usage stats, system events |
| [haasonsaas/claude-usage-tracker](https://github.com/haasonsaas/claude-usage-tracker) | - | - | ~35% | F | Usage-focused, missing content |
| [ZeroSumQuant/claude-conversation-extractor](https://github.com/ZeroSumQuant/claude-conversation-extractor) | - | - | ~20% | F | Basic content only, zero deps |
| [withLinda/claude-JSONL-browser](https://github.com/withLinda/claude-JSONL-browser) | - | - | ~25% | F | Web-based, limited extraction |
| Native `/export` | - | - | ~8% | F | Minimal extraction |

**Note:** Scores are estimates based on documented features. Actual element coverage may vary.

**claude-snatch Target: 100% (Grade A)** — Extract all 77+ JSONL elements plus 21+ beyond-JSONL sources

---

## 4. Data Element Extraction Requirements

### 4.1 Core Content Elements (10 elements)

| ID | Element | JSONL Path | Required | Priority |
|----|---------|------------|----------|----------|
| [x] CE-001 | User text | `message.content` (string) | Yes | P0 | ✓ |
| [x] CE-002 | Assistant text | `message.content[].text` | Yes | P0 | ✓ |
| [x] CE-003 | Thinking blocks | `message.content[].thinking` | Yes | P0 | ✓ |
| [x] CE-004 | Thinking signatures | `message.content[].signature` | Yes | P0 | ✓ |
| [x] CE-005 | Tool call names | `message.content[].name` | Yes | P0 | ✓ |
| [x] CE-006 | Tool call inputs | `message.content[].input` | Yes | P0 | ✓ |
| [x] CE-007 | Tool call IDs | `message.content[].id` | Yes | P0 | ✓ |
| [x] CE-008 | Tool results | `message.content[].content` (tool_result) | Yes | P0 | ✓ |
| [x] CE-009 | Tool errors (3-state) | `message.content[].is_error` | Yes | P0 | ✓ |
| [x] CE-010 | Images (base64/url/file) | `message.content[].source` | Yes | P0 | ✓ |

### 4.2 Identity & Linking Elements (7 elements)

| ID | Element | JSONL Path | Required | Priority |
|----|---------|------------|----------|----------|
| [x] IL-001 | Timestamps | `timestamp` | Yes | P0 | ✓ |
| [x] IL-002 | Message UUIDs | `uuid` | Yes | P0 | ✓ |
| [x] IL-003 | Parent UUIDs | `parentUuid` | Yes | P0 | ✓ |
| [x] IL-004 | Logical parent UUIDs | `logicalParentUuid` | Yes | P0 | ✓ |
| [x] IL-005 | Session IDs | `sessionId` | Yes | P0 | ✓ |
| [x] IL-006 | Request IDs | `requestId` | Yes | P1 | ✓ |
| [x] IL-007 | Message IDs (grouping) | `message.id` | Yes | P0 | ✓ |

### 4.3 Usage & Token Statistics (11 elements)

| ID | Element | JSONL Path | Required | Priority |
|----|---------|------------|----------|----------|
| [x] UT-001 | Model info | `message.model` | Yes | P0 | ✓ |
| [x] UT-002 | Input tokens | `message.usage.input_tokens` | Yes | P0 | ✓ |
| [x] UT-003 | Output tokens | `message.usage.output_tokens` | Yes | P0 | ✓ |
| [x] UT-004 | Cache creation tokens | `message.usage.cache_creation_input_tokens` | Yes | P0 | ✓ |
| [x] UT-005 | Cache read tokens | `message.usage.cache_read_input_tokens` | Yes | P0 | ✓ |
| [x] UT-006 | 5-min cache tokens | `message.usage.cache_creation.ephemeral_5m_input_tokens` | Yes | P1 | ✓ |
| [x] UT-007 | 1-hour cache tokens | `message.usage.cache_creation.ephemeral_1h_input_tokens` | Yes | P1 | ✓ |
| [x] UT-008 | Web search count | `message.usage.server_tool_use.web_search_requests` | Yes | P1 | ✓ |
| [x] UT-009 | Web fetch count | `message.usage.server_tool_use.web_fetch_requests` | Yes | P1 | ✓ |
| [x] UT-010 | Service tier | `message.usage.service_tier` | Yes | P1 | ✓ |
| [x] UT-011 | Stop reason | `message.stop_reason` | Yes | P0 | ✓ |

### 4.4 Context & Environment Elements (4 elements)

| ID | Element | JSONL Path | Required | Priority |
|----|---------|------------|----------|----------|
| [x] EN-001 | Working directory | `cwd` | Yes | P0 | ✓ |
| [x] EN-002 | Git branch | `gitBranch` | Yes | P0 | ✓ |
| [x] EN-003 | Claude Code version | `version` | Yes | P0 | ✓ |
| [x] EN-004 | User type | `userType` | Yes | P1 | ✓ |

### 4.5 Agent & Hierarchy Elements (4 elements)

| ID | Element | JSONL Path | Required | Priority |
|----|---------|------------|----------|----------|
| [x] AH-001 | Sidechain status | `isSidechain` | Yes | P0 | ✓ |
| [x] AH-002 | Teammate status | `isTeammate` | Yes | P0 | ✓ |
| [x] AH-003 | Agent ID | `agentId` | Yes | P0 | ✓ |
| [x] AH-004 | Session slug | `slug` | Yes | P1 | ✓ |

### 4.6 Error & Recovery Elements (7 elements)

| ID | Element | JSONL Path | Required | Priority |
|----|---------|------------|----------|----------|
| [x] ER-001 | API error flag | `isApiErrorMessage` | Yes | P0 | ✓ |
| [x] ER-002 | Error string | `error` (assistant messages) | Yes | P0 | ✓ |
| [x] ER-003 | Error status | `error.status` (system/api_error) | Yes | P0 | ✓ |
| [x] ER-004 | Retry attempt | `retryAttempt` | Yes | P0 | ✓ |
| [x] ER-005 | Max retries | `maxRetries` | Yes | P0 | ✓ |
| [x] ER-006 | Retry delay | `retryInMs` | Yes | P0 | ✓ |
| [x] ER-007 | Error cause | `cause` | Yes | P1 | ✓ |

### 4.7 System & Metadata Elements (9 elements)

| ID | Element | JSONL Path | Required | Priority |
|----|---------|------------|----------|----------|
| [x] SM-001 | System subtypes (4 types) | `subtype` | Yes | P0 | ✓ |
| [x] SM-002 | Severity level | `level` | Yes | P1 | ✓ |
| [x] SM-003 | Tool metadata | `toolUseResult` | Yes | P0 | ✓ |
| [x] SM-004 | Compaction metadata | `compactMetadata` | Yes | P0 | ✓ |
| [x] SM-005 | Container info | `message.container` | Yes | P1 | ✓ |
| [x] SM-006 | Context management | `message.context_management` | Yes | P1 | ✓ |
| [x] SM-007 | Thinking level | `thinkingMetadata.level` | Yes | P1 | ✓ |
| [x] SM-008 | Thinking disabled | `thinkingMetadata.disabled` | Yes | P1 | ✓ |
| [x] SM-009 | Thinking triggers | `thinkingMetadata.triggers` | Yes | P1 | ✓ |

### 4.8 Specialized Message Elements (12 elements)

| ID | Element | JSONL Path | Required | Priority |
|----|---------|------------|----------|----------|
| [x] SP-001 | Summary text | `summary` | Yes | P0 | ✓ |
| [x] SP-002 | Leaf UUID | `leafUuid` | Yes | P0 | ✓ |
| [x] SP-003 | File snapshots | `snapshot.trackedFileBackups` | Yes | P0 | ✓ |
| [x] SP-004 | Backup file names | `trackedFileBackups[].backupFileName` | Yes | P1 | ✓ |
| [x] SP-005 | Backup versions | `trackedFileBackups[].version` | Yes | P1 | ✓ |
| [x] SP-006 | Backup timestamps | `trackedFileBackups[].backupTime` | Yes | P1 | ✓ |
| [x] SP-007 | Queue operations (4 types) | `operation`, `content` | Yes | P0 | ✓ |
| [x] SP-008 | Local commands | `content` (system/local_command) | Yes | P1 | ✓ |
| [x] SP-009 | Todo content | `todos[].content` | Yes | P0 | ✓ |
| [x] SP-010 | Todo status | `todos[].status` | Yes | P0 | ✓ |
| [x] SP-011 | Todo active form | `todos[].activeForm` | Yes | P0 | ✓ |
| [x] SP-012 | Structured patches | `toolUseResult.structuredPatch` | Yes | P0 | ✓ |

### 4.9 Beyond-JSONL Data Elements (20 elements)

| ID | Element | Source | Required | Priority |
|----|---------|--------|----------|----------|
| [x] BJ-001 | File backup contents | `~/.claude/filehistory/` | Yes | P1 | ✓ |
| [x] BJ-002 | Global settings | `~/.claude/settings.json` | Yes | P1 | ✓ |
| [x] BJ-003 | Project settings | `.claude/settings.json` | Yes | P1 | ✓ |
| [x] BJ-004 | CLAUDE.md instructions | `~/.claude/CLAUDE.md` | Yes | P1 | ✓ |
| [x] BJ-005 | Project CLAUDE.md | `.claude/CLAUDE.md` | Yes | P1 | ✓ |
| [x] BJ-006 | MCP server configs | `~/.claude/mcp.json` | Yes | P1 | ✓ |
| [x] BJ-007 | Custom commands | `~/.claude/commands/` | Yes | P2 | ✓ |
| [x] BJ-008 | Project commands | `.claude/commands/` | Yes | P2 | ✓ |
| [x] BJ-009 | API key presence | `~/.claude/.credentials.json` | No | P2 | ✓ |
| [ ] BJ-010 | Permissions state | Runtime (not persisted) | No | P3 |
| [x] BJ-011 | Hook configurations | `settings.json` hooks section | Yes | P1 | ✓ |
| [ ] BJ-012 | Git correlation | `.git/` directory | Yes | P2 |
| [x] BJ-013 | Subagent JSONL files | `agent-*.jsonl` | Yes | P0 | ✓ |
| [x] BJ-014 | Session retention config | `settings.json` | Yes | P2 | ✓ |
| [x] BJ-015 | Sandbox configuration | `settings.json` sandbox section | Yes | P2 | ✓ |
| [ ] BJ-016 | Telemetry OTEL data | OTEL endpoint (if enabled) | No | P3 |
| [x] BJ-017 | Global rules directory | `~/.claude/rules/*.md` | Yes | P1 | ✓ |
| [x] BJ-018 | Project rules directory | `.claude/rules/*.md` | Yes | P1 | ✓ |
| [ ] BJ-019 | Checkpoint data | Checkpoint system (v2.0.64+) | Yes | P1 |
| [ ] BJ-020 | Chrome MCP integration | Chrome extension state (Beta) | No | P2 |
| [x] BJ-021 | Output styles directory | `~/.claude/output-styles/` | Yes | P2 | ✓ |

**Note on BJ-021 (Output Styles):** Output styles were initially deprecated in v2.0.30 (October 2025) but were **un-deprecated** after community feedback. They remain an active feature as of v2.0.74+. Custom output styles modify Claude's communication behavior but do NOT change the JSONL schema—they affect message content, not structure.

### 4.10 New Feature Elements (v2.0.64+)

| ID | Element | JSONL Path / Source | Required | Priority |
|----|---------|---------------------|----------|----------|
| [x] NF-001 | Named session slug | `slug` field | Yes | P1 | ✓ |
| [ ] NF-002 | Session rename events | `/rename` command logs | Yes | P1 |
| [ ] NF-003 | Checkpoint restore events | `/rewind` command logs | Yes | P1 |
| [ ] NF-004 | Chrome MCP tool calls | `mcp__chrome__*` patterns | No | P2 |
| [x] NF-005 | Rules directory content | Path-scoped rule loading | Yes | P1 | ✓ |

### 4.11 LSP Tool Elements (v2.0.74+)

Language Server Protocol integration was added in Claude Code v2.0.74, providing code intelligence features. These elements capture LSP tool invocations and results.

| ID | Element | JSONL Path | Required | Priority |
|----|---------|------------|----------|----------|
| [x] LSP-001 | LSP operation type | `message.content[].input.operation` | Yes | P1 | ✓ |
| [x] LSP-002 | Target file path | `message.content[].input.filePath` | Yes | P1 | ✓ |
| [x] LSP-003 | Line number (1-based) | `message.content[].input.line` | Yes | P1 | ✓ |
| [x] LSP-004 | Character offset (1-based) | `message.content[].input.character` | Yes | P1 | ✓ |
| [x] LSP-005 | Definition locations | `toolUseResult` (goToDefinition) | Yes | P1 | ✓ |
| [x] LSP-006 | Reference locations | `toolUseResult` (findReferences) | Yes | P1 | ✓ |
| [x] LSP-007 | Hover documentation | `toolUseResult` (hover) | Yes | P1 | ✓ |
| [x] LSP-008 | Document symbols | `toolUseResult` (documentSymbol) | Yes | P1 | ✓ |
| [x] LSP-009 | Workspace symbols | `toolUseResult` (workspaceSymbol) | Yes | P1 | ✓ |
| [x] LSP-010 | Implementation locations | `toolUseResult` (goToImplementation) | Yes | P1 | ✓ |
| [x] LSP-011 | Call hierarchy items | `toolUseResult` (prepareCallHierarchy) | Yes | P1 | ✓ |
| [x] LSP-012 | Incoming call references | `toolUseResult` (incomingCalls) | Yes | P1 | ✓ |
| [x] LSP-013 | Outgoing call references | `toolUseResult` (outgoingCalls) | Yes | P1 | ✓ |

**LSP Operations Reference:**

| Operation | Description |
|-----------|-------------|
| `goToDefinition` | Find where a symbol is defined |
| `findReferences` | Find all references to a symbol |
| `hover` | Get hover information (documentation, type info) |
| `documentSymbol` | Get all symbols in a document (functions, classes, variables) |
| `workspaceSymbol` | Search for symbols across the entire workspace |
| `goToImplementation` | Find implementations of an interface or abstract method |
| `prepareCallHierarchy` | Get call hierarchy item at a position |
| `incomingCalls` | Find all callers of the function at a position |
| `outgoingCalls` | Find all functions called by the function at a position |

**Note:** LSP servers must be configured for the file type. If no server is available, an error will be returned in the tool result.

---

## 5. Core Feature Requirements

### 5.1 JSONL Parsing & Extraction

| ID | Requirement | Priority | Status |
|----|-------------|----------|--------|
| [x] PARSE-001 | Parse single JSONL session file | P0 | ✓ |
| [x] PARSE-002 | Parse all sessions in project directory | P0 | ✓ |
| [x] PARSE-003 | Parse all projects in ~/.claude/projects/ | P0 | ✓ |
| [x] PARSE-004 | Handle malformed/corrupted lines gracefully | P0 | ✓ (lenient mode) |
| [x] PARSE-005 | Support streaming parsing for large files | P1 | ✓ |
| [x] PARSE-006 | Detect and parse subagent (agent-*.jsonl) files | P0 | ✓ |
| [x] PARSE-007 | Handle all 7 message types correctly | P0 | ✓ |
| [x] PARSE-008 | Handle all 5 content block types correctly | P0 | ✓ |
| [x] PARSE-009 | Preserve original JSON for lossless export | P1 | ✓ |
| [x] PARSE-010 | Validate against known schema | P1 | ✓ |
| [x] PARSE-011 | Handle partially-written lines at EOF gracefully | P1 | ✓ |
| [x] PARSE-012 | Detect active session via file modification heuristics | P1 | ✓ |

#### 5.1.1 Concurrent Session Handling (PARSE-011, PARSE-012 Implementation)

When Claude Code is actively writing to a JSONL file while claude-snatch reads it, special handling is required:

**Partial Line Detection (PARSE-011):**
- Read file in streaming mode with line buffering
- Detect incomplete final line (no newline terminator)
- Options: (a) Skip incomplete line, (b) Wait and retry, (c) Return partial with warning
- Default behavior: Skip with warning, allow `--wait-for-complete` flag

**Active Session Detection (PARSE-012):**
- Check file modification time vs current time
- If modified within last 5 seconds: consider "possibly active"
- If modified within last 60 seconds: consider "recently active"
- Check for file locks (platform-specific)
- Display active session indicator in TUI/CLI output

**File Locking Strategy:**
- Claude Code does NOT use advisory file locks on JSONL files
- Safe to read concurrently without coordination
- Use read-only file handles exclusively
- Never hold file handles open longer than necessary

**Race Condition Mitigation:**
```
1. Open file in read-only mode
2. Seek to beginning
3. Read and parse line-by-line
4. On incomplete final line: note position, continue
5. If --follow mode: poll for changes, resume from position
```

### 5.2 Session Discovery & Management

| ID | Requirement | Priority | Status |
|----|-------------|----------|--------|
| [x] SESS-001 | Auto-discover ~/.claude/projects/ location | P0 | ✓ |
| [x] SESS-002 | Support XDG config path (~/.config/claude/) | P1 | ✓ |
| [x] SESS-003 | Support Windows path (%USERPROFILE%\.claude\) | P1 | ✓ |
| [x] SESS-004 | Support WSL paths correctly | P1 | ✓ |
| [x] SESS-005 | Decode project path encoding (/ → -) | P0 | ✓ |
| [x] SESS-006 | List all available projects | P0 | ✓ |
| [x] SESS-007 | List all sessions within a project | P0 | ✓ |
| [x] SESS-008 | Show session metadata (date, messages, tokens) | P0 | ✓ |
| [x] SESS-009 | Detect active/running sessions | P1 | ✓ |
| [x] SESS-010 | Support session filtering by date range | P1 | ✓ |
| [x] SESS-011 | Support session filtering by keyword/regex | P1 | ✓ |
| [x] SESS-012 | Map session UUIDs to project paths | P0 | ✓ |

### 5.3 Conversation Reconstruction

| ID | Requirement | Priority | Status |
|----|-------------|----------|--------|
| [x] CONV-001 | Build conversation tree from parentUuid links | P0 | ✓ |
| [x] CONV-002 | Handle conversation branching/forking | P0 | ✓ |
| [x] CONV-003 | Preserve logicalParentUuid across compaction | P0 | ✓ |
| [x] CONV-004 | Group streaming chunks by message.id | P0 | ✓ |
| [x] CONV-005 | Reconstruct chronological order | P0 | ✓ |
| [x] CONV-006 | Identify main thread vs. sidechains | P0 | ✓ |
| [x] CONV-007 | Link tool_use to corresponding tool_result | P0 | ✓ |
| [x] CONV-008 | Handle retry chains (error recovery flow) | P0 | ✓ |
| [x] CONV-009 | Support multiple concurrent sessions | P1 | ✓ |
| [ ] CONV-010 | Merge forked conversations (optional) | P2 | |

### 5.4 Analytics & Statistics

| ID | Requirement | Priority | Status |
|----|-------------|----------|--------|
| [x] STAT-001 | Calculate total token usage (input/output) | P0 | ✓ |
| [x] STAT-002 | Calculate cache efficiency metrics | P1 | ✓ |
| [x] STAT-003 | Track cost estimates by model | P1 | ✓ |
| [x] STAT-004 | Count tool invocations by type | P0 | ✓ |
| [x] STAT-005 | Measure session duration | P0 | ✓ |
| [x] STAT-006 | Track message counts by role | P0 | ✓ |
| [x] STAT-007 | Calculate thinking token usage | P1 | ✓ |
| [x] STAT-008 | Identify most-used tools | P1 | ✓ |
| [x] STAT-009 | Track error rates and types | P1 | ✓ |
| [ ] STAT-010 | Generate usage trends over time | P2 | |
| [ ] STAT-011 | Calculate average response times | P2 | |
| [ ] STAT-012 | Track file modification patterns | P2 | |
| [x] STAT-013 | Real-time burn rate calculation | P1 | ✓ |
| [ ] STAT-014 | Usage predictions (time to limit) | P1 | |
| [x] STAT-015 | Cost per session/project | P1 | ✓ (SessionAnalytics) |
| [ ] STAT-016 | Cross-session efficiency metrics | P2 | |

### 5.5 Search & Filtering

| ID | Requirement | Priority | Status |
|----|-------------|----------|--------|
| [x] SRCH-001 | Full-text search across conversations | P0 | ✓ |
| [x] SRCH-002 | Regex pattern matching | P1 | ✓ |
| [x] SRCH-003 | Filter by message type | P0 | ✓ |
| [x] SRCH-004 | Filter by date range | P0 | ✓ |
| [x] SRCH-005 | Filter by model used | P1 | ✓ (`--model` flag) |
| [x] SRCH-006 | Filter by tool names | P1 | ✓ (`--tool-name` flag) |
| [x] SRCH-007 | Filter by error status | P1 | ✓ (`--errors` flag) |
| [ ] SRCH-008 | Filter by token usage thresholds | P2 | |
| [ ] SRCH-009 | Filter by git branch | P2 | |
| [x] SRCH-010 | Search within tool inputs/outputs | P1 | ✓ |
| [ ] SRCH-011 | Fuzzy search support | P2 | |
| [ ] SRCH-012 | Search result ranking/relevance | P2 | |

### 5.6 File Backup Integration

| ID | Requirement | Priority | Status |
|----|-------------|----------|--------|
| [x] BKUP-001 | Parse file-history-snapshot events | P0 | ✓ |
| [x] BKUP-002 | Locate backup files in ~/.claude/filehistory/ | P1 | ✓ |
| [ ] BKUP-003 | Retrieve backup content by reference | P1 | |
| [ ] BKUP-004 | Show file version history | P1 | |
| [ ] BKUP-005 | Diff between file versions | P2 | |
| [ ] BKUP-006 | Export file at specific version | P2 | |
| [ ] BKUP-007 | Reconstruct file state at any point | P2 | |

### 5.7 Subagent Correlation

| ID | Requirement | Priority | Status |
|----|-------------|----------|--------|
| [x] AGENT-001 | Detect Task tool invocations | P0 | ✓ |
| [x] AGENT-002 | Locate corresponding agent-*.jsonl files | P0 | ✓ |
| [x] AGENT-003 | Link parent session to subagent sessions | P0 | ✓ |
| [x] AGENT-004 | Parse subagent JSONL with same fidelity | P0 | ✓ |
| [x] AGENT-005 | Show nested agent hierarchy | P1 | ✓ (TUI tree) |
| [x] AGENT-006 | Aggregate statistics across agents | P1 | ✓ (AggregatedStats) |
| [x] AGENT-007 | Export combined parent+agent transcripts | P1 | ✓ (--combine-agents) |

---

## 6. Output Format Requirements

### 6.1 Markdown Export

| ID | Requirement | Priority | Status |
|----|-------------|----------|--------|
| [x] MD-001 | Export human-readable markdown | P0 | ✓ |
| [x] MD-002 | Proper syntax highlighting for code blocks | P0 | ✓ |
| [ ] MD-003 | Collapsible sections for long content | P1 | |
| [x] MD-004 | Include metadata header | P0 | ✓ |
| [x] MD-005 | Include token usage summary | P1 | ✓ |
| [x] MD-006 | Preserve timestamps | P0 | ✓ |
| [x] MD-007 | Format tool calls with inputs/outputs | P0 | ✓ |
| [x] MD-008 | Support thinking block toggle | P1 | ✓ |
| [ ] MD-009 | Include table of contents | P2 | |
| [x] MD-010 | GitHub-flavored markdown compliance | P0 | ✓ |

### 6.2 JSON Export

| ID | Requirement | Priority | Status |
|----|-------------|----------|--------|
| [x] JSON-001 | Export structured JSON | P0 | ✓ |
| [x] JSON-002 | Lossless round-trip capability | P0 | ✓ |
| [x] JSON-003 | Pretty-printed option | P0 | ✓ |
| [x] JSON-004 | Minified option | P1 | ✓ |
| [x] JSON-005 | JSON Lines output option | P1 | ✓ (JSONL format) |
| [x] JSON-006 | Include all 64 JSONL elements | P0 | ✓ |
| [ ] JSON-007 | Schema-compliant output | P1 | |
| [ ] JSON-008 | Streaming JSON output for large files | P2 | |

### 6.3 HTML Export

| ID | Requirement | Priority | Status |
|----|-------------|----------|--------|
| [x] HTML-001 | Self-contained HTML file | P1 | ✓ |
| [x] HTML-002 | Syntax highlighting for code | P1 | ✓ |
| [x] HTML-003 | Dark/light theme support | P2 | ✓ |
| [x] HTML-004 | Collapsible sections | P1 | ✓ |
| [ ] HTML-005 | Navigation/table of contents | P2 | |
| [x] HTML-006 | Responsive design | P2 | ✓ |
| [ ] HTML-007 | Inline images (base64) | P2 | |
| [x] HTML-008 | Print-friendly CSS | P2 | ✓ |
| [ ] HTML-009 | Interactive filtering (JS) | P3 | |

### 6.4 Plain Text Export

| ID | Requirement | Priority | Status |
|----|-------------|----------|--------|
| [x] TXT-001 | Clean plain text output | P1 | TextExporter with word wrapping |
| [x] TXT-002 | Configurable line width | P2 | TextExporter::with_line_width() |
| [x] TXT-003 | ASCII art formatting | P2 | ASCII separators in TextExporter |
| [x] TXT-004 | Tool call formatting | P1 | Tool use/result output in TextExporter |
| [x] TXT-005 | Timestamp inclusion | P1 | Respects ExportOptions.include_timestamps |

### 6.5 XML Export

| ID | Requirement | Priority | Status |
|----|-------------|----------|--------|
| [x] XML-001 | Well-formed XML output | P2 | XmlExporter with proper escaping |
| [x] XML-002 | Full metadata labeling | P2 | Session metadata, usage stats |
| [x] XML-003 | Session-level and message-level elements | P2 | conversation/messages/message structure |
| [x] XML-004 | UUID preservation | P2 | uuid/parent-uuid attributes |
| [ ] XML-005 | Schema definition (XSD) | P3 | |

### 6.6 SQLite Export

| ID | Requirement | Priority | Status |
|----|-------------|----------|--------|
| [x] SQL-001 | Export to SQLite database | P2 | SqliteExporter.export_to_file() |
| [x] SQL-002 | Normalized schema design | P2 | 7 tables with proper normalization |
| [x] SQL-003 | Include all data elements | P2 | Messages, content, thinking, tools, usage |
| [x] SQL-004 | Foreign key relationships | P2 | FOREIGN KEY constraints with ON DELETE CASCADE |
| [x] SQL-005 | Full-text search indexes | P2 | FTS5 for messages and thinking |
| [ ] SQL-006 | Incremental updates | P3 | |

### 6.7 CSV Export

| ID | Requirement | Priority | Status |
|----|-------------|----------|--------|
| [x] CSV-001 | Export messages to CSV | P2 | CsvExporter with Messages mode |
| [x] CSV-002 | Export usage statistics to CSV | P2 | CsvExporter with Usage mode |
| [x] CSV-003 | Export tool invocations to CSV | P2 | CsvExporter with Tools mode |
| [x] CSV-004 | Configurable column selection | P2 | Multiple CsvMode options |
| [x] CSV-005 | Proper escaping and quoting | P2 | escape_field() with RFC 4180 compliance |

---

## 7. CLI Interface Requirements

### 7.1 Command Structure

| ID | Requirement | Priority | Status |
|----|-------------|----------|--------|
| [x] CLI-001 | `snatch list` - List projects/sessions | P0 | ✓ |
| [x] CLI-002 | `snatch export` - Export sessions | P0 | ✓ |
| [x] CLI-003 | `snatch search` - Search conversations | P0 | ✓ |
| [x] CLI-004 | `snatch stats` - Show statistics | P0 | ✓ |
| [x] CLI-005 | `snatch info` - Show session details | P0 | ✓ |
| [x] CLI-006 | `snatch watch` - Monitor active sessions | P1 | ✓ |
| [x] CLI-007 | `snatch verify` - Validate JSONL integrity | P1 | ✓ (as `validate`) |
| [x] CLI-008 | `snatch diff` - Compare sessions | P2 | ✓ |
| [x] CLI-009 | `snatch config` - Manage configuration | P2 | ✓ |
| [x] CLI-010 | `snatch completions` - Shell completions | P1 | ✓ |
| [x] CLI-011 | Documented exit codes for scripting | P0 | ✓ |

#### 7.1.1 Exit Code Reference (CLI-011)

Standard exit codes for scripting integration and automation:

| Exit Code | Constant | Description |
|-----------|----------|-------------|
| 0 | `EXIT_SUCCESS` | Operation completed successfully |
| 1 | `EXIT_GENERAL_ERROR` | General/unspecified error |
| 2 | `EXIT_PARSE_ERROR` | JSONL parsing failed (malformed input) |
| 3 | `EXIT_FILE_NOT_FOUND` | Specified file or session not found |
| 4 | `EXIT_PERMISSION_DENIED` | Insufficient permissions to read/write |
| 5 | `EXIT_CONFIG_ERROR` | Invalid configuration file or options |
| 6 | `EXIT_EXPORT_ERROR` | Export operation failed |
| 7 | `EXIT_SEARCH_ERROR` | Search operation failed |
| 64 | `EXIT_USAGE_ERROR` | Invalid command-line usage (BSD standard) |
| 65 | `EXIT_DATA_ERROR` | Input data format error (BSD standard) |
| 74 | `EXIT_IO_ERROR` | I/O error (BSD standard) |
| 130 | `EXIT_INTERRUPTED` | Terminated by Ctrl+C (128 + SIGINT) |

**Usage Example:**
```bash
snatch export -f json -o output.json --session abc123
case $? in
  0) echo "Export successful" ;;
  3) echo "Session not found" ;;
  4) echo "Permission denied" ;;
  *) echo "Export failed with code $?" ;;
esac
```

### 7.2 Global Options

| ID | Requirement | Priority | Status |
|----|-------------|----------|--------|
| [x] OPT-001 | `--help` - Show help | P0 | ✓ |
| [x] OPT-002 | `--version` - Show version | P0 | ✓ |
| [x] OPT-003 | `--verbose` / `-v` - Verbose output | P0 | ✓ |
| [x] OPT-004 | `--quiet` / `-q` - Suppress output | P1 | ✓ |
| [x] OPT-005 | `--json` - JSON output mode | P0 | ✓ |
| [x] OPT-006 | `--no-color` - Disable colors | P1 | ✓ (as `--color`) |
| [ ] OPT-007 | `--config` - Custom config file | P2 | |
| [x] OPT-008 | `--claude-dir` - Custom Claude directory | P1 | ✓ |

### 7.3 Export Command Options

| ID | Requirement | Priority | Status |
|----|-------------|----------|--------|
| [x] EXP-001 | `--format` / `-f` - Output format | P0 | ✓ |
| [x] EXP-002 | `--output` / `-o` - Output file/directory | P0 | ✓ |
| [x] EXP-003 | `--all` - Export all sessions | P0 | ✓ |
| [x] EXP-004 | `--project` / `-p` - Specific project | P0 | ✓ |
| [x] EXP-005 | `--session` / `-s` - Specific session | P0 | ✓ (positional arg) |
| [x] EXP-006 | `--since` - Filter by start date | P1 | ✓ |
| [x] EXP-007 | `--until` - Filter by end date | P1 | ✓ |
| [x] EXP-008 | `--include-agents` - Include subagents | P0 | ✓ |
| [x] EXP-009 | `--include-thinking` - Include thinking | P0 | ✓ (as `--thinking`) |
| [x] EXP-010 | `--include-tools` - Include tool calls | P0 | ✓ (as `--tool-use`) |
| [x] EXP-011 | `--detailed` - Full metadata export | P0 | ✓ (as `--metadata`) |
| [x] EXP-012 | `--lossless` - Preserve all data | P0 | ✓ |
| [x] EXP-013 | `--stdout` - Output to stdout | P1 | ✓ (default) |
| [x] EXP-014 | `--overwrite` - Overwrite existing | P1 | ✓ |

### 7.4 Search Command Options

| ID | Requirement | Priority | Status |
|----|-------------|----------|--------|
| [x] SCH-001 | `--regex` / `-r` - Regex search | P1 | ✓ (default) |
| [x] SCH-002 | `--case-insensitive` / `-i` - Case insensitive | P1 | ✓ |
| [x] SCH-003 | `--context` / `-C` - Show context lines | P1 | ✓ |
| [x] SCH-004 | `--files-only` - Show only file names | P1 | ✓ |
| [x] SCH-005 | `--count` - Show match counts | P1 | ✓ |
| [x] SCH-006 | `--type` - Filter by message type | P1 | ✓ |

### 7.5 Output & Formatting

| ID | Requirement | Priority | Status |
|----|-------------|----------|--------|
| [x] OUT-001 | Colored terminal output | P0 | ✓ |
| [x] OUT-002 | Progress bars for long operations | P0 | ✓ |
| [x] OUT-003 | Table formatting for lists | P0 | ✓ |
| [x] OUT-004 | Human-readable file sizes | P1 | ✓ |
| [ ] OUT-005 | Relative timestamps option | P2 | |
| [ ] OUT-006 | Pager support (less/more) | P2 | |

### 7.6 Shell Completions

| ID | Requirement | Priority | Status |
|----|-------------|----------|--------|
| [x] COMP-001 | Bash completions | P1 | ✓ |
| [x] COMP-002 | Zsh completions | P1 | ✓ |
| [x] COMP-003 | Fish completions | P2 | ✓ |
| [x] COMP-004 | PowerShell completions | P2 | ✓ |
| [ ] COMP-005 | Dynamic session/project completion | P2 | |

---

## 8. TUI Interface Requirements

### 8.1 Core TUI Features

| ID | Requirement | Priority | Status |
|----|-------------|----------|--------|
| [x] TUI-001 | Launch with `snatch` or `snatch tui` | P0 | ✓ |
| [x] TUI-002 | Project browser panel | P0 | ✓ |
| [x] TUI-003 | Session list panel | P0 | ✓ |
| [x] TUI-004 | Conversation viewer panel | P0 | ✓ |
| [x] TUI-005 | Split-pane layout | P0 | ✓ |
| [x] TUI-006 | Keyboard navigation | P0 | ✓ |
| [x] TUI-007 | Mouse support | P1 | ✓ (click, scroll) |
| [x] TUI-008 | Resizable panels | P1 | ✓ (auto-resize) |
| [x] TUI-009 | Status bar with stats | P0 | ✓ |
| [ ] TUI-010 | Command palette (Ctrl+P) | P2 | |

### 8.2 Navigation

| ID | Requirement | Priority | Status |
|----|-------------|----------|--------|
| [x] NAV-001 | Arrow keys for list navigation | P0 | ✓ |
| [x] NAV-002 | Enter to select/expand | P0 | ✓ |
| [x] NAV-003 | Tab to switch panels | P0 | ✓ |
| [x] NAV-004 | Page up/down for scrolling | P0 | ✓ |
| [x] NAV-005 | Home/End for list bounds | P1 | ✓ |
| [x] NAV-006 | / for search | P0 | ✓ |
| [x] NAV-007 | Escape to cancel/close | P0 | ✓ |
| [x] NAV-008 | q to quit | P0 | ✓ |
| [x] NAV-009 | Vim-style navigation (j/k/h/l) | P1 | ✓ |
| [ ] NAV-010 | Go to line number | P2 | |

### 8.3 Search & Filter

| ID | Requirement | Priority | Status |
|----|-------------|----------|--------|
| [x] TSRCH-001 | Real-time search as you type | P0 | ✓ |
| [x] TSRCH-002 | Highlight search matches | P0 | ✓ |
| [ ] TSRCH-003 | Filter panel for message types | P1 | |
| [ ] TSRCH-004 | Filter by date range | P1 | |
| [ ] TSRCH-005 | Filter by model | P2 | |
| [ ] TSRCH-006 | Save/load filter presets | P3 | |

### 8.4 Conversation Display

| ID | Requirement | Priority | Status |
|----|-------------|----------|--------|
| [x] DISP-001 | Syntax highlighting for code | P0 | ✓ |
| [x] DISP-002 | Collapsible tool calls | P1 | ✓ (toggle with 'o') |
| [x] DISP-003 | Collapsible thinking blocks | P1 | ✓ (toggle with 't') |
| [ ] DISP-004 | Image preview (sixel/kitty) | P2 | |
| [ ] DISP-005 | Diff view for edits | P2 | |
| [x] DISP-006 | Timestamp display toggle | P1 | ✓ |
| [x] DISP-007 | Token usage display toggle | P1 | ✓ (details panel) |
| [x] DISP-008 | Word wrap toggle | P1 | ✓ (w key) |
| [ ] DISP-009 | Line numbers toggle | P2 | |

### 8.5 Actions & Commands

| ID | Requirement | Priority | Status |
|----|-------------|----------|--------|
| [x] ACT-001 | Export current session | P0 | ✓ (e key) |
| [ ] ACT-002 | Export selected sessions | P1 | |
| [x] ACT-003 | Copy message to clipboard | P1 | ✓ (c key) |
| [x] ACT-004 | Copy code block to clipboard | P1 | ✓ (C key) |
| [ ] ACT-005 | Open in external editor | P2 | |
| [ ] ACT-006 | Resume session in Claude Code | P2 | |
| [x] ACT-007 | Show session statistics | P0 | ✓ (details panel) |
| [x] ACT-008 | Refresh session list | P1 | ✓ (r key) |

### 8.6 Visual Design

| ID | Requirement | Priority | Status |
|----|-------------|----------|--------|
| [x] VIS-001 | Dark theme (default) | P0 | ✓ |
| [x] VIS-002 | Light theme | P1 | ✓ |
| [x] VIS-003 | Theme switching | P2 | ✓ (T key) |
| [ ] VIS-004 | Custom color schemes | P3 | |
| [x] VIS-005 | Unicode box drawing | P0 | ✓ |
| [x] VIS-006 | Emoji support | P1 | ✓ |
| [ ] VIS-007 | ASCII fallback mode | P2 | |
| [x] VIS-008 | Responsive layout | P1 | ✓ |

---

## 9. Performance Requirements

### 9.1 Speed Benchmarks

| ID | Requirement | Target | Status |
|----|-------------|--------|--------|
| [ ] PERF-001 | Parse 1MB JSONL | <50ms | |
| [ ] PERF-002 | Parse 10MB JSONL | <500ms | |
| [ ] PERF-003 | Parse 100MB JSONL | <5s | |
| [ ] PERF-004 | Parse 1GB JSONL | <60s | |
| [ ] PERF-005 | List all projects | <100ms | |
| [ ] PERF-006 | Search 100 sessions | <1s | |
| [ ] PERF-007 | Export to markdown | <2x parse time | |
| [ ] PERF-008 | TUI startup | <100ms | |
| [ ] PERF-009 | TUI refresh rate | 60fps minimum | |
| [ ] PERF-010 | Configurable max file size limit | Default: 10GB | |
| [ ] PERF-011 | Configurable batch session limit | Default: unlimited | |

### 9.2 Memory Efficiency

| ID | Requirement | Target | Status |
|----|-------------|--------|--------|
| [ ] MEM-001 | Peak memory (10MB file) | <50MB | |
| [ ] MEM-002 | Peak memory (100MB file) | <300MB | |
| [ ] MEM-003 | Streaming mode memory | O(1) | |
| [ ] MEM-004 | TUI idle memory | <30MB | |
| [ ] MEM-005 | No memory leaks | 0 leaks | |
| [ ] MEM-006 | Configurable memory ceiling with graceful degradation | Default: 512MB | |

### 9.3 Concurrency & Parallelism

| ID | Requirement | Priority | Status |
|----|-------------|----------|--------|
| [x] CONC-001 | Parallel file parsing | P1 | ✓ (`rayon` parallel iterators in stats) |
| [ ] CONC-002 | Async I/O operations | P1 | |
| [ ] CONC-003 | Background indexing | P2 | |
| [x] CONC-004 | Non-blocking TUI updates | P0 | ✓ (event handler) |
| [ ] CONC-005 | Configurable thread count | P2 | |

---

## 10. Architecture Requirements

### 10.1 Core Architecture

| ID | Requirement | Priority | Status |
|----|-------------|----------|--------|
| [x] ARCH-001 | Modular component design | P0 | ✓ |
| [x] ARCH-002 | Clear separation of concerns | P0 | ✓ |
| [x] ARCH-003 | Parser module (JSONL) | P0 | ✓ |
| [x] ARCH-004 | Exporter module (formats) | P0 | ✓ |
| [x] ARCH-005 | CLI module | P0 | ✓ |
| [x] ARCH-006 | TUI module | P0 | ✓ |
| [x] ARCH-007 | Analytics module | P1 | ✓ |
| [x] ARCH-008 | Search module | P1 | ✓ |
| [x] ARCH-009 | Configuration module | P1 | ✓ |

### 10.2 Data Model

| ID | Requirement | Priority | Status |
|----|-------------|----------|--------|
| [x] DATA-001 | Strongly-typed message structures | P0 | ✓ |
| [x] DATA-002 | Type-safe content block enums | P0 | ✓ |
| [x] DATA-003 | Complete tool result types | P0 | ✓ |
| [x] DATA-004 | Session/Project hierarchy | P0 | ✓ |
| [x] DATA-005 | Conversation tree structure | P0 | ✓ |
| [x] DATA-006 | Serialization/deserialization traits | P0 | ✓ |
| [ ] DATA-007 | Zero-copy parsing where possible | P1 | |
| [x] DATA-008 | Unknown field preservation (forward-compat) | P0 | ✓ (via IndexMap) |
| [x] DATA-009 | Optional field handling for version differences | P0 | ✓ |

### 10.3 Schema Versioning Strategy

| ID | Requirement | Priority | Status |
|----|-------------|----------|--------|
| [x] SCHEMA-001 | Detect Claude Code version from `version` field | P0 | ✓ |
| [x] SCHEMA-002 | Maintain schema definitions per major version | P0 | ✓ |
| [x] SCHEMA-003 | Graceful handling of unknown fields | P0 | ✓ |
| [x] SCHEMA-004 | Preserve unknown fields in lossless mode | P0 | ✓ |
| [ ] SCHEMA-005 | Version-specific parsing strategies | P1 | |
| [ ] SCHEMA-006 | Schema migration for export formats | P1 | |
| [ ] SCHEMA-007 | Backward compatibility with v1.x logs | P2 | |
| [ ] SCHEMA-008 | Schema change detection and warnings | P1 | |
| [x] SCHEMA-009 | Document known schema versions | P1 | ✓ |
| [ ] SCHEMA-010 | Automated schema diff on CI | P2 | |

#### 10.3.1 Version Detection Methodology (SCHEMA-001 Implementation)

**Field Location:** The `version` field appears at the top level of each JSONL line:
```json
{"type":"user","timestamp":"...","version":"2.0.74","sessionId":"...","message":{...}}
```

**Detection Algorithm:**
1. Parse first valid JSONL line in file
2. Extract `version` field value (string format: "X.Y.Z")
3. Parse as semver: major.minor.patch
4. Select appropriate schema definition based on major.minor version
5. Cache version for subsequent lines in same file

**Fallback Behavior:**
- **Missing `version` field**: Assume earliest supported schema (v1.0.0)
- **Malformed version string**: Log warning, treat as unknown version
- **Unknown version**: Use latest known schema with unknown field preservation enabled

**Version Comparison Logic:**
```
Given versions A (file) and B (schema):
- If A.major > B.major: Forward-compatible mode (preserve unknowns)
- If A.major < B.major: Legacy mode (apply migrations if available)
- If A.major == B.major && A.minor > B.minor: Minor forward-compat
- If A.major == B.major && A.minor <= B.minor: Exact match
```

**Known Schema Versions:**

| Version Range | Schema Identifier | Key Changes |
|---------------|-------------------|-------------|
| 1.0.x | `v1_legacy` | Original format |
| 2.0.0 - 2.0.29 | `v2_base` | Major restructure, new message types |
| 2.0.30 - 2.0.39 | `v2_sandbox` | Sandbox mode for Bash tool (Linux/macOS) |
| 2.0.40 - 2.0.44 | `v2_slug` | Added `slug` field for human-readable session names |
| 2.0.45 - 2.0.55 | `v2_hooks` | Permission-related hook events |
| 2.0.56 - 2.0.59 | `v2_compact` | Expanded `compactMetadata` structure |
| 2.0.60 - 2.0.63 | `v2_agents` | Background agents, async task execution |
| 2.0.64 - 2.0.69 | `v2_unified` | **Breaking**: Unified `TaskOutput` (AgentOutputTool/BashOutputTool deprecated); named sessions |
| 2.0.70 - 2.0.71 | `v2_thinking` | Added `thinkingMetadata` structure, MCP wildcard permissions |
| 2.0.72 - 2.0.73 | `v2_chrome` | Chrome integration MCP (beta), web teleport support |
| 2.0.74+ | `v2_lsp` | LSP tool, output styles un-deprecated |

**Migration Notes:**
- v2.0.64 is a **breaking change**: `AgentOutputTool` and `BashOutputTool` were unified into `TaskOutput`
- Always check `version` field to determine available fields
- Use optional handling when accessing newer fields for backward compatibility

### 10.4 Caching & Indexing Architecture

| ID | Requirement | Priority | Status |
|----|-------------|----------|--------|
| [x] CACHE-001 | Session metadata cache | P1 | ✓ (`SessionMetadataCache`) |
| [x] CACHE-002 | Parsed message cache (LRU) | P1 | ✓ (`ParsedEntriesCache` with LRU) |
| [x] CACHE-003 | Cache invalidation on file change | P1 | ✓ (mtime-based) |
| [x] CACHE-004 | Configurable cache size limits | P2 | ✓ (`CacheConfig.max_size`) |
| [ ] CACHE-005 | Cache persistence between runs | P2 | |
| [ ] INDEX-001 | Full-text search index (tantivy or similar) | P1 | |
| [ ] INDEX-002 | Incremental index updates | P1 | |
| [ ] INDEX-003 | Index storage location configuration | P2 | |
| [ ] INDEX-004 | Index rebuild command | P1 | |
| [ ] INDEX-005 | Field-specific indexes (tool names, models) | P2 | |

### 10.5 Configuration System

| ID | Requirement | Priority | Status |
|----|-------------|----------|--------|
| [x] CFG-001 | TOML configuration file format | P1 | ✓ |
| [x] CFG-002 | Config file location: `~/.config/claude-snatch/config.toml` | P1 | ✓ |
| [x] CFG-003 | Environment variable overrides | P1 | ✓ (`SNATCH_*` env vars for all global options) |
| [x] CFG-004 | Command-line argument overrides | P0 | ✓ |
| [x] CFG-005 | Default configuration generation | P1 | ✓ (`config init`) |
| [x] CFG-006 | Configuration validation | P1 | ✓ |
| [ ] CFG-007 | Per-project configuration (`.claude-snatch.toml`) | P2 | |
| [x] CFG-008 | Log output location (stderr default, file optional) | P1 | ✓ (`--log-file`, default stderr) |
| [x] CFG-009 | Log level configuration (error/warn/info/debug/trace) | P1 | ✓ (`--log-level`, `SNATCH_LOG_LEVEL`) |
| [ ] CFG-010 | Structured logging format option (JSON for machines) | P2 | |

### 10.6 Error Handling

| ID | Requirement | Priority | Status |
|----|-------------|----------|--------|
| [x] ERR-001 | Comprehensive error types | P0 | ✓ (SnatchError enum) |
| [x] ERR-002 | Contextual error messages | P0 | ✓ |
| [x] ERR-003 | Error recovery strategies | P0 | ✓ (is_recoverable) |
| [x] ERR-004 | Graceful degradation | P1 | ✓ |
| [x] ERR-005 | Structured error reporting | P1 | ✓ |
| [x] ERR-006 | User-friendly error display | P0 | ✓ |

### 10.4 Extensibility

| ID | Requirement | Priority | Status |
|----|-------------|----------|--------|
| [ ] EXT-001 | Plugin architecture for exporters | P2 | |
| [ ] EXT-002 | Custom output format support | P2 | |
| [ ] EXT-003 | Hook system for processing | P3 | |
| [ ] EXT-004 | API for programmatic use | P2 | |

---

## 11. Quality & Reliability Requirements

### 11.1 Code Quality

| ID | Requirement | Priority | Status |
|----|-------------|----------|--------|
| [x] QUAL-001 | Clippy warnings = 0 | P0 | ✓ (pedantic allowed) |
| [x] QUAL-002 | rustfmt compliance | P0 | ✓ |
| [x] QUAL-003 | Documentation coverage >90% | P1 | ✓ |
| [x] QUAL-004 | No unsafe code (unless justified) | P0 | ✓ (forbid) |
| [ ] QUAL-005 | Dependency audit clean | P1 | |
| [x] QUAL-006 | MSRV policy (1.70+) | P1 | ✓ (1.75) |

### 11.2 Reliability

| ID | Requirement | Priority | Status |
|----|-------------|----------|--------|
| [x] REL-001 | Handle corrupted input gracefully | P0 | ✓ |
| [x] REL-002 | No panics on user input | P0 | ✓ |
| [ ] REL-003 | Atomic file writes | P1 | |
| [x] REL-004 | Interrupt handling (Ctrl+C) | P0 | ✓ |
| [x] REL-005 | Idempotent operations | P1 | ✓ |
| [ ] REL-006 | Transactional exports | P2 | |

### 11.3 Compatibility

| ID | Requirement | Priority | Status |
|----|-------------|----------|--------|
| [x] COMPAT-001 | Linux x86_64 | P0 | ✓ |
| [x] COMPAT-002 | Linux aarch64 | P1 | ✓ |
| [x] COMPAT-003 | macOS x86_64 | P0 | ✓ |
| [x] COMPAT-004 | macOS aarch64 (Apple Silicon) | P0 | ✓ |
| [x] COMPAT-005 | Windows x86_64 | P1 | ✓ |
| [x] COMPAT-006 | WSL compatibility | P1 | ✓ |
| [x] COMPAT-007 | Claude Code v2.0.x | P0 | ✓ |
| [ ] COMPAT-008 | Claude Code v1.x (legacy) | P2 | |

---

## 12. Security & Privacy Requirements

### 12.1 Data Handling

| ID | Requirement | Priority | Status |
|----|-------------|----------|--------|
| [x] SEC-001 | No network calls (offline-only) | P0 | ✓ |
| [x] SEC-002 | No telemetry or analytics | P0 | ✓ |
| [x] SEC-003 | No data collection | P0 | ✓ |
| [x] SEC-004 | Read-only by default | P0 | ✓ |
| [x] SEC-005 | No credential exposure | P0 | ✓ |
| [ ] SEC-006 | Sensitive data redaction option | P2 | |
| [ ] SEC-007 | PII detection warnings | P2 | |

### 12.2 File System Safety

| ID | Requirement | Priority | Status |
|----|-------------|----------|--------|
| [x] FS-001 | No writes outside output paths | P0 | ✓ |
| [x] FS-002 | Respect file permissions | P0 | ✓ |
| [x] FS-003 | Symlink handling (no following) | P1 | ✓ |
| [x] FS-004 | Path traversal prevention | P0 | ✓ |
| [x] FS-005 | Temp file cleanup | P1 | ✓ |
| [x] FS-006 | Path normalization (Windows/WSL/Unix) | P1 | ✓ |
| [x] FS-007 | WSL path detection and handling | P1 | ✓ |

### 12.3 Compliance & Legal

| ID | Requirement | Priority | Status |
|----|-------------|----------|--------|
| [ ] LEGAL-001 | GDPR-compliant export options | P2 | |
| [ ] LEGAL-002 | Data minimization option for shared exports | P2 | |
| [ ] LEGAL-003 | Dependency license audit (cargo-deny) | P1 | |
| [ ] LEGAL-004 | License compatibility verification | P1 | |
| [ ] LEGAL-005 | SPDX license identifiers in output | P2 | |
| [ ] LEGAL-006 | Third-party attribution generation | P2 | |

---

## 13. Documentation Requirements

### 13.1 User Documentation

| ID | Requirement | Priority | Status |
|----|-------------|----------|--------|
| [x] DOC-001 | README.md with quick start | P0 | ✓ |
| [x] DOC-002 | Installation guide | P0 | ✓ |
| [x] DOC-003 | CLI reference manual | P0 | ✓ (--help) |
| [ ] DOC-004 | TUI user guide | P1 | |
| [ ] DOC-005 | Export format documentation | P1 | |
| [ ] DOC-006 | Configuration reference | P1 | |
| [ ] DOC-007 | FAQ and troubleshooting | P2 | |
| [ ] DOC-008 | Examples and recipes | P1 | |
| [ ] DOC-009 | Migration guide for version upgrades | P2 | |

#### 13.1.1 Migration Guide Requirements (DOC-009)

The migration guide must document:

**Version Upgrade Procedures:**
- Pre-upgrade checklist (backup config, check compatibility)
- Step-by-step upgrade instructions per platform
- Post-upgrade verification steps
- Rollback procedures

**Breaking Change Notifications:**
- Semantic versioning policy adherence
- Deprecation timeline (minimum 2 minor versions)
- Configuration migration scripts when applicable
- CLI flag/option changes with mapping table

**Data Migration:**
- Cache format changes and rebuild procedures
- Index schema updates and reindexing
- Configuration file format changes
- Export format version compatibility

**Backward Compatibility Strategy:**
- Support window: current major version + 1 previous major version
- Claude Code version support matrix
- Feature availability by version

### 13.2 Developer Documentation

| ID | Requirement | Priority | Status |
|----|-------------|----------|--------|
| [ ] DEVDOC-001 | Architecture overview | P1 | |
| [x] DEVDOC-002 | API documentation (rustdoc) | P0 | ✓ |
| [ ] DEVDOC-003 | Contributing guide | P1 | |
| [x] DEVDOC-004 | Data format specification | P1 | ✓ |
| [x] DEVDOC-005 | Build instructions | P0 | ✓ |
| [ ] DEVDOC-006 | Release process | P2 | |

### 13.3 In-App Help

| ID | Requirement | Priority | Status |
|----|-------------|----------|--------|
| [x] HELP-001 | --help for all commands | P0 | ✓ |
| [ ] HELP-002 | Man pages | P2 | |
| [x] HELP-003 | TUI help overlay (?) | P1 | ✓ |
| [x] HELP-004 | Error messages with suggestions | P1 | ✓ |

---

## 14. Testing Requirements

### 14.1 Unit Testing

| ID | Requirement | Priority | Status |
|----|-------------|----------|--------|
| [x] TEST-001 | Parser unit tests | P0 | ✓ |
| [x] TEST-002 | Exporter unit tests | P0 | ✓ |
| [x] TEST-003 | Search unit tests | P1 | ✓ |
| [x] TEST-004 | Analytics unit tests | P1 | ✓ |
| [x] TEST-005 | Data model tests | P0 | ✓ |
| [ ] TEST-006 | Coverage target >80% | P1 | |

### 14.2 Integration Testing

| ID | Requirement | Priority | Status |
|----|-------------|----------|--------|
| [x] INT-001 | End-to-end CLI tests | P0 | ✓ |
| [x] INT-002 | Export round-trip tests | P0 | ✓ |
| [x] INT-003 | Real JSONL file tests | P0 | ✓ |
| [ ] INT-004 | Large file handling tests | P1 | |
| [x] INT-005 | Cross-platform CI | P1 | ✓ |

### 14.3 Test Data

| ID | Requirement | Priority | Status |
|----|-------------|----------|--------|
| [ ] TDATA-001 | Synthetic JSONL generators | P1 | |
| [x] TDATA-002 | Edge case samples | P0 | ✓ |
| [x] TDATA-003 | All message type samples | P0 | ✓ |
| [x] TDATA-004 | All content block samples | P0 | ✓ |
| [x] TDATA-005 | Error condition samples | P1 | ✓ |
| [ ] TDATA-006 | Large file samples | P1 | |

### 14.4 Fidelity Verification

| ID | Requirement | Priority | Status |
|----|-------------|----------|--------|
| [x] FID-001 | 64-element extraction test suite | P0 | ✓ |
| [ ] FID-002 | Automated fidelity scoring | P1 | |
| [x] FID-003 | Regression tests for each element | P0 | ✓ |
| [ ] FID-004 | Comparison against reference tool | P2 | |

---

## 15. Distribution Requirements

### 15.1 Release Artifacts

| ID | Requirement | Priority | Status |
|----|-------------|----------|--------|
| [x] REL-001 | Prebuilt Linux x86_64 binary | P0 | CI workflow configured |
| [x] REL-002 | Prebuilt Linux aarch64 binary | P1 | CI workflow configured (musl) |
| [x] REL-003 | Prebuilt macOS x86_64 binary | P0 | CI workflow configured |
| [x] REL-004 | Prebuilt macOS aarch64 binary | P0 | CI workflow configured |
| [x] REL-005 | Prebuilt Windows x86_64 binary | P1 | CI workflow configured |
| [x] REL-006 | Source tarball | P1 | GitHub auto-generates |
| [x] REL-007 | Checksums (SHA256) | P0 | gh-release action handles |
| [ ] REL-008 | GPG signatures | P2 | |

### 15.2 Package Managers

| ID | Requirement | Priority | Status |
|----|-------------|----------|--------|
| [x] PKG-001 | crates.io publication | P0 | Ready for publish |
| [ ] PKG-002 | Homebrew formula | P1 | |
| [ ] PKG-003 | AUR package | P2 | |
| [ ] PKG-004 | Nix package | P2 | |
| [ ] PKG-005 | Scoop manifest (Windows) | P2 | |
| [ ] PKG-006 | Debian package | P3 | |
| [ ] PKG-007 | RPM package | P3 | |

### 15.3 Installation Methods

| ID | Requirement | Priority | Status |
|----|-------------|----------|--------|
| [x] INST-001 | `cargo install claude-snatch` | P0 | Ready for publish |
| [x] INST-002 | Direct binary download | P0 | Via GitHub releases |
| [ ] INST-003 | Install script (curl \| sh) | P1 | |
| [ ] INST-004 | Version management | P2 | |
| [ ] INST-005 | Self-update capability | P3 | |

### 15.4 CI/CD

| ID | Requirement | Priority | Status |
|----|-------------|----------|--------|
| [x] CI-001 | GitHub Actions workflow | P0 | ✓ |
| [x] CI-002 | Automated testing on PR | P0 | ✓ |
| [x] CI-003 | Cross-platform builds | P0 | ✓ |
| [x] CI-004 | Release automation | P1 | softprops/action-gh-release |
| [x] CI-005 | Changelog generation | P2 | generate_release_notes in workflow |
| [ ] CI-006 | Security scanning | P2 | |

---

## 16. Future Considerations

### 16.1 Potential Future Features

| ID | Feature | Priority | Notes |
|----|---------|----------|-------|
| [ ] FUT-001 | MCP server mode | P3 | Expose as MCP tool |
| [ ] FUT-002 | Real-time session monitoring | P2 | Watch active sessions |
| [ ] FUT-003 | Web UI | P3 | Browser-based interface |
| [ ] FUT-004 | Remote session support | P3 | SSH to fetch logs |
| [ ] FUT-005 | Cloud sync integration | P3 | S3, GCS, etc. |
| [ ] FUT-006 | Diff between sessions | P2 | Compare conversations |
| [ ] FUT-007 | Session merging | P3 | Combine sessions |
| [ ] FUT-008 | AI-powered summarization | P3 | Use Claude to summarize |
| [ ] FUT-009 | Conversation visualization | P3 | Graph-based view |
| [ ] FUT-010 | Plugin system | P3 | Third-party extensions |
| [ ] FUT-011 | Internationalization (i18n) | P3 | CLI/TUI interface localization |

**Note on FUT-011 (i18n):** Claude Code itself is currently English-only. JSONL content is already multilingual (UTF-8). CLI/TUI localization is a v2.0+ enhancement.

### 16.2 Non-Goals (Explicit Exclusions)

The following are explicitly NOT in scope:

- [ ] **NGO-001**: Modifying Claude Code session files
- [ ] **NGO-002**: Interacting with the Claude API
- [ ] **NGO-003**: Managing Claude Code installations
- [ ] **NGO-004**: Providing IDE integrations
- [ ] **NGO-005**: Implementing conversation continuation
- [ ] **NGO-006**: Storing or caching API credentials

### 16.3 Competitive Monitoring Process

To maintain competitive advantage in a rapidly evolving ecosystem, the following monitoring activities should be performed:

#### 16.3.1 Quarterly Assessment Schedule

| Quarter | Activities |
|---------|------------|
| Q1 (Jan-Mar) | Full competitive fidelity re-analysis, update Section 3.2 |
| Q2 (Apr-Jun) | Monitor Claude Code changelogs, assess new tools |
| Q3 (Jul-Sep) | Full competitive re-analysis, update success metrics |
| Q4 (Oct-Dec) | Monitor changelogs, prepare annual strategy review |

#### 16.3.2 Monitoring Checklist

**Monthly:**
- [ ] Review Claude Code changelog for schema changes
- [ ] Check GitHub stars/forks of top 5 competitors
- [ ] Note new Claude Code tools or message types

**Quarterly:**
- [ ] Re-calculate fidelity scores for top 5 competitors
- [ ] Update competitor version numbers in Section 3.2
- [ ] Identify new entrants with >100 stars
- [ ] Assess feature parity gaps

**Annually:**
- [ ] Comprehensive 49+ tool re-analysis
- [ ] Strategic positioning review
- [ ] Roadmap adjustment based on market trends

#### 16.3.3 Key Competitors to Track

1. **daaain/claude-code-log** (Python, TUI, HTML export)
2. **d-kimuson/claude-code-viewer** (Web-based, interactive)
3. **haasonsaas/claude-usage-tracker** (Usage analytics focus)
4. **ZeroSumQuant/claude-conversation-extractor** (Zero-dependency)
5. **Any new tool with >200 stars or unique capabilities**

---

## Appendix A: Message Type Reference

| Type | Description | Fields |
|------|-------------|--------|
| `assistant` | Claude's responses | message, requestId |
| `user` | Human input/tool results | message |
| `system` | Notifications, compaction | subtype, level, compactMetadata |
| `summary` | Context summaries | summary, leafUuid |
| `file-history-snapshot` | File backups | snapshot, messageId |
| `queue-operation` | Input buffering | operation, content |
| `turn_end` | Turn markers | agentId |

### Stop Reason Values

The `message.stop_reason` field indicates why generation stopped:

| Value | Meaning | Frequency |
|-------|---------|-----------|
| `tool_use` | Generation paused for tool invocation | ~94% |
| `end_turn` | Response completed naturally | ~6% |
| `max_tokens` | Output token limit reached (response may be truncated) | Rare |
| `stop_sequence` | Custom stop sequence triggered | Rare |
| `null` | During streaming or intermediate chunks | Common in streaming |

### Message Grouping

Related JSONL lines from a single API response share the same `message.id` identifier. This is crucial for understanding that multiple JSONL lines often represent a single logical response:

```
Line 1: {"message":{"id":"msg_01ABC...","content":[{"type":"thinking",...}]}}
Line 2: {"message":{"id":"msg_01ABC...","content":[{"type":"text",...}]}}
Line 3: {"message":{"id":"msg_01ABC...","content":[{"type":"tool_use",...}]}}
```

All three lines share `msg_01ABC...` and represent streaming chunks from one assistant turn. Typical operations span 2-3 log entries sharing a single message ID.

### System Subtypes Reference

| Subtype | Purpose | Key Fields |
|---------|---------|------------|
| `compact_boundary` | Context compaction marker | `compactMetadata` |
| `stop_hook_summary` | Hook execution results | `hookCount`, `hookInfos` |
| `api_error` | API failure with retry info | `error`, `retryAttempt`, `maxRetries` |
| `local_command` | CLI slash command | `content` (XML format) |

## Appendix B: Content Block Type Reference

| Type | Description | Fields |
|------|-------------|--------|
| `text` | Natural language | text |
| `tool_use` | Tool invocation | id, name, input |
| `tool_result` | Tool output | tool_use_id, content, is_error |
| `thinking` | Reasoning | thinking, signature |
| `image` | Visual input | source (base64/url/file) |

### Three-State Error Semantics

The `is_error` field in `tool_result` blocks has **three distinct states**:

| Value | Meaning | Interpretation |
|-------|---------|----------------|
| `true` | Explicit failure | Tool execution failed |
| `false` | Explicit success | Tool completed successfully |
| (absent) | Implicit success | Tool completed (legacy/default behavior) |

**Important:** When parsing, treat `is_error: false` and absent `is_error` equivalently as success, but note that `is_error: false` represents explicit confirmation while absent represents implicit success.

### Image Source Types

| Source Type | Description | When Used |
|-------------|-------------|-----------|
| `base64` | Inline base64-encoded data | Screenshots, Read tool images, pasted images |
| `url` | Public URL reference | Referenced web images |
| `file` | Files API file ID | Pre-uploaded files (beta) |

## Appendix C: Tool Reference

### Core Tools (25+)

File Operations: Read, Write, Edit, MultiEdit, LS, Glob, Grep, NotebookEdit, NotebookRead
Shell: Bash, KillShell
Web: WebSearch, WebFetch
Agent: Task, TaskOutput, TodoRead, TodoWrite
Interaction: AskUserQuestion, EnterPlanMode, ExitPlanMode, Skill
MCP: ListMcpResourcesTool, ReadMcpResourceTool
Code Intelligence: LSP (v2.0.74+)
Deprecated: AgentOutputTool, BashOutputTool (→ TaskOutput)

### LSP Tool Operations (v2.0.74+)

| Operation | Description |
|-----------|-------------|
| `goToDefinition` | Find symbol definition location |
| `findReferences` | Find all references to a symbol |
| `hover` | Get documentation and type info |
| `documentSymbol` | List all symbols in a document |
| `workspaceSymbol` | Search symbols across workspace |
| `goToImplementation` | Find interface/abstract implementations |
| `prepareCallHierarchy` | Get call hierarchy at position |
| `incomingCalls` | Find callers of function |
| `outgoingCalls` | Find functions called by function |

### MCP Tool Pattern

```
mcp__<server-name>__<method-name>
```

### Tool ID Patterns

Tool use IDs follow specific prefixes based on execution context:

| Prefix | Type | Description | Example |
|--------|------|-------------|---------|
| `toolu_` | Client tool | Standard tools executed locally | `toolu_013Xeg9XPXus1or3pHKNB6Lq` |
| `srvtoolu_` | Server tool | Tools executed on Anthropic's servers | `srvtoolu_018es16J4ZnSyvS3LSGnjFp9` |

**Server-side tools** (use `srvtoolu_` prefix):
- `web_search` — WebSearch tool (executed server-side)
- `web_fetch` — WebFetch tool (executed server-side)
- `code_execution` — Python code execution sandbox
- `tool_search_tool_regex` — Tool discovery search

### Additional ID Patterns

| Pattern | Type | Example |
|---------|------|---------|
| `msg_` | API message ID | `msg_01P2rK5331DYicNLtUz9Bs4G` |
| `req_` | API request ID | `req_011CWAwcWYSyTZ6UCmE38HE2` |
| `agent-` | Subagent ID | `agent-3e533ee` |
| `file_` | Files API ID | `file_abc123def456` |

---

## Appendix D: Priority Legend

| Priority | Meaning | Timeline |
|----------|---------|----------|
| P0 | Critical | MVP Required |
| P1 | Important | v1.0 Release |
| P2 | Nice-to-have | v1.x Release |
| P3 | Future | v2.0+ |

---

## Appendix E: Revision History

| Version | Date | Author | Changes |
|---------|------|--------|---------|
| 1.0.0 | 2025-12-19 | Initial | Initial specification |
| 1.1.0 | 2025-12-19 | Peer Review | Revised after peer review: Added schema versioning strategy (10.3), caching/indexing architecture (10.4), configuration system (10.5), burn rate analytics (STAT-013-016), GDPR compliance (12.3), path normalization (FS-006-007), new feature elements (4.10), extended beyond-JSONL elements (BJ-017-020), unknown field preservation (DATA-008-009), clarified lossless scope (OBJ-006), revised positioning to emphasize performance + reliability |
| 1.2.0 | 2025-12-23 | Multi-level Review | Added LSP tool elements (4.11), MVP scope definition (1.2), schema version detection methodology (10.3.1), concurrent session handling (PARSE-011/012, 5.1.1), exit codes (CLI-011, 7.1.1), configuration schema (Appendix F), logging configuration (CFG-008-010), resource limits (PERF-010-011, MEM-006), output styles (BJ-021), i18n future consideration (FUT-011), non-extractable features (Appendix G), competitive monitoring (16.3), migration guide (DOC-009), updated baseline to v2.0.74+ |
| 1.2.1 | 2025-12-23 | Reference Sync | Incorporated reference document: tool ID patterns (toolu_/srvtoolu_), server-side tools, stop reason values, three-state error semantics, message grouping documentation, system subtypes reference, image source types, expanded schema version history (11 versions), enhanced Appendix G with hook events (G.4), permission events (G.5), MCP lifecycle (G.6), SSE streaming (G.7) |

---

## Appendix F: Configuration Schema

The claude-snatch configuration file uses TOML format and is located at `~/.config/claude-snatch/config.toml` by default.

### F.1 Complete Configuration Example

```toml
# claude-snatch Configuration File
# Location: ~/.config/claude-snatch/config.toml
# All values shown are defaults unless otherwise noted

[general]
# Path to Claude Code data directory (auto-detected if not specified)
claude_dir = "~/.claude"

# Enable colored terminal output
color = true

# Default verbosity level (0=quiet, 1=normal, 2=verbose, 3=debug)
verbosity = 1

# Check for updates on startup (requires network)
check_updates = false

[export]
# Default export format: markdown, json, html, txt, xml, sqlite, csv
default_format = "markdown"

# Include thinking blocks in exports
include_thinking = true

# Include tool call details in exports
include_tools = true

# Include subagent conversations in exports
include_agents = true

# Enable lossless mode (preserve all original JSONL data)
lossless = false

# Default output directory for exports
output_dir = "."

# Overwrite existing files without prompting
overwrite = false

[export.markdown]
# Include table of contents in markdown exports
table_of_contents = true

# Use collapsible sections for long content
collapsible_sections = true

# Code block syntax highlighting language hints
syntax_highlighting = true

[export.json]
# Pretty-print JSON output
pretty_print = true

# Indent size for pretty-printed JSON
indent = 2

[performance]
# Maximum memory usage in MB (0 = unlimited)
max_memory_mb = 512

# Number of parallel workers for file processing (0 = auto-detect CPU cores)
parallel_workers = 0

# Maximum file size to process in MB (0 = unlimited)
max_file_size_mb = 10240  # 10GB

# Enable memory-mapped file I/O for large files
mmap_enabled = true

# Threshold in MB above which to use mmap
mmap_threshold_mb = 100

[cache]
# Enable session metadata caching
enabled = true

# Cache directory location
dir = "~/.cache/claude-snatch"

# Maximum cache size in MB
max_size_mb = 256

# Cache time-to-live in seconds (0 = no expiration)
ttl_seconds = 86400  # 24 hours

[search]
# Enable full-text search index
index_enabled = true

# Index storage location
index_dir = "~/.cache/claude-snatch/index"

# Rebuild index on startup if stale
auto_rebuild = true

# Maximum search results to return
max_results = 1000

[tui]
# Default theme: dark, light
theme = "dark"

# Enable syntax highlighting in conversation viewer
syntax_highlighting = true

# Enable mouse support
mouse_enabled = true

# Show timestamps in conversation view
show_timestamps = true

# Show token usage in conversation view
show_tokens = false

# Default panel layout: horizontal, vertical
layout = "horizontal"

# Vim-style keybindings (j/k/h/l navigation)
vim_keys = true

[logging]
# Log output destination: stderr, file, both, none
output = "stderr"

# Log file path (only used if output includes "file")
file = "~/.local/share/claude-snatch/claude-snatch.log"

# Log level: error, warn, info, debug, trace
level = "info"

# Use structured JSON logging format
json_format = false

# Include timestamps in log output
timestamps = true

# Maximum log file size in MB before rotation
max_file_size_mb = 10

# Number of rotated log files to keep
max_files = 5
```

### F.2 Environment Variable Overrides

All configuration options can be overridden via environment variables using the pattern:
`SNATCH_<SECTION>_<KEY>=value`

| Environment Variable | Config Equivalent |
|---------------------|-------------------|
| `SNATCH_GENERAL_CLAUDE_DIR` | `general.claude_dir` |
| `SNATCH_GENERAL_COLOR` | `general.color` |
| `SNATCH_EXPORT_DEFAULT_FORMAT` | `export.default_format` |
| `SNATCH_PERFORMANCE_MAX_MEMORY_MB` | `performance.max_memory_mb` |
| `SNATCH_TUI_THEME` | `tui.theme` |
| `SNATCH_LOGGING_LEVEL` | `logging.level` |

### F.3 Per-Project Configuration

Project-specific settings can be placed in `.claude-snatch.toml` in the project root. These override global settings.

```toml
# .claude-snatch.toml (project-level)
[export]
default_format = "json"
include_thinking = false

[tui]
theme = "light"
```

---

## Appendix G: Non-Extractable Features

The following Claude Code features do **not** appear in JSONL logs and cannot be extracted by claude-snatch:

### G.1 Client-Side UX Features

| Feature | Description | Reason Non-Extractable |
|---------|-------------|----------------------|
| Prompt Suggestions | Tab-to-accept suggestions | Generated client-side, not logged unless accepted |
| Keyboard Shortcuts | alt+p model switch, etc. | Client UI only |
| Terminal Styling | Colors, formatting | Ephemeral display |
| Auto-completions | Tab completion | Not logged |

### G.2 Ephemeral State

| Feature | Description | Reason Non-Extractable |
|---------|-------------|----------------------|
| Permissions State | Runtime allow/deny decisions | Not persisted to JSONL |
| Active Model Selection | Current model in use | Only logged per-message |
| Plan Mode State | Whether plan mode is active | Logged as tool calls only |
| Memory/Context Window | What Claude "remembers" | Implicit in conversation |

### G.3 External Integrations

| Feature | Description | Reason Non-Extractable |
|---------|-------------|----------------------|
| OAuth Tokens | Third-party auth | Security: not logged |
| MCP Server State | Server connection status | Runtime only |
| IDE Integration | VS Code/JetBrains state | External to Claude Code |

### G.4 Hook Execution Events

Claude Code supports 8 hook types, but only **one produces JSONL events**:

| Hook Event | Produces JSONL | Notes |
|------------|----------------|-------|
| `PreToolUse` | ❌ No | Runs before tool calls, can block them |
| `PostToolUse` | ❌ No | Runs after successful tool calls |
| `Notification` | ❌ No | Runs on notification dispatch |
| `Stop` | ✅ Yes (`stop_hook_summary`) | Runs when main agent finishes |
| `SubagentStop` | ❌ No | Runs when subagent finishes |
| `PreCompact` | ❌ No | Runs before compaction |
| `SessionStart` | ❌ No | Runs when session starts/resumes |
| `UserPromptSubmit` | ❌ No | Runs when user submits prompt |

Hooks run as external processes. Their output may be injected into the conversation as `user` messages with the hook's stdout, but the hook execution itself is not logged as a distinct event type.

### G.5 Permission/Approval Events

Tool permission decisions are **not logged as separate JSONL events**. The permission system operates at runtime through:

1. **Hooks** (PreToolUse can allow/deny)
2. **Permission rules** (settings.json allow/deny rules)
3. **SDK callbacks** (`canUseTool` callback)
4. **Permission modes** (default, acceptEdits, bypassPermissions)

The JSONL captures `tool_use` and `tool_result` events, but not the approval/denial decision flow. If a tool is denied, no `tool_result` appears—the `tool_use` simply has no corresponding result.

**Detection strategy:** Compare `tool_use` events to `tool_result` events. Missing results may indicate denied permissions or user cancellation.

### G.6 MCP Lifecycle Events

MCP server connection and disconnection are **not logged as specific events**. MCP interactions appear only as:

- **Tool invocations**: `mcp__<server>__<method>` tool_use events
- **Resource access**: `ListMcpResourcesTool`, `ReadMcpResourceTool` tool_use events
- **Errors**: Connection failures may appear in error messages within tool_result content

Normal MCP lifecycle (connect, disconnect, reconnect) is not captured in JSONL logs.

### G.7 Raw SSE Streaming Events

JSONL captures merged final content, not raw Server-Sent Events stream chunks:

| SSE Event | Captured in JSONL |
|-----------|-------------------|
| `message_start` | ❌ No |
| `content_block_start` | ❌ No |
| `content_block_delta` | ❌ No (merged into final) |
| `content_block_stop` | ❌ No |
| `message_delta` | ❌ No |
| `message_stop` | ❌ No |

To capture raw SSE data, use proxy interception tools (e.g., claude-code-logger, claude-code-proxy).

---

*End of Functional Requirements Specification*
