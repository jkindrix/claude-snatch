# snatch CLI Cheatsheet

Retrieve, analyze, and export Claude Code and Codex CLI session logs. Run
`snatch <command> --help` for the authoritative option list.

## Global options

| Option | Meaning |
|--------|---------|
| `-d, --claude-dir PATH` | Override the Claude data root |
| `-o, --output text|json|tsv|compact` | Select structured command output |
| `--json` | Shorthand for `--output json` |
| `-v, --verbose` | Enable verbose output |
| `-q, --quiet` | Suppress nonessential output |
| `-j, --threads N` | Set parallel worker count |
| `--config PATH` | Use a custom TOML config |
| `--max-file-size BYTES` | Tighten parse limits; `0` adds no user cap |
| `--log-level LEVEL` | `error`, `warn`, `info`, `debug`, or `trace` |
| `--log-format FORMAT` | `text`, `json`, `compact`, or `pretty` |

`CODEX_HOME` overrides the Codex data root. Otherwise snatch uses `~/.codex`.

## Provider model

```bash
snatch providers                         # availability, roots, capabilities
snatch providers --json                  # machine-readable status
snatch list sessions --provider codex    # one provider
snatch list sessions --provider all      # explicit union
snatch list sessions \
  --provider claude-code \
  --provider codex                       # explicit subset
```

Selection rules:

- Flagless unqualified references use the classic Claude-only route.
- A qualified id such as `codex:<id>` opts into the named provider.
- An unqualified prefix must be unique in the selected scope.
- Explicit providers fail atomically if one is unavailable.
- `all` may return partial results, but skipped providers are reported.
- Union scans can be expensive; narrow by provider/project/date/session first.

Qualified ids are `provider:native-id`, or
`provider:namespace:native-id` for a provider-local namespace. Copy them from
provider listings; literal delimiters inside segments use a strict escaped
form.

## Command aliases

```text
list: ls             info: i, show       pick: browse
search: s, find      stats: stat         standup: daily
diff: d              export: x           recover: restore
cleanup: clean       index: idx          config: cfg
extract: ext         quickstart: guide, examples
serve-mcp: mcp
```

`digest` has no `d` alias; `d` means `diff`.

## Discover and inspect

```bash
snatch list sessions                    # 50 recent Claude sessions
snatch list projects
snatch list sessions -p myproject -n 20
snatch list sessions --since 3days --active
snatch list sessions --tag bugfix --bookmarked
snatch list sessions --provider codex --full-ids
snatch list projects --provider all --hide-empty

snatch recent                           # shorthand for list -n 5
snatch recent -n 10 --provider codex

snatch info <SESSION>
snatch info codex:<SESSION>
snatch info <SESSION> --files
snatch info <SESSION> --tree
snatch info <SESSION> --raw

snatch pick
snatch pick -p myproject -a info
```

Provider metadata fields—name, tags, bookmark, outcome, notes, and links—are
joined by exact logical key in `list`, `recent`, and `info`.

## Read progressively

```bash
snatch digest <SESSION>                  # compact orientation
snatch chunks <SESSION>                  # prompt-boundary map
snatch timeline <SESSION> -l 30          # turn narrative

snatch messages <SESSION> -D overview
snatch messages <SESSION> -D conversation
snatch messages <SESSION> -D standard
snatch messages <SESSION> -D full

snatch messages <SESSION> --chunk 4 -D full
snatch messages <SESSION> --chunk 2-5 --errors-only -D full
snatch messages <SESSION> --offset 50 --limit 25
snatch messages <SESSION> --max-text-len 2000
```

Detail levels:

| Level | Output |
|-------|--------|
| `overview` | Prompt boundaries only |
| `conversation` | User and assistant prose; tool-only turns omitted |
| `standard` | Prose plus tool names |
| `full` | Tool details and persisted reasoning summaries |

## Search and thread

```bash
snatch search "pattern" -i -p myproject
snatch search "pattern" -s <SESSION>
snatch search "pattern" --thinking --tools
snatch search "pattern" --provider codex
snatch search "pattern" --provider all -p myproject --since 30days

snatch thread "decision|tradeoff" -p myproject
snatch thread "schema drift" --provider all --recent 100
snatch thread "migration" --provider codex --decisions-only --summary
```

Provider-aware search uses the committed provider-partitioned index:

```bash
snatch index build --provider codex
snatch index rebuild --provider all
snatch index status
snatch index clear
```

## Lineage and projects

```bash
snatch chain                             # classic Claude continuation chains
snatch chain --provider all              # typed continuation/fork/spawn graph
snatch chain --provider codex -p repo

snatch list projects --provider all
snatch standup --provider all --project repo --period 7d
snatch lessons --all --provider all -p repo
```

Cross-provider project grouping uses credential-free git remote, canonical git
root, cwd, then a session fallback. Activity views collapse typed
continuations and do not count fork-inherited history as new work.

## Analysis

```bash
snatch stats <SESSION> --all
snatch stats codex:<SESSION> --tools --models
snatch stats --global --blocks --sparkline

snatch lessons <SESSION>
snatch lessons <SESSION> --category errors
snatch lessons <SESSION> --category corrections

snatch health /path/to/project --provider all --since 30days
snatch priorities /path/to/project --provider all --since 30days
snatch file-history src/main.rs --provider all -p /path/to/project
snatch file-evolution src/main.rs /path/to/project --provider all
snatch context <SESSION> --message-id <UUID>
```

An unpriced provider reports cost as unavailable. It is never treated as zero
and never assigned a fallback model rate.

