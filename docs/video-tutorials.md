# Video Tutorials for Key Features

This document describes the video tutorial series for Ethos-Protocol. Each tutorial maps to a specific feature or workflow. Tutorials are organized by audience level and include outlines, key talking points, and links to the matching written documentation.

## Table of Contents

- [Overview](#overview)
- [Tutorial Conventions](#tutorial-conventions)
- [Series 1: Getting Started](#series-1-getting-started)
- [Series 2: Core Vault Flows](#series-2-core-vault-flows)
- [Series 3: Beneficiary Features](#series-3-beneficiary-features)
- [Series 4: Authentication and Passkeys](#series-4-authentication-and-passkeys)
- [Series 5: Configuration Tutorials](#series-5-configuration-tutorials)
- [Series 6: Troubleshooting Videos](#series-6-troubleshooting-videos)
- [Tutorial Production Process](#tutorial-production-process)
- [Contributing a Tutorial](#contributing-a-tutorial)

---

## Overview

Text documentation is valuable for reference, but visual, step-by-step walkthroughs lower the barrier for new contributors and users. This tutorial series covers:

- **Issuance** — creating and funding a vault.
- **Check-in (Attestation)** — proving liveness to extend TTL.
- **Verification** — confirming vault state and release eligibility.
- **Configuration** — environment setup, deployment, network selection.
- **Troubleshooting** — common errors and how to fix them.

Each tutorial is self-contained. Viewers can watch a single episode without having seen the others.

---

## Tutorial Conventions

All tutorials follow a consistent format:

| Element | Standard |
|---|---|
| **Length** | 5–15 minutes per episode |
| **Resolution** | 1080p minimum |
| **Audio** | Clear voiceover; no background music during code sections |
| **Code font** | Fira Code or JetBrains Mono at 16pt minimum |
| **Terminal theme** | Dark background, high-contrast text |
| **Chapters** | Timestamps in video description for each major step |
| **Captions** | Auto-generated + manual review for accuracy |
| **Code samples** | Always shown in full; no partial pastes without explanation |
| **Environment** | Testnet for all tutorials unless explicitly stated otherwise |

---

## Series 1: Getting Started

### T-101 · Environment Setup and Local Development

**Audience**: Developers new to Ethos-Protocol  
**Duration**: ~12 minutes  
**Written reference**: [README.md Quick Start](../README.md#-quick-start), [docs/deployment-guide.md](deployment-guide.md)
**Last verified**: 2026-08-29 — Verified — matches current README Quick Start and deployment guide.

**Outline**:

1. Prerequisites: Rust 1.70+, Soroban CLI, Stellar CLI, Docker
2. Clone the repository
3. Copy `.env.example` to `.env` and configure key variables
4. Run `docker-compose up -d` and verify all three services are healthy
5. Run `cargo test` to confirm everything is working
6. Tour of the project structure: `contracts/`, `backend/`, `docs/`, `scripts/`

**Key talking points**:
- Why Docker Compose is the fastest local setup path
- The role of each service (PostgreSQL, backend, Stellar Quickstart)
- What the `.env` variables control and which ones are required immediately

---

### T-102 · Deploying to Testnet

**Audience**: Developers ready for their first on-chain deployment  
**Duration**: ~10 minutes  
**Written reference**: [docs/deployment-guide.md](deployment-guide.md)
**Last verified**: 2026-08-29 — Verified — deploy_testnet.sh and environments.toml match current scripts.

**Outline**:

1. Generate a testnet identity: `stellar keys generate deployer --network testnet`
2. Fund the account using Friendbot
3. Run `./scripts/deploy_testnet.sh`
4. Copy the deployed contract address into `.env`
5. Invoke a test function using Stellar CLI to confirm deployment
6. View contract state using Stellar Laboratory or Horizon

**Key talking points**:
- Difference between testnet and standalone for development
- How `environments.toml` selects the right RPC endpoint
- What to do if the deployment script fails

---

## Series 2: Core Vault Flows

### T-201 · Creating Your First Vault (Issuance)

**Audience**: End users and developers  
**Duration**: ~8 minutes  
**Written reference**: [README.md Smart Contract API](../README.md#-smart-contract-api)
**Last verified**: 2026-02-14 — NEEDS RE-RECORDING — create_vault signature audited against contracts/ttl_vault/src/lib.rs; confirm beneficiary/check_in_interval ordering still matches.

**Outline**:

1. What a vault is and what parameters it requires
2. Choosing an appropriate `check_in_interval` (seconds)
3. Calling `create_vault(beneficiary, check_in_interval)`
4. Recording the returned `vault_id`
5. Calling `get_vault(vault_id)` to inspect the initial state
6. Making the first deposit: `deposit(vault_id, amount)`

**Key talking points**:
- Why `owner == beneficiary` is rejected
- Amount units: stroops (1 XLM = 10,000,000 stroops)
- How the TTL countdown starts on creation

---

### T-202 · Performing a Check-In (Attestation)

**Audience**: End users  
**Duration**: ~6 minutes  
**Written reference**: [docs/ttl-logic.md](ttl-logic.md)
**Last verified**: 2026-08-29 — Verified — check_in and get_ttl_remaining signatures current.

**Outline**:

1. What happens without a check-in (TTL expiry and release)
2. Calling `check_in(vault_id)`
3. Calling `get_ttl_remaining(vault_id)` before and after check-in
4. The check-in cooldown: what `CheckInTooFrequent` means and how to avoid it
5. Optional: geo check-in with `check_in_with_geo`

**Key talking points**:
- Setting up a reminder so you never miss a check-in
- The rate limiter default (60 seconds) and how to configure it
- Geographic metadata privacy considerations

---

### T-203 · Triggering Vault Release (Verification)

**Audience**: Beneficiaries and developers  
**Duration**: ~8 minutes  
**Written reference**: [docs/ttl-logic.md](ttl-logic.md), [docs/beneficiary-conditional-acceptance.md](beneficiary-conditional-acceptance.md)
**Last verified**: 2026-08-29 — Verified — trigger_release/get_release_status flow current.

**Outline**:

1. Conditions required for release: TTL expired, vault not already released
2. Calling `is_expired(vault_id)` to check eligibility
3. Calling `trigger_release(vault_id)`
4. Observing the fund transfer on Stellar explorer
5. Calling `get_release_status(vault_id)` to confirm completion
6. What happens with archived vaults: automatic restoration during release

**Key talking points**:
- Anyone can call `trigger_release`, not just the beneficiary
- What `ContractError::NotExpired` means and how to verify TTL
- Viewing the transaction on [Stellar Expert](https://stellar.expert)

---

### T-204 · Withdrawing Funds

**Audience**: Vault owners  
**Duration**: ~7 minutes  
**Written reference**: [docs/withdrawal-features.md](withdrawal-features.md)
**Last verified**: 2026-02-14 — NEEDS RE-RECORDING — withdrawal batching/dispute flow should be re-checked against docs/withdrawal-features.md for signature drift.

**Outline**:

1. Prerequisites: vault must be active (not expired, not released)
2. Calling `withdraw(vault_id, amount)`
3. The withdrawal audit trail: how every attempt is logged
4. Withdrawal notifications: real-time alerts for all withdrawal events
5. The 24-hour dispute window for unauthorized withdrawals
6. Withdrawal batching for multiple small amounts

**Key talking points**:
- Why withdrawals are logged even when they fail
- How to batch withdrawals efficiently
- How to open a dispute

---

## Series 3: Beneficiary Features

### T-301 · Conditional Acceptance and Minimum Thresholds

**Audience**: Advanced users  
**Duration**: ~7 minutes  
**Written reference**: [docs/beneficiary-conditional-acceptance.md](beneficiary-conditional-acceptance.md), [docs/beneficiary-minimum-threshold.md](beneficiary-minimum-threshold.md)
**Last verified**: 2026-08-29 — Verified — conditional acceptance and threshold docs current.

**Outline**:

1. What conditional acceptance means for beneficiaries
2. Setting a minimum fund threshold for acceptance
3. The beneficiary floor and cap concepts
4. Walk-through: creating a vault where beneficiary must accept when funds exceed 100 XLM

**Key talking points**:
- Protecting beneficiaries from inheriting underfunded vaults
- The difference between floor, threshold, and cap
- What happens if conditions are never met

---

### T-302 · Conflict Resolution Between Multiple Beneficiaries

**Audience**: Advanced users and legal-tech enthusiasts  
**Duration**: ~9 minutes  
**Written reference**: [docs/beneficiary-conflict-resolution.md](beneficiary-conflict-resolution.md)
**Last verified**: 2026-08-29 — Verified — conflict resolution flow current.

**Outline**:

1. Scenario: multiple parties claim the same vault
2. How the automated conflict resolution algorithm works
3. The beneficiary ranking system
4. The beneficiary auction mechanism
5. Final resolution: who gets paid and how much

**Key talking points**:
- No human intermediary is involved — all on-chain
- How ranking scores are calculated
- What beneficiaries need to do to maximize their claim

---

### T-303 · Beneficiary Delegation

**Audience**: Beneficiaries  
**Duration**: ~5 minutes  
**Written reference**: [docs/beneficiary-advanced-features.md](beneficiary-advanced-features.md)
**Last verified**: 2026-08-29 — Verified — delegate_beneficiary_role signature current.

**Outline**:

1. Why delegation exists (beneficiary cannot act at time of release)
2. Calling `delegate_beneficiary_role(vault_id, delegate_address)`
3. The delegation chain: how authority passes down
4. The `del_ben` event and how to monitor it

---

## Series 4: Authentication and Passkeys

### T-401 · Passkey Setup and Biometric Check-In

**Audience**: End users  
**Duration**: ~10 minutes  
**Written reference**: [docs/passkeys.md](passkeys.md)
**Last verified**: 2026-01-10 — NEEDS RE-RECORDING — describes WebAuthn as 'planned for v2.0'; re-verify against current passkeys.md#current-status before next publish.

**Outline**:

1. What Passkeys are and why Ethos uses them instead of seed phrases
2. Current status: WebAuthn planned for v2.0; current auth via Stellar address
3. Registering a biometric credential: `bind_passkey_biometric`
4. Performing a biometric check-in: `biometric_check_in`
5. Listing registered biometrics: `get_vault_biometrics`
6. Removing a credential: `unbind_passkey_biometric`

**Key talking points**:
- Raw biometric data never leaves your device — only a SHA-256 hash is stored
- Multiple credentials per vault (fingerprint + face ID)
- Phishing resistance: why WebAuthn is safer than passwords or seed phrases

---

### T-402 · Passkey Expiry and Compromise Response

**Audience**: Security-conscious users  
**Duration**: ~8 minutes  
**Written reference**: [docs/passkeys.md](passkeys.md#passkey-expiry-enforcement-issue-549)
**Last verified**: 2026-01-10 — NEEDS RE-RECORDING — passkey expiry/compromise error codes should be re-confirmed against contracts/ttl_vault/src/lib.rs enum.

**Outline**:

1. Setting a passkey expiry with `extend_passkey_expiry`
2. What `PasskeyExpired` (error 59) looks like and how to recover
3. Reporting a compromise: `report_passkey_compromise`
4. Automatic compromise detection: the 3-consecutive-different-hash heuristic
5. Clearing a compromise flag: `clear_passkey_compromise`
6. The `pk_expd` and `pk_comp` events and how to monitor them

---

## Series 5: Configuration Tutorials

### T-501 · Deploying to Mainnet Safely

**Audience**: Operators deploying production vaults  
**Duration**: ~12 minutes  
**Written reference**: [docs/deployment-guide.md](deployment-guide.md)
**Last verified**: 2026-08-29 — Verified — mainnet deployment steps current.

**Outline**:

1. Generate a mainnet identity: `stellar keys generate deployer-mainnet --network mainnet`
2. Set `STELLAR_MAINNET_RPC_URL`
3. Review `environments.toml` for mainnet settings
4. Run `./scripts/deploy_mainnet.sh` — why it asks you to type `mainnet`
5. Post-deployment: update `.env` with contract address, verify on Stellar Expert
6. Setting up monitoring: [docs/monitoring-guide.md](monitoring-guide.md)

**Key talking points**:
- Never share your mainnet signing key
- Test thoroughly on testnet first — mainnet mistakes cost real funds
- The importance of the WASM size budget: [docs/wasm-size-budget.md](wasm-size-budget.md)

---

### T-502 · Configuring the Reminder and Notification System

**Audience**: Operators  
**Duration**: ~8 minutes  
**Written reference**: [docs/push-notifications.md](push-notifications.md), [docs/backend-api.md](backend-api.md)
**Last verified**: 2026-01-10 — NEEDS RE-RECORDING — reminder/notification env vars should be re-checked against docs/push-notifications.md and docs/backend-api.md.

**Outline**:

1. Setting up email reminders: `REMINDER_EMAIL_API_KEY`
2. Setting up SMS reminders: `REMINDER_SMS_API_KEY`
3. WebSocket real-time alerts for withdrawal and TTL events
4. Webhook delivery for external integrations
5. Configuring the scheduler for reminder frequency

---

### T-503 · Vesting Schedules and Token Management

**Audience**: Advanced users and protocol integrators  
**Duration**: ~10 minutes  
**Written reference**: [docs/vesting-schedules.md](vesting-schedules.md), [docs/token-management.md](token-management.md)
**Last verified**: 2026-08-29 — Verified — vesting/token management references current.

**Outline**:

1. What vesting schedules are and why you might use them
2. Configuring a vesting schedule on a vault
3. Token management: native XLM vs. custom Stellar tokens
4. How token support will expand in v1.1

---

## Series 6: Troubleshooting Videos

### T-601 · Diagnosing Contract Errors

**Audience**: Developers  
**Duration**: ~10 minutes  
**Written reference**: [docs/faq.md](faq.md#smart-contract-errors)
**Last verified**: 2026-08-29 — Verified — error table matches docs/faq.md#smart-contract-errors.

**Outline**:

1. How to read a Soroban `HostError` response
2. The most common errors: `NotExpired`, `InvalidPasskey`, `CheckInTooFrequent`
3. Using `get_vault`, `get_ttl_remaining`, and `is_expired` to diagnose state
4. Reading on-chain events for `pk_expd`, `pk_comp`, `ci_rl`
5. Escalation: what to log before opening a GitHub issue

---

### T-602 · Local Environment Issues

**Audience**: Developers  
**Duration**: ~8 minutes  
**Written reference**: [docs/faq.md](faq.md#troubleshooting-common-issues)
**Last verified**: 2026-08-29 — Verified — local environment troubleshooting steps current.

**Outline**:

1. Docker Compose containers not starting: port conflicts, volume issues
2. `wasm-opt not found`: installing `binaryen`
3. Stellar CLI network errors: verifying RPC URLs
4. PostgreSQL connection refused: `.env` mismatch, health check failures
5. Resetting local state completely: `docker-compose down -v`

---

### T-603 · Disaster Recovery

**Audience**: Operators  
**Duration**: ~12 minutes  
**Written reference**: [docs/disaster-recovery-runbook.md](disaster-recovery-runbook.md)
**Last verified**: 2026-08-29 — Verified — disaster recovery steps current.

**Outline**:

1. Scenario: contract state archived unexpectedly
2. Calling `restore_vault(vault_id)` manually
3. Re-deploying a contract if the WASM is unavailable
4. Database recovery from PostgreSQL backup
5. Key rotation after a suspected compromise

---

## Audit Findings (2026-08-29)

Each tutorial above now carries a **Last verified** field recording the date
its outline was last checked against the current API/contract function
signatures and the corresponding written docs. The following tutorials were
flagged during this audit and need re-recording (outline and/or on-screen
code no longer guaranteed to match current signatures):

| Tutorial | Reason | Follow-up |
|---|---|---|
| T-201 | Verify `create_vault(beneficiary, check_in_interval)` argument order against `contracts/ttl_vault/src/lib.rs` | File follow-up issue: re-verify + re-record if signature drifted |
| T-204 | Withdrawal batching/dispute flow may have drifted from `docs/withdrawal-features.md` | File follow-up issue: audit withdrawal batching API |
| T-401 | References WebAuthn as "planned for v2.0"; confirm current status hasn't shipped | File follow-up issue: re-check `docs/passkeys.md#current-status` |
| T-402 | Passkey expiry/compromise error codes need re-confirmation against the `ContractError` enum | File follow-up issue: verify error codes 59/62 still match |
| T-502 | Reminder/notification env var names should be re-checked | File follow-up issue: audit against `docs/push-notifications.md` |

All other tutorials (T-101, T-102, T-202, T-203, T-301, T-302, T-303, T-501,
T-503, T-601, T-602, T-603) were verified current as of 2026-08-29 and
require no action.

**Process**: Follow-up items for flagged tutorials should be filed as
GitHub issues labeled `documentation` + `video-tutorials`, referencing the
tutorial ID (e.g. `T-401`) in the title.

---

## Tutorial Production Process

When producing a new tutorial:

1. **Write the script** — follow the outline format above. Full sentences, not bullet notes.
2. **Record in stages** — record each section separately; assemble in post.
3. **Screen capture** — use a clean terminal with the theme defined in [Tutorial Conventions](#tutorial-conventions).
4. **Voiceover** — record in a quiet room; review for accuracy against the current codebase.
5. **Add timestamps** — include chapter markers matching the outline headings.
6. **Generate captions** — auto-generate, then manually review for technical term accuracy.
7. **Link from this file** — add the video URL to the relevant tutorial entry above.
8. **Cross-link in written docs** — add a "Video Walkthrough" link in the corresponding `.md` file.

### Keeping Tutorials Current

Each tutorial entry lists the written docs it is based on. When those docs change:

1. Review the tutorial for outdated steps.
2. Record an updated segment for changed sections only (avoid re-recording the whole video).
3. Pin a note in the video description: `"Updated section at 3:42 for v1.1 token support."`
4. Update the outline in this file to reflect the current state.

---

## Contributing a Tutorial

Community-contributed tutorials are welcome. Before recording:

1. Open an issue in the repository describing the tutorial topic.
2. Confirm there is no existing tutorial covering the same flow.
3. Follow the [Tutorial Conventions](#tutorial-conventions) and [Production Process](#tutorial-production-process).
4. Submit the outline in your PR alongside the video link.

See [CONTRIBUTING.md](../CONTRIBUTING.md) for general contribution guidelines.
