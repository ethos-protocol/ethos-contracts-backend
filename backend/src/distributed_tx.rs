//! # Task #77 — Distributed Transactions Across Shards
//!
//! Implements a **Two-Phase Commit (2PC)** coordinator that spans multiple SQLite
//! shards.  Each shard is modelled as a [`ShardNode`] with its own connection; the
//! [`DistributedTxCoordinator`] orchestrates prepare/commit/rollback across all
//! participating shards.
//!
//! ## Two-Phase Commit flow
//!
//! ```text
//!  Coordinator ──PREPARE──▶ shard-0
//!             ──PREPARE──▶ shard-1
//!             ◀─ OK ─────── shard-0
//!             ◀─ OK ─────── shard-1
//!             ──COMMIT───▶ shard-0
//!             ──COMMIT───▶ shard-1
//! ```
//!
//! If any shard votes ABORT during Phase 1 the coordinator issues ROLLBACK to all
//! shards that already voted OK.
//!
//! ## Shard awareness
//!
//! Each shard owns a key range determined by [`ShardKey`].  The coordinator's
//! [`route_operation`] method maps an operation to the responsible shard(s).
//!
//! ## Configuration
//!
//! | Variable | Default | Description |
//! |---|---|---|
//! | `DB_SHARD_COUNT` | `1` | Number of shards |
//! | `DB_SHARD_PATH_PREFIX` | `shard` | File path prefix; shard N is at `{prefix}_{N}.db` |

use rusqlite::{params, Connection, OptionalExtension};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

// ── Durable coordinator transaction log (#354) ────────────────────────────────

/// Lifecycle of a distributed transaction as recorded in the coordinator's
/// durable log. Transitions are written **before** the corresponding
/// participant RPCs so a coordinator crash can always be resolved on restart.
///
/// ```text
///   Preparing ──▶ Prepared ──▶ Committing ──▶ Committed
///        │            │
///        └────────────┴────────▶ Aborting ──▶ Aborted
/// ```
///
/// The commit point is the durable write of `Committing`: once it is on disk
/// the transaction is presumed-commit on recovery; before it, presumed-abort.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TxState {
    Preparing,
    Prepared,
    Committing,
    Committed,
    Aborting,
    Aborted,
}

impl TxState {
    pub fn as_str(self) -> &'static str {
        match self {
            TxState::Preparing => "preparing",
            TxState::Prepared => "prepared",
            TxState::Committing => "committing",
            TxState::Committed => "committed",
            TxState::Aborting => "aborting",
            TxState::Aborted => "aborted",
        }
    }

    fn from_str(s: &str) -> Option<Self> {
        Some(match s {
            "preparing" => TxState::Preparing,
            "prepared" => TxState::Prepared,
            "committing" => TxState::Committing,
            "committed" => TxState::Committed,
            "aborting" => TxState::Aborting,
            "aborted" => TxState::Aborted,
            _ => return None,
        })
    }

    /// Whether a transaction in this state still needs recovery work.
    fn is_in_flight(self) -> bool {
        !matches!(self, TxState::Committed | TxState::Aborted)
    }
}

/// A single recovered transaction record.
#[derive(Debug, Clone)]
pub struct TxLogRecord {
    pub tx_id: String,
    pub state: TxState,
    /// Shard indices that participate in this transaction.
    pub participants: Vec<usize>,
}

/// Durable write-ahead log of coordinator decisions. Backed by its own SQLite
/// database so it survives a coordinator process crash independently of the
/// shards.
pub struct CoordinatorLog {
    conn: Mutex<Connection>,
}

