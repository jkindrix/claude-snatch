# claude-snatch Justfile
# Run `just` to see available commands

project_name := "claude-snatch"
binary_name := "snatch"
msrv := "1.95"

# Read version from Cargo.toml
version := `grep '^version' Cargo.toml | head -1 | sed 's/.*"\(.*\)"/\1/'`

cargo := env_var_or_default("CARGO", "cargo")
jobs := env_var_or_default("JOBS", num_cpus())

# Use bash for recipes
set shell := ["bash", "-euo", "pipefail", "-c"]
set export
set dotenv-load

# Show help by default
default:
    @just --list --unsorted

# ─────────────────────────────────────────────────────────────────────────────
# Build
# ─────────────────────────────────────────────────────────────────────────────

# Build in debug mode
build:
    {{cargo}} build --all-features -j {{jobs}}

# Build in release mode
build-release:
    {{cargo}} build --all-features --release -j {{jobs}}

# Type check only
check:
    {{cargo}} check --all-features -j {{jobs}}

# Clean build artifacts
[confirm("Delete all build artifacts?")]
clean:
    {{cargo}} clean
    rm -rf coverage/ lcov.info

# ─────────────────────────────────────────────────────────────────────────────
# Test
# ─────────────────────────────────────────────────────────────────────────────

# Run all tests
test:
    {{cargo}} test --all-features -j {{jobs}}

# Run tests with locked deps
test-locked:
    {{cargo}} test --all-features --locked -j {{jobs}}

# Run tests with cargo-nextest
nextest:
    {{cargo}} nextest run --all-features -j {{jobs}}

# Test feature combinations
test-features:
    {{cargo}} test --no-default-features -j {{jobs}}
    {{cargo}} test --no-default-features --features mmap -j {{jobs}}
    {{cargo}} test --no-default-features --features mcp -j {{jobs}}
    {{cargo}} test --no-default-features --features codex -j {{jobs}}
    {{cargo}} test --all-features -j {{jobs}}

# Run aggregate-only semantic audits against the local native corpus.
# Unlike invoking the ignored tests directly, this fails if the corpus is
# unavailable or empty, preventing a hollow green audit.
audit-native-corpus:
    SNATCH_REQUIRE_REAL_CORPUS=1 {{cargo}} test --all-features --locked codex_real_corpus -- --ignored --test-threads=1 --nocapture

# Reproduce the explicit full-union inventory benchmark on a GNU/Linux host.
# Normal CI pins the algorithmic contract (one bulk inventory, no per-session
# rediscovery); this opt-in gate catches machine-local wall-time/RSS regressions.
benchmark-provider-union max_seconds="10" max_rss_kib="262144":
    #!/usr/bin/env bash
    if [[ ! -x /usr/bin/time ]] || ! /usr/bin/time --version 2>&1 | grep -q GNU; then
        echo "error: benchmark-provider-union requires GNU /usr/bin/time" >&2
        exit 1
    fi
    {{cargo}} build --release --all-features --locked -j {{jobs}}
    metrics=$(mktemp)
    trap 'rm -f "$metrics"' EXIT
    /usr/bin/time -f '%e %M' -o "$metrics" \
        target/release/{{binary_name}} --json list sessions --provider all >/dev/null
    read -r elapsed rss_kib < "$metrics"
    echo "provider union: ${elapsed}s, ${rss_kib} KiB peak RSS"
    if ! awk -v actual="$elapsed" -v limit="{{max_seconds}}" \
        'BEGIN { exit !(actual <= limit) }'; then
        echo "error: ${elapsed}s exceeds {{max_seconds}}s ceiling" >&2
        exit 1
    fi
    if (( rss_kib > {{max_rss_kib}} )); then
        echo "error: ${rss_kib} KiB exceeds {{max_rss_kib}} KiB ceiling" >&2
        exit 1
    fi

# ─────────────────────────────────────────────────────────────────────────────
# Lint
# ─────────────────────────────────────────────────────────────────────────────

# Run clippy
clippy:
    {{cargo}} clippy --all-features --all-targets -- -D warnings

# Check formatting
fmt-check:
    {{cargo}} fmt --all -- --check

# Format code
fmt:
    {{cargo}} fmt --all

# Run all lints
lint: fmt-check clippy

# Fix lint issues
lint-fix:
    {{cargo}} fix --all-features --allow-dirty --allow-staged
    {{cargo}} fmt --all

