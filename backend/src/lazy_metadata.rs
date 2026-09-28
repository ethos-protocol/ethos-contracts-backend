/// Lazy loading for vault metadata (Issue #564).
///
/// Large metadata blobs are stored in a dedicated `vault_metadata` table and
/// are never loaded as part of a standard vault query.  Callers ask for
/// metadata explicitly via [`LazyMetadataLoader::get`]; results are kept in
/// an in-process [`MetadataCache`] so repeated accesses within the same
/// process do not hit SQLite.
///
/// # Design
/// * The core [`Vault`] type does **not** carry metadata — it stays small.
/// * [`LazyMetadataLoader`] wraps the `Db` connection and the cache; it is
///   cheaply `Clone` because both inner types are `Arc`-wrapped.
/// * The in-memory cache uses a configurable TTL so stale entries are not
///   served indefinitely.
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use serde::{Deserialize, Serialize};

use crate::db::Db;

// ── Public types ─────────────────────────────────────────────────────────────

/// Arbitrary key-value metadata attached to a vault.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct VaultMetadata {
    /// Owning vault identifier.
    pub vault_id: String,
    /// Free-form metadata fields stored as a JSON object.
    pub fields: serde_json::Value,
    /// When this metadata record was last written.
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

/// Configuration for the in-process metadata cache.
#[derive(Debug, Clone)]
pub struct MetadataCacheConfig {
    /// How long a cached entry is considered fresh.
    pub ttl: Duration,
    /// Maximum number of entries the cache will hold.  Oldest entries are
    /// evicted when the limit is exceeded.
    pub max_entries: usize,
}

impl Default for MetadataCacheConfig {
    fn default() -> Self {
        Self {
            ttl: Duration::from_secs(300), // 5 minutes
            max_entries: 1_000,
        }
    }
}

/// In-process LRU-style metadata cache.
///
/// Access order is tracked so the least-recently-used entry is evicted when
/// `max_entries` is exceeded.
pub struct MetadataCache {
    entries: Mutex<HashMap<String, CacheEntry>>,
    config: MetadataCacheConfig,
    /// Hit / miss counters for observability.
    hits: std::sync::atomic::AtomicU64,
    misses: std::sync::atomic::AtomicU64,
}

struct CacheEntry {
    metadata: VaultMetadata,
    inserted_at: Instant,
    last_accessed: Instant,
}

impl MetadataCache {
    /// Create a new cache with the given configuration.
    pub fn new(config: MetadataCacheConfig) -> Self {
        Self {
            entries: Mutex::new(HashMap::new()),
            config,
            hits: std::sync::atomic::AtomicU64::new(0),
            misses: std::sync::atomic::AtomicU64::new(0),
        }
    }

    /// Retrieve a cached entry.  Returns `None` if absent or expired.
    pub fn get(&self, vault_id: &str) -> Option<VaultMetadata> {
        let mut guard = self.entries.lock().unwrap();
        if let Some(entry) = guard.get_mut(vault_id) {
            if entry.inserted_at.elapsed() < self.config.ttl {
                entry.last_accessed = Instant::now();
                self.hits
                    .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                return Some(entry.metadata.clone());
            }
            // Expired — remove eagerly.
            guard.remove(vault_id);
        }
        self.misses
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        None
    }

    /// Insert or update an entry, evicting the LRU entry if the cache is full.
    pub fn insert(&self, metadata: VaultMetadata) {
        let mut guard = self.entries.lock().unwrap();

        // Evict the least-recently-used entry when at capacity.
        if guard.len() >= self.config.max_entries && !guard.contains_key(&metadata.vault_id) {
            if let Some(lru_key) = guard
                .iter()
                .min_by_key(|(_, e)| e.last_accessed)
                .map(|(k, _)| k.clone())
            {
                guard.remove(&lru_key);
            }
        }

        guard.insert(
            metadata.vault_id.clone(),
            CacheEntry {
                metadata,
                inserted_at: Instant::now(),
                last_accessed: Instant::now(),
            },
        );
    }

    /// Explicitly invalidate a single entry (e.g. after a write).
    pub fn invalidate(&self, vault_id: &str) {
        self.entries.lock().unwrap().remove(vault_id);
    }

    /// Invalidate all entries.
    pub fn clear(&self) {
        self.entries.lock().unwrap().clear();
    }

    /// Return (hits, misses) counters.
    pub fn stats(&self) -> (u64, u64) {
        (
            self.hits.load(std::sync::atomic::Ordering::Relaxed),
            self.misses.load(std::sync::atomic::Ordering::Relaxed),
        )
    }

    /// Number of entries currently in the cache (including potentially stale ones).
    pub fn len(&self) -> usize {
        self.entries.lock().unwrap().len()
    }

