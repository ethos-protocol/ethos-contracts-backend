/// Query plan optimization for common vault queries (Issue #566).
///
/// This module provides:
/// * A [`QueryPlanner`] that selects the best execution strategy for a given
///   [`SearchQuery`] based on available indexes and query shape.
/// * A [`QueryPlanCache`] that caches the selected plan keyed on a
///   deterministic hash of the query parameters so the planner is only invoked
///   once per distinct query shape.
/// * An [`OptimizedQueryRunner`] that combines both into a single entry point
///   and can emit EXPLAIN QUERY PLAN output for slow-query diagnostics.
///
/// # Design
/// SQLite's own query planner is capable, but the backend programmatically
/// builds filter predicates that can be ordered suboptimally.  This module
/// ensures high-selectivity predicates (owner, status) are applied before
/// low-selectivity ones (date ranges) and that composite indexes are
/// exploited correctly.
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use serde::{Deserialize, Serialize};

use crate::db::Db;
use crate::models::SearchQuery;

// ── Query plan types ─────────────────────────────────────────────────────────

/// The set of available execution strategies for a vault list query.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum PlanStrategy {
    /// Full sequential scan of the `vaults` table.  Used when no selective
    /// filter is present.
    FullScan,
    /// Index seek on `owner`.  Fastest when the caller supplies an owner
    /// filter.
    OwnerIndexSeek,
    /// Index seek on `status`.  Used when the caller supplies a status filter
    /// but no owner.
    StatusIndexSeek,
    /// Covering index on `(owner, status)`.  Used when both filters are
    /// present.
    OwnerStatusIndex,
    /// Index seek on `created_at` for date-range–only queries.
    DateRangeIndex,
}

/// A resolved query execution plan including the SQL template for the
/// `vaults` table.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QueryPlan {
    /// The chosen strategy.
    pub strategy: PlanStrategy,
    /// The SQL SELECT statement with `?` placeholders.
    pub sql: String,
    /// Estimated selectivity score (lower = fewer expected rows = better).
    pub estimated_selectivity: f64,
    /// When this plan was compiled.
    pub compiled_at: chrono::DateTime<chrono::Utc>,
}

// ── QueryPlanner ─────────────────────────────────────────────────────────────

/// Selects the optimal execution plan for a [`SearchQuery`].
pub struct QueryPlanner;

impl QueryPlanner {
    /// Analyse `query` and return the best [`QueryPlan`].
    pub fn plan(query: &SearchQuery) -> QueryPlan {
        let strategy = Self::choose_strategy(query);
        let sql = Self::build_sql(&strategy, query);
        let estimated_selectivity = Self::estimate_selectivity(query);

        QueryPlan {
            strategy,
            sql,
            estimated_selectivity,
            compiled_at: chrono::Utc::now(),
        }
    }

    fn choose_strategy(q: &SearchQuery) -> PlanStrategy {
        match (&q.owner, &q.status) {
            // Both high-selectivity filters present → composite index.
            (Some(_), Some(_)) => PlanStrategy::OwnerStatusIndex,
            // Owner only → owner index.
            (Some(_), None) => PlanStrategy::OwnerIndexSeek,
            // Status only → status index.
            (None, Some(_)) => PlanStrategy::StatusIndexSeek,
            // Date range only → date index.
            (None, None) if q.created_after.is_some() || q.created_before.is_some() => {
                PlanStrategy::DateRangeIndex
            }
            // Nothing selective → full scan.
            _ => PlanStrategy::FullScan,
        }
    }

    fn build_sql(strategy: &PlanStrategy, q: &SearchQuery) -> String {
        let base = "SELECT id, owner, beneficiary, balance, check_in_interval, \
                    last_check_in, created_at, status, ttl_remaining \
                    FROM vaults";

        // Build WHERE clause with high-selectivity predicates first.
        let mut predicates: Vec<&str> = Vec::new();

        if q.owner.is_some() {
            predicates.push("owner = ?");
        }
        if q.status.is_some() {
            predicates.push("status = ?");
        }
        if q.created_after.is_some() {
            predicates.push("created_at >= ?");
        }
        if q.created_before.is_some() {
            predicates.push("created_at <= ?");
        }

        // Index-hint comment so DBA tooling can identify the intended plan.
        let hint = match strategy {
            PlanStrategy::OwnerStatusIndex => " /* idx: vaults_owner_status */",
            PlanStrategy::OwnerIndexSeek => " /* idx: vaults_owner */",
            PlanStrategy::StatusIndexSeek => " /* idx: vaults_status */",
            PlanStrategy::DateRangeIndex => " /* idx: vaults_created_at */",
            PlanStrategy::FullScan => "",
        };

        let order = "ORDER BY created_at DESC";
        let page_clause = "LIMIT ? OFFSET ?";

        if predicates.is_empty() {
            format!("{base}{hint} {order} {page_clause}")
        } else {
            format!(
                "{base}{hint} WHERE {} {order} {page_clause}",
                predicates.join(" AND ")
            )
        }
    }

