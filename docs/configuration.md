# Configuration Reference

snatch works out of the box; configuration is optional and uses sensible defaults.

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

Cost alerts based on estimated spend (see `snatch stats`).

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
