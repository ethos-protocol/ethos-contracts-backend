# Duplicate Vault Prevention

## Overview

Ethos-Protocol prevents accidental creation of duplicate vaults — vaults with identical `(owner, beneficiary, check_in_interval)` parameters — by maintaining an on-chain fingerprint registry. A second `create_vault` call with the same triple is rejected with `DuplicateVault` (error 57) as long as the original vault is still active (`Locked`).

## How It Works

On every successful `create_vault`:

1. A SHA-256 fingerprint is computed over `owner || beneficiary || check_in_interval`.
2. The fingerprint is stored under `DataKey::VaultFingerprint(hash)` mapping to the existing `vault_id`.
3. If a fingerprint already exists when `create_vault` is called, a `dup_vlt` event is emitted (carrying the conflicting `vault_id`) and the call panics with `DuplicateVault`.

The fingerprint is removed when the vault leaves the `Locked` state:
- `cancel_vault` — owner cancels the vault
- `trigger_release` — vault expires and funds are released

This means the same parameters can be reused once the original vault is no longer active.

## Fingerprint Key

```
fingerprint = sha256(owner.to_xdr() || beneficiary.to_xdr() || check_in_interval_be_bytes)
```

Stored as `DataKey::VaultFingerprint(BytesN<32>)` → `vault_id: u64`.

## What Counts as a Duplicate

Two vaults are duplicates if and only if all three of these match:

| Field | Must match |
|---|---|
| `owner` | ✅ |
| `beneficiary` | ✅ |
| `check_in_interval` | ✅ |

Changing any one of these creates a distinct vault and is allowed.

## Error

```rust
ContractError::DuplicateVault = 57
```

## Event

A `dup_vlt` event is emitted **before** the panic so off-chain indexers can observe the attempt:

| Topic | Payload |
|---|---|
| `dup_vlt` | `(owner, beneficiary, check_in_interval, existing_vault_id)` |

## Example

```rust
// First vault — succeeds
let id = client.create_vault(&owner, &beneficiary, &3600)?;

// Exact same params — rejected
client.create_vault(&owner, &beneficiary, &3600)?; // DuplicateVault (57)

// Different interval — allowed
client.create_vault(&owner, &beneficiary, &7200)?; // OK

// After cancelling the original:
client.cancel_vault(&id, &owner)?;
client.create_vault(&owner, &beneficiary, &3600)?; // OK — fingerprint was cleared
```

## Concurrency Guarantee

Soroban contracts execute on a single-threaded, serialised ledger. Every
`create_vault` call is processed atomically — there is no window between the
fingerprint check and the fingerprint write in which a second call could slip
through.

### Same-block ordering

When multiple transactions in the same ledger block target the same
`(owner, beneficiary, check_in_interval)` triple, the validator applies them in
a deterministic sequential order. The first transaction to reach the fingerprint
check finds no existing entry and succeeds; all later transactions in that block
find the fingerprint written by the first and are rejected with `DuplicateVault`.

**Guarantee**: regardless of how rapidly (or simultaneously) creation requests
arrive, **exactly one vault** will be created for any given triple as long as
the original is still active.

### Guard properties

| Property | Behaviour |
|---|---|
| Race-free | The persistent-storage check-then-write is atomic within a single transaction |
| Deterministic | Second call always fails, even at identical ledger timestamp |
| Lifecycle-aware | Fingerprint is cleared on `cancel_vault` and `trigger_release` |
| Scoped correctly | Guard key is `(owner, beneficiary, interval)` — different owners or intervals are independent |

### What changes bypass the guard (by design)

Any of the three fields being different creates a **distinct** triple:

```rust
// Only interval differs — both succeed
client.create_vault(&owner, &beneficiary, &3600)?; // OK
client.create_vault(&owner, &beneficiary, &7200)?; // OK (different interval)

// Only owner differs — both succeed
client.create_vault(&owner_a, &beneficiary, &3600)?; // OK
client.create_vault(&owner_b, &beneficiary, &3600)?; // OK (different owner)
```

## Test Coverage

`contracts/ttl_vault/src/duplicate_vault_concurrency_tests.rs` contains the
following scenarios:

| Test | What it validates |
|---|---|
| `test_concurrent_same_params_only_first_succeeds` | N rapid calls with identical params → exactly one succeeds, N−1 fail with `DuplicateVault`; vault ID counter does not advance for rejected calls |
| `test_same_block_ordering_deterministically_rejects_second_call` | Two calls at identical ledger timestamp → second is rejected without any timestamp advancement |
| `test_colliding_near_simultaneous_calls_different_params_both_succeed` | Same owner+interval, different beneficiary → both succeed (guard does not cross-contaminate) |
| `test_different_interval_same_owner_beneficiary_is_not_duplicate` | Same owner+beneficiary, different interval → both succeed; original fingerprint still active |
| `test_fingerprint_cleared_after_cancel_allows_recreation` | cancel_vault → fingerprint removed → same triple accepted again |
| `test_fingerprint_cleared_after_release_allows_recreation` | trigger_release → fingerprint removed → same triple accepted again |
| `test_multiple_owners_same_beneficiary_interval_all_succeed` | Three different owners, same beneficiary+interval → all three succeed independently |