impl CoordinatorLog {
    /// Open (or create) the log at `path`. Use `":memory:"` only in tests — a
    /// crash-recoverable deployment must point this at a real file.
    pub fn open(path: &str) -> Result<Self, DistributedTxError> {
        let conn = Connection::open(path)
            .map_err(|e| DistributedTxError::LogUnavailable(e.to_string()))?;
        conn.execute_batch(
            r"
            CREATE TABLE IF NOT EXISTS coordinator_tx_log (
                tx_id        TEXT PRIMARY KEY,
                state        TEXT NOT NULL,
                participants TEXT NOT NULL,
                updated_at   TEXT NOT NULL
            );
            ",
        )
        .map_err(|e| DistributedTxError::LogUnavailable(e.to_string()))?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    /// Record (or move) a transaction to `state`. Called **before** the
    /// participant RPCs the state authorises. Returns only once the row is
    /// durably written (SQLite fsync on commit).
    pub fn record(
        &self,
        tx_id: &str,
        state: TxState,
        participants: &[usize],
    ) -> Result<(), DistributedTxError> {
        let parts = participants
            .iter()
            .map(usize::to_string)
            .collect::<Vec<_>>()
            .join(",");
        self.conn
            .lock()
            .unwrap()
            .execute(
                "INSERT INTO coordinator_tx_log (tx_id, state, participants, updated_at)
                 VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT(tx_id) DO UPDATE SET state = excluded.state, updated_at = excluded.updated_at",
                params![tx_id, state.as_str(), parts, chrono::Utc::now().to_rfc3339()],
            )
            .map_err(|e| DistributedTxError::LogUnavailable(e.to_string()))?;
        Ok(())
    }

    /// Load the current state of a transaction, if the log knows about it.
    pub fn get(&self, tx_id: &str) -> Result<Option<TxLogRecord>, DistributedTxError> {
        let conn = self.conn.lock().unwrap();
        conn.query_row(
            "SELECT tx_id, state, participants FROM coordinator_tx_log WHERE tx_id = ?1",
            params![tx_id],
            |r| {
                let tx_id: String = r.get(0)?;
                let state: String = r.get(1)?;
                let participants: String = r.get(2)?;
                Ok((tx_id, state, participants))
            },
        )
        .optional()
        .map_err(|e| DistributedTxError::LogUnavailable(e.to_string()))?
        .map(|(tx_id, state, participants)| decode_record(&tx_id, &state, &participants))
        .transpose()
    }

    /// Every transaction that has not reached a terminal state.
    pub fn load_in_flight(&self) -> Result<Vec<TxLogRecord>, DistributedTxError> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn
            .prepare(
                "SELECT tx_id, state, participants FROM coordinator_tx_log
                 WHERE state NOT IN ('committed', 'aborted')
                 ORDER BY updated_at",
            )
            .map_err(|e| DistributedTxError::LogUnavailable(e.to_string()))?;
        let rows = stmt
            .query_map([], |r| {
                let tx_id: String = r.get(0)?;
                let state: String = r.get(1)?;
                let participants: String = r.get(2)?;
                Ok((tx_id, state, participants))
            })
            .map_err(|e| DistributedTxError::LogUnavailable(e.to_string()))?;

        let mut out = Vec::new();
        for row in rows {
            let (tx_id, state, participants) =
                row.map_err(|e| DistributedTxError::LogUnavailable(e.to_string()))?;
            out.push(decode_record(&tx_id, &state, &participants)?);
        }
        Ok(out)
    }
}

fn decode_record(
    tx_id: &str,
    state: &str,
    participants: &str,
) -> Result<TxLogRecord, DistributedTxError> {
    let state = TxState::from_str(state)
        .ok_or_else(|| DistributedTxError::Serialization(format!("bad tx state: {state}")))?;
    let participants = if participants.is_empty() {
        Vec::new()
    } else {
        participants
            .split(',')
            .map(|s| {
                s.parse::<usize>()
                    .map_err(|e| DistributedTxError::Serialization(e.to_string()))
            })
            .collect::<Result<Vec<_>, _>>()?
    };
    Ok(TxLogRecord {
        tx_id: tx_id.to_string(),
        state,
        participants,
    })
}

/// What the recovery routine did with one in-flight transaction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RecoveryAction {
    /// `Committing`/`Committed` was durable → commit was re-driven to all
    /// participants.
    ResumedCommit,
    /// No durable commit decision → participants were rolled back / compensated.
    Compensated,
}

/// Outcome of recovering a single transaction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecoveryOutcome {
    pub tx_id: String,
    pub action: RecoveryAction,
}

// ── Shard key ─────────────────────────────────────────────────────────────────

/// A routing key used to determine which shard owns a piece of data.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ShardKey(pub String);

impl ShardKey {
    pub fn new(s: impl Into<String>) -> Self {
        Self(s.into())
    }

    /// Map this key to a shard index given `total_shards`.
    pub fn shard_index(&self, total_shards: usize) -> usize {
        if total_shards == 0 {
            return 0;
        }
        // Simple hash-based mapping using the first byte of a FNV-like hash.
        let hash: u64 = self
            .0
            .bytes()
            .fold(14_695_981_039_346_656_037_u64, |acc, b| {
                acc.wrapping_mul(1_099_511_628_211_u64)
                    .wrapping_add(b as u64)
            });
        (hash % total_shards as u64) as usize
    }
}

// ── Shard node ────────────────────────────────────────────────────────────────

/// A shard participant in the distributed transaction protocol.
pub struct ShardNode {
    /// Shard identifier (0-based index).
    pub shard_id: usize,
    /// Underlying connection to this shard's database.
    conn: Mutex<Connection>,
}

impl ShardNode {
    /// Open or create the shard database at `path`.
    pub fn open(shard_id: usize, path: &str) -> Result<Self, DistributedTxError> {
        let conn = Connection::open(path)
            .map_err(|e| DistributedTxError::ShardUnavailable(shard_id, e.to_string()))?;
        Ok(Self {
            shard_id,
            conn: Mutex::new(conn),
        })
    }

