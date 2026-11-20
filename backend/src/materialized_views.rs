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
    /// Number of vault rows whose balance could not be parsed and were
    /// therefore omitted from `total_balance` (Issue #852).
    #[serde(default)]
    pub unparseable_balance_rows: i64,
    /// Average check-in interval across all vaults, or 0.0 when no vaults exist.
    pub avg_check_in_interval: f64,
    /// Timestamp of the last full refresh.
    ///
    /// `None` when the stored refresh time could not be parsed; callers must
    /// treat such a summary as stale rather than fresh (Issue #853).
    pub last_refreshed_at: Option<DateTime<Utc>>,
}

impl VaultSummaryView {
    /// Whether the summary's stored refresh time could not be parsed and the
    /// summary should therefore be treated as stale (Issue #853).
    pub fn is_stale(&self) -> bool {
        self.last_refreshed_at.is_none()
    }
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

/// Maximum number of unparseable balance rows tolerated during a summary
/// refresh before the refresh is failed (Issue #852).
///
/// Overridable via the `MV_MAX_UNPARSEABLE_BALANCE_ROWS` environment variable.
const DEFAULT_MAX_UNPARSEABLE_BALANCE_ROWS: i64 = 0;

fn max_unparseable_balance_rows() -> i64 {
    std::env::var("MV_MAX_UNPARSEABLE_BALANCE_ROWS")
        .ok()
        .and_then(|v| v.parse::<i64>().ok())
        .unwrap_or(DEFAULT_MAX_UNPARSEABLE_BALANCE_ROWS)
}

// ── Db extension methods ──────────────────────────────────────────────────────

impl Db {
    /// Full refresh of the vault summary materialized view.
    ///
    /// Balances are parsed row-by-row so that corrupt values are counted and
    /// logged rather than silently coerced to zero.  If the number of
    /// unparseable rows exceeds the configured maximum the refresh fails
    /// (Issue #852).
    pub fn mv_refresh_vault_summary(&self) -> Result<(), rusqlite::Error> {
        let conn = self.conn.lock().unwrap_or_else(|p| p.into_inner());

        let (total_vaults, active_vaults, released_vaults, expired_vaults, avg_check_in_interval) = {
            let mut stmt = conn.prepare(
                r"SELECT
                    COUNT(*)                                                AS total_vaults,
                    SUM(CASE WHEN status = 'active'   THEN 1 ELSE 0 END)   AS active_vaults,
                    SUM(CASE WHEN status = 'released' THEN 1 ELSE 0 END)   AS released_vaults,
                    SUM(CASE WHEN status = 'expired'  THEN 1 ELSE 0 END)   AS expired_vaults,
                    COALESCE(AVG(CAST(check_in_interval AS REAL)), 0.0)     AS avg_check_in_interval
                  FROM vaults",
            )?;
            stmt.query_row([], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, i64>(3)?,
                    row.get::<_, f64>(4)?,
                ))
            })?
        };

        let mut total_balance: i128 = 0;
        let mut unparseable_balance_rows: i64 = 0;
        {
            let mut stmt = conn.prepare("SELECT id, balance FROM vaults")?;
            let mut rows = stmt.query([])?;
            while let Some(row) = rows.next()? {
                let id: i64 = row.get(0)?;
                let balance_str: String = row.get(1)?;
                match balance_str.trim().parse::<i128>() {
                    Ok(v) => total_balance += v,
                    Err(_) => {
                        unparseable_balance_rows += 1;
                        eprintln!(
                            "materialized_views: vault {id} has unparseable balance {balance_str:?}; omitting from total_balance"
                        );
                    }
                }
            }
        }

        let max_bad = max_unparseable_balance_rows();
        if unparseable_balance_rows > max_bad {
            eprintln!(
                "materialized_views: refresh aborted — {unparseable_balance_rows} unparseable balance rows exceed configured maximum {max_bad}"
            );
            return Err(rusqlite::Error::InvalidQuery);
        }

        // Capture the refresh time on the write path so the read path never
        // has to synthesize a timestamp (Issue #853).
        let refreshed_at = Utc::now().to_rfc3339();

        conn.execute(
            r"
            INSERT OR REPLACE INTO vault_summary_view
                (id, total_vaults, active_vaults, released_vaults, expired_vaults,
                 total_balance_text, avg_check_in_interval, last_refreshed_at)
            VALUES (1, ?1, ?2, ?3, ?4, ?5, ?6, ?7)
            ",
            rusqlite::params![
                total_vaults,
                active_vaults,
                released_vaults,
                expired_vaults,
                total_balance.to_string(),
                avg_check_in_interval,
                refreshed_at,
            ],
        )?;
        Ok(())
    }

    /// Full refresh of the per-owner vault counts materialized view.
    pub fn mv_refresh_owner_counts(&self) -> Result<(), rusqlite::Error> {
        let conn = self.conn.lock().unwrap_or_else(|p| p.into_inner());
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
        // Record the refresh time on the write path (Issue #853).
        let refreshed_at = Utc::now().to_rfc3339();
        self.conn.lock().unwrap_or_else(|p| p.into_inner()).execute(
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
                refreshed_at,
            ],
        )?;
        Ok(())
    }
}
