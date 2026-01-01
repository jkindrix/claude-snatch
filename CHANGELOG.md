# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added
- Per-message token usage columns in SQLite export (input_tokens, output_tokens, cache_creation_tokens, cache_read_tokens)
- `is_sidechain` column for branch/subagent message detection in SQLite
- `agent_hash` column to uniquely identify subagent sessions
- Session metadata in SQLite exports (project_path, is_subagent, file_size, git_branch, git_commit)
- ThinkingMetadata columns in SQLite export (thinking_level, thinking_disabled, thinking_triggers)
- `snatch prompts` command for streamlined prompt extraction
- Session outcome tagging with `snatch tag outcome`
- Multi-session merged SQLite export with `--all` flag
- Comprehensive TUI test coverage (events, state, components, theme, highlight)
- `snatch quickstart` command with interactive help for new users
- TUI focus mode (`z` key) for distraction-free conversation reading
- TUI pagination indicator showing line position and entry count
- TUI line numbers toggle (`#` key)
- TUI help panel scroll support for long help text
- `--redact-preview` flag to preview redactions without applying them

### Changed
- Standardized `--subagents` flag across all commands (was `--include-agents` in some)
- `--main-thread` now defaults to false (exports all entries by default)

### Fixed
- Export data loss issue where only 13-75% of entries were captured
- Empty sessions appearing in SQLite multi-session exports
- Tool results extraction from user messages to SQLite

## [0.1.0] - 2025-12-30

### Added

#### Core Features
- Complete JSONL parser supporting all 7 Claude Code message types
- Conversation tree reconstruction with branch detection
- Session discovery across all Claude Code project directories
- Support for Claude Code schema versions 2.0.x

#### Export Formats
- **Markdown** - Human-readable with collapsible thinking blocks
- **JSON** - Structured export with analytics metadata
- **JSON Pretty** - Formatted JSON for readability
- **JSONL** - Original format preservation
- **HTML** - Styled output with inline images and table of contents
- **CSV** - Tabular format for spreadsheet analysis
- **XML** - Structured markup for data interchange
- **Text** - Plain text with configurable wrapping
- **SQLite** - Queryable database with full-text search

#### CLI Commands
- `snatch list` - Browse sessions with filtering and sorting
- `snatch export` - Export to 9 formats with extensive options
- `snatch search` - Full-text search with regex and fuzzy matching
- `snatch info` - Session and project metadata display
- `snatch stats` - Usage analytics and cost estimation
- `snatch diff` - Compare sessions or conversation versions
- `snatch validate` - JSONL integrity checking
- `snatch watch` - Real-time session monitoring
- `snatch extract` - Beyond-JSONL data extraction
- `snatch cleanup` - Session management and pruning
- `snatch cache` - Cache management utilities
- `snatch index` - Session indexing for fast search
- `snatch config` - Configuration management
- `snatch tag` - Session tagging and bookmarking
- `snatch prompts` - Bulk prompt extraction

#### Terminal User Interface (TUI)
- Interactive session browser with keyboard navigation
- Message filtering by type (user, assistant, system, tool)
- Thinking block expansion/collapse
- Tool call visualization with diff view for edits
- Full-text search within sessions
- Command palette (Ctrl+P)
- Go-to-line navigation (Ctrl+G)
- Resume session in Claude Code (R key)
- Open in external editor (O key)
- Theme support (dark, light, high-contrast)
- ASCII fallback mode for limited terminals

#### Analytics
- Token usage tracking with cost estimation
- Model-specific pricing (Opus, Sonnet, Haiku)
- Cache efficiency metrics
- Tool success/failure rates
- Thinking time analysis
- Session duration calculations
- File modification tracking
- Usage trend analysis

#### Search Capabilities
- Full-text search across all sessions
- Regular expression support
- Fuzzy matching with configurable threshold
- Git branch filtering
- Date range filtering
- Result ranking and relevance scoring
- Context lines display

#### Data Extraction
- File backup history with version reconstruction
- MCP server configuration parsing
- Claude settings extraction
- Custom commands discovery
- Project rules parsing

#### Performance
- Streaming parser for large files (tested to 1GB+)
- LRU cache with configurable size
- Background indexing support
- Parallel session processing
- Zero-copy line iteration

#### Configuration
- TOML configuration file support
- Per-project configuration overrides
- Environment variable support
- Custom config file path option

#### Privacy & Compliance
- PII detection warnings during export
- Data minimization options for sharing
- GDPR-compliant export enhancements
- Configurable redaction levels
- SPDX license attribution

#### Developer Experience
- Comprehensive error handling with BSD exit codes
- Structured logging with configurable levels
- Shell completion generation (bash, zsh, fish, PowerShell)
- Programmatic API for library usage
- Extensive rustdoc documentation

### Security
- No unsafe code (`#![forbid(unsafe_code)]`)
- Clippy pedantic + nursery lints enabled
- cargo-deny for dependency auditing

---

## Version History Summary

| Version | Date | Highlights |
|---------|------|------------|
| 0.1.0 | 2025-12-30 | Initial release with full feature set |

[Unreleased]: https://github.com/jkindrix/claude-snatch/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/jkindrix/claude-snatch/releases/tag/v0.1.0
