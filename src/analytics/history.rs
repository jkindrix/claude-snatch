//! Historical cost tracking and persistence.
//!
//! This module provides:
//! - Persistent storage of cost data over time
//! - Daily/weekly/monthly cost snapshots
//! - Historical cost querying and trend analysis
//!
//! Data is stored in `~/.config/claude-snatch/cost_history.json` by default.

use std::collections::BTreeMap;
use std::fs;
use std::io::{BufReader, BufWriter};
use std::path::PathBuf;

use chrono::{DateTime, Datelike, Duration, NaiveDate, Utc};
use serde::{Deserialize, Serialize};

use crate::error::{Result, SnatchError};

/// A single cost data point recorded at a specific time.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CostDataPoint {
    /// The date this data point was recorded.
    pub date: NaiveDate,
    /// Total tokens used on this date.
    pub tokens: u64,
    /// Input tokens.
    pub input_tokens: u64,
    /// Output tokens.
    pub output_tokens: u64,
    /// Cache read tokens.
    pub cache_read_tokens: u64,
    /// Estimated cost in USD.
    pub cost: f64,
    /// Number of sessions.
    pub session_count: usize,
    /// Number of messages.
    pub message_count: usize,
    /// Last updated timestamp.
    pub updated_at: DateTime<Utc>,
}

impl CostDataPoint {
    /// Create a new cost data point for today.
    pub fn new(date: NaiveDate) -> Self {
        Self {
            date,
            tokens: 0,
            input_tokens: 0,
            output_tokens: 0,
            cache_read_tokens: 0,
            cost: 0.0,
            session_count: 0,
            message_count: 0,
            updated_at: Utc::now(),
        }
    }

    /// Create a cost data point with values.
    pub fn with_values(
        date: NaiveDate,
        tokens: u64,
        input_tokens: u64,
        output_tokens: u64,
        cache_read_tokens: u64,
        cost: f64,
        session_count: usize,
        message_count: usize,
    ) -> Self {
        Self {
            date,
            tokens,
            input_tokens,
            output_tokens,
            cache_read_tokens,
            cost,
            session_count,
            message_count,
            updated_at: Utc::now(),
        }
    }

    /// Add usage from another data point.
    pub fn add(&mut self, other: &CostDataPoint) {
        self.tokens += other.tokens;
        self.input_tokens += other.input_tokens;
        self.output_tokens += other.output_tokens;
        self.cache_read_tokens += other.cache_read_tokens;
        self.cost += other.cost;
        self.session_count += other.session_count;
        self.message_count += other.message_count;
        self.updated_at = Utc::now();
    }
}

/// Aggregated cost statistics for a time period.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CostPeriodStats {
    /// Total tokens in the period.
    pub total_tokens: u64,
    /// Total cost in USD.
    pub total_cost: f64,
    /// Average daily cost.
    pub avg_daily_cost: f64,
    /// Maximum daily cost.
    pub max_daily_cost: f64,
    /// Minimum daily cost (non-zero days only).
    pub min_daily_cost: f64,
    /// Total sessions.
    pub total_sessions: usize,
    /// Number of days with activity.
    pub active_days: usize,
    /// Cost trend direction (positive = increasing).
    pub trend_direction: f64,
    /// Daily costs for sparkline visualization.
    pub daily_costs: Vec<f64>,
    /// Daily tokens for sparkline visualization.
    pub daily_tokens: Vec<u64>,
}

/// Persistent storage for historical cost data.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CostHistory {
    /// Daily cost data points keyed by date string (YYYY-MM-DD).
    #[serde(default)]
    daily: BTreeMap<String, CostDataPoint>,
    /// Last snapshot timestamp.
    #[serde(default)]
    last_snapshot: Option<DateTime<Utc>>,
    /// Version for future compatibility.
    #[serde(default = "default_version")]
    version: u32,
}

fn default_version() -> u32 {
    1
}

impl Default for CostHistory {
    fn default() -> Self {
        Self::new()
    }
}

impl CostHistory {
    /// Create a new empty cost history.
    pub fn new() -> Self {
        Self {
            daily: BTreeMap::new(),
            last_snapshot: None,
            version: 1,
        }
    }

    /// Get the default history file path.
    pub fn default_path() -> PathBuf {
        dirs::config_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("claude-snatch")
            .join("cost_history.json")
    }

