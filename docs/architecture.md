# Architecture Overview

This document describes the high-level architecture of snatch, a high-performance Rust CLI/TUI tool for extracting and analyzing Claude Code conversation logs.

## System Overview

```
┌─────────────────────────────────────────────────────────────────────────┐
│                              snatch                                      │
├─────────────────────────────────────────────────────────────────────────┤
│  CLI Layer (clap)                                                        │
│  ┌─────────┐ ┌─────────┐ ┌─────────┐ ┌─────────┐ ┌─────────┐           │
│  │  list   │ │ export  │ │ search  │ │  stats  │ │   tui   │           │
│  └────┬────┘ └────┬────┘ └────┬────┘ └────┬────┘ └────┬────┘           │
├───────┴──────────┴──────────┴──────────┴──────────┴────────────────────┤
│  TUI Layer (ratatui)                                                     │
│  ┌─────────────┐ ┌─────────────┐ ┌─────────────┐                        │
│  │  Tree View  │ │ Conversation│ │  Details    │                        │
│  │   Panel     │ │   Panel     │ │   Panel     │                        │
│  └─────────────┘ └─────────────┘ └─────────────┘                        │
├─────────────────────────────────────────────────────────────────────────┤
│  Core Library                                                            │
│  ┌──────────┐ ┌──────────┐ ┌──────────┐ ┌──────────┐ ┌──────────┐      │
│  │Discovery │ │  Parser  │ │  Model   │ │Reconstruct│ │  Export  │      │
│  └────┬─────┘ └────┬─────┘ └────┬─────┘ └────┬─────┘ └────┬─────┘      │
│       │            │            │            │            │              │
│  ┌────┴────────────┴────────────┴────────────┴────────────┴────┐       │
│  │                        Analytics                              │       │
│  └───────────────────────────────────────────────────────────────┘       │
├─────────────────────────────────────────────────────────────────────────┤
│  Infrastructure                                                          │
│  ┌──────────┐ ┌──────────┐ ┌──────────┐ ┌──────────┐                    │
│  │  Cache   │ │  Config  │ │  Error   │ │Extraction│                    │
│  └──────────┘ └──────────┘ └──────────┘ └──────────┘                    │
└─────────────────────────────────────────────────────────────────────────┘
                                    │
                                    ▼
┌─────────────────────────────────────────────────────────────────────────┐
│  Claude Code Data                                                        │
│  ~/.claude/                                                              │
│  ├── projects/                   # Encoded project paths                 │
│  │   └── <encoded-path>/         # Per-project directory                 │
│  │       └── <session-id>.jsonl  # Conversation logs                     │
│  ├── filehistory/                # File backup system                    │
│  ├── settings.json               # User settings                         │
│  ├── mcp.json                    # MCP server config                     │
│  └── commands/                   # Custom slash commands                 │
└─────────────────────────────────────────────────────────────────────────┘
```

## Module Structure

### `src/cli/` - Command Line Interface

Handles command-line argument parsing and subcommand dispatch using clap.

```
cli/
├── mod.rs          # CLI structure and argument definitions
├── commands/       # Subcommand implementations
│   ├── export.rs   # Export conversations
│   ├── list.rs     # List sessions/projects
│   ├── search.rs   # Search functionality
│   ├── stats.rs    # Analytics and statistics
│   └── diff.rs     # Session comparison
└── output.rs       # Output formatting helpers
```

### `src/tui/` - Terminal User Interface

Interactive terminal interface built with ratatui and crossterm.

```
tui/
├── mod.rs          # TUI entry point
├── app.rs          # Main event loop
├── state.rs        # Application state
├── components.rs   # Reusable UI components
├── events.rs       # Event handling
├── highlight.rs    # Syntax highlighting
└── theme.rs        # Color themes
```

### `src/model/` - Data Model

Strongly-typed Rust structures for Claude Code JSONL format.

```
model/
├── mod.rs          # Module exports
├── message.rs      # Core message types (User, Assistant, System, etc.)
├── content.rs      # Content block types (Text, Thinking, ToolUse, etc.)
├── metadata.rs     # Session metadata structures
├── tools.rs        # Tool definitions and results
└── usage.rs        # Token usage tracking
```

Key types:
- `LogEntry`: Enum for all message types
- `ContentBlock`: Enum for content variants
- `ToolUse`, `ToolResult`: Tool interaction types

### `src/parser/` - JSONL Parser

Streaming parser for Claude Code JSONL files.

```
parser/
├── mod.rs          # Parser interface
└── streaming.rs    # Streaming iterator implementation
```

Features:
- Line-by-line parsing for memory efficiency
- Lenient parsing with error recovery
- Schema version detection
- Progress tracking

### `src/reconstruction/` - Conversation Reconstruction

