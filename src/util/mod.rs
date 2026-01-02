//! Utility functions for common operations.
//!
//! This module provides shared utilities used across the crate:
//! - Atomic file operations for data safety
//! - Path utilities
//! - Sensitive data redaction

use std::borrow::Cow;
use std::io::{self, Write};
use std::path::Path;

use regex::Regex;
use once_cell::sync::Lazy;

use tempfile::NamedTempFile;

use crate::error::{Result, SnatchError};

/// Atomically write content to a file.
///
/// This function ensures data integrity by:
/// 1. Writing to a temporary file in the same directory
/// 2. Syncing the data to disk
/// 3. Atomically renaming the temp file to the target path
///
/// If any step fails, the original file (if it exists) remains unchanged.
///
/// # Arguments
///
/// * `path` - The target file path
/// * `content` - The content to write as bytes
///
/// # Errors
///
/// Returns an error if:
/// - The parent directory cannot be determined or doesn't exist
/// - The temporary file cannot be created
/// - Writing to the temporary file fails
/// - The atomic rename (persist) operation fails
///
/// # Example
///
/// ```rust,no_run
/// use claude_snatch::util::atomic_write;
///
/// atomic_write("config.toml", b"key = \"value\"").unwrap();
/// ```
pub fn atomic_write(path: impl AsRef<Path>, content: &[u8]) -> Result<()> {
    let path = path.as_ref();

    // Get the parent directory for creating the temp file
    let parent = path.parent().ok_or_else(|| SnatchError::IoError {
        context: format!("Cannot determine parent directory for: {}", path.display()),
        source: io::Error::new(io::ErrorKind::InvalidInput, "No parent directory"),
    })?;

    // Ensure parent directory exists
    if !parent.exists() {
        std::fs::create_dir_all(parent).map_err(|e| {
            SnatchError::io(
                format!("Failed to create directory: {}", parent.display()),
                e,
            )
        })?;
    }

    // Create temp file in the same directory (ensures same filesystem for atomic rename)
    let mut temp_file = NamedTempFile::new_in(parent).map_err(|e| {
        SnatchError::io(
            format!("Failed to create temporary file in: {}", parent.display()),
            e,
        )
    })?;

    // Write content to temp file
    temp_file.write_all(content).map_err(|e| {
        SnatchError::io(
            format!("Failed to write to temporary file for: {}", path.display()),
            e,
        )
    })?;

    // Sync to disk before rename
    temp_file.flush().map_err(|e| {
        SnatchError::io(
            format!("Failed to flush temporary file for: {}", path.display()),
            e,
        )
    })?;

    // Atomically rename temp file to target
    temp_file.persist(path).map_err(|e| {
        SnatchError::io(
            format!("Failed to atomically write file: {}", path.display()),
            e.error,
        )
    })?;

    Ok(())
}

/// Atomically write content to a file using a writer function.
///
/// This is useful when you need to write using a `Write` trait object
/// rather than providing bytes directly.
///
/// # Arguments
///
/// * `path` - The target file path
/// * `write_fn` - A function that writes content to the provided writer
///
/// # Errors
///
/// Returns an error if any file operation fails.
///
/// # Example
///
/// ```rust,no_run
/// use claude_snatch::util::atomic_write_with;
/// use std::io::Write;
///
/// atomic_write_with("output.txt", |writer| {
///     writeln!(writer, "Hello, world!")
/// }).unwrap();
/// ```
pub fn atomic_write_with<F>(path: impl AsRef<Path>, write_fn: F) -> Result<()>
where
    F: FnOnce(&mut dyn Write) -> io::Result<()>,
{
    let path = path.as_ref();

    // Get the parent directory
    let parent = path.parent().ok_or_else(|| SnatchError::IoError {
        context: format!("Cannot determine parent directory for: {}", path.display()),
        source: io::Error::new(io::ErrorKind::InvalidInput, "No parent directory"),
    })?;

    // Ensure parent directory exists
    if !parent.exists() {
        std::fs::create_dir_all(parent).map_err(|e| {
            SnatchError::io(
                format!("Failed to create directory: {}", parent.display()),
                e,
            )
        })?;
    }

    // Create temp file in the same directory
    let mut temp_file = NamedTempFile::new_in(parent).map_err(|e| {
        SnatchError::io(
            format!("Failed to create temporary file in: {}", parent.display()),
            e,
        )
    })?;

    // Let the caller write content
    write_fn(&mut temp_file).map_err(|e| {
        SnatchError::io(
            format!("Failed to write content for: {}", path.display()),
            e,
        )
    })?;

    // Sync to disk
    temp_file.flush().map_err(|e| {
        SnatchError::io(
            format!("Failed to flush temporary file for: {}", path.display()),
            e,
        )
    })?;

    // Atomically rename
    temp_file.persist(path).map_err(|e| {
        SnatchError::io(
            format!("Failed to atomically write file: {}", path.display()),
            e.error,
        )
    })?;

    Ok(())
}

/// Create an atomic file writer that will atomically replace the target file on drop.
///
/// This struct wraps a `NamedTempFile` and provides a `finish()` method to
/// complete the atomic write. If `finish()` is not called, the temporary file
/// is discarded without modifying the target.
///
/// # Example
///
/// ```rust,no_run
/// use claude_snatch::util::AtomicFile;
/// use std::io::Write;
///
/// let mut atomic = AtomicFile::create("output.txt").unwrap();
/// writeln!(atomic.writer(), "Hello, world!").unwrap();
/// atomic.finish().unwrap();
/// ```
pub struct AtomicFile {
    temp_file: NamedTempFile,
    target_path: std::path::PathBuf,
}

