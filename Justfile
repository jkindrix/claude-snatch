# ═══════════════════════════════════════════════════════════════════════════════
# claude-snatch Justfile
# ═══════════════════════════════════════════════════════════════════════════════
#
# High-performance CLI/TUI tool for extracting and analyzing Claude Code
# conversation logs with maximum data fidelity.
#
# Usage:
#   just              - Show all available commands
#   just setup        - First-time development setup
#   just build        - Build debug
#   just ci           - Run full CI pipeline
#   just release-check - Validate release readiness
#   just <recipe>     - Run any recipe
#
# Requirements:
#   - Just >= 1.23.0 (for [group], [confirm], [doc] attributes)
#   - Rust toolchain (rustup recommended)
#
# Install Just:
#   cargo install just
#   # or: brew install just / apt install just / pacman -S just
#
# ═══════════════════════════════════════════════════════════════════════════════

# ─────────────────────────────────────────────────────────────────────────────────
# Project Configuration
# ─────────────────────────────────────────────────────────────────────────────────

project_name := "claude-snatch"
binary_name := "snatch"
# Version is read dynamically from Cargo.toml to avoid drift
version := `cargo metadata --no-deps --format-version 1 2>/dev/null | jq -r '.packages[] | select(.name == "claude-snatch") | .version' 2>/dev/null || grep '^version' Cargo.toml | head -1 | sed 's/.*"\(.*\)"/\1/'`
msrv := "1.75.0"

# Feature configurations
features_all := "tui,tracing,mmap"

# ─────────────────────────────────────────────────────────────────────────────────
# Tool Configuration (can be overridden via environment)
# ─────────────────────────────────────────────────────────────────────────────────

cargo := env_var_or_default("CARGO", "cargo")

# Parallel jobs: auto-detect CPU count
jobs := env_var_or_default("JOBS", num_cpus())

# Runtime configuration
rust_log := env_var_or_default("RUST_LOG", "info")
rust_backtrace := env_var_or_default("RUST_BACKTRACE", "1")

# ─────────────────────────────────────────────────────────────────────────────────
# Platform Detection
# ─────────────────────────────────────────────────────────────────────────────────

platform := if os() == "linux" { "linux" } else if os() == "macos" { "macos" } else { "windows" }
open_cmd := if os() == "linux" { "xdg-open" } else if os() == "macos" { "open" } else { "start" }

# ─────────────────────────────────────────────────────────────────────────────────
# ANSI Color Codes
# ─────────────────────────────────────────────────────────────────────────────────

reset := '\033[0m'
bold := '\033[1m'
dim := '\033[2m'

red := '\033[31m'
green := '\033[32m'
yellow := '\033[33m'
blue := '\033[34m'
magenta := '\033[35m'
cyan := '\033[36m'
white := '\033[37m'

# ─────────────────────────────────────────────────────────────────────────────────
# Default Recipe & Settings
# ─────────────────────────────────────────────────────────────────────────────────

# Show help by default
default:
    @just --list --unsorted

# Load .env file if present
set dotenv-load

# Use bash with strict mode for safer scripts
# -e: Exit on error
# -u: Error on undefined variables
# -o pipefail: Fail on pipe errors
set shell := ["bash", "-euo", "pipefail", "-c"]

# Export all variables to child processes
set export

# ═══════════════════════════════════════════════════════════════════════════════
# SETUP RECIPES
# Bootstrap development environment
# ═══════════════════════════════════════════════════════════════════════════════

[group('setup')]
[doc("First-time environment setup with verification")]
bootstrap:
    #!/usr/bin/env bash
    set -euo pipefail
    printf '{{blue}}{{bold}}╔════════════════════════════════════════╗{{reset}}\n'
    printf '{{blue}}{{bold}}║   {{project_name}} Development Bootstrap  ║{{reset}}\n'
    printf '{{blue}}{{bold}}╚════════════════════════════════════════╝{{reset}}\n\n'

    # Check prerequisites
    printf '{{cyan}}[1/5]{{reset}} Checking prerequisites...\n'
    if ! command -v rustup &> /dev/null; then
        printf '{{red}}[ERR]{{reset}}  rustup not found. Install from https://rustup.rs\n'
        exit 1
    fi
    if ! command -v git &> /dev/null; then
        printf '{{red}}[ERR]{{reset}}  git not found. Please install git first.\n'
        exit 1
    fi
    printf '{{green}}[OK]{{reset}}   Prerequisites satisfied\n'

    # Setup Rust toolchain
    printf '\n{{cyan}}[2/5]{{reset}} Setting up Rust toolchain...\n'
    rustup toolchain install stable --profile default
    rustup toolchain install nightly --profile minimal
    rustup component add rustfmt clippy llvm-tools-preview
    rustup component add --toolchain nightly rustfmt
    printf '{{green}}[OK]{{reset}}   Rust toolchain ready\n'

    # Install minimal tools for development
    printf '\n{{cyan}}[3/5]{{reset}} Installing development tools...\n'
    {{cargo}} install cargo-nextest cargo-llvm-cov cargo-deny cargo-audit 2>/dev/null || true
    {{cargo}} install cargo-semver-checks cargo-watch typos-cli cargo-machete 2>/dev/null || true
    printf '{{green}}[OK]{{reset}}   Development tools installed\n'

    # Setup git hooks
    printf '\n{{cyan}}[4/5]{{reset}} Setting up git hooks...\n'
    if [ -d .git ]; then
        echo '#!/bin/sh
just pre-commit' > .git/hooks/pre-commit
        chmod +x .git/hooks/pre-commit
        printf '{{green}}[OK]{{reset}}   Git hooks configured\n'
    else
        printf '{{yellow}}[SKIP]{{reset}} Not a git repository\n'
    fi

    # Verify setup
    printf '\n{{cyan}}[5/5]{{reset}} Verifying setup...\n'
    {{cargo}} check --all-features -j {{jobs}}
    printf '{{green}}[OK]{{reset}}   Build verification passed\n'

    printf '\n{{green}}{{bold}}╔════════════════════════════════════════╗{{reset}}\n'
    printf '{{green}}{{bold}}║   Bootstrap Complete!                  ║{{reset}}\n'
    printf '{{green}}{{bold}}╚════════════════════════════════════════╝{{reset}}\n\n'
    printf '{{cyan}}Next steps:{{reset}}\n'
    printf '  just build      Build the project\n'
    printf '  just test       Run tests\n'
    printf '  just ci         Run full CI locally\n'
    printf '  just help       Show all commands\n\n'

[group('setup')]
[doc("Full development setup (rust + tools + hooks)")]
setup: setup-rust setup-tools setup-hooks
    @printf '{{green}}{{bold}}✓ Development environment ready{{reset}}\n'

