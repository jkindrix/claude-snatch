//! Config command implementation.
//!
//! View and modify snatch configuration settings.

use std::path::PathBuf;

use crate::cli::{Cli, ConfigArgs, ConfigAction, OutputFormat};
use crate::config::{default_config_path, Config};
use crate::discovery::format_size;
use crate::error::{Result, SnatchError};

/// Run the config command.
pub fn run(cli: &Cli, args: &ConfigArgs) -> Result<()> {
    match &args.action {
        ConfigAction::Show => show_config(cli),
        ConfigAction::Get { key } => get_config_value(cli, key),
        ConfigAction::Set { key, value } => set_config_value(cli, key, value),
        ConfigAction::Path => show_config_path(),
        ConfigAction::Init => init_config(),
        ConfigAction::Reset => reset_config(),
    }
}

/// Show full configuration.
fn show_config(cli: &Cli) -> Result<()> {
    let config = Config::load().unwrap_or_default();

    match cli.effective_output() {
        OutputFormat::Json => {
            let json = serde_json::to_string_pretty(&config)?;
            println!("{json}");
        }
        _ => {
            println!("Snatch Configuration");
            println!("====================\n");

            println!("[default_format]");
            println!("  format = \"{}\"", config.default_format.format);
            println!("  include_thinking = {}", config.default_format.include_thinking);
            println!("  include_tool_use = {}", config.default_format.include_tool_use);
            println!("  include_timestamps = {}", config.default_format.include_timestamps);
            println!("  pretty_json = {}", config.default_format.pretty_json);
            println!();

            println!("[theme]");
            println!("  name = \"{}\"", config.theme.name);
            println!("  color = {}", config.theme.color);
            println!("  unicode = {}", config.theme.unicode);
            println!();

            println!("[display]");
            println!("  full_ids = {}", config.display.full_ids);
            println!("  show_sizes = {}", config.display.show_sizes);
            println!("  truncate_at = {}", config.display.truncate_at);
            println!("  context_lines = {}", config.display.context_lines);
            println!();

            println!("[cache]");
            println!("  enabled = {}", config.cache.enabled);
            if let Some(dir) = &config.cache.directory {
                println!("  directory = \"{}\"", dir.display());
            } else {
                println!("  directory = # not set (auto-detect)");
            }
            println!(
                "  max_size = {} ({} bytes)",
                format_size(config.cache.max_size),
                config.cache.max_size
            );
            println!(
                "  ttl_seconds = {} ({} seconds)",
                format_duration_human(config.cache.ttl_seconds),
                config.cache.ttl_seconds
            );
            println!();

            println!("[budget]");
            if let Some(limit) = config.budget.daily_limit {
                println!("  daily_limit = {:.2} # ${:.2} per day", limit, limit);
            } else {
                println!("  daily_limit = # not set");
            }
            if let Some(limit) = config.budget.weekly_limit {
                println!("  weekly_limit = {:.2} # ${:.2} per week", limit, limit);
            } else {
                println!("  weekly_limit = # not set");
            }
            if let Some(limit) = config.budget.monthly_limit {
                println!("  monthly_limit = {:.2} # ${:.2} per month", limit, limit);
            } else {
                println!("  monthly_limit = # not set");
            }
            println!("  warning_threshold = {:.0}%", config.budget.warning_threshold * 100.0);
            println!("  show_in_stats = {}", config.budget.show_in_stats);
        }
    }

    Ok(())
}

/// Get a specific configuration value.
fn get_config_value(cli: &Cli, key: &str) -> Result<()> {
    let config = Config::load().unwrap_or_default();

    let value = match key {
        "default_format.format" => config.default_format.format,
        "default_format.include_thinking" => config.default_format.include_thinking.to_string(),
        "default_format.include_tool_use" => config.default_format.include_tool_use.to_string(),
        "default_format.include_timestamps" => config.default_format.include_timestamps.to_string(),
        "default_format.pretty_json" => config.default_format.pretty_json.to_string(),

        "theme.name" => config.theme.name,
        "theme.color" => config.theme.color.to_string(),
        "theme.unicode" => config.theme.unicode.to_string(),

        "display.full_ids" => config.display.full_ids.to_string(),
        "display.show_sizes" => config.display.show_sizes.to_string(),
        "display.truncate_at" => config.display.truncate_at.to_string(),
        "display.context_lines" => config.display.context_lines.to_string(),

        "cache.enabled" => config.cache.enabled.to_string(),
        "cache.directory" => config.cache.directory
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| "(auto-detect)".to_string()),
        "cache.max_size" => config.cache.max_size.to_string(),
        "cache.ttl_seconds" => config.cache.ttl_seconds.to_string(),

        "budget.daily_limit" => config.budget.daily_limit
            .map(|v| format!("{:.2}", v))
            .unwrap_or_else(|| "(not set)".to_string()),
        "budget.weekly_limit" => config.budget.weekly_limit
            .map(|v| format!("{:.2}", v))
            .unwrap_or_else(|| "(not set)".to_string()),
        "budget.monthly_limit" => config.budget.monthly_limit
            .map(|v| format!("{:.2}", v))
            .unwrap_or_else(|| "(not set)".to_string()),
        "budget.warning_threshold" => format!("{:.0}", config.budget.warning_threshold * 100.0),
        "budget.show_in_stats" => config.budget.show_in_stats.to_string(),

        _ => return Err(SnatchError::ConfigError {
            message: format!("Unknown configuration key: {key}"),
        }),
    };

    match cli.effective_output() {
        OutputFormat::Json => {
            println!("{}", serde_json::json!({ key: value }));
        }
        _ => {
            println!("{value}");
        }
    }

    Ok(())
}

