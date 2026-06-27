# Export Formats

snatch supports multiple export formats for conversation logs. Each format has specific use cases and options.

## Available Formats

| Format | Extension | Use Case |
|--------|-----------|----------|
| Markdown | `.md` | Documentation, reading, sharing |
| JSON | `.json` | Data processing, API integration |
| JSON (Pretty) | `.json` | Human-readable JSON with indentation |
| HTML | `.html` | Web viewing, archiving |
| Plain Text | `.txt` | Simple text, compatibility |
| CSV | `.csv` | Spreadsheet analysis |
| XML | `.xml` | Enterprise integration |
| SQLite | `.db` | Database queries, analysis |
| JSONL | `.jsonl` | Normalized line-by-line export (filterable, not byte-faithful) |
| raw-jsonl | `.jsonl` | Byte-faithful archival passthrough of the original log |
| OpenTelemetry | `.json` | Observability pipelines, distributed tracing |

## Command Line Usage

```bash
# Export to Markdown (default)
snatch export <session-id>

# Export to specific format
snatch export <session-id> --format json
snatch export <session-id> --format html
snatch export <session-id> --format csv

# Specify output file
snatch export <session-id> --output conversation.md

# Export with options
snatch export <session-id> --include-thinking --include-tools

# Export main thread only (exclude branches)
snatch export <session-id> --main-thread
```

## Format Details

### Markdown

Best for documentation and sharing.

```markdown
# Conversation Export

**Session**: abc12345-def6-7890-abcd-ef1234567890
**Date**: 2025-01-15 10:30:00 UTC
**Messages**: 42

---

## User (10:30:15)

Can you help me with this code?

---

## Assistant (10:30:18)

Of course! Let me analyze your code...

```python
def example():
    return "Hello, World!"
```
```

Options:
- `--include-thinking`: Include thinking blocks in output
- `--include-tools`: Include tool calls and results
- `--main-thread`: Export only main conversation thread

### JSON

Structured data format for programmatic access.

```json
{
  "version": "1.0",
  "exported_at": "2025-01-15T10:30:00Z",
  "exporter": "snatch",
  "metadata": { "session_id": "abc12345-def6-7890-abcd-ef1234567890" },
  "analytics": { "total_messages": 42 },
  "tree": { },
  "entries": [
    {
      "type": "user",
      "timestamp": "2025-01-15T10:30:15Z",
      "message": { "role": "user", "content": "Can you help me with this code?" }
    },
    {
      "type": "assistant",
      "timestamp": "2025-01-15T10:30:18Z",
      "message": { "role": "assistant", "content": [{ "type": "text", "text": "..." }] }
    }
  ]
}
```

The top-level conversation array is `entries` (one element per log entry). Use
`--format json-pretty` for indented output.

### HTML

Self-contained HTML file with embedded CSS for web viewing.

Features:
- Light (default) / dark theme support
- Syntax highlighted code blocks
- Collapsible thinking blocks
- Responsive design

```bash
# Light theme (default)
snatch export <session-id> --format html

# Dark theme
snatch export <session-id> --format html --dark
```

### Plain Text

Simple text format without formatting.

```
=== Conversation Export ===
Session: abc12345
Date: 2025-01-15

--- User (10:30:15) ---
Can you help me with this code?

--- Assistant (10:30:18) ---
Of course! Let me analyze your code...
```

Options:
- `--width <n>`: Set line width for wrapping (default: 80)

### CSV

Spreadsheet-compatible format for data analysis.

Columns:
- `timestamp`: ISO 8601 timestamp
- `type`: Message type (user/assistant/system/tool)
- `content`: Message content (truncated if too long)
- `tokens`: Token count (if available)
- `model`: Model used (if available)

```csv
timestamp,type,content,tokens,model
2025-01-15T10:30:15Z,user,"Can you help with this code?",12,
2025-01-15T10:30:18Z,assistant,"Of course! Let me analyze...",45,claude-3.5-sonnet
```

### XML

Enterprise-friendly structured format.

```xml
<?xml version="1.0" encoding="UTF-8"?>
<conversation>
  <metadata>
    <session_id>abc12345-def6-7890-abcd-ef1234567890</session_id>
    <exported_at>2025-01-15T10:30:00Z</exported_at>
    <total_messages>42</total_messages>
  </metadata>
  <messages>
    <message type="user" timestamp="2025-01-15T10:30:15Z">
      <content>Can you help me with this code?</content>
    </message>
    <message type="assistant" timestamp="2025-01-15T10:30:18Z">
      <content>Of course! Let me analyze your code...</content>
    </message>
  </messages>
</conversation>
```

### SQLite

Database format for complex queries and analysis.

Tables:
- `sessions`: Session metadata
- `entries`: One row per raw log entry, with full content
- `content_blocks`: Content blocks belonging to an entry
- `tool_uses`: Tool invocations
- `tool_results`: Tool execution results
- `thinking_blocks`: Extended thinking content
- `usage_stats`: Per-session token/cost statistics (logical message counts)
- `tool_usage`: Per-tool invocation counts
- `entries_fts` / `thinking_fts`: Full-text search indexes

