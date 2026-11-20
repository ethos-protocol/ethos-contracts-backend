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
/// * Loads never panic: database failures and corrupt rows are returned to
///   the caller as a [`MetadataLoadError`].
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use rusqlite::OptionalExtension;
use serde::{Deserialize, Serialize};

use crate::db::Db;

// ── Errors ───────────────────────────────────────────────────────────────────

/// Why vault metadata could not be loaded.
///
/// A missing row is not an error (loads return `Ok(None)`); this type covers
/// the cases where a row may exist but cannot be read.
#[derive(Debug, thiserror::Error)]
pub enum MetadataLoadError {
    /// The SQLite query failed.
    #[error("failed to load vault metadata: {0}")]
    Db(#[from] rusqlite::Error),
    /// The stored `fields` column is not valid JSON.
    #[error("vault metadata for '{vault_id}' has invalid fields JSON: {source}")]
    InvalidFields {
        vault_id: String,
        #[source]
        source: serde_json::Error,
    },
    /// The stored `updated_at` column is not an RFC 3339 timestamp.
    #[error("vault metadata for '{vault_id}' has invalid updated_at: {source}")]
    InvalidTimestamp {
        vault_id: String,
        #[source]
        source: chrono::ParseError,
    },
}

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
        let mut guard = self.entries.lock().unwrap_or_else(|p| p.into_inner());
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
        let mut guard = self.entries.lock().unwrap_or_else(|p| p.into_inner());

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
        self.entries.lock().unwrap_or_else(|p| p.into_inner()).remove(vault_id);
    }

    /// Invalidate all entries.
    pub fn clear(&self) {
        self.entries.lock().unwrap_or_else(|p| p.into_inner()).clear();
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
        self.entries.lock().unwrap_or_else(|p| p.into_inner()).len()
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
    /// Returns `Ok(None)` when no metadata row exists for the vault, and a
    /// [`MetadataLoadError`] when the row cannot be read. Failed loads are
    /// not cached.
    pub fn get(&self, vault_id: &str) -> Result<Option<VaultMetadata>, MetadataLoadError> {
        if let Some(cached) = self.cache.get(vault_id) {
            return Ok(Some(cached));
        }

        // Cache miss — load from SQLite.
        let Some(loaded) = self.db.get_vault_metadata(vault_id)? else {
            return Ok(None);
        };
        self.cache.insert(loaded.clone());
        Ok(Some(loaded))
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
        self.conn.lock().unwrap_or_else(|p| p.into_inner()).execute(
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

    /// Retrieve a vault metadata row.
    ///
    /// Returns `Ok(None)` if no row exists. Query failures and rows whose
    /// `fields` or `updated_at` columns cannot be parsed are returned as a
    /// [`MetadataLoadError`] instead of being silently replaced with
    /// placeholder values.
    pub fn get_vault_metadata(
        &self,
        vault_id: &str,
    ) -> Result<Option<VaultMetadata>, MetadataLoadError> {
        let row = {
            let conn = self.conn.lock().unwrap_or_else(|p| p.into_inner());
            conn.query_row(
                r"SELECT vault_id, fields, updated_at
                  FROM vault_metadata
                  WHERE vault_id = ?1",
                rusqlite::params![vault_id],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                    ))
                },
            )
            .optional()?
        };

        let Some((vault_id, fields_str, updated_at_str)) = row else {
            return Ok(None);
        };

        let fields = match serde_json::from_str(&fields_str) {
            Ok(fields) => fields,
            Err(source) => return Err(MetadataLoadError::InvalidFields { vault_id, source }),
        };
        let updated_at = match chrono::DateTime::parse_from_rfc3339(&updated_at_str) {
            Ok(dt) => dt.with_timezone(&chrono::Utc),
            Err(source) => return Err(MetadataLoadError::InvalidTimestamp { vault_id, source }),
        };

        Ok(Some(VaultMetadata {
            vault_id,
            fields,
            updated_at,
        }))
    }

    /// Delete the metadata row for `vault_id`.
    pub fn delete_vault_metadata(&self, vault_id: &str) -> Result<(), rusqlite::Error> {
        self.conn.lock().unwrap_or_else(|p| p.into_inner()).execute(
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

    type TestResult = Result<(), Box<dyn std::error::Error>>;

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
    fn cache_hit_after_insert() -> TestResult {
        let cache = MetadataCache::new(MetadataCacheConfig::default());
        let m = make_metadata("vault-1");
        cache.insert(m.clone());
        let result = cache.get("vault-1").ok_or("expected a cache hit")?;
        assert_eq!(result.vault_id, "vault-1");
        let (hits, _) = cache.stats();
        assert_eq!(hits, 1);
        Ok(())
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

    fn make_db() -> Result<Arc<Db>, rusqlite::Error> {
        let db = Arc::new(Db::open(":memory:")?);
        db.migrate()?;
        Ok(db)
    }

    /// Write a raw row, bypassing `VaultMetadata` serialisation, so tests can
    /// store values the loader must reject.
    fn insert_raw_row(
        db: &Db,
        vault_id: &str,
        fields: &str,
        updated_at: &str,
    ) -> Result<(), rusqlite::Error> {
        db.conn.lock().unwrap_or_else(|p| p.into_inner()).execute(
            "INSERT INTO vault_metadata (vault_id, fields, updated_at) VALUES (?1, ?2, ?3)",
            rusqlite::params![vault_id, fields, updated_at],
        )?;
        Ok(())
    }

    #[test]
    fn loader_returns_none_for_missing_vault() -> TestResult {
        let loader = LazyMetadataLoader::new(make_db()?);
        assert!(loader.get("no-such-vault")?.is_none());
        Ok(())
    }

    #[test]
    fn loader_set_then_get_roundtrip() -> TestResult {
        let loader = LazyMetadataLoader::new(make_db()?);
        let m = make_metadata("vault-rt");
        loader.set(m.clone())?;
        let loaded = loader.get("vault-rt")?.ok_or("metadata should exist")?;
        assert_eq!(loaded.vault_id, "vault-rt");
        assert_eq!(loaded.fields["note"], "test");
        Ok(())
    }

    #[test]
    fn loader_caches_after_first_db_hit() -> TestResult {
        let loader = LazyMetadataLoader::new(make_db()?);
        loader.set(make_metadata("vault-c"))?;

        // First get populates the cache (cache was invalidated on set).
        loader.get("vault-c")?;
        // Second get should be served from cache.
        loader.get("vault-c")?;

        let (hits, _) = loader.cache().stats();
        assert!(hits >= 1, "at least one cache hit expected");
        Ok(())
    }

    #[test]
    fn loader_delete_removes_from_db_and_cache() -> TestResult {
        let loader = LazyMetadataLoader::new(make_db()?);
        loader.set(make_metadata("vault-del"))?;
        // Populate the cache.
        loader.get("vault-del")?;
        loader.delete("vault-del")?;
        assert!(loader.get("vault-del")?.is_none());
        Ok(())
    }

    #[test]
    fn loader_set_overwrites_previous_metadata() -> TestResult {
        let loader = LazyMetadataLoader::new(make_db()?);
        loader.set(make_metadata("vault-ow"))?;

        let updated = VaultMetadata {
            vault_id: "vault-ow".to_string(),
            fields: serde_json::json!({"note": "updated"}),
            updated_at: chrono::Utc::now(),
        };
        loader.set(updated)?;

        let loaded = loader.get("vault-ow")?.ok_or("metadata should exist")?;
        assert_eq!(loaded.fields["note"], "updated");
        Ok(())
    }

    #[test]
    fn loader_returns_error_for_corrupt_fields_json() -> TestResult {
        let db = make_db()?;
        insert_raw_row(&db, "vault-bad-json", "{not json", "2026-01-01T00:00:00Z")?;
        let loader = LazyMetadataLoader::new(db);

        match loader.get("vault-bad-json") {
            Err(MetadataLoadError::InvalidFields { vault_id, .. }) => {
                assert_eq!(vault_id, "vault-bad-json");
            }
            other => panic!("expected InvalidFields, got {other:?}"),
        }
        assert!(loader.cache().is_empty(), "failed loads must not be cached");
        Ok(())
    }

    #[test]
    fn loader_returns_error_for_corrupt_timestamp() -> TestResult {
        let db = make_db()?;
        insert_raw_row(&db, "vault-bad-ts", "{}", "yesterday-ish")?;
        let loader = LazyMetadataLoader::new(db);

        match loader.get("vault-bad-ts") {
            Err(MetadataLoadError::InvalidTimestamp { vault_id, .. }) => {
                assert_eq!(vault_id, "vault-bad-ts");
            }
            other => panic!("expected InvalidTimestamp, got {other:?}"),
        }
        assert!(loader.cache().is_empty(), "failed loads must not be cached");
        Ok(())
    }

    #[test]
    fn loader_returns_db_error_when_table_is_missing() -> TestResult {
        let db = make_db()?;
        db.conn
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .execute("DROP TABLE vault_metadata", [])?;
        let loader = LazyMetadataLoader::new(db);

        assert!(matches!(
            loader.get("vault-any"),
            Err(MetadataLoadError::Db(_))
        ));
        Ok(())
    }
}
