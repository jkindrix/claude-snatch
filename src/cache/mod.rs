//! Caching infrastructure for claude-snatch.
//!
//! This module provides LRU caching for:
//! - Session metadata (CACHE-001)
//! - Parsed message entries (CACHE-002)
//! - Automatic invalidation on file changes (CACHE-003)
//! - Configurable cache size limits (CACHE-004)
//!
//! All caches are thread-safe and use modification time for invalidation.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::SystemTime;

use parking_lot::RwLock;

use crate::config::CacheConfig;
use crate::discovery::QuickSessionMetadata;
use crate::error::Result;
use crate::model::LogEntry;

/// Cache key combining path and modification time.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct CacheKey {
    /// Path to the file.
    path: PathBuf,
    /// Modification time when cached.
    mtime: SystemTime,
}

impl CacheKey {
    /// Create a new cache key from a path.
    fn from_path(path: &Path) -> Option<Self> {
        let mtime = std::fs::metadata(path).ok()?.modified().ok()?;
        Some(Self {
            path: path.to_path_buf(),
            mtime,
        })
    }

    /// Check if this key is still valid (file hasn't changed).
    fn is_valid(&self) -> bool {
        std::fs::metadata(&self.path)
            .and_then(|m| m.modified())
            .map(|mtime| mtime == self.mtime)
            .unwrap_or(false)
    }
}

/// LRU cache entry with access tracking.
#[derive(Debug)]
struct CacheEntry<T> {
    /// The cached value.
    value: T,
    /// Access order (higher = more recent).
    access_order: u64,
    /// Size estimate in bytes.
    size_estimate: usize,
}

/// Generic LRU cache with mtime-based invalidation.
#[derive(Debug)]
pub struct LruCache<T> {
    /// Cache entries keyed by path.
    entries: HashMap<PathBuf, (CacheKey, CacheEntry<T>)>,
    /// Global access counter for LRU tracking.
    access_counter: u64,
    /// Maximum number of entries.
    max_entries: usize,
    /// Maximum total size in bytes.
    max_size: usize,
    /// Current estimated size.
    current_size: usize,
}

impl<T> LruCache<T> {
    /// Create a new cache with the specified limits.
    pub fn new(max_entries: usize, max_size: usize) -> Self {
        Self {
            entries: HashMap::new(),
            access_counter: 0,
            max_entries,
            max_size,
            current_size: 0,
        }
    }

    /// Get an entry if it exists and is still valid.
    pub fn get(&mut self, path: &Path) -> Option<&T> {
        // Check if entry exists
        if let Some((key, entry)) = self.entries.get_mut(path) {
            // Validate mtime
            if key.is_valid() {
                // Update access order
                self.access_counter += 1;
                entry.access_order = self.access_counter;
                return Some(&entry.value);
            } else {
                // Entry is stale, will be removed
                return None;
            }
        }
        None
    }

    /// Insert a value into the cache.
    pub fn insert(&mut self, path: &Path, value: T, size_estimate: usize) {
        // Create cache key
        let Some(key) = CacheKey::from_path(path) else {
            return; // Can't cache if we can't get mtime
        };

        // Remove old entry if exists
        if let Some((_, old_entry)) = self.entries.remove(path) {
            self.current_size = self.current_size.saturating_sub(old_entry.size_estimate);
        }

        // Evict if necessary
        self.evict_if_needed(size_estimate);

        // Insert new entry
        self.access_counter += 1;
        self.entries.insert(
            path.to_path_buf(),
            (
                key,
                CacheEntry {
                    value,
                    access_order: self.access_counter,
                    size_estimate,
                },
            ),
        );
        self.current_size += size_estimate;
    }

    /// Evict entries if cache is over limits.
    fn evict_if_needed(&mut self, incoming_size: usize) {
        // Evict by count
        while self.entries.len() >= self.max_entries {
            self.evict_lru();
        }

        // Evict by size
        while self.current_size + incoming_size > self.max_size && !self.entries.is_empty() {
            self.evict_lru();
        }
    }

    /// Evict the least recently used entry.
    fn evict_lru(&mut self) {
        if self.entries.is_empty() {
            return;
        }

        // Find LRU entry
        let lru_path = self
            .entries
            .iter()
            .min_by_key(|(_, (_, entry))| entry.access_order)
            .map(|(path, _)| path.clone());

        if let Some(path) = lru_path {
            if let Some((_, entry)) = self.entries.remove(&path) {
                self.current_size = self.current_size.saturating_sub(entry.size_estimate);
            }
        }
    }