impl AtomicFile {
    /// Create a new atomic file writer for the given target path.
    pub fn create(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();

        // Get the parent directory
        let parent = path.parent().ok_or_else(|| SnatchError::IoError {
            context: format!("Cannot determine parent directory for: {}", path.display()),
            source: io::Error::new(io::ErrorKind::InvalidInput, "No parent directory"),
        })?;

        // Ensure parent directory exists
        if !parent.exists() {
            std::fs::create_dir_all(parent).map_err(|e| {
                SnatchError::io(
                    format!("Failed to create directory: {}", parent.display()),
                    e,
                )
            })?;
        }

        // Create temp file in the same directory
        let temp_file = NamedTempFile::new_in(parent).map_err(|e| {
            SnatchError::io(
                format!("Failed to create temporary file in: {}", parent.display()),
                e,
            )
        })?;

        Ok(Self {
            temp_file,
            target_path: path.to_path_buf(),
        })
    }

    /// Get a mutable reference to the underlying writer.
    pub fn writer(&mut self) -> &mut NamedTempFile {
        &mut self.temp_file
    }

    /// Finish the atomic write by syncing and renaming the temp file.
    ///
    /// This consumes the `AtomicFile`. If this method is not called,
    /// the temporary file is discarded without affecting the target.
    pub fn finish(mut self) -> Result<()> {
        // Sync to disk
        self.temp_file.flush().map_err(|e| {
            SnatchError::io(
                format!("Failed to flush file: {}", self.target_path.display()),
                e,
            )
        })?;

        // Atomically rename
        self.temp_file.persist(&self.target_path).map_err(|e| {
            SnatchError::io(
                format!("Failed to atomically write: {}", self.target_path.display()),
                e.error,
            )
        })?;

        Ok(())
    }
}

// ============================================================================
// Sensitive Data Redaction
// ============================================================================

/// Patterns for sensitive data detection.
static PATTERNS: Lazy<RedactionPatterns> = Lazy::new(RedactionPatterns::new);

/// Configuration for what types of sensitive data to redact.
#[derive(Debug, Clone, Default)]
pub struct RedactionConfig {
    /// Redact API keys and tokens (Bearer, AWS, GitHub, etc.).
    pub api_keys: bool,
    /// Redact email addresses.
    pub emails: bool,
    /// Redact passwords (in connection strings, env vars, etc.).
    pub passwords: bool,
    /// Redact credit card numbers.
    pub credit_cards: bool,
    /// Redact IP addresses (IPv4 and IPv6).
    pub ip_addresses: bool,
    /// Redact phone numbers.
    pub phone_numbers: bool,
    /// Redact Social Security Numbers (US format).
    pub ssn: bool,
    /// Redact URLs with embedded credentials.
    pub url_credentials: bool,
    /// Redact AWS access keys and secret keys.
    pub aws_keys: bool,
    /// Custom placeholder for redacted content.
    pub placeholder: Option<String>,
}

impl RedactionConfig {
    /// Create a new config with all redaction disabled.
    pub fn none() -> Self {
        Self::default()
    }

    /// Create a config with all redaction enabled.
    pub fn all() -> Self {
        Self {
            api_keys: true,
            emails: true,
            passwords: true,
            credit_cards: true,
            ip_addresses: true,
            phone_numbers: true,
            ssn: true,
            url_credentials: true,
            aws_keys: true,
            placeholder: None,
        }
    }

    /// Create a config for common security-sensitive data.
    /// Includes: API keys, passwords, credit cards, SSN, AWS keys, URL credentials.
    /// Excludes: emails, IP addresses, phone numbers (which may be less sensitive).
    pub fn security() -> Self {
        Self {
            api_keys: true,
            emails: false,
            passwords: true,
            credit_cards: true,
            ip_addresses: false,
            phone_numbers: false,
            ssn: true,
            url_credentials: true,
            aws_keys: true,
            placeholder: None,
        }
    }

    /// Check if any redaction is enabled.
    #[must_use]
    pub fn is_enabled(&self) -> bool {
        self.api_keys
            || self.emails
            || self.passwords
            || self.credit_cards
            || self.ip_addresses
            || self.phone_numbers
            || self.ssn
            || self.url_credentials
            || self.aws_keys
    }

    /// Builder: set API keys redaction.
    #[must_use]
    pub fn with_api_keys(mut self, enabled: bool) -> Self {
        self.api_keys = enabled;
        self
    }

    /// Builder: set email redaction.
    #[must_use]
    pub fn with_emails(mut self, enabled: bool) -> Self {
        self.emails = enabled;
        self
    }

    /// Builder: set custom placeholder.
    #[must_use]
    pub fn with_placeholder(mut self, placeholder: impl Into<String>) -> Self {
        self.placeholder = Some(placeholder.into());
        self
    }
}

/// Compiled regex patterns for sensitive data.
struct RedactionPatterns {
    api_key: Regex,
    bearer_token: Regex,
    email: Regex,
    password_env: Regex,
    password_url: Regex,
    credit_card: Regex,
    ipv4: Regex,
    ipv6: Regex,
    phone: Regex,
    ssn: Regex,
    url_credentials: Regex,
    aws_access_key: Regex,
    aws_secret_key: Regex,
    github_token: Regex,
    generic_secret: Regex,
}

