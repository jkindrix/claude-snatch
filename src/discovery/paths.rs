//! Cross-platform path discovery and encoding utilities.
//!
//! This module handles:
//! - Auto-discovery of Claude Code data directory
//! - Platform-specific paths (Linux, macOS, Windows, WSL)
//! - Project path encoding/decoding (/ → -)

use std::path::{Path, PathBuf};

use crate::error::{Result, SnatchError};
use crate::CLAUDE_DIR_NAME;

/// Discover the Claude Code data directory.
///
/// Checks locations in order:
/// 1. Environment variable `CLAUDE_CODE_DIR`
/// 2. XDG config directory (`~/.config/claude/`)
/// 3. Home directory (`~/.claude/`)
/// 4. Windows-specific (`%USERPROFILE%\.claude\`)
pub fn discover_claude_directory() -> Result<PathBuf> {
    // Check environment variable first
    if let Ok(env_path) = std::env::var("CLAUDE_CODE_DIR") {
        let path = PathBuf::from(env_path);
        if path.exists() {
            return Ok(path);
        }
    }

    // Get home directory
    let home = home_directory().ok_or_else(|| SnatchError::ClaudeDirectoryNotFound {
        expected_path: PathBuf::from("~/.claude"),
    })?;

    // Check XDG config directory first (Linux)
    if cfg!(target_os = "linux") {
        let xdg_path = xdg_config_path();
        if let Some(xdg) = xdg_path {
            if xdg.exists() {
                return Ok(xdg);
            }
        }
    }

    // Check standard home directory location
    let home_path = home.join(CLAUDE_DIR_NAME);
    if home_path.exists() {
        return Ok(home_path);
    }

    // On Windows, try USERPROFILE
    #[cfg(target_os = "windows")]
    {
        if let Ok(userprofile) = std::env::var("USERPROFILE") {
            let win_path = PathBuf::from(userprofile).join(CLAUDE_DIR_NAME);
            if win_path.exists() {
                return Ok(win_path);
            }
        }
    }

    Err(SnatchError::ClaudeDirectoryNotFound {
        expected_path: home_path,
    })
}

/// Get the user's home directory.
pub fn home_directory() -> Option<PathBuf> {
    // Try directories crate first for cross-platform support
    directories::UserDirs::new().map(|dirs| dirs.home_dir().to_path_buf())
}

/// Get the XDG config directory for Claude.
fn xdg_config_path() -> Option<PathBuf> {
    // Check XDG_CONFIG_HOME first
    if let Ok(xdg_config) = std::env::var("XDG_CONFIG_HOME") {
        let path = PathBuf::from(xdg_config).join("claude");
        return Some(path);
    }

    // Fall back to ~/.config/claude
    home_directory().map(|h| h.join(".config").join("claude"))
}

/// Detect if running in WSL.
#[must_use]
pub fn is_wsl() -> bool {
    if cfg!(target_os = "linux") {
        // Check for WSL indicators
        if let Ok(release) = std::fs::read_to_string("/proc/sys/kernel/osrelease") {
            if release.to_lowercase().contains("microsoft")
                || release.to_lowercase().contains("wsl")
            {
                return true;
            }
        }

        // Check for WSL-specific directories
        if Path::new("/mnt/c/Windows").exists() {
            return true;
        }
    }

    false
}

/// Get the current platform identifier.
#[must_use]
pub fn platform_id() -> &'static str {
    if is_wsl() {
        "wsl"
    } else if cfg!(target_os = "windows") {
        "windows"
    } else if cfg!(target_os = "macos") {
        "macos"
    } else if cfg!(target_os = "linux") {
        "linux"
    } else {
        "unknown"
    }
}

/// Encode a project path for storage (replace / with -).
///
/// Uses percent-encoding for hyphens to ensure roundtrip correctness.
/// Example: `/home/user/my-project` → `-home-user-my%2Dproject`
#[must_use]
pub fn encode_project_path(path: &str) -> String {
    // Normalize path separators
    let normalized = path.replace('\\', "/");

    // Percent-encode hyphens in path components to avoid ambiguity
    let escaped = normalized.replace('-', "%2D");

    // Handle absolute paths
    if escaped.starts_with('/') {
        escaped.replace('/', "-")
    } else {
        format!("-{}", escaped.replace('/', "-"))
    }
}

