# claude-snatch Justfile
# Run `just` to see available commands

project_name := "claude-snatch"
binary_name := "snatch"
msrv := "1.75.0"

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
    {{cargo}} test --no-default-features --features tui -j {{jobs}}
    {{cargo}} test --no-default-features --features mmap -j {{jobs}}
    {{cargo}} test --all-features -j {{jobs}}

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

# Run full CI pipeline
ci: fmt-check clippy test-locked doc-check

# Fast CI checks (no tests)
ci-fast: fmt-check clippy check

# MSRV check
msrv-check:
    rustup run {{msrv}} cargo check --all-features

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
release-check: ci security test-features
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
    {{cargo}} install --path . --locked --all-features

# Install minimal (no optional features)
install-minimal:
    {{cargo}} install --path . --locked

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
