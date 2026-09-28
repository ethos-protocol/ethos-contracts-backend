# ADR-0002: Use Passkeys (WebAuthn) for Owner Authentication

**Date:** 2024-02-01  
**Status:** Accepted  
**Deciders:** Core team

---

## Context

Vault owners must authenticate to perform privileged actions (check-in, deposit, withdraw, update beneficiary). The authentication mechanism is the single most important security control in the system — a compromised credential means a compromised vault.

Traditional crypto wallets rely on seed phrases (BIP-39 mnemonics). Seed phrases are:

- A single point of failure: lose the phrase, lose the funds permanently.
- A phishing target: users can be tricked into entering them on malicious sites.
- Operationally risky: users write them down, store them insecurely, or share them.

Ethos-Protocol targets non-technical users who want digital legacy planning without seed phrase complexity.

## Decision

We will use Passkeys (WebAuthn / FIDO2) as the exclusive authentication mechanism for vault owner actions. Seed phrase fallback will not be provided in the production interface.

## Alternatives Considered

| Alternative | Reason rejected |
|---|---|
| BIP-39 seed phrases | Single point of failure; phishing risk; poor UX for non-technical users |
| Hardware wallets (Ledger, Trezor) | Requires separate device purchase; still exposes a seed phrase for recovery |
| Email + password | Centralised; phishing risk; password reuse is endemic |
| Multi-party computation (MPC) wallet | Higher implementation complexity; trust assumptions shift to MPC provider |
| Social recovery (smart account) | Good UX but adds on-chain complexity and relies on trusted guardians |

## Consequences

### Positive

- Passkeys are phishing-resistant by design — credentials are bound to a specific origin.
- No seed phrases stored or transmitted; eliminates the most common crypto asset loss vector.
- OS/browser vendors (Apple, Google, Microsoft) manage secure key storage, offloading HSM-grade security to commodity devices.
- Aligns with W3C and FIDO Alliance standards; broad browser and OS support.

### Negative

- Users must manage Passkey devices — losing all registered devices requires a recovery flow.
- Passkey portability across ecosystems (Apple ↔ Android) has historically been limited (improving with passkey syncing standards).
- Backend must implement WebAuthn credential storage and verification (`backend/src/webauthn.rs`).
- Auditors must be familiar with WebAuthn in addition to Soroban.

### Neutral

- At least two Passkey devices should be registered per vault owner (see [`docs/security-best-practices.md`](../security-best-practices.md)).
- Recovery flows must be documented and tested; see [`docs/webauthn-setup.md`](../webauthn-setup.md).

## Related ADRs

- [ADR-0001](0001-stellar-soroban-platform.md) — Stellar account model that Passkeys authenticate against
- [ADR-0004](0004-rust-backend.md) — Backend technology that implements WebAuthn credential management