Reconstructs conversation trees from flat JSONL entries.

```
reconstruction/
├── mod.rs          # Reconstruction interface
├── tree.rs         # Conversation tree structure
└── thread.rs       # Thread extraction and branching
```

Features:
- UUID-based parent/child linking
- Branch detection and handling
- Main thread extraction
- Retry detection

### `src/export/` - Export Formats

Multiple export format implementations.

```
export/
├── mod.rs          # Exporter trait and options
├── markdown.rs     # Markdown exporter
├── json.rs         # JSON/JSONL exporter
├── html.rs         # HTML exporter
├── text.rs         # Plain text exporter
├── csv.rs          # CSV exporter
├── xml.rs          # XML exporter
└── sqlite.rs       # SQLite exporter
```

All exporters implement the `Exporter` trait:

```rust
pub trait Exporter: Send + Sync {
    fn export_conversation<W: Write>(
        &self,
        conversation: &Conversation,
        writer: &mut W,
        options: &ExportOptions,
    ) -> Result<()>;
}
```

### `src/discovery/` - File Discovery

Locates and organizes Claude Code data.

```
discovery/
├── mod.rs          # Discovery interface
├── paths.rs        # Path encoding/decoding
├── project.rs      # Project abstraction
├── session.rs      # Session abstraction
└── hierarchy.rs    # Agent hierarchy building
```

Features:
- Cross-platform path handling (Linux, macOS, WSL)
- Project path encoding (% and - encoding)
- Session discovery and metadata
- Agent/subagent hierarchy detection

### `src/extraction/` - Data Extraction

Extracts specific data types from conversations.

```
extraction/
├── mod.rs          # Extraction interface
├── backup.rs       # File backup handling
├── commands.rs     # Custom command discovery
├── mcp.rs          # MCP configuration
├── rules.rs        # Project rules extraction
└── settings.rs     # Settings file parsing
```

### `src/analytics/` - Analytics Engine

Computes statistics and insights.

```
analytics/
├── mod.rs          # Analytics interface
└── session.rs      # Session analytics
```

Metrics:
- Message counts by type
- Token usage (input/output/cache)
- Cost estimation
- Tool invocation statistics
- Thinking block analysis
- Duration calculation

### `src/cache/` - Caching System

LRU cache with mtime-based invalidation.

```
cache/
└── mod.rs          # Cache implementation
```

Features:
- In-memory LRU cache
- File modification time tracking
- Configurable size limits
- Automatic invalidation

### `src/config/` - Configuration

Layered configuration system.

```
config/
└── mod.rs          # Config loading and merging
```

### `src/error.rs` - Error Handling

Unified error types using thiserror.

## Data Flow

### Parsing Flow

```
JSONL File → JsonlParser → LogEntry[] → Conversation → Export
                 │                           │
                 │                           │
                 ▼                           ▼
           ParseStats              ConversationTree
```

### TUI Flow

```
User Input → EventHandler → AppState → UI Render
                               │
                               ├─→ ClaudeDirectory (discovery)
                               ├─→ JsonlParser (parsing)
                               ├─→ Conversation (reconstruction)
                               └─→ SessionAnalytics (analysis)
```

## Design Principles

### 1. Zero-Copy Where Possible

The parser minimizes memory allocations:
- Streaming line-by-line parsing
- Reference-based content access
- Lazy conversion where applicable

### 2. Graceful Degradation

Unknown or malformed data is handled gracefully:
- `#[serde(other)]` for unknown enum variants
- `#[serde(flatten)]` for unknown fields
- Lenient parsing mode for recovery

### 3. Type Safety

Strong typing throughout:
- Enum variants for message types
- Newtype wrappers where appropriate
- Builder patterns for complex construction

### 4. Separation of Concerns

Clear module boundaries:
- Parsing is independent of display
- Model is independent of storage
- Export is pluggable

### 5. Performance

Optimized for large files:
- Streaming parser
- LRU caching
- Efficient data structures
- Parallel processing where applicable

## Extension Points

### Adding a New Export Format

1. Create `src/export/myformat.rs`
2. Implement `Exporter` trait
3. Add to `ExportFormat` enum
4. Register in CLI

### Adding a New Message Type

1. Add variant to `LogEntry` enum
2. Add serde rename if needed
3. Update display/export logic
4. Add tests

### Adding a New TUI Feature

1. Update `AppState` in `state.rs`
2. Add key binding in `app.rs`
3. Update `draw_ui()` if UI changes
4. Update help overlay

## Testing Strategy

```
tests/
├── integration_tests.rs    # Full pipeline tests
└── fixtures/               # Test data
    └── sessions/           # Sample JSONL files
```

- Unit tests: Per-module, testing individual functions
- Integration tests: End-to-end pipeline testing
- Property tests: Parser robustness (future)