    /// Bootstrap a minimal schema for this shard (idempotent).
    pub fn bootstrap(&self) -> Result<(), DistributedTxError> {
        self.conn
            .lock()
            .unwrap()
            .execute_batch(
                r"
                CREATE TABLE IF NOT EXISTS prepared_transactions (
                    tx_id       TEXT NOT NULL,
                    operations  TEXT NOT NULL,
                    prepared_at TEXT NOT NULL,
                    PRIMARY KEY (tx_id)
                );
                CREATE TABLE IF NOT EXISTS committed_transactions (
                    tx_id        TEXT NOT NULL,
                    committed_at TEXT NOT NULL,
                    PRIMARY KEY (tx_id)
                );
                CREATE TABLE IF NOT EXISTS kv_data (
                    shard_key TEXT PRIMARY KEY,
                    value     TEXT NOT NULL
                );
                ",
            )
            .map_err(|e| DistributedTxError::ShardUnavailable(self.shard_id, e.to_string()))
    }

    // ── Phase 1: Prepare ─────────────────────────────────────────────────────

    /// Write the transaction's operations to the prepare log.
    ///
    /// Returns `Ok(Vote::Commit)` if the shard accepts, `Ok(Vote::Abort)` if it
    /// detects a conflict.
    pub fn prepare(
        &self,
        tx_id: &str,
        operations: &[Operation],
    ) -> Result<Vote, DistributedTxError> {
        let ops_json = serde_json::to_string(operations)
            .map_err(|e| DistributedTxError::Serialization(e.to_string()))?;

        let conn = self.conn.lock().unwrap();

        // Check for duplicate tx_id (idempotency guard): refuse a transaction
        // that is already prepared on this shard, or that has already been
        // committed here (its prepare-log row is cleared on commit, so we must
        // consult `committed_transactions` too).
        let already_seen: bool = conn
            .query_row(
                "SELECT 1 WHERE EXISTS (SELECT 1 FROM prepared_transactions WHERE tx_id = ?1)
                             OR EXISTS (SELECT 1 FROM committed_transactions WHERE tx_id = ?1)",
                params![tx_id],
                |_| Ok(true),
            )
            .unwrap_or(false);

        if already_seen {
            return Ok(Vote::Abort); // refuse duplicate
        }

        conn.execute(
            "INSERT INTO prepared_transactions (tx_id, operations, prepared_at) VALUES (?1, ?2, ?3)",
            params![tx_id, ops_json, chrono::Utc::now().to_rfc3339()],
        )
        .map_err(|e| DistributedTxError::ShardUnavailable(self.shard_id, e.to_string()))?;

        Ok(Vote::Commit)
    }

    // ── Phase 2a: Commit ──────────────────────────────────────────────────────

    /// Apply the prepared operations and record the commit.
    pub fn commit(&self, tx_id: &str) -> Result<(), DistributedTxError> {
        let conn = self.conn.lock().unwrap();

        // Retrieve prepared operations.
        let ops_json: String = conn
            .query_row(
                "SELECT operations FROM prepared_transactions WHERE tx_id = ?1",
                params![tx_id],
                |r| r.get(0),
            )
            .map_err(|_| DistributedTxError::TransactionNotFound(tx_id.to_string()))?;

        let operations: Vec<Operation> = serde_json::from_str(&ops_json)
            .map_err(|e| DistributedTxError::Serialization(e.to_string()))?;

        // Apply each operation inside the SQLite transaction.
        conn.execute_batch("BEGIN IMMEDIATE").ok();

        for op in &operations {
            if let Err(e) = apply_operation_inner(&conn, op) {
                conn.execute_batch("ROLLBACK").ok();
                return Err(e);
            }
        }

        conn.execute(
            "INSERT OR REPLACE INTO committed_transactions (tx_id, committed_at) VALUES (?1, ?2)",
            params![tx_id, chrono::Utc::now().to_rfc3339()],
        )
        .map_err(|e| DistributedTxError::ShardUnavailable(self.shard_id, e.to_string()))?;

        // Clean up prepare log.
        conn.execute(
            "DELETE FROM prepared_transactions WHERE tx_id = ?1",
            params![tx_id],
        )
        .ok();

        conn.execute_batch("COMMIT")
            .map_err(|e| DistributedTxError::ShardUnavailable(self.shard_id, e.to_string()))
    }

    // ── Phase 2b: Rollback ────────────────────────────────────────────────────

    /// Roll back a prepared transaction by discarding the prepare log entry.
    pub fn rollback(&self, tx_id: &str) -> Result<(), DistributedTxError> {
        self.conn
            .lock()
            .unwrap()
            .execute(
                "DELETE FROM prepared_transactions WHERE tx_id = ?1",
                params![tx_id],
            )
            .map_err(|e| DistributedTxError::ShardUnavailable(self.shard_id, e.to_string()))?;
        Ok(())
    }

    // ── KV read ───────────────────────────────────────────────────────────────

    /// Read a value from the shard's kv_data store.
    pub fn read(&self, key: &str) -> Result<Option<String>, DistributedTxError> {
        let conn = self.conn.lock().unwrap();
        let result = conn
            .query_row(
                "SELECT value FROM kv_data WHERE shard_key = ?1",
                params![key],
                |r| r.get(0),
            )
            .optional()
            .map_err(|e| DistributedTxError::ShardUnavailable(self.shard_id, e.to_string()))?;
        Ok(result)
    }
}

// ── Operation ─────────────────────────────────────────────────────────────────

