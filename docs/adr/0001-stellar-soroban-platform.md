# ADR-0001: Use Stellar/Soroban for Smart Contract Layer

**Date:** 2024-01-15  
**Status:** Accepted  
**Deciders:** Core team

---

## Context

Ethos-Protocol requires a programmable smart contract layer to enforce vault TTL logic, manage fund custody, and trigger automatic beneficiary releases without a trusted intermediary. The choice of blockchain platform determines the programming model, cost structure, security model, and developer toolchain for the entire project.

Key requirements:

- Deterministic TTL / time-based logic with native ledger timestamps
- State archival that maps naturally to "dead man's switch" semantics
- Low transaction fees suitable for periodic check-ins
- A memory-safe, auditable contract language
- Active ecosystem and tooling support

## Decision

We will use Stellar's Soroban smart contract platform with Rust as the contract language.

## Alternatives Considered

| Alternative | Reason rejected |
|---|---|
| Ethereum / Solidity | High gas costs make frequent check-ins expensive; Solidity's memory model increases audit surface; no native state archival/TTL primitives |
| Ethereum / EVM with L2 | L2 bridges introduce additional trust assumptions; native TTL still absent |
| Algorand | Smaller developer ecosystem; TEAL contract language is lower-level and harder to audit |
| Cosmos (CosmWasm) | Rust support is good, but no native TTL/archival primitive — would require off-chain cron jobs |
| Solana | Rust-based, but account model is complex; no built-in TTL; high operational complexity |

## Consequences

### Positive

- Soroban's State Archival and TTL feature maps directly to the dead man's switch concept — no off-chain cron job needed to trigger release.
- Rust provides memory safety and strong typing, reducing common smart contract vulnerability classes.
- Low, predictable fees make periodic check-ins economical for end users.
- Passkey / WebAuthn authentication aligns with Stellar's account model.

### Negative

- Soroban is a relatively young platform; tooling and ecosystem are less mature than Ethereum.
- Rust has a steeper learning curve than Solidity for contributors unfamiliar with the language.
- Smaller auditor pool familiar with Soroban compared to EVM.

### Neutral

- All contract code must target `wasm32-unknown-unknown`; standard library features requiring OS syscalls are unavailable.
- Wasm binary size must stay within the budget defined in [`docs/wasm-size-budget.md`](../wasm-size-budget.md).

## Related ADRs

- [ADR-0002](0002-passkey-authentication.md) — Authentication strategy that complements the Stellar account model
- [ADR-0003](0003-ttl-based-release.md) — TTL release mechanism enabled by this platform choice