    /// Remove an entry.
    pub fn remove(&mut self, path: &Path) {
        if let Some((_, entry)) = self.entries.remove(path) {
            self.current_size = self.current_size.saturating_sub(entry.size_estimate);
        }
    }

    /// Invalidate stale entries.
    pub fn invalidate_stale(&mut self) {
        let stale_paths: Vec<PathBuf> = self
            .entries
            .iter()
            .filter(|(_, (key, _))| !key.is_valid())
            .map(|(path, _)| path.clone())
            .collect();

        for path in stale_paths {
            self.remove(&path);
        }
    }

    /// Clear all entries.
    pub fn clear(&mut self) {
        self.entries.clear();
        self.current_size = 0;
        self.access_counter = 0;
    }

    /// Get cache statistics.
    pub fn stats(&self) -> CacheStats {
        CacheStats {
            entry_count: self.entries.len(),
            max_entries: self.max_entries,
            current_size: self.current_size,
            max_size: self.max_size,
        }
    }
}

/// Cache statistics.
#[derive(Debug, Clone)]
pub struct CacheStats {
    /// Number of entries.
    pub entry_count: usize,
    /// Maximum entries allowed.
    pub max_entries: usize,
    /// Current estimated size in bytes.
    pub current_size: usize,
    /// Maximum size allowed in bytes.
    pub max_size: usize,
}

impl CacheStats {
    /// Get usage as percentage.
    pub fn usage_percent(&self) -> f64 {
        if self.max_entries == 0 {
            return 0.0;
        }
        (self.entry_count as f64 / self.max_entries as f64) * 100.0
    }

    /// Get size usage as percentage.
    pub fn size_usage_percent(&self) -> f64 {
        if self.max_size == 0 {
            return 0.0;
        }
        (self.current_size as f64 / self.max_size as f64) * 100.0
    }
}

/// Thread-safe session metadata cache.
pub struct SessionMetadataCache {
    inner: RwLock<LruCache<QuickSessionMetadata>>,
}

impl SessionMetadataCache {
    /// Create a new metadata cache.
    pub fn new(config: &CacheConfig) -> Self {
        // Default: 1000 sessions, 10MB for metadata
        let max_entries = 1000;
        let max_size = (config.max_size / 10) as usize; // 10% of total cache for metadata
        Self {
            inner: RwLock::new(LruCache::new(max_entries, max_size)),
        }
    }

    /// Get cached metadata for a session.
    pub fn get(&self, path: &Path) -> Option<QuickSessionMetadata> {
        self.inner.write().get(path).cloned()
    }

    /// Cache session metadata.
    pub fn insert(&self, path: &Path, metadata: QuickSessionMetadata) {
        // Estimate size (rough approximation)
        let size = std::mem::size_of::<QuickSessionMetadata>()
            + metadata.session_id.len()
            + metadata.version.as_ref().map_or(0, String::len);
        self.inner.write().insert(path, metadata, size);
    }

    /// Invalidate stale entries.
    pub fn invalidate_stale(&self) {
        self.inner.write().invalidate_stale();
    }

    /// Clear all entries.
    pub fn clear(&self) {
        self.inner.write().clear();
    }

    /// Get statistics.
    pub fn stats(&self) -> CacheStats {
        self.inner.read().stats()
    }
}

impl std::fmt::Debug for SessionMetadataCache {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SessionMetadataCache")
            .field("stats", &self.stats())
            .finish()
    }
}

/// Thread-safe parsed entries cache.
pub struct ParsedEntriesCache {
    inner: RwLock<LruCache<Arc<Vec<LogEntry>>>>,
}

impl ParsedEntriesCache {
    /// Create a new parsed entries cache.
    pub fn new(config: &CacheConfig) -> Self {
        // Default: 100 sessions, 90% of cache for parsed entries
        let max_entries = 100;
        let max_size = (config.max_size * 9 / 10) as usize;
        Self {
            inner: RwLock::new(LruCache::new(max_entries, max_size)),
        }
    }

    /// Get cached entries for a session.
    pub fn get(&self, path: &Path) -> Option<Arc<Vec<LogEntry>>> {
        self.inner.write().get(path).cloned()
    }