    /// Load cost history from the default path.
    pub fn load() -> Result<Self> {
        Self::load_from(&Self::default_path())
    }

    /// Load cost history from a specific path.
    pub fn load_from(path: &PathBuf) -> Result<Self> {
        if !path.exists() {
            return Ok(Self::new());
        }

        let file = fs::File::open(path).map_err(|e| SnatchError::io(
            format!("Failed to open cost history file: {}", path.display()),
            e,
        ))?;

        let reader = BufReader::new(file);
        let history: Self = serde_json::from_reader(reader).map_err(|e| {
            SnatchError::SerializationError {
                context: format!("Failed to parse cost history: {}", path.display()),
                source: e,
            }
        })?;

        Ok(history)
    }

    /// Save cost history to the default path.
    pub fn save(&self) -> Result<()> {
        self.save_to(&Self::default_path())
    }

    /// Save cost history to a specific path.
    pub fn save_to(&self, path: &PathBuf) -> Result<()> {
        // Create parent directory if needed
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|e| SnatchError::io(
                format!("Failed to create directory: {}", parent.display()),
                e,
            ))?;
        }

        // Write atomically using temp file
        let temp_path = path.with_extension("json.tmp");
        let file = fs::File::create(&temp_path).map_err(|e| SnatchError::io(
            format!("Failed to create temp file: {}", temp_path.display()),
            e,
        ))?;

        let writer = BufWriter::new(file);
        serde_json::to_writer_pretty(writer, self).map_err(|e| {
            SnatchError::SerializationError {
                context: "Failed to serialize cost history".to_string(),
                source: e,
            }
        })?;

        // Atomically rename
        fs::rename(&temp_path, path).map_err(|e| SnatchError::io(
            format!("Failed to rename temp file to: {}", path.display()),
            e,
        ))?;

        Ok(())
    }

    /// Record a cost data point for a specific date.
    pub fn record(&mut self, point: CostDataPoint) {
        let key = point.date.format("%Y-%m-%d").to_string();
        self.daily.insert(key, point);
        self.last_snapshot = Some(Utc::now());
    }

    /// Record or update cost data for today.
    pub fn record_today(
        &mut self,
        tokens: u64,
        input_tokens: u64,
        output_tokens: u64,
        cache_read_tokens: u64,
        cost: f64,
        session_count: usize,
        message_count: usize,
    ) {
        let today = Utc::now().date_naive();
        let point = CostDataPoint::with_values(
            today,
            tokens,
            input_tokens,
            output_tokens,
            cache_read_tokens,
            cost,
            session_count,
            message_count,
        );
        self.record(point);
    }

    /// Get data point for a specific date.
    pub fn get(&self, date: NaiveDate) -> Option<&CostDataPoint> {
        let key = date.format("%Y-%m-%d").to_string();
        self.daily.get(&key)
    }

    /// Get all data points within a date range (inclusive).
    pub fn range(&self, start: NaiveDate, end: NaiveDate) -> Vec<&CostDataPoint> {
        let start_key = start.format("%Y-%m-%d").to_string();
        let end_key = end.format("%Y-%m-%d").to_string();

        self.daily
            .range(start_key..=end_key)
            .map(|(_, v)| v)
            .collect()
    }

    /// Get statistics for the last N days.
    pub fn stats_last_days(&self, days: i64) -> CostPeriodStats {
        let end = Utc::now().date_naive();
        let start = end - Duration::days(days - 1);
        self.stats_range(start, end)
    }

    /// Get statistics for a date range.
    pub fn stats_range(&self, start: NaiveDate, end: NaiveDate) -> CostPeriodStats {
        let points = self.range(start, end);

        if points.is_empty() {
            return CostPeriodStats::default();
        }

        let total_tokens: u64 = points.iter().map(|p| p.tokens).sum();
        let total_cost: f64 = points.iter().map(|p| p.cost).sum();
        let total_sessions: usize = points.iter().map(|p| p.session_count).sum();
        let active_days = points.len();

        // Calculate non-zero costs for min calculation
        let non_zero_costs: Vec<f64> = points.iter()
            .map(|p| p.cost)
            .filter(|&c| c > 0.0)
            .collect();

        let max_daily_cost = points.iter().map(|p| p.cost).fold(0.0_f64, f64::max);
        let min_daily_cost = non_zero_costs.iter().copied().fold(f64::INFINITY, f64::min);
        let min_daily_cost = if min_daily_cost.is_infinite() { 0.0 } else { min_daily_cost };

        let avg_daily_cost = if active_days > 0 {
            total_cost / active_days as f64
        } else {
            0.0
        };

        // Build daily arrays for sparklines (fill gaps with zeros)
        let mut daily_costs = Vec::new();
        let mut daily_tokens = Vec::new();
        let mut current = start;
        while current <= end {
            if let Some(point) = self.get(current) {
                daily_costs.push(point.cost);
                daily_tokens.push(point.tokens);
            } else {
                daily_costs.push(0.0);
                daily_tokens.push(0);
            }
            current += Duration::days(1);
        }

        // Calculate trend direction using linear regression
        let trend_direction = if daily_costs.len() >= 2 {
            calculate_trend(&daily_costs)
        } else {
            0.0
        };

        CostPeriodStats {
            total_tokens,
            total_cost,
            avg_daily_cost,
            max_daily_cost,
            min_daily_cost,
            total_sessions,
            active_days,
            trend_direction,
            daily_costs,
            daily_tokens,
        }
    }

    /// Get statistics for the current week (Monday to Sunday).
    pub fn stats_this_week(&self) -> CostPeriodStats {
        let today = Utc::now().date_naive();
        let days_since_monday = today.weekday().num_days_from_monday() as i64;
        let monday = today - Duration::days(days_since_monday);
        let sunday = monday + Duration::days(6);
        self.stats_range(monday, sunday.min(today))
    }

    /// Get statistics for the current month.
    pub fn stats_this_month(&self) -> CostPeriodStats {
        let today = Utc::now().date_naive();
        let first_of_month = NaiveDate::from_ymd_opt(today.year(), today.month(), 1)
            .unwrap_or(today);
        self.stats_range(first_of_month, today)
    }

    /// Get statistics for all time.
    pub fn stats_all_time(&self) -> CostPeriodStats {
        if self.daily.is_empty() {
            return CostPeriodStats::default();
        }

        let first = self.daily.keys().next().and_then(|k| NaiveDate::parse_from_str(k, "%Y-%m-%d").ok());
        let last = self.daily.keys().last().and_then(|k| NaiveDate::parse_from_str(k, "%Y-%m-%d").ok());

        match (first, last) {
            (Some(start), Some(end)) => self.stats_range(start, end),
            _ => CostPeriodStats::default(),
        }
    }

    /// Get the number of recorded days.
    pub fn len(&self) -> usize {
        self.daily.len()
    }

    /// Check if history is empty.
    pub fn is_empty(&self) -> bool {
        self.daily.is_empty()
    }

    /// Get the first recorded date.
    pub fn first_date(&self) -> Option<NaiveDate> {
        self.daily.keys().next()
            .and_then(|k| NaiveDate::parse_from_str(k, "%Y-%m-%d").ok())
    }

    /// Get the last recorded date.
    pub fn last_date(&self) -> Option<NaiveDate> {
        self.daily.keys().last()
            .and_then(|k| NaiveDate::parse_from_str(k, "%Y-%m-%d").ok())
    }

    /// Get the last snapshot timestamp.
    pub fn last_snapshot(&self) -> Option<DateTime<Utc>> {
        self.last_snapshot
    }

    /// Clear all history.
    pub fn clear(&mut self) {
        self.daily.clear();
        self.last_snapshot = None;
    }

    /// Export history as CSV string.
    pub fn to_csv(&self) -> String {
        let mut csv = String::from("date,tokens,input_tokens,output_tokens,cache_read_tokens,cost,sessions,messages\n");
        for (date, point) in &self.daily {
            csv.push_str(&format!(
                "{},{},{},{},{},{:.6},{},{}\n",
                date,
                point.tokens,
                point.input_tokens,
                point.output_tokens,
                point.cache_read_tokens,
                point.cost,
                point.session_count,
                point.message_count,
            ));
        }
        csv
    }

    /// Get weekly aggregated costs (for the last N weeks).
    pub fn weekly_costs(&self, weeks: usize) -> Vec<(String, f64)> {
        let today = Utc::now().date_naive();
        let mut result = Vec::new();

        for week_offset in 0..weeks {
            let week_end = today - Duration::days((week_offset * 7) as i64);
            let week_start = week_end - Duration::days(6);

            let points = self.range(week_start, week_end);
            let week_cost: f64 = points.iter().map(|p| p.cost).sum();

            let label = format!("{} - {}", week_start.format("%m/%d"), week_end.format("%m/%d"));
            result.push((label, week_cost));
        }

        result.reverse();
        result
    }

    /// Get monthly aggregated costs (for the last N months).
    pub fn monthly_costs(&self, months: usize) -> Vec<(String, f64)> {
        let today = Utc::now().date_naive();
        let mut result = Vec::new();

        for month_offset in 0..months {
            let target_month = if today.month() as i32 - month_offset as i32 > 0 {
                NaiveDate::from_ymd_opt(
                    today.year(),
                    (today.month() as i32 - month_offset as i32) as u32,
                    1,
                )
            } else {
                let years_back = (month_offset as i32 - today.month() as i32) / 12 + 1;
                let month = 12 - ((month_offset as i32 - today.month() as i32) % 12);
                NaiveDate::from_ymd_opt(today.year() - years_back, month as u32, 1)
            };

            if let Some(month_start) = target_month {
                let month_end = if month_start.month() == 12 {
                    NaiveDate::from_ymd_opt(month_start.year() + 1, 1, 1)
                        .map(|d| d - Duration::days(1))
                } else {
                    NaiveDate::from_ymd_opt(month_start.year(), month_start.month() + 1, 1)
                        .map(|d| d - Duration::days(1))
                };

                if let Some(end) = month_end {
                    let end = end.min(today);
                    let points = self.range(month_start, end);
                    let month_cost: f64 = points.iter().map(|p| p.cost).sum();

                    let label = month_start.format("%Y-%m").to_string();
                    result.push((label, month_cost));
                }
            }
        }

        result.reverse();
        result
    }
}

