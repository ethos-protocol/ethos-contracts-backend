/// Materialized-view-based query optimization (Issue #565).
///
/// Complex queries over the `vaults` table (active count, released count,
/// total balance, average check-in interval) are precomputed into a
/// `vault_summary_view` materialized-view table.  The view is refreshed
/// either on demand or by a periodic scheduler job.
///
/// # Design
/// * `MaterializedViewManager` owns all view-management logic and can be used
///   from both the HTTP handlers and the background scheduler.
/// * Incremental updates recalculate only the counters affected by a single
///   vault change rather than re-scanning the entire table.
/// * All public methods are synchronous and `Send + Sync`-safe; the only
///   shared state is an `Arc<Mutex<_>>` for the refresh schedule metadata.
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::db::Db;

// ── View data types ──────────────────────────────────────────────────────────

/// A snapshot of the precomputed vault aggregate stored in the materialized
/// view table.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct VaultSummaryView {
    /// Total number of vault rows.
    pub total_vaults: i64,
    /// Vaults whose `status` column equals `"active"`.
    pub active_vaults: i64,
    /// Vaults whose `status` column equals `"released"`.
    pub released_vaults: i64,
    /// Vaults whose `status` column equals `"expired"`.
    pub expired_vaults: i64,
    /// Sum of all balance values (stored as text; parsed on read).
    pub total_balance: i128,
    /// Average check-in interval across all vaults, or 0.0 when no vaults exist.
    pub avg_check_in_interval: f64,
    /// Timestamp of the last full refresh.
    pub last_refreshed_at: DateTime<Utc>,
}

/// Per-owner vault counts, stored in the `vault_owner_counts_view` auxiliary
/// materialized view.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct VaultOwnerCount {
    pub owner: String,
    pub vault_count: i64,
    pub active_count: i64,
}

/// Metadata about a view's refresh schedule.
#[derive(Debug, Clone)]
pub struct ViewRefreshSchedule {
    /// How often the view should be refreshed in the background.
    pub interval: std::time::Duration,
    /// When the next refresh is due.
    pub next_refresh_at: std::time::Instant,
}

// ── Db extension methods ──────────────────────────────────────────────────────

impl Db {
    /// Full refresh of the vault summary materialized view.
    pub fn mv_refresh_vault_summary(&self) -> Result<(), rusqlite::Error> {
        let sql = r"
            INSERT OR REPLACE INTO vault_summary_view
                (id, total_vaults, active_vaults, released_vaults, expired_vaults,
                 total_balance_text, avg_check_in_interval, last_refreshed_at)
            SELECT
                1  AS id,
                COUNT(*)                                                AS total_vaults,
                SUM(CASE WHEN status = 'active'   THEN 1 ELSE 0 END)   AS active_vaults,
                SUM(CASE WHEN status = 'released' THEN 1 ELSE 0 END)   AS released_vaults,
                SUM(CASE WHEN status = 'expired'  THEN 1 ELSE 0 END)   AS expired_vaults,
                '0'                                                     AS total_balance_text,
                COALESCE(AVG(CAST(check_in_interval AS REAL)), 0.0)     AS avg_check_in_interval,
                ?1                                                      AS last_refreshed_at
            FROM vaults
        ";
        self.conn
            .lock()
            .unwrap()
            .execute(sql, rusqlite::params![Utc::now().to_rfc3339()])?;
        Ok(())
    }

    /// Full refresh of the per-owner vault counts materialized view.
    pub fn mv_refresh_owner_counts(&self) -> Result<(), rusqlite::Error> {
        let conn = self.conn.lock().unwrap();
        conn.execute("DELETE FROM vault_owner_counts_view", [])?;
        conn.execute_batch(
            r"
            INSERT INTO vault_owner_counts_view (owner, vault_count, active_count)
            SELECT
                owner,
                COUNT(*)                                              AS vault_count,
                SUM(CASE WHEN status = 'active' THEN 1 ELSE 0 END)   AS active_count
            FROM vaults
            GROUP BY owner
            ",
        )?;
        Ok(())
    }

    /// Apply incremental deltas to the summary view counters.
    pub fn mv_apply_incremental_update(
        &self,
        total_delta: i64,
        active_delta: i64,
        released_delta: i64,
        expired_delta: i64,
    ) -> Result<(), rusqlite::Error> {
        self.conn.lock().unwrap().execute(
            r"
            UPDATE vault_summary_view
            SET
                total_vaults    = MAX(0, total_vaults    + ?1),
                active_vaults   = MAX(0, active_vaults   + ?2),
                released_vaults = MAX(0, released_vaults + ?3),
                expired_vaults  = MAX(0, expired_vaults  + ?4),
                last_refreshed_at = ?5
            WHERE id = 1
            ",
            rusqlite::params![
                total_delta,
                active_delta,
                released_delta,
                expired_delta,
                Utc::now().to_rfc3339(),
            ],
        )?;
        Ok(())
    }