/// Decode an encoded project path.
///
/// Claude Code's encoding is lossy: it replaces multiple characters with `-`:
/// - `/` (forward slash) → `-`
/// - `_` (underscore) → `-`
/// - `.` (period) → `-`
/// - `-` (hyphen) → `-` (no escaping)
///
/// This creates ambiguity that can only be resolved by checking the filesystem.
/// For example, `--` could represent `/_`, `/.`, or `//`.
///
/// This function attempts to resolve the ambiguity by checking which paths
/// actually exist on the filesystem. If no path exists, it falls back to the
/// naive decode (all hyphens become slashes).
///
/// Example: `-home-user-my%2Dproject` → `/home/user/my-project` (if %2D encoded)
/// Example: `-home-user-claude-snatch` → `/home/user/claude-snatch` (if path exists)
/// Example: `-mnt-c--dev` → `/mnt/c/_dev` (if path exists with underscore)
/// Example: `-mnt-c--dev-CMA-Central` → `/mnt/c/_dev/CMA.Central` (if path exists)
#[must_use]
pub fn decode_project_path(encoded: &str) -> String {
    // First handle percent-encoded hyphens (from our own encoding or future Claude versions)
    let working = encoded.replace("%2D", "\x00HYPHEN\x00");

    // Try to find the best decode by checking filesystem
    if let Some(best) = decode_with_filesystem_check(&working) {
        return best.replace("\x00HYPHEN\x00", "-");
    }

    // Fallback: naive decode (all hyphens to slashes)
    let path = if working.starts_with('-') {
        working.replacen('-', "/", 1).replace('-', "/")
    } else {
        working.replace('-', "/")
    };

    path.replace("\x00HYPHEN\x00", "-")
}

/// Try to decode by checking which paths exist on the filesystem.
///
/// Claude Code's encoding converts multiple characters to `-`:
/// - `/` → `-` (path separator)
/// - `_` → `-` (underscore)
/// - `.` → `-` (period)
/// - `-` → `-` (hyphen, no escaping)
///
/// This means:
/// - `--` could be `/_`, `/.`, or `//` (empty segment, rare)
/// - A single `-` in a path segment could be `-`, `.`, or `_`
///
/// Uses a greedy approach with filesystem validation to find the correct path.
fn decode_with_filesystem_check(encoded: &str) -> Option<String> {
    use std::path::Path;

    // Remove leading hyphen and get content
    let content = encoded.strip_prefix('-').unwrap_or(encoded);
    if content.is_empty() {
        return Some("/".to_string());
    }

    // First, try the smart decode that handles underscores and periods
    if let Some(path) = decode_with_special_chars(content) {
        return Some(path);
    }

    // Fall back to the original hyphen-preserving logic
    let segments: Vec<&str> = content.split('-').collect();
    if segments.is_empty() {
        return Some("/".to_string());
    }

    // Build the path greedily, checking filesystem at each step
    let mut current_path = String::from("/");
    let mut i = 0;

    while i < segments.len() {
        let segment = segments[i];
        i += 1;

        // Try building longer segments by keeping hyphens
        let mut best_segment = segment.to_string();
        let mut best_j = i;

        // Look ahead to see if joining segments with hyphens creates a valid path
        for j in i..=segments.len() {
            let test_segment = if j == i {
                segment.to_string()
            } else {
                segments[i - 1..j].join("-")
            };

            let test_path = format!("{}{}", current_path, test_segment);

            if Path::new(&test_path).exists() {
                best_segment = test_segment;
                best_j = j;
            }
        }

        current_path.push_str(&best_segment);
        i = best_j;

        if i < segments.len() {
            current_path.push('/');
        }
    }

    // Only return if the path exists or we made meaningful progress
    if Path::new(&current_path).exists() || current_path.contains('-') {
        Some(current_path)
    } else {
        None
    }
}

