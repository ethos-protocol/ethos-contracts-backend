# ADR-0004: Rust for the Backend Service

**Date:** 2024-03-01  
**Status:** Accepted  
**Deciders:** Core team

---

## Context

Ethos-Protocol requires a backend service to handle:

- WebAuthn / Passkey credential registration and verification
- Encrypted check-in reminder delivery (email / SMS)
- Off-chain vault metadata indexing
- Webhook delivery and signature verification
- Admin API for monitoring and operations

The backend must be reliable, maintainable, and consistent with the rest of the codebase. The smart contracts are already written in Rust; a shared language reduces context-switching and enables code sharing (e.g., types, serialisation logic).

## Decision

We will implement the backend service in Rust using the Axum web framework with PostgreSQL (via SQLx) for persistent storage.

## Alternatives Considered

| Alternative | Reason rejected |
|---|---|
| Node.js / TypeScript | Familiar for web developers but introduces a second language; weaker memory safety guarantees |
| Go | Good performance and simplicity, but adds a second language with no code-sharing benefit |
| Python (FastAPI) | Easier prototyping but slower at runtime; weaker type safety |
| Java / Spring Boot | High memory footprint; JVM startup time; less idiomatic for crypto tooling |

## Consequences

### Positive

- Single language across contracts and backend reduces cognitive overhead for contributors.
- Rust's ownership model prevents data races and many classes of security vulnerabilities by default.
- Axum is async-native and performs well under concurrent load (WebSocket, webhook delivery).
- Shared type definitions between backend and contracts are possible without an FFI boundary.

### Negative

- Rust has a longer compile time than interpreted languages, slowing the inner development loop.
- Hiring Rust engineers is harder than hiring TypeScript or Go engineers.
- Some crates in the ecosystem are less mature than equivalents in older ecosystems.

### Neutral

- The TypeScript client (`clients/typescript/`) provides an SDK for frontend consumers without requiring Rust knowledge.
- Backend is packaged as a Docker image (`backend/Dockerfile`) for consistent deployments.

## Related ADRs

- [ADR-0002](0002-passkey-authentication.md) — Passkey/WebAuthn implementation lives in the backend
- [ADR-0005](0005-postgresql-backend-storage.md) — Storage backend used by this service