impl RedactionPatterns {
    fn new() -> Self {
        Self {
            // API keys: sk-xxx, pk-xxx, api_key=xxx, apikey=xxx, etc.
            api_key: Regex::new(
                r#"(?i)(api[_-]?key|apikey|secret[_-]?key|private[_-]?key|access[_-]?key)\s*[=:]\s*['"]?([a-zA-Z0-9_-]{16,})['"]?"#
            ).unwrap(),

            // Bearer tokens
            bearer_token: Regex::new(
                r"(?i)Bearer\s+[a-zA-Z0-9_.-]+"
            ).unwrap(),

            // Email addresses
            email: Regex::new(
                r"[a-zA-Z0-9._%+-]+@[a-zA-Z0-9.-]+\.[a-zA-Z]{2,}"
            ).unwrap(),

            // Password in environment variables: PASSWORD=xxx, PASS=xxx
            password_env: Regex::new(
                r#"(?i)(password|passwd|pass|pwd)\s*[=:]\s*['"]?([^\s'"]{3,})['"]?"#
            ).unwrap(),

            // Password in URLs: ://user:password@
            password_url: Regex::new(
                r"://[^:/@]+:([^@]+)@"
            ).unwrap(),

            // Credit card numbers (basic pattern for 13-19 digit numbers with optional separators)
            credit_card: Regex::new(
                r"\b(?:\d{4}[-\s]?){3,4}\d{1,4}\b"
            ).unwrap(),

            // IPv4 addresses
            ipv4: Regex::new(
                r"\b(?:(?:25[0-5]|2[0-4][0-9]|[01]?[0-9][0-9]?)\.){3}(?:25[0-5]|2[0-4][0-9]|[01]?[0-9][0-9]?)\b"
            ).unwrap(),

            // IPv6 addresses (simplified pattern)
            ipv6: Regex::new(
                r"(?i)\b(?:[0-9a-f]{1,4}:){7}[0-9a-f]{1,4}\b|(?:[0-9a-f]{1,4}:){1,7}:|(?:[0-9a-f]{1,4}:){1,6}:[0-9a-f]{1,4}"
            ).unwrap(),

            // Phone numbers (US format, various patterns)
            phone: Regex::new(
                r"\b(?:\+1[-.\s]?)?(?:\(?[0-9]{3}\)?[-.\s]?)?[0-9]{3}[-.\s]?[0-9]{4}\b"
            ).unwrap(),

            // US Social Security Numbers
            ssn: Regex::new(
                r"\b\d{3}[-\s]?\d{2}[-\s]?\d{4}\b"
            ).unwrap(),

            // URLs with embedded credentials
            url_credentials: Regex::new(
                r"(?i)(https?|ftp|ssh)://[^:/@\s]+:[^@\s]+@[^\s]+"
            ).unwrap(),

            // AWS Access Key ID (starts with AKIA, ABIA, ACCA, ASIA)
            aws_access_key: Regex::new(
                r"\b(?:A3T[A-Z0-9]|AKIA|ABIA|ACCA|ASIA)[A-Z0-9]{16}\b"
            ).unwrap(),

            // AWS Secret Access Key (40 character base64)
            aws_secret_key: Regex::new(
                r#"(?i)(aws[_-]?secret[_-]?access[_-]?key|secret[_-]?access[_-]?key)\s*[=:]\s*['"]?([a-zA-Z0-9/+=]{40})['"]?"#
            ).unwrap(),

            // GitHub tokens (ghp_, gho_, ghu_, ghs_, ghr_)
            github_token: Regex::new(
                r"\b(gh[pousr]_[a-zA-Z0-9]{36,})\b"
            ).unwrap(),

            // Generic secrets/tokens (long alphanumeric strings after key-like identifiers)
            generic_secret: Regex::new(
                r#"(?i)(token|secret|credential|auth)\s*[=:]\s*['"]?([a-zA-Z0-9_-]{20,})['"]?"#
            ).unwrap(),
        }
    }
}

/// Redact sensitive data from text based on the provided configuration.
///
/// # Arguments
///
/// * `text` - The text to redact
/// * `config` - Configuration specifying what types of data to redact
///
/// # Returns
///
/// The text with sensitive data replaced by redaction placeholders.
///
/// # Example
///
/// ```rust
/// use claude_snatch::util::{redact_sensitive, RedactionConfig};
///
/// let config = RedactionConfig::all();
/// let text = "The api_key=sk-abc123xyz789abcd must be kept secret";
/// let redacted = redact_sensitive(text, &config);
/// assert!(!redacted.contains("sk-abc123xyz789abcd"));
/// ```
pub fn redact_sensitive<'a>(text: &'a str, config: &RedactionConfig) -> Cow<'a, str> {
    if !config.is_enabled() {
        return Cow::Borrowed(text);
    }

    let placeholder = config.placeholder.as_deref().unwrap_or("[REDACTED]");
    let mut result = text.to_string();

    // Apply redactions in order of specificity (more specific patterns first)

    // AWS keys (specific patterns)
    if config.aws_keys {
        result = PATTERNS.aws_access_key.replace_all(&result, placeholder.to_string()).to_string();
        result = PATTERNS.aws_secret_key.replace_all(&result, |caps: &regex::Captures| {
            format!("{}={}", &caps[1], placeholder)
        }).to_string();
    }

    // GitHub tokens
    if config.api_keys {
        result = PATTERNS.github_token.replace_all(&result, placeholder).to_string();
    }

    // URL credentials (before general URL patterns)
    if config.url_credentials {
        result = PATTERNS.url_credentials.replace_all(&result, |caps: &regex::Captures| {
            // Replace just the credentials part, keep the URL structure
            let url = &caps[0];
            PATTERNS.password_url.replace(url, |inner: &regex::Captures| {
                format!("://{}@", placeholder).replace(&inner[1], placeholder)
            }).to_string()
        }).to_string();
    }

    // Password patterns
    if config.passwords {
        result = PATTERNS.password_env.replace_all(&result, |caps: &regex::Captures| {
            format!("{}={}", &caps[1], placeholder)
        }).to_string();
    }

    // API keys and tokens
    if config.api_keys {
        result = PATTERNS.bearer_token.replace_all(&result, format!("Bearer {}", placeholder)).to_string();
        result = PATTERNS.api_key.replace_all(&result, |caps: &regex::Captures| {
            format!("{}={}", &caps[1], placeholder)
        }).to_string();
        result = PATTERNS.generic_secret.replace_all(&result, |caps: &regex::Captures| {
            format!("{}={}", &caps[1], placeholder)
        }).to_string();
    }

    // SSN (before phone to avoid conflicts)
    if config.ssn {
        result = PATTERNS.ssn.replace_all(&result, placeholder).to_string();
    }

    // Credit cards
    if config.credit_cards {
        result = PATTERNS.credit_card.replace_all(&result, placeholder).to_string();
    }

    // Email addresses
    if config.emails {
        result = PATTERNS.email.replace_all(&result, placeholder).to_string();
    }

    // IP addresses
    if config.ip_addresses {
        result = PATTERNS.ipv4.replace_all(&result, placeholder).to_string();
        result = PATTERNS.ipv6.replace_all(&result, placeholder).to_string();
    }

    // Phone numbers
    if config.phone_numbers {
        result = PATTERNS.phone.replace_all(&result, placeholder).to_string();
    }

    if result == text {
        Cow::Borrowed(text)
    } else {
        Cow::Owned(result)
    }
}

