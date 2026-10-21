//! Read-connection pool for [`crate::db::Db`] (#696, #697).
//!
//! SQLite allows many concurrent readers alongside a single writer. `Db`
//! keeps one writer connection, behind a mutex, for every write. Read-only
//! queries check a connection out of this pool instead, so a slow scan no
//! longer blocks every other request.
//!
//! # Behaviour
//!
//! - **File databases** run in WAL mode, so readers do not block the writer
//!   and each reader sees the latest committed data when its query starts.
//! - **`:memory:` databases** get a uniquely named shared-cache URI, so every
//!   connection in the pool sees the same data. Shared-cache readers use
//!   `read_uncommitted` so they never fail with `SQLITE_LOCKED` while the
//!   writer holds a table lock. A reader can therefore observe rows from a
//!   statement that is still running. The writer's statements are
//!   single-row or autocommit, which keeps that window small.
//! - **Sizing**: `min` connections open at startup and the pool grows on
//!   demand up to `max`. Readers set `query_only`, so a write routed here
//!   fails with `SQLITE_READONLY` rather than changing data.
//! - **Overload**: when the pool is full and no connection frees up within
//!   `acquire_timeout`, or a new connection cannot be opened, the caller falls
//!   back to the writer connection. Requests get slower under load but do not
//!   fail.

use rusqlite::{Connection, OpenFlags};
use std::ops::Deref;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Condvar, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

static MEMORY_DB_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Map a configured database path to the target SQLite should open.
///
/// `:memory:` becomes a uniquely named shared-cache in-memory database so that
/// several connections can share it. Every other path is returned unchanged.
pub fn resolve_target(path: &str) -> String {
    if path == ":memory:" {
        let n = MEMORY_DB_COUNTER.fetch_add(1, Ordering::Relaxed);
        format!(
            "file:ethos-mem-{}-{n}?mode=memory&cache=shared",
            std::process::id()
        )
    } else {
        path.to_string()
    }
}

/// Open the single writer connection for `target` (a path from [`resolve_target`]).
pub fn open_writer(target: &str, busy_timeout: Duration) -> rusqlite::Result<Connection> {
    let flags = OpenFlags::SQLITE_OPEN_READ_WRITE
        | OpenFlags::SQLITE_OPEN_CREATE
        | OpenFlags::SQLITE_OPEN_URI
        | OpenFlags::SQLITE_OPEN_NO_MUTEX;
    let conn = Connection::open_with_flags(target, flags)?;
    conn.busy_timeout(busy_timeout)?;
    if !target.contains("mode=memory") {
        // WAL lets readers proceed while the writer commits.
        conn.pragma_update(None, "journal_mode", "WAL")?;
    }
    Ok(conn)
}

#[derive(Clone)]
struct ConnSource {
    target: String,
    busy_timeout: Duration,
    shared_memory: bool,
}

impl ConnSource {
    fn open_reader(&self) -> rusqlite::Result<Connection> {
        let flags = OpenFlags::SQLITE_OPEN_READ_ONLY
            | OpenFlags::SQLITE_OPEN_URI
            | OpenFlags::SQLITE_OPEN_NO_MUTEX;
        let conn = Connection::open_with_flags(&self.target, flags)?;
        conn.busy_timeout(self.busy_timeout)?;
        // The read-only open flag is not enforced for shared-cache in-memory
        // databases, so also reject writes on the connection itself. A stray
        // write routed here then fails instead of changing data.
        conn.pragma_update(None, "query_only", true)?;
        if self.shared_memory {
            conn.pragma_update(None, "read_uncommitted", true)?;
        }
        Ok(conn)
    }
}

#[derive(Default)]
struct PoolState {
    idle: Vec<Connection>,
    /// Connections opened so far (idle or checked out).
    open: usize,
}

/// A bounded pool of read-only SQLite connections.
pub struct ReadPool {
    source: ConnSource,
    max: usize,
    acquire_timeout: Duration,
    state: Mutex<PoolState>,
    available: Condvar,
}

