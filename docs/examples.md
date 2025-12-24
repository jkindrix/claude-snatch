# Examples and Recipes

Practical examples for common snatch use cases.

## Quick Start

### View Recent Sessions

```bash
# List all sessions
snatch list

# List sessions for a specific project
snatch list --project /path/to/project

# List with details
snatch list --verbose
```

### Interactive Browsing

```bash
# Launch TUI browser
snatch tui

# Open specific project
snatch tui --project ~/my-project

# Open specific session
snatch tui --session abc12345
```

### Export a Conversation

```bash
# Export to Markdown
snatch export abc12345

# Export to JSON
snatch export abc12345 --format json --output conversation.json

# Export with thinking blocks
snatch export abc12345 --include-thinking
```

## Common Workflows

### 1. Archive All Project Conversations

```bash
#!/bin/bash
# archive-project.sh

PROJECT="/path/to/project"
OUTPUT_DIR="./archives/$(date +%Y-%m-%d)"

mkdir -p "$OUTPUT_DIR"

snatch list --project "$PROJECT" --format json | \
  jq -r '.sessions[].id' | \
  while read session_id; do
    snatch export "$session_id" \
      --format markdown \
      --output "$OUTPUT_DIR/${session_id}.md" \
      --include-thinking \
      --include-tools
  done

echo "Archived to $OUTPUT_DIR"
```

### 2. Search Across All Sessions

```bash
# Search for a keyword in all sessions
snatch search "API endpoint" --project /path/to/project

# Search with context
snatch search "error" --context 3

# Search in specific date range
snatch search "refactor" --after 2025-01-01 --before 2025-01-31
```

### 3. Generate Project Documentation

```bash
#!/bin/bash
# generate-docs.sh

# Export all sessions as a single document
snatch export --project /path/to/project \
  --format markdown \
  --output docs/development-log.md \
  --merge \
  --include-thinking

echo "Generated docs/development-log.md"
```

### 4. Analyze Token Usage

```bash
# Show token usage for a session
snatch stats abc12345

# Show usage for all sessions in a project
snatch stats --project /path/to/project

# Export usage to CSV
snatch stats --project /path/to/project --format csv > usage.csv
```

### 5. Extract Code Blocks

```bash
# Extract all code from a session
snatch extract abc12345 --type code --output code-blocks/

# Extract specific language
snatch extract abc12345 --type code --language python --output python-code/

# Extract with file metadata
snatch extract abc12345 --type code --with-metadata
```

## TUI Recipes

### Navigate Large Sessions

1. Press `/` to start search
2. Type your search term
3. Press `Enter` to confirm
4. Use `n` for next match, `N` for previous
5. Press `Escape` to exit search mode

### Filter by Message Type

1. Press `F` to cycle through filters:
   - All → User → Assistant → System → Tools
2. Press `X` to clear filters

### Filter by Date Range

1. Press `[` to set start date
2. Type date in YYYY-MM-DD format
3. Press `Enter` to confirm
4. Press `]` to set end date
5. Press `X` to clear date filters

### Quick Export from TUI

1. Select a session
2. Press `e` to open export dialog
3. Use `h`/`l` to select format
4. Press `t` to toggle thinking blocks
5. Press `o` to toggle tool outputs
6. Press `Enter` to export

### Copy Code to Clipboard

1. Navigate to message with code
2. Press `C` to copy code block
3. Press `c` to copy entire message

## Integration Examples

### With Git Hooks

```bash
#!/bin/bash
# .git/hooks/post-commit

# Archive Claude session after each commit
SESSION=$(snatch list --project "$(pwd)" --limit 1 --format json | jq -r '.sessions[0].id')

if [ -n "$SESSION" ]; then
  snatch export "$SESSION" \
    --format markdown \
    --output ".claude/archives/$(git rev-parse --short HEAD).md"
fi
```

### With CI/CD

```yaml
# .github/workflows/archive.yml
name: Archive Conversations

on:
  schedule:
    - cron: '0 0 * * 0'  # Weekly

jobs:
  archive:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4

      - name: Install snatch
        run: cargo install claude-snatch

      - name: Archive conversations
        run: |
          snatch export --project . \
            --format json \
            --output archives/$(date +%Y-%m-%d).json

      - name: Commit archives
        run: |
          git add archives/
          git commit -m "chore: archive conversations"
          git push
```

### As a Library

```rust
use claude_snatch::{
    discovery::ClaudeDirectory,
    parser::JsonlParser,
    reconstruction::Conversation,
    analytics::SessionAnalytics,
    export::{MarkdownExporter, ExportOptions, Exporter},
};

fn main() -> anyhow::Result<()> {
    // Discover Claude directory
    let claude_dir = ClaudeDirectory::discover()?;

    // Find a session
    let session = claude_dir.find_session("abc12345")?
        .expect("Session not found");

    // Parse the session
    let mut parser = JsonlParser::new();
    let entries = parser.parse_file(session.path())?;

    // Build conversation
    let conversation = Conversation::from_entries(entries)?;

    // Get analytics
    let analytics = SessionAnalytics::from_conversation(&conversation);
    let summary = analytics.summary_report();

    println!("Messages: {}", summary.total_messages);
    println!("Tokens: {}", summary.total_tokens);
    println!("Cost: {}", summary.cost_string());

    // Export to Markdown
    let exporter = MarkdownExporter::new();
    let options = ExportOptions::default()
        .with_thinking(true)
        .with_tool_use(true);

    let mut output = std::fs::File::create("output.md")?;
    exporter.export_conversation(&conversation, &mut output, &options)?;

    Ok(())
}
```

## Advanced Recipes

### Diff Two Sessions

```bash
# Compare two sessions
snatch diff session1 session2

# Show only message differences
snatch diff session1 session2 --messages-only

# Output as JSON
snatch diff session1 session2 --format json
```

### Merge Agent Hierarchy

```bash
# Export parent session with all subagents
snatch export parent-session --combine-agents

# Show agent hierarchy
snatch tree parent-session
```

### Extract Backup History

```bash
# List file backups from a session
snatch backups abc12345

# Extract specific file version
snatch backups abc12345 --file src/main.rs --version 2

# Diff backup versions
snatch backups abc12345 --file src/main.rs --diff 1 2
```

### Generate Statistics Report

```bash
#!/bin/bash
# weekly-report.sh

echo "# Weekly Claude Usage Report"
echo "Generated: $(date)"
echo

snatch stats \
  --project /path/to/project \
  --after "$(date -d '7 days ago' +%Y-%m-%d)" \
  --format markdown

echo
echo "## Cost Summary"
snatch stats \
  --project /path/to/project \
  --after "$(date -d '7 days ago' +%Y-%m-%d)" \
  --summary-only
```

### Watch for New Sessions

```bash
#!/bin/bash
# watch-sessions.sh

snatch watch --project /path/to/project --on-new "
  echo 'New session: \$SESSION_ID'
  snatch export \$SESSION_ID --format markdown --output latest.md
"
```

## Troubleshooting

### Session Not Found

```bash
# List all sessions to find the correct ID
snatch list --all

# Check if Claude directory is correct
snatch config show | grep claude_dir
```

### Export Fails

```bash
# Check if session exists
snatch show abc12345

# Try with verbose output
snatch export abc12345 --verbose

# Check permissions
ls -la ~/.claude/projects/
```

### TUI Not Rendering

```bash
# Check terminal capabilities
echo $TERM

# Try with different theme
snatch tui --theme high_contrast

# Check for color support
snatch tui --no-color
```