[group('setup')]
[doc("Install/update Rust toolchain components")]
setup-rust:
    #!/usr/bin/env bash
    printf '{{blue}}{{bold}}Installing Rust toolchain...{{reset}}\n'
    rustup toolchain install stable --profile default
    rustup toolchain install nightly --profile minimal
    rustup component add rustfmt clippy llvm-tools-preview
    rustup component add --toolchain nightly rustfmt
    printf '{{green}}[OK]{{reset}}   Rust toolchain ready\n'

[group('setup')]
[doc("Install development tools (cargo extensions)")]
setup-tools:
    #!/usr/bin/env bash
    printf '{{blue}}{{bold}}Installing development tools...{{reset}}\n'
    # Core tools (required for CI)
    {{cargo}} install cargo-nextest cargo-llvm-cov cargo-deny cargo-audit
    # Release tools
    {{cargo}} install cargo-semver-checks git-cliff
    # Quality tools
    {{cargo}} install cargo-machete typos-cli cargo-outdated
    # Development tools
    {{cargo}} install cargo-watch cargo-insta
    printf '{{green}}[OK]{{reset}}   Tools installed\n'

[group('setup')]
[doc("Install minimal tools for CI/release checks")]
setup-tools-minimal:
    #!/usr/bin/env bash
    printf '{{blue}}{{bold}}Installing minimal tools...{{reset}}\n'
    {{cargo}} install cargo-deny cargo-audit cargo-semver-checks cargo-nextest
    printf '{{green}}[OK]{{reset}}   Minimal tools installed\n'

[group('setup')]
[doc("Install git hooks")]
setup-hooks:
    #!/usr/bin/env bash
    printf '{{blue}}{{bold}}Setting up git hooks...{{reset}}\n'
    if [ -d .git ]; then
        echo '#!/bin/sh
just pre-commit' > .git/hooks/pre-commit
        chmod +x .git/hooks/pre-commit
        printf '{{green}}[OK]{{reset}}   Pre-commit hook installed\n'
    else
        printf '{{yellow}}[WARN]{{reset}} Not a git repository, skipping hooks\n'
    fi

[group('setup')]
[doc("Check which development tools are installed")]
check-tools:
    #!/usr/bin/env bash
    printf '\n{{bold}}Development Tool Status{{reset}}\n'
    printf '═══════════════════════════════════════\n'

    check_tool() {
        if command -v "$1" &> /dev/null; then
            printf '{{green}}✓{{reset}} %s\n' "$1"
        else
            printf '{{red}}✗{{reset}} %s (not installed)\n' "$1"
        fi
    }

    # Core tools
    printf '\n{{cyan}}Core:{{reset}}\n'
    printf '  '; rustc --version
    printf '  '; cargo --version
    check_tool "rustfmt"
    check_tool "clippy-driver"

    # Cargo extensions
    printf '\n{{cyan}}Cargo Extensions:{{reset}}\n'
    for tool in nextest llvm-cov audit deny semver-checks machete; do
        if {{cargo}} $tool --version &> /dev/null 2>&1; then
            printf '{{green}}✓{{reset}} cargo-%s\n' "$tool"
        else
            printf '{{red}}✗{{reset}} cargo-%s\n' "$tool"
        fi
    done

    # External tools
    printf '\n{{cyan}}External:{{reset}}\n'
    check_tool "typos"
    check_tool "git-cliff"
    check_tool "lychee"

    printf '\n'

# ═══════════════════════════════════════════════════════════════════════════════
# BUILD RECIPES
# Compilation and build targets
# ═══════════════════════════════════════════════════════════════════════════════

[group('build')]
[doc("Build in debug mode")]
build:
    #!/usr/bin/env bash
    printf '{{blue}}{{bold}}Building (debug)...{{reset}}\n'
    {{cargo}} build --all-features -j {{jobs}}
    printf '{{green}}[OK]{{reset}}   Build complete\n'

[group('build')]
[doc("Build in release mode with optimizations")]
build-release:
    #!/usr/bin/env bash
    printf '{{blue}}{{bold}}Building (release)...{{reset}}\n'
    {{cargo}} build --all-features --release -j {{jobs}}
    printf '{{green}}[OK]{{reset}}   Release build complete\n'

[group('build')]
[doc("Fast type check without code generation")]
check-build:
    #!/usr/bin/env bash
    printf '{{cyan}}[INFO]{{reset}} Type checking...\n'
    {{cargo}} check --all-features -j {{jobs}}
    printf '{{green}}[OK]{{reset}}   Type check passed\n'

[group('build')]
[doc("Build with specific features")]
build-features features:
    #!/usr/bin/env bash
    printf '{{cyan}}[INFO]{{reset}} Building with features: {{features}}...\n'
    {{cargo}} build --features "{{features}}" -j {{jobs}}
    printf '{{green}}[OK]{{reset}}   Build complete\n'

[group('build')]
[doc("Analyze build times")]
build-timing:
    #!/usr/bin/env bash
    printf '{{cyan}}[INFO]{{reset}} Building with timing analysis...\n'
    {{cargo}} build --all-features --timings
    printf '{{green}}[OK]{{reset}}   Build timing report: target/cargo-timings/\n'

[group('build')]
[doc("Cross-compile for a target platform (requires cross)")]
build-cross target:
    #!/usr/bin/env bash
    printf '{{cyan}}[INFO]{{reset}} Cross-compiling for {{target}}...\n'
    if ! command -v cross &> /dev/null; then
        printf '{{yellow}}[INFO]{{reset}} Installing cross...\n'
        {{cargo}} install cross
    fi
    cross build --release --target {{target}}
    printf '{{green}}[OK]{{reset}}   Cross-compilation complete: target/{{target}}/release/{{binary_name}}\n'

[group('build')]
[doc("Build static Linux binary (musl)")]
build-static:
    #!/usr/bin/env bash
    printf '{{cyan}}[INFO]{{reset}} Building static binary (musl)...\n'
    if ! rustup target list --installed | grep -q x86_64-unknown-linux-musl; then
        printf '{{cyan}}[INFO]{{reset}} Adding musl target...\n'
        rustup target add x86_64-unknown-linux-musl
    fi
    {{cargo}} build --release --target x86_64-unknown-linux-musl
    printf '{{green}}[OK]{{reset}}   Static binary: target/x86_64-unknown-linux-musl/release/{{binary_name}}\n'

[group('build')]
[confirm("This will delete all build artifacts. Continue?")]
[doc("Clean all build artifacts")]
clean:
    #!/usr/bin/env bash
    printf '{{yellow}}Cleaning build artifacts...{{reset}}\n'
    {{cargo}} clean
    rm -rf coverage/ lcov.info *.profraw *.profdata
    printf '{{green}}[OK]{{reset}}   Clean complete\n'

[group('build')]
[doc("Clean and rebuild from scratch")]
rebuild: clean build

