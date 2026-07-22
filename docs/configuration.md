# Configuration Reference

snatch works out of the box; configuration is optional and uses sensible defaults.

Provider source roots are intentionally configured outside the TOML file. The
TOML schema controls display, caches, indexing, and budgets; source selection is
explicit through environment/global CLI options so commands cannot silently
switch corpora.

## Configuration Locations

Configuration is loaded and merged in this order (later overrides earlier):

1. Default values (lowest priority)
2. User config: `~/.config/claude-snatch/config.toml`
3. Project config: `.claude-snatch.toml` in the project directory
4. Command-line arguments (highest priority)

The user-config directory follows the platform convention (`$XDG_CONFIG_HOME` or
`~/.config` on Linux, `~/Library/Application Support` on macOS, `%APPDATA%` on
Windows), under `claude-snatch/config.toml`. Run `snatch config path` to print the
exact location.

## Provider sources and selection

| Provider | Default root | Override |
|----------|--------------|----------|
| `claude-code` | `~/.claude` | global `--claude-dir PATH` or `SNATCH_CLAUDE_DIR` |
| `codex` | `$CODEX_HOME`, otherwise `~/.codex` | set `CODEX_HOME` |

There is no `codex_root` TOML key or `--codex-dir` CLI flag. Embedded/library
callers can supply one with `provider::registry::RegistryConfig`.

Provider-aware commands accept repeatable `--provider` values:

```bash
snatch providers
snatch list sessions --provider codex
snatch list sessions --provider claude-code --provider codex
snatch list sessions --provider all
snatch info codex:<native-session-id>
```

Without `--provider`, an unqualified reference uses the classic Claude route.
A qualified id is itself an explicit provider selection. `--provider all` is
an explicit full-corpus scan: unavailable providers are reported with partial
results, while an explicitly named unavailable provider fails atomically.
Scope large union operations by provider, project, session, or date whenever
possible.

Qualified ids are `provider:native-id` for the global namespace and
`provider:namespace:native-id` for provider-local namespaces. Literal `%` and
`:` within segments are escaped; copy ids from `snatch list --provider ...`
rather than constructing them manually.

## Parsing safety limits

The global `--max-file-size BYTES` option (or `SNATCH_MAX_FILE_SIZE`) applies to
classic parsing and tightens every selected provider's own limits. It never
loosens a provider guard:

- omitted or `0` means no additional user cap;
- Claude files are bounded by the supplied nonzero value;
- Codex plain and decompressed streams are bounded by the supplied nonzero
  value in addition to built-in compressed/decompressed/window guards;
- the effective policy participates in provider cache revision tokens, so
  parses made under different limits cannot alias.

Codex defaults currently cap compressed input at 1 GiB, decompressed output at
4 GiB, and the zstd window at 128 MiB. These are safety ceilings, not promises
that a file near the ceiling will fit the configured in-memory cache.

## Build features

| Feature | Default | Purpose |
|---------|---------|---------|
| `codex` | yes | Codex rollout discovery and streaming zstd decode |
| `mcp` | no | stdio MCP server and its 19 tools |
| `mmap` | no | memory-mapped classic parsing for large JSONL files |
| `tracing` | no | additional tracing instrumentation |

Examples:

```bash
cargo build                              # default provider set
cargo build --no-default-features        # classic provider only
cargo build --all-features               # all providers + MCP + mmap + tracing
cargo install --path . --locked --all-features --force
```

After replacing an installed MCP-enabled binary, restart or reconnect the MCP
client; an already-running stdio server continues using its old process image.

## File Format

snatch uses TOML. Every section and key is optional; anything omitted falls back to
the default shown below.

### Example

```toml
# ~/.config/claude-snatch/config.toml

[display]
full_ids = false
show_sizes = true
truncate_at = 10000
context_lines = 2

[cache]
enabled = true
# directory = "/custom/cache/path"   # omitted = auto-detect
max_size = 104857600                 # bytes (100 MB)
ttl_seconds = 3600

[index]
# directory = "/custom/index/path"   # omitted = auto-detect

[budget]
# daily_limit = 5.00                 # USD; omitted = no limit
# weekly_limit = 25.00
# monthly_limit = 100.00
warning_threshold = 0.8              # warn at 80% of a limit
show_in_stats = true
```

## Configuration Sections

### `[display]`

| Key | Type | Default | Description |
|-----|------|---------|-------------|
| `full_ids` | bool | `false` | Show full session UUIDs instead of short prefixes |
| `show_sizes` | bool | `true` | Show file sizes |
| `truncate_at` | int | `10000` | Truncate long content at this many characters |
| `context_lines` | int | `2` | Context lines shown around search matches |

### `[cache]`

| Key | Type | Default | Description |
|-----|------|---------|-------------|
| `enabled` | bool | `true` | Enable session caching |
| `directory` | string | auto-detect | Cache directory (platform cache dir if unset) |
| `max_size` | int (bytes) | `104857600` | Maximum cache size, in bytes (100 MB) |
| `ttl_seconds` | int | `3600` | Cache entry lifetime, in seconds |

### `[index]`

| Key | Type | Default | Description |
|-----|------|---------|-------------|
| `directory` | string | auto-detect | Search-index directory. Read from the config file only — not exposed via `config set`. |

### `[budget]`

Cost alerts based on estimated spend (see `snatch stats`). Provider pricing is
capability-gated: a source without an applicable exact rate table reports cost
as unavailable rather than fabricating `$0` or applying a fallback model.

| Key | Type | Default | Description |
|-----|------|---------|-------------|
| `daily_limit` | float (USD) | unset | Daily spend limit; omit for no limit |
| `weekly_limit` | float (USD) | unset | Weekly spend limit |
| `monthly_limit` | float (USD) | unset | Monthly spend limit |
| `warning_threshold` | float | `0.8` | Warn when spend reaches this fraction of a limit (accepts `0`–`1`, or a percentage like `80`) |
| `show_in_stats` | bool | `true` | Show budget status in `stats` output |

## Project Configuration

Place a `.claude-snatch.toml` in a project directory to override the user config for
sessions in that project. It uses the same format and is merged on top of the user
config:

```toml
# .claude-snatch.toml

[display]
truncate_at = 3000

[budget]
monthly_limit = 50.00
```

## Managing Configuration

```bash
snatch config show                          # print effective configuration (alias: list)
snatch config get display.truncate_at       # read one value
snatch config set display.truncate_at 5000  # set a value
snatch config path                          # print the config file location
snatch config init                          # write a default config file
snatch config reset                         # reset to defaults
```

Keys accepted by `config get` / `config set`:

- `display.full_ids`, `display.show_sizes`, `display.truncate_at`, `display.context_lines`
- `cache.enabled`, `cache.directory`, `cache.max_size`, `cache.ttl_seconds`
- `budget.daily_limit`, `budget.weekly_limit`, `budget.monthly_limit`, `budget.warning_threshold`, `budget.show_in_stats`

`[index]` is read from the config file but is not exposed through `config set`.

The provider search index is partitioned by provider and stores a snapshot of
the selected corpus. Use explicit provider scope when building or searching:

```bash
snatch index build --provider codex
snatch index rebuild --provider all
snatch search "query" --provider all
```