/// Calculate trend direction using linear regression slope.
fn calculate_trend(values: &[f64]) -> f64 {
    if values.len() < 2 {
        return 0.0;
    }

    let n = values.len() as f64;
    let sum_x: f64 = (0..values.len()).map(|i| i as f64).sum();
    let sum_y: f64 = values.iter().sum();
    let sum_xy: f64 = values.iter().enumerate()
        .map(|(i, v)| i as f64 * v)
        .sum();
    let sum_x2: f64 = (0..values.len()).map(|i| (i * i) as f64).sum();

    let denominator = n * sum_x2 - sum_x * sum_x;
    if denominator.abs() < f64::EPSILON {
        return 0.0;
    }

    (n * sum_xy - sum_x * sum_y) / denominator
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cost_data_point_new() {
        let date = NaiveDate::from_ymd_opt(2025, 1, 15).unwrap();
        let point = CostDataPoint::new(date);

        assert_eq!(point.date, date);
        assert_eq!(point.tokens, 0);
        assert_eq!(point.cost, 0.0);
    }

    #[test]
    fn test_cost_data_point_with_values() {
        let date = NaiveDate::from_ymd_opt(2025, 1, 15).unwrap();
        let point = CostDataPoint::with_values(
            date, 1000, 600, 400, 100, 0.05, 5, 20,
        );

        assert_eq!(point.date, date);
        assert_eq!(point.tokens, 1000);
        assert_eq!(point.input_tokens, 600);
        assert_eq!(point.output_tokens, 400);
        assert_eq!(point.cost, 0.05);
        assert_eq!(point.session_count, 5);
    }

    #[test]
    fn test_cost_data_point_add() {
        let date = NaiveDate::from_ymd_opt(2025, 1, 15).unwrap();
        let mut point1 = CostDataPoint::with_values(date, 1000, 600, 400, 100, 0.05, 5, 20);
        let point2 = CostDataPoint::with_values(date, 500, 300, 200, 50, 0.03, 2, 10);

        point1.add(&point2);

        assert_eq!(point1.tokens, 1500);
        assert_eq!(point1.input_tokens, 900);
        assert_eq!(point1.cost, 0.08);
        assert_eq!(point1.session_count, 7);
    }

    #[test]
    fn test_cost_history_new() {
        let history = CostHistory::new();
        assert!(history.is_empty());
        assert_eq!(history.len(), 0);
    }

    #[test]
    fn test_cost_history_record() {
        let mut history = CostHistory::new();
        let date = NaiveDate::from_ymd_opt(2025, 1, 15).unwrap();
        let point = CostDataPoint::with_values(date, 1000, 600, 400, 100, 0.05, 5, 20);

        history.record(point);

        assert_eq!(history.len(), 1);
        assert!(history.get(date).is_some());
    }

    #[test]
    fn test_cost_history_range() {
        let mut history = CostHistory::new();

        for day in 1..=10 {
            let date = NaiveDate::from_ymd_opt(2025, 1, day).unwrap();
            let point = CostDataPoint::with_values(date, 1000, 600, 400, 100, 0.05, 1, 10);
            history.record(point);
        }

        let start = NaiveDate::from_ymd_opt(2025, 1, 3).unwrap();
        let end = NaiveDate::from_ymd_opt(2025, 1, 7).unwrap();
        let points = history.range(start, end);

        assert_eq!(points.len(), 5);
    }

    #[test]
    fn test_cost_history_stats_range() {
        let mut history = CostHistory::new();

        for day in 1..=5 {
            let date = NaiveDate::from_ymd_opt(2025, 1, day).unwrap();
            let point = CostDataPoint::with_values(
                date,
                1000 * day as u64,
                600 * day as u64,
                400 * day as u64,
                100,
                0.01 * day as f64,
                1,
                10,
            );
            history.record(point);
        }

        let start = NaiveDate::from_ymd_opt(2025, 1, 1).unwrap();
        let end = NaiveDate::from_ymd_opt(2025, 1, 5).unwrap();
        let stats = history.stats_range(start, end);

        assert_eq!(stats.active_days, 5);
        assert_eq!(stats.total_tokens, 1000 + 2000 + 3000 + 4000 + 5000);
        assert!((stats.total_cost - 0.15).abs() < 0.001);
        assert_eq!(stats.total_sessions, 5);
    }

    #[test]
    fn test_cost_history_serialization() {
        let mut history = CostHistory::new();
        let date = NaiveDate::from_ymd_opt(2025, 1, 15).unwrap();
        history.record(CostDataPoint::with_values(date, 1000, 600, 400, 100, 0.05, 5, 20));

        let json = serde_json::to_string(&history).unwrap();
        let loaded: CostHistory = serde_json::from_str(&json).unwrap();

        assert_eq!(loaded.len(), 1);
        let point = loaded.get(date).unwrap();
        assert_eq!(point.tokens, 1000);
    }

    #[test]
    fn test_cost_history_to_csv() {
        let mut history = CostHistory::new();
        let date = NaiveDate::from_ymd_opt(2025, 1, 15).unwrap();
        history.record(CostDataPoint::with_values(date, 1000, 600, 400, 100, 0.05, 5, 20));

        let csv = history.to_csv();
        assert!(csv.contains("date,tokens,input_tokens"));
        assert!(csv.contains("2025-01-15"));
        assert!(csv.contains("1000"));
    }

    #[test]
    fn test_calculate_trend() {
        // Increasing trend
        let values = vec![1.0, 2.0, 3.0, 4.0, 5.0];
        let trend = calculate_trend(&values);
        assert!(trend > 0.0);

        // Decreasing trend
        let values = vec![5.0, 4.0, 3.0, 2.0, 1.0];
        let trend = calculate_trend(&values);
        assert!(trend < 0.0);

        // Flat trend
        let values = vec![3.0, 3.0, 3.0, 3.0, 3.0];
        let trend = calculate_trend(&values);
        assert!(trend.abs() < 0.001);
    }

    #[test]
    fn test_cost_period_stats_default() {
        let stats = CostPeriodStats::default();
        assert_eq!(stats.total_tokens, 0);
        assert_eq!(stats.total_cost, 0.0);
        assert_eq!(stats.active_days, 0);
    }

    #[test]
    fn test_cost_history_first_last_date() {
        let mut history = CostHistory::new();

        assert!(history.first_date().is_none());
        assert!(history.last_date().is_none());

        let date1 = NaiveDate::from_ymd_opt(2025, 1, 10).unwrap();
        let date2 = NaiveDate::from_ymd_opt(2025, 1, 20).unwrap();

        history.record(CostDataPoint::new(date1));
        history.record(CostDataPoint::new(date2));

        assert_eq!(history.first_date(), Some(date1));
        assert_eq!(history.last_date(), Some(date2));
    }

    #[test]
    fn test_cost_history_clear() {
        let mut history = CostHistory::new();
        let date = NaiveDate::from_ymd_opt(2025, 1, 15).unwrap();
        history.record(CostDataPoint::new(date));

        assert!(!history.is_empty());
        history.clear();
        assert!(history.is_empty());
    }
}
