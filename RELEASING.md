# Releasing claude-snatch

> **⚠️ CRITICAL: Read the [Cardinal Rules](#cardinal-rules) section before your first release.**
>
> These rules exist because of painful lessons learned. Violating them risks broken releases,
> user frustration, and hours of debugging. The 10 minutes spent following this guide prevents
> days of cleanup.

This document describes the release process for claude-snatch.

## Table of Contents

- [Cardinal Rules](#cardinal-rules)
- [CI Parity](#ci-parity)
- [Version Numbering](#version-numbering)
- [Pre-Release Checklist](#pre-release-checklist)
- [Feature-Specific Testing](#feature-specific-testing)
- [Release Workflow](#release-workflow)
- [Post-Release Checklist](#post-release-checklist)
- [Hotfix Releases](#hotfix-releases)
- [Security Incident Response](#security-incident-response)
- [Manual Recovery Procedures](#manual-recovery-procedures)
- [Troubleshooting](#troubleshooting)
- [Platform-Specific Notes](#platform-specific-notes)
- [Justfile Recipe Reference](#justfile-recipe-reference)
- [Release Checklist Template](#release-checklist-template)
- [Lessons Learned](#lessons-learned)
- [Appendix: Release Commands](#appendix-release-commands)

---

## Cardinal Rules

These rules are **non-negotiable**. Violating them risks broken releases, user frustration, and painful rollbacks.

### 1. Never Skip the Full CI Pipeline

```bash
just ci
```

Every release **must** pass the complete CI pipeline locally before tagging. This includes:
- All tests (including feature matrix)
- All lints (formatting, Clippy)
- Documentation builds
- Security audits

**Why:** CI catches issues that local development might miss. The 5 minutes spent running `just ci` prevents hours of debugging broken releases.

### 2. Never Release from a Dirty Working Directory

```bash
just release-check-clean
```

All changes must be committed before releasing. Uncommitted changes mean:
- The tag won't match what's actually released
- Debugging becomes impossible ("which version is this?")
- Users can't reproduce issues

### 3. Never Force-Push Tags

Once a tag is pushed, it's immutable. If a tag is wrong:
1. Create a new patch version
2. Document what went wrong in CHANGELOG.md
3. Never delete or move the broken tag (it may already be cached/downloaded)

### 4. Always Update CHANGELOG.md Before Tagging

Users and maintainers rely on the changelog to understand what changed. An entry must exist for every version.

```bash
# Verify changelog has entry for current version
just release-check-changelog
```

### 5. Always Test the Built Artifact

Before publishing:
```bash
cargo build --release --all-features
./target/release/snatch --version
./target/release/snatch list projects  # Quick smoke test
```

### 6. Never Publish Without Dry Run

```bash
just release-dry-run
```

Always verify that `cargo publish` will succeed before the actual publish.

### 7. Always Run Quality Gates Before Release

```bash
# Check for TODO/FIXME/WIP markers
just wip-check

# Audit panic paths
just panic-audit

# Verify metadata for crates.io
just metadata-check

# Check for typos
just typos
```

---

## CI Parity

Local commands should match CI behavior. This table maps Justfile recipes to CI workflows:

| Local Command | CI Equivalent | Purpose |
|--------------|---------------|---------|
| `just check` | `cargo check --all-features` | Fast compilation check |
| `just build` | `cargo build --all-features` | Full build |
| `just test` | `cargo test --all-features` | Run all tests |
| `just test-matrix` | Matrix build across features | Test all feature combinations |
| `just lint` | `cargo fmt --check && cargo clippy` | Formatting + linting |
| `just doc-check` | `cargo doc --all-features --no-deps` | Documentation build |
| `just security` | `cargo deny check && cargo audit` | Security audit |
| `just msrv-check` | Rust 1.75.0 toolchain | MSRV verification |
| `just ci` | **All of the above** | Complete CI pipeline |
| `just ci-status` | GitHub Actions API | Check if CI passed on main |

### Verify CI Status Before Tagging

**CRITICAL:** Always verify CI passed on your exact commit before creating a tag:

```bash
# Check CI status on main branch
just ci-status

# Expected output:
# ✓ CI passed on main (commit abc1234)
# Ready to tag

# If CI is still running or failed:
# ✗ CI is still running or failed
# Do NOT proceed with tagging
```

The `just ci-status` recipe:
1. Checks the latest CI run on the `main` branch
2. Verifies the run completed successfully
3. Confirms the commit SHA matches your current HEAD
4. Exits with error if CI hasn't passed

### CI Automation Coverage

| Check | Automated in CI | Manual Required | Notes |
|-------|----------------|-----------------|-------|
| Compilation | ✅ | - | All features tested |
| Tests | ✅ | - | Including integration tests |
| Formatting | ✅ | - | `rustfmt` |
| Linting | ✅ | - | `clippy` with all warnings |
| Documentation | ✅ | - | Doc tests included |
| Security Audit | ✅ | - | `cargo-deny` + `cargo-audit` |
| MSRV | ✅ | - | Rust 1.75.0 |
| Semver Check | ⚠️ | Optional | `cargo-semver-checks` |
| Smoke Test | - | ✅ | Manual binary verification |
| Changelog Update | - | ✅ | Human-written |
| Version Bump | - | ✅ | Human decision |

---

## Version Numbering

claude-snatch follows [Semantic Versioning 2.0.0](https://semver.org/):

```
MAJOR.MINOR.PATCH
```

### When to Increment

| Change Type | Version Bump | Example |
|-------------|--------------|---------|
| Breaking API changes | MAJOR | Removing a command, changing output format |
| New features (backward compatible) | MINOR | New export format, new command |
| Bug fixes (backward compatible) | PATCH | Fixing parser edge case, typo in output |

### Pre-1.0 Rules

While at version 0.x.y:
- MINOR bumps may include breaking changes
- PATCH bumps should still be backward compatible
- Document all breaking changes clearly in CHANGELOG.md

### Examples

```
0.1.0 -> 0.1.1  # Bug fix
0.1.1 -> 0.2.0  # New feature (e.g., new export format)
0.2.0 -> 0.3.0  # Breaking change (e.g., changed CLI flags)
0.9.0 -> 1.0.0  # Stable release, API frozen
1.0.0 -> 1.0.1  # Bug fix
1.0.1 -> 1.1.0  # New feature
1.1.0 -> 2.0.0  # Breaking change
```

### Version Bump Commands

```bash
# Get current version
just version

# Bump patch (e.g., 0.1.0 -> 0.1.1)
just bump-patch

# Bump minor (e.g., 0.1.0 -> 0.2.0)
just bump-minor

# Bump major (e.g., 0.1.0 -> 1.0.0)
just bump-major
```

---

## Pre-Release Checklist

Complete these steps **in order** before creating a release.

### Phase 1: Preparation

#### 1. Ensure Clean Working Directory

```bash
just release-check-clean
# Or manually: git status
```

If not clean, commit or stash changes:
```bash
git add -A && git commit -m "chore: prepare for release"
```

#### 2. Check for WIP Code

```bash
just wip-check
```

Fails if any `TODO`, `FIXME`, `todo!()`, or `unimplemented!()` markers exist.

#### 3. Audit Panic Paths

```bash
just panic-audit
```

Reviews `.unwrap()` and `.expect()` usage. Address any issues in hot paths.

### Phase 2: Version & Changelog

#### 4. Update Version Number

```bash
# Option A: Use just recipes
just bump-patch  # or bump-minor, bump-major

# Option B: Manual edit
# Edit Cargo.toml: version = "X.Y.Z"
# Then: cargo check (updates Cargo.lock)
```

#### 5. Update CHANGELOG.md

Add an entry for the new version:

```markdown
## [X.Y.Z] - YYYY-MM-DD

### Added
- New feature description

### Changed
- Changed behavior description

### Fixed
- Bug fix description

### Removed
- Removed feature description (if applicable)
```

Verify the changelog:
```bash
just release-check-changelog
```

### Phase 3: Quality Gates

#### 6. Run Full CI Pipeline

```bash
just ci
```

This runs:
- `just check` - Compilation check
- `just lint` - Formatting and Clippy
- `just test-matrix` - Tests across all feature combinations
- `just doc-check` - Documentation validation
- `just security` - Security audit

**All checks must pass before proceeding.**

#### 7. Test MSRV Compatibility

```bash
just msrv-check
```

Ensures compatibility with Rust 1.75.0 (our MSRV).

#### 8. Run Semver Check

```bash
just release-check-semver
```

Detects accidental breaking changes in the public API.

#### 9. Verify Metadata

```bash
just metadata-check
```

Ensures all required metadata for crates.io is present.

#### 10. Check for Typos

```bash
just typos
```

Catches spelling mistakes in code and documentation.

### Phase 4: Final Verification

#### 11. Verify Publish Dry Run

```bash
just release-dry-run
```

Ensures `cargo publish` will succeed.

#### 12. Build and Smoke Test

```bash
just build-release
./target/release/snatch --version
./target/release/snatch list projects
```

#### 13. Commit Version Bump

```bash
git add Cargo.toml Cargo.lock CHANGELOG.md
git commit -m "chore: release v$(just version)"
```

---

## Feature-Specific Testing

claude-snatch has several optional features that must be tested before release:

### Feature Matrix

| Feature | Description | Test Command | Notes |
|---------|-------------|--------------|-------|
| `default` | Standard functionality (no features) | `cargo test` | Base functionality |
| `tui` | Terminal UI mode | `cargo test --features tui` | Requires terminal |
| `tracing` | Enhanced tracing/debugging | `cargo test --features tracing` | Additional logging |
| `mmap` | Memory-mapped file parsing | `cargo test --features mmap` | For large JSONL files |
| All features | Complete test | `cargo test --all-features` | Required before release |

### Quick Feature Matrix Test

```bash
# Run the full feature matrix (required before release)
just test-features

# Or manually:
cargo test                           # Default (no features)
cargo test --features tui            # TUI feature
cargo test --features tracing        # Tracing feature
cargo test --features mmap           # Memory-mapped files
cargo test --all-features            # All features combined
```

### Feature-Specific Considerations

**TUI Feature (`tui`):**
- Tests may require a TTY environment
- CI runs in non-interactive mode; ensure tests handle this gracefully
- Visual inspection of TUI is recommended before release

**Tracing Feature (`tracing`):**
- Increases binary size and runtime overhead
- Verify log output format hasn't changed unexpectedly
- Check that RUST_LOG environment variable works correctly

**Memory-Mapped Files (`mmap`):**
- Test with large files (100MB+) if possible
- Verify behavior on different filesystems (ext4, NTFS, APFS)
- Check memory usage doesn't spike unexpectedly

### Smoke Tests for Each Feature

```bash
# Default (no features)
./target/release/snatch --version
./target/release/snatch list projects

# With TUI (if available)
./target/release/snatch --tui  # Should launch TUI mode

# With tracing
RUST_LOG=debug ./target/release/snatch list projects 2>&1 | grep -q "DEBUG" || echo "No debug output"

# With mmap (test with large file if available)
./target/release/snatch parse large-session.jsonl --mmap
```

---

## Release Workflow

### Step 1: Final Verification

```bash
just release-check
```

This runs all pre-release validations:
- Clean working directory
- WIP check
- Full CI pipeline
- Changelog verification
- Metadata check

### Step 2: Create and Push Tag

```bash
# Create annotated tag
just release-tag

# Push commit and tag
git push origin main
git push origin v$(just version)
```

### Step 3: Verify GitHub Release (If Applicable)

If GitHub Actions are configured:
1. Check the Actions tab for successful workflow
2. Verify release artifacts are generated
3. Review auto-generated release notes

### Step 4: Publish to crates.io (When Ready)

```bash
# Final dry run
just release-dry-run

# Actual publish
cargo publish
```

**Note:** Requires authentication:
```bash
cargo login
```

### Step 5: Announce Release

Update relevant channels:
- GitHub Releases page
- Project README (if version badge exists)
- Any announcement channels

---

## Post-Release Checklist

### Immediate (Within 1 Hour)

- [ ] Verify tag exists on GitHub: `git ls-remote --tags origin | grep $(just version)`
- [ ] Verify crates.io shows new version (if published)
- [ ] Test installation: `cargo install claude-snatch`
- [ ] Quick smoke test of installed binary

### Short-Term (Within 24 Hours)

- [ ] Monitor for issue reports
- [ ] Check download statistics
- [ ] Update any dependent projects

### If Issues Are Found

See [Hotfix Releases](#hotfix-releases) section.

---

## Hotfix Releases

When a critical bug is found in a release:

### 1. Assess Severity

| Severity | Action |
|----------|--------|
| Critical (data loss, security) | Immediate hotfix |
| Major (functionality broken) | Hotfix within 24 hours |
| Minor (cosmetic, workaround exists) | Include in next regular release |

### 2. Create Hotfix Branch (Optional)

For complex fixes:
```bash
git checkout -b hotfix/v0.1.1 v0.1.0
```

For simple fixes, work directly on main.

### 3. Apply Fix

1. Write fix with test proving the bug
2. Commit: `git commit -m "fix: description of fix"`
3. Bump PATCH version: `just bump-patch`
4. Update CHANGELOG.md

### 4. Release

Follow normal [Release Workflow](#release-workflow).

### 5. Document

Add post-mortem to CHANGELOG.md if appropriate:
```markdown
## [0.1.1] - 2024-01-16

### Fixed
- Fixed crash when parsing sessions with empty messages (#42)

**Note:** This is a hotfix for a critical bug in 0.1.0. Users should upgrade immediately.
```

---

## Security Incident Response

When a security vulnerability is discovered in a released version:

### 1. Assess Severity

| Severity | CVSS Score | Response Time | Disclosure |
|----------|------------|---------------|------------|
| Critical | 9.0-10.0 | Immediate (within hours) | Private until fix available |
| High | 7.0-8.9 | Within 24 hours | Private until fix available |
| Medium | 4.0-6.9 | Within 7 days | Can be public after fix |
| Low | 0.1-3.9 | Next regular release | Public immediately |

### 2. Immediate Actions (Critical/High)

```bash
# 1. Create a private branch for the fix
git checkout -b security/CVE-XXXX-XXXX

# 2. Apply fix with minimal changes
# Focus ONLY on the security issue

# 3. Test thoroughly
just ci
just test-features

# 4. Bump patch version
just bump-patch

# 5. Update CHANGELOG with security note
```

### 3. Disclosure Process

1. **Do NOT disclose publicly** until fix is available
2. **Notify known affected users** privately if possible
3. **Prepare release notes** that explain the vulnerability without providing exploit details
4. **Coordinate with security researchers** if they reported the issue
5. **Request CVE ID** if warranted (via GitHub Security Advisories)

### 4. Security Release Changelog Format

```markdown
## [0.X.Y] - YYYY-MM-DD

### Security
- **CVE-XXXX-XXXX**: Fixed [brief description] (credit: @researcher)
  - Severity: High
  - Affected versions: 0.X.0 - 0.X.Z
  - Recommendation: Upgrade immediately

### Fixed
- [Additional non-security fixes if any]
```

### 5. Post-Release Actions

- [ ] Publish GitHub Security Advisory
- [ ] Update any security documentation
- [ ] Notify package managers (if applicable)
- [ ] Consider blog post for critical issues
- [ ] Update security policy (SECURITY.md)

### 6. Yank Policy

Only yank versions with **critical** security vulnerabilities:

```bash
# Yank a version (use sparingly)
cargo yank --version 0.1.0

# Users with Cargo.lock are unaffected
# Only prevents NEW installs of that version
```

**Note:** Yanking is a last resort. A patched release is usually sufficient.

---

## Manual Recovery Procedures

### Recovering from a Bad Tag

If you tagged the wrong commit:

```bash
# DO NOT delete the tag from remote
# Instead, create a new patch version

# 1. Bump to next patch
just bump-patch

# 2. Update changelog with explanation
cat >> CHANGELOG.md << 'EOF'

## [0.1.2] - $(date +%Y-%m-%d)

### Fixed
- Re-release to fix incorrect tag in 0.1.1

**Note:** Version 0.1.1 was tagged on the wrong commit. Please use 0.1.2 instead.
EOF

# 3. Commit and release normally
git add -A
git commit -m "chore: release v$(just version) (fixes bad tag)"
just release-tag
git push origin main
git push origin v$(just version)
```

### Recovering from a Failed Publish

If `cargo publish` fails mid-upload:

1. **Wait 10 minutes** - crates.io may need time to clean up
2. **Check crates.io** - the version may have actually published
3. **If not published:** Re-run `cargo publish`
4. **If partially published:** Bump patch version and try again

### Recovering from Pushed Commits After Tag

If you pushed additional commits after tagging but before publishing:

```bash
# Option A: If changes are trivial, bump patch version
just bump-patch
# Update changelog, commit, tag, publish

# Option B: If tag hasn't been pushed yet
git tag -d v0.1.0  # Delete local tag
# Make your commits
just release-tag   # Re-tag
```

---

## Troubleshooting

### `just ci` Fails

#### Clippy Warnings
```bash
just lint-fix  # Auto-fix what's possible
just lint-clippy  # See remaining issues
```

#### Test Failures
```bash
just test-verbose  # See detailed output
just test-one test_name  # Run specific test
```

#### Documentation Errors
```bash
cargo doc --all-features 2>&1 | head -50  # See first errors
```

### `cargo publish` Fails

#### "crate version already exists"
You cannot re-publish a version. Bump the version and try again.

#### "missing documentation"
Ensure all public items have doc comments:
```rust
/// This function does something.
pub fn something() {}
```

#### "missing license"
Ensure `Cargo.toml` has:
```toml
license = "MIT"
```

#### "failed to verify package tarball"
```bash
# Check what would be packaged
cargo package --list

# Common issues:
# - Missing files referenced in Cargo.toml
# - Files too large
# - Build script issues
```

### Tag Already Exists

**Never delete tags.** Instead:
1. Bump to next PATCH version
2. Document in CHANGELOG that previous version was problematic
3. Create new tag

### MSRV Check Fails

```bash
rustup install 1.75.0
rustup run 1.75.0 cargo check --all-features
```

Common causes:
- Using features stabilized after MSRV
- Dependencies with higher MSRV

Fix by:
1. Updating code to avoid new features
2. Pinning dependency versions
3. Or updating MSRV (breaking change)

### Semver Check Finds Issues

```bash
cargo semver-checks check-release --verbose
```

Options:
1. Revert the breaking change
2. Bump MAJOR version
3. Mark as intentional breaking change (pre-1.0 only for MINOR)

### WIP Check Fails

```bash
just wip-check
```

Remove or address all:
- `TODO` comments
- `FIXME` comments
- `todo!()` macros
- `unimplemented!()` macros

### Metadata Check Fails

```bash
just metadata-check
```

Ensure `Cargo.toml` has all required fields:
- `description`
- `license`
- `repository`
- `keywords`
- `categories`

---

## Platform-Specific Notes

### Linux

Linux is the primary development and CI platform. All features should work out of the box.

**Musl Builds:**
```bash
# Static binary (no glibc dependency)
cargo build --release --target x86_64-unknown-linux-musl

# Requires musl-tools on Debian/Ubuntu:
sudo apt-get install musl-tools
```

**Known Issues:**
- None currently

### macOS

macOS is fully supported on both Intel (x86_64) and Apple Silicon (aarch64).

**Cross-compilation from Linux:**
```bash
# Requires osxcross or cross tool
cross build --release --target x86_64-apple-darwin
cross build --release --target aarch64-apple-darwin
```

**Known Issues:**
- Notarization may be required for distribution outside the App Store
- Consider code signing for security-conscious users

### Windows

Windows is supported via the MSVC toolchain.

**Build Requirements:**
- Visual Studio Build Tools or full Visual Studio
- Rust with MSVC target

**Cross-compilation from Linux:**
```bash
# Requires mingw-w64
cross build --release --target x86_64-pc-windows-gnu

# Or with MSVC (requires Wine + MSVC)
cross build --release --target x86_64-pc-windows-msvc
```

**Known Issues:**
- Path separators: Use `std::path::PathBuf` consistently
- Line endings: Ensure CRLF doesn't break tests
- Unicode filenames: Test with non-ASCII characters

### CI Build Matrix

| Target | OS | Architecture | Notes |
|--------|-----|--------------|-------|
| x86_64-unknown-linux-gnu | Linux | x86_64 | Primary target |
| x86_64-unknown-linux-musl | Linux | x86_64 | Static binary |
| aarch64-unknown-linux-gnu | Linux | ARM64 | Raspberry Pi, AWS Graviton |
| x86_64-apple-darwin | macOS | Intel | macOS 10.9+ |
| aarch64-apple-darwin | macOS | Apple Silicon | macOS 11+ |
| x86_64-pc-windows-msvc | Windows | x86_64 | Windows 7+ |

---

## Justfile Recipe Reference

Quick reference mapping release tasks to Justfile recipes:

| Task | Recipe | Description |
|------|--------|-------------|
| Show version | `just version` | Display current version |
| Bump patch | `just bump-patch` | Increment patch version |
| Bump minor | `just bump-minor` | Increment minor version |
| Bump major | `just bump-major` | Increment major version |
| Full CI | `just ci` | Run complete CI pipeline |
| Run tests | `just test` | Run all tests |
| Run lints | `just lint` | Check formatting and clippy |
| Fix lints | `just lint-fix` | Auto-fix lint issues |
| Check docs | `just doc-check` | Build documentation |
| Security audit | `just security` | Run security checks |
| MSRV check | `just msrv-check` | Verify minimum Rust version |
| WIP check | `just wip-check` | Find TODO/FIXME markers |
| Panic audit | `just panic-audit` | Find .unwrap()/.expect() |
| Typo check | `just typos` | Check for spelling errors |
| Metadata check | `just metadata-check` | Verify crates.io metadata |
| Clean check | `just release-check-clean` | Verify clean git state |
| Changelog check | `just release-check-changelog` | Verify changelog entry |
| Semver check | `just release-check-semver` | Check for breaking changes |
| Dry run | `just release-dry-run` | Test cargo publish |
| Full release check | `just release-check` | All pre-release checks |
| Create tag | `just release-tag` | Create annotated tag |
| Build release | `just build-release` | Build optimized binary |

---

## Release Checklist Template

Copy this checklist for each release:

```markdown
## Release v0.X.Y Checklist

### Preparation
- [ ] Working directory clean: `just release-check-clean`
- [ ] No WIP code: `just wip-check`
- [ ] Panic paths reviewed: `just panic-audit`

### Version & Changelog
- [ ] Version bumped: `just bump-{patch|minor|major}`
- [ ] CHANGELOG.md updated with release notes
- [ ] Changelog verified: `just release-check-changelog`

### Quality Gates
- [ ] Full CI passes: `just ci`
- [ ] MSRV compatible: `just msrv-check`
- [ ] Semver checked: `just release-check-semver`
- [ ] Metadata valid: `just metadata-check`
- [ ] No typos: `just typos`

### Final Verification
- [ ] Dry run passes: `just release-dry-run`
- [ ] Binary smoke tested:
  - [ ] `./target/release/snatch --version`
  - [ ] `./target/release/snatch list projects`
- [ ] Version bump committed: `git commit -m "chore: release v$(just version)"`

### Release
- [ ] Tag created: `just release-tag`
- [ ] Pushed to origin: `git push origin main && git push origin v$(just version)`
- [ ] GitHub release verified (if applicable)
- [ ] Published to crates.io: `cargo publish`

### Post-Release
- [ ] Tag visible on GitHub
- [ ] crates.io shows new version
- [ ] Installation tested: `cargo install claude-snatch`
- [ ] Smoke test passed
```

---

## Lessons Learned

This section documents issues encountered in past releases and how to avoid them.

### 1. Always Run Full Feature Matrix Tests

**Issue:** Feature-gated code that compiles with `--all-features` may fail when individual features are tested in isolation.

**Prevention:** Always run `just test-features` before release, not just `just test`.

### 2. Verify CI Status Before Tagging

**Issue:** Creating a tag while CI is still running (or failing) leads to broken releases.

**Prevention:** Use `just ci-status` to verify CI passed on the exact commit you're about to tag:

```bash
# Check CI status before tagging
just ci-status

# Only proceed if status shows success
just release-tag
```

### 3. Memory-Mapped Files Across Platforms

**Issue:** The `mmap` feature behaves differently on Windows vs. Unix systems.

**Prevention:**
- Test on all platforms before release
- Document any platform-specific limitations
- Consider feature-gating platform-specific behavior

### 4. Changelog Date Consistency

**Issue:** Changelog entries with future dates or inconsistent formats cause confusion.

**Prevention:**
- Use ISO 8601 date format: `YYYY-MM-DD`
- Set the date on the day of release, not when writing the entry
- Verify with `just release-check-changelog`

### 5. Binary Size Regression

**Issue:** Adding dependencies or features can unexpectedly increase binary size.

**Prevention:**
```bash
# Check binary size before release
ls -lh target/release/snatch

# Compare with previous release
# Consider using cargo-bloat for detailed analysis
cargo bloat --release
```

### 6. Index Propagation Delays

**Issue:** After `cargo publish`, the new version may not be immediately available on crates.io.

**Prevention:**
- Wait 30-60 seconds after publishing before testing `cargo install`
- Don't panic if the version doesn't appear immediately
- Check crates.io web interface for confirmation

### 7. Documentation Build Failures

**Issue:** Code examples in documentation that compile locally may fail on docs.rs.

**Prevention:**
```bash
# Test documentation builds with same flags as docs.rs
RUSTDOCFLAGS="--cfg docsrs -D warnings" cargo +nightly doc --all-features
```

### 8. Pre-Release Version Handling

**Issue:** Pre-release versions (e.g., `0.1.0-alpha.1`) have special semver semantics.

**Prevention:**
- Understand that `0.1.0-alpha.1 < 0.1.0`
- Pre-release versions are excluded from default version resolution
- Document pre-release status clearly in CHANGELOG

### 9. Git Tag Format Consistency

**Issue:** Inconsistent tag formats (`v0.1.0` vs `0.1.0`) break automation.

**Prevention:**
- Always use `v` prefix: `v0.1.0`
- Use `just release-tag` which enforces the correct format
- Never manually create tags with `git tag`

### 10. Cargo.lock in Version Control

**Issue:** Not tracking `Cargo.lock` can lead to different dependencies in CI vs. local builds.

**Prevention:**
- Always commit `Cargo.lock` for binary crates
- Use `cargo test --locked` in CI to catch lockfile drift

---

## Appendix: Release Commands

### Quick Reference

```bash
# Check release readiness
just release-check

# Show current version
just version

# Create tag
just release-tag

# Dry run publish
just release-dry-run

# Full CI
just ci

# List all available recipes
just --list
```

### Complete Release Script

```bash
#!/bin/bash
set -euo pipefail

echo "=== claude-snatch Release Script ==="
echo ""

# Pre-flight checks
echo "Running pre-flight checks..."
just release-check

# Build and smoke test
echo ""
echo "Building release binary..."
just build-release
echo ""
echo "Smoke testing binary..."
./target/release/snatch --version
./target/release/snatch list projects 2>/dev/null || true

# Create and push tag
echo ""
echo "Creating release tag..."
just release-tag

echo ""
echo "Pushing to origin..."
git push origin main
git push origin "v$(just version)"

# Publish (uncomment when ready)
echo ""
echo "Ready to publish. Run:"
echo "  cargo publish"

echo ""
echo "=== Release v$(just version) complete! ==="
```

### One-Liner Release (After Checklist Complete)

```bash
just release-check && just release-tag && git push origin main && git push origin v$(just version) && cargo publish
```

---

## Release History

| Version | Date | Notes |
|---------|------|-------|
| 0.1.0 | TBD | Initial release |

---

## Maintainer Notes

### Crates.io Publishing

Before first publish:
1. Create account at https://crates.io
2. Run `cargo login` with API token
3. Verify `Cargo.toml` metadata is complete: `just metadata-check`

### GitHub Releases

Consider setting up GitHub Actions for:
- Automated release creation on tag push
- Binary artifact generation for multiple platforms
- Changelog extraction from CHANGELOG.md

### Feature Flag Considerations

When releasing with optional features:
- Test all feature combinations: `just test-matrix`
- Document which features are included in default builds
- Consider separate release notes for feature-specific changes

Current features:
- `default` - Standard functionality
- `tui` - Terminal UI mode
- `tracing` - Enhanced tracing/debugging
- `mmap` - Memory-mapped file parsing for very large JSONL files

### Automated Changelog Generation

For future releases, consider using git-cliff:
```bash
# Install
cargo install git-cliff

# Generate changelog
git cliff --output CHANGELOG.md
```

### Binary Distribution

For cross-platform binary distribution:
```bash
# Build for current platform
just build-release

# Cross-compile (requires cross)
just build-cross aarch64-unknown-linux-gnu
just build-cross x86_64-unknown-linux-musl
just build-cross x86_64-apple-darwin
just build-cross x86_64-pc-windows-gnu
```
