# Contributing Guide

Thank you for your interest in contributing to snatch! This guide will help you get started.

## Getting Started

### Prerequisites

- Rust 1.70+ (stable)
- Git
- A text editor or IDE with Rust support

### Setting Up Development Environment

```bash
# Clone the repository
git clone https://github.com/your-username/claude-snatch.git
cd claude-snatch

# Build the project
cargo build

# Run tests
cargo test

# Run with optimizations
cargo run --release -- --help
```

### IDE Setup

**VS Code:**
- Install rust-analyzer extension
- Install CodeLLDB for debugging

**IntelliJ/CLion:**
- Install Rust plugin
- Configure Cargo project

## Project Structure

```
claude-snatch/
├── src/
│   ├── lib.rs           # Library entry point
│   ├── main.rs          # CLI entry point
│   ├── cli/             # Command-line interface
│   ├── tui/             # Terminal UI
│   ├── model/           # Data model
│   ├── parser/          # JSONL parser
│   ├── reconstruction/  # Conversation tree
│   ├── export/          # Export formats
│   ├── discovery/       # File discovery
│   ├── extraction/      # Data extraction
│   ├── analytics/       # Statistics
│   ├── cache/           # Caching
│   ├── config/          # Configuration
│   └── error.rs         # Error types
├── tests/               # Integration tests
├── docs/                # Documentation
└── Cargo.toml           # Dependencies
```

## Development Workflow

### 1. Create a Branch

```bash
# Create a feature branch
git checkout -b feature/my-feature

# Or a bugfix branch
git checkout -b fix/issue-123
```

### 2. Make Changes

- Write code following the style guide
- Add tests for new functionality
- Update documentation as needed

### 3. Test Your Changes

```bash
# Run all tests
cargo test

# Run specific test
cargo test test_name

# Run with output
cargo test -- --nocapture

# Run clippy
cargo clippy

# Check formatting
cargo fmt --check
```

### 4. Commit Your Changes

Use conventional commit format:

```bash
# Features
git commit -m "feat(export): add XML export format"

# Bug fixes
git commit -m "fix(parser): handle empty content blocks"

# Documentation
git commit -m "docs: update TUI guide"

# Refactoring
git commit -m "refactor(model): simplify ContentBlock enum"

# Tests
git commit -m "test(analytics): add token counting tests"
```

### 5. Submit a Pull Request

- Push your branch
- Open a PR against `main`
- Fill in the PR template
- Wait for review

## Code Style

### Formatting

Use rustfmt with default settings:

```bash
cargo fmt
```

### Linting

Code should pass clippy:

```bash
cargo clippy -- -D warnings
```

### Naming Conventions

- Types: `PascalCase`
- Functions/methods: `snake_case`
- Constants: `SCREAMING_SNAKE_CASE`
- Modules: `snake_case`

### Documentation

- Public items should have doc comments
- Use `///` for item docs
- Use `//!` for module docs
- Include examples where helpful

```rust
/// Parses a JSONL file and returns log entries.
///
/// # Arguments
///
/// * `path` - Path to the JSONL file
///
/// # Returns
///
/// A vector of parsed log entries
///
/// # Errors
///
/// Returns an error if the file cannot be read or parsed
///
/// # Examples
///
/// ```
/// let entries = parser.parse_file("session.jsonl")?;
/// ```
pub fn parse_file(&mut self, path: &Path) -> Result<Vec<LogEntry>> {
    // ...
}
```

### Error Handling

- Use `Result` for fallible operations
- Use `thiserror` for error types
- Provide context with error messages
- Don't panic in library code

```rust
use crate::error::{Result, SnatchError};

fn process_file(path: &Path) -> Result<Data> {
    let content = std::fs::read_to_string(path)
        .map_err(|e| SnatchError::io("Failed to read file", e))?;

    serde_json::from_str(&content)
        .map_err(|e| SnatchError::parse("Invalid JSON", e))
}
```

## Adding Features

### Adding a New CLI Command

1. Create command module in `src/cli/commands/`
2. Define arguments with clap
3. Implement command logic
4. Register in `src/cli/mod.rs`
5. Add tests
6. Update documentation

### Adding a New Export Format

1. Create file in `src/export/`
2. Implement `Exporter` trait
3. Add to `ExportFormat` enum
4. Register in CLI and TUI
5. Add tests
6. Document format

### Adding a New Model Type

1. Add struct/enum in `src/model/`
2. Add serde attributes
3. Add to `LogEntry` if needed
4. Update parser tests
5. Update exporters if needed

## Testing

### Unit Tests

Place unit tests in the same file:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_simple() {
        let input = r#"{"type": "user"}"#;
        let result = parse(input);
        assert!(result.is_ok());
    }
}
```

### Integration Tests

Place in `tests/` directory:

```rust
// tests/export_tests.rs
use claude_snatch::export::*;

#[test]
fn test_markdown_export() {
    // ...
}
```

### Test Fixtures

Add test data to `tests/fixtures/`:

```
tests/fixtures/
├── sessions/
│   ├── simple.jsonl
│   ├── with_tools.jsonl
│   └── branching.jsonl
└── expected/
    └── simple.md
```

## Performance

### Benchmarks

Add benchmarks with criterion:

```rust
// benches/parsing.rs
use criterion::{criterion_group, criterion_main, Criterion};

fn parse_benchmark(c: &mut Criterion) {
    c.bench_function("parse_session", |b| {
        b.iter(|| parse_file("tests/fixtures/sessions/large.jsonl"))
    });
}

criterion_group!(benches, parse_benchmark);
criterion_main!(benches);
```

### Profiling

```bash
# Build with debug symbols
cargo build --release

# Profile with perf (Linux)
perf record ./target/release/snatch export large-session
perf report

# Profile with Instruments (macOS)
instruments -t "Time Profiler" ./target/release/snatch export large-session
```

## Release Process

1. Update version in `Cargo.toml`
2. Update CHANGELOG.md
3. Create release commit
4. Tag with version
5. Push tags
6. CI builds and publishes

## Getting Help

- Open an issue for bugs
- Start a discussion for questions
- Check existing issues before creating new ones

## Code of Conduct

- Be respectful and inclusive
- Provide constructive feedback
- Help others learn and grow
- Report unacceptable behavior

## License

By contributing, you agree that your contributions will be licensed under the MIT License.
