# ADR-0006: SQLite for Backend Storage

**Date:** 2026-10-16  
**Status:** Accepted  
**Supersedes:** [ADR-0005](0005-postgresql-backend-storage.md)  
**Deciders:** Core team

---

## Context

[ADR-0005](0005-postgresql-backend-storage.md) chose PostgreSQL accessed through SQLx. The
implementation never followed that decision:

- `backend/Cargo.toml` has no `sqlx`, `tokio-postgres`, or other Postgres client crate.
- The backend opens a SQLite database through `rusqlite` (`backend/src/db.rs` and the other
  modules that use `rusqlite` directly). Vault, notification, audit, and reminder state all
  go through that layer.
- Until #690 the database was `:memory:`, so data was lost on every restart. Since #690 the
  database is a file at `DB_PATH` on a mounted volume.
- `docker-compose.yml` ran a Postgres 16 service that the backend never connected to. Its
  `DATABASE_URL` was ignored by the binary.

The repository therefore had two storage stories. Code, deployment files, and docs disagreed
about which one was authoritative.

## Decision

SQLite is the authoritative store for the backend in every environment (local development,
staging, and production). The database is one file at `DB_PATH`, default
`/var/lib/ethos-protocol/ethos-protocol.db`, on a persistent volume.

- The Postgres service and `DATABASE_URL` are removed from `docker-compose.yml`.
- `rusqlite` (bundled) stays. It is the only database driver the backend uses.
- ADR-0005 is superseded and kept for history.

## Consequences

**Positive**

- One storage engine to run, back up, and test. Restores are a file copy, and
  `backend/src/backup_validation.rs` already works against this layer.
- No separate database container, credentials, or network hop for local development.
- Data is durable across restarts once `DB_PATH` is on a volume (#690).

**Negative / constraints**

- A SQLite file is written by one process at a time. Running several backend instances
  against the same file over a network filesystem is unsupported. A horizontally scaled
  deployment needs a different store.
- Backups must be taken from the file with SQLite-aware tooling (for example `sqlite3 .backup`)
  rather than a raw copy while the server is writing.
- Moving to PostgreSQL later means rewriting the `rusqlite` call sites in `db.rs` and the
  other modules that use it.

**Revisit this decision if** the backend must run more than one writer, or if the database
needs to be shared across hosts.

## Alternatives Considered

| Alternative | Reason not chosen now |
|---|---|
| PostgreSQL (ADR-0005) | Not implemented. Would require rewriting the `rusqlite` layer and adding a server to every environment. Recorded as the path if multi-writer scaling is needed. |
| Keep both (SQLite for tests, Postgres for deployment) | Two engines with different SQL dialects and behaviour. The bugs would only show up in production. |