    fn estimate_selectivity(q: &SearchQuery) -> f64 {
        // Heuristic: each applied filter cuts the expected row count.
        let mut score = 1.0_f64;
        if q.owner.is_some() {
            score *= 0.1; // very selective
        }
        if q.status.is_some() {
            score *= 0.25;
        }
        if q.created_after.is_some() || q.created_before.is_some() {
            score *= 0.5;
        }
        score
    }
}

// ── QueryPlanCache ────────────────────────────────────────────────────────────

/// Cache that maps a deterministic query-shape fingerprint to a compiled
/// [`QueryPlan`].  Plans are valid until their TTL expires or the cache is
/// explicitly invalidated.
pub struct QueryPlanCache {
    entries: Mutex<HashMap<String, PlanCacheEntry>>,
    ttl: Duration,
    hits: std::sync::atomic::AtomicU64,
    misses: std::sync::atomic::AtomicU64,
}

struct PlanCacheEntry {
    plan: QueryPlan,
    inserted_at: Instant,
}

impl QueryPlanCache {
    /// Create a cache with the default TTL (10 minutes).
    pub fn new() -> Self {
        Self::with_ttl(Duration::from_secs(600))
    }

    /// Create a cache with a custom TTL.
    pub fn with_ttl(ttl: Duration) -> Self {
        Self {
            entries: Mutex::new(HashMap::new()),
            ttl,
            hits: std::sync::atomic::AtomicU64::new(0),
            misses: std::sync::atomic::AtomicU64::new(0),
        }
    }

    /// Return the cached plan for `key`, or `None` if absent/expired.
    pub fn get(&self, key: &str) -> Option<QueryPlan> {
        let mut guard = self.entries.lock().unwrap();
        if let Some(entry) = guard.get(key) {
            if entry.inserted_at.elapsed() < self.ttl {
                self.hits
                    .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                return Some(entry.plan.clone());
            }
            guard.remove(key);
        }
        self.misses
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        None
    }

    /// Store a plan under `key`.
    pub fn insert(&self, key: String, plan: QueryPlan) {
        self.entries.lock().unwrap().insert(
            key,
            PlanCacheEntry {
                plan,
                inserted_at: Instant::now(),
            },
        );
    }

    /// Invalidate all cached plans (e.g. after a schema change).
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

    /// Number of entries currently in the cache.
    pub fn len(&self) -> usize {
        self.entries.lock().unwrap().len()
    }

    /// `true` when the cache has no entries.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl Default for QueryPlanCache {
    fn default() -> Self {
        Self::new()
    }
}

// ── OptimizedQueryRunner ─────────────────────────────────────────────────────

/// Combines the [`QueryPlanner`] and [`QueryPlanCache`] into a single entry
/// point.  Callers retrieve a [`QueryPlan`] for a [`SearchQuery`] without
/// managing cache state themselves.
#[derive(Clone)]
pub struct OptimizedQueryRunner {
    db: Arc<Db>,
    plan_cache: Arc<QueryPlanCache>,
}

impl OptimizedQueryRunner {
    pub fn new(db: Arc<Db>) -> Self {
        Self {
            db,
            plan_cache: Arc::new(QueryPlanCache::new()),
        }
    }

    /// Return the best [`QueryPlan`] for `query`, using the cache when
    /// possible.
    pub fn get_plan(&self, query: &SearchQuery) -> QueryPlan {
        let key = fingerprint(query);
        if let Some(plan) = self.plan_cache.get(&key) {
            return plan;
        }
        let plan = QueryPlanner::plan(query);
        self.plan_cache.insert(key, plan.clone());
        plan
    }

    /// Emit SQLite EXPLAIN QUERY PLAN output for the given query.
    /// Useful for slow-query diagnostics.
    pub fn explain_plan(&self, query: &SearchQuery) -> Vec<String> {
        let plan = self.get_plan(query);
        self.db.explain_query_plan(&plan.sql)
    }

    /// Access the underlying plan cache (for monitoring / metrics).
    pub fn plan_cache(&self) -> &QueryPlanCache {
        &self.plan_cache
    }
}

// ── Fingerprint helper ────────────────────────────────────────────────────────

/// Produce a deterministic string key from the shape of a [`SearchQuery`].
/// Only the *presence* of each filter is encoded, not its value, so that
/// queries with the same predicate set share the same plan.
pub fn fingerprint(q: &SearchQuery) -> String {
    format!(
        "owner={}&status={}&after={}&before={}",
        q.owner.is_some() as u8,
        q.status.is_some() as u8,
        q.created_after.is_some() as u8,
        q.created_before.is_some() as u8,
    )
}

