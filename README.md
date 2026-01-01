# claude-snatch

[![Rust](https://img.shields.io/badge/rust-1.75%2B-orange.svg)](https://www.rust-lang.org)
[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)

High-performance CLI/TUI tool for extracting, analyzing, and exporting Claude Code conversation logs with **maximum data fidelity**.

## Features

- **Maximum Fidelity**: Extract all 77+ documented JSONL data elements
- **Multiple Export Formats**: Markdown, JSON, HTML, CSV, XML, SQLite, OpenTelemetry, and more
- **Rust Performance**: Native speed, 10-100x faster than Python/Node alternatives
- **Lossless Round-Trip**: Preserve unknown fields for forward compatibility
- **Dual Interface**: CLI (scriptable) and TUI (interactive) modes
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

# Build in release mode
cargo build --release

# Install to ~/.cargo/bin
cargo install --path .
```

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
snatch export <session-id> -o conversation.md

# Export to JSON with pretty printing
snatch export <session-id> -f json --pretty -o conversation.json

# Export to HTML (with dark theme)
snatch export <session-id> -f html -o conversation.html

# Search across all sessions
snatch search "pattern" -i  # case-insensitive

# Show session statistics
snatch stats -s <session-id>

# Show global statistics across all sessions
snatch stats --global

# Launch interactive TUI
snatch tui
```

## Commands

| Command | Alias | Description |
|---------|-------|-------------|
| `list` | `ls` | List projects and sessions |
| `export` | `x` | Export conversations to various formats |
| `search` | `s`, `find` | Search across sessions |
| `stats` | `stat` | Show usage statistics |
| `standup` | `daily` | Generate standup/progress report |
| `info` | `i`, `show` | Display detailed information |
| `pick` | `browse` | Interactively pick a session using fuzzy search |
| `tui` | `ui` | Launch interactive terminal UI |
| `diff` | | Compare two sessions or files |
| `tag` | | Manage session tags, names, and bookmarks |
| `prompts` | | Extract user prompts from sessions |
| `extract` | | Extract beyond-JSONL data (settings, MCP, etc.) |
| `index` | | Manage full-text search index |
| `cache` | | Manage the session cache |
| `cleanup` | | Clean up old or empty sessions |
| `config` | | View and modify configuration |
| `validate` | | Validate session files |
| `watch` | | Watch for session changes |
| `completions` | | Generate shell completions |
| `quickstart` | | Interactive guide for new users |

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
snatch export <session-id> -f markdown -o output.md
```

### JSON

Lossless structured data export, preserving all JSONL elements.

```bash
snatch export <session-id> -f json -o output.json
snatch export <session-id> -f json-pretty -o output.json  # Pretty-printed
```

### HTML

Rich formatted output with dark/light theme support and collapsible sections.

```bash
snatch export <session-id> -f html -o output.html
```

### Plain Text

Simple unformatted text output.

```bash
snatch export <session-id> -f text -o output.txt
```

### CSV

Tabular format for spreadsheet analysis.

```bash
snatch export <session-id> -f csv -o output.csv
```

### SQLite

Queryable database with full-text search support.

```bash
snatch export <session-id> -f sqlite -o output.db
snatch export --all -f sqlite -o archive.db  # Multi-session archive
```

### XML

Structured markup for data interchange.

```bash
snatch export <session-id> -f xml -o output.xml
```

### JSONL

Original format preservation for backup or re-import.

```bash
snatch export <session-id> -f jsonl -o output.jsonl
```

### OpenTelemetry

OTLP JSON format for observability pipelines.

```bash
snatch export <session-id> -f otel -o traces.json
```

## Export Options

| Option | Default | Description |
|--------|---------|-------------|
| `--thinking` | true | Include thinking blocks |
| `--tool-use` | true | Include tool use blocks |
| `--tool-results` | true | Include tool results |
| `--system` | false | Include system messages |
| `--timestamps` | true | Include timestamps |
| `--usage` | true | Include usage statistics |
| `--metadata` | false | Include metadata (UUIDs, etc.) |
| `--main-thread` | true | Only export main thread (exclude branches) |
| `--pretty` | false | Pretty-print JSON output |
| `--gist` | false | Upload export to GitHub Gist (requires `gh` CLI) |
| `--gist-public` | false | Make the gist public (default is secret) |
| `--gist-description` | - | Description for the gist |
| `--toc` | false | Include table of contents/navigation sidebar (HTML only) |
| `--dark` | false | Use dark theme (HTML only) |
| `--clipboard` | false | Copy export to clipboard instead of writing to file/stdout |
| `--redact` | - | Redact sensitive data (`security`, `all`) |
| `--redact-preview` | false | Preview what would be redacted without removing |

## Stats Options

| Option | Default | Description |
|--------|---------|-------------|
| `--session`, `-s` | - | Show stats for specific session |
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
snatch stats -s <session-id> --tools
```

## TUI Navigation

| Key | Action |
|-----|--------|
| `j` / `↓` | Move down |
| `k` / `↑` | Move up |
| `Enter` | Select / Expand |
| `Tab` | Switch panel |
| `/` | Search |
| `e` | Export current |
| `q` | Quit |
| `?` | Show help |

## Architecture

```
claude-snatch/
├── src/
│   ├── analytics/     # Statistics and usage tracking
│   ├── cli/           # Command-line interface
│   ├── config/        # Configuration management
│   ├── discovery/     # Session and project discovery
│   ├── error.rs       # Error types and handling
│   ├── export/        # Export formats (Markdown, JSON, HTML, Text)
│   ├── lib.rs         # Library root
│   ├── main.rs        # CLI entry point
│   ├── model/         # Data structures for all message types
│   ├── parser/        # JSONL parsing with streaming support
│   ├── reconstruction/# Conversation tree building
│   └── tui/           # Terminal user interface
├── tests/
│   ├── fixtures/      # Sample JSONL test files
│   └── integration_tests.rs
└── Cargo.toml
```

## Data Model

claude-snatch supports all 7 message types in Claude Code JSONL logs:

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

- Rust 1.75.0 or later
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

# Terminal image preview (sixel/kitty/iterm2/halfblocks)
cargo build --features image-preview

# Memory-mapped file parsing for very large JSONL files
cargo build --features mmap

# Enable all optional features
cargo build --features "mcp,image-preview,mmap"
```

| Feature | Description |
|---------|-------------|
| `mcp` | MCP server mode exposing claude-snatch as tools for AI models |
| `image-preview` | Terminal image rendering using sixel, kitty, or iterm2 protocols |
| `mmap` | Memory-mapped file parsing for very large JSONL files |

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

claude-snatch looks for configuration in:

1. `$XDG_CONFIG_HOME/claude-snatch/config.toml`
2. `~/.config/claude-snatch/config.toml`
3. `~/.claude-snatch.toml`

Example configuration:

```toml
[defaults]
claude_dir = "~/.claude"
output_format = "text"
color = true

[export]
include_thinking = true
include_tool_use = true
include_timestamps = true

[tui]
theme = "dark"
```

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