# ═══════════════════════════════════════════════════════════════════════════════
# TEST RECIPES
# Testing and quality assurance
# ═══════════════════════════════════════════════════════════════════════════════

[group('test')]
[doc("Run all tests")]
test:
    #!/usr/bin/env bash
    printf '{{blue}}{{bold}}Running tests...{{reset}}\n'
    {{cargo}} test --all-features -j {{jobs}}
    printf '{{green}}[OK]{{reset}}   All tests passed\n'

[group('test')]
[doc("Run tests with locked dependencies (reproducible)")]
test-locked:
    #!/usr/bin/env bash
    printf '{{blue}}{{bold}}Running tests (locked)...{{reset}}\n'
    {{cargo}} test --all-features --locked -j {{jobs}}
    printf '{{green}}[OK]{{reset}}   All tests passed (locked)\n'

[group('test')]
[doc("Run tests with cargo-nextest (faster, parallel)")]
nextest:
    #!/usr/bin/env bash
    printf '{{blue}}{{bold}}Running tests (nextest)...{{reset}}\n'
    {{cargo}} nextest run --all-features -j {{jobs}}
    printf '{{green}}[OK]{{reset}}   All tests passed\n'

[group('test')]
[doc("Run tests with output visible")]
test-verbose:
    #!/usr/bin/env bash
    printf '{{blue}}{{bold}}Running tests (verbose)...{{reset}}\n'
    {{cargo}} test --all-features -j {{jobs}} -- --nocapture
    printf '{{green}}[OK]{{reset}}   All tests passed\n'

[group('test')]
[doc("Test specific pattern")]
test-filter pattern:
    #!/usr/bin/env bash
    printf '{{cyan}}[INFO]{{reset}} Running tests matching: {{pattern}}\n'
    {{cargo}} test --all-features -- {{pattern}} --nocapture
    printf '{{green}}[OK]{{reset}}   Filtered tests complete\n'

[group('test')]
[doc("Run documentation tests only")]
test-doc:
    #!/usr/bin/env bash
    printf '{{cyan}}[INFO]{{reset}} Running doc tests...\n'
    {{cargo}} test --all-features --doc
    printf '{{green}}[OK]{{reset}}   Doc tests passed\n'

[group('test')]
[doc("Run tests without default features")]
test-minimal:
    #!/usr/bin/env bash
    printf '{{cyan}}[INFO]{{reset}} Running tests (no default features)...\n'
    {{cargo}} test --no-default-features -j {{jobs}}
    printf '{{green}}[OK]{{reset}}   Minimal tests passed\n'

[group('test')]
[doc("Run tests with various feature combinations")]
test-features:
    #!/usr/bin/env bash
    printf '{{blue}}{{bold}}Testing feature matrix...{{reset}}\n'
    printf '{{cyan}}[INFO]{{reset}} Testing with no features...\n'
    {{cargo}} test --no-default-features -j {{jobs}}
    printf '{{cyan}}[INFO]{{reset}} Testing with tui feature...\n'
    {{cargo}} test --no-default-features --features tui -j {{jobs}}
    printf '{{cyan}}[INFO]{{reset}} Testing with mmap feature...\n'
    {{cargo}} test --no-default-features --features mmap -j {{jobs}}
    printf '{{cyan}}[INFO]{{reset}} Testing with all features...\n'
    {{cargo}} test --all-features -j {{jobs}}
    printf '{{green}}[OK]{{reset}}   Feature matrix tests passed\n'

[group('test')]
[doc("Update insta snapshots")]
test-update-snapshots:
    #!/usr/bin/env bash
    printf '{{cyan}}[INFO]{{reset}} Updating insta snapshots...\n'
    {{cargo}} insta test --all-features
    {{cargo}} insta review
    printf '{{green}}[OK]{{reset}}   Snapshots updated\n'

# ═══════════════════════════════════════════════════════════════════════════════
# LINT RECIPES
# Code quality and style checks
# ═══════════════════════════════════════════════════════════════════════════════

[group('lint')]
[doc("Run clippy lints (matches CI configuration)")]
clippy:
    #!/usr/bin/env bash
    printf '{{cyan}}[INFO]{{reset}} Running clippy...\n'
    {{cargo}} clippy --all-features --all-targets -- -D warnings
    printf '{{green}}[OK]{{reset}}   Clippy passed\n'

[group('lint')]
[doc("Run clippy with strict pedantic lints")]
clippy-strict:
    #!/usr/bin/env bash
    printf '{{cyan}}[INFO]{{reset}} Running clippy (strict)...\n'
    {{cargo}} clippy --all-targets --all-features -- \
        -D warnings \
        -D clippy::all \
        -D clippy::pedantic \
        -D clippy::nursery \
        -A clippy::module_name_repetitions \
        -A clippy::too_many_lines
    printf '{{green}}[OK]{{reset}}   Clippy (strict) passed\n'

[group('lint')]
[doc("Auto-fix clippy warnings")]
clippy-fix:
    #!/usr/bin/env bash
    printf '{{cyan}}[INFO]{{reset}} Auto-fixing clippy warnings...\n'
    {{cargo}} clippy --all-targets --all-features --fix --allow-dirty --allow-staged
    printf '{{green}}[OK]{{reset}}   Clippy fixes applied\n'

[group('lint')]
[doc("Check code formatting")]
fmt-check:
    #!/usr/bin/env bash
    printf '{{cyan}}[INFO]{{reset}} Checking format...\n'
    {{cargo}} fmt --all -- --check
    printf '{{green}}[OK]{{reset}}   Format check passed\n'

[group('lint')]
[doc("Format all code")]
fmt:
    #!/usr/bin/env bash
    printf '{{cyan}}[INFO]{{reset}} Formatting code...\n'
    {{cargo}} fmt --all
    printf '{{green}}[OK]{{reset}}   Formatting complete\n'

[group('lint')]
[doc("Run typos spell checker")]
typos:
    #!/usr/bin/env bash
    printf '{{cyan}}[INFO]{{reset}} Checking for typos...\n'
    if ! command -v typos &> /dev/null; then
        printf '{{yellow}}[WARN]{{reset}} typos not installed (cargo install typos-cli)\n'
        exit 0
    fi
    typos src/ tests/ docs/ README.md CHANGELOG.md RELEASING.md 2>/dev/null || true
    printf '{{green}}[OK]{{reset}}   Typos check passed\n'

[group('lint')]
[doc("Fix typos automatically")]
typos-fix:
    #!/usr/bin/env bash
    printf '{{cyan}}[INFO]{{reset}} Fixing typos...\n'
    typos --write-changes
    printf '{{green}}[OK]{{reset}}   Typos fixed\n'

