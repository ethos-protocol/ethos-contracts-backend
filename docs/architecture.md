# Architecture

## Distributed Tracing Sampling

The tracing layer uses an **adaptive sampling rate** rather than a fixed rate.
The effective rate scales inversely with the current request volume:

```
rate = clamp(base_rate * REFERENCE_VOLUME / volume, MIN_RATE, MAX_RATE)
```

- `base_rate` is the configured rate (default `0.1`).
- `REFERENCE_VOLUME` (100 requests/window) is the volume at which the
  configured base rate is used unchanged.
- `MIN_RATE` / `MAX_RATE` bound the rate so it never collapses to zero or
  exceeds full sampling.

### Behavior

- **Traffic spikes:** as volume rises above the reference, the rate drops,
  protecting the tracing backend from being overwhelmed.
- **Low traffic:** as volume falls below the reference, the rate rises so
  interesting events are not under-sampled.
- **Errors:** requests that result in an error are **always sampled**,
  regardless of the configured or adaptive rate.

Request volume is measured over a rolling window (`SamplingConfig::window`,
default 1s) and reset on window rollover. The adaptive strategy is implemented
in `backend/src/tracing_sampling.rs` and covered by unit tests that simulate
load and verify rate adjustment.

## Database Concurrency Model

The SQLite database in `backend/src/db.rs` is accessed through two kinds of connection (#696, #697).

- **One writer.** All writes (`INSERT`, `UPDATE`, `DELETE`, DDL, migrations) run on a single
  connection behind a mutex. SQLite allows only one writer at a time, so this is intentional.
- **A read-connection pool.** Read-only methods (for example `get_vault`, `get`, `all`,
  `list_feature_flags`) check out a connection from `ReadPool` (`backend/src/db_read_pool.rs`).
  Reads therefore run in parallel with each other and with the writer.

### Sizing

| Setting | Effect |
|---|---|
| `DB_POOL_MIN` | Read connections opened at startup |
| `DB_POOL_MAX` | Upper bound on read connections; the pool grows on demand |
| `DB_POOL_TIMEOUT_SECS` | Busy timeout, and how long a read waits for a free connection |

The effective size is logged at startup as `read_connections_max` and `read_connections_open`.

### Storage-specific behaviour

- **File databases** use WAL mode. Readers do not block the writer, and each read sees the latest
  committed data.
- **`:memory:` databases** (the current default in `main.rs`) use a named shared-cache database.
  Shared-cache readers are serialized inside SQLite, so the pool is capped at **one** read
  connection. Measured with 50 concurrent readers, more connections raised p95 latency for
  short reads (0.49 ms with one connection, 1.7 ms with four, 2.9 ms with sixteen).

### Overload

If no read connection frees up within `DB_POOL_TIMEOUT_SECS`, the read falls back to the writer
connection. Requests slow down under overload but do not fail.

### Read-only enforcement

Read connections set `PRAGMA query_only`, so a write routed to the pool fails with
`SQLITE_READONLY` instead of changing data.

### Benchmark

`cargo test -p ethos-protocol-backend db_read_pool::tests::bench -- --ignored --nocapture`
runs 50 concurrent readers. Five of them run full-table scans and 45 run point lookups. The
p95 is reported for the point lookups.

On a 2-core development machine with a 20,000-row table, file-backed p95 was 1.06 ms with one
read connection, 0.17 ms with four, and 0.004 ms with sixteen. Results depend on core count and
query mix. A workload of only CPU-bound scans on a small machine gains nothing from more connections.

### Rule for maintainers

Do not call `Db::reader()` while holding the writer lock or another read guard. If the pool is
exhausted, the fallback waits on a mutex the thread already holds, which deadlocks. The existing
code scopes guards in blocks so that nested calls run after the outer guard drops.
