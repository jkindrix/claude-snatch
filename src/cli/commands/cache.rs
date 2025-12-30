//! Cache command implementation.
//!
//! Manages the session metadata and parsed entries cache.

use crate::cache::global_cache;
use crate::cli::{CacheAction, CacheArgs, Cli};
use crate::discovery::format_size;
use crate::error::Result;

/// Run the cache command.
pub fn run(_cli: &Cli, args: &CacheArgs) -> Result<()> {
    match &args.action {
        CacheAction::Stats => show_stats(),
        CacheAction::Clear => clear_cache(),
        CacheAction::Invalidate => invalidate_stale(),
        CacheAction::Status { enable, disable } => {
            if *enable {
                eprintln!("Caching is enabled by default. To persist, edit config with: snatch config set cache.enabled true");
                Ok(())
            } else if *disable {
                eprintln!("To disable caching, edit config with: snatch config set cache.enabled false");
                Ok(())
            } else {
                show_status()
            }
        }
    }
}

/// Show cache statistics.
fn show_stats() -> Result<()> {
    let cache = global_cache();
    let stats = cache.stats();

    println!("Cache Status");
    println!("============");
    println!("Enabled: {}", if stats.enabled { "yes" } else { "no" });
    println!();

    println!("Metadata Cache:");
    println!("  Entries: {} / {}", stats.metadata.entry_count, stats.metadata.max_entries);
    println!(
        "  Size: {} / {} ({:.1}%)",
        format_size(stats.metadata.current_size as u64),
        format_size(stats.metadata.max_size as u64),
        stats.metadata.size_usage_percent()
    );
    println!();

    println!("Entries Cache:");
    println!("  Entries: {} / {}", stats.entries.entry_count, stats.entries.max_entries);
    println!(
        "  Size: {} / {} ({:.1}%)",
        format_size(stats.entries.current_size as u64),
        format_size(stats.entries.max_size as u64),
        stats.entries.size_usage_percent()
    );
    println!();

    println!("Total:");
    println!("  Entries: {}", stats.total_entries());
    println!("  Size: {}", format_size(stats.total_size() as u64));

    Ok(())
}

/// Clear all cached data.
fn clear_cache() -> Result<()> {
    let cache = global_cache();
    let stats_before = cache.stats();

    cache.clear();

    println!(
        "Cleared {} entries ({})",
        stats_before.total_entries(),
        format_size(stats_before.total_size() as u64)
    );

    Ok(())
}

/// Invalidate stale cache entries.
fn invalidate_stale() -> Result<()> {
    let cache = global_cache();
    let entries_before = cache.stats().total_entries();

    cache.invalidate_stale();

    let entries_after = cache.stats().total_entries();
    let removed = entries_before - entries_after;

    if removed > 0 {
        println!("Invalidated {} stale entries", removed);
    } else {
        println!("No stale entries found");
    }

    Ok(())
}

/// Show cache status.
fn show_status() -> Result<()> {
    let cache = global_cache();
    let stats = cache.stats();

    if stats.enabled {
        println!("Cache: enabled");
        println!(
            "Usage: {} entries, {}",
            stats.total_entries(),
            format_size(stats.total_size() as u64)
        );
    } else {
        println!("Cache: disabled");
    }

    Ok(())
}
