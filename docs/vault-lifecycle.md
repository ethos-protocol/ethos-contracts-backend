# Vault Lifecycle Documentation

**Issue**: #577  
**Status**: Implemented  
**Last Updated**: 2026-09-28

## Overview

This document describes the complete lifecycle of an Ethos-Protocol vault — from creation through expiry and fund release. Understanding vault states and transitions is essential for building integrations and diagnosing operational issues.

## Vault States

A vault exists in one of three logical states at any point in time:

| State | Description |
|-------|-------------|
| **Active** | Vault is live. Owner can check in, deposit, withdraw, and manage settings. |
| **Expired** | TTL has elapsed since the last check-in. No new check-ins are accepted. Release can be triggered. |
| **Released** | Funds have been transferred to the beneficiary. The vault record remains on-chain for audit purposes. |

State is derived at query time from `last_check_in`, `check_in_interval`, and `ReleaseStatus` — there is no explicit status field.

```
Active  ──(TTL lapses)──►  Expired  ──(trigger_release)──►  Released
  ▲                                                              │
  └─────────────── (check_in resets TTL) ──────────────────────┘
                                                          (terminal)
```

A Released vault is terminal — it cannot be reactivated.

## State Machine Diagram

```
┌─────────────────────────────────────────────────────┐
│                       ACTIVE                        │
│                                                     │
│  last_check_in + check_in_interval > now            │
│                                                     │
│  Allowed operations:                                │
│  • check_in          • deposit                      │
│  • withdraw          • update_beneficiary           │
│  • enter_hibernation • set_vesting                  │
│  • add_passkey       • set_spending_limit           │
└───────────────────────┬─────────────────────────────┘
                        │
              now >= last_check_in
              + check_in_interval
              (TTL elapses, no check-in)
                        │
                        ▼
┌─────────────────────────────────────────────────────┐
│                      EXPIRED                        │
│                                                     │
│  last_check_in + check_in_interval <= now           │
│  ReleaseStatus::Locked                              │
│                                                     │
│  Allowed operations:                                │
│  • trigger_release   • get_vault (read-only)        │
│  • restore_vault     • get_release_status           │
└───────────────────────┬─────────────────────────────┘
                        │
                  trigger_release()
                  (transfers funds)
                        │
                        ▼
┌─────────────────────────────────────────────────────┐
│                     RELEASED                        │
│                                                     │
│  ReleaseStatus::Released                            │
│  balance == 0                                       │
│                                                     │
│  Allowed operations:                                │
│  • get_vault (read-only audit)                      │
│  • get_release_status                               │
└─────────────────────────────────────────────────────┘
```

### Hibernation Sub-state

While **Active**, a vault can enter **Hibernation**. During hibernation the TTL countdown is suspended — the vault will not expire until the owner exits hibernation and the normal interval elapses again.

```
Active ──(enter_hibernation)──► Hibernating ──(exit_hibernation)──► Active
```

The vault remains in the Active state from the perspective of `ReleaseStatus`; hibernation is tracked separately via `HibernationEntry`.

## Operations and Their Effects

### `create_vault(beneficiary, check_in_interval)`

- Creates a new vault record with `balance = 0`.
- Sets `last_check_in` to the current ledger timestamp.
- Starts the TTL countdown immediately.
- Emits `v_created`.

### `check_in(vault_id, caller, passkey_hash, nonce)`

- Resets `last_check_in` to the current ledger timestamp.
- Restarts the full `check_in_interval` countdown.
- Blocked if vault is **Expired** or **Released**.
- Subject to minimum cooldown (`get_min_checkin_cooldown`).
- Emits `check_in`.

### `deposit(vault_id, caller, amount)`

- Increases `balance` by `amount`.
- Does **not** reset the TTL (use `check_in` to extend TTL).
- Blocked if vault is **Released**.
- Emits `deposit`.

### `withdraw(vault_id, caller, amount)`

- Decreases `balance` by `amount`.
- Does **not** reset the TTL.
- Subject to spending limits and withdrawal rate limits.
- Blocked if vault is **Released**.
- Emits `withdraw`.