/// Decode path handling underscores (--) and periods in path components.
///
/// This handles Claude Code's encoding where:
/// - `/_` becomes `--` (slash followed by underscore)
/// - `/.` becomes `--` (slash followed by period, less common)
/// - `.` in path names becomes `-`
fn decode_with_special_chars(content: &str) -> Option<String> {
    use std::path::Path;

    // Split on single dash, but track where double-dashes occur
    // Double-dash indicates underscore or period after a slash
    let mut result = String::from("/");
    let mut chars = content.chars().peekable();
    let mut current_segment = String::new();

    while let Some(c) = chars.next() {
        if c == '-' {
            // Check for double-dash
            if chars.peek() == Some(&'-') {
                // Consume the second dash
                chars.next();

                // Double-dash: this is either /_ or /. or //
                // First, complete the current segment if any
                if !current_segment.is_empty() {
                    result.push_str(&current_segment);
                    current_segment.clear();
                }

                // Add path separator
                if !result.ends_with('/') {
                    result.push('/');
                }

                // Try underscore first (most common), then period
                let test_with_underscore = format!("{}_", result);
                let test_with_period = format!("{}.", result);

                // Peek ahead to see what comes next to build test paths
                let mut lookahead = String::new();
                let mut temp_chars = chars.clone();
                while let Some(&next_c) = temp_chars.peek() {
                    if next_c == '-' {
                        break;
                    }
                    lookahead.push(temp_chars.next().unwrap());
                }

                // Test which variant exists
                let underscore_path = format!("{}{}", test_with_underscore, lookahead);
                let period_path = format!("{}{}", test_with_period, lookahead);

                if Path::new(&underscore_path).exists()
                    || path_prefix_exists(&underscore_path)
                {
                    result.push('_');
                } else if Path::new(&period_path).exists()
                    || path_prefix_exists(&period_path)
                {
                    result.push('.');
                } else {
                    // Default to underscore as it's more common
                    result.push('_');
                }
            } else {
                // Single dash: this is a path separator
                if !current_segment.is_empty() {
                    // Complete the segment
                    result.push_str(&current_segment);
                    current_segment.clear();
                }
                result.push('/');
            }
        } else {
            current_segment.push(c);
        }
    }

    // Don't forget the last segment
    if !current_segment.is_empty() {
        result.push_str(&current_segment);
    }

    // Clean up any trailing slashes (except for root)
    while result.len() > 1 && result.ends_with('/') {
        result.pop();
    }

    // Now we have a basic decode, try to improve it by checking for periods in segments
    let improved = improve_path_with_periods(&result);

    if Path::new(&improved).exists() {
        Some(improved)
    } else if Path::new(&result).exists() {
        Some(result)
    } else {
        // Return the improved version even if it doesn't exist
        // (the path might have been deleted)
        Some(improved)
    }
}

/// Check if any path starting with this prefix exists.
fn path_prefix_exists(prefix: &str) -> bool {
    use std::path::Path;

    let path = Path::new(prefix);

    // Check if parent directory exists and might contain matching entries
    if let Some(parent) = path.parent() {
        if parent.exists() && parent.is_dir() {
            if let Some(file_name) = path.file_name() {
                let prefix_str = file_name.to_string_lossy();
                if let Ok(entries) = std::fs::read_dir(parent) {
                    for entry in entries.flatten() {
                        let name = entry.file_name();
                        let name_str = name.to_string_lossy();
                        if name_str.starts_with(prefix_str.as_ref()) {
                            return true;
                        }
                    }
                }
            }
        }
    }
    false
}