/// Check if text contains potential sensitive data.
///
/// This is a quick check without redaction, useful for warnings.
///
/// # Arguments
///
/// * `text` - The text to check
/// * `config` - Configuration specifying what types of data to check for
///
/// # Returns
///
/// A list of detected sensitive data types.
pub fn detect_sensitive(text: &str, config: &RedactionConfig) -> Vec<SensitiveDataType> {
    let mut detected = Vec::new();

    if config.api_keys
        && (PATTERNS.api_key.is_match(text)
            || PATTERNS.bearer_token.is_match(text)
            || PATTERNS.github_token.is_match(text)
            || PATTERNS.generic_secret.is_match(text))
    {
        detected.push(SensitiveDataType::ApiKey);
    }

    if config.aws_keys
        && (PATTERNS.aws_access_key.is_match(text) || PATTERNS.aws_secret_key.is_match(text))
    {
        detected.push(SensitiveDataType::AwsCredential);
    }

    if config.emails && PATTERNS.email.is_match(text) {
        detected.push(SensitiveDataType::Email);
    }

    if config.passwords
        && (PATTERNS.password_env.is_match(text) || PATTERNS.password_url.is_match(text))
    {
        detected.push(SensitiveDataType::Password);
    }

    if config.credit_cards && PATTERNS.credit_card.is_match(text) {
        detected.push(SensitiveDataType::CreditCard);
    }

    if config.ssn && PATTERNS.ssn.is_match(text) {
        detected.push(SensitiveDataType::Ssn);
    }

    if config.ip_addresses && (PATTERNS.ipv4.is_match(text) || PATTERNS.ipv6.is_match(text)) {
        detected.push(SensitiveDataType::IpAddress);
    }

    if config.phone_numbers && PATTERNS.phone.is_match(text) {
        detected.push(SensitiveDataType::PhoneNumber);
    }

    if config.url_credentials && PATTERNS.url_credentials.is_match(text) {
        detected.push(SensitiveDataType::UrlCredential);
    }

    detected
}

/// Preview redactions without actually removing data.
///
/// This function wraps detected sensitive data with preview markers instead of
/// replacing it, allowing users to see what would be redacted.
///
/// # Arguments
///
/// * `text` - The text to analyze
/// * `config` - Configuration specifying what types of data to detect
///
/// # Returns
///
/// The text with sensitive data wrapped in preview markers like:
/// `[WOULD-REDACT:ApiKey]secret-value[/WOULD-REDACT]`
///
/// # Example
///
/// ```rust
/// use claude_snatch::util::{preview_redactions, RedactionConfig};
///
/// let config = RedactionConfig::all();
/// let text = "The api_key=sk-abc123xyz789abcd must be kept secret";
/// let preview = preview_redactions(text, &config);
/// assert!(preview.contains("[WOULD-REDACT:"));
/// assert!(preview.contains("sk-abc123xyz789abcd")); // Value is still visible
/// ```
pub fn preview_redactions<'a>(text: &'a str, config: &RedactionConfig) -> Cow<'a, str> {
    if !config.is_enabled() {
        return Cow::Borrowed(text);
    }

    let mut result = text.to_string();
    let mut had_match = false;

    // Track positions to avoid double-wrapping overlapping matches
    // We process patterns in order and wrap matches

    // AWS keys (specific patterns)
    if config.aws_keys {
        if PATTERNS.aws_access_key.is_match(&result) {
            result = PATTERNS.aws_access_key.replace_all(&result, |caps: &regex::Captures| {
                format!("[WOULD-REDACT:AwsKey]{}[/WOULD-REDACT]", &caps[0])
            }).to_string();
            had_match = true;
        }
        if PATTERNS.aws_secret_key.is_match(&result) {
            result = PATTERNS.aws_secret_key.replace_all(&result, |caps: &regex::Captures| {
                format!("{}=[WOULD-REDACT:AwsSecret]{}[/WOULD-REDACT]", &caps[1], &caps[2])
            }).to_string();
            had_match = true;
        }
    }

    // GitHub tokens
    if config.api_keys && PATTERNS.github_token.is_match(&result) {
        result = PATTERNS.github_token.replace_all(&result, |caps: &regex::Captures| {
            format!("[WOULD-REDACT:GitHubToken]{}[/WOULD-REDACT]", &caps[0])
        }).to_string();
        had_match = true;
    }

    // URL credentials
    if config.url_credentials && PATTERNS.url_credentials.is_match(&result) {
        result = PATTERNS.url_credentials.replace_all(&result, |caps: &regex::Captures| {
            format!("[WOULD-REDACT:UrlCredential]{}[/WOULD-REDACT]", &caps[0])
        }).to_string();
        had_match = true;
    }

    // Password patterns
    if config.passwords && PATTERNS.password_env.is_match(&result) {
        result = PATTERNS.password_env.replace_all(&result, |caps: &regex::Captures| {
            format!("{}=[WOULD-REDACT:Password]{}[/WOULD-REDACT]", &caps[1], &caps[2])
        }).to_string();
        had_match = true;
    }

    // API keys and tokens
    if config.api_keys {
        if PATTERNS.bearer_token.is_match(&result) {
            result = PATTERNS.bearer_token.replace_all(&result, |caps: &regex::Captures| {
                format!("[WOULD-REDACT:BearerToken]{}[/WOULD-REDACT]", &caps[0])
            }).to_string();
            had_match = true;
        }
        if PATTERNS.api_key.is_match(&result) {
            result = PATTERNS.api_key.replace_all(&result, |caps: &regex::Captures| {
                format!("{}=[WOULD-REDACT:ApiKey]{}[/WOULD-REDACT]", &caps[1], &caps[2])
            }).to_string();
            had_match = true;
        }
        if PATTERNS.generic_secret.is_match(&result) {
            result = PATTERNS.generic_secret.replace_all(&result, |caps: &regex::Captures| {
                format!("{}=[WOULD-REDACT:Secret]{}[/WOULD-REDACT]", &caps[1], &caps[2])
            }).to_string();
            had_match = true;
        }
    }

    // SSN
    if config.ssn && PATTERNS.ssn.is_match(&result) {
        result = PATTERNS.ssn.replace_all(&result, |caps: &regex::Captures| {
            format!("[WOULD-REDACT:SSN]{}[/WOULD-REDACT]", &caps[0])
        }).to_string();
        had_match = true;
    }

    // Credit cards
    if config.credit_cards && PATTERNS.credit_card.is_match(&result) {
        result = PATTERNS.credit_card.replace_all(&result, |caps: &regex::Captures| {
            format!("[WOULD-REDACT:CreditCard]{}[/WOULD-REDACT]", &caps[0])
        }).to_string();
        had_match = true;
    }

    // Email addresses
    if config.emails && PATTERNS.email.is_match(&result) {
        result = PATTERNS.email.replace_all(&result, |caps: &regex::Captures| {
            format!("[WOULD-REDACT:Email]{}[/WOULD-REDACT]", &caps[0])
        }).to_string();
        had_match = true;
    }

    // IP addresses
    if config.ip_addresses {
        if PATTERNS.ipv4.is_match(&result) {
            result = PATTERNS.ipv4.replace_all(&result, |caps: &regex::Captures| {
                format!("[WOULD-REDACT:IPv4]{}[/WOULD-REDACT]", &caps[0])
            }).to_string();
            had_match = true;
        }
        if PATTERNS.ipv6.is_match(&result) {
            result = PATTERNS.ipv6.replace_all(&result, |caps: &regex::Captures| {
                format!("[WOULD-REDACT:IPv6]{}[/WOULD-REDACT]", &caps[0])
            }).to_string();
            had_match = true;
        }
    }

    // Phone numbers
    if config.phone_numbers && PATTERNS.phone.is_match(&result) {
        result = PATTERNS.phone.replace_all(&result, |caps: &regex::Captures| {
            format!("[WOULD-REDACT:Phone]{}[/WOULD-REDACT]", &caps[0])
        }).to_string();
        had_match = true;
    }

    if had_match {
        Cow::Owned(result)
    } else {
        Cow::Borrowed(text)
    }
}