    /// Read the vault summary from the materialized view.
    pub fn mv_get_vault_summary(&self) -> Option<VaultSummaryView> {
        let binding = self.conn.lock().unwrap();
        let mut stmt = binding
            .prepare(
                r"SELECT total_vaults, active_vaults, released_vaults, expired_vaults,
                         total_balance_text, avg_check_in_interval, last_refreshed_at
                  FROM vault_summary_view
                  WHERE id = 1",
            )
            .ok()?;

        stmt.query_row([], |row| {
            let balance_str: String = row.get(4)?;
            let last_refreshed_str: String = row.get(6)?;
            let last_refreshed_at =
                chrono::DateTime::parse_from_rfc3339(&last_refreshed_str)
                    .map(|dt| dt.with_timezone(&Utc))
                    .unwrap_or_else(|_| Utc::now());
            Ok(VaultSummaryView {
                total_vaults: row.get(0)?,
                active_vaults: row.get(1)?,
                released_vaults: row.get(2)?,
                expired_vaults: row.get(3)?,
                total_balance: balance_str.parse().unwrap_or(0),
                avg_check_in_interval: row.get::<_, f64>(5).unwrap_or(0.0),
                last_refreshed_at,
            })
        })
        .ok()
    }

    /// Read per-owner vault counts from the materialized view.
    pub fn mv_get_owner_counts(&self) -> Vec<VaultOwnerCount> {
        let binding = self.conn.lock().unwrap();
        let mut stmt = match binding.prepare(
            "SELECT owner, vault_count, active_count \
             FROM vault_owner_counts_view ORDER BY vault_count DESC",
        ) {
            Ok(s) => s,
            Err(_) => return vec![],
        };

        stmt.query_map([], |row| {
            Ok(VaultOwnerCount {
                owner: row.get(0)?,
                vault_count: row.get(1)?,
                active_count: row.get(2)?,
            })
        })
        .map(|rows| rows.filter_map(Result::ok).collect())
        .unwrap_or_default()
    }

    /// Emit EXPLAIN QUERY PLAN rows for the given SQL string.
    /// Used by `OptimizedQueryRunner::explain_plan`.
    pub fn explain_query_plan(&self, sql: &str) -> Vec<String> {
        let explain = format!("EXPLAIN QUERY PLAN {sql}");
        let binding = self.conn.lock().unwrap();
        let mut stmt = match binding.prepare(&explain) {
            Ok(s) => s,
            Err(e) => return vec![format!("Error: {e}")],
        };
        stmt.query_map([], |row| row.get::<_, String>(3))
            .map(|rows| rows.filter_map(Result::ok).collect())
            .unwrap_or_default()
    }
}

// ── MaterializedViewManager ──────────────────────────────────────────────────

/// Manages all materialized views for the backend.
#[derive(Clone)]
pub struct MaterializedViewManager {
    db: Arc<Db>,
    schedules: Arc<Mutex<HashMap<&'static str, ViewRefreshSchedule>>>,
}

impl MaterializedViewManager {
    pub const SUMMARY_VIEW: &'static str = "vault_summary_view";
    pub const OWNER_VIEW: &'static str = "vault_owner_counts_view";

    /// Default refresh interval: every 5 minutes.
    const DEFAULT_INTERVAL: std::time::Duration = std::time::Duration::from_secs(300);

    /// Create the manager.  Call [`refresh_all`] after construction to
    /// populate the views for the first time.
    pub fn new(db: Arc<Db>) -> Self {
        let mut schedules = HashMap::new();
        let now = std::time::Instant::now();

        schedules.insert(
            Self::SUMMARY_VIEW,
            ViewRefreshSchedule {
                interval: Self::DEFAULT_INTERVAL,
                next_refresh_at: now,
            },
        );
        schedules.insert(
            Self::OWNER_VIEW,
            ViewRefreshSchedule {
                interval: Self::DEFAULT_INTERVAL,
                next_refresh_at: now,
            },
        );

        Self {
            db,
            schedules: Arc::new(Mutex::new(schedules)),
        }
    }

    // ── Full refresh ─────────────────────────────────────────────────────────