/// A single unit of work to be applied within a distributed transaction.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub enum Operation {
    /// Insert or replace a key-value pair.
    Put { shard_key: String, value: String },
    /// Delete a key-value pair.
    Delete { shard_key: String },
    /// Execute arbitrary SQL (must be non-DDL for safety in tests).
    RawSql { sql: String },
}

/// Applies a single operation to `conn` without transaction management.
fn apply_operation_inner(conn: &Connection, op: &Operation) -> Result<(), DistributedTxError> {
    match op {
        Operation::Put { shard_key, value } => {
            conn.execute(
                "INSERT OR REPLACE INTO kv_data (shard_key, value) VALUES (?1, ?2)",
                params![shard_key, value],
            )
            .map_err(|e| DistributedTxError::ApplyFailed(e.to_string()))?;
        }
        Operation::Delete { shard_key } => {
            conn.execute(
                "DELETE FROM kv_data WHERE shard_key = ?1",
                params![shard_key],
            )
            .map_err(|e| DistributedTxError::ApplyFailed(e.to_string()))?;
        }
        Operation::RawSql { sql } => {
            conn.execute_batch(sql)
                .map_err(|e| DistributedTxError::ApplyFailed(e.to_string()))?;
        }
    }
    Ok(())
}

// ── 2PC Vote ──────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Vote {
    /// Shard is ready to commit.
    Commit,
    /// Shard vetoes the transaction.
    Abort,
}

// ── Transaction descriptor ────────────────────────────────────────────────────

/// Describes a distributed transaction before it is submitted to the coordinator.
#[derive(Debug, Clone)]
pub struct DistributedTransaction {
    /// Globally unique transaction ID.
    pub tx_id: String,
    /// Per-shard operations: shard_index → list of operations.
    pub shard_ops: HashMap<usize, Vec<Operation>>,
}

impl DistributedTransaction {
    pub fn new(tx_id: impl Into<String>) -> Self {
        Self {
            tx_id: tx_id.into(),
            shard_ops: HashMap::new(),
        }
    }

    /// Add an operation destined for `shard_index`.
    pub fn add_op(&mut self, shard_index: usize, op: Operation) {
        self.shard_ops.entry(shard_index).or_default().push(op);
    }
}

// ── Coordinator ───────────────────────────────────────────────────────────────

/// Outcome of a distributed transaction execution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TxOutcome {
    Committed,
    RolledBack,
}

/// Coordinator that runs 2PC across a set of [`ShardNode`]s.
pub struct DistributedTxCoordinator {
    shards: Vec<Arc<ShardNode>>,
    /// Durable write-ahead log of coordinator decisions, consulted by
    /// [`recover`](Self::recover) after a crash.
    log: Arc<CoordinatorLog>,
}

impl DistributedTxCoordinator {
    /// Build a coordinator from explicit shard nodes with an in-memory decision
    /// log (tests only — an in-memory log does not survive a real crash).
    pub fn new(shards: Vec<Arc<ShardNode>>) -> Self {
        Self {
            shards,
            log: Arc::new(CoordinatorLog::open(":memory:").expect("in-memory log")),
        }
    }

    /// Build a coordinator with an explicit durable decision log.
    pub fn with_log(shards: Vec<Arc<ShardNode>>, log: Arc<CoordinatorLog>) -> Self {
        Self { shards, log }
    }

    /// Access the durable decision log.
    pub fn log(&self) -> &Arc<CoordinatorLog> {
        &self.log
    }

    /// Build a coordinator from environment variables.
    ///
    /// Creates `DB_SHARD_COUNT` in-memory shards for simplicity (in production
    /// replace `:memory:` with the shard file paths).
    pub fn from_env() -> Result<Self, DistributedTxError> {
        let count: usize = std::env::var("DB_SHARD_COUNT")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(1);

        let mut shards = Vec::with_capacity(count);
        for i in 0..count {
            let path = std::env::var("DB_SHARD_PATH_PREFIX")
                .map(|prefix| format!("{prefix}_{i}.db"))
                .unwrap_or_else(|_| ":memory:".to_string());

            let shard = Arc::new(ShardNode::open(i, &path)?);
            shard.bootstrap()?;
            shards.push(shard);
        }

        // The decision log lives alongside the shards; a file path makes it
        // crash-recoverable, `:memory:` (the default) does not.
        let log_path =
            std::env::var("DB_COORDINATOR_LOG_PATH").unwrap_or_else(|_| ":memory:".to_string());
        let log = Arc::new(CoordinatorLog::open(&log_path)?);

        Ok(Self { shards, log })
    }

    /// Route an [`Operation`] to the correct shard based on its key.
    ///
    /// Returns `(shard_index, operation)`.
    pub fn route_operation(&self, op: &Operation) -> usize {
        let key = match op {
            Operation::Put { shard_key, .. } => shard_key.clone(),
            Operation::Delete { shard_key } => shard_key.clone(),
            Operation::RawSql { .. } => String::new(), // raw SQL → shard 0
        };
        ShardKey::new(key).shard_index(self.shards.len())
    }

