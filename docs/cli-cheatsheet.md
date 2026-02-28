Snatch Cheatsheet (v0.1.0)

Claude Code conversation log extractor and analyzer.

Reads ~/.claude session data. All session IDs support short prefixes (e.g. 780893e4).
Running snatch with no subcommand launches the interactive TUI in a terminal.

Global Options (apply to all commands)

┌─────────────────────────┬───────┬──────────────────────────────────────────────┐
│          Flag           │ Short │                 Description                  │
├─────────────────────────┼───────┼──────────────────────────────────────────────┤
│ --claude-dir <PATH>     │ -d    │ Custom Claude directory (default: ~/.claude) │
├─────────────────────────┼───────┼──────────────────────────────────────────────┤
│ --output <FMT>          │ -o    │ Output format: text, json, tsv, compact      │
├─────────────────────────┼───────┼──────────────────────────────────────────────┤
│ --json                  │       │ Shorthand for -o json                        │
├─────────────────────────┼───────┼──────────────────────────────────────────────┤
│ --verbose               │ -v    │ Verbose output                               │
├─────────────────────────┼───────┼──────────────────────────────────────────────┤
│ --quiet                 │ -q    │ Suppress non-essential output                │
├─────────────────────────┼───────┼──────────────────────────────────────────────┤
│ --color true/false      │       │ Force color on/off                           │
├─────────────────────────┼───────┼──────────────────────────────────────────────┤
│ --threads <N>           │ -j    │ Parallel threads (default: CPU count)        │
├─────────────────────────┼───────┼──────────────────────────────────────────────┤
│ --max-file-size <BYTES> │       │ Cap file parsing to prevent OOM              │
├─────────────────────────┼───────┼──────────────────────────────────────────────┤
│ --config <PATH>         │       │ Custom config file                           │
├─────────────────────────┼───────┼──────────────────────────────────────────────┤
│ --log-level <LVL>       │       │ error, warn, info, debug, trace              │
├─────────────────────────┼───────┼──────────────────────────────────────────────┤
│ --log-format <FMT>      │       │ text, json, compact, pretty                  │
├─────────────────────────┼───────┼──────────────────────────────────────────────┤
│ --log-file <PATH>       │       │ Log output file (default: stderr)            │
└─────────────────────────┴───────┴──────────────────────────────────────────────┘

Command Aliases

Most commands have short aliases for quick access:

  list → ls            info → i, show       pick → browse
  search → s, find     stats → stat         standup → daily
  diff → d             export → x           recover → restore
  tui → ui             cleanup → clean      index → idx
  config → cfg         extract → ext        quickstart → guide, examples

---
Browsing & Discovery

List sessions and projects (alias: ls)

snatch list                           # 50 most recent sessions
snatch list projects                  # list all projects
snatch list projects --hide-empty     # skip projects with 0 sessions
snatch list sessions -p myproject     # sessions for a project (substring match)
snatch list -n 20 --sort size --sizes # top 20 by size, show file sizes
snatch list --since 3days             # sessions from last 3 days
snatch list --until 2026-01-01        # sessions before a date
snatch list --tag bugfix              # filter by tag
snatch list --tags bug,wip            # filter by multiple tags
snatch list --bookmarked              # only bookmarked sessions
snatch list --outcome success         # filter by outcome
snatch list --by-name "auth refactor" # filter by custom name
snatch list -c                        # show first-prompt preview
snatch list -c --context-length 200   # longer preview
snatch list --active                  # only currently-active sessions
snatch list --subagents               # include subagent sessions
snatch list --full-ids                # full UUIDs instead of short IDs
snatch list --min-size 1MB            # only sessions larger than 1MB
snatch list --max-size 100KB          # only sessions smaller than 100KB
snatch list --pager                   # pipe output through less/more

Quick recent sessions

snatch recent                         # last 5 sessions
snatch recent -n 10                   # last 10 sessions
snatch recent -p myproject            # recent for a project

Session details (aliases: i, show)

snatch info <SESSION>                 # detailed session info
snatch info <SESSION> --tree          # tree structure view
snatch info <SESSION> --raw           # raw JSONL entries
snatch info <SESSION> --paths         # file paths and locations
snatch info <SESSION> -m 10           # preview first 10 messages
snatch info <SESSION> --files         # files touched (created/modified/read)
snatch info <SESSION> --entry <UUID>  # specific entry by UUID

