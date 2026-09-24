# Withdrawal Features Documentation

This document describes the four new withdrawal features implemented in Issues #565-#568.

## Withdrawal / Clawback Ordering Guarantee (Issue #428)

### Overview
A withdrawal request and a concurrent clawback (or vesting update) may target the same vault funds within the same ledger close. The contract enforces a deterministic ordering so that funds can never be double-spent and a withdrawal cannot bypass an in-flight clawback.

### Guarantee
- Within a single ledger close, the contract processes state transitions in a fixed order: any in-flight clawback/vesting update is applied **before** a withdrawal is released.
- A withdrawal that would draw on funds already claimed by an in-flight clawback is rejected rather than partially fulfilled.
- Because ordering is deterministic, replaying the same ledger close always yields the same outcome; the vault balance can never be reduced below zero by the combined effects of a withdrawal and a clawback.

### Enforcement
- The clawback/vesting path and the withdrawal path both mutate the vault balance through the same guarded accounting, so the second operation observes the first operation's committed state.
- If a clawback is in flight for the funds a withdrawal targets, the withdrawal fails with the existing insufficient-funds / in-flight error instead of succeeding.

### Tests
- `contracts/ttl_vault/src/withdrawal_escrow_tests.rs` contains a race test that submits a withdrawal and a clawback against the same funds in the same ledger close and asserts no double-spend occurs.
- `contracts/ttl_vault/src/regression_tests.rs` contains a regression test locking in the resolved ordering so future changes cannot silently reintroduce the race.

---

## Issue #565: Withdrawal Scheduling Validation

### Overview
Prevents overlapping or conflicting withdrawal schedules by validating that scheduled withdrawals don't occur within a 1-hour window of each other.

### Key Functions

#### `schedule_withdrawal(vault_id, caller, timestamp, amount) -> Result<(), ContractError>`
Schedules a withdrawal with conflict detection.

**Parameters:**
- `vault_id`: The vault ID
- `caller`: The vault owner (must be authenticated)
- `timestamp`: Unix timestamp for the scheduled withdrawal
- `amount`: Amount to withdraw in stroops

**Returns:**
- `Ok(())` on success
- `Err(ContractError::ConflictingWithdrawalSchedule)` if overlapping with existing schedule
- `Err(ContractError::NotOwner)` if caller is not the vault owner
- `Err(ContractError::AlreadyReleased)` if vault is not in Locked status

**Events:**
- `WITHDRAWAL_VALIDATION_TOPIC`: Emitted when a withdrawal is successfully scheduled

### Implementation Details
- Maintains a vector of `WithdrawalScheduleEntry` structs per vault
- Checks for conflicts within a 1-hour (3600 second) window
- Prevents scheduling withdrawals that would overlap with existing schedules
- Stores schedules in persistent storage with TTL management

### Error Codes
- `OverlappingWithdrawalSchedule = 64`: Withdrawal overlaps with existing schedule
- `ConflictingWithdrawalSchedule = 65`: Withdrawal conflicts with existing schedule

---

## Issue #566: Withdrawal Limits by Time

### Overview
Implements daily, weekly, and monthly withdrawal limits with automatic period resets. Limits are tracked per vault and reset automatically when their respective periods expire.

### Key Functions

#### `set_withdrawal_limits(vault_id, caller, daily_limit, weekly_limit, monthly_limit) -> Result<(), ContractError>`
Configures withdrawal limits for a vault.

**Parameters:**
- `vault_id`: The vault ID
- `caller`: The vault owner (must be authenticated)
- `daily_limit`: Maximum amount withdrawable per day (in stroops)
- `weekly_limit`: Maximum amount withdrawable per week (in stroops)
- `monthly_limit`: Maximum amount withdrawable per month (in stroops)

**Returns:**
- `Ok(())` on success
- `Err(ContractError::NotOwner)` if caller is not the vault owner

**Events:**
- `WITHDRAWAL_LIMIT_SET_TOPIC`: Emitted when limits are configured

#### `get_withdrawal_limits(vault_id) -> Option<WithdrawalLimit>`
Retrieves the current withdrawal limits for a vault.

**Returns:**
- `Some(WithdrawalLimit)` if limits are configured
- `None` if no limits are set

### Data Structures

#### `WithdrawalLimit`
```rust
pub struct WithdrawalLimit {
    pub daily_limit: i128,
    pub weekly_limit: i128,
    pub monthly_limit: i128,
}
```

#### `WithdrawalTracker`
```rust
pub struct WithdrawalTracker {
    pub daily_withdrawn: i128,
    pub daily_reset_at: u64,
    pub weekly_withdrawn: i128,
    pub weekly_reset_at: u64,
    pub monthly_withdrawn: i128,
    pub monthly_reset_at: u64,
}
```

### Implementation Details
- Limits are checked during every `withdraw()` call
- Trackers automatically reset when their period expires
- Daily period: 24 hours (86,400 seconds)
- Weekly period: 7 days (604,800 seconds)
- Monthly period: 30 days (2,592,000 seconds)
- Limits are optional; if not set, no restrictions apply