    /// Execute a [`DistributedTransaction`] using two-phase commit.
    ///
    /// Returns [`TxOutcome::Committed`] if all shards voted Commit, or
    /// [`TxOutcome::RolledBack`] if any shard voted Abort (in which case all
    /// shards that already voted Commit receive a rollback request).
    pub fn execute(&self, tx: &DistributedTransaction) -> Result<TxOutcome, DistributedTxError> {
        let tx_id = &tx.tx_id;
        let mut participating_shards: Vec<usize> = tx.shard_ops.keys().copied().collect();
        participating_shards.sort_unstable();

        // Durably record intent BEFORE contacting any participant, so a crash
        // here is recoverable as presumed-abort.
        self.log
            .record(tx_id, TxState::Preparing, &participating_shards)?;

        // ── Phase 1: Prepare ─────────────────────────────────────────────────
        let mut prepared_shards: Vec<usize> = Vec::new();
        for &shard_idx in &participating_shards {
            let shard = self
                .shards
                .get(shard_idx)
                .ok_or(DistributedTxError::ShardUnavailable(
                    shard_idx,
                    "index out of range".to_string(),
                ))?;

            let ops = tx
                .shard_ops
                .get(&shard_idx)
                .map(Vec::as_slice)
                .unwrap_or(&[]);
            match shard.prepare(tx_id, ops)? {
                Vote::Commit => {
                    prepared_shards.push(shard_idx);
                }
                Vote::Abort => {
                    // No commit decision was ever made durable → abort.
                    self.log
                        .record(tx_id, TxState::Aborting, &participating_shards)?;
                    for &already in &prepared_shards {
                        if let Some(s) = self.shards.get(already) {
                            s.rollback(tx_id).ok();
                        }
                    }
                    self.log
                        .record(tx_id, TxState::Aborted, &participating_shards)?;
                    return Ok(TxOutcome::RolledBack);
                }
            }
        }

        self.log
            .record(tx_id, TxState::Prepared, &participating_shards)?;

        // ── Commit point ────────────────────────────────────────────────────
        // Persisting `Committing` is the atomic commit decision: if the
        // coordinator crashes after this write, recovery drives the commit
        // forward on every participant; if it crashes before, recovery aborts.
        self.log
            .record(tx_id, TxState::Committing, &participating_shards)?;

        // ── Phase 2: Commit ─────────────────────────────────────────────────
        for &shard_idx in &participating_shards {
            let shard = self
                .shards
                .get(shard_idx)
                .ok_or(DistributedTxError::ShardUnavailable(
                    shard_idx,
                    "index out of range".to_string(),
                ))?;

            shard.commit(tx_id)?;
        }

        self.log
            .record(tx_id, TxState::Committed, &participating_shards)?;

        Ok(TxOutcome::Committed)
    }

    /// Recovery routine — run once on coordinator startup.
    ///
    /// Scans the durable decision log for transactions that never reached a
    /// terminal state (a crash mid-flight) and resolves each one:
    ///
    /// | Durable state on restart | Decision | Recovery action |
    /// |---|---|---|
    /// | `Committing` / `Committed` | commit was decided | re-drive `commit` on every participant (idempotent) |
    /// | `Preparing` / `Prepared` / `Aborting` | no commit decision | `rollback` every participant (presumed abort) |
    ///
    /// Participant `commit`/`rollback` are idempotent (the shard's
    /// `committed_transactions` / `prepared_transactions` tables dedupe), so
    /// recovery is safe to run repeatedly.
    pub fn recover(&self) -> Result<Vec<RecoveryOutcome>, DistributedTxError> {
        let in_flight = self.log.load_in_flight()?;
        let mut outcomes = Vec::with_capacity(in_flight.len());

        for rec in in_flight {
            debug_assert!(rec.state.is_in_flight());
            let presumed_commit = matches!(rec.state, TxState::Committing | TxState::Committed);

            if presumed_commit {
                self.log
                    .record(&rec.tx_id, TxState::Committing, &rec.participants)?;
                for &shard_idx in &rec.participants {
                    if let Some(shard) = self.shards.get(shard_idx) {
                        // A participant that already committed pre-crash returns
                        // TransactionNotFound (prepare log cleared) — treat as done.
                        match shard.commit(&rec.tx_id) {
                            Ok(()) | Err(DistributedTxError::TransactionNotFound(_)) => {}
                            Err(e) => return Err(e),
                        }
                    }
                }
                self.log
                    .record(&rec.tx_id, TxState::Committed, &rec.participants)?;
                outcomes.push(RecoveryOutcome {
                    tx_id: rec.tx_id,
                    action: RecoveryAction::ResumedCommit,
                });
            } else {
                self.log
                    .record(&rec.tx_id, TxState::Aborting, &rec.participants)?;
                for &shard_idx in &rec.participants {
                    if let Some(shard) = self.shards.get(shard_idx) {
                        shard.rollback(&rec.tx_id).ok();
                    }
                }
                self.log
                    .record(&rec.tx_id, TxState::Aborted, &rec.participants)?;
                outcomes.push(RecoveryOutcome {
                    tx_id: rec.tx_id,
                    action: RecoveryAction::Compensated,
                });
            }
        }

        Ok(outcomes)
    }