/// Try to improve a decoded path by combining adjacent segments with periods.
///
/// Claude Code encodes periods as dashes, which means a directory name like
/// `CMA.Central` becomes `CMA-Central` and gets decoded as `CMA/Central`.
/// This function tries to combine adjacent segments with periods to find
/// the correct path.
///
/// Example: `/mnt/c/_dev/CMA/Central` might actually be `/mnt/c/_dev/CMA.Central`
fn improve_path_with_periods(path: &str) -> String {
    use std::path::Path;

    let parts: Vec<&str> = path.split('/').collect();
    if parts.len() <= 1 {
        return path.to_string();
    }

    // Try to find the best path by combining adjacent segments with periods
    let result = try_combine_segments_with_periods(&parts, 0, String::new());

    if let Some(best_path) = result {
        if Path::new(&best_path).exists() {
            return best_path;
        }
    }

    // Fall back to original path
    path.to_string()
}

/// Recursively try combining segments with periods to find valid paths.
fn try_combine_segments_with_periods(
    parts: &[&str],
    start: usize,
    prefix: String,
) -> Option<String> {
    use std::path::Path;

    if start >= parts.len() {
        return Some(prefix);
    }

    // Handle empty parts (from leading slash)
    if parts[start].is_empty() {
        let new_prefix = if prefix.is_empty() {
            String::new()
        } else {
            format!("{}/", prefix)
        };
        return try_combine_segments_with_periods(parts, start + 1, new_prefix);
    }

    let mut best_result: Option<String> = None;

    // Try combining consecutive segments with periods (greedy - try longest first)
    for end in (start + 1..=parts.len()).rev() {
        // Build the combined segment
        let combined = parts[start..end].join(".");

        // Build the test path
        let test_path = if prefix.is_empty() || prefix == "/" {
            format!("/{}", combined)
        } else if prefix.ends_with('/') {
            format!("{}{}", prefix, combined)
        } else {
            format!("{}/{}", prefix, combined)
        };

        // Check if this path (or prefix) exists
        let path_exists = Path::new(&test_path).exists();
        let could_be_prefix = !path_exists && has_matching_prefix(&test_path);

        if path_exists || could_be_prefix {
            // Recursively try the rest
            if let Some(result) =
                try_combine_segments_with_periods(parts, end, test_path.clone())
            {
                // Verify the full result exists
                if Path::new(&result).exists() {
                    return Some(result);
                }
                // Keep as potential best if no better found
                if best_result.is_none() {
                    best_result = Some(result);
                }
            }
        }
    }

    // If no combining worked, try the single segment as-is
    let single = parts[start];
    let test_path = if prefix.is_empty() || prefix == "/" {
        format!("/{}", single)
    } else if prefix.ends_with('/') {
        format!("{}{}", prefix, single)
    } else {
        format!("{}/{}", prefix, single)
    };

    if let Some(result) = try_combine_segments_with_periods(parts, start + 1, test_path) {
        if best_result.is_none() || Path::new(&result).exists() {
            return Some(result);
        }
    }

    best_result
}

/// Check if any directory entry starts with this path's filename.
fn has_matching_prefix(path_str: &str) -> bool {
    use std::path::Path;

    let path = Path::new(path_str);
    if let (Some(parent), Some(file_name)) = (path.parent(), path.file_name()) {
        if parent.exists() && parent.is_dir() {
            let prefix = file_name.to_string_lossy();
            if let Ok(entries) = std::fs::read_dir(parent) {
                for entry in entries.flatten() {
                    if entry.file_name().to_string_lossy().starts_with(prefix.as_ref()) {
                        return true;
                    }
                }
            }
        }
    }
    false
}

