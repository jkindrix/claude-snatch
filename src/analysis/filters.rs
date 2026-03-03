//! Time period parsing and session filtering utilities.

use chrono::{Duration, Utc};

/// Parse a period string like "24h", "7d", "30d" into a [`chrono::Duration`].
///
/// Returns `None` for `"all"` (unbounded).
pub fn parse_period(period: &str) -> Result<Option<Duration>, String> {
    match period.trim().to_lowercase().as_str() {
        "all" => Ok(None),
        s => {
            if let Some(h) = s.strip_suffix('h') {
                let hours: i64 = h.parse().map_err(|_| format!("Invalid hours: {h}"))?;
                Ok(Some(Duration::hours(hours)))
            } else if let Some(d) = s.strip_suffix('d') {
                let days: i64 = d.parse().map_err(|_| format!("Invalid days: {d}"))?;
                Ok(Some(Duration::days(days)))
            } else if let Some(w) = s.strip_suffix('w') {
                let weeks: i64 = w.parse().map_err(|_| format!("Invalid weeks: {w}"))?;
                Ok(Some(Duration::weeks(weeks)))
            } else {
                Err(format!(
                    "Invalid period format: {s}. Use e.g. '24h', '7d', '30d', 'all'"
                ))
            }
        }
    }
}

/// Compute a cutoff timestamp from a period string.
///
/// Returns `None` for `"all"` (no cutoff). Otherwise returns `Some(now - duration)`.
pub fn period_cutoff(period: &str) -> Result<Option<chrono::DateTime<Utc>>, String> {
    match parse_period(period)? {
        Some(duration) => Ok(Some(Utc::now() - duration)),
        None => Ok(None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_period_hours() {
        let d = parse_period("24h").unwrap().unwrap();
        assert_eq!(d.num_hours(), 24);
    }

    #[test]
    fn test_parse_period_days() {
        let d = parse_period("7d").unwrap().unwrap();
        assert_eq!(d.num_days(), 7);
    }

    #[test]
    fn test_parse_period_weeks() {
        let d = parse_period("2w").unwrap().unwrap();
        assert_eq!(d.num_weeks(), 2);
    }

    #[test]
    fn test_parse_period_all() {
        assert!(parse_period("all").unwrap().is_none());
    }

    #[test]
    fn test_parse_period_invalid() {
        assert!(parse_period("xyz").is_err());
    }

    #[test]
    fn test_parse_period_whitespace() {
        let d = parse_period("  7d  ").unwrap().unwrap();
        assert_eq!(d.num_days(), 7);
    }

    #[test]
    fn test_period_cutoff_bounded() {
        let cutoff = period_cutoff("24h").unwrap().unwrap();
        let now = Utc::now();
        // Should be roughly 24h ago (within a second of tolerance)
        let diff = now - cutoff;
        assert!((diff.num_hours() - 24).abs() <= 1);
    }

    #[test]
    fn test_period_cutoff_all() {
        assert!(period_cutoff("all").unwrap().is_none());
    }
}