Interactive picker (alias: browse)

snatch pick                           # fuzzy-find a session, print its ID
snatch pick -a info                   # pick then show info
snatch pick -a stats                  # pick then show stats
snatch pick -a open                   # pick then print file path
snatch pick -p myproject              # scope to project
snatch pick --subagents               # include subagents

Interactive TUI browser (alias: ui)

snatch tui                            # launch TUI
snatch tui -p myproject               # start on a project
snatch tui -s <SESSION>               # start on a session
snatch tui --theme mytheme            # custom theme
snatch tui --ascii                    # ASCII-only (no Unicode box drawing)

---
Searching

Regex/fuzzy search across sessions (aliases: s, find)

snatch search "pattern"               # regex search user+assistant text
snatch search "pattern" -i            # case-insensitive
snatch search "pattern" -a            # search everywhere (thinking+tools too)
snatch search "pattern" --thinking    # also search thinking blocks
snatch search "pattern" --tools       # also search tool outputs
snatch search "pattern" -p myproject  # scope to project
snatch search "pattern" -s <SESSION>  # scope to single session
snatch search "pattern" -C 5          # 5 context lines
snatch search "pattern" -n 100        # max 100 results
snatch search "pattern" --no-limit    # all results
snatch search "pattern" -l            # session IDs only (like grep -l)
snatch search "pattern" -c            # match count only (like grep -c)
snatch search "pattern" -f            # fuzzy matching
snatch search "pattern" -f --fuzzy-threshold 80  # stricter fuzzy
snatch search "pattern" -t user       # only user messages
snatch search "pattern" -t assistant  # only assistant messages
snatch search "pattern" -m opus       # filter by model
snatch search "pattern" --tool-name Bash  # filter by tool
snatch search "pattern" --errors      # only messages with errors
snatch search "pattern" -b main       # filter by git branch
snatch search "pattern" --sort        # sort by relevance
snatch search "pattern" --min-tokens 500   # minimum token count
snatch search "pattern" --max-tokens 10000 # maximum token count

Full-text index (alias: idx) — faster than regex

snatch index build                    # build/update the search index
snatch index build -p myproject       # build index for one project
snatch index rebuild                  # rebuild from scratch
snatch index rebuild -p myproject     # rebuild for one project
snatch index status                   # show index status
snatch index clear                    # clear the index
snatch index search "query"           # search the index
snatch index search "query" -n 20     # limit results
snatch index search "query" -t user   # filter by message type
snatch index search "query" -m opus   # filter by model
snatch index search "query" -s <ID>   # filter by session
snatch index search "query" --tool-name Bash  # filter by tool
snatch index search "query" --thinking  # include thinking blocks

---
Exporting

Export conversations (alias: x)

snatch export <SESSION>               # to markdown (default)
snatch export <SESSION> -f json       # JSON
snatch export <SESSION> -f html       # HTML
snatch export <SESSION> -f html --toc --dark  # HTML with TOC, dark theme
snatch export <SESSION> -f text       # plain text
snatch export <SESSION> -f csv        # CSV
snatch export <SESSION> -f xml        # XML
snatch export <SESSION> -f jsonl      # original JSONL
snatch export <SESSION> -f json-pretty # pretty JSON
snatch export <SESSION> -f sqlite     # SQLite database
snatch export <SESSION> -f otel       # OpenTelemetry OTLP JSON
snatch export <SESSION> -O out.md     # write to file
snatch export <SESSION> --clipboard   # copy to clipboard (alias: --copy)
snatch export <SESSION> --gist        # upload as private GitHub Gist
snatch export <SESSION> --gist --gist-public  # public Gist
snatch export <SESSION> --gist --gist-description "My session"  # with description

Content filtering

snatch export <SESSION> --only user               # only user messages
snatch export <SESSION> --only prompts             # only human-typed prompts
snatch export <SESSION> --only code                # only code blocks
snatch export <SESSION> --only thinking,assistant  # thinking + assistant
snatch export <SESSION> --no-thinking              # exclude thinking
snatch export <SESSION> --no-tool-use              # exclude tool calls
snatch export <SESSION> --no-tool-results          # exclude tool results
snatch export <SESSION> --system                   # include system messages
snatch export <SESSION> --metadata                 # include UUIDs and metadata
snatch export <SESSION> --main-thread              # exclude branches
snatch export <SESSION> --lossless                 # preserve all data