### `trigger_release(vault_id)`

- Requires vault to be **Expired** (`is_expired() == true`).
- Transfers the full balance to the beneficiary (or splits per BPS if multiple beneficiaries are set).
- Sets `ReleaseStatus::Released`.
- Sets `balance = 0`.
- Emits `release`.
- Callable by anyone — no auth required.

### `is_expired(vault_id) → bool`

- Returns `true` when `current_time >= last_check_in + check_in_interval`.
- Pure query; no state mutation.

### `get_release_status(vault_id) → ReleaseStatus`

- Returns `ReleaseStatus::Locked` (active/expired but not yet released) or `ReleaseStatus::Released`.

### `get_ttl_remaining(vault_id) → Option<u64>`

- Returns the number of seconds until expiry, or `None` if already expired.

## Common Flows

### Flow 1: Normal Lifecycle (Owner Checks In Regularly)

```
1. Owner calls create_vault(beneficiary, 30_days_in_seconds)
   → Vault created, TTL starts

2. Owner calls deposit(vault_id, amount)
   → balance += amount

3. Owner calls check_in(vault_id, ...) every ~25 days
   → last_check_in resets, TTL restarts

4. Owner calls withdraw(vault_id, amount) as needed
   → balance -= amount (within limits)

5. [Vault continues indefinitely as long as owner checks in]
```

### Flow 2: Owner Becomes Inactive (Release Triggered)

```
1. Vault is Active with balance > 0

2. Owner stops checking in

3. After check_in_interval seconds elapse:
   → is_expired() returns true

4. Anyone calls trigger_release(vault_id)
   → Funds transferred to beneficiary
   → ReleaseStatus set to Released

5. Beneficiary receives funds on-chain
```

### Flow 3: Planned Absence (Hibernation)

```
1. Owner calls enter_hibernation(vault_id, owner, duration_seconds)
   → Vault enters hibernation sub-state
   → TTL countdown suspended

2. Owner is away for the hibernation duration

3. Owner returns, calls exit_hibernation(vault_id, owner)
   → TTL countdown resumes from exit time

4. Owner calls check_in(...) to reset TTL
   → Normal operation continues
```

### Flow 4: Vault Archival and Restoration

```
1. All vault activity stops (owner inactive, no deposits/withdrawals)

2. Soroban archives the persistent storage entry after it expires on-chain

3. Beneficiary calls restore_vault(vault_id) to re-extend TTL
   → Vault becomes accessible again

4. Beneficiary calls trigger_release(vault_id)
   → trigger_release automatically attempts restoration before transferring funds
```

## Error Reference

| Error | State | Cause |
|-------|-------|-------|
| `NotExpired` | Active | `trigger_release` called before TTL lapses |
| `AlreadyReleased` | Released | Operation called on a released vault |
| `CheckInTooFrequent` | Active | `check_in` called within minimum cooldown window |
| `NotExpired` | Hibernating | `trigger_release` called while vault is hibernating |
| `VaultNotFound` | Any | Vault ID does not exist |

## Testing

Lifecycle integration tests are in `contracts/ttl_vault/src/lifecycle_tests.rs`:

- `test_full_lifecycle_single_beneficiary` — create → deposit → check-in → expire → release
- `test_full_lifecycle_multi_beneficiary_bps_split` — 70/30 BPS split release
- `test_full_lifecycle_with_hibernation` — hibernation suspends expiry

Property-based tests are in `contracts/ttl_vault/tests/property_tests.rs`:

- `prop_vault_status_transitions_valid` — valid state transitions only
- `prop_no_double_release` — funds released at most once
- `prop_ttl_always_increases_on_check_in` — check-in strictly extends TTL

## Related Documentation

- [TTL & State Archival Logic](ttl-logic.md)
- [Vault Hibernation](hibernation.md)
- [Beneficiary Conflict Resolution](beneficiary-conflict-resolution.md)
- [Withdrawal Features](withdrawal-features.md)