[group('lint')]
[doc("Check markdown links (requires lychee)")]
link-check:
    #!/usr/bin/env bash
    set -e
    printf '{{cyan}}[INFO]{{reset}} Checking markdown links...\n'
    if ! command -v lychee &> /dev/null; then
        printf '{{yellow}}[WARN]{{reset}} lychee not installed (cargo install lychee)\n'
        printf '{{yellow}}[WARN]{{reset}} Skipping link check\n'
        exit 0
    fi
    lychee --no-progress --accept 200,204,206 \
        --exclude '^https://crates.io' \
        --exclude '^https://docs.rs' \
        './docs/**/*.md' './README.md' './CONTRIBUTING.md' './RELEASING.md' 2>/dev/null || true
    printf '{{green}}[OK]{{reset}}   Link check passed\n'

[group('lint')]
[doc("Find unused dependencies via cargo-machete (fast, heuristic)")]
machete:
    #!/usr/bin/env bash
    printf '{{cyan}}[INFO]{{reset}} Finding unused dependencies...\n'
    {{cargo}} machete
    printf '{{green}}[OK]{{reset}}   Machete check complete\n'

[group('lint')]
[doc("Run all lints (fmt + clippy + typos)")]
lint: fmt-check clippy typos
    @printf '{{green}}[OK]{{reset}}   All lints passed\n'

[group('lint')]
[doc("Run comprehensive lint suite")]
lint-full: fmt-check clippy-strict typos machete link-check
    @printf '{{green}}[OK]{{reset}}   Full lint suite passed\n'

[group('lint')]
[doc("Fix all auto-fixable issues")]
fix:
    #!/usr/bin/env bash
    printf '{{cyan}}[INFO]{{reset}} Auto-fixing issues...\n'
    {{cargo}} fix --all-features --allow-dirty --allow-staged
    {{cargo}} fmt --all
    typos --write-changes || true
    printf '{{green}}[OK]{{reset}}   Fixed\n'

# ═══════════════════════════════════════════════════════════════════════════════
# DOCUMENTATION RECIPES
# Documentation generation and checking
# ═══════════════════════════════════════════════════════════════════════════════

[group('docs')]
[doc("Generate documentation")]
doc:
    #!/usr/bin/env bash
    printf '{{cyan}}[INFO]{{reset}} Generating documentation...\n'
    {{cargo}} doc --all-features --no-deps
    printf '{{green}}[OK]{{reset}}   Documentation generated\n'

[group('docs')]
[doc("Generate and open documentation")]
doc-open:
    #!/usr/bin/env bash
    printf '{{cyan}}[INFO]{{reset}} Generating documentation...\n'
    {{cargo}} doc --all-features --no-deps --open
    printf '{{green}}[OK]{{reset}}   Documentation opened\n'

[group('docs')]
[doc("Check documentation for warnings")]
doc-check:
    #!/usr/bin/env bash
    printf '{{cyan}}[INFO]{{reset}} Checking documentation...\n'
    RUSTDOCFLAGS="-D warnings" {{cargo}} doc --all-features --no-deps
    printf '{{green}}[OK]{{reset}}   Documentation check passed\n'

# ═══════════════════════════════════════════════════════════════════════════════
# COVERAGE RECIPES
# Code coverage generation
# ═══════════════════════════════════════════════════════════════════════════════

[group('coverage')]
[doc("Generate HTML coverage report")]
coverage:
    #!/usr/bin/env bash
    printf '{{blue}}{{bold}}Generating coverage report...{{reset}}\n'
    {{cargo}} llvm-cov --all-features --html
    printf '{{green}}[OK]{{reset}}   Coverage report: target/llvm-cov/html/index.html\n'

[group('coverage')]
[doc("Generate coverage and open in browser")]
coverage-open:
    #!/usr/bin/env bash
    printf '{{blue}}{{bold}}Generating coverage report...{{reset}}\n'
    {{cargo}} llvm-cov --all-features --html --open
    printf '{{green}}[OK]{{reset}}   Coverage report opened\n'

[group('coverage')]
[doc("Generate LCOV coverage for CI integration")]
coverage-lcov output="lcov.info":
    #!/usr/bin/env bash
    printf '{{cyan}}[INFO]{{reset}} Generating LCOV coverage...\n'
    {{cargo}} llvm-cov --all-features --lcov --output-path {{output}}
    printf '{{green}}[OK]{{reset}}   Coverage saved to {{output}}\n'

[group('coverage')]
[doc("Show coverage summary in terminal")]
coverage-summary:
    #!/usr/bin/env bash
    printf '{{cyan}}[INFO]{{reset}} Coverage summary:\n'
    {{cargo}} llvm-cov --all-features --summary-only

# ═══════════════════════════════════════════════════════════════════════════════
# CI/CD RECIPES
# Continuous integration simulation
# ═══════════════════════════════════════════════════════════════════════════════

[group('ci')]
[doc("Check documentation versions match Cargo.toml")]
version-sync:
    #!/usr/bin/env bash
    printf '{{cyan}}[INFO]{{reset}} Checking version sync...\n'
    VERSION=$(cargo metadata --no-deps --format-version 1 | jq -r '.packages[] | select(.name == "claude-snatch") | .version')
    MAJOR_MINOR=$(echo "$VERSION" | cut -d. -f1,2)

    # Check README.md
    if grep -q "claude-snatch = \"$MAJOR_MINOR\"" README.md 2>/dev/null || grep -q "$VERSION" README.md 2>/dev/null; then
        printf '{{green}}[OK]{{reset}}   README.md version matches\n'
    else
        printf '{{yellow}}[WARN]{{reset}} README.md may need version update (expected %s)\n' "$MAJOR_MINOR"
    fi

    printf '{{green}}[OK]{{reset}}   Version sync check complete (v%s)\n' "$VERSION"

