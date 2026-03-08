# Quality Profile: claude-snatch

> A Rust CLI and MCP server for extracting, analyzing, and recalling Claude Code conversation history — serving both human developers and AI agents.

Quality means the recalled data is accurate, the parser never loses information, errors guide the user to resolution, and the right command is findable in under 10 seconds.

## Priority Dimensions (ranked)

1. **Recall Accuracy** — The MCP server is the primary interface: AI agents make decisions based on returned session data. Inaccurate recall (wrong lessons, missing context, garbled timeline) causes downstream errors that compound silently. Every MCP tool must produce correct output from known inputs, verified by tests. When recall accuracy and any other dimension conflict, recall accuracy wins.

2. **Data Fidelity & Parse Robustness** — The parser is the foundation. If parsing drops data, truncates thinking blocks, or mislinks parent-child relationships, every downstream feature inherits the error. The parser must never panic on arbitrary input, must handle all known JSONL variants, and must preserve unknown fields for forward compatibility. Parsing bugs, once found, must become permanent regression tests.

3. **Error Guidance** — With 30+ commands and two audiences (humans and AI agents), errors must do three things: say what happened, say why, and suggest what to do next. "Session not found: abc123" is insufficient. "Session not found: abc123. Use 'snatch list sessions' to see available sessions, or use a shorter prefix." is the target. Errors are a user interface.

4. **CLI Discoverability** — A user (human or AI) should be able to find the right command for their task without reading source code. This means: logical grouping in `--help`, a README that reflects actual capabilities (including MCP server), and command descriptions that answer "when would I use this?" not just "what does this do?"

## Anti-Targets

- **TUI visual polish** — The TUI is a secondary interface. We'll sacrifice TUI aesthetics to invest in recall accuracy and CLI testing. The TUI needs to work, not be beautiful.
- **Export format breadth** — Nine export formats is sufficient. We'll sacrifice adding new formats to invest in testing the MCP pipeline and improving error messages.
- **Plugin/extension architecture** — The tool's value comes from deep, specific knowledge of Claude Code's JSONL format. We'll sacrifice extensibility to keep the codebase focused and the parser correct.
- **Broad platform support** — Claude Code targets Linux and macOS. We'll sacrifice Windows-native testing to maintain a simpler build and faster iteration cycle.

## Current State vs. Target

| Dimension | Current | Target | Key Gaps |
|-----------|---------|--------|----------|
| Recall Accuracy | Analysis module: 51 tests. MCP server module: 13 tests covering 10 tools (list_sessions, get_session_info, search_sessions, get_session_timeline, get_session_digest, get_session_lessons, get_stats, get_session_messages, get_project_history, get_tool_calls). Tests use temp Claude directory fixtures with realistic JSONL data. | Every MCP tool has at least one known-input/expected-output test. Analysis functions have regression tests for production bugs. | manage_goals, manage_notes, manage_decisions, tag_message untested. No end-to-end MCP pipeline tests (JSON-RPC over stdio). |
| Data Fidelity | Strong: property tests (1000+ cases), lenient mode, snapshot tests for 6 export formats, edge case suite (null bytes, deep nesting, 10MB lines). | Current approach plus a dedicated regression test file for parsing bugs, and `assert_cmd` tests for the actual `snatch` binary. | No `tests/regression.rs`. No CLI binary integration tests (testing `snatch export`, `snatch search` as a subprocess). |
| Error Guidance | 27 typed error variants with descriptive messages. Proper exit codes (2-130 range). `hint()` method on 12 variants returning actionable suggestions (SessionNotFound → "snatch list sessions", ProjectNotFound → "snatch list projects", etc.). Hints displayed in CLI error output. | Every user-facing error includes an actionable suggestion. Ambiguous session prefixes list matches. | No "did you mean?" for typos. Remaining 15 variants (mostly infrastructure: IoError, SerializationError, etc.) return None from hint(). No hint mechanism in MCP error responses. |
| CLI Discoverability | 30+ commands with good per-command `--help`. Short/long help split (`-h`/`--help`). Env var support for all global options. README documents MCP server setup, 14 tools, analysis commands, and updated architecture tree. | README documents all capabilities. Top-level `--help` groups commands by category. | No command grouping in `--help` output. |

## Exemplars Referenced

- **[ripgrep](https://github.com/BurntSushi/ripgrep)** — Demonstrated that every flag deserves purpose-built documentation (`doc_short`/`doc_long`), regression tests are the backbone of a robust parser, and non-fatal errors should continue gracefully while tracking aggregate error state.
- **[bat](https://github.com/sharkdp/bat)** — Demonstrated that error messages should include the tool name and context (`[bat error]: Error while parsing metadata.yaml`), integration tests should exercise the actual binary via `assert_cmd`, and error types should be compact and focused rather than exhaustive.
- **[atuin](https://github.com/atuinsh/atuin)** — Demonstrated that a history/recall tool should lead its README with the primary use case and a demo, organize into feature-focused crates, and keep the command surface area proportional to the user's mental model.