/// Generate variants of a path segment by replacing hyphens with periods.
///
/// For a segment like "CMA-Apps-Bumblebee", generates:
/// - "CMA.Apps.Bumblebee" (all periods)
/// - "CMA.Apps-Bumblebee" (some periods)
/// - etc.
///
/// Returns variants ordered by likelihood (all periods first, then mixed).
#[cfg(test)]
fn generate_segment_variants(segment: &str) -> Vec<String> {
    let mut variants = Vec::new();

    // Count hyphens
    let hyphen_count = segment.chars().filter(|&c| c == '-').count();

    if hyphen_count == 0 {
        return vec![segment.to_string()];
    }

    // For segments with hyphens, prioritize:
    // 1. All hyphens as periods (e.g., CMA.Apps.Bumblebee)
    // 2. Original with hyphens (e.g., CMA-Apps-Bumblebee)
    // 3. Mixed variants (less common)

    // All periods
    variants.push(segment.replace('-', "."));

    // Original (all hyphens)
    variants.push(segment.to_string());

    // For small number of hyphens, generate all combinations
    if hyphen_count <= 3 {
        let positions: Vec<usize> = segment
            .char_indices()
            .filter_map(|(i, c)| if c == '-' { Some(i) } else { None })
            .collect();

        // Generate 2^n - 2 combinations (excluding all-hyphen and all-period)
        for mask in 1..(1 << hyphen_count) - 1 {
            let mut variant = segment.to_string();

            for (bit_idx, &pos) in positions.iter().enumerate() {
                if (mask >> bit_idx) & 1 == 1 {
                    variant.replace_range(pos..pos + 1, ".");
                }
            }

            if !variants.contains(&variant) {
                variants.push(variant);
            }
        }
    }

    variants
}

/// Validate that a decoded path looks reasonable.
#[must_use]
pub fn is_valid_project_path(decoded: &str) -> bool {
    // Must be absolute
    if !decoded.starts_with('/') && !decoded.contains(':') {
        return false;
    }

    // Shouldn't contain invalid characters
    if decoded.contains('\0') {
        return false;
    }

    true
}

/// Parse a session filename to extract the UUID.
///
/// Session files are named `<uuid>.jsonl` or `agent-<hash>.jsonl`.
#[must_use]
pub fn parse_session_filename(filename: &str) -> Option<SessionFileInfo> {
    let name = filename.strip_suffix(".jsonl")?;

    if let Some(hash) = name.strip_prefix("agent-") {
        Some(SessionFileInfo {
            session_id: name.to_string(),
            is_subagent: true,
            agent_hash: Some(hash.to_string()),
        })
    } else {
        // Validate as UUID format (loose check)
        if name.len() >= 32 && name.chars().all(|c| c.is_ascii_hexdigit() || c == '-') {
            Some(SessionFileInfo {
                session_id: name.to_string(),
                is_subagent: false,
                agent_hash: None,
            })
        } else {
            None
        }
    }
}

/// Information extracted from a session filename.
#[derive(Debug, Clone)]
pub struct SessionFileInfo {
    /// Session identifier (UUID or agent-<hash>).
    pub session_id: String,
    /// Whether this is a subagent session.
    pub is_subagent: bool,
    /// Agent hash if subagent.
    pub agent_hash: Option<String>,
}

/// Normalize a path for consistent handling across platforms.
#[must_use]
pub fn normalize_path(path: &Path) -> PathBuf {
    // Convert to absolute if possible
    let path = if path.is_relative() {
        std::env::current_dir()
            .map(|cwd| cwd.join(path))
            .unwrap_or_else(|_| path.to_path_buf())
    } else {
        path.to_path_buf()
    };

    // Normalize separators
    #[cfg(target_os = "windows")]
    {
        PathBuf::from(path.to_string_lossy().replace('/', "\\"))
    }

    #[cfg(not(target_os = "windows"))]
    {
        path
    }
}

/// Check if a path appears to be a valid session file.
#[must_use]
pub fn is_session_file(path: &Path) -> bool {
    path.extension()
        .map(|ext| ext == "jsonl")
        .unwrap_or(false)
        && path
            .file_name()
            .and_then(|n| n.to_str())
            .map(|n| parse_session_filename(n).is_some())
            .unwrap_or(false)
}

/// Convert a Windows path to WSL path format.
#[must_use]
pub fn windows_to_wsl_path(windows_path: &str) -> String {
    // C:\Users\foo → /mnt/c/Users/foo
    if windows_path.len() >= 2 && windows_path.chars().nth(1) == Some(':') {
        // Safety: len >= 2 guarantees at least one character
        let drive = windows_path
            .chars()
            .next()
            .expect("len >= 2")
            .to_ascii_lowercase();
        let rest = &windows_path[2..].replace('\\', "/");
        format!("/mnt/{drive}{rest}")
    } else {
        windows_path.replace('\\', "/")
    }
}

