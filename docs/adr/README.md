# Architecture Decision Records (ADRs)

This directory contains Architecture Decision Records for Ethos-Protocol. ADRs document the significant design decisions made during the project's development, the context that led to each decision, and the trade-offs accepted.

## What is an ADR?

An ADR is a short document that captures an important architectural decision, the context around it, and its consequences. ADRs are immutable history — once accepted, they are not edited (except to update their status). If a decision changes, a new ADR is created and the old one is marked as superseded.

## Status values

| Status | Meaning |
|---|---|
| **Proposed** | Under discussion, not yet decided |
| **Accepted** | Decision made and in effect |
| **Deprecated** | No longer relevant but not actively superseded |
| **Superseded** | Replaced by a newer ADR (link provided) |

## Index

| ADR | Title | Status | Date |
|---|---|---|---|
| [ADR-0000](0000-adr-template.md) | ADR Template | — | — |
| [ADR-0001](0001-stellar-soroban-platform.md) | Use Stellar/Soroban for Smart Contract Layer | Accepted | 2024-01-15 |
| [ADR-0002](0002-passkey-authentication.md) | Use Passkeys (WebAuthn) for Owner Authentication | Accepted | 2024-02-01 |
| [ADR-0003](0003-ttl-based-release.md) | TTL-Based Automatic Release Mechanism | Accepted | 2024-02-15 |
| [ADR-0004](0004-rust-backend.md) | Rust for the Backend Service | Accepted | 2024-03-01 |
| [ADR-0005](0005-postgresql-backend-storage.md) | PostgreSQL for Backend Storage | Accepted | 2024-03-10 |

## Creating a new ADR

1. Copy `0000-adr-template.md` to a new file: `NNNN-short-title.md` (next sequential number).
2. Fill in all sections.
3. Set status to **Proposed** and open a PR for discussion.
4. Once consensus is reached, update status to **Accepted** and add it to the index above.
5. Link it from relevant code or documentation where appropriate.