    /// `true` when the cache has no entries.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

// ── LazyMetadataLoader ───────────────────────────────────────────────────────

/// Lazy loader for vault metadata.
///
/// Wraps a shared `Db` reference and a `MetadataCache` so callers can ask
/// for metadata without knowing whether it comes from the cache or SQLite.
#[derive(Clone)]
pub struct LazyMetadataLoader {
    db: Arc<Db>,
    cache: Arc<MetadataCache>,
}

impl LazyMetadataLoader {
    /// Create a loader backed by `db` and a default-configured cache.
    pub fn new(db: Arc<Db>) -> Self {
        Self::with_config(db, MetadataCacheConfig::default())
    }

    /// Create a loader with a custom cache configuration.
    pub fn with_config(db: Arc<Db>, config: MetadataCacheConfig) -> Self {
        Self {
            db,
            cache: Arc::new(MetadataCache::new(config)),
        }
    }

    /// Fetch metadata for `vault_id`, using the cache when possible.
    ///
    /// Returns `None` when no metadata row exists for the vault.
    pub fn get(&self, vault_id: &str) -> Option<VaultMetadata> {
        if let Some(cached) = self.cache.get(vault_id) {
            return Some(cached);
        }

        // Cache miss — load from SQLite.
        let loaded = self.db.get_vault_metadata(vault_id)?;
        self.cache.insert(loaded.clone());
        Some(loaded)
    }

    /// Write (upsert) metadata for `vault_id` and invalidate the cache entry.
    pub fn set(&self, metadata: VaultMetadata) -> Result<(), rusqlite::Error> {
        self.db.upsert_vault_metadata(&metadata)?;
        self.cache.invalidate(&metadata.vault_id);
        Ok(())
    }

    /// Delete metadata for `vault_id` and remove it from the cache.
    pub fn delete(&self, vault_id: &str) -> Result<(), rusqlite::Error> {
        self.db.delete_vault_metadata(vault_id)?;
        self.cache.invalidate(vault_id);
        Ok(())
    }

    /// Access the underlying cache directly (useful for monitoring endpoints).
    pub fn cache(&self) -> &MetadataCache {
        &self.cache
    }
}

// ── Db extension methods ─────────────────────────────────────────────────────
//
// These are implemented as free functions that forward to methods added to
// `Db` via the `DbMetadataExt` trait below.  The trait is sealed so it can
// only be called through `LazyMetadataLoader` or within this crate's tests.

impl Db {
    /// Upsert a vault metadata row into `vault_metadata`.
    pub fn upsert_vault_metadata(&self, m: &VaultMetadata) -> Result<(), rusqlite::Error> {
        self.conn.lock().unwrap().execute(
            r"INSERT INTO vault_metadata (vault_id, fields, updated_at)
              VALUES (?1, ?2, ?3)
              ON CONFLICT(vault_id) DO UPDATE SET
                  fields     = excluded.fields,
                  updated_at = excluded.updated_at",
            rusqlite::params![
                m.vault_id,
                m.fields.to_string(),
                m.updated_at.to_rfc3339(),
            ],
        )?;
        Ok(())
    }

    /// Retrieve a vault metadata row, or `None` if no row exists.
    pub fn get_vault_metadata(&self, vault_id: &str) -> Option<VaultMetadata> {
        let binding = self.conn.lock().unwrap();
        let mut stmt = binding
            .prepare(
                r"SELECT vault_id, fields, updated_at
                  FROM vault_metadata
                  WHERE vault_id = ?1",
            )
            .ok()?;

        stmt.query_row(rusqlite::params![vault_id], |row| {
            let fields_str: String = row.get(1)?;
            let updated_at_str: String = row.get(2)?;
            let fields: serde_json::Value =
                serde_json::from_str(&fields_str).unwrap_or(serde_json::Value::Null);
            let updated_at = chrono::DateTime::parse_from_rfc3339(&updated_at_str)
                .map(|dt| dt.with_timezone(&chrono::Utc))
                .unwrap_or_else(|_| chrono::Utc::now());
            Ok(VaultMetadata {
                vault_id: row.get(0)?,
                fields,
                updated_at,
            })
        })
        .ok()
    }

