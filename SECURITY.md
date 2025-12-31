# Security Policy

## Supported Versions

| Version | Supported          |
| ------- | ------------------ |
| 0.1.x   | :white_check_mark: |

## Security Model

claude-snatch is a **local-only** tool that operates exclusively on your filesystem. It has the following security properties:

### What claude-snatch Does

- Reads Claude Code JSONL session logs from `~/.claude/projects/`
- Parses and exports conversation data to various formats
- Writes export files to user-specified paths
- Indexes sessions locally for search (using SQLite/Tantivy)

### What claude-snatch Does NOT Do

- **No network access**: The core functionality makes no network requests
- **No data exfiltration**: Your conversation data stays on your machine
- **No code execution**: User-provided data is never executed
- **No elevated privileges**: Runs with standard user permissions

## Security Measures

### Code Safety

- `#![forbid(unsafe_code)]` enforced at the crate level
- All Clippy pedantic and nursery lints enabled
- Dependency auditing via `cargo-deny`
- No unsafe blocks in user-facing code

### Data Handling

- **SQL Injection Prevention**: All database queries use parameterized statements
- **HTML Escaping**: Export to HTML properly escapes all user content
- **Path Handling**: File paths are validated before access
- **No Arbitrary File Access**: Only reads from known Claude Code directories

### Dependency Security

We maintain `deny.toml` for dependency auditing:
- Known vulnerabilities are tracked and documented
- License compliance is enforced
- Duplicate dependencies are monitored

## Reporting a Vulnerability

If you discover a security vulnerability, please report it responsibly:

1. **Do NOT** open a public GitHub issue for security vulnerabilities
2. **Email**: Send details to the maintainer (see repository)
3. **Include**:
   - Description of the vulnerability
   - Steps to reproduce
   - Potential impact assessment
   - Suggested fix (if any)

### Response Timeline

- **Acknowledgment**: Within 48 hours
- **Initial Assessment**: Within 7 days
- **Fix Timeline**: Depends on severity
  - Critical: Within 24-48 hours
  - High: Within 7 days
  - Medium: Within 30 days
  - Low: Next release cycle

### What to Expect

1. We will acknowledge receipt of your report
2. We will investigate and validate the issue
3. We will develop and test a fix
4. We will release a patched version
5. We will credit you in the release notes (unless you prefer anonymity)

## Threat Model

### In Scope

- Parsing vulnerabilities (malformed JSONL causing crashes or undefined behavior)
- Path traversal in export functionality
- SQL injection in SQLite exports
- XSS in HTML exports
- Memory safety issues
- Denial of service via crafted input

### Out of Scope

- Social engineering attacks
- Physical access attacks
- Attacks requiring malicious Claude Code installation
- Issues in development dependencies only

## Known Limitations

1. **Trust in Input Data**: claude-snatch trusts that Claude Code session files are not maliciously crafted. If an attacker can write to your `~/.claude/` directory, they have already compromised your system.

2. **Export File Permissions**: Exported files inherit standard umask permissions. Users should manage access to exported data appropriately.

3. **No Encryption**: Session data is stored and exported in plain text. Users handling sensitive conversations should use filesystem encryption.

## Security Best Practices for Users

1. **Protect your Claude Code directory**: Ensure `~/.claude/` has appropriate permissions
2. **Review exports before sharing**: Check for sensitive information in exported files
3. **Use filesystem encryption**: If conversations contain sensitive data
4. **Keep dependencies updated**: Run `cargo update` regularly
5. **Verify checksums**: When installing pre-built binaries

## Acknowledgments

We thank the following individuals for responsibly disclosing security issues:

*No security issues have been reported yet.*