// ── Unit tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::VaultStatus;

    fn empty_query() -> SearchQuery {
        SearchQuery {
            owner: None,
            beneficiary: None,
            status: None,
            created_after: None,
            created_before: None,
            page: None,
            limit: None,
        }
    }

    // ── QueryPlanner ────────────────────────────────────────────────────────

    #[test]
    fn planner_full_scan_for_empty_query() {
        let plan = QueryPlanner::plan(&empty_query());
        assert_eq!(plan.strategy, PlanStrategy::FullScan);
    }

    #[test]
    fn planner_owner_index_when_owner_only() {
        let mut q = empty_query();
        q.owner = Some("alice".to_string());
        let plan = QueryPlanner::plan(&q);
        assert_eq!(plan.strategy, PlanStrategy::OwnerIndexSeek);
    }

    #[test]
    fn planner_status_index_when_status_only() {
        let mut q = empty_query();
        q.status = Some(VaultStatus::Active);
        let plan = QueryPlanner::plan(&q);
        assert_eq!(plan.strategy, PlanStrategy::StatusIndexSeek);
    }

    #[test]
    fn planner_composite_index_when_owner_and_status() {
        let mut q = empty_query();
        q.owner = Some("alice".to_string());
        q.status = Some(VaultStatus::Active);
        let plan = QueryPlanner::plan(&q);
        assert_eq!(plan.strategy, PlanStrategy::OwnerStatusIndex);
    }

    #[test]
    fn planner_date_range_index_for_date_only_query() {
        let mut q = empty_query();
        q.created_after = Some(chrono::Utc::now());
        let plan = QueryPlanner::plan(&q);
        assert_eq!(plan.strategy, PlanStrategy::DateRangeIndex);
    }

    #[test]
    fn planner_sql_owner_before_status_predicate() {
        let mut q = empty_query();
        q.owner = Some("alice".to_string());
        q.status = Some(VaultStatus::Active);
        let plan = QueryPlanner::plan(&q);
        let owner_pos = plan.sql.find("owner = ?").unwrap();
        let status_pos = plan.sql.find("status = ?").unwrap();
        assert!(
            owner_pos < status_pos,
            "owner predicate must appear before status for composite-index exploitation"
        );
    }

    #[test]
    fn planner_selectivity_owner_lower_than_full_scan() {
        let mut q_owner = empty_query();
        q_owner.owner = Some("alice".to_string());
        let full = QueryPlanner::plan(&empty_query());
        let owner = QueryPlanner::plan(&q_owner);
        assert!(owner.estimated_selectivity < full.estimated_selectivity);
    }

    // ── QueryPlanCache ───────────────────────────────────────────────────────

    #[test]
    fn plan_cache_miss_on_empty() {
        let cache = QueryPlanCache::new();
        assert!(cache.get("no-such-key").is_none());
        let (_, misses) = cache.stats();
        assert_eq!(misses, 1);
    }

    #[test]
    fn plan_cache_hit_after_insert() {
        let cache = QueryPlanCache::new();
        let plan = QueryPlanner::plan(&empty_query());
        cache.insert("key1".to_string(), plan.clone());
        let retrieved = cache.get("key1").unwrap();
        assert_eq!(retrieved.strategy, plan.strategy);
        let (hits, _) = cache.stats();
        assert_eq!(hits, 1);
    }

    #[test]
    fn plan_cache_clear_removes_all_entries() {
        let cache = QueryPlanCache::new();
        cache.insert("k1".to_string(), QueryPlanner::plan(&empty_query()));
        cache.insert("k2".to_string(), QueryPlanner::plan(&empty_query()));
        cache.clear();
        assert!(cache.is_empty());
    }

    #[test]
    fn plan_cache_expired_entry_returns_none() {
        let cache = QueryPlanCache::with_ttl(Duration::from_nanos(1));
        cache.insert("k".to_string(), QueryPlanner::plan(&empty_query()));
        std::thread::sleep(Duration::from_millis(1));
        assert!(cache.get("k").is_none());
    }

    // ── OptimizedQueryRunner ─────────────────────────────────────────────────

    #[test]
    fn runner_caches_plan_on_second_call() {
        let db = Arc::new(Db::open(":memory:").unwrap());
        db.migrate().unwrap();
        let runner = OptimizedQueryRunner::new(db);
        let q = empty_query();
        let _ = runner.get_plan(&q);
        let _ = runner.get_plan(&q);
        let (hits, _) = runner.plan_cache().stats();
        assert_eq!(hits, 1);
    }

    #[test]
    fn runner_different_query_shapes_produce_different_plans() {
        let db = Arc::new(Db::open(":memory:").unwrap());
        db.migrate().unwrap();
        let runner = OptimizedQueryRunner::new(db);

        let mut q1 = empty_query();
        q1.owner = Some("alice".to_string());
        let mut q2 = empty_query();
        q2.status = Some(VaultStatus::Active);

        let p1 = runner.get_plan(&q1);
        let p2 = runner.get_plan(&q2);
        assert_ne!(p1.strategy, p2.strategy);
    }

    // ── fingerprint ─────────────────────────────────────────────────────────

    #[test]
    fn fingerprint_same_shape_produces_same_key() {
        let mut q1 = empty_query();
        q1.owner = Some("alice".to_string());
        let mut q2 = empty_query();
        q2.owner = Some("bob".to_string()); // different value, same shape
        assert_eq!(fingerprint(&q1), fingerprint(&q2));
    }

    #[test]
    fn fingerprint_different_shapes_produce_different_keys() {
        let mut q1 = empty_query();
        q1.owner = Some("alice".to_string());
        let mut q2 = empty_query();
        q2.status = Some(VaultStatus::Active);
        assert_ne!(fingerprint(&q1), fingerprint(&q2));
    }
}