Note: --timestamps and --usage are enabled by default. Use --no-timestamps
or --no-usage to exclude them.

Bulk export

snatch export --all                   # export all sessions
snatch export --all -p myproject      # all sessions in project
snatch export --all --since 1week     # sessions from last week
snatch export --all --combine-agents  # interleave subagent transcripts
snatch export --all --progress        # show progress bar
snatch export --all -O ./exports/ --overwrite  # write to dir

Privacy/redaction

snatch export <SESSION> --redact security      # redact API keys, passwords
snatch export <SESSION> --redact all           # also redact emails, IPs, phones
snatch export <SESSION> --redact-preview --redact security  # preview what gets redacted
snatch export <SESSION> --warn-pii             # warn about PII without modifying

Custom templates

snatch export <SESSION> --template mytemplate   # use custom template
snatch export --list-templates                  # list available templates

---
Extracting Content

Code blocks

snatch code <SESSION>                 # all code blocks
snatch code <SESSION> -l rust         # only Rust blocks
snatch code <SESSION> -l python       # only Python blocks
snatch code <SESSION> --assistant-only  # from assistant only
snatch code <SESSION> --user-only     # from user only
snatch code <SESSION> -n 5            # first 5 blocks
snatch code <SESSION> -m              # include metadata
snatch code <SESSION> -c              # concatenate all blocks
snatch code <SESSION> -f -O ./code/   # write each block to separate file
snatch code <SESSION> --main-thread   # exclude branches

User prompts

snatch prompts <SESSION>              # all prompts from a session
snatch prompts --all                  # all prompts across all sessions
snatch prompts --all -p myproject     # all prompts in a project
snatch prompts --all --since 1week    # prompts from last week
snatch prompts --all --exclude-system # filter out system messages
snatch prompts --all -g "implement"   # grep for pattern in prompts
snatch prompts --all -g "TODO" -i     # case-insensitive grep
snatch prompts --all -g "error" --invert-match  # exclude matches
snatch prompts --all -f               # frequency analysis (deduplicated counts)
snatch prompts --all -u               # unique prompts only
snatch prompts --all --stats          # prompt statistics summary
snatch prompts --all --contains "search online"  # count prompts with phrase
snatch prompts --all --timestamps     # include timestamps
snatch prompts --all --numbered       # numbered output
snatch prompts --all -n 50            # limit to 50 prompts
snatch prompts --all --min-length 50  # skip short prompts
snatch prompts --all -f --min-count 3 # only prompts appearing 3+ times
snatch prompts --all -f --sort-by-length  # sort by length not count
snatch prompts --all -f --no-truncate # full prompt text

Recover files from Write/Edit operations (alias: restore)

snatch recover <SESSION>              # recover last-written version of each file
snatch recover <SESSION> --preview    # preview without writing (alias: --dry-run)
snatch recover <SESSION> --apply-edits  # replay Edit operations for final state
snatch recover <SESSION> -f "*.rs"    # only Rust files
snatch recover <SESSION> -O ./recovered/  # output directory
snatch recover <SESSION> --strip-prefix /home/user/project  # relative paths
snatch recover <SESSION> --overwrite  # overwrite existing files
snatch recover <SESSION> --main-thread  # exclude branches

---
Statistics & Reporting

Usage stats (alias: stat)

snatch stats                          # global stats
snatch stats <SESSION>                # stats for one session
snatch stats -p myproject             # stats for a project
snatch stats -p proj1,proj2           # stats for multiple projects
snatch stats --global                 # explicit global
snatch stats -a                       # all detailed stats
snatch stats --tools                  # tool usage breakdown
snatch stats --models                 # model usage breakdown
snatch stats --costs                  # cost breakdown
snatch stats -b                       # 5-hour billing window blocks
snatch stats -b --token-limit 500000  # blocks with token limit line
snatch stats --sparkline              # sparkline visualizations
snatch stats --timeline               # activity timeline
snatch stats --timeline --granularity hourly  # hourly granularity
snatch stats --graph                  # token usage graph
snatch stats --graph --graph-width 80 # wider graph