impl ReadPool {
    /// Build a pool for `target`, opening `min` connections up front (capped at `max`).
    ///
    /// `max` is clamped to at least 1. For shared-cache in-memory targets it is
    /// capped at 1, because SQLite serializes shared-cache readers internally and
    /// extra connections only add contention (see the benchmark in the tests).
    /// Returns an error if an initial connection cannot be opened.
    pub fn new(
        target: &str,
        min: usize,
        max: usize,
        acquire_timeout: Duration,
        busy_timeout: Duration,
    ) -> rusqlite::Result<Self> {
        let shared_memory = target.contains("mode=memory");
        let max = if shared_memory {
            if max > 1 {
                tracing::info!(
                    requested_max = max,
                    "in-memory database: read connections capped at 1 (shared-cache readers serialize)"
                );
            }
            1
        } else {
            max.max(1)
        };
        let source = ConnSource {
            target: target.to_string(),
            busy_timeout,
            shared_memory,
        };
        let mut idle = Vec::new();
        for _ in 0..min.min(max) {
            idle.push(source.open_reader()?);
        }
        let open = idle.len();
        Ok(Self {
            source,
            max,
            acquire_timeout,
            state: Mutex::new(PoolState { idle, open }),
            available: Condvar::new(),
        })
    }

    /// Maximum number of read connections the pool will open.
    pub fn max_size(&self) -> usize {
        self.max
    }

    /// Number of read connections opened so far.
    pub fn open_connections(&self) -> usize {
        self.lock_state().open
    }

    /// Check out a read connection, or `None` if the pool is exhausted past
    /// `acquire_timeout` or a new connection could not be opened.
    pub fn acquire(&self) -> Option<Connection> {
        let deadline = Instant::now() + self.acquire_timeout;
        let mut state = self.lock_state();
        loop {
            if let Some(conn) = state.idle.pop() {
                return Some(conn);
            }
            if state.open < self.max {
                state.open += 1;
                drop(state);
                return match self.source.open_reader() {
                    Ok(conn) => Some(conn),
                    Err(err) => {
                        tracing::warn!(error = %err, "failed to open read connection; falling back to writer");
                        self.lock_state().open -= 1;
                        self.available.notify_one();
                        None
                    }
                };
            }
            let now = Instant::now();
            if now >= deadline {
                tracing::warn!(
                    max = self.max,
                    "read pool exhausted; falling back to writer connection"
                );
                return None;
            }
            let (guard, _) = self
                .available
                .wait_timeout(state, deadline - now)
                .unwrap_or_else(PoisonError::into_inner);
            state = guard;
        }
    }

    /// Return a connection obtained from [`ReadPool::acquire`].
    pub fn release(&self, conn: Connection) {
        self.lock_state().idle.push(conn);
        self.available.notify_one();
    }

    fn lock_state(&self) -> MutexGuard<'_, PoolState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// A connection checked out for a read. Derefs to [`Connection`].
///
/// Pooled connections return to the pool on drop. The writer fallback holds the
/// writer mutex for the guard's lifetime, the same as the old single-connection path.
pub enum ReadGuard<'a> {
    Pooled {
        pool: &'a ReadPool,
        conn: Option<Connection>,
    },
    Writer(MutexGuard<'a, Connection>),
}

impl<'a> ReadGuard<'a> {
    pub fn pooled(pool: &'a ReadPool, conn: Connection) -> Self {
        Self::Pooled {
            pool,
            conn: Some(conn),
        }
    }

    pub fn writer(guard: MutexGuard<'a, Connection>) -> Self {
        Self::Writer(guard)
    }
}

impl Deref for ReadGuard<'_> {
    type Target = Connection;

    fn deref(&self) -> &Connection {
        match self {
            Self::Pooled { conn, .. } => conn.as_ref().expect("read connection present until drop"),
            Self::Writer(guard) => guard,
        }
    }
}