    /// Number of shards managed by this coordinator.
    pub fn shard_count(&self) -> usize {
        self.shards.len()
    }

    // ── Shard rebalancing ────────────────────────────────────────────────────

    /// Redistribute keys from `source_shard` to `target_shard`.
    ///
    /// Reads all `kv_data` rows from the source whose key hashes to
    /// `target_shard` under the *new* total shard count, writes them to the
    /// target, and deletes them from the source — all in a single 2PC
    /// transaction.
    ///
    /// In practice rebalancing would be triggered by adding/removing shards.
    pub fn rebalance(
        &self,
        source_shard: usize,
        target_shard: usize,
        new_total_shards: usize,
    ) -> Result<u64, DistributedTxError> {
        let source = self.shards.get(source_shard).ok_or_else(|| {
            DistributedTxError::ShardUnavailable(source_shard, "not found".into())
        })?;
        // Validate the target exists up front (the 2PC below routes to it).
        let _target = self.shards.get(target_shard).ok_or_else(|| {
            DistributedTxError::ShardUnavailable(target_shard, "not found".into())
        })?;

        // Collect keys to migrate.
        let rows: Vec<(String, String)> = {
            let conn = source.conn.lock().unwrap();
            let mut stmt = conn
                .prepare("SELECT shard_key, value FROM kv_data")
                .map_err(|e| DistributedTxError::ShardUnavailable(source_shard, e.to_string()))?;

            let collected = stmt
                .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
                .map_err(|e| DistributedTxError::ShardUnavailable(source_shard, e.to_string()))?
                .collect::<Result<Vec<_>, _>>()
                .map_err(|e| DistributedTxError::ShardUnavailable(source_shard, e.to_string()))?;
            collected
        };

        let to_migrate: Vec<(String, String)> = rows
            .into_iter()
            .filter(|(k, _)| ShardKey::new(k).shard_index(new_total_shards) == target_shard)
            .collect();

        let count = to_migrate.len() as u64;
        if count == 0 {
            return Ok(0);
        }

        // Build a 2PC transaction.
        let tx_id = uuid::Uuid::new_v4().to_string();
        let mut tx = DistributedTransaction::new(&tx_id);

        for (key, value) in &to_migrate {
            tx.add_op(
                target_shard,
                Operation::Put {
                    shard_key: key.clone(),
                    value: value.clone(),
                },
            );
            tx.add_op(
                source_shard,
                Operation::Delete {
                    shard_key: key.clone(),
                },
            );
        }

        self.execute(&tx)?;
        Ok(count)
    }
}

// ── Error type ────────────────────────────────────────────────────────────────

#[derive(Debug)]
pub enum DistributedTxError {
    /// A shard could not be reached.
    ShardUnavailable(usize, String),
    /// The transaction ID is unknown.
    TransactionNotFound(String),
    /// Serialization of operations failed.
    Serialization(String),
    /// An operation could not be applied.
    ApplyFailed(String),
    /// The durable coordinator decision log could not be read or written.
    LogUnavailable(String),
}

impl std::fmt::Display for DistributedTxError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ShardUnavailable(id, msg) => write!(f, "shard {id} unavailable: {msg}"),
            Self::TransactionNotFound(tx_id) => write!(f, "transaction not found: {tx_id}"),
            Self::Serialization(msg) => write!(f, "serialization error: {msg}"),
            Self::ApplyFailed(msg) => write!(f, "apply failed: {msg}"),
            Self::LogUnavailable(msg) => write!(f, "coordinator log unavailable: {msg}"),
        }
    }
}