Cost tracking

snatch stats --history                # historical cost data (30 days)
snatch stats --history --days 90      # last 90 days
snatch stats --weekly                 # weekly cost aggregation
snatch stats --monthly                # monthly cost aggregation
snatch stats --csv                    # export cost history as CSV
snatch stats --record                 # record current stats to history
snatch stats --clear-history          # clear all cost history

Quick summary

snatch summary                        # last 24 hours
snatch summary -p 7d                  # last 7 days
snatch summary -p 1w                  # last week

Standup report (alias: daily)

snatch standup                        # last 24 hours activity
snatch standup -p 1d                  # explicit 1-day period
snatch standup -p 7d --project myproj # weekly, one project
snatch standup --all                  # include usage, files, and tools
snatch standup --usage                # include token stats
snatch standup --files                # include file modifications
snatch standup --tools                # include tool breakdown
snatch standup -f markdown            # Markdown output (for Slack/Teams)
snatch standup -f json                # JSON output
snatch standup --clipboard            # copy to clipboard

Diff two sessions (alias: d)

snatch diff <SESSION1> <SESSION2>     # semantic diff
snatch diff <SESSION1> <SESSION2> -s  # summary only
snatch diff <SESSION1> <SESSION2> --line-based  # line-by-line JSONL diff
snatch diff <SESSION1> <SESSION2> --prompts     # compare user prompts only
snatch diff <SESSION1> <SESSION2> --type user --type assistant  # specific types
snatch diff <SESSION1> <SESSION2> -e  # exit code 1 if they differ

---
Organization & Tagging

Tags

snatch tag add bugfix -s <SESSION>         # tag a session
snatch tag add bugfix --since 1week        # bulk-tag recent sessions
snatch tag add bugfix --since 1week --preview  # preview bulk operation
snatch tag remove bugfix -s <SESSION>      # remove a tag
snatch tag list                            # list all tags
snatch tag list -s <SESSION>               # tags for a session
snatch tag find bugfix                     # sessions with tag "bugfix"

Names & bookmarks

snatch tag name <SESSION> "Auth refactor"  # name a session
snatch tag bookmark <SESSION>              # bookmark a session
snatch tag unbookmark <SESSION>            # remove bookmark
snatch tag bookmarks                       # list all bookmarks

Outcomes

snatch tag outcome <SESSION> success       # mark as success
snatch tag outcome <SESSION> partial       # mark as partial
snatch tag outcome <SESSION> failed        # mark as failed
snatch tag outcome <SESSION> abandoned     # mark as abandoned
snatch tag outcome <SESSION> clear         # remove outcome
snatch tag outcomes                        # list sessions by outcome

Notes & links

snatch tag note <SESSION> "Fixed the auth bug"  # add a note
snatch tag notes <SESSION>                 # list notes
snatch tag unnote <SESSION> 0              # remove note by index
snatch tag clear-notes <SESSION>           # clear all notes
snatch tag link <SESSION1> <SESSION2>      # link related sessions
snatch tag unlink <SESSION1> <SESSION2>    # unlink
snatch tag links <SESSION>                 # show linked sessions
snatch tag similar <SESSION>               # find similar sessions

---
Maintenance

Cleanup (alias: clean)

snatch cleanup --empty --preview      # preview empty session deletion
snatch cleanup --empty -y             # delete empty sessions (no prompt)
snatch cleanup --older-than 3months   # delete old sessions
snatch cleanup --older-than 1week -p myproject  # scoped cleanup
snatch cleanup --subagents --older-than 1week   # clean old subagents

Validate integrity

snatch validate <SESSION>             # validate one session
snatch validate --all                 # validate all sessions
snatch validate --all --schema        # check schema compatibility
snatch validate --all --unknown-fields  # report unknown fields
snatch validate --all --relationships # check parent-child relationships

Cache management

snatch cache stats                    # cache statistics
snatch cache clear                    # clear all cached data
snatch cache invalidate               # invalidate stale entries
snatch cache status                   # enable/disable caching

Configuration (alias: cfg)

snatch config show                    # show all config values
snatch config get <KEY>               # get a specific value
snatch config set <KEY> <VALUE>       # set a value
snatch config path                    # show config file path
snatch config init                    # initialize with defaults
snatch config reset                   # reset to defaults