/// Convert a WSL path to Windows path format.
#[must_use]
pub fn wsl_to_windows_path(wsl_path: &str) -> Option<String> {
    // /mnt/c/Users/foo → C:\Users\foo
    if let Some(rest) = wsl_path.strip_prefix("/mnt/") {
        if rest.len() >= 2 {
            let drive = rest.chars().next()?.to_ascii_uppercase();
            let path = &rest[1..];
            return Some(format!("{drive}:{}", path.replace('/', "\\")));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_encode_project_path() {
        assert_eq!(encode_project_path("/home/user/project"), "-home-user-project");
        assert_eq!(encode_project_path("/"), "-");
        assert_eq!(encode_project_path("/a/b/c"), "-a-b-c");
        // Paths with hyphens are percent-encoded
        assert_eq!(encode_project_path("/home/user/my-project"), "-home-user-my%2Dproject");
    }

    #[test]
    fn test_decode_project_path_with_percent_encoding() {
        // Paths with percent-encoded hyphens are decoded correctly
        assert_eq!(decode_project_path("-home-user-my%2Dproject"), "/home/user/my-project");
    }

    #[test]
    fn test_decode_project_path_simple() {
        // Simple path without ambiguity (when fs check fails, falls back to naive decode)
        // Note: In tests, paths don't exist so we get the fallback behavior
        assert_eq!(decode_project_path("-"), "/");
    }

    #[test]
    fn test_roundtrip_with_percent_encoding() {
        // Round-trip works perfectly when we use our own encoding
        let paths = [
            "/home/user/project",
            "/",
            "/a/b/c/d/e",
            "/Users/someone/dev/my-project",
        ];

        for path in paths {
            let encoded = encode_project_path(path);
            let decoded = decode_project_path(&encoded);
            assert_eq!(decoded, path, "Roundtrip failed for: {path}");
        }
    }

    #[test]
    fn test_decode_with_filesystem_check_existing_path() {
        use std::env;

        // Test with a path we know exists - the temp directory
        let temp_dir = env::temp_dir();
        let temp_str = temp_dir.to_string_lossy();

        // Encode the temp path
        let encoded = encode_project_path(&temp_str);
        let decoded = decode_project_path(&encoded);

        // Should decode back to the original
        assert_eq!(decoded, temp_str.as_ref());
    }

    #[test]
    fn test_parse_session_filename() {
        // Regular session
        let info = parse_session_filename("40afc8a7-3fcb-4d29-b1ee-100b81b8c6c0.jsonl").unwrap();
        assert_eq!(info.session_id, "40afc8a7-3fcb-4d29-b1ee-100b81b8c6c0");
        assert!(!info.is_subagent);

        // Subagent session
        let info = parse_session_filename("agent-3e533ee.jsonl").unwrap();
        assert!(info.is_subagent);
        assert_eq!(info.agent_hash, Some("3e533ee".to_string()));

        // Invalid
        assert!(parse_session_filename("not-a-session.txt").is_none());
        assert!(parse_session_filename("readme.md").is_none());
    }

    #[test]
    fn test_windows_to_wsl_path() {
        assert_eq!(
            windows_to_wsl_path(r"C:\Users\foo\project"),
            "/mnt/c/Users/foo/project"
        );
        assert_eq!(windows_to_wsl_path(r"D:\dev"), "/mnt/d/dev");
    }

    #[test]
    fn test_wsl_to_windows_path() {
        assert_eq!(
            wsl_to_windows_path("/mnt/c/Users/foo"),
            Some(r"C:\Users\foo".to_string())
        );
        assert_eq!(wsl_to_windows_path("/home/user"), None);
    }

    #[test]
    fn test_is_valid_project_path() {
        assert!(is_valid_project_path("/home/user/project"));
        assert!(is_valid_project_path("C:/Users/foo")); // Windows with forward slash
        assert!(!is_valid_project_path("relative/path"));
    }

    #[test]
    fn test_generate_segment_variants() {
        // No hyphens
        let variants = generate_segment_variants("simple");
        assert_eq!(variants, vec!["simple"]);

        // Single hyphen
        let variants = generate_segment_variants("CMA-Central");
        assert!(variants.contains(&"CMA.Central".to_string()));
        assert!(variants.contains(&"CMA-Central".to_string()));

        // Multiple hyphens
        let variants = generate_segment_variants("CMA-Apps-Bumblebee");
        assert!(variants.contains(&"CMA.Apps.Bumblebee".to_string()));
        assert!(variants.contains(&"CMA-Apps-Bumblebee".to_string()));
    }

    #[test]
    fn test_decode_double_dash_underscore() {
        // Test that double-dash decodes to underscore when path exists
        // This test uses /tmp which should exist on most systems
        use std::fs;

        let temp_dir = std::env::temp_dir();
        let test_dir = temp_dir.join("_test_snatch_decode");

        // Create a test directory with underscore prefix
        if !test_dir.exists() {
            let _ = fs::create_dir(&test_dir);
        }

        if test_dir.exists() {
            // Simulate Claude Code's encoding: /tmp/_test_snatch_decode
            // would be encoded as -tmp--test_snatch_decode (double dash for /_)

            // Build the encoded path manually
            let encoded = format!(
                "-{}--test_snatch_decode",
                temp_dir
                    .to_string_lossy()
                    .trim_start_matches('/')
                    .replace('/', "-")
            );

            let decoded = decode_project_path(&encoded);

            // The decoded path should have underscore, not double slash
            assert!(
                !decoded.contains("//"),
                "Decoded path should not contain double slash: {decoded}"
            );

            // Clean up
            let _ = fs::remove_dir(&test_dir);
        }
    }

    #[test]
    fn test_decode_special_chars_unit() {
        // Unit test for decode_with_special_chars without filesystem dependency
        // When no path exists, it should still not produce double slashes

        // Simulated encoding of /mnt/c/_dev
        let result = decode_with_special_chars("mnt-c--dev");
        assert!(result.is_some());
        let path = result.unwrap();
        // Should decode to /mnt/c/_dev (with underscore), not /mnt/c//dev
        assert!(
            !path.contains("//"),
            "Path should not contain double slash: {path}"
        );
        assert!(
            path.contains("/_") || path.contains("/."),
            "Path should contain underscore or period after slash: {path}"
        );
    }

    #[test]
    fn test_decode_periods_in_path_segment() {
        // Test that periods in path segments are handled
        // e.g., CMA.Apps.Bumblebee encoded as CMA-Apps-Bumblebee

        // This tests the variant generation
        let segment = "CMA-Apps-Bumblebee";
        let variants = generate_segment_variants(segment);

        // Should include the all-periods variant
        assert!(
            variants.contains(&"CMA.Apps.Bumblebee".to_string()),
            "Should generate period variant"
        );
    }

    #[test]
    fn test_decode_preserves_existing_behavior() {
        // Ensure we don't break existing behavior for simple paths

        // Root path
        assert_eq!(decode_project_path("-"), "/");

        // Simple paths (when filesystem check fails, falls back to naive decode)
        // These paths don't exist, so we get the fallback behavior
        let decoded = decode_project_path("-nonexistent-path-here");
        assert!(decoded.starts_with('/'));

        // Percent-encoded hyphens should still work
        assert_eq!(
            decode_project_path("-home-user-my%2Dproject"),
            "/home/user/my-project"
        );
    }

    #[test]
    fn test_path_prefix_exists() {
        // Test the path_prefix_exists helper
        let temp_dir = std::env::temp_dir();

        // The temp directory itself should be found as a prefix
        let parent = temp_dir.parent();
        if let Some(p) = parent {
            let partial_name = temp_dir
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("");
            if !partial_name.is_empty() && partial_name.len() > 2 {
                let prefix_path = p.join(&partial_name[..2]);
                // This might or might not find a match depending on directory contents
                // Just ensure it doesn't panic
                let _ = path_prefix_exists(&prefix_path.to_string_lossy());
            }
        }
    }
}