    /// Delete the metadata row for `vault_id`.
    pub fn delete_vault_metadata(&self, vault_id: &str) -> Result<(), rusqlite::Error> {
        self.conn.lock().unwrap().execute(
            "DELETE FROM vault_metadata WHERE vault_id = ?1",
            rusqlite::params![vault_id],
        )?;
        Ok(())
    }
}

// ── Unit tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn make_metadata(vault_id: &str) -> VaultMetadata {
        VaultMetadata {
            vault_id: vault_id.to_string(),
            fields: serde_json::json!({"note": "test", "priority": 1}),
            updated_at: chrono::Utc::now(),
        }
    }

    // ── MetadataCache unit tests ─────────────────────────────────────────────

    #[test]
    fn cache_miss_on_empty() {
        let cache = MetadataCache::new(MetadataCacheConfig::default());
        assert!(cache.get("vault-1").is_none());
        let (hits, misses) = cache.stats();
        assert_eq!(hits, 0);
        assert_eq!(misses, 1);
    }

    #[test]
    fn cache_hit_after_insert() {
        let cache = MetadataCache::new(MetadataCacheConfig::default());
        let m = make_metadata("vault-1");
        cache.insert(m.clone());
        let result = cache.get("vault-1").unwrap();
        assert_eq!(result.vault_id, "vault-1");
        let (hits, _) = cache.stats();
        assert_eq!(hits, 1);
    }

    #[test]
    fn cache_invalidation_removes_entry() {
        let cache = MetadataCache::new(MetadataCacheConfig::default());
        cache.insert(make_metadata("vault-2"));
        cache.invalidate("vault-2");
        assert!(cache.get("vault-2").is_none());
    }

    #[test]
    fn cache_evicts_lru_when_full() {
        let config = MetadataCacheConfig {
            ttl: Duration::from_secs(300),
            max_entries: 2,
        };
        let cache = MetadataCache::new(config);
        cache.insert(make_metadata("v1"));
        cache.insert(make_metadata("v2"));
        // Access v1 to make v2 the LRU.
        let _ = cache.get("v1");
        // Inserting a third entry should evict v2.
        cache.insert(make_metadata("v3"));
        assert!(cache.get("v2").is_none(), "v2 should have been evicted");
        assert!(cache.get("v1").is_some());
        assert!(cache.get("v3").is_some());
    }

    #[test]
    fn cache_clear_removes_all_entries() {
        let cache = MetadataCache::new(MetadataCacheConfig::default());
        cache.insert(make_metadata("v1"));
        cache.insert(make_metadata("v2"));
        cache.clear();
        assert!(cache.is_empty());
    }

    #[test]
    fn cache_expired_entry_treated_as_miss() {
        let config = MetadataCacheConfig {
            ttl: Duration::from_nanos(1), // expires immediately
            max_entries: 100,
        };
        let cache = MetadataCache::new(config);
        cache.insert(make_metadata("v-exp"));
        // The entry's TTL is 1 ns; by the time we call get() it is expired.
        std::thread::sleep(Duration::from_millis(1));
        assert!(cache.get("v-exp").is_none());
    }

    // ── LazyMetadataLoader integration tests ────────────────────────────────

    fn make_db() -> Arc<Db> {
        let db = Arc::new(Db::open(":memory:").unwrap());
        db.migrate().unwrap();
        db
    }

    #[test]
    fn loader_returns_none_for_missing_vault() {
        let loader = LazyMetadataLoader::new(make_db());
        assert!(loader.get("no-such-vault").is_none());
    }

    #[test]
    fn loader_set_then_get_roundtrip() {
        let loader = LazyMetadataLoader::new(make_db());
        let m = make_metadata("vault-rt");
        loader.set(m.clone()).unwrap();
        let loaded = loader.get("vault-rt").unwrap();
        assert_eq!(loaded.vault_id, "vault-rt");
        assert_eq!(loaded.fields["note"], "test");
    }

    #[test]
    fn loader_caches_after_first_db_hit() {
        let loader = LazyMetadataLoader::new(make_db());
        loader.set(make_metadata("vault-c")).unwrap();

        // First get populates the cache (cache was invalidated on set).
        let _ = loader.get("vault-c");
        // Second get should be served from cache.
        let _ = loader.get("vault-c");

        let (hits, _) = loader.cache().stats();
        assert!(hits >= 1, "at least one cache hit expected");
    }

    #[test]
    fn loader_delete_removes_from_db_and_cache() {
        let loader = LazyMetadataLoader::new(make_db());
        loader.set(make_metadata("vault-del")).unwrap();
        // Populate the cache.
        let _ = loader.get("vault-del");
        loader.delete("vault-del").unwrap();
        assert!(loader.get("vault-del").is_none());
    }

    #[test]
    fn loader_set_overwrites_previous_metadata() {
        let loader = LazyMetadataLoader::new(make_db());
        loader.set(make_metadata("vault-ow")).unwrap();

        let updated = VaultMetadata {
            vault_id: "vault-ow".to_string(),
            fields: serde_json::json!({"note": "updated"}),
            updated_at: chrono::Utc::now(),
        };
        loader.set(updated).unwrap();

        let loaded = loader.get("vault-ow").unwrap();
        assert_eq!(loaded.fields["note"], "updated");
    }
}