# ─────────────────────────────────────────────────────────────────────────────
# Documentation
# ─────────────────────────────────────────────────────────────────────────────

# Generate docs
doc:
    {{cargo}} doc --all-features --no-deps

# Generate and open docs
doc-open:
    {{cargo}} doc --all-features --no-deps --open

# Check docs build
doc-check:
    RUSTDOCFLAGS="-D warnings" {{cargo}} doc --all-features --no-deps

# ─────────────────────────────────────────────────────────────────────────────
# CI
# ─────────────────────────────────────────────────────────────────────────────

# Fail when local stable lags the floating stable CI uses — a stale local
# clippy passes lints that CI's newer clippy rejects (6-day red streak, 2026-07).
toolchain-check:
    @if rustup check | grep -q '^stable.*update available'; then \
        echo "error: local stable is behind CI (CI floats on latest stable). Run: rustup update stable"; \
        exit 1; \
    fi

# Run full CI pipeline
ci: toolchain-check fmt-check clippy test-locked doc-check

# Fast CI checks (no tests)
ci-fast: fmt-check clippy check

# MSRV check
msrv-check:
    rustup run {{msrv}} cargo check --all-features --locked

# ─────────────────────────────────────────────────────────────────────────────
# Security
# ─────────────────────────────────────────────────────────────────────────────

# Run cargo-audit
audit:
    {{cargo}} audit

# Run cargo-deny
deny:
    {{cargo}} deny --all-features check

# Run all security checks
security: audit deny

# ─────────────────────────────────────────────────────────────────────────────
# Coverage
# ─────────────────────────────────────────────────────────────────────────────

# Generate HTML coverage
coverage:
    {{cargo}} llvm-cov --all-features --html

# Generate LCOV coverage
coverage-lcov:
    {{cargo}} llvm-cov --all-features --lcov --output-path lcov.info

# ─────────────────────────────────────────────────────────────────────────────
# Dependencies
# ─────────────────────────────────────────────────────────────────────────────

# Check for outdated deps
outdated:
    {{cargo}} outdated -R

# Update deps
update:
    {{cargo}} update

# Show dep tree
tree:
    {{cargo}} tree

# ─────────────────────────────────────────────────────────────────────────────
# Release
# ─────────────────────────────────────────────────────────────────────────────

# Show current version
version:
    @echo "{{version}}"

# Check release readiness
release-check: ci security test-features msrv-check
    @echo "Release checks passed"

# Check for clean working directory
release-check-clean:
    #!/usr/bin/env bash
    if ! git diff-index --quiet HEAD --; then
        echo "ERROR: Uncommitted changes"
        exit 1
    fi
    echo "Working directory is clean"

# Dry run publish
release-dry-run:
    {{cargo}} publish --dry-run --allow-dirty

# Create release tag
release-tag:
    git tag -a "v{{version}}" -m "Release v{{version}}"
    @echo "Created tag v{{version}}"

# Check semver
semver:
    {{cargo}} semver-checks check-release || true

# ─────────────────────────────────────────────────────────────────────────────
# Run
# ─────────────────────────────────────────────────────────────────────────────

# Run CLI
run *args:
    {{cargo}} run -- {{args}}

# Run release build
run-release *args:
    {{cargo}} run --release -- {{args}}

# ─────────────────────────────────────────────────────────────────────────────
# Install
# ─────────────────────────────────────────────────────────────────────────────

# Install locally (all features)
install:
    {{cargo}} install --path . --locked --all-features --force

# Install minimal (no optional features)
install-minimal:
    {{cargo}} install --path . --locked --no-default-features --force

# Uninstall
uninstall:
    {{cargo}} uninstall {{project_name}} || true

# ─────────────────────────────────────────────────────────────────────────────
# Development
# ─────────────────────────────────────────────────────────────────────────────

# Watch for changes
watch:
    {{cargo}} watch -x "test --all-features"

# Watch clippy
watch-clippy:
    {{cargo}} watch -x "clippy --all-targets --all-features"

# Pre-commit checks
pre-commit: fmt-check clippy check

# ─────────────────────────────────────────────────────────────────────────────
# Info
# ─────────────────────────────────────────────────────────────────────────────

# Show project info
info:
    @echo "Project: {{project_name}}"
    @echo "Binary:  {{binary_name}}"
    @echo "Version: {{version}}"
    @echo "MSRV:    {{msrv}}"