[group('ci')]
[doc("Check CI status on main branch with HEAD verification")]
ci-status:
    #!/usr/bin/env bash
    set -euo pipefail
    printf '{{cyan}}[INFO]{{reset}} Checking CI status on main...\n'

    if ! command -v gh &> /dev/null; then
        printf '{{yellow}}[WARN]{{reset}} gh CLI not installed, cannot check CI status\n'
        printf '{{yellow}}[WARN]{{reset}} Install with: brew install gh / apt install gh\n'
        exit 0
    fi

    # Get local HEAD sha
    LOCAL_SHA=$(git rev-parse HEAD)
    LOCAL_SHORT=$(git rev-parse --short HEAD)

    # Get remote main HEAD sha (if available)
    REMOTE_SHA=$(git ls-remote origin refs/heads/main 2>/dev/null | cut -f1 || echo "")
    REMOTE_SHORT=$(echo "$REMOTE_SHA" | cut -c1-7)

    printf '{{cyan}}[INFO]{{reset}} Local HEAD:  %s\n' "$LOCAL_SHORT"
    if [ -n "$REMOTE_SHA" ]; then
        printf '{{cyan}}[INFO]{{reset}} Remote main: %s\n' "$REMOTE_SHORT"

        # Check if local is in sync with remote
        if [ "$LOCAL_SHA" != "$REMOTE_SHA" ]; then
            printf '{{yellow}}[WARN]{{reset}} Local HEAD differs from remote main\n'
            printf '{{yellow}}[WARN]{{reset}} CI status may not reflect your current commits\n'
        fi
    fi

    # Get latest CI run
    printf '\n{{cyan}}[INFO]{{reset}} Latest CI run on main:\n'
    RUN_INFO=$(gh run list --limit 1 --branch main --json status,conclusion,headSha,displayTitle,url 2>/dev/null || echo "")

    if [ -z "$RUN_INFO" ] || [ "$RUN_INFO" = "[]" ]; then
        printf '{{yellow}}[WARN]{{reset}} No CI runs found for main branch\n'
        exit 0
    fi

    STATUS=$(echo "$RUN_INFO" | jq -r '.[0].status')
    CONCLUSION=$(echo "$RUN_INFO" | jq -r '.[0].conclusion // "pending"')
    RUN_SHA=$(echo "$RUN_INFO" | jq -r '.[0].headSha' | cut -c1-7)
    TITLE=$(echo "$RUN_INFO" | jq -r '.[0].displayTitle')
    URL=$(echo "$RUN_INFO" | jq -r '.[0].url')

    printf '  Title:  %s\n' "$TITLE"
    printf '  Commit: %s\n' "$RUN_SHA"
    printf '  Status: '

    if [ "$STATUS" = "completed" ]; then
        if [ "$CONCLUSION" = "success" ]; then
            printf '{{green}}✓ passed{{reset}}\n'
        elif [ "$CONCLUSION" = "failure" ]; then
            printf '{{red}}✗ failed{{reset}}\n'
        else
            printf '{{yellow}}%s{{reset}}\n' "$CONCLUSION"
        fi
    else
        printf '{{yellow}}%s{{reset}}\n' "$STATUS"
    fi

    printf '  URL:    %s\n' "$URL"

[group('ci')]
[doc("Watch CI run in real-time")]
ci-watch:
    #!/usr/bin/env bash
    printf '{{cyan}}[INFO]{{reset}} Watching CI run...\n'
    gh run watch

[group('ci')]
[doc("Standard CI pipeline (matches GitHub Actions)")]
ci: fmt-check clippy test-locked doc-check version-sync
    #!/usr/bin/env bash
    printf '\n{{bold}}{{blue}}══════ CI Pipeline Complete ══════{{reset}}\n\n'
    printf '{{green}}[OK]{{reset}}   All CI checks passed\n'

[group('ci')]
[doc("Fast CI checks (no tests)")]
ci-fast: fmt-check clippy check-build
    @printf '{{green}}[OK]{{reset}}   Fast CI checks passed\n'

[group('ci')]
[doc("Full CI with coverage and security audit")]
ci-full: ci coverage-lcov audit deny semver
    @printf '{{green}}[OK]{{reset}}   Full CI pipeline passed\n'

[group('ci')]
[doc("Complete CI with all checks (for releases)")]
ci-release: ci-full msrv-check test-features link-check
    @printf '{{green}}[OK]{{reset}}   Release CI pipeline passed\n'

[group('ci')]
[doc("Verify MSRV compliance")]
msrv-check:
    #!/usr/bin/env bash
    printf '{{cyan}}[INFO]{{reset}} Checking MSRV {{msrv}}...\n'
    if ! rustup run {{msrv}} cargo check --all-features 2>/dev/null; then
        printf '{{yellow}}[INFO]{{reset}} Installing Rust {{msrv}}...\n'
        rustup install {{msrv}}
        rustup run {{msrv}} cargo check --all-features
    fi
    printf '{{green}}[OK]{{reset}}   MSRV {{msrv}} check passed\n'

[group('ci')]
[doc("Pre-commit hook checks")]
pre-commit: fmt-check clippy check-build
    @printf '{{green}}[OK]{{reset}}   Pre-commit checks passed\n'

[group('ci')]
[doc("Pre-push hook checks")]
pre-push: ci
    @printf '{{green}}[OK]{{reset}}   Pre-push checks passed\n'

# ═══════════════════════════════════════════════════════════════════════════════
# DEPENDENCY MANAGEMENT
# Dependency analysis and auditing
# ═══════════════════════════════════════════════════════════════════════════════

[group('deps')]
[doc("Run cargo-deny checks (licenses, bans, advisories) - matches CI")]
deny:
    #!/usr/bin/env bash
    printf '{{cyan}}[INFO]{{reset}} Running cargo-deny (matches CI)...\n'
    if [ -f deny.toml ]; then
        {{cargo}} deny --all-features check
        printf '{{green}}[OK]{{reset}}   Deny checks passed\n'
    else
        printf '{{yellow}}[WARN]{{reset}} No deny.toml found, skipping\n'
    fi

[group('deps')]
[doc("Security vulnerability audit via cargo-audit")]
audit:
    #!/usr/bin/env bash
    printf '{{cyan}}[INFO]{{reset}} Running security audit...\n'
    {{cargo}} audit
    printf '{{green}}[OK]{{reset}}   Security audit passed\n'

[group('deps')]
[doc("Check for outdated dependencies")]
outdated:
    #!/usr/bin/env bash
    printf '{{cyan}}[INFO]{{reset}} Checking for outdated dependencies...\n'
    {{cargo}} outdated -R

[group('deps')]
[doc("Update Cargo.lock to latest compatible versions")]
update:
    #!/usr/bin/env bash
    printf '{{cyan}}[INFO]{{reset}} Updating dependencies...\n'
    {{cargo}} update
    printf '{{green}}[OK]{{reset}}   Dependencies updated\n'

[group('deps')]
[doc("Update specific dependency")]
update-dep package:
    #!/usr/bin/env bash
    printf '{{cyan}}[INFO]{{reset}} Updating {{package}}...\n'
    {{cargo}} update -p {{package}}
    printf '{{green}}[OK]{{reset}}   {{package}} updated\n'

[group('deps')]
[doc("Show dependency tree")]
tree:
    #!/usr/bin/env bash
    printf '{{cyan}}[INFO]{{reset}} Dependency tree:\n'
    {{cargo}} tree

[group('deps')]
[doc("Show duplicate dependencies")]
tree-duplicates:
    #!/usr/bin/env bash
    printf '{{cyan}}[INFO]{{reset}} Duplicate dependencies:\n'
    {{cargo}} tree --duplicates

# ═══════════════════════════════════════════════════════════════════════════════
# RELEASE RECIPES
# Version management and publishing
# ═══════════════════════════════════════════════════════════════════════════════