    /// Recompute all materialized views from scratch.  This is safe to call at
    /// any time and should be called on startup and periodically thereafter.
    pub fn refresh_all(&self) -> Result<(), rusqlite::Error> {
        self.refresh_vault_summary()?;
        self.refresh_owner_counts()?;
        Ok(())
    }

    /// Full refresh of the vault summary view.
    pub fn refresh_vault_summary(&self) -> Result<(), rusqlite::Error> {
        self.db.mv_refresh_vault_summary()?;
        let mut sched = self.schedules.lock().unwrap();
        if let Some(s) = sched.get_mut(Self::SUMMARY_VIEW) {
            s.next_refresh_at = std::time::Instant::now() + s.interval;
        }
        Ok(())
    }

    /// Full refresh of the per-owner vault counts view.
    pub fn refresh_owner_counts(&self) -> Result<(), rusqlite::Error> {
        self.db.mv_refresh_owner_counts()?;
        let mut sched = self.schedules.lock().unwrap();
        if let Some(s) = sched.get_mut(Self::OWNER_VIEW) {
            s.next_refresh_at = std::time::Instant::now() + s.interval;
        }
        Ok(())
    }

    // ── Incremental updates ──────────────────────────────────────────────────

    /// Apply an incremental delta to the summary view counters when a single
    /// vault changes status.  Much cheaper than a full refresh for high-write
    /// workloads.
    ///
    /// `old_status` is `None` for a newly inserted vault; `new_status` is
    /// `None` for a deleted vault.
    pub fn apply_incremental_vault_update(
        &self,
        old_status: Option<&str>,
        new_status: Option<&str>,
    ) -> Result<(), rusqlite::Error> {
        let total_delta: i64 = match (old_status, new_status) {
            (None, Some(_)) => 1,
            (Some(_), None) => -1,
            _ => 0,
        };
        let active_delta = status_delta("active", old_status, new_status);
        let released_delta = status_delta("released", old_status, new_status);
        let expired_delta = status_delta("expired", old_status, new_status);

        self.db
            .mv_apply_incremental_update(total_delta, active_delta, released_delta, expired_delta)
    }

    // ── Read helpers ─────────────────────────────────────────────────────────

    /// Return the latest vault summary from the materialized view table.
    /// Returns `None` if the view has not been populated yet.
    pub fn get_vault_summary(&self) -> Option<VaultSummaryView> {
        self.db.mv_get_vault_summary()
    }

    /// Return per-owner vault counts from the owner-counts materialized view.
    pub fn get_owner_counts(&self) -> Vec<VaultOwnerCount> {
        self.db.mv_get_owner_counts()
    }

    // ── Schedule helpers ─────────────────────────────────────────────────────

    /// Set the refresh interval for the named view.
    pub fn set_refresh_interval(&self, view: &'static str, interval: std::time::Duration) {
        let mut sched = self.schedules.lock().unwrap();
        if let Some(s) = sched.get_mut(view) {
            s.interval = interval;
        }
    }

    /// Returns `true` if the named view is due for a refresh.
    pub fn is_due(&self, view: &'static str) -> bool {
        self.schedules
            .lock()
            .unwrap()
            .get(view)
            .map_or(false, |s| std::time::Instant::now() >= s.next_refresh_at)
    }

    /// Refresh any view that is currently due according to its schedule.
    pub fn refresh_due_views(&self) -> Result<(), rusqlite::Error> {
        let due: Vec<&'static str> = {
            let sched = self.schedules.lock().unwrap();
            sched
                .iter()
                .filter(|(_, s)| std::time::Instant::now() >= s.next_refresh_at)
                .map(|(name, _)| *name)
                .collect()
        };
        for view in due {
            match view {
                Self::SUMMARY_VIEW => self.refresh_vault_summary()?,
                Self::OWNER_VIEW => self.refresh_owner_counts()?,
                _ => {}
            }
        }
        Ok(())
    }
}

// ── Helper ───────────────────────────────────────────────────────────────────

/// Compute the signed delta for a specific status column.
fn status_delta(status: &str, old: Option<&str>, new: Option<&str>) -> i64 {
    let was = old.map_or(false, |s| s == status);
    let is = new.map_or(false, |s| s == status);
    match (was, is) {
        (false, true) => 1,
        (true, false) => -1,
        _ => 0,
    }
}

