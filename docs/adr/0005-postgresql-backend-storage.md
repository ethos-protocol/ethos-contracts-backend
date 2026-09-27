# ADR-0005: PostgreSQL for Backend Storage

**Date:** 2024-03-10  
**Status:** Accepted  
**Deciders:** Core team

---

## Context

The backend service needs persistent storage for:

- WebAuthn credential records (public keys, credential IDs, counters)
- Off-chain vault metadata index (vault ID, owner, beneficiary, TTL cache)
- Reminder schedule state (next notification time, delivery status)
- Audit logs and event history

The storage layer must support ACID transactions, complex queries, and reliable concurrent access.

## Decision

We will use PostgreSQL as the backend's primary persistent data store, accessed via SQLx with compile-time checked queries.

## Alternatives Considered

| Alternative | Reason rejected |
|---|---|
| SQLite | Not suitable for concurrent server workloads; no network replication |
| MySQL / MariaDB | Weaker ACID semantics historically; less idiomatic in the Rust ecosystem |
| MongoDB | Schema flexibility not needed; ACID transactions are a hard requirement for credential storage |
| Redis (primary store) | Not durable by default; not suitable as a primary source of truth |
| CockroachDB | Distributed SQL adds operational complexity not justified at current scale |

## Consequences

### Positive

- ACID transactions ensure credential and vault records are never partially written.
- SQLx compile-time query checking catches SQL errors at build time, not runtime.
- PostgreSQL's JSONB support allows flexible storage of WebAuthn authenticator data.
- Well-understood operational model; extensive tooling for backups, replication, and monitoring.

### Negative

- Requires running a PostgreSQL instance; adds operational overhead compared to embedded databases.
- Schema migrations must be managed carefully (SQLx migrate or similar).

### Neutral

- Local development uses Docker Compose (`docker-compose.yml`) to spin up PostgreSQL on `localhost:5432`.
- Connection pooling is handled within the backend; see `backend/src/db.rs`.

## Related ADRs

- [ADR-0004](0004-rust-backend.md) — Rust backend that uses this storage layer