[group('release')]
[doc("Check for clean git working directory")]
release-check-clean:
    #!/usr/bin/env bash
    printf '{{cyan}}[INFO]{{reset}} Checking for uncommitted changes...\n'
    if ! git diff-index --quiet HEAD --; then
        printf '{{red}}[ERR]{{reset}}  Uncommitted changes detected:\n'
        git status --short
        exit 1
    fi
    if [ -n "$(git status --porcelain)" ]; then
        printf '{{red}}[ERR]{{reset}}  Untracked files detected:\n'
        git status --short
        exit 1
    fi
    printf '{{green}}[OK]{{reset}}   Working directory is clean\n'

[group('release')]
[doc("Verify CHANGELOG.md has entry for current version")]
release-check-changelog:
    #!/usr/bin/env bash
    printf '{{cyan}}[INFO]{{reset}} Checking CHANGELOG.md for v{{version}}...\n'
    if [ ! -f CHANGELOG.md ]; then
        printf '{{red}}[ERR]{{reset}}  CHANGELOG.md not found\n'
        exit 1
    fi
    if grep -q "\[{{version}}\]" CHANGELOG.md; then
        printf '{{green}}[OK]{{reset}}   CHANGELOG.md has entry for v{{version}}\n'
    else
        printf '{{red}}[ERR]{{reset}}  CHANGELOG.md missing entry for v{{version}}\n'
        printf '{{cyan}}[INFO]{{reset}} Add a section: ## [{{version}}] - YYYY-MM-DD\n'
        exit 1
    fi

[group('release')]
[doc("Check semver compatibility (alias for semver)")]
release-check-semver: semver

[group('release')]
[doc("Dry run cargo publish (alias for publish-dry)")]
release-dry-run: publish-dry

[group('release')]
[doc("Create release tag (alias for tag)")]
release-tag: tag

[group('release')]
[doc("Full release validation (REQUIRED before tagging)")]
release-check: release-check-clean ci-release wip-check panic-audit version-sync typos machete metadata-check release-check-changelog publish-dry
    #!/usr/bin/env bash
    printf '\n{{bold}}{{blue}}══════ Release Validation ══════{{reset}}\n\n'
    printf '{{cyan}}[INFO]{{reset}} Checking for uncommitted changes...\n'
    if ! git diff-index --quiet HEAD --; then
        printf '{{red}}[ERR]{{reset}}  Uncommitted changes detected\n'
        exit 1
    fi
    printf '{{cyan}}[INFO]{{reset}} Checking for unpushed commits...\n'
    if [ -n "$(git log @{u}.. 2>/dev/null)" ]; then
        printf '{{yellow}}[WARN]{{reset}} Unpushed commits detected\n'
    fi
    printf '{{green}}[OK]{{reset}}   Ready for release\n'

[group('release')]
[doc("Check for WIP markers (TODO, FIXME, XXX, HACK, todo!, unimplemented!)")]
wip-check:
    #!/usr/bin/env bash
    printf '{{cyan}}[INFO]{{reset}} Checking for WIP markers...\n'

    # Search for comment markers
    COMMENTS=$(grep -rn "TODO\|FIXME\|XXX\|HACK" --include="*.rs" src/ 2>/dev/null || true)
    if [ -n "$COMMENTS" ]; then
        printf '{{yellow}}[WARN]{{reset}} Found WIP comments:\n'
        echo "$COMMENTS" | head -20
        COMMENT_COUNT=$(echo "$COMMENTS" | wc -l)
        if [ "$COMMENT_COUNT" -gt 20 ]; then
            printf '{{yellow}}[WARN]{{reset}} ... and %d more\n' "$((COMMENT_COUNT - 20))"
        fi
    fi

    # Search for incomplete macros (excluding tests)
    MACROS=$(grep -rn "todo!\|unimplemented!" --include="*.rs" src/ 2>/dev/null || true)
    if [ -n "$MACROS" ]; then
        printf '{{red}}[ERR]{{reset}}  Found incomplete macros in production code:\n'
        echo "$MACROS"
        exit 1
    fi

    printf '{{green}}[OK]{{reset}}   WIP check passed (no blocking issues)\n'

[group('release')]
[doc("Audit panic paths (.unwrap(), .expect()) in production code")]
panic-audit:
    #!/usr/bin/env bash
    printf '{{cyan}}[INFO]{{reset}} Auditing panic paths in production code...\n'

    # Find .unwrap() in src/ directories (production code)
    UNWRAPS=$(grep -rn "\.unwrap()" src/ --include="*.rs" 2>/dev/null || true)
    EXPECTS=$(grep -rn "\.expect(" src/ --include="*.rs" 2>/dev/null || true)

    if [ -n "$UNWRAPS" ] || [ -n "$EXPECTS" ]; then
        printf '{{yellow}}[WARN]{{reset}} Found potential panic paths:\n'
        if [ -n "$UNWRAPS" ]; then
            echo "$UNWRAPS" | head -15
            UNWRAP_COUNT=$(echo "$UNWRAPS" | wc -l)
            printf '{{cyan}}[INFO]{{reset}} Total .unwrap() calls: %d\n' "$UNWRAP_COUNT"
        fi
        if [ -n "$EXPECTS" ]; then
            echo "$EXPECTS" | head -10
            EXPECT_COUNT=$(echo "$EXPECTS" | wc -l)
            printf '{{cyan}}[INFO]{{reset}} Total .expect() calls: %d\n' "$EXPECT_COUNT"
        fi
        printf '{{yellow}}[NOTE]{{reset}} Review each for production safety.\n'
    else
        printf '{{green}}[OK]{{reset}}   No panic paths found in production code\n'
    fi

[group('release')]
[doc("Verify Cargo.toml metadata for crates.io publishing")]
metadata-check:
    #!/usr/bin/env bash
    printf '{{cyan}}[INFO]{{reset}} Checking Cargo.toml metadata...\n'

    METADATA=$(cargo metadata --no-deps --format-version 1 | jq -r '.packages[] | select(.name == "claude-snatch")')

    # Required fields
    DESC=$(echo "$METADATA" | jq -r '.description // empty')
    LICENSE=$(echo "$METADATA" | jq -r '.license // empty')
    REPO=$(echo "$METADATA" | jq -r '.repository // empty')

    MISSING=""
    [ -z "$DESC" ] && MISSING="$MISSING description"
    [ -z "$LICENSE" ] && MISSING="$MISSING license"
    [ -z "$REPO" ] && MISSING="$MISSING repository"

    if [ -n "$MISSING" ]; then
        printf '{{red}}[ERR]{{reset}}  Missing required fields:%s\n' "$MISSING"
        exit 1
    fi

    printf '{{green}}[OK]{{reset}}   Metadata valid\n'