/// Types of sensitive data that can be detected.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SensitiveDataType {
    /// API keys and tokens.
    ApiKey,
    /// AWS credentials.
    AwsCredential,
    /// Email addresses.
    Email,
    /// Passwords.
    Password,
    /// Credit card numbers.
    CreditCard,
    /// Social Security Numbers.
    Ssn,
    /// IP addresses.
    IpAddress,
    /// Phone numbers.
    PhoneNumber,
    /// URLs with embedded credentials.
    UrlCredential,
}

impl SensitiveDataType {
    /// Get a human-readable description of the data type.
    #[must_use]
    pub fn description(&self) -> &'static str {
        match self {
            Self::ApiKey => "API key or token",
            Self::AwsCredential => "AWS credential",
            Self::Email => "email address",
            Self::Password => "password",
            Self::CreditCard => "credit card number",
            Self::Ssn => "Social Security Number",
            Self::IpAddress => "IP address",
            Self::PhoneNumber => "phone number",
            Self::UrlCredential => "URL with credentials",
        }
    }
}

/// Pager support for large output.
///
/// Provides utilities for piping output through a pager (like `less` or `more`)
/// on Unix systems.
pub mod pager {
    use std::io::{self, Write};
    use std::process::{Command, Stdio};

    /// Get the preferred pager from environment.
    ///
    /// Checks `PAGER` environment variable, falls back to `less` or `more`.
    #[must_use]
    pub fn get_pager() -> Option<String> {
        // Check PAGER environment variable first
        if let Ok(pager) = std::env::var("PAGER") {
            if !pager.is_empty() {
                return Some(pager);
            }
        }

        // Try common pagers
        for pager in &["less", "more"] {
            if Command::new("which")
                .arg(pager)
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .is_ok_and(|s| s.success())
            {
                return Some(pager.to_string());
            }
        }

        None
    }

    /// Pipe content through the system pager.
    ///
    /// If no pager is available or piping fails, falls back to stdout.
    pub fn pipe_through_pager(content: &str) -> io::Result<()> {
        let Some(pager_cmd) = get_pager() else {
            // No pager available, print directly
            print!("{}", content);
            return Ok(());
        };

        // Parse pager command and arguments
        let parts: Vec<&str> = pager_cmd.split_whitespace().collect();
        let (pager, args) = parts.split_first().unwrap_or((&"less", &[]));

        match Command::new(pager)
            .args(args.iter())
            .stdin(Stdio::piped())
            .spawn()
        {
            Ok(mut child) => {
                if let Some(stdin) = child.stdin.as_mut() {
                    let _ = stdin.write_all(content.as_bytes());
                }
                let _ = child.wait();
                Ok(())
            }
            Err(_) => {
                // Pager failed, fall back to stdout
                print!("{}", content);
                Ok(())
            }
        }
    }

    /// Output writer that can optionally use a pager.
    ///
    /// Collects output in memory and pipes through pager when done.
    pub struct PagerWriter {
        buffer: Vec<u8>,
        use_pager: bool,
    }

