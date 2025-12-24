# Configuration Reference

snatch uses a layered configuration system with sensible defaults.

## Configuration Locations

Configuration is loaded from these locations (in order of priority):

1. Command-line arguments (highest priority)
2. Environment variables
3. Project-specific config: `.snatch/config.toml`
4. User config: `~/.config/snatch/config.toml`
5. Default values (lowest priority)

## Configuration File Format

snatch uses TOML format for configuration files.

### Example Configuration

```toml
# ~/.config/snatch/config.toml

[general]
# Default Claude directory (auto-detected if not set)
claude_dir = "~/.claude"

# Default output directory for exports
output_dir = "~/snatch-exports"

# Enable verbose logging
verbose = false

[tui]
# Default theme: "dark", "light", or "high_contrast"
theme = "dark"

# Show thinking blocks by default
show_thinking = true

# Show tool outputs by default
show_tools = true

# Enable word wrap by default
word_wrap = true

# Number of recent sessions to show
max_recent_sessions = 50

[export]
# Default export format
format = "markdown"

# Include thinking blocks in exports
include_thinking = true

# Include tool calls in exports
include_tools = true

# Export main thread only by default
main_thread_only = false

# Default line width for text export
text_width = 80

[cache]
# Enable session caching
enabled = true

# Cache directory
dir = "~/.cache/snatch"

# Maximum cache size in MB
max_size_mb = 100

# Cache entry TTL in seconds
ttl_seconds = 3600

[display]
# Date format for timestamps
date_format = "%Y-%m-%d %H:%M:%S"

# Truncate long content in previews
max_preview_length = 200

# Show token counts
show_tokens = true

# Show cost estimates
show_costs = true

[analytics]
# Default cost per 1M input tokens (USD)
input_token_cost = 3.00

# Default cost per 1M output tokens (USD)
output_token_cost = 15.00

[keybindings]
# Custom key bindings (TUI)
quit = "q"
up = "k"
down = "j"
left = "h"
right = "l"
select = "Enter"
back = "Escape"
```

## Environment Variables

All configuration options can be set via environment variables with the `SNATCH_` prefix:

| Variable | Description | Example |
|----------|-------------|---------|
| `SNATCH_CLAUDE_DIR` | Claude directory path | `~/.claude` |
| `SNATCH_OUTPUT_DIR` | Default output directory | `~/exports` |
| `SNATCH_THEME` | TUI theme | `dark` |
| `SNATCH_FORMAT` | Default export format | `markdown` |
| `SNATCH_CACHE_DIR` | Cache directory | `~/.cache/snatch` |
| `SNATCH_VERBOSE` | Enable verbose output | `true` |

```bash
# Example usage
export SNATCH_THEME=light
export SNATCH_FORMAT=json
snatch tui
```

## Configuration Sections

### `[general]`

| Key | Type | Default | Description |
|-----|------|---------|-------------|
| `claude_dir` | string | `~/.claude` | Path to Claude directory |
| `output_dir` | string | `.` | Default export output directory |
| `verbose` | bool | `false` | Enable verbose logging |

### `[tui]`

| Key | Type | Default | Description |
|-----|------|---------|-------------|
| `theme` | string | `dark` | Color theme |
| `show_thinking` | bool | `true` | Show thinking blocks |
| `show_tools` | bool | `true` | Show tool calls |
| `word_wrap` | bool | `true` | Enable word wrap |
| `max_recent_sessions` | int | `50` | Max sessions in tree |

### `[export]`

| Key | Type | Default | Description |
|-----|------|---------|-------------|
| `format` | string | `markdown` | Default export format |
| `include_thinking` | bool | `true` | Include thinking blocks |
| `include_tools` | bool | `true` | Include tool calls |
| `main_thread_only` | bool | `false` | Export main thread only |
| `text_width` | int | `80` | Line width for text |

### `[cache]`

| Key | Type | Default | Description |
|-----|------|---------|-------------|
| `enabled` | bool | `true` | Enable caching |
| `dir` | string | `~/.cache/snatch` | Cache directory |
| `max_size_mb` | int | `100` | Maximum cache size |
| `ttl_seconds` | int | `3600` | Cache entry lifetime |

### `[display]`

| Key | Type | Default | Description |
|-----|------|---------|-------------|
| `date_format` | string | `%Y-%m-%d %H:%M:%S` | Timestamp format |
| `max_preview_length` | int | `200` | Preview truncation |
| `show_tokens` | bool | `true` | Display token counts |
| `show_costs` | bool | `true` | Display cost estimates |

### `[analytics]`

| Key | Type | Default | Description |
|-----|------|---------|-------------|
| `input_token_cost` | float | `3.00` | Cost per 1M input tokens |
| `output_token_cost` | float | `15.00` | Cost per 1M output tokens |

### `[keybindings]`

Customize TUI key bindings:

| Key | Default | Description |
|-----|---------|-------------|
| `quit` | `q` | Exit application |
| `up` | `k` | Move up |
| `down` | `j` | Move down |
| `left` | `h` | Focus left |
| `right` | `l` | Focus right |
| `select` | `Enter` | Select item |
| `back` | `Escape` | Go back |

## Project-Specific Configuration

Create `.snatch/config.toml` in your project root for project-specific settings:

```toml
# .snatch/config.toml

[export]
# Always include tools for this project
include_tools = true

# Export to specific directory
output_dir = "./docs/conversations"

[display]
# Show more context in previews
max_preview_length = 500
```

## Configuration Precedence

When the same setting is defined in multiple places:

1. **Command-line arguments** override everything
2. **Environment variables** override config files
3. **Project config** overrides user config
4. **User config** overrides defaults

Example:

```bash
# Config file says format=markdown
# Environment says format=json
# Command line says --format html

# Result: HTML format is used
snatch export session --format html
```

## Validating Configuration

Check your configuration:

```bash
# Show effective configuration
snatch config show

# Validate configuration file
snatch config validate

# Show config file locations
snatch config paths
```

## Resetting Configuration

```bash
# Reset to defaults
snatch config reset

# Create default config file
snatch config init
```
