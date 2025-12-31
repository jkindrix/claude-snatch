# Contributing to claude-snatch

Thank you for your interest in contributing to claude-snatch! This document provides guidelines and instructions for contributing.

## Table of Contents

- [Code of Conduct](#code-of-conduct)
- [Getting Started](#getting-started)
- [Development Setup](#development-setup)
- [Making Changes](#making-changes)
- [Testing](#testing)
- [Submitting Changes](#submitting-changes)
- [Style Guide](#style-guide)

## Code of Conduct

This project follows the [Rust Code of Conduct](https://www.rust-lang.org/policies/code-of-conduct). Please be respectful and constructive in all interactions.

## Getting Started

### Prerequisites

- Rust 1.75+ (see `rust-toolchain.toml` for exact version)
- Git
- A working Claude Code installation (for testing with real data)

### Fork and Clone

1. Fork the repository on GitHub
2. Clone your fork locally:
   ```bash
   git clone https://github.com/YOUR_USERNAME/claude-snatch.git
   cd claude-snatch
   ```
3. Add the upstream remote:
   ```bash
   git remote add upstream https://github.com/jkindrix/claude-snatch.git
   ```

## Development Setup

### Install Dependencies

```bash
# Install Rust (if not already installed)
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh

# Install development tools
cargo install cargo-deny cargo-tarpaulin cargo-insta

# Build the project
cargo build

# Run tests
cargo test
```

### Recommended Tools

- `cargo-watch` - Auto-rebuild on file changes
- `cargo-edit` - Add/remove dependencies easily
- `rust-analyzer` - IDE support

### IDE Setup

We recommend VS Code with rust-analyzer, or any editor with LSP support.

## Making Changes

### Branch Naming

Use descriptive branch names:
- `feat/add-pdf-export` - New features
- `fix/parser-unicode-handling` - Bug fixes
- `docs/update-readme` - Documentation
- `refactor/simplify-tree-builder` - Code improvements
- `test/add-proptest-coverage` - Test additions

### Commit Messages

Follow [Conventional Commits](https://www.conventionalcommits.org/):

```
type(scope): short description

Longer description if needed.

Fixes #123
```

Types:
- `feat` - New feature
- `fix` - Bug fix
- `docs` - Documentation only
- `style` - Formatting, no code change
- `refactor` - Code change that neither fixes a bug nor adds a feature
- `test` - Adding or fixing tests
- `chore` - Maintenance tasks

### Code Guidelines

1. **Safety First**: Never use `unsafe` code without explicit justification
2. **Error Handling**: Use `thiserror` for error types, provide context
3. **Documentation**: Add rustdoc comments to all public items
4. **Testing**: Add tests for new functionality
5. **Performance**: Consider memory usage for large files

## Testing

### Running Tests

```bash
# Run all tests
cargo test

# Run with all features
cargo test --all-features

# Run specific test
cargo test test_name

# Run with output
cargo test -- --nocapture

# Run benchmarks
cargo bench
```

### Test Categories

- **Unit tests**: In `src/*/mod.rs` with `#[cfg(test)]`
- **Integration tests**: In `tests/`
- **Property tests**: In `tests/proptest_parser.rs`
- **Snapshot tests**: In `tests/snapshot_exports.rs`

### Updating Snapshots

If you intentionally change export format output:

```bash
cargo insta review
```

### Code Coverage

```bash
cargo tarpaulin --out html --all-features
open tarpaulin-report.html
```

## Submitting Changes

### Before Submitting

1. **Format code**: `cargo fmt`
2. **Run lints**: `cargo clippy --all-targets --all-features`
3. **Run tests**: `cargo test --all-features`
4. **Check licenses**: `cargo deny check`
5. **Update docs**: If you changed public APIs

### Pull Request Process

1. Update your fork with upstream changes:
   ```bash
   git fetch upstream
   git rebase upstream/main
   ```

2. Push your branch:
   ```bash
   git push origin your-branch-name
   ```

3. Create a Pull Request with:
   - Clear title following commit conventions
   - Description of changes
   - Link to related issues
   - Screenshots (for TUI changes)

4. Address review feedback

### PR Checklist

- [ ] Code compiles without warnings
- [ ] All tests pass
- [ ] Clippy reports no errors
- [ ] cargo-deny checks pass
- [ ] Documentation updated (if applicable)
- [ ] CHANGELOG.md updated (for user-facing changes)
- [ ] Commit messages follow conventions

## Style Guide

### Rust Style

We use `rustfmt` with default settings. Run `cargo fmt` before committing.

### Clippy Lints

We enable `clippy::pedantic` and `clippy::nursery`. Fix all warnings.

### Documentation Style

```rust
/// Brief one-line description.
///
/// Longer description if needed, with details about behavior,
/// edge cases, and usage patterns.
///
/// # Examples
///
/// ```rust
/// use claude_snatch::parser::JsonlParser;
///
/// let mut parser = JsonlParser::new();
/// let entries = parser.parse_str(r#"{"type":"user"}"#)?;
/// ```
///
/// # Errors
///
/// Returns `SnatchError::Parse` if the input is invalid JSON.
///
/// # Panics
///
/// This function does not panic.
pub fn example_function() -> Result<()> {
    // ...
}
```

### Module Organization

```
src/
├── lib.rs          # Crate root, re-exports
├── error.rs        # Error types
├── model/          # Data structures
│   ├── mod.rs
│   ├── message.rs
│   └── content.rs
├── parser/         # Parsing logic
├── export/         # Export formats
└── ...
```

## Questions?

- Open a [Discussion](https://github.com/jkindrix/claude-snatch/discussions) for questions
- Open an [Issue](https://github.com/jkindrix/claude-snatch/issues) for bugs or feature requests
- See [SECURITY.md](SECURITY.md) for security-related concerns

Thank you for contributing!
