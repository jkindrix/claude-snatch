//! CLI command implementations.
//!
//! Each command is implemented in its own module with a `run` function
//! that handles the command logic.

pub mod cache;
pub mod cleanup;
pub mod code;
pub mod completions;
pub mod config;
pub mod diff;
pub mod export;
pub mod extract;
pub mod index;
pub mod info;
pub mod list;
pub mod pick;
pub mod prompts;
pub mod quickstart;
pub mod recent;
pub mod search;
pub mod standup;
pub mod stats;
pub mod summary;
pub mod tag;
pub mod tui;
pub mod validate;
pub mod watch;

use std::path::PathBuf;
use std::time::SystemTime;

use chrono::{Duration, NaiveDate, Utc};

use crate::discovery::ClaudeDirectory;
use crate::error::{Result, SnatchError};

/// Get the Claude directory from CLI args or discover automatically.
pub fn get_claude_dir(custom_path: Option<&PathBuf>) -> Result<ClaudeDirectory> {
    match custom_path {
        Some(path) => ClaudeDirectory::from_path(path),
        None => ClaudeDirectory::discover(),
    }
}

/// Parse a date filter string.
///
/// Supports:
/// - ISO date: `2024-12-24`
/// - Relative: `1day`, `2weeks`, `3months`
pub fn parse_date_filter(s: &str) -> Result<SystemTime> {
    // Try parsing as ISO date first
    if let Ok(date) = NaiveDate::parse_from_str(s, "%Y-%m-%d") {
        // and_hms_opt(0, 0, 0) always returns Some for valid h/m/s values
        let datetime = date.and_hms_opt(0, 0, 0).expect("midnight is always valid");
        let utc = chrono::TimeZone::from_utc_datetime(&Utc, &datetime);
        return Ok(SystemTime::from(utc));
    }

    // Try parsing as relative duration
    let s_lower = s.to_lowercase();

    // Extract numeric part and unit
    let numeric_end = s_lower
        .char_indices()
        .find(|(_, c)| !c.is_ascii_digit())
        .map(|(i, _)| i)
        .unwrap_or(s_lower.len());

    if numeric_end == 0 || numeric_end == s_lower.len() {
        return Err(SnatchError::InvalidArgument {
            name: "date".to_string(),
            reason: format!(
                "Invalid date '{}'. Use YYYY-MM-DD or relative like '1week', '2days'",
                s
            ),
        });
    }

    let amount: i64 = s_lower[..numeric_end].parse().map_err(|_| {
        SnatchError::InvalidArgument {
            name: "date".to_string(),
            reason: format!("Invalid number in date filter: {}", &s_lower[..numeric_end]),
        }
    })?;

    let unit = &s_lower[numeric_end..];
    let duration = match unit {
        "d" | "day" | "days" => Duration::days(amount),
        "w" | "week" | "weeks" => Duration::weeks(amount),
        "m" | "month" | "months" => Duration::days(amount * 30), // Approximate
        "y" | "year" | "years" => Duration::days(amount * 365),  // Approximate
        "h" | "hour" | "hours" => Duration::hours(amount),
        _ => {
            return Err(SnatchError::InvalidArgument {
                name: "date".to_string(),
                reason: format!(
                    "Unknown time unit '{}'. Use days, weeks, months, or years",
                    unit
                ),
            })
        }
    };

    let target = Utc::now() - duration;
    Ok(SystemTime::from(target))
}

/// Parse a size filter string (e.g., "1KB", "10MB", "1.5GB").
pub fn parse_size(s: &str) -> Result<u64> {
    let s_trimmed = s.trim();
    let s_upper = s_trimmed.to_uppercase();

    // Find where the numeric part ends
    let numeric_end = s_upper
        .char_indices()
        .find(|(_, c)| !c.is_ascii_digit() && *c != '.')
        .map(|(i, _)| i)
        .unwrap_or(s_upper.len());

    if numeric_end == 0 {
        return Err(SnatchError::InvalidArgument {
            name: "size".to_string(),
            reason: format!("Invalid size '{}'. Use format like '1KB', '10MB', '1GB'", s),
        });
    }

    let amount: f64 = s_upper[..numeric_end].parse().map_err(|_| {
        SnatchError::InvalidArgument {
            name: "size".to_string(),
            reason: format!("Invalid number in size filter: {}", &s_upper[..numeric_end]),
        }
    })?;

    let unit = s_upper[numeric_end..].trim();
    let multiplier: u64 = match unit {
        "" | "B" => 1,
        "K" | "KB" | "KIB" => 1024,
        "M" | "MB" | "MIB" => 1024 * 1024,
        "G" | "GB" | "GIB" => 1024 * 1024 * 1024,
        _ => {
            return Err(SnatchError::InvalidArgument {
                name: "size".to_string(),
                reason: format!("Unknown size unit '{}'. Use B, KB, MB, or GB", unit),
            })
        }
    };

    Ok((amount * multiplier as f64) as u64)
}