---
Integration & Automation

Extract metadata (alias: ext)

snatch extract --all --pretty         # all data sources, pretty JSON
snatch extract --settings             # Claude Code settings
snatch extract --claude-md            # CLAUDE.md content
snatch extract --mcp                  # MCP configuration
snatch extract --commands             # custom commands
snatch extract --rules                # rules
snatch extract --hooks                # hooks configuration
snatch extract --file-history         # file history
snatch extract -p myproject           # scoped to project

MCP server mode

snatch serve-mcp                      # start MCP server

# Requires the mcp feature. Build with:
# cargo install claude-snatch --features mcp

Shell completions

snatch completions bash               # Bash completions
snatch completions zsh                # Zsh completions
snatch completions fish               # Fish completions
snatch completions powershell         # PowerShell completions
snatch completions elvish             # Elvish completions

# Install (example for bash):
snatch completions bash >> ~/.bashrc

Real-time watching

snatch watch <SESSION>                # watch a session for changes
snatch watch <SESSION> -f             # follow mode (like tail -f)
snatch watch <SESSION> -l             # live dashboard with stats
snatch watch --all                    # watch all active sessions
snatch watch <SESSION> --interval 1000  # 1-second polling (default: 500ms)

Built-in help (aliases: guide, examples)

snatch quickstart                     # overview guide
snatch quickstart explore             # exploring sessions
snatch quickstart export              # exporting conversations
snatch quickstart search              # searching sessions
snatch quickstart stats               # usage statistics
snatch quickstart tui                 # TUI browser
snatch quickstart workflows           # common recipes
snatch quickstart all                 # all topics

---
Common Workflows

# "What did I do yesterday?"
snatch standup -f markdown --clipboard

# "Find that session where I discussed authentication"
snatch search "authentication" -l | head -5

# "Export my last session as pretty HTML"
snatch recent -n 1 --json | jq -r '.[0].id' | xargs snatch export -f html --toc -O session.html

# "How much have I spent this month?"
snatch stats --costs --monthly

# "Recover a file I wrote in a session"
snatch recover <SESSION> -f "*.py" --preview

# "Bulk tag all sessions from this week"
snatch tag add sprint-42 --since 1week --preview

# "Get all my prompts for analysis"
snatch prompts --all --stats

# "Compare two approaches I tried"
snatch diff <SESSION1> <SESSION2> --prompts

---
Environment Variables

All flags have env var equivalents prefixed with SNATCH_:

┌──────────────────────┬───────────────────────┐
│       Variable       │        Purpose        │
├──────────────────────┼───────────────────────┤
│ SNATCH_CLAUDE_DIR    │ Claude directory path │
├──────────────────────┼───────────────────────┤
│ SNATCH_OUTPUT        │ Default output format │
├──────────────────────┼───────────────────────┤
│ SNATCH_JSON          │ Always output JSON    │
├──────────────────────┼───────────────────────┤
│ SNATCH_VERBOSE       │ Enable verbose        │
├──────────────────────┼───────────────────────┤
│ SNATCH_QUIET         │ Suppress output       │
├──────────────────────┼───────────────────────┤
│ SNATCH_COLOR         │ Force color on/off    │
├──────────────────────┼───────────────────────┤
│ SNATCH_THREADS       │ Thread count          │
├──────────────────────┼───────────────────────┤
│ SNATCH_LOG_LEVEL     │ Log level             │
├──────────────────────┼───────────────────────┤
│ SNATCH_LOG_FORMAT    │ Log format            │
├──────────────────────┼───────────────────────┤
│ SNATCH_LOG_FILE      │ Log destination       │
├──────────────────────┼───────────────────────┤
│ SNATCH_CONFIG        │ Config file path      │
├──────────────────────┼───────────────────────┤
│ SNATCH_MAX_FILE_SIZE │ File size limit       │
├──────────────────────┼───────────────────────┤
│ SNATCH_EXPORT_FORMAT │ Default export format │
├──────────────────────┼───────────────────────┤
│ SNATCH_TUI_THEME     │ TUI theme             │
├──────────────────────┼───────────────────────┤
│ SNATCH_ASCII         │ ASCII-only TUI mode   │
└──────────────────────┴───────────────────────┘