## Export

### Normalized formats

```bash
snatch export <SESSION>                          # Markdown to stdout
snatch export <SESSION> -f markdown -O out.md
snatch export <SESSION> -f json-pretty -O out.json
snatch export <SESSION> -f text -O out.txt
snatch export <SESSION> -f csv -O out.csv
snatch export <SESSION> -f html --toc --dark -O out.html
snatch export <SESSION> -f sqlite -O out.db
snatch export <SESSION> -f jsonl -O normalized.jsonl
```

Content controls:

```bash
snatch export <SESSION> --only prompts,assistant
snatch export <SESSION> --no-thinking --no-tool-results
snatch export <SESSION> --no-timestamps --no-usage
snatch export <SESSION> --system --metadata
snatch export <SESSION> --full
snatch export <SESSION> --redact security -O sanitized.md
snatch export <SESSION> --redact all --redact-preview
snatch export <SESSION> --warn-pii
```

Thinking, tool use, tool results, images, timestamps, and usage are on by
default. `--full` is the current name for the content-complete preset;
`--lossless` remains a deprecated alias but is not byte fidelity.

### Fidelity tiers

```bash
snatch export codex:<SESSION> -f raw-jsonl -O source.jsonl
snatch export codex:<SESSION> -f native -O preferred-artifact.bin
snatch export codex:<SESSION> -f archive -O session.bundle
```

| Format | Guarantee |
|--------|-----------|
| `jsonl` | Normalized, content-preserving; fields/wrappers can differ |
| `raw-jsonl` | Exact logical JSONL record stream |
| `native` | Exact bytes of the preferred artifact |
| `archive` | Lossless manifest plus every session artifact |

Source-fidelity tiers bypass filters and redaction.

### Batch export

```bash
snatch export --all -p myproject --since 1week -O ./exports/
snatch export --all --provider codex -f archive -O ./archives/ --progress
```

## Extract focused content

```bash
snatch code <SESSION> -l rust
snatch code <SESSION> --assistant-only --metadata
snatch prompts <SESSION>
snatch prompts --all -p myproject --since 1week --stats
```

## Qualified metadata

```bash
snatch tag add bugfix -s codex:<SESSION>
snatch tag remove bugfix -s codex:<SESSION>
snatch tag name codex:<SESSION> "Auth refactor"
snatch tag bookmark codex:<SESSION>
snatch tag outcome codex:<SESSION> success
snatch tag note codex:<SESSION> "Validated against source records"
snatch tag link codex:<SESSION> claude-code:<SESSION>

snatch tag --provider all list
snatch tag --provider all bookmarks
snatch tag --provider all outcomes
snatch tag links codex:<SESSION>
```

Bulk date/project tagging and similarity are currently Claude-only; provider
combinations without a typed contract refuse instead of being ignored.

## Validate and diagnose

```bash
snatch validate <SESSION>
snatch validate codex:<SESSION>
snatch validate --provider all --all

snatch doctor                             # classic Claude drift scan
snatch doctor --provider codex --all      # native provider vocabulary
snatch doctor --provider all --all --json
```

`validate` checks source and normalized integrity. `doctor` reports native
vocabulary/coverage drift. Preserved unknown records remain visible data and
are not silently discarded.

## Recover and live operations

```bash
snatch recover <SESSION> --preview
snatch recover <SESSION> --apply-edits -f "src/**/*.rs" --preview
snatch recover <SESSION> --apply-edits -O ./recovered --overwrite

snatch watch <SESSION>
snatch cleanup --empty --preview
snatch cache stats
snatch cache clear
```

`recover`/`restore`, `watch`, and `cleanup` are Claude-specific capability
commands and reject unsupported provider scope.

## Claude project-memory registries

```bash
snatch goals list -p myproject
snatch notes list -p myproject
snatch decisions list -p myproject
```

These registries live in Claude project-memory storage. They are not unified
session-log data and explicitly reject `codex`/`all` scope.

## Configuration and installation

```bash
snatch config show
snatch config get cache.max_size
snatch config set cache.max_size 209715200
snatch config path

./install.sh                              # current checkout when run locally
./install.sh --source                     # current remote main
cargo install --path . --locked --all-features --force
```

Restart/reconnect an MCP client after replacing the binary; a live stdio
subprocess does not reload itself.

## MCP server

```bash
snatch serve-mcp                          # alias: mcp
```

The MCP build exposes 19 tools. Provider-aware requests accept a provider
array and return qualified identities; goal/note/decision management remains
explicitly Claude-storage-scoped.

## Shell completions and built-in guide

```bash
snatch completions bash
snatch completions zsh
snatch completions fish
snatch completions powershell
snatch quickstart all
```

## Environment variables

| Variable | Purpose |
|----------|---------|
| `SNATCH_CLAUDE_DIR` | Claude data root |
| `CODEX_HOME` | Codex data root |
| `SNATCH_OUTPUT` | Default structured output format |
| `SNATCH_JSON` | JSON output toggle |
| `SNATCH_VERBOSE` / `SNATCH_QUIET` | Output verbosity |
| `SNATCH_THREADS` | Parallel worker count |
| `SNATCH_CONFIG` | Custom TOML config path |
| `SNATCH_MAX_FILE_SIZE` | Additional parse-size cap |
| `SNATCH_EXPORT_FORMAT` | Default export format |
| `SNATCH_LOG_LEVEL` / `SNATCH_LOG_FORMAT` / `SNATCH_LOG_FILE` | Logging |