[group('release')]
[doc("Publish to crates.io (dry run)")]
publish-dry:
    #!/usr/bin/env bash
    printf '{{cyan}}[INFO]{{reset}} Publishing (dry run)...\n'
    {{cargo}} publish --dry-run --allow-dirty
    printf '{{green}}[OK]{{reset}}   Dry run complete\n'

[group('release')]
[confirm("This will publish to crates.io. This action is IRREVERSIBLE. Continue?")]
[doc("Publish to crates.io")]
publish:
    #!/usr/bin/env bash
    printf '\n{{bold}}{{blue}}══════ Publishing to crates.io ══════{{reset}}\n\n'
    printf '{{yellow}}[WARN]{{reset}} This action is IRREVERSIBLE!\n'
    {{cargo}} publish
    printf '\n{{green}}[OK]{{reset}}   Published successfully!\n'
    printf '{{cyan}}[INFO]{{reset}} Next steps:\n'
    printf '  1. Verify: cargo search claude-snatch\n'
    printf '  2. Check docs.rs in ~15 minutes\n'
    printf '  3. Update CHANGELOG.md [Unreleased] section\n'

[group('release')]
[doc("Check semver compatibility")]
semver:
    #!/usr/bin/env bash
    printf '{{cyan}}[INFO]{{reset}} Checking semver compliance...\n'
    {{cargo}} semver-checks check-release || true
    printf '{{green}}[OK]{{reset}}   Semver check complete\n'

[group('release')]
[doc("Create annotated git tag for current version")]
tag:
    #!/usr/bin/env bash
    printf '{{cyan}}[INFO]{{reset}} Creating tag v{{version}}...\n'

    # Verify CI status first
    if command -v gh &> /dev/null; then
        CONCLUSION=$(gh run list --limit 1 --branch main --json conclusion -q '.[0].conclusion' 2>/dev/null || echo "unknown")
        if [ "$CONCLUSION" != "success" ]; then
            printf '{{yellow}}[WARN]{{reset}} CI status is not success (got: %s)\n' "$CONCLUSION"
            printf '{{yellow}}[WARN]{{reset}} Consider running: just ci-status\n'
        fi
    fi

    git tag -a "v{{version}}" -m "Release v{{version}}"
    printf '{{green}}[OK]{{reset}}   Tag created: v{{version}}\n'
    printf '{{dim}}Push with: git push origin v{{version}}{{reset}}\n'

[group('release')]
[doc("Generate changelog with git-cliff")]
changelog:
    #!/usr/bin/env bash
    printf '{{cyan}}[INFO]{{reset}} Generating changelog...\n'
    if command -v git-cliff &> /dev/null; then
        git-cliff -o CHANGELOG.md
        printf '{{green}}[OK]{{reset}}   CHANGELOG.md updated\n'
    else
        printf '{{yellow}}[WARN]{{reset}} git-cliff not installed\n'
    fi

[group('release')]
[doc("Show current version")]
version:
    @echo "{{version}}"

[group('release')]
[doc("Bump patch version (e.g., 0.1.0 -> 0.1.1)")]
bump-patch:
    #!/usr/bin/env bash
    printf '{{cyan}}[INFO]{{reset}} Bumping patch version...\n'
    if command -v cargo-set-version &> /dev/null; then
        cargo set-version --bump patch
    else
        CURRENT="{{version}}"
        IFS='.' read -r MAJOR MINOR PATCH <<< "$CURRENT"
        NEW_PATCH=$((PATCH + 1))
        NEW_VERSION="$MAJOR.$MINOR.$NEW_PATCH"
        sed -i "s/^version = \"$CURRENT\"/version = \"$NEW_VERSION\"/" Cargo.toml
        printf '{{cyan}}[INFO]{{reset}} Version bumped: %s -> %s\n' "$CURRENT" "$NEW_VERSION"
    fi
    {{cargo}} check --quiet
    printf '{{green}}[OK]{{reset}}   Version bumped to %s\n' "$(just version)"

[group('release')]
[doc("Bump minor version (e.g., 0.1.0 -> 0.2.0)")]
bump-minor:
    #!/usr/bin/env bash
    printf '{{cyan}}[INFO]{{reset}} Bumping minor version...\n'
    if command -v cargo-set-version &> /dev/null; then
        cargo set-version --bump minor
    else
        CURRENT="{{version}}"
        IFS='.' read -r MAJOR MINOR PATCH <<< "$CURRENT"
        NEW_MINOR=$((MINOR + 1))
        NEW_VERSION="$MAJOR.$NEW_MINOR.0"
        sed -i "s/^version = \"$CURRENT\"/version = \"$NEW_VERSION\"/" Cargo.toml
        printf '{{cyan}}[INFO]{{reset}} Version bumped: %s -> %s\n' "$CURRENT" "$NEW_VERSION"
    fi
    {{cargo}} check --quiet
    printf '{{green}}[OK]{{reset}}   Version bumped to %s\n' "$(just version)"

[group('release')]
[doc("Bump major version (e.g., 0.1.0 -> 1.0.0)")]
bump-major:
    #!/usr/bin/env bash
    printf '{{cyan}}[INFO]{{reset}} Bumping major version...\n'
    if command -v cargo-set-version &> /dev/null; then
        cargo set-version --bump major
    else
        CURRENT="{{version}}"
        IFS='.' read -r MAJOR MINOR PATCH <<< "$CURRENT"
        NEW_MAJOR=$((MAJOR + 1))
        NEW_VERSION="$NEW_MAJOR.0.0"
        sed -i "s/^version = \"$CURRENT\"/version = \"$NEW_VERSION\"/" Cargo.toml
        printf '{{cyan}}[INFO]{{reset}} Version bumped: %s -> %s\n' "$CURRENT" "$NEW_VERSION"
    fi
    {{cargo}} check --quiet
    printf '{{green}}[OK]{{reset}}   Version bumped to %s\n' "$(just version)"

# ═══════════════════════════════════════════════════════════════════════════════
# RUN RECIPES
# Running the application
# ═══════════════════════════════════════════════════════════════════════════════

[group('run')]
[doc("Run the CLI (debug mode)")]
run *args:
    {{cargo}} run -- {{args}}

[group('run')]
[doc("Run the CLI (release mode)")]
run-release *args:
    {{cargo}} run --release -- {{args}}

[group('run')]
[doc("Run with TUI mode")]
run-tui:
    {{cargo}} run --release --features tui -- tui

[group('run')]
[doc("Run with verbose output")]
run-verbose *args:
    {{cargo}} run --release -- -v {{args}}

# ═══════════════════════════════════════════════════════════════════════════════
# INSTALL RECIPES
# Installation targets
# ═══════════════════════════════════════════════════════════════════════════════

[group('install')]
[doc("Install locally")]
install:
    #!/usr/bin/env bash
    printf '{{cyan}}[INFO]{{reset}} Installing {{binary_name}}...\n'
    {{cargo}} install --path . --locked
    printf '{{green}}[OK]{{reset}}   {{binary_name}} installed\n'