/// Set a configuration value.
fn set_config_value(_cli: &Cli, key: &str, value: &str) -> Result<()> {
    let mut config = Config::load().unwrap_or_default();

    match key {
        "default_format.format" => {
            config.default_format.format = value.to_string();
        }
        "default_format.include_thinking" => {
            config.default_format.include_thinking = parse_bool(value)?;
        }
        "default_format.include_tool_use" => {
            config.default_format.include_tool_use = parse_bool(value)?;
        }
        "default_format.include_timestamps" => {
            config.default_format.include_timestamps = parse_bool(value)?;
        }
        "default_format.pretty_json" => {
            config.default_format.pretty_json = parse_bool(value)?;
        }

        "theme.name" => {
            config.theme.name = value.to_string();
        }
        "theme.color" => {
            config.theme.color = parse_bool(value)?;
        }
        "theme.unicode" => {
            config.theme.unicode = parse_bool(value)?;
        }

        "display.full_ids" => {
            config.display.full_ids = parse_bool(value)?;
        }
        "display.show_sizes" => {
            config.display.show_sizes = parse_bool(value)?;
        }
        "display.truncate_at" => {
            config.display.truncate_at = parse_usize(value)?;
        }
        "display.context_lines" => {
            config.display.context_lines = parse_usize(value)?;
        }

        "cache.enabled" => {
            config.cache.enabled = parse_bool(value)?;
        }
        "cache.directory" => {
            config.cache.directory = Some(PathBuf::from(value));
        }
        "cache.max_size" => {
            config.cache.max_size = parse_u64(value)?;
        }
        "cache.ttl_seconds" => {
            config.cache.ttl_seconds = parse_u64(value)?;
        }

        "budget.daily_limit" => {
            config.budget.daily_limit = parse_optional_f64(value)?;
        }
        "budget.weekly_limit" => {
            config.budget.weekly_limit = parse_optional_f64(value)?;
        }
        "budget.monthly_limit" => {
            config.budget.monthly_limit = parse_optional_f64(value)?;
        }
        "budget.warning_threshold" => {
            let pct = parse_f64(value)?;
            // Accept either 0-1 range or percentage (0-100)
            config.budget.warning_threshold = if pct > 1.0 { pct / 100.0 } else { pct };
        }
        "budget.show_in_stats" => {
            config.budget.show_in_stats = parse_bool(value)?;
        }

        _ => return Err(SnatchError::ConfigError {
            message: format!("Unknown configuration key: {key}"),
        }),
    }

    config.save()?;
    println!("Set {key} = {value}");

    Ok(())
}

/// Show configuration file path.
fn show_config_path() -> Result<()> {
    let path = default_config_path()?;
    println!("{}", path.display());
    Ok(())
}

/// Initialize configuration file with defaults.
fn init_config() -> Result<()> {
    let path = default_config_path()?;

    if path.exists() {
        println!("Configuration file already exists at: {}", path.display());
        println!("Use 'snatch config reset' to reset to defaults.");
        return Ok(());
    }

    let config = Config::default();
    config.save()?;
    println!("Created configuration file at: {}", path.display());

    Ok(())
}

/// Reset configuration to defaults.
fn reset_config() -> Result<()> {
    let path = default_config_path()?;

    if !path.exists() {
        println!("No configuration file exists. Use 'snatch config init' to create one.");
        return Ok(());
    }

    let config = Config::default();
    config.save()?;
    println!("Reset configuration to defaults at: {}", path.display());

    Ok(())
}

/// Parse boolean value.
fn parse_bool(s: &str) -> Result<bool> {
    match s.to_lowercase().as_str() {
        "true" | "yes" | "1" | "on" => Ok(true),
        "false" | "no" | "0" | "off" => Ok(false),
        _ => Err(SnatchError::ConfigError {
            message: format!("Invalid boolean value: {s}. Use true/false."),
        }),
    }
}

/// Parse usize value.
fn parse_usize(s: &str) -> Result<usize> {
    s.parse().map_err(|_| SnatchError::ConfigError {
        message: format!("Invalid number: {s}"),
    })
}

/// Parse u64 value.
fn parse_u64(s: &str) -> Result<u64> {
    s.parse().map_err(|_| SnatchError::ConfigError {
        message: format!("Invalid number: {s}"),
    })
}

/// Parse f64 value.
fn parse_f64(s: &str) -> Result<f64> {
    s.parse().map_err(|_| SnatchError::ConfigError {
        message: format!("Invalid decimal number: {s}"),
    })
}

/// Parse optional f64 value (supports "none", "unset", "clear" to remove).
fn parse_optional_f64(s: &str) -> Result<Option<f64>> {
    match s.to_lowercase().as_str() {
        "none" | "unset" | "clear" | "0" | "" => Ok(None),
        _ => {
            let v = s.parse().map_err(|_| SnatchError::ConfigError {
                message: format!("Invalid decimal number: {s}. Use a number like 100.00 or 'none' to clear."),
            })?;
            Ok(Some(v))
        }
    }
}

/// Format seconds as human-readable duration.
fn format_duration_human(seconds: u64) -> String {
    if seconds == 0 {
        return "0 seconds".to_string();
    }

    let hours = seconds / 3600;
    let minutes = (seconds % 3600) / 60;
    let secs = seconds % 60;

    let mut parts = Vec::new();
    if hours > 0 {
        parts.push(if hours == 1 {
            "1 hour".to_string()
        } else {
            format!("{hours} hours")
        });
    }
    if minutes > 0 {
        parts.push(if minutes == 1 {
            "1 minute".to_string()
        } else {
            format!("{minutes} minutes")
        });
    }
    if secs > 0 {
        parts.push(if secs == 1 {
            "1 second".to_string()
        } else {
            format!("{secs} seconds")
        });
    }

    parts.join(" ")
}