impl Drop for ReadGuard<'_> {
    fn drop(&mut self) {
        if let Self::Pooled { pool, conn } = self {
            if let Some(conn) = conn.take() {
                pool.release(conn);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Barrier};

    const TIMEOUT: Duration = Duration::from_secs(5);

    fn temp_db_path(tag: &str) -> String {
        let n = MEMORY_DB_COUNTER.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir()
            .join(format!(
                "ethos-pool-{tag}-{}-{n}.sqlite",
                std::process::id()
            ))
            .to_string_lossy()
            .into_owned()
    }

    fn remove_db_files(path: &str) {
        for suffix in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{path}{suffix}"));
        }
    }

    fn seed(conn: &Connection, rows: usize) {
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS items (id INTEGER PRIMARY KEY, label TEXT NOT NULL)",
        )
        .unwrap();
        let tx = conn.unchecked_transaction().unwrap();
        for i in 0..rows {
            tx.execute(
                "INSERT INTO items (label) VALUES (?1)",
                [format!("row-{i}")],
            )
            .unwrap();
        }
        tx.commit().unwrap();
    }

    fn count(conn: &Connection) -> i64 {
        conn.query_row("SELECT COUNT(*) FROM items", [], |r| r.get(0))
            .unwrap()
    }

    #[test]
    fn each_memory_database_gets_its_own_shared_cache_name() {
        let a = resolve_target(":memory:");
        let b = resolve_target(":memory:");
        assert_ne!(a, b);
        assert!(a.contains("mode=memory") && a.contains("cache=shared"));
        assert_eq!(resolve_target("/data/ethos.db"), "/data/ethos.db");
    }

    #[test]
    fn memory_readers_see_the_writers_committed_data() {
        let target = resolve_target(":memory:");
        let writer = open_writer(&target, TIMEOUT).unwrap();
        seed(&writer, 2);
        let pool = ReadPool::new(&target, 2, 4, TIMEOUT, TIMEOUT).unwrap();
        let reader = pool.acquire().expect("reader");
        assert_eq!(count(&reader), 2);
    }

    #[test]
    fn pooled_readers_cannot_write() {
        let target = resolve_target(":memory:");
        let writer = open_writer(&target, TIMEOUT).unwrap();
        seed(&writer, 1);
        let pool = ReadPool::new(&target, 1, 1, TIMEOUT, TIMEOUT).unwrap();
        let reader = pool.acquire().expect("reader");
        let err = reader
            .execute("INSERT INTO items (label) VALUES ('x')", [])
            .unwrap_err();
        assert_eq!(err.sqlite_error_code(), Some(rusqlite::ErrorCode::ReadOnly));
        assert_eq!(count(&writer), 1, "rejected write must not change data");
    }

    #[test]
    fn file_databases_use_wal_and_readers_see_committed_writes() {
        let path = temp_db_path("wal");
        let writer = open_writer(&path, TIMEOUT).unwrap();
        let mode: String = writer
            .query_row("PRAGMA journal_mode", [], |r| r.get(0))
            .unwrap();
        assert_eq!(mode.to_lowercase(), "wal");

        seed(&writer, 2);
        let pool = ReadPool::new(&path, 1, 3, TIMEOUT, TIMEOUT).unwrap();
        let reader = pool.acquire().unwrap();
        assert_eq!(count(&reader), 2);
        pool.release(reader);

        writer
            .execute("INSERT INTO items (label) VALUES ('later')", [])
            .unwrap();
        let reader = pool.acquire().unwrap();
        assert_eq!(count(&reader), 3, "a read after a committed write sees it");
        drop(reader);
        drop(writer);
        remove_db_files(&path);
    }

    #[test]
    fn in_memory_pool_is_capped_at_one_read_connection() {
        let target = resolve_target(":memory:");
        let _writer = open_writer(&target, TIMEOUT).unwrap();
        let pool = ReadPool::new(&target, 4, 16, TIMEOUT, TIMEOUT).unwrap();
        assert_eq!(pool.max_size(), 1);
        assert_eq!(pool.open_connections(), 1);
    }

    #[test]
    fn file_pool_opens_min_up_front_and_grows_on_demand_to_max() {
        let path = temp_db_path("grow");
        let writer = open_writer(&path, TIMEOUT).unwrap();
        let pool = ReadPool::new(&path, 1, 3, TIMEOUT, TIMEOUT).unwrap();
        assert_eq!(pool.open_connections(), 1);

        let a = pool.acquire().unwrap();
        let b = pool.acquire().unwrap();
        let c = pool.acquire().unwrap();
        assert_eq!(pool.open_connections(), 3);
        drop((a, b, c));
        drop(writer);
        remove_db_files(&path);
    }

    #[test]
    fn exhausted_pool_returns_none_after_the_acquire_timeout() {
        let target = resolve_target(":memory:");
        let _writer = open_writer(&target, TIMEOUT).unwrap();
        let pool = ReadPool::new(&target, 0, 1, Duration::from_millis(50), TIMEOUT).unwrap();
        let held = pool.acquire().expect("first connection");
        let started = Instant::now();
        assert!(
            pool.acquire().is_none(),
            "pool is full, so acquire must give up"
        );
        assert!(started.elapsed() >= Duration::from_millis(50));

        pool.release(held);
        let again = pool.acquire().expect("released connection is reusable");
        assert_eq!(
            pool.open_connections(),
            1,
            "reuse must not open a new connection"
        );
        drop(again);
    }

    #[test]
    fn min_is_capped_at_max_and_max_is_at_least_one() {
        let path = temp_db_path("caps");
        let writer = open_writer(&path, TIMEOUT).unwrap();

        let pool = ReadPool::new(&path, 5, 2, TIMEOUT, TIMEOUT).unwrap();
        assert_eq!(pool.max_size(), 2);
        assert_eq!(pool.open_connections(), 2);

        let pool = ReadPool::new(&path, 0, 0, TIMEOUT, TIMEOUT).unwrap();
        assert_eq!(pool.max_size(), 1);
        assert_eq!(pool.open_connections(), 0);
        drop(writer);
        remove_db_files(&path);
    }

    #[test]
    fn concurrent_readers_use_one_connection_each_when_configured() {
        const READERS: usize = 4;
        let target = temp_db_path("concurrent");
        let writer = open_writer(&target, TIMEOUT).unwrap();
        seed(&writer, 2);

        let pool = Arc::new(ReadPool::new(&target, 0, READERS, TIMEOUT, TIMEOUT).unwrap());
        let barrier = Arc::new(Barrier::new(READERS));
        let handles: Vec<_> = (0..READERS)
            .map(|_| {
                let pool = Arc::clone(&pool);
                let barrier = Arc::clone(&barrier);
                std::thread::spawn(move || {
                    let conn = pool.acquire().expect("reader");
                    // Every thread holds its own connection at this point.
                    barrier.wait();
                    let n = count(&conn);
                    pool.release(conn);
                    n
                })
            })
            .collect();
        for handle in handles {
            assert_eq!(handle.join().unwrap(), 2);
        }
        assert_eq!(pool.open_connections(), READERS);
        drop(pool);
        drop(writer);
        remove_db_files(&target);
    }

    /// Benchmark for #697: p95 latency of short reads with 50 concurrent readers.
    ///
    /// 5 readers run full-table scans and 45 run point lookups, which is the
    /// head-of-line blocking case from #697. With a single connection (`max = 1`,
    /// close to the old single-mutex behaviour) every point lookup waits behind
    /// the scans. The reported p95 is over the point lookups only.
    ///
    /// Run with:
    /// `cargo test -p ethos-protocol-backend db_read_pool::tests::bench -- --ignored --nocapture`
    #[test]
    #[ignore = "benchmark: run manually with --ignored --nocapture"]
    fn bench_p95_point_read_latency_with_50_concurrent_readers() {
        const READERS: usize = 50;
        const SCANNERS: usize = 5;
        const POINT_READS: usize = 200;
        const SCANS: usize = 20;
        const ROWS: usize = 20_000;

        // `:memory:` is what production runs (see main.rs). The pool caps it at one
        // read connection, so only size 1 is measured there. Measured before the cap,
        // the in-memory p95 rose from 0.49 ms (1 connection) to 1.7 ms (4) and 2.9 ms (16).
        for (storage, file_backed, sizes) in [
            ("file", true, vec![1usize, 4, 16]),
            ("memory", false, vec![1usize]),
        ] {
            for max in sizes {
                let path = if file_backed {
                    temp_db_path("bench")
                } else {
                    resolve_target(":memory:")
                };
                let writer = open_writer(&path, TIMEOUT).unwrap();
                seed(&writer, ROWS);
                let pool = Arc::new(ReadPool::new(&path, max, max, TIMEOUT, TIMEOUT).unwrap());
                let barrier = Arc::new(Barrier::new(READERS));

                let handles: Vec<_> = (0..READERS)
                    .map(|reader| {
                        let pool = Arc::clone(&pool);
                        let barrier = Arc::clone(&barrier);
                        std::thread::spawn(move || {
                            barrier.wait();
                            let scanner = reader < SCANNERS;
                            let mut latencies = Vec::new();
                            for n in 0..if scanner { SCANS } else { POINT_READS } {
                                let started = Instant::now();
                                let conn = pool.acquire().expect("reader");
                                if scanner {
                                    let _: i64 = conn
                                        .query_row(
                                            "SELECT SUM(length(label)) FROM items",
                                            [],
                                            |r| r.get(0),
                                        )
                                        .unwrap();
                                } else {
                                    let id = ((reader * 7 + n) % ROWS + 1) as i64;
                                    let _: String = conn
                                        .query_row(
                                            "SELECT label FROM items WHERE id = ?1",
                                            [id],
                                            |r| r.get(0),
                                        )
                                        .unwrap();
                                }
                                pool.release(conn);
                                if !scanner {
                                    latencies.push(started.elapsed());
                                }
                            }
                            latencies
                        })
                    })
                    .collect();

                let mut point: Vec<Duration> = handles
                    .into_iter()
                    .flat_map(|h| h.join().unwrap())
                    .collect();
                point.sort();
                let p95 = point[(point.len() * 95 / 100).min(point.len() - 1)];
                println!(
                    "storage={storage} read_connections={max:>2} readers={READERS} point_reads={} p95={p95:?}",
                    point.len()
                );
                drop(pool);
                drop(writer);
                if file_backed {
                    remove_db_files(&path);
                }
            }
        }
    }
}
