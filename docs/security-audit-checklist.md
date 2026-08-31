# Security Audit Checklist

Use this checklist before every release and as a guide for external auditors. Each item should be marked ✅ (pass), ❌ (fail), or N/A.

Related documents: [Threat Model & Security](security.md) · [Security Policy](../SECURITY.md)

## Automation Status Legend

| Symbol | Meaning |
|--------|---------|
| 🤖 CI-gated | Enforced automatically by a required CI check; a failing build blocks merge |
| 🧩 Partial | A lint/static-analysis tool catches part of the item, but manual review is still required for full coverage |
| 🧍 Manual-only | No practical static-analysis equivalent exists; must be verified by human review |

The automated checks referenced below are wired into `.github/workflows/security.yml` as required gates (job `security-lints`). See that workflow for exact invocations.

---

## 1. Authentication & Authorization

- [ ] 🧩 Every owner action calls `owner.require_auth()` — partially caught by `cargo clippy -- -W clippy::missing_auth` custom lint config + grep-based CI check for `require_auth` presence per public fn; false negatives possible for conditionally-skipped auth
- [ ] 🧩 Every admin action calls `admin.require_auth()` — same tooling as above
- [ ] 🧍 `initialize()` rejects a second call (`AlreadyInitialized`) — requires manual trace of storage flag logic
- [ ] 🧍 `propose_admin` / `accept_admin` two-step transfer is enforced
- [ ] 🧍 Passkey hash is validated before accepting a check-in
- [ ] 🧍 Backup codes are single-use and marked `used = true` after consumption
- [ ] 🧍 Beneficiary cannot trigger release before TTL expiry

## 2. Reentrancy

- [ ] 🧩 All state mutations (balance, status) are written **before** token transfers — `clippy::disallowed_methods` config flags any `token::Client::transfer` call not preceded by a state write in the same function scope (best-effort heuristic, not a full data-flow check)
- [ ] 🧍 `trigger_release` sets `vault.status = Released` before calling `token.transfer`
- [ ] 🧍 `claim_vested_installment` decrements balance before transferring
- [ ] 🧍 No external calls are made between reading and writing vault state

## 3. Integer Arithmetic

- [ ] 🤖 All balance additions use `checked_add` or `saturating_add` to prevent overflow — `overflow-checks = true` is set in `Cargo.toml` release profile and `clippy::arithmetic_side_effects` is a **CI-gated deny** lint
- [ ] 🧍 BPS distribution sums to exactly 10 000 before saving beneficiaries
- [ ] 🧍 Last-beneficiary rounding absorbs remainder (no dust left in vault)
- [ ] 🤖 `vault_ttl_ledgers` uses `saturating_mul` / `saturating_div` — covered by the same `clippy::arithmetic_side_effects` gate
- [ ] 🧍 Vesting `per_installment` calculation handles zero `num_installments`

## 4. TTL Management

- [ ] 🧍 `save_vault` always calls `extend_ttl` with the correct ledger count
- [ ] 🧍 `check_in` rejects if the new deadline would exceed `max_ttl_seconds`
- [ ] 🧍 `create_vault` sets TTL proportional to `check_in_interval` (2× buffer)
- [ ] 🧍 Instance storage TTL is extended on every state-mutating call
- [ ] 🧍 `ping_expiry` emits a warning event when TTL < `EXPIRY_WARNING_THRESHOLD`
- [ ] 🧍 Archived vault state can be restored via `restore_vault` before `trigger_release`

## 5. Access Control — Vault Operations

- [ ] 🧩 `deposit` / `withdraw` reject if vault is paused or released — CI check greps for `assert_not_paused` call at top of each `pub fn` in modified files
- [ ] 🧍 `withdraw` enforces `vault.balance >= amount`
- [ ] 🧍 `update_beneficiary` rejects `owner == new_beneficiary`
- [ ] 🧍 `set_beneficiaries` rejects owner appearing in the list
- [ ] 🧍 `cancel_vault` is owner-only and only allowed while `Locked`
- [ ] 🧍 `pause_vault` / `resume_vault` are owner-only

