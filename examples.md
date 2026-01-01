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

## Session 3: Comprehensive Flag Coverage

### Export - Additional Options

```bash
# Lossless export (preserves all data including unknown fields)
snatch export 29e7bbff --lossless | head -50

# Combine parent with subagent transcripts (interleaved by time)
snatch export 29e7bbff --combine-agents | head -100

# Include timestamps and usage stats
snatch export 29e7bbff --timestamps --usage | head -50

# Main thread only (exclude branches)
snatch export 29e7bbff --main-thread | head -50

# Multiple content type filters
snatch export 29e7bbff --only user,thinking | head -50

# Missing formats
snatch export 29e7bbff -f jsonl | head -20       # Original format
snatch export 29e7bbff -f json-pretty | head -50 # Pretty JSON

# Progress bar for large exports
snatch export --all -p snatch --progress -O /tmp/all-snatch.md
```

### List - Additional Options

```bash
# Show full UUIDs
snatch list --full-ids -n 5

# Include subagent sessions
snatch list --subagents -n 10

# Different sort orders
snatch list --sort oldest -n 5
snatch list --sort size -n 5
snatch list --sort name -n 5

# Date range filter
snatch list --since 2024-12-01 --until 2024-12-15

# List everything
snatch list all -n 10

# Pipe through pager
snatch list -n 0 --pager
```

### Search - Additional Options

```bash
# Search in thinking blocks
snatch search "reasoning" --thinking -n 5

# Search everywhere (user, assistant, thinking, tools)
snatch search "error" --all -n 5

# More context lines
snatch search "bug" -C 5 -n 3

# Count matches only
snatch search "TODO" --count

# Files only (like grep -l)
snatch search "refactor" --files-only -n 10

# Filter by token count
snatch search "implementation" --min-tokens 100 -n 5

# Filter by git branch
snatch search "feature" --branch main -n 5

# Only show errors
snatch search "failed" --errors -n 5

# Sort by relevance
snatch search "performance" --sort -n 10
```

### Stats - Additional Options

```bash
# Cost breakdown
snatch stats --costs

# All detailed stats
snatch stats -a

# Stats for specific session
snatch stats -s 29e7bbff
```

### Info - Additional Options

```bash
# Raw JSONL entries
snatch info 29e7bbff --raw | head -20

# Specific entry by UUID
snatch info 29e7bbff --entry <uuid>

# Show file paths
snatch info 29e7bbff --paths
```

### Extract - Individual Flags

```bash
# Specific data types
snatch extract --claude-md
snatch extract --mcp
snatch extract --commands
snatch extract --rules
snatch extract --hooks
snatch extract --file-history

# Project-specific
snatch extract -p /home/user/project --all
```

### Config - Full Workflow

```bash
# Get specific value
snatch config get cache.enabled

# Set value
snatch config set display.truncate_at 5000

# Show config file path
snatch config path

# Initialize with defaults
snatch config init

# Reset to defaults
snatch config reset
```

### Cache - Full Workflow

```bash
# Clear all cache
snatch cache clear

# Invalidate stale entries
snatch cache invalidate

# Enable/disable caching
snatch cache status enable
snatch cache status disable
```

### Index - Full Workflow

```bash
# Build index
snatch index build

# Rebuild from scratch
snatch index rebuild

# Clear index
snatch index clear
```

### Prompts - Additional Options

```bash
# With session separators
snatch prompts --all --separators | head -50

# With timestamps
snatch prompts 29e7bbff --timestamps

# Numbered list
snatch prompts 29e7bbff --numbered

# Output to file
snatch prompts --all -O prompts.txt
```

### Completions - All Shells

```bash
# Generate for different shells
snatch completions zsh > ~/.zsh/completions/_snatch
snatch completions fish > ~/.config/fish/completions/snatch.fish
snatch completions powershell > snatch.ps1
```

### Tag - Additional Outcomes

```bash
# Other outcome types
snatch tag outcome <session> partial
snatch tag outcome <session> failed
snatch tag outcome <session> abandoned
```

### Global Flags - Demonstrations

```bash
# Logging levels
snatch list --log-level debug 2>&1 | head -20
snatch list --log-level trace 2>&1 | head -20

# Log to file
snatch stats --global --log-file /tmp/snatch.log

# Parallel processing
snatch export --all -j 4 -O /tmp/export.md

# Custom max file size
snatch list --max-file-size 0         # Unlimited
snatch list --max-file-size 52428800  # 50MB
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
