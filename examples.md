# Snatch CLI Examples

A comprehensive collection of example commands demonstrating snatch's features.

## Session 1: Initial Exploration

### Discovery & Help

```bash
# Project overview
ls -la

# Get help
snatch --help
snatch --version
snatch list --help
```

### Listing Sessions & Projects

```bash
# List sessions (default)
snatch list

# List projects
snatch list projects

# List with JSON output
snatch list --json | head -100

# List with TSV output
snatch list -o tsv -n 10

# Filter by date
snatch list --since 1week -n 10
```

### Statistics

```bash
# Basic stats
snatch stats --help
snatch stats

# Global statistics
snatch stats --global

# Tool and model breakdowns
snatch stats --tools
snatch stats --models
```

### Search

```bash
# Basic search
snatch search --help
snatch search "TODO" -n 5

# Fuzzy search
snatch search "error" --fuzzy -n 3

# Search in tool outputs
snatch search "implement" --tool-name Write -n 3

# Indexed search (faster)
snatch index search "rust async" -n 3
```

### Session Information

```bash
# Session info
snatch info --help
snatch info 29e7bbff

# Tree structure view
snatch info 29e7bbff --tree
```

### Export Formats

```bash
# Export help
snatch export --help

# Markdown (default)
snatch export 29e7bbff -f markdown | head -100

# HTML (user messages only)
snatch export 29e7bbff -f html --only user | head -60

# CSV
snatch export 29e7bbff -f csv | head -20

# XML
snatch export 29e7bbff -f xml | head -50

# Plain text (prompts only)
snatch export 29e7bbff -f text --only prompts | head -30

# SQLite database
snatch export 29e7bbff -f sqlite -O /tmp/test_session.db
sqlite3 /tmp/test_session.db ".tables"
sqlite3 /tmp/test_session.db "SELECT COUNT(*) as total_messages FROM messages;"
sqlite3 /tmp/test_session.db ".schema messages"
```

### Diff & Comparison

```bash
snatch diff --help
snatch diff 29e7bbff 780893e4 --summary-only
```

### Extract Beyond-JSONL Data

```bash
snatch extract --help
snatch extract --all | head -60
```

### Configuration

```bash
snatch config --help
snatch config show
```

### Tags & Organization

```bash
snatch tag --help
snatch tag list
```

### Prompts Extraction

```bash
snatch prompts --help
snatch prompts 29e7bbff | head -30
# Note: requires session ID or --all flag
```

### Cache Management

```bash
snatch cache --help
snatch cache stats
```

### Search Index

```bash
snatch index --help
snatch index status
```

### Other Commands

```bash
# Watch for changes
snatch watch --help

# Cleanup empty sessions
snatch cleanup --help
snatch cleanup --empty --preview

# Validate session files
snatch validate --help
snatch validate 29e7bbff

# Shell completions
snatch completions --help

# TUI (interactive)
snatch tui --help
```

---

## Session 2: Gap Coverage

### Security & Privacy

```bash
# Redact sensitive data
snatch export 29e7bbff --redact security | head -60
snatch export 29e7bbff --redact all | head -60

# Warn about PII without redacting
snatch export 29e7bbff --warn-pii | head -40
snatch export 29e7bbff --warn-pii -f text | head -5

# Search for sensitive patterns
snatch search "sk-" --tools -n 3
snatch search "password" --tools -n 3
snatch search "ANTHROPIC_API_KEY\|OPENAI_API_KEY\|Bearer " --tools -n 3
snatch search "Bearer\|sk-ant\|ghp_\|npm_" --tools -n 3

# Check for redaction markers
snatch export 29e7bbff --redact all -f text | grep -i "REDACTED" | head -10
snatch export 29e7bbff --redact all -f text | grep -E "REDACTED|\[IP\]|\[EMAIL\]" | head -10
snatch export 29e7bbff --redact all -f text | grep -F "[REDACTED]" | head -5
```

### Diff Commands

```bash
# Full semantic diff
snatch diff 29e7bbff 780893e4 | head -80

# Line-based diff
snatch diff 29e7bbff 780893e4 --line-based --summary-only
snatch diff 476d6f8f 12fcb411
snatch diff 476d6f8f 12fcb411 --no-content
snatch diff 476d6f8f 12fcb411 --line-based | head -50
snatch diff 476d6f8f 12fcb411 --line-based | tail -80

# Diff help for content options
snatch diff --help | grep -A2 "content\|detail"
```

### Tagging Workflow

```bash
# Add tags
snatch tag add 29e7bbff "reviewed"

# Name sessions
snatch tag name 29e7bbff "Tree2Repo Analysis Session"

# Bookmark sessions
snatch tag bookmark 29e7bbff

# Set outcomes
snatch tag outcome 29e7bbff success

# List and find
snatch tag list
snatch tag find "reviewed"
snatch tag bookmarks
snatch tag outcomes

# View session with tags
snatch info 29e7bbff
snatch tag list 29e7bbff

# Remove tags
snatch tag remove 29e7bbff "reviewed"
snatch tag unbookmark 29e7bbff
```

### Bug Fix Verification

```bash
# Compare condensed vs full help
snatch list -h | wc -l      # 29 lines (condensed)
snatch list --help | wc -l  # 148 lines (full)
snatch list -h

# Verify -n/--limit flag on prompts
snatch prompts --help | grep -E "\-n|--limit"
snatch prompts -n 3 --all | head -30

# Verify info shows tags
snatch tag add 29e7bbff "verification-test"
snatch info 29e7bbff  # Should show tags
snatch tag remove 29e7bbff "verification-test"
```

### Watch (Real-time Monitoring)

```bash
snatch watch --help | head -20

# List active sessions
snatch list --active

# Watch specific session
timeout 3 snatch watch 404215fa --follow

# Live dashboard mode
timeout 3 snatch watch --all --live
```

### Additional Search Patterns

```bash
# Search for emails/IPs
snatch search "email\|@.*\.com" -n 3 --tools
snatch search "192\.\|10\.\|172\." -n 3 --tools

# Search for IP pattern in specific session
snatch search "[0-9]+\.[0-9]+\.[0-9]+\.[0-9]+" -s 29e7bbff -n 3

# Extract IPs from export
snatch export 29e7bbff -f text | grep -oE "[0-9]+\.[0-9]+\.[0-9]+\.[0-9]+" | head -5
snatch export 29e7bbff -f text | grep -i "ip\|address\|192\|10\.\|172\." | head -10
snatch export 29e7bbff --redact all -f text | grep -i "ip\|address\|redact" | head -10
```

### Other

```bash
# List sessions in project with sizes
snatch list -p snatch -n 5 --sizes

# Diff between sessions in same project
snatch diff ddb23bc4 476d6f8f | head -60

# Extract settings
snatch extract --settings

# TUI theme options
snatch tui --help | grep -A5 "theme"

# Config as JSON
snatch config show --json | head -30
```

---

## Commands That Failed (Expected)

```bash
# TUI requires interactive terminal
timeout 2 snatch tui  # Error: no TTY

# List doesn't accept session ID as argument
snatch list 29e7bbff  # Error: invalid argument

# Prompts requires session ID or --all
snatch prompts -n 5  # Error: missing required argument
```
