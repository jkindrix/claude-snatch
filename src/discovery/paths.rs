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
/// Claude Code's encoding is lossy: it replaces `/` with `-` without escaping
/// existing hyphens. This makes decoding ambiguous - we can't distinguish between
/// a hyphen that was originally a `/` vs one that was always a hyphen.
///
/// This function attempts to resolve the ambiguity by checking which paths
/// actually exist on the filesystem. If no path exists, it falls back to the
/// naive decode (all hyphens become slashes).
///
/// Example: `-home-user-my%2Dproject` → `/home/user/my-project` (if %2D encoded)
/// Example: `-home-user-claude-snatch` → `/home/user/claude-snatch` (if path exists)
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
/// Uses a greedy approach: starting from the root, at each hyphen position
/// we prefer keeping it as a hyphen if that path segment exists on disk.
fn decode_with_filesystem_check(encoded: &str) -> Option<String> {
    use std::path::Path;

    // Remove leading hyphen and split into segments
    let content = encoded.strip_prefix('-').unwrap_or(encoded);
    if content.is_empty() {
        return Some("/".to_string());
    }

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
}