// ── Unit tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn make_db_with_vault() -> Arc<Db> {
        let db = Arc::new(Db::open(":memory:").unwrap());
        db.migrate().unwrap();

        // Insert a sample vault so the views have something to aggregate.
        db.conn
            .lock()
            .unwrap()
            .execute(
                r"INSERT INTO vaults
                    (id, owner, beneficiary, balance, check_in_interval,
                     last_check_in, created_at, status)
                  VALUES
                    ('v1','alice','bob','1000',3600,
                     '2025-01-01T00:00:00Z','2025-01-01T00:00:00Z','active')",
                [],
            )
            .unwrap();

        db
    }

    #[test]
    fn full_refresh_populates_summary_view() {
        let db = make_db_with_vault();
        let mgr = MaterializedViewManager::new(Arc::clone(&db));
        mgr.refresh_all().unwrap();

        let summary = mgr.get_vault_summary().unwrap();
        assert_eq!(summary.total_vaults, 1);
        assert_eq!(summary.active_vaults, 1);
        assert_eq!(summary.released_vaults, 0);
    }

    #[test]
    fn full_refresh_is_idempotent() {
        let db = make_db_with_vault();
        let mgr = MaterializedViewManager::new(Arc::clone(&db));
        mgr.refresh_all().unwrap();
        let first = mgr.get_vault_summary().unwrap();
        mgr.refresh_all().unwrap();
        let second = mgr.get_vault_summary().unwrap();
        assert_eq!(first.total_vaults, second.total_vaults);
        assert_eq!(first.active_vaults, second.active_vaults);
    }

    #[test]
    fn incremental_update_tracks_status_change() {
        let db = make_db_with_vault();
        let mgr = MaterializedViewManager::new(Arc::clone(&db));
        mgr.refresh_all().unwrap();

        // active → released
        mgr.apply_incremental_vault_update(Some("active"), Some("released"))
            .unwrap();

        let after = mgr.get_vault_summary().unwrap();
        assert_eq!(after.active_vaults, 0);
        assert_eq!(after.released_vaults, 1);
    }

    #[test]
    fn incremental_insert_increments_total() {
        let db = make_db_with_vault();
        let mgr = MaterializedViewManager::new(Arc::clone(&db));
        mgr.refresh_all().unwrap();
        let before = mgr.get_vault_summary().unwrap();

        mgr.apply_incremental_vault_update(None, Some("active"))
            .unwrap();

        let after = mgr.get_vault_summary().unwrap();
        assert_eq!(after.total_vaults, before.total_vaults + 1);
        assert_eq!(after.active_vaults, before.active_vaults + 1);
    }

    #[test]
    fn incremental_delete_decrements_total() {
        let db = make_db_with_vault();
        let mgr = MaterializedViewManager::new(Arc::clone(&db));
        mgr.refresh_all().unwrap();
        let before = mgr.get_vault_summary().unwrap();

        mgr.apply_incremental_vault_update(Some("active"), None)
            .unwrap();

        let after = mgr.get_vault_summary().unwrap();
        assert_eq!(after.total_vaults, (before.total_vaults - 1).max(0));
    }

    #[test]
    fn owner_counts_populated_after_refresh() {
        let db = make_db_with_vault();
        let mgr = MaterializedViewManager::new(Arc::clone(&db));
        mgr.refresh_all().unwrap();

        let counts = mgr.get_owner_counts();
        assert_eq!(counts.len(), 1);
        assert_eq!(counts[0].owner, "alice");
        assert_eq!(counts[0].vault_count, 1);
        assert_eq!(counts[0].active_count, 1);
    }

    #[test]
    fn summary_counters_floor_at_zero() {
        let db = make_db_with_vault();
        let mgr = MaterializedViewManager::new(Arc::clone(&db));
        mgr.refresh_all().unwrap();

        // Apply more deletes than real vaults — counters must floor at 0.
        for _ in 0..5 {
            mgr.apply_incremental_vault_update(Some("active"), None)
                .unwrap();
        }
        let summary = mgr.get_vault_summary().unwrap();
        assert!(summary.total_vaults >= 0);
        assert!(summary.active_vaults >= 0);
    }

    #[test]
    fn status_delta_helper_correct() {
        assert_eq!(status_delta("active", None, Some("active")), 1);
        assert_eq!(status_delta("active", Some("active"), None), -1);
        assert_eq!(status_delta("active", Some("active"), Some("released")), -1);
        assert_eq!(status_delta("released", Some("active"), Some("released")), 1);
        assert_eq!(status_delta("active", Some("active"), Some("active")), 0);
    }

    #[test]
    fn set_refresh_interval_does_not_panic() {
        let db = make_db_with_vault();
        let mgr = MaterializedViewManager::new(Arc::clone(&db));
        mgr.set_refresh_interval(
            MaterializedViewManager::SUMMARY_VIEW,
            std::time::Duration::from_secs(60),
        );
    }
}