    impl PagerWriter {
        /// Create a new pager writer.
        #[must_use]
        pub fn new(use_pager: bool) -> Self {
            Self {
                buffer: Vec::new(),
                use_pager,
            }
        }

        /// Flush the buffer through the pager or stdout.
        pub fn finish(self) -> io::Result<()> {
            let content = String::from_utf8_lossy(&self.buffer);
            if self.use_pager && !self.buffer.is_empty() {
                pipe_through_pager(&content)
            } else {
                print!("{}", content);
                Ok(())
            }
        }
    }

    impl Write for PagerWriter {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.buffer.write(buf)
        }

        fn flush(&mut self) -> io::Result<()> {
            // Don't flush to pager yet, wait for finish()
            Ok(())
        }
    }
}

// ============================================================================
// Display Utilities
// ============================================================================

/// Truncate a path or string for display, keeping the end (most informative part).
///
/// If the string is longer than `max_len`, returns `...{last max_len-3 chars}`.
/// This is useful for paths where the end (filename or project name) is most relevant.
///
/// # Arguments
///
/// * `text` - The string to truncate
/// * `max_len` - Maximum display length (must be > 3)
///
/// # Example
///
/// ```
/// use claude_snatch::util::truncate_path;
///
/// assert_eq!(truncate_path("/home/user/project", 30), "/home/user/project");
/// assert_eq!(truncate_path("/home/user/very/long/project/path", 20), "...long/project/path");
/// ```
pub fn truncate_path(text: &str, max_len: usize) -> String {
    if max_len <= 3 {
        return text.chars().take(max_len).collect();
    }

    if text.len() <= max_len {
        text.to_string()
    } else {
        format!("...{}", &text[text.len() - (max_len - 3)..])
    }
}

/// Truncate a string for display, keeping the beginning.
///
/// If the string is longer than `max_len`, returns `{first max_len-3 chars}...`.
///
/// # Arguments
///
/// * `text` - The string to truncate
/// * `max_len` - Maximum display length (must be > 3)
///
/// # Example
///
/// ```
/// use claude_snatch::util::truncate_text;
///
/// assert_eq!(truncate_text("short text", 30), "short text");
/// assert_eq!(truncate_text("this is a very long text that needs truncation", 20), "this is a very lo...");
/// ```
pub fn truncate_text(text: &str, max_len: usize) -> String {
    if max_len <= 3 {
        return text.chars().take(max_len).collect();
    }

    if text.len() <= max_len {
        text.to_string()
    } else {
        format!("{}...", &text[..max_len - 3])
    }
}

// ============================================================================
// Sparkline Visualization
// ============================================================================

