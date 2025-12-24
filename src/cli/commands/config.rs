//! Config command implementation.
//!
//! View and modify snatch configuration settings.

use std::path::PathBuf;

use crate::cli::{Cli, ConfigArgs, ConfigAction, OutputFormat};
use crate::config::{default_config_path, Config};
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
            println!("  max_size = {}", config.cache.max_size);
            println!("  ttl_seconds = {}", config.cache.ttl_seconds);
        }
    }

    Ok(())
}

/// Get a specific configuration value.
fn get_config_value(cli: &Cli, key: &str) -> Result<()> {
    let config = Config::load().unwrap_or_default();

    let value = match key {
        "default_format.format" => config.default_format.format.clone(),
        "default_format.include_thinking" => config.default_format.include_thinking.to_string(),
        "default_format.include_tool_use" => config.default_format.include_tool_use.to_string(),
        "default_format.include_timestamps" => config.default_format.include_timestamps.to_string(),
        "default_format.pretty_json" => config.default_format.pretty_json.to_string(),

        "theme.name" => config.theme.name.clone(),
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