### Error Codes
- `DailyWithdrawalLimitExceeded = 66`: Daily limit would be exceeded
- `WeeklyWithdrawalLimitExceeded = 67`: Weekly limit would be exceeded
- `MonthlyWithdrawalLimitExceeded = 68`: Monthly limit would be exceeded

### Events
- `WITHDRAWAL_LIMIT_SET_TOPIC`: Emitted when limits are configured
- `WITHDRAWAL_LIMIT_EXCEEDED_TOPIC`: Emitted when a limit is exceeded

---

## Issue #567: Withdrawal Destination Whitelist

### Overview
Restricts withdrawals to only whitelisted addresses. Vault owners can add and remove addresses from the whitelist.

### Key Functions

#### `add_whitelist_address(vault_id, caller, address, label) -> Result<(), ContractError>`
Adds an address to the withdrawal whitelist.

**Parameters:**
- `vault_id`: The vault ID
- `caller`: The vault owner (must be authenticated)
- `address`: The address to whitelist
- `label`: A descriptive label for the address (e.g., "cold_wallet")

**Returns:**
- `Ok(())` on success
- `Err(ContractError::NotOwner)` if caller is not the vault owner

**Events:**
- `WHITELIST_ADDED_TOPIC`: Emitted when an address is added

#### `remove_whitelist_address(vault_id, caller, address) -> Result<(), ContractError>`
Removes an address from the withdrawal whitelist.

**Parameters:**
- `vault_id`: The vault ID
- `caller`: The vault owner (must be authenticated)
- `address`: The address to remove

**Returns:**
- `Ok(())` on success
- `Err(ContractError::NotOwner)` if caller is not the vault owner

**Events:**
- `WHITELIST_REMOVED_TOPIC`: Emitted when an address is removed

#### `get_whitelist(vault_id) -> Option<Vec<WhitelistEntry>>`
Retrieves the whitelist for a vault.

**Returns:**
- `Some(Vec<WhitelistEntry>)` if whitelist exists
- `None` if no whitelist is configured

### Data Structures

#### `WhitelistEntry`
```rust
pub struct WhitelistEntry {
    pub address: Address,
    pub added_at: u64,
    pub label: String,
}
```

### Implementation Details
- If no whitelist is configured, all addresses are allowed (backward compatible)
- If a whitelist exists, only whitelisted addresses can receive withdrawals
- Whitelist entries include timestamps for audit trails
- Whitelist is stored in persistent storage with TTL management

### Error Codes
- `WithdrawalDestinationNotWhitelisted = 69`: Destination address is not whitelisted

### Events
- `WHITELIST_ADDED_TOPIC`: Emitted when an address is added
- `WHITELIST_REMOVED_TOPIC`: Emitted when an address is removed
- `WHITELIST_VIOLATION_TOPIC`: Emitted when a withdrawal to non-whitelisted address is attempted

---

## Issue #568: Withdrawal Reversal

### Overview
Allows vault owners to reverse withdrawals within a grace period (24 hours by default). Reversed withdrawals restore funds to the vault.

### Key Functions

#### `reverse_withdrawal(vault_id, caller, withdrawal_id) -> Result<(), ContractError>`
Reverses a withdrawal within the grace period.

**Parameters:**
- `vault_id`: The vault ID
- `caller`: The vault owner (must be authenticated)
- `withdrawal_id`: The ID of the withdrawal to reverse

**Returns:**
- `Ok(())` on success
- `Err(ContractError::WithdrawalReversalGracePeriodExpired)` if grace period has expired
- `Err(ContractError::WithdrawalAlreadyReversed)` if already reversed
- `Err(ContractError::NotOwner)` if caller is not the vault owner

**Events:**
- `WITHDRAWAL_REVERSED_TOPIC`: Emitted when a withdrawal is successfully reversed

#### `get_withdrawal_reversal(vault_id, withdrawal_id) -> Option<WithdrawalReversal>`
Retrieves a withdrawal reversal record.

**Returns:**
- `Some(WithdrawalReversal)` if the record exists
- `None` if not found

### Data Structures

#### `WithdrawalReversal`
```rust
pub struct WithdrawalReversal {
    pub withdrawal_id: u64,
    pub amount: i128,
    pub withdrawn_at: u64,
    pub grace_period_until: u64,
    pub reversed: bool,
}
```

### Implementation Details
- Every withdrawal is automatically recorded for potential reversal
- Grace period is 24 hours (86,400 seconds) from withdrawal time
- Withdrawal IDs are auto-incremented per vault
- Reversals restore funds to the vault balance
- Once reversed, a withdrawal cannot be reversed again
- Reversal records are stored in persistent storage with TTL management

### Error Codes
- `WithdrawalReversalGracePeriodExpired = 70`: Grace period has expired
- `WithdrawalAlreadyReversed = 71`: Withdrawal has already been reversed

### Events
- `WITHDRAWAL_REVERSED_TOPIC`: Emitted when a withdrawal is reversed
- `REVERSAL_GRACE_EXPIRED_TOPIC`: Emitted when a grace period expires

---

## Integration with Existing Withdrawal Function

The withdrawal features above integrate with the existing `withdraw()` entry point. All validation (scheduling conflicts, limits, whitelist) runs before funds are released, and the ordering guarantee described at the top of this document ensures concurrent clawbacks cannot be bypassed.