/// Unicode block characters for sparklines (from lowest to highest).
const SPARKLINE_BLOCKS: [char; 8] = ['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];

/// Generate a sparkline visualization from a list of values.
///
/// Returns a Unicode string using block characters to represent relative values.
///
/// # Arguments
///
/// * `values` - Slice of values to visualize
/// * `min_val` - Optional minimum value for scaling (defaults to data minimum)
/// * `max_val` - Optional maximum value for scaling (defaults to data maximum)
///
/// # Example
///
/// ```
/// use claude_snatch::util::sparkline;
/// let data = [1.0, 3.0, 5.0, 2.0, 7.0];
/// let spark = sparkline(&data, None, None);
/// // Returns something like "▁▃▅▂█"
/// ```
pub fn sparkline(values: &[f64], min_val: Option<f64>, max_val: Option<f64>) -> String {
    if values.is_empty() {
        return String::new();
    }

    // Calculate min/max
    let min = min_val.unwrap_or_else(|| {
        values.iter().copied().fold(f64::INFINITY, f64::min)
    });
    let max = max_val.unwrap_or_else(|| {
        values.iter().copied().fold(f64::NEG_INFINITY, f64::max)
    });

    // Handle constant values
    if (max - min).abs() < f64::EPSILON {
        return SPARKLINE_BLOCKS[4].to_string().repeat(values.len());
    }

    let range = max - min;
    let block_count = SPARKLINE_BLOCKS.len() as f64;

    values
        .iter()
        .map(|&v| {
            let normalized = ((v - min) / range).clamp(0.0, 1.0);
            let index = ((normalized * (block_count - 1.0)).round() as usize).min(SPARKLINE_BLOCKS.len() - 1);
            SPARKLINE_BLOCKS[index]
        })
        .collect()
}

/// Generate a sparkline from u64 values.
pub fn sparkline_u64(values: &[u64]) -> String {
    let float_values: Vec<f64> = values.iter().map(|&v| v as f64).collect();
    sparkline(&float_values, None, None)
}

/// Generate a labeled sparkline with min/max values.
///
/// Returns a string like "▁▃▅▂█ (0 - 100)"
pub fn sparkline_with_range(values: &[f64], label: Option<&str>) -> String {
    if values.is_empty() {
        return String::new();
    }

    let min = values.iter().copied().fold(f64::INFINITY, f64::min);
    let max = values.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    let spark = sparkline(values, Some(min), Some(max));

    match label {
        Some(l) => format!("{spark} {l}"),
        None => {
            if min == max {
                format!("{spark} ({min:.0})")
            } else {
                format!("{spark} ({min:.0}-{max:.0})")
            }
        }
    }
}

/// Format a sparkline with a trend indicator.
///
/// Shows an arrow indicating the overall trend (↑ ↓ →).
pub fn sparkline_with_trend(values: &[f64]) -> String {
    if values.is_empty() {
        return String::new();
    }

    let spark = sparkline(values, None, None);

    // Calculate simple trend based on first and last value
    let trend = if values.len() < 2 {
        "→"
    } else {
        let first = values[0];
        let last = values[values.len() - 1];
        let threshold = (values.iter().copied().fold(f64::NEG_INFINITY, f64::max)
            - values.iter().copied().fold(f64::INFINITY, f64::min)) * 0.1;

        if last > first + threshold {
            "↑"
        } else if last < first - threshold {
            "↓"
        } else {
            "→"
        }
    };

    format!("{spark} {trend}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;
    use tempfile::tempdir;

    #[test]
    fn test_atomic_write() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("test.txt");

        atomic_write(&path, b"Hello, world!").unwrap();

        let mut content = String::new();
        std::fs::File::open(&path)
            .unwrap()
            .read_to_string(&mut content)
            .unwrap();
        assert_eq!(content, "Hello, world!");
    }

    #[test]
    fn test_atomic_write_creates_parent_dirs() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("nested").join("dir").join("test.txt");

        atomic_write(&path, b"Nested content").unwrap();

        assert!(path.exists());
    }

    #[test]
    fn test_atomic_write_with_closure() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("closure.txt");

        atomic_write_with(&path, |w| {
            writeln!(w, "Line 1")?;
            writeln!(w, "Line 2")
        })
        .unwrap();

        let content = std::fs::read_to_string(&path).unwrap();
        assert_eq!(content, "Line 1\nLine 2\n");
    }

    #[test]
    fn test_atomic_file() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("atomic.txt");

        let mut atomic = AtomicFile::create(&path).unwrap();
        writeln!(atomic.writer(), "Atomic write").unwrap();
        atomic.finish().unwrap();

        let content = std::fs::read_to_string(&path).unwrap();
        assert_eq!(content, "Atomic write\n");
    }

    #[test]
    fn test_atomic_file_abort() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("aborted.txt");

        // Write initial content
        std::fs::write(&path, "Original content").unwrap();

        // Start atomic write but don't finish
        {
            let mut atomic = AtomicFile::create(&path).unwrap();
            writeln!(atomic.writer(), "New content").unwrap();
            // Drop without calling finish()
        }

        // Original content should remain
        let content = std::fs::read_to_string(&path).unwrap();
        assert_eq!(content, "Original content");
    }

    // Redaction tests

    #[test]
    fn test_redaction_config_none() {
        let config = RedactionConfig::none();
        assert!(!config.is_enabled());
    }

    #[test]
    fn test_redaction_config_all() {
        let config = RedactionConfig::all();
        assert!(config.is_enabled());
        assert!(config.api_keys);
        assert!(config.emails);
        assert!(config.passwords);
    }

    #[test]
    fn test_redaction_config_security() {
        let config = RedactionConfig::security();
        assert!(config.is_enabled());
        assert!(config.api_keys);
        assert!(!config.emails); // Emails not included in security preset
        assert!(config.passwords);
    }

    #[test]
    fn test_redact_api_key() {
        let config = RedactionConfig::none().with_api_keys(true);
        let text = "api_key=sk-1234567890abcdefghij";
        let redacted = redact_sensitive(text, &config);
        assert!(redacted.contains("[REDACTED]"));
        assert!(!redacted.contains("sk-1234567890abcdefghij"));
    }

    #[test]
    fn test_redact_bearer_token() {
        let config = RedactionConfig::none().with_api_keys(true);
        let text = "Authorization: Bearer eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9.test";
        let redacted = redact_sensitive(text, &config);
        assert!(redacted.contains("Bearer [REDACTED]"));
        assert!(!redacted.contains("eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9.test"));
    }

    #[test]
    fn test_redact_email() {
        let config = RedactionConfig::none().with_emails(true);
        let text = "Contact me at user@example.com for more info";
        let redacted = redact_sensitive(text, &config);
        assert!(redacted.contains("[REDACTED]"));
        assert!(!redacted.contains("user@example.com"));
    }

    #[test]
    fn test_redact_password_env() {
        let config = RedactionConfig::all();
        let text = "PASSWORD=supersecret123";
        let redacted = redact_sensitive(text, &config);
        assert!(redacted.contains("PASSWORD=[REDACTED]"));
        assert!(!redacted.contains("supersecret123"));
    }

    #[test]
    fn test_redact_ip_address() {
        let config = RedactionConfig::all();
        let text = "Server at 192.168.1.100:8080";
        let redacted = redact_sensitive(text, &config);
        assert!(redacted.contains("[REDACTED]"));
        assert!(!redacted.contains("192.168.1.100"));
    }

    #[test]
    fn test_redact_github_token() {
        let config = RedactionConfig::none().with_api_keys(true);
        let text = "GITHUB_TOKEN=ghp_1234567890abcdefghijklmnopqrstuvwxyz12";
        let redacted = redact_sensitive(text, &config);
        assert!(redacted.contains("[REDACTED]"));
        assert!(!redacted.contains("ghp_"));
    }

    #[test]
    fn test_redact_custom_placeholder() {
        let config = RedactionConfig::all().with_placeholder("***");
        let text = "email: test@example.com";
        let redacted = redact_sensitive(text, &config);
        assert!(redacted.contains("***"));
        assert!(!redacted.contains("test@example.com"));
    }

    #[test]
    fn test_redact_no_change() {
        let config = RedactionConfig::all();
        let text = "This is just normal text with no sensitive data.";
        let redacted = redact_sensitive(text, &config);
        assert_eq!(redacted.as_ref(), text);
    }

    #[test]
    fn test_detect_sensitive_api_key() {
        let config = RedactionConfig::all();
        let text = "token=abcdefghijklmnopqrstuvwxyz";
        let detected = detect_sensitive(text, &config);
        assert!(detected.contains(&SensitiveDataType::ApiKey));
    }

    #[test]
    fn test_detect_sensitive_email() {
        let config = RedactionConfig::all();
        let text = "Contact: admin@company.org";
        let detected = detect_sensitive(text, &config);
        assert!(detected.contains(&SensitiveDataType::Email));
    }

    #[test]
    fn test_detect_sensitive_empty() {
        let config = RedactionConfig::all();
        let text = "No sensitive data here";
        let detected = detect_sensitive(text, &config);
        assert!(detected.is_empty());
    }

    #[test]
    fn test_sensitive_data_type_description() {
        assert_eq!(SensitiveDataType::ApiKey.description(), "API key or token");
        assert_eq!(SensitiveDataType::Email.description(), "email address");
        assert_eq!(SensitiveDataType::Ssn.description(), "Social Security Number");
    }

    // Preview redaction tests

    #[test]
    fn test_preview_redactions_api_key() {
        let config = RedactionConfig::none().with_api_keys(true);
        let text = "api_key=sk-1234567890abcdefghij";
        let preview = super::preview_redactions(text, &config);
        assert!(preview.contains("[WOULD-REDACT:ApiKey]"));
        assert!(preview.contains("[/WOULD-REDACT]"));
        assert!(preview.contains("sk-1234567890abcdefghij")); // Value still visible
    }

    #[test]
    fn test_preview_redactions_email() {
        let config = RedactionConfig::none().with_emails(true);
        let text = "Contact me at user@example.com for more info";
        let preview = super::preview_redactions(text, &config);
        assert!(preview.contains("[WOULD-REDACT:Email]"));
        assert!(preview.contains("user@example.com")); // Value still visible
    }

    #[test]
    fn test_preview_redactions_password() {
        let config = RedactionConfig::all();
        let text = "PASSWORD=supersecret123";
        let preview = super::preview_redactions(text, &config);
        assert!(preview.contains("[WOULD-REDACT:Password]"));
        assert!(preview.contains("supersecret123")); // Value still visible
    }

    #[test]
    fn test_preview_redactions_no_match() {
        let config = RedactionConfig::all();
        let text = "This is just normal text with no sensitive data.";
        let preview = super::preview_redactions(text, &config);
        assert_eq!(preview.as_ref(), text); // No changes
        assert!(!preview.contains("[WOULD-REDACT"));
    }

    #[test]
    fn test_preview_redactions_disabled() {
        let config = RedactionConfig::none();
        let text = "api_key=sk-1234567890abcdefghij";
        let preview = super::preview_redactions(text, &config);
        assert_eq!(preview.as_ref(), text); // No changes when disabled
    }

    // Sparkline tests

    #[test]
    fn test_sparkline_basic() {
        let values = [1.0, 3.0, 5.0, 2.0, 7.0];
        let spark = super::sparkline(&values, None, None);
        assert_eq!(spark.chars().count(), 5);
        // First value should be lowest block, last should be highest
        let chars: Vec<char> = spark.chars().collect();
        assert_eq!(chars[0], '▁'); // 1.0 is the minimum
        assert_eq!(chars[4], '█'); // 7.0 is the maximum
    }

    #[test]
    fn test_sparkline_empty() {
        let values: [f64; 0] = [];
        let spark = super::sparkline(&values, None, None);
        assert!(spark.is_empty());
    }

    #[test]
    fn test_sparkline_constant() {
        let values = [5.0, 5.0, 5.0, 5.0];
        let spark = super::sparkline(&values, None, None);
        // All same value should produce middle blocks
        assert_eq!(spark.chars().count(), 4);
        let chars: Vec<char> = spark.chars().collect();
        assert!(chars.iter().all(|&c| c == chars[0]));
    }

    #[test]
    fn test_sparkline_u64() {
        let values = [100, 200, 300, 400, 500];
        let spark = super::sparkline_u64(&values);
        assert_eq!(spark.chars().count(), 5);
        let chars: Vec<char> = spark.chars().collect();
        assert_eq!(chars[0], '▁');
        assert_eq!(chars[4], '█');
    }

    #[test]
    fn test_sparkline_with_range() {
        let values = [10.0, 50.0, 100.0];
        let spark = super::sparkline_with_range(&values, None);
        // Should contain the sparkline plus range info
        assert!(spark.contains('▁'));
        assert!(spark.contains('█'));
        assert!(spark.contains("10"));
        assert!(spark.contains("100"));
    }

    #[test]
    fn test_sparkline_with_range_label() {
        let values = [10.0, 50.0, 100.0];
        let spark = super::sparkline_with_range(&values, Some("tokens"));
        assert!(spark.contains("tokens"));
    }

    #[test]
    fn test_sparkline_with_trend_up() {
        let values = [10.0, 50.0, 100.0];
        let spark = super::sparkline_with_trend(&values);
        assert!(spark.contains('↑'));
    }

    #[test]
    fn test_sparkline_with_trend_down() {
        let values = [100.0, 50.0, 10.0];
        let spark = super::sparkline_with_trend(&values);
        assert!(spark.contains('↓'));
    }

    #[test]
    fn test_sparkline_with_trend_stable() {
        let values = [50.0, 51.0, 50.0, 49.0, 50.0];
        let spark = super::sparkline_with_trend(&values);
        assert!(spark.contains('→'));
    }

    // Display utility tests

    #[test]
    fn test_truncate_path_short() {
        assert_eq!(super::truncate_path("/home/user/project", 30), "/home/user/project");
    }

    #[test]
    fn test_truncate_path_long() {
        let path = "/home/user/very/long/project/path/name";
        let truncated = super::truncate_path(path, 20);
        assert_eq!(truncated.len(), 20);
        assert!(truncated.starts_with("..."));
        assert!(truncated.ends_with("ath/name")); // "...oject/path/name" -> ends with "ath/name"
    }

    #[test]
    fn test_truncate_path_exact() {
        let path = "exactly20characters!";
        assert_eq!(super::truncate_path(path, 20), path);
    }

    #[test]
    fn test_truncate_text_short() {
        assert_eq!(super::truncate_text("short text", 30), "short text");
    }

    #[test]
    fn test_truncate_text_long() {
        let text = "this is a very long text that needs truncation";
        let truncated = super::truncate_text(text, 20);
        assert_eq!(truncated.len(), 20);
        assert!(truncated.ends_with("..."));
        assert!(truncated.starts_with("this is a very lo"));
    }

    #[test]
    fn test_truncate_text_exact() {
        let text = "exactly20characters!";
        assert_eq!(super::truncate_text(text, 20), text);
    }
}