[group('install')]
[doc("Install with all features")]
install-all:
    #!/usr/bin/env bash
    printf '{{cyan}}[INFO]{{reset}} Installing {{binary_name}} with all features...\n'
    {{cargo}} install --path . --locked --all-features
    printf '{{green}}[OK]{{reset}}   {{binary_name}} installed (all features)\n'

[group('install')]
[confirm("This will uninstall {{binary_name}}. Continue?")]
[doc("Uninstall")]
uninstall:
    #!/usr/bin/env bash
    printf '{{yellow}}Uninstalling {{binary_name}}...{{reset}}\n'
    {{cargo}} uninstall {{project_name}} || true
    printf '{{green}}[OK]{{reset}}   {{binary_name}} uninstalled\n'

# ═══════════════════════════════════════════════════════════════════════════════
# DEVELOPMENT WORKFLOW RECIPES
# Day-to-day development utilities
# ═══════════════════════════════════════════════════════════════════════════════

[group('dev')]
[doc("Full development setup and validation")]
dev: build test lint
    @printf '{{green}}[OK]{{reset}}   Development environment ready\n'

[group('dev')]
[no-exit-message]
[doc("Watch mode: re-run tests on file changes")]
watch:
    #!/usr/bin/env bash
    printf '{{cyan}}[INFO]{{reset}} Watching for changes (tests)...\n'
    {{cargo}} watch -x "test --all-features"

[group('dev')]
[no-exit-message]
[doc("Watch mode: re-run check on file changes")]
watch-check:
    #!/usr/bin/env bash
    printf '{{cyan}}[INFO]{{reset}} Watching for changes (check)...\n'
    {{cargo}} watch -x "check --all-features"

[group('dev')]
[no-exit-message]
[doc("Watch mode: re-run clippy on file changes")]
watch-clippy:
    #!/usr/bin/env bash
    printf '{{cyan}}[INFO]{{reset}} Watching for changes (clippy)...\n'
    {{cargo}} watch -x "clippy --all-targets --all-features"

# ═══════════════════════════════════════════════════════════════════════════════
# BENCHMARK RECIPES
# Performance benchmarking
# ═══════════════════════════════════════════════════════════════════════════════

[group('bench')]
[doc("Run all benchmarks")]
bench:
    #!/usr/bin/env bash
    printf '{{blue}}{{bold}}Running benchmarks...{{reset}}\n'
    {{cargo}} bench --all-features
    printf '{{green}}[OK]{{reset}}   Benchmarks complete\n'

[group('bench')]
[doc("Run benchmarks matching a pattern")]
bench-filter pattern:
    #!/usr/bin/env bash
    printf '{{cyan}}[INFO]{{reset}} Running benchmarks matching: {{pattern}}\n'
    {{cargo}} bench --all-features -- {{pattern}}
    printf '{{green}}[OK]{{reset}}   Benchmarks complete\n'

# ═══════════════════════════════════════════════════════════════════════════════
# UTILITY RECIPES
# Miscellaneous utilities
# ═══════════════════════════════════════════════════════════════════════════════

[group('util')]
[doc("Show version and environment info")]
info:
    #!/usr/bin/env bash
    printf '\n{{bold}}{{project_name}} v{{version}}{{reset}}\n'
    printf '═══════════════════════════════════════\n'
    printf '{{cyan}}Binary:{{reset}}    {{binary_name}}\n'
    printf '{{cyan}}MSRV:{{reset}}      {{msrv}}\n'
    printf '{{cyan}}Platform:{{reset}}  {{platform}}\n'
    printf '{{cyan}}Jobs:{{reset}}      {{jobs}}\n'
    printf '\n{{cyan}}Rust:{{reset}}      %s\n' "$(rustc --version)"
    printf '{{cyan}}Cargo:{{reset}}     %s\n' "$(cargo --version)"
    printf '{{cyan}}Just:{{reset}}      %s\n' "$(just --version)"
    printf '\n'

[group('util')]
[doc("Count lines of code")]
loc:
    #!/usr/bin/env bash
    printf '{{cyan}}[INFO]{{reset}} Lines of code:\n'
    if command -v tokei &> /dev/null; then
        tokei src/
    else
        find src -name '*.rs' -exec cat {} + | wc -l | xargs printf 'Rust source: %s lines\n'
    fi

[group('util')]
[doc("Generate and display project statistics")]
stats: loc
    #!/usr/bin/env bash
    printf '\n{{bold}}{{blue}}══════ Project Statistics ══════{{reset}}\n\n'
    printf '{{cyan}}Dependencies:{{reset}}\n'
    printf '  Direct: %s\n' "$({{cargo}} tree --depth 1 | grep -c '├\|└')"
    printf '  Total:  %s\n' "$({{cargo}} tree | wc -l)"
    printf '\n'

[group('util')]
[doc("Check git status")]
git-status:
    @git status --short

[group('util')]
[doc("Generate shell completions")]
completions shell:
    {{cargo}} run --release -- completions {{shell}}

# ═══════════════════════════════════════════════════════════════════════════════
# HELP RECIPES
# Documentation and assistance
# ═══════════════════════════════════════════════════════════════════════════════

[group('help')]
[doc("Show all available recipes grouped by category")]
help:
    #!/usr/bin/env bash
    printf '\n{{bold}}{{project_name}} v{{version}}{{reset}} — Development Command Runner\n'
    printf 'MSRV: {{msrv}} | Platform: {{platform}}\n\n'
    printf '{{bold}}Usage:{{reset}} just [recipe] [arguments...]\n\n'
    just --list --unsorted

[group('help')]
[doc("Show commonly used recipes")]
quick:
    #!/usr/bin/env bash
    printf '{{cyan}}{{bold}}Quick Reference{{reset}}\n\n'
    printf '{{bold}}Development:{{reset}}\n'
    printf '  {{green}}just build{{reset}}          Build debug\n'
    printf '  {{green}}just test{{reset}}           Run tests\n'
    printf '  {{green}}just clippy{{reset}}         Run clippy\n'
    printf '  {{green}}just fmt{{reset}}            Format code\n'
    printf '  {{green}}just watch{{reset}}          Watch mode\n'
    printf '\n{{bold}}CI/Release:{{reset}}\n'
    printf '  {{green}}just ci{{reset}}             Run full CI\n'
    printf '  {{green}}just ci-release{{reset}}     Release CI\n'
    printf '  {{green}}just release-check{{reset}}  Pre-release validation\n'
    printf '\n{{bold}}Analysis:{{reset}}\n'
    printf '  {{green}}just coverage{{reset}}       Code coverage\n'
    printf '  {{green}}just deny{{reset}}           Security/license check\n'
    printf '\n'