impl std::error::Error for DistributedTxError {}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn make_coordinator(n: usize) -> DistributedTxCoordinator {
        let shards: Vec<Arc<ShardNode>> = (0..n)
            .map(|i| {
                let shard = Arc::new(ShardNode::open(i, ":memory:").expect("open"));
                shard.bootstrap().expect("bootstrap");
                shard
            })
            .collect();
        DistributedTxCoordinator::new(shards)
    }

    #[test]
    fn test_shard_key_maps_consistently() {
        let key = ShardKey::new("vault-abc");
        let idx1 = key.shard_index(4);
        let idx2 = key.shard_index(4);
        assert_eq!(idx1, idx2);
        assert!(idx1 < 4);
    }

    #[test]
    fn test_shard_key_zero_shards_returns_zero() {
        let key = ShardKey::new("anything");
        assert_eq!(key.shard_index(0), 0);
    }

    #[test]
    fn test_single_shard_commit() {
        let coord = make_coordinator(1);

        let mut tx = DistributedTransaction::new("tx-001");
        tx.add_op(
            0,
            Operation::Put {
                shard_key: "k1".into(),
                value: "v1".into(),
            },
        );

        let outcome = coord.execute(&tx).expect("execute");
        assert_eq!(outcome, TxOutcome::Committed);

        let val = coord.shards[0].read("k1").unwrap();
        assert_eq!(val, Some("v1".to_string()));
    }

    #[test]
    fn test_multi_shard_commit() {
        let coord = make_coordinator(2);

        let mut tx = DistributedTransaction::new("tx-multi");
        tx.add_op(
            0,
            Operation::Put {
                shard_key: "key0".into(),
                value: "val0".into(),
            },
        );
        tx.add_op(
            1,
            Operation::Put {
                shard_key: "key1".into(),
                value: "val1".into(),
            },
        );

        let outcome = coord.execute(&tx).expect("execute");
        assert_eq!(outcome, TxOutcome::Committed);

        assert_eq!(
            coord.shards[0].read("key0").unwrap(),
            Some("val0".to_string())
        );
        assert_eq!(
            coord.shards[1].read("key1").unwrap(),
            Some("val1".to_string())
        );
    }

    #[test]
    fn test_duplicate_tx_id_votes_abort() {
        let coord = make_coordinator(1);

        let mut tx = DistributedTransaction::new("tx-dup");
        tx.add_op(
            0,
            Operation::Put {
                shard_key: "k".into(),
                value: "v".into(),
            },
        );

        coord.execute(&tx).unwrap(); // first commit

        // Re-submit same tx_id — shard already has it in committed log,
        // so prepare detects duplicate and votes Abort.
        let outcome2 = coord.execute(&tx).unwrap();
        assert_eq!(outcome2, TxOutcome::RolledBack);
    }

    #[test]
    fn test_rollback_on_abort() {
        let coord = make_coordinator(2);

        // Manually prepare shard-0 with the same tx_id to trigger an abort on
        // the second shard's prepare call.
        let ops = vec![Operation::Put {
            shard_key: "x".into(),
            value: "y".into(),
        }];
        coord.shards[0].prepare("tx-force-abort", &ops).unwrap();

        let mut tx = DistributedTransaction::new("tx-force-abort");
        tx.add_op(
            0,
            Operation::Put {
                shard_key: "x".into(),
                value: "y".into(),
            },
        );
        tx.add_op(
            1,
            Operation::Put {
                shard_key: "z".into(),
                value: "w".into(),
            },
        );

        // Shard 0 will abort (duplicate), coordinator should rollback shard 1.
        let outcome = coord.execute(&tx).unwrap();
        assert_eq!(outcome, TxOutcome::RolledBack);

        // Key "z" must NOT have been committed to shard 1.
        assert_eq!(coord.shards[1].read("z").unwrap(), None);
    }

    #[test]
    fn test_delete_operation() {
        let coord = make_coordinator(1);

        let mut tx1 = DistributedTransaction::new("tx-put");
        tx1.add_op(
            0,
            Operation::Put {
                shard_key: "del-me".into(),
                value: "some-value".into(),
            },
        );
        coord.execute(&tx1).unwrap();

        let mut tx2 = DistributedTransaction::new("tx-del");
        tx2.add_op(
            0,
            Operation::Delete {
                shard_key: "del-me".into(),
            },
        );
        coord.execute(&tx2).unwrap();

        assert_eq!(coord.shards[0].read("del-me").unwrap(), None);
    }

    #[test]
    fn test_coordinator_shard_count() {
        let coord = make_coordinator(3);
        assert_eq!(coord.shard_count(), 3);
    }

    #[test]
    fn test_route_operation_returns_valid_index() {
        let coord = make_coordinator(4);
        let op = Operation::Put {
            shard_key: "some-key".into(),
            value: "v".into(),
        };
        let idx = coord.route_operation(&op);
        assert!(idx < 4);
    }

    #[test]
    fn test_rebalance_moves_keys() {
        // Create a coordinator with 2 shards, then rebalance under 3 shards.
        let coord = make_coordinator(3);

        // Seed some keys into shard-0.
        let keys = ["alpha", "beta", "gamma", "delta", "epsilon"];
        for k in &keys {
            let mut tx = DistributedTransaction::new(format!("seed-{k}"));
            tx.add_op(
                0,
                Operation::Put {
                    shard_key: k.to_string(),
                    value: "v".into(),
                },
            );
            coord.execute(&tx).unwrap();
        }

        // Rebalance: move keys that would belong to shard-2 under 3-shard layout.
        let moved = coord.rebalance(0, 2, 3).unwrap();
        // moved should be ≥ 0; exact count depends on hash distribution.
        let _ = moved; // just asserting it doesn't panic
    }

    #[test]
    fn test_empty_transaction_commits() {
        let coord = make_coordinator(1);
        let tx = DistributedTransaction::new("tx-empty");
        // No ops — should commit vacuously.
        let outcome = coord.execute(&tx).unwrap();
        assert_eq!(outcome, TxOutcome::Committed);
    }

    #[test]
    fn test_from_env_single_shard() {
        std::env::set_var("DB_SHARD_COUNT", "1");
        let coord = DistributedTxCoordinator::from_env().unwrap();
        assert_eq!(coord.shard_count(), 1);
        std::env::remove_var("DB_SHARD_COUNT");
    }

    // ── Crash-recovery tests (#354) ─────────────────────────────────────────

    /// Build `n` shards plus a shared durable log, returning both so a test can
    /// drop the "crashed" coordinator and reopen a fresh one over the same
    /// shards and log.
    fn shards_and_log(n: usize) -> (Vec<Arc<ShardNode>>, Arc<CoordinatorLog>) {
        let shards: Vec<Arc<ShardNode>> = (0..n)
            .map(|i| {
                let shard = Arc::new(ShardNode::open(i, ":memory:").expect("open"));
                shard.bootstrap().expect("bootstrap");
                shard
            })
            .collect();
        let log = Arc::new(CoordinatorLog::open(":memory:").expect("log"));
        (shards, log)
    }

    #[test]
    fn execute_records_terminal_state_in_the_log() {
        let (shards, log) = shards_and_log(2);
        let coord = DistributedTxCoordinator::with_log(shards, Arc::clone(&log));

        let mut tx = DistributedTransaction::new("tx-logged");
        tx.add_op(
            0,
            Operation::Put {
                shard_key: "a".into(),
                value: "1".into(),
            },
        );
        tx.add_op(
            1,
            Operation::Put {
                shard_key: "b".into(),
                value: "2".into(),
            },
        );
        coord.execute(&tx).unwrap();

        let rec = log.get("tx-logged").unwrap().unwrap();
        assert_eq!(rec.state, TxState::Committed);
        assert_eq!(rec.participants, vec![0, 1]);
        assert!(log.load_in_flight().unwrap().is_empty());
    }

    #[test]
    fn recovery_resumes_commit_when_decision_was_durable() {
        // Simulate: coordinator persisted `Committing`, committed shard 0, then
        // crashed before committing shard 1.
        let (shards, log) = shards_and_log(2);
        let tx_id = "tx-crash-after-decision";
        let ops0 = vec![Operation::Put {
            shard_key: "k0".into(),
            value: "v0".into(),
        }];
        let ops1 = vec![Operation::Put {
            shard_key: "k1".into(),
            value: "v1".into(),
        }];
        shards[0].prepare(tx_id, &ops0).unwrap();
        shards[1].prepare(tx_id, &ops1).unwrap();
        log.record(tx_id, TxState::Committing, &[0, 1]).unwrap();
        shards[0].commit(tx_id).unwrap(); // partial commit before "crash"

        // Restart: fresh coordinator over the same shards + log.
        let coord = DistributedTxCoordinator::with_log(shards, Arc::clone(&log));
        let outcomes = coord.recover().unwrap();
        assert_eq!(outcomes.len(), 1);
        assert_eq!(outcomes[0].action, RecoveryAction::ResumedCommit);

        assert_eq!(coord.shards[0].read("k0").unwrap(), Some("v0".into()));
        assert_eq!(coord.shards[1].read("k1").unwrap(), Some("v1".into()));
        assert_eq!(log.get(tx_id).unwrap().unwrap().state, TxState::Committed);
    }

    #[test]
    fn recovery_aborts_when_no_commit_decision_was_durable() {
        // Simulate: coordinator prepared both shards, persisted `Prepared`, then
        // crashed before the commit decision.
        let (shards, log) = shards_and_log(2);
        let tx_id = "tx-crash-before-decision";
        let ops = vec![Operation::Put {
            shard_key: "z".into(),
            value: "w".into(),
        }];
        shards[0].prepare(tx_id, &ops).unwrap();
        shards[1].prepare(tx_id, &ops).unwrap();
        log.record(tx_id, TxState::Prepared, &[0, 1]).unwrap();

        let coord = DistributedTxCoordinator::with_log(shards, Arc::clone(&log));
        let outcomes = coord.recover().unwrap();
        assert_eq!(outcomes.len(), 1);
        assert_eq!(outcomes[0].action, RecoveryAction::Compensated);

        // Nothing was applied, and the prepared entries were discarded so the
        // shards can accept new work under the same key.
        assert_eq!(coord.shards[0].read("z").unwrap(), None);
        assert_eq!(coord.shards[1].read("z").unwrap(), None);
        assert_eq!(log.get(tx_id).unwrap().unwrap().state, TxState::Aborted);
    }

    #[test]
    fn recovery_is_idempotent() {
        let (shards, log) = shards_and_log(1);
        let tx_id = "tx-idem";
        let ops = vec![Operation::Put {
            shard_key: "i".into(),
            value: "1".into(),
        }];
        shards[0].prepare(tx_id, &ops).unwrap();
        log.record(tx_id, TxState::Committing, &[0]).unwrap();

        let coord = DistributedTxCoordinator::with_log(shards, Arc::clone(&log));
        let first = coord.recover().unwrap();
        assert_eq!(first.len(), 1);
        // Second pass has nothing in flight and must not error.
        let second = coord.recover().unwrap();
        assert!(second.is_empty());
        assert_eq!(coord.shards[0].read("i").unwrap(), Some("1".into()));
    }
}