    /// Cache parsed entries.
    pub fn insert(&self, path: &Path, entries: Vec<LogEntry>) {
        // Estimate size based on entry count (rough approximation: 1KB per entry)
        let size = entries.len() * 1024;
        self.inner.write().insert(path, Arc::new(entries), size);
    }

    /// Get entries or parse and cache.
    pub fn get_or_insert<F>(&self, path: &Path, parse_fn: F) -> Result<Arc<Vec<LogEntry>>>
    where
        F: FnOnce() -> Result<Vec<LogEntry>>,
    {
        // Try cache first
        if let Some(entries) = self.get(path) {
            return Ok(entries);
        }

        // Parse and cache
        let entries = parse_fn()?;
        let arc_entries = Arc::new(entries);

        // Cache it
        {
            let size = arc_entries.len() * 1024;
            self.inner.write().insert(path, arc_entries.clone(), size);
        }

        Ok(arc_entries)
    }

    /// Invalidate stale entries.
    pub fn invalidate_stale(&self) {
        self.inner.write().invalidate_stale();
    }

    /// Clear all entries.
    pub fn clear(&self) {
        self.inner.write().clear();
    }

    /// Get statistics.
    pub fn stats(&self) -> CacheStats {
        self.inner.read().stats()
    }
}

impl std::fmt::Debug for ParsedEntriesCache {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ParsedEntriesCache")
            .field("stats", &self.stats())
            .finish()
    }
}

/// Combined cache manager for the application.
pub struct CacheManager {
    /// Session metadata cache.
    pub metadata: SessionMetadataCache,
    /// Parsed entries cache.
    pub entries: ParsedEntriesCache,
    /// Whether caching is enabled.
    enabled: bool,
}

impl CacheManager {
    /// Create a new cache manager from configuration.
    pub fn new(config: &CacheConfig) -> Self {
        Self {
            metadata: SessionMetadataCache::new(config),
            entries: ParsedEntriesCache::new(config),
            enabled: config.enabled,
        }
    }

    /// Create a disabled cache manager (no-op operations).
    pub fn disabled() -> Self {
        let config = CacheConfig {
            enabled: false,
            max_size: 0,
            ..Default::default()
        };
        Self {
            metadata: SessionMetadataCache::new(&config),
            entries: ParsedEntriesCache::new(&config),
            enabled: false,
        }
    }

    /// Check if caching is enabled.
    pub fn is_enabled(&self) -> bool {
        self.enabled
    }

    /// Get cached session metadata.
    pub fn get_metadata(&self, path: &Path) -> Option<QuickSessionMetadata> {
        if self.enabled {
            self.metadata.get(path)
        } else {
            None
        }
    }

    /// Cache session metadata.
    pub fn cache_metadata(&self, path: &Path, metadata: QuickSessionMetadata) {
        if self.enabled {
            self.metadata.insert(path, metadata);
        }
    }

    /// Get cached parsed entries.
    pub fn get_entries(&self, path: &Path) -> Option<Arc<Vec<LogEntry>>> {
        if self.enabled {
            self.entries.get(path)
        } else {
            None
        }
    }

    /// Cache parsed entries.
    pub fn cache_entries(&self, path: &Path, entries: Vec<LogEntry>) {
        if self.enabled {
            self.entries.insert(path, entries);
        }
    }

    /// Get entries from cache or parse.
    pub fn get_or_parse<F>(&self, path: &Path, parse_fn: F) -> Result<Arc<Vec<LogEntry>>>
    where
        F: FnOnce() -> Result<Vec<LogEntry>>,
    {
        if self.enabled {
            self.entries.get_or_insert(path, parse_fn)
        } else {
            parse_fn().map(Arc::new)
        }
    }

    /// Invalidate all stale entries.
    pub fn invalidate_stale(&self) {
        if self.enabled {
            self.metadata.invalidate_stale();
            self.entries.invalidate_stale();
        }
    }

    /// Clear all caches.
    pub fn clear(&self) {
        self.metadata.clear();
        self.entries.clear();
    }

    /// Get combined statistics.
    pub fn stats(&self) -> CacheManagerStats {
        CacheManagerStats {
            enabled: self.enabled,
            metadata: self.metadata.stats(),
            entries: self.entries.stats(),
        }
    }
}

impl std::fmt::Debug for CacheManager {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CacheManager")
            .field("enabled", &self.enabled)
            .field("stats", &self.stats())
            .finish()
    }
}