```bash
# Export to SQLite
snatch export <session-id> --format sqlite

# Query the database
sqlite3 session.db "SELECT * FROM entries WHERE role='assistant'"
```

### JSONL (JSON Lines)

Line-delimited JSON for streaming and processing.

```jsonl
{"type":"metadata","session_id":"abc12345","message_count":42}
{"type":"user","timestamp":"2025-01-15T10:30:15Z","content":"Can you help?"}
{"type":"assistant","timestamp":"2025-01-15T10:30:18Z","content":"Of course!"}
```

Ideal for:
- Streaming processing
- Large file handling
- Log aggregation tools

`jsonl` is normalized (content-preserving but reordered/reshaped). For a
byte-for-byte copy of the original Claude Code log, use `raw-jsonl`.

### raw-jsonl (archival passthrough)

Byte-faithful passthrough of the original Claude Code JSONL — no parsing,
filtering, redaction, or reordering. This is the archival mode.

```bash
snatch export <session-id> --format raw-jsonl -O archive.jsonl
```

Notes:
- Rejects `--redact`, `--only`, and other transforming flags (it must stay
  byte-identical to the source).
- Single-file by design: subagent transcripts (the `subagents/` directory) are
  **not** included; snatch warns when they exist so the omission isn't silent.

### OpenTelemetry (OTLP)

Export conversations as OpenTelemetry traces for observability platforms.

```bash
snatch export <session-id> --format otel --output traces.json
```

Creates OTLP JSON format compatible with:
- Jaeger
- Grafana Tempo
- Honeycomb
- Datadog APM
- Any OTLP-compatible backend

Each message becomes a span with:
- Timestamps as span start/end times
- Message content as span attributes
- Tool calls as child spans
- Token usage as metrics

## Export Options

### Common Options

| Option | Description |
|--------|-------------|
| `--output <file>` | Output file path |
| `--include-thinking` | Include thinking blocks |
| `--include-tools` | Include tool calls and results |
| `--main-thread` | Export only main thread |
| `--no-timestamps` | Omit timestamps |
| `--no-metadata` | Omit session metadata |

### Privacy & Redaction Options

| Option | Description |
|--------|-------------|
| `--redact security` | Redact API keys, passwords, tokens, secrets |
| `--redact all` | Also redact emails, IP addresses, phone numbers |
| `--redact-preview` | Preview what would be redacted without removing |
| `--warn-pii` | Warn about potential PII without redacting |

Example workflow for safe sharing:

```bash
# First, preview what will be redacted
snatch export <session-id> --redact security --redact-preview

# If satisfied, perform the actual redaction
snatch export <session-id> --redact security --output safe-export.md
```

Redaction replaces sensitive data with type-specific placeholders:
- `[REDACTED:API_KEY]`
- `[REDACTED:PASSWORD]`
- `[REDACTED:EMAIL]`
- `[REDACTED:IP_ADDRESS]`

### Content Filtering

Control which content appears in the export:

- `--only <types>`: Exclusive whitelist — include only the listed content types
  (e.g. `--only prompts`, `--only tool-results`, `--only user`, `--only code`).
  Accepts a comma-separated list.
- `--no-thinking` / `--no-tool-use` / `--no-tool-results` / `--no-images`: Exclude
  a content type while keeping everything else (the blocklist counterpart to
  `--only`). `--no-images` prunes top-level image blocks; images embedded inside
  tool-result content are not affected.
- `--full`: Include everything (system messages, metadata, etc.).
- `--warn-pii`: Scan the export (including tool-result content) for sensitive data
  and warn before writing.

`--only` is a focus filter; `--redact` is the privacy control. Use `--redact` to
remove secrets, not `--only`.

### Subagents

- `--combine-agents`: Interleave a parent session with its subagent transcripts
  into a single export (works with markdown/json/sqlite/jsonl).
- `--subagents`: Affects `--all` batch listing only; inert on a single session.
- `raw-jsonl` cannot include subagents (single-file); snatch warns when they exist.

### Destinations

- `-O, --out <FILE>`: Write to a file (otherwise stdout).
- `--gist`: Upload the export to a GitHub gist (requires `gh`).
- `--clipboard`: Copy the export to the clipboard.
- `--template <name>`: Render with a custom template (`--template list` to see them).

### Format-Specific Options

- **HTML:** `--dark` (dark theme; default is light), `--toc` (table of contents).
- **JSON:** `--pretty` (or use the `json-pretty` format).

## Batch Export

Export multiple sessions at once:

```bash
# Export all sessions from a project
snatch export --project /path/to/project --format markdown

# Export sessions from a date range
snatch export --all --project /path/to/project --since 2025-01-01 --until 2025-01-31

# Export all matching sessions
snatch export --all --project /path/to/project --format markdown
```

## Programmatic Export

Use snatch as a library:

```rust
use claude_snatch::export::{MarkdownExporter, ExportOptions, Exporter};
use claude_snatch::reconstruction::Conversation;

let conversation = Conversation::from_entries(entries)?;
let exporter = MarkdownExporter::new();
let options = ExportOptions::default()
    .with_thinking(true)
    .with_tool_use(true);

let mut output = Vec::new();
exporter.export_conversation(&conversation, &mut output, &options)?;
```
