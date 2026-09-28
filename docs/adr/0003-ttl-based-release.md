# ADR-0003: TTL-Based Automatic Release Mechanism

**Date:** 2024-02-15  
**Status:** Accepted  
**Deciders:** Core team

---

## Context

The core protocol mechanic requires that vault funds transfer to a beneficiary automatically when the owner stops checking in. This "dead man's switch" behaviour must be:

- Trustless — no human intermediary can block or accelerate release.
- Tamper-resistant — the release condition must be verifiable on-chain.
- Recoverable — the owner must be able to reset the countdown easily while alive.

Stellar/Soroban provides a native TTL (Time to Live) mechanism tied to ledger state archival. When a contract entry's TTL reaches zero, the entry is archived; it can no longer be read or written by the contract without restoration.

## Decision

We will use Soroban's native State Archival and TTL mechanism as the primary release trigger. The vault contract stores a `last_check_in` timestamp and a `check_in_interval`. The release is triggered when `current_ledger_timestamp - last_check_in > check_in_interval`, which is equivalent to the contract's persistent entry TTL expiring.

Owner check-ins extend the TTL, resetting the countdown.

## Alternatives Considered

| Alternative | Reason rejected |
|---|---|
| Off-chain cron job / oracle | Introduces a trusted third party that can be censored or compromised |
| Block height countdown | Less intuitive for users; ledger time is more natural for "days since last check-in" |
| Multisig with time-lock | Requires beneficiary co-operation before TTL; adds UX friction |
| Zero-knowledge proof of liveness | Significantly higher implementation complexity; not necessary for MVP |

## Consequences

### Positive

- Release condition is entirely on-chain and verifiable by anyone.
- No cron job, oracle, or off-chain service required to trigger release.
- Soroban TTL extension on check-in is a single ledger operation, keeping costs low.
- The model maps directly to the intuitive concept of "if I stop checking in, release my funds."

### Negative

- Soroban archival/restoration mechanics must be understood by operators; archived entries require a fee to restore.
- If the owner's TTL lapses accidentally (e.g., extended illness without a backup device), recovery requires the admin pause mechanism and a defined recovery flow.
- The protocol cannot distinguish between owner death and owner incapacitation.

### Neutral

- The `check_in_interval` is set at vault creation and cannot be changed without creating a new vault. This immutability simplifies the security model.
- TTL logic details are documented in [`docs/ttl-logic.md`](../ttl-logic.md).

## Related ADRs

- [ADR-0001](0001-stellar-soroban-platform.md) — Stellar/Soroban platform that provides the TTL primitive
- [ADR-0005](0005-postgresql-backend-storage.md) — Off-chain index stores vault metadata for the reminder service