/// Combined cache statistics.
#[derive(Debug, Clone)]
pub struct CacheManagerStats {
    /// Whether caching is enabled.
    pub enabled: bool,
    /// Metadata cache stats.
    pub metadata: CacheStats,
    /// Entries cache stats.
    pub entries: CacheStats,
}

impl CacheManagerStats {
    /// Get total entry count.
    pub fn total_entries(&self) -> usize {
        self.metadata.entry_count + self.entries.entry_count
    }

    /// Get total current size.
    pub fn total_size(&self) -> usize {
        self.metadata.current_size + self.entries.current_size
    }
}

/// Global cache instance.
static GLOBAL_CACHE: once_cell::sync::OnceCell<CacheManager> = once_cell::sync::OnceCell::new();

/// Initialize the global cache.
pub fn init_global_cache(config: &CacheConfig) {
    let _ = GLOBAL_CACHE.set(CacheManager::new(config));
}

/// Get the global cache manager.
pub fn global_cache() -> &'static CacheManager {
    GLOBAL_CACHE.get_or_init(|| {
        let config = CacheConfig::default();
        CacheManager::new(&config)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_lru_cache_basic() {
        let mut cache: LruCache<String> = LruCache::new(3, 10000);

        // This test uses temp files to test caching
        let temp_dir = tempfile::tempdir().unwrap();
        let path1 = temp_dir.path().join("file1.txt");
        let path2 = temp_dir.path().join("file2.txt");

        std::fs::write(&path1, "content1").unwrap();
        std::fs::write(&path2, "content2").unwrap();

        cache.insert(&path1, "value1".to_string(), 100);
        cache.insert(&path2, "value2".to_string(), 100);

        assert_eq!(cache.get(&path1), Some(&"value1".to_string()));
        assert_eq!(cache.get(&path2), Some(&"value2".to_string()));
    }

    #[test]
    fn test_lru_eviction() {
        let mut cache: LruCache<String> = LruCache::new(2, 10000);

        let temp_dir = tempfile::tempdir().unwrap();
        let path1 = temp_dir.path().join("file1.txt");
        let path2 = temp_dir.path().join("file2.txt");
        let path3 = temp_dir.path().join("file3.txt");

        std::fs::write(&path1, "1").unwrap();
        std::fs::write(&path2, "2").unwrap();
        std::fs::write(&path3, "3").unwrap();

        cache.insert(&path1, "v1".to_string(), 100);
        cache.insert(&path2, "v2".to_string(), 100);

        // Access path1 to make it more recent
        let _ = cache.get(&path1);

        // Insert path3, should evict path2 (least recently used)
        cache.insert(&path3, "v3".to_string(), 100);

        assert!(cache.get(&path1).is_some());
        assert!(cache.get(&path2).is_none()); // Evicted
        assert!(cache.get(&path3).is_some());
    }

    #[test]
    fn test_mtime_invalidation() {
        let mut cache: LruCache<String> = LruCache::new(10, 10000);

        let temp_dir = tempfile::tempdir().unwrap();
        let path = temp_dir.path().join("test.txt");

        std::fs::write(&path, "original").unwrap();
        cache.insert(&path, "cached".to_string(), 100);

        assert_eq!(cache.get(&path), Some(&"cached".to_string()));

        // Modify the file (simulate file change)
        std::thread::sleep(std::time::Duration::from_millis(10));
        std::fs::write(&path, "modified").unwrap();

        // Cache should be invalidated
        assert!(cache.get(&path).is_none());
    }

    #[test]
    fn test_cache_manager() {
        let config = CacheConfig::default();
        let manager = CacheManager::new(&config);

        assert!(manager.is_enabled());

        let stats = manager.stats();
        assert!(stats.enabled);
        assert_eq!(stats.metadata.entry_count, 0);
        assert_eq!(stats.entries.entry_count, 0);
    }

    #[test]
    fn test_disabled_cache() {
        let manager = CacheManager::disabled();

        assert!(!manager.is_enabled());

        let temp_dir = tempfile::tempdir().unwrap();
        let path = temp_dir.path().join("test.jsonl");
        std::fs::write(&path, "content").unwrap();

        // Cache operations should be no-ops
        manager.cache_entries(&path, vec![]);
        assert!(manager.get_entries(&path).is_none());
    }
}
