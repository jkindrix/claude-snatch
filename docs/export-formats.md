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
| JSONL | `.jsonl` | Line-by-line processing, streaming |

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
  "metadata": {
    "session_id": "abc12345-def6-7890-abcd-ef1234567890",
    "exported_at": "2025-01-15T10:30:00Z",
    "total_messages": 42,
    "schema_version": "1.0"
  },
  "messages": [
    {
      "type": "user",
      "timestamp": "2025-01-15T10:30:15Z",
      "content": "Can you help me with this code?"
    },
    {
      "type": "assistant",
      "timestamp": "2025-01-15T10:30:18Z",
      "content": [
        {
          "type": "text",
          "text": "Of course! Let me analyze your code..."
        }
      ]
    }
  ]
}
```

Use `--format json-pretty` for indented output.

### HTML

Self-contained HTML file with embedded CSS for web viewing.

Features:
- Dark/light theme support
- Syntax highlighted code blocks
- Collapsible thinking blocks
- Responsive design

```bash
# Dark theme (default)
snatch export <session-id> --format html

# Light theme
snatch export <session-id> --format html --theme light
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
- `messages`: All messages with full content
- `tool_calls`: Tool invocations
- `tool_results`: Tool execution results
- `thinking_blocks`: Extended thinking content
- `tokens`: Token usage per message

```bash
# Export to SQLite
snatch export <session-id> --format sqlite

# Query the database
sqlite3 session.db "SELECT * FROM messages WHERE type='assistant'"
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

### Format-Specific Options

**HTML:**
- `--theme <dark|light>`: Color theme
- `--standalone`: Include all CSS inline

**Text:**
- `--width <n>`: Line width for wrapping

**JSON:**
- `--pretty`: Pretty-print with indentation

**CSV:**
- `--delimiter <char>`: Field delimiter (default: comma)
- `--quote <char>`: Quote character (default: double-quote)

## Batch Export

Export multiple sessions at once:

```bash
# Export all sessions from a project
snatch export --project /path/to/project --format markdown

# Export sessions from a date range
snatch export --project /path/to/project --after 2025-01-01 --before 2025-01-31

# Export to a directory
snatch export --project /path/to/project --output-dir ./exports/
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