## 6. Contract-Level Pause

- [ ] 🧩 `assert_not_paused` is called at the top of every state-mutating function — same grep-based CI check as section 5
- [ ] 🧍 Paused state blocks `deposit`, `withdraw`, `check_in`, `trigger_release`
- [ ] 🧍 Admin cannot access or redirect vault funds while paused
- [ ] 🧍 Unpause restores full functionality without data loss

## 7. Soroban-Specific Checks

- [ ] 🤖 No `panic!` / `unwrap` in production paths — all errors use `panic_with_error!` — **CI-gated**: `clippy::unwrap_used` and `clippy::panic` are set to `deny` in `contracts/*/src/lib.rs` lint attributes and checked via `cargo clippy -- -D clippy::unwrap_used -D clippy::panic`
- [ ] 🧍 `load_vault` panics with `VaultNotFound` rather than returning a default
- [ ] 🧍 Persistent storage keys are unique per vault ID (no key collisions)
- [ ] 🧍 `MAX_METADATA_LEN`, `MAX_CUSTOM_METADATA_LEN` are enforced before storage writes
- [ ] 🧍 Host function budget (CPU / memory) is not exhausted in worst-case loops
- [ ] 🧍 Ledger entry size limits are respected for `Vec<BeneficiaryEntry>` and metadata

## 8. Token Handling

- [ ] 🧍 Only whitelisted token addresses are accepted in `create_vault`
- [ ] 🤖 `token.transfer` return value is not silently ignored — **CI-gated**: `clippy::must_use_candidate` + `#[must_use]` annotations plus `-D unused_must_use` catch ignored `Result`/return values
- [ ] 🧍 Contract never holds more balance than the sum of all vault balances
- [ ] 🧍 XLM token address is validated at `initialize` time

## 9. Beneficiary & Vesting

- [ ] 🧍 Vesting schedule `total_amount` matches vault balance at schedule creation
- [ ] 🧍 `claim_vested_installment` is only callable after `trigger_release`
- [ ] 🤖 Installment index cannot overflow `u32` — covered by `clippy::arithmetic_side_effects` gate (section 3)
- [ ] 🧍 Declined beneficiary blocks `trigger_release` (`InvalidBeneficiary`)
- [ ] 🧍 Dispute status `Filed` blocks release until resolved

## 10. Upgrade & Versioning

- [ ] 🧍 Contract version is stored and readable via `get_contract_version`
- [ ] 🧍 Any upgrade path preserves existing vault data layout
- [ ] 🧍 Breaking storage key changes are documented and migration tested

---

## Automation Coverage Summary

| Category | 🤖 CI-gated | 🧩 Partial | 🧍 Manual-only |
|----------|------------|-----------|---------------|
| Auth & Authorization | 0 | 2 | 5 |
| Reentrancy | 0 | 1 | 3 |
| Integer Arithmetic | 2 | 0 | 3 |
| TTL Management | 0 | 0 | 6 |
| Access Control | 0 | 1 | 5 |
| Contract-Level Pause | 0 | 1 | 3 |
| Soroban-Specific | 1 | 0 | 5 |
| Token Handling | 1 | 0 | 3 |
| Beneficiary & Vesting | 1 | 0 | 4 |
| Upgrade & Versioning | 0 | 0 | 3 |
| **Total** | **5** | **5** | **40** |

Manual-only items remain the majority of the checklist. As lint coverage matures (custom clippy lints, data-flow analysis), items should be re-classified from 🧍 to 🧩 or 🤖 and this table updated accordingly.

## Audit Sign-Off

| Auditor | Date | Findings | Status |
|---------|------|----------|--------|
| (internal review) | | | Pending |
| (external auditor) | | | Not started |

> **Note**: No mainnet deployment should occur without a completed external audit. See [SECURITY.md](../SECURITY.md) for the full security policy.
