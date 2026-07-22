# claude-snatch

[![Rust](https://img.shields.io/badge/rust-1.95%2B-orange.svg)](https://www.rust-lang.org)
[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)

High-performance CLI and MCP tool for retrieving, analyzing, and exporting
Claude Code and OpenAI Codex CLI session logs with **maximum data fidelity**.

## Features

- **Maximum Fidelity**: Preserve every native record with explicit provenance,
  including records not yet normalized
- **Claude Code + Codex CLI**: Provider-qualified sessions, normalized views,
  native/archive export, and cross-provider project history
- **Multiple Export Formats**: Markdown, JSON, HTML, CSV, SQLite, JSONL, and more
- **Rust Performance**: Native speed, 10-100x faster than Python/Node alternatives
- **Lossless Round-Trip**: Preserve unknown fields for forward compatibility
- **Cross-Platform**: Linux, macOS, Windows (including WSL)
- **Conversation Reconstruction**: Tree building with parent-child linking and branch detection
- **Session Analytics**: Token usage, cost estimation, tool statistics

## Quick Start

### Installation

#### From Source (Recommended)

```bash
# Clone the repository
git clone https://github.com/jkindrix/claude-snatch.git
cd claude-snatch

# Install this checkout to ~/.cargo/bin with Codex + MCP support.
# Re-running ./install.sh replaces an installed build of the same version.
./install.sh
```

For a remote install, use:

```bash
curl -fsSL https://raw.githubusercontent.com/jkindrix/claude-snatch/main/install.sh | bash
```

The piped installer uses a published binary when available and otherwise
builds the current `main` branch with Cargo. After updating an MCP-enabled
installation, restart or reconnect the MCP client so its stdio subprocess uses
the replacement binary.

#### Shell Completions

Generate shell completions for your preferred shell:

```bash
# Bash
snatch completions bash > ~/.local/share/bash-completion/completions/snatch

# Zsh
snatch completions zsh > ~/.zfunc/_snatch

# Fish
snatch completions fish > ~/.config/fish/completions/snatch.fish
```

### Basic Usage

```bash
# List all projects
snatch list projects

# List sessions (most recent first)
snatch list sessions

# List sessions for a specific project
snatch list sessions -p /path/to/project

# Export a session to Markdown
snatch export <session-id> -O conversation.md

# Export to JSON with pretty printing
snatch export <session-id> -f json --pretty -O conversation.json

# Export to HTML with the dark theme
snatch export <session-id> -f html --dark -O conversation.html

# Search across all sessions
snatch search "pattern" -i  # case-insensitive

# Show session statistics
snatch stats <session-id>

# Show global statistics across all sessions
snatch stats --global
```

### OpenAI Codex CLI and provider selection

Codex support is built by default. `snatch` discovers `$CODEX_HOME` (or
`~/.codex`) alongside Claude Code's store. Existing flagless commands remain
Claude-only for compatibility; select Codex or a union explicitly:

```bash
# Inspect provider availability and format diagnostics
snatch providers

# Discover Codex sessions and use a qualified id
snatch list sessions --provider codex
snatch info codex:<thread-id>
snatch messages codex:<thread-id> --detail full
snatch timeline codex:<thread-id>
snatch chunks codex:<thread-id>

# Normalized and source-fidelity exports
snatch export codex:<thread-id> -f markdown -O conversation.md
snatch export codex:<thread-id> -f raw-jsonl -O rollout.jsonl
snatch export codex:<thread-id> -f archive -O session.archive

# Cross-provider project and lesson views
snatch list projects --provider all
snatch list sessions --provider all --project /path/to/project
snatch lessons --all --provider all
```

Codex `.jsonl` and `.jsonl.zst` rollouts are supported, including archived
twins, forks, subagents, compaction windows, steering prompts, usage
observations, and drift diagnostics. Pre-envelope Codex files (CLI ≤0.31.0)
are inventoried and source-exportable but intentionally refused for normalized
analysis until provenance-backed fixtures justify a legacy parser. Codex cost
is reported as unavailable—not `$0`—because ChatGPT-plan sessions cannot be
honestly priced from token counts as API spend.

## Commands

| Command | Alias | Description |
|---------|-------|-------------|
| `list` | `ls` | List projects and sessions |
| `recent` | | List recent sessions |
| `info` | `i`, `show` | Show session or project details |
| `pick` | `browse` | Interactively select a session |
| `chain` | | Show continuation chains or typed provider lineage |
| `file-history` | | Find sessions that modified a file |
| `search` | `s`, `find` | Search session content |
| `thread` | | Thread a topic across sessions |
| `stats` | `stat` | Show usage statistics and cost tracking |
| `summary` | | Show a quick usage summary |
| `standup` | `daily` | Generate an activity report |
| `diff` | `d` | Compare sessions or conversation versions |
| `lessons` | | Extract error→fix pairs and human corrections |
| `health` | | Show a project health dashboard |
| `file-evolution` | | Explain how and why a file changed |
| `priorities` | | Suggest next work from project evidence |
| `doctor` | | Diagnose schema drift and degraded coverage |
| `providers` | | Report provider roots, capabilities, and availability |
| `context` | | Zoom around a session event |
| `timeline` | | Show a turn-by-turn narrative |
| `messages` | `msgs` | Read messages at selectable detail levels |
| `chunks` | | List prompt-boundary chunks |
| `goals` | | Manage the Claude project-memory goal registry |
| `digest` | | Generate a compact session digest |
| `notes` | | Manage the Claude project-memory note registry |
| `decisions` | | Manage the Claude project-memory decision registry |
| `export` | `x` | Export normalized or source-fidelity data |
| `grab` | | Bundle a Claude parent session and its subagents |
| `code` | | Extract code blocks |
| `prompts` | | Extract user prompts |
| `recover` | `restore` | Reconstruct files from Write/Edit operations |
| `watch` | | Watch active Claude sessions |
| `tag` | | Manage qualified session metadata |
| `cleanup` | `clean` | Clean old or empty Claude sessions |
| `validate` | | Validate source and normalized integrity |
| `cache` | | Manage the session cache |
| `index` | `idx` | Manage the provider-partitioned search index |
| `config` | `cfg` | View and modify configuration |
| `extract` | `ext` | Extract Claude-specific supplementary data |
| `completions` | | Generate shell completions |
| `quickstart` | `guide`, `examples` | Show built-in usage guidance |
| `serve-mcp` | `mcp` | Start the MCP server (when built with `mcp`) |

## MCP Server

When built with the `mcp` feature, claude-snatch runs as an MCP server over stdio, exposing session data and analysis as tools that AI agents can call directly.

### Setup

```bash
# Install the checkout with MCP support (and replace the same package version)
cargo install --path . --locked --all-features --force

# Start the server (stdio transport)
snatch serve-mcp
```

Configure it in your Claude Code MCP settings:

```json
{
  "mcpServers": {
    "snatch": {
      "command": "snatch",
      "args": ["serve-mcp"]
    }
  }
}
```

### Tools

| Tool | Description |
|------|-------------|
| `list_sessions` | List sessions with optional project and time filters |
| `get_session_info` | Metadata, duration, and summary for a session |
| `get_session_messages` | Full message content with optional thinking and tool blocks |
| `get_session_timeline` | Turn-by-turn timeline with timing and tool activity |
| `get_session_digest` | Concise summary of session activity and key moments |
| `get_session_lessons` | Error→fix pairs and user corrections for retrospective learning |
| `thread_topic` | Chronological cross-session topic thread with content provenance |
| `get_stats` | Token usage and cost statistics |
| `get_project_history` | Cross-session activity history for a project |
| `search_sessions` | Full-text search across all sessions |
| `get_tool_calls` | Tool call history with input summaries and error detection |
| `get_file_history` | Source-backed file modification history |
| `get_project_health` | Hotspots, rework, and failure trends |
| `get_event_context` | Context around a message or timestamp |
| `suggest_priorities` | Evidence-backed next-work suggestions |
| `explain_file_evolution` | Conversation context for a file's changes |
| `manage_goals` | Manage the Claude Code project-memory goal registry |
| `manage_notes` | Manage the Claude Code project-memory note registry |
| `manage_decisions` | Manage the Claude Code project-memory decision registry |

Provider-aware MCP tools accept an optional `provider` selection and return
qualified session ids. `get_project_history` can union Claude Code and Codex by
cwd/git identity while excluding fork-copied history from new-activity totals.
The three persistent registries above remain explicitly Claude-storage-scoped;
they reject `codex`/`all` rather than pretending their data is unified.

## Analysis and Recall

The `src/analysis/` module powers session intelligence features across both CLI and MCP interfaces:

- **Session digests** — concise summaries of activity, tools used, and key moments (`digest`)
- **Lesson extraction** — identifies error→fix pairs and user corrections from session history (`lessons`)
- **Timeline construction** — turn-by-turn view of conversation flow with timing (`timeline`)
- **Full-text search** — searches content, thinking blocks, and tool inputs (`search` / `s`)

All four capabilities are also available to AI agents as MCP tools (`get_session_digest`, `get_session_lessons`, `get_session_timeline`, `search_sessions`).

## Claude Code Skills

The `skills/` directory ships two Claude Code skills built on snatch's
prompt-boundary chunk retrieval:

- **session-audit** — walk a past session chunk-by-chunk: map where time, tool
  calls, and errors concentrated, read the narrative, verify claims against
  the commands that actually ran.
- **session-debrief** — extract durable, non-derivable knowledge (corrected
  assumptions, rejected alternatives, dead ends, standing instructions) and
  file it into its strongest enforceable home.

Install by symlinking into your user-level skills directory (a symlink keeps
them current with `git pull`):

```bash
ln -s "$(pwd)/skills/session-audit" ~/.claude/skills/session-audit
ln -s "$(pwd)/skills/session-debrief" ~/.claude/skills/session-debrief
```

Both require the `snatch` binary on PATH. Trigger them in any project with
phrases like "audit that session" or "debrief this session".

## Global Options

| Option | Short | Description |
|--------|-------|-------------|
| `--claude-dir` | `-d` | Path to Claude directory (default: `~/.claude`) |
| `--output` | `-o` | Output format: `text`, `json`, `tsv`, `compact` |
| `--verbose` | `-v` | Enable verbose output |
| `--quiet` | `-q` | Suppress non-essential output |
| `--json` | | Output as JSON (shorthand for `-o json`) |
| `--color` | | Enable/disable colored output |

## Export Formats

### Markdown (default)

Human-readable conversation transcript with syntax highlighting for code blocks.

```bash
snatch export <session-id> -f markdown -O output.md
```

### JSON

Normalized structured export that retains all JSONL elements (content-preserving,
not byte-exact — fields may be reordered; use `raw-jsonl` for a byte-faithful archive).

```bash
snatch export <session-id> -f json -O output.json
snatch export <session-id> -f json-pretty -O output.json  # Pretty-printed
```

### HTML

Rich formatted output with dark/light theme support and collapsible sections.

```bash
snatch export <session-id> -f html -O output.html
```

### Plain Text

Simple unformatted text output.

```bash
snatch export <session-id> -f text -O output.txt
```

### CSV

Tabular format for spreadsheet analysis.

```bash
snatch export <session-id> -f csv -O output.csv
```

### SQLite

Queryable database with full-text search support.

```bash
snatch export <session-id> -f sqlite -O output.db
snatch export --all -f sqlite -O archive.db  # Multi-session archive
```

### JSONL and source-fidelity tiers

`jsonl` is a normalized, content-preserving representation. It is not the
original byte stream: fields can be reordered and provider-routed output adds
versioned provenance wrappers.

```bash
snatch export <session-id> -f jsonl -O output.jsonl
snatch export <qualified-id> -f raw-jsonl -O source.jsonl
snatch export <qualified-id> -f native -O preferred-artifact.bin
snatch export <qualified-id> -f archive -O session.bundle
```

`raw-jsonl` streams the original logical JSONL record stream. `native` streams
the exact bytes of the preferred artifact (which may be compressed). `archive`
is the universal lossless tier and includes every discovered artifact plus a
manifest. Source-fidelity exports bypass content filters and redaction; use a
normalized format when transforming or sanitizing content.

## Export Options

| Option | Default | Description |
|--------|---------|-------------|
| `--thinking` | true | Include thinking blocks |
| `--tool-use` | true | Include tool use blocks |
| `--tool-results` | true | Include tool results |
| `--system` | false | Include system messages |
| `--timestamps` | true | Include timestamps (`--no-timestamps` disables them) |
| `--usage` | true | Include usage statistics (`--no-usage` disables them) |
| `--metadata` | false | Include metadata (UUIDs, etc.) |
| `--main-thread` | false | Only export main thread (exclude branches) |
| `--pretty` | false | Pretty-print JSON output |
| `--gist` | false | Upload export to GitHub Gist (requires `gh` CLI) |
| `--gist-public` | false | Make the gist public (default is secret) |
| `--gist-description` | - | Description for the gist |
| `--toc` | false | Include table of contents/navigation sidebar (HTML only) |
| `--dark` | false | Use dark theme (HTML only) |
| `--images` | true | Include image blocks (`--no-images` disables them) |
| `--clipboard` | false | Copy export to clipboard instead of writing to file/stdout |
| `--redact` | - | Redact sensitive data (`security`, `all`) |
| `--redact-preview` | false | Preview what would be redacted without removing |

## Stats Options

| Option | Default | Description |
|--------|---------|-------------|
| positional `SESSION` | - | Show stats for a specific session |
| `--project`, `-p` | - | Show stats for specific project |
| `--global` | false | Show global stats across all sessions |
| `--blocks` | false | Show 5-hour billing window breakdown |
| `--sparkline` | false | Show sparkline visualizations (▁▂▃▄▅▆▇█) |
| `--tools` | false | Show tool usage breakdown |
| `--models` | false | Show model usage breakdown |
| `--costs` | false | Show cost breakdown by model |
| `--all` | false | Show all available statistics |

### Examples

```bash
# Show billing blocks with sparkline trends
snatch stats --blocks --sparkline

# Show global stats with all breakdowns
snatch stats --global --all

# Show session stats with tool usage
snatch stats <session-id> --tools
```

## Architecture

```
claude-snatch/
├── src/
│   ├── analysis/      # Session analysis (digest, lessons, timeline, search)
│   ├── analytics/     # Statistics and usage tracking
│   ├── async_io/      # Async I/O helpers
│   ├── cache/         # Session cache
│   ├── cli/           # Command-line interface
│   ├── config/        # Configuration management
│   ├── decisions/     # Architectural decision tracking
│   ├── discovery/     # Session and project discovery
│   ├── export/        # Export formats (Markdown, JSON, HTML, CSV, SQLite, JSONL)
│   ├── extraction/    # Beyond-JSONL extraction (settings, MCP configs, commands)
│   ├── git/           # Git integration
│   ├── goals/         # Goal management
│   ├── index/         # Full-text search index
│   ├── mcp_server/    # MCP server (19 tools for agent integration)
│   ├── model/         # Data structures for all message types
│   ├── notes/         # Note management
│   ├── parser/        # JSONL parsing with streaming support
│   ├── provider/      # Provider registry, adapters, provenance, lineage
│   ├── reconstruction/# Conversation tree building
│   ├── util/          # Utility functions
│   ├── api.rs         # API types
│   ├── error.rs       # Error types and handling
│   ├── lib.rs         # Library root
│   ├── main.rs        # CLI entry point
│   └── tags.rs        # Session tags
├── tests/
│   ├── fixtures/      # Sample JSONL test files
│   └── integration_tests.rs
└── Cargo.toml
```

## Data Model

The normalized model supports Claude Code's seven established entry families
and preserves provider-native records that do not yet have a normalized form:

| Type | Description |
|------|-------------|
| `user` | User messages with text or tool results |
| `assistant` | Assistant responses with content blocks |
| `result` | API response metadata and usage |
| `system` | System prompts and context |
| `summary` | Conversation summaries |
| `snapshot` | File backup events |
| `queue-operation` | Input buffer operations |

### Content Blocks

- Text content
- Thinking blocks (extended thinking)
- Tool use (all built-in tools + MCP)
- Tool results (with three-state: success, error, ignored)
- Images (base64 or URL references)
- Server tool use (MCP tools)

## Building from Source

### Requirements

- Rust 1.95.0 or later
- Cargo

### Development Build

```bash
cargo build
```

### Release Build

```bash
cargo build --release
```

### Optional Features

Enable additional functionality with feature flags:

```bash
# MCP server mode for AI model integration
cargo build --features mcp

# Memory-mapped file parsing for very large JSONL files
cargo build --features mmap

# Enable all optional features
cargo build --features "mcp,mmap,tracing"
```

| Feature | Description |
|---------|-------------|
| `mcp` | MCP server exposing tools for session recall, search, lesson extraction, and goal and decision management |
| `mmap` | Memory-mapped file parsing for very large JSONL files |
| `tracing` | Enable tracing/diagnostic instrumentation |

### Running Tests

```bash
# Run all tests
cargo test

# Run with output
cargo test -- --nocapture

# Run specific test
cargo test test_parse_simple_session
```

### Linting

```bash
# Run Clippy
cargo clippy -- -W clippy::all -W clippy::pedantic

# Format code
cargo fmt
```

## Configuration

claude-snatch works out of the box. To customize, create a TOML config file:

- **User config:** `~/.config/claude-snatch/config.toml` (platform config dir; run
  `snatch config path` to print the exact location)
- **Project config:** `.claude-snatch.toml` in a project directory, which overrides
  the user config for sessions in that project

Example configuration:

```toml
[theme]
color = true

[display]
truncate_at = 10000
context_lines = 2

[cache]
enabled = true
ttl_seconds = 3600

[budget]
monthly_limit = 100.00   # USD; warns at 80% of the limit by default
```

Manage values with `snatch config show`, `snatch config get <key>`, and
`snatch config set <key> <value>`. See
[docs/configuration.md](docs/configuration.md) for the full reference.

## Performance

### Parsing Performance

| File Size | Target | Typical |
|-----------|--------|---------|
| 1 MB | <50ms | ~30ms |
| 10 MB | <500ms | ~200ms |
| 100 MB | <5s | ~2s |

Memory usage is typically <2x file size.

### Benchmark Results

Benchmarks run on `cargo bench`:

| Operation | 10 entries | 100 entries | 1000 entries | 10000 entries |
|-----------|------------|-------------|--------------|---------------|
| Parse (parse_str) | 0.1 ms | 1.1 ms | 11 ms | ~295 MiB/s |
| Tree reconstruction | 3.0 µs | 46 µs | 318 µs | - |

| Export Format | Time (100 messages) |
|---------------|---------------------|
| Markdown | 2.3 µs |
| Plain Text | 2.4 µs |
| JSON | 5.1 µs |

Run benchmarks locally:

```bash
cargo bench --bench parser_bench
```

## License

MIT License - see [LICENSE](LICENSE) for details.

## Contributing

Contributions are welcome! Please ensure:

1. Code passes `cargo clippy` with no warnings
2. Code is formatted with `cargo fmt`
3. All tests pass with `cargo test`
4. New features include appropriate tests

## Related Projects

- [Claude Code](https://github.com/anthropics/claude-code) - The Anthropic CLI that generates these logs
- [Codex CLI](https://github.com/openai/codex) - The coding-agent CLI whose rollout logs are supported
