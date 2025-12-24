# TUI User Guide

The snatch TUI (Terminal User Interface) provides an interactive way to browse and explore Claude Code conversation logs.

## Launching the TUI

```bash
# Launch TUI browser
snatch tui

# Open a specific project
snatch tui --project /path/to/project

# Open a specific session
snatch tui --session abc12345

# Use a specific theme
snatch tui --theme dark
```

## Interface Layout

The TUI is divided into three panels:

```
┌─────────────────┬────────────────────────────┬──────────────────┐
│   Projects/     │                            │                  │
│   Sessions      │      Conversation          │     Details      │
│   (Tree View)   │         View               │     Panel        │
│                 │                            │                  │
└─────────────────┴────────────────────────────┴──────────────────┘
```

### Left Panel: Tree View
- Browse projects and sessions hierarchically
- Sessions are organized under their parent projects
- Subagent sessions are shown indented under their parent

### Center Panel: Conversation View
- Displays the selected session's conversation
- Shows user messages, assistant responses, tool calls, and thinking blocks
- Syntax highlighted code blocks
- Scrollable with keyboard or mouse

### Right Panel: Details
- Session analytics and statistics
- Message counts (user/assistant)
- Token usage (input/output)
- Tool invocation count
- Thinking block count
- Duration and cost estimates

## Keyboard Shortcuts

### Navigation

| Key | Action |
|-----|--------|
| `j` / `↓` | Move down |
| `k` / `↑` | Move up |
| `h` / `←` | Focus left panel |
| `l` / `→` | Focus right panel |
| `Enter` | Select/expand item |
| `Esc` | Go back |
| `1`, `2`, `3` | Focus panel 1, 2, or 3 directly |

### Scrolling

| Key | Action |
|-----|--------|
| `PageUp` | Scroll up 10 lines |
| `PageDown` | Scroll down 10 lines |
| `Home` | Scroll to top |
| `End` | Scroll to bottom |
| Mouse wheel | Scroll up/down |

### Search

| Key | Action |
|-----|--------|
| `/` | Start search |
| `n` | Next search result |
| `N` | Previous search result |
| `Enter` | Confirm search |
| `Esc` | Cancel search |

Search is case-insensitive by default and searches through all visible conversation content.

### Display Toggles

| Key | Action |
|-----|--------|
| `t` | Toggle thinking blocks |
| `o` | Toggle tool outputs |
| `w` | Toggle word wrap |
| `T` | Cycle theme (dark/light/high-contrast) |

### Filters

| Key | Action |
|-----|--------|
| `f` | Toggle filter panel |
| `F` | Cycle message type filter (All/User/Assistant/System/Tools) |
| `E` | Toggle errors-only filter |
| `[` | Set date-from filter (YYYY-MM-DD) |
| `]` | Set date-to filter (YYYY-MM-DD) |
| `X` | Clear all filters |

When entering a date filter, type the date in YYYY-MM-DD format and press Enter to confirm.

### Actions

| Key | Action |
|-----|--------|
| `r` | Refresh current view |
| `e` | Export session |
| `c` | Copy message to clipboard |
| `C` | Copy code block to clipboard |
| `?` | Toggle help overlay |
| `q` | Quit |

## Export Dialog

Press `e` to open the export dialog for the current session.

### Export Options

- **Format**: Use `h`/`l` or arrow keys to select format
  - Markdown
  - JSON / JSON (Pretty)
  - HTML
  - Plain Text
  - CSV
  - XML
  - SQLite

- **Include Thinking**: Toggle with `t`
- **Include Tools**: Toggle with `o`

Press `Enter` to export or `Esc` to cancel.

The exported file is saved to the current working directory with a name like `session_abc12345.md`.

## Themes

Three themes are available:

1. **Dark** (default): Dark background with vibrant colors
2. **Light**: Light background for bright environments
3. **High Contrast**: Maximum readability with stark contrasts

Cycle through themes with `T` or specify at launch:

```bash
snatch tui --theme light
```

## Agent Hierarchy

Sessions that spawn subagents are displayed hierarchically:

```
abc12345 (2 agents)
  └─ def67890 [agent]
  └─ ghi13579 [agent]
```

Select any session to view its conversation. Subagent sessions show their parent relationship in the tree.

## Mouse Support

- **Left click**: Select panel or item
- **Scroll wheel**: Scroll conversation up/down

## Tips

1. **Large sessions**: Use `PageUp`/`PageDown` for faster scrolling
2. **Finding code**: Use `/` to search for function names or keywords
3. **Focused reading**: Toggle off thinking blocks (`t`) and tool outputs (`o`) for cleaner view
4. **Quick export**: Press `e`, select format, press `Enter`
5. **Date filtering**: Use `[` and `]` to narrow down to specific date ranges
