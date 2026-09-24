//! Tests for the slice failover mechanism (Issue #35).
//!
//! Covers:
//! - Authorized failover: owner can register backup slices, record failures,
//!   trigger automatic and explicit failover, and revert.
//! - Unauthorized caller rejection: non-owner is rejected with `NotOwner` on
//!   every mutating entry point.
//! - Threshold-based automatic failover: once failure count reaches the
//!   configured threshold the backup is promoted automatically.
//! - Read-only queries: `get_backup_slices`, `get_active_slice`, and
//!   `get_failure_count` are accessible without auth.

#![cfg(test)]

extern crate alloc;

use super::*;
use soroban_sdk::{testutils::Address as _, token::StellarAssetClient, Address, Env};

// ── Test helpers ──────────────────────────────────────────────────────────────

/// Bootstrap a minimal environment: contract, XLM token, admin, vault owner,
/// and one vault.  Returns the pieces needed by all test cases.
fn setup() -> (
    Env,
    Address, // owner
    Address, // admin
    TtlVaultContractClient<'static>,
    u64, // vault_id
) {
    let env = Env::default();
    env.mock_all_auths();

    let owner = Address::generate(&env);
    let beneficiary = Address::generate(&env);
    let admin = Address::generate(&env);

    let token_admin = Address::generate(&env);
    let token_address = env
        .register_stellar_asset_contract_v2(token_admin)
        .address();
    StellarAssetClient::new(&env, &token_address).mint(&owner, &10_000_000);

    let contract_address = env.register_contract(None, TtlVaultContract);
    let client = TtlVaultContractClient::new(&env, &contract_address);
    client.initialize(&token_address, &admin);

    let vault_id = client.create_vault(&owner, &beneficiary, &3_600u64, &None);

    // Safety: the client borrow must outlive the env in tests.
    let client: TtlVaultContractClient<'static> = unsafe { core::mem::transmute(client) };
    (env, owner, admin, client, vault_id)
}

// ── Authorized operations ─────────────────────────────────────────────────────

/// The vault owner can register a backup slice and read it back.
#[test]
fn test_register_backup_slice_by_owner() {
    let (_env, owner, _admin, client, vault_id) = setup();

    let primary_id = 1u64;
    let backup_id = 2u64;
    let threshold = 3u32;

    let returned =
        client.register_backup_slice(&vault_id, &owner, &primary_id, &backup_id, &threshold);
    assert_eq!(returned, backup_id);

    let backups = client.get_backup_slices(&primary_id);
    assert_eq!(backups.len(), 1);
    assert_eq!(backups.get(0).unwrap(), backup_id);
}

/// Recording failures below the threshold does NOT activate failover.
#[test]
fn test_record_failure_below_threshold_does_not_failover() {
    let (_env, owner, _admin, client, vault_id) = setup();

    let primary_id = 10u64;
    let backup_id = 20u64;
    let threshold = 5u32;

    client.register_backup_slice(&vault_id, &owner, &primary_id, &backup_id, &threshold);

    // Record threshold-1 failures — should not trigger failover.
    for _ in 0..(threshold - 1) {
        let activated = client.record_slice_failure(
            &vault_id,
            &owner,
            &primary_id,
            &slice_failover::FailoverReason::ThresholdExceeded,
        );
        assert!(!activated, "failover should not activate below threshold");
    }

    // Active slice is still the primary.
    assert_eq!(client.get_active_slice(&primary_id), primary_id);
    // Failure count reflects the recorded events.
    assert_eq!(client.get_failure_count(&primary_id), threshold - 1);
}

/// When the failure count reaches the threshold, the backup slice is promoted
/// automatically through `record_slice_failure`.
#[test]
fn test_record_failure_at_threshold_activates_failover() {
    let (_env, owner, _admin, client, vault_id) = setup();

    let primary_id = 11u64;
    let backup_id = 21u64;
    let threshold = 3u32;

    client.register_backup_slice(&vault_id, &owner, &primary_id, &backup_id, &threshold);

    // Record threshold-1 failures silently.
    for _ in 0..(threshold - 1) {
        client.record_slice_failure(
            &vault_id,
            &owner,
            &primary_id,
            &slice_failover::FailoverReason::Timeout,
        );
    }

    // The threshold-th failure must activate failover and return true.
    let activated = client.record_slice_failure(
        &vault_id,
        &owner,
        &primary_id,
        &slice_failover::FailoverReason::Timeout,
    );
    assert!(activated, "failover should activate at threshold");

    // Active slice must now point to the backup.
    assert_eq!(client.get_active_slice(&primary_id), backup_id);
}

/// The owner can explicitly activate failover without going through the
/// failure-recording path.
#[test]
fn test_explicit_activate_failover_by_owner() {
    let (_env, owner, _admin, client, vault_id) = setup();

    let primary_id = 12u64;
    let backup_id = 22u64;

    client.register_backup_slice(&vault_id, &owner, &primary_id, &backup_id, &10u32);

    let activated = client.activate_failover(
        &vault_id,
        &owner,
        &primary_id,
        &backup_id,
        &slice_failover::FailoverReason::ExplicitFailure,
    );
    assert!(activated);
    assert_eq!(client.get_active_slice(&primary_id), backup_id);
}

/// The owner can revert an active failover, restoring the primary and
/// resetting the failure counter.
#[test]
fn test_revert_failover_by_owner() {
    let (_env, owner, _admin, client, vault_id) = setup();

    let primary_id = 13u64;
    let backup_id = 23u64;

    client.register_backup_slice(&vault_id, &owner, &primary_id, &backup_id, &10u32);
    client.activate_failover(
        &vault_id,
        &owner,
        &primary_id,
        &backup_id,
        &slice_failover::FailoverReason::ExplicitFailure,
    );

    let reverted = client.revert_failover(&vault_id, &owner, &primary_id, &backup_id);
    assert!(reverted);

    // Active slice returns to primary.
    assert_eq!(client.get_active_slice(&primary_id), primary_id);
    // Failure count was reset.
    assert_eq!(client.get_failure_count(&primary_id), 0u32);
}

/// Activating failover a second time (while already active) is a no-op that
/// returns false.
#[test]
fn test_double_activate_is_noop() {
    let (_env, owner, _admin, client, vault_id) = setup();

    let primary_id = 14u64;
    let backup_id = 24u64;

    client.register_backup_slice(&vault_id, &owner, &primary_id, &backup_id, &10u32);
    client.activate_failover(
        &vault_id,
        &owner,
        &primary_id,
        &backup_id,
        &slice_failover::FailoverReason::ExplicitFailure,
    );

    let second = client.activate_failover(
        &vault_id,
        &owner,
        &primary_id,
        &backup_id,
        &slice_failover::FailoverReason::ExplicitFailure,
    );
    assert!(
        !second,
        "second activate while already active should return false"
    );
}

/// Reverting when no failover is active returns false.
#[test]
fn test_revert_when_not_active_is_noop() {
    let (_env, owner, _admin, client, vault_id) = setup();

    let primary_id = 15u64;
    let backup_id = 25u64;

    client.register_backup_slice(&vault_id, &owner, &primary_id, &backup_id, &10u32);

    let reverted = client.revert_failover(&vault_id, &owner, &primary_id, &backup_id);
    assert!(!reverted, "revert when not active should return false");
}

/// get_active_slice returns the slice_id itself when no failover is configured.
#[test]
fn test_get_active_slice_default_returns_primary() {
    let (_env, _owner, _admin, client, _vault_id) = setup();

    let slice_id = 99u64;
    // No backup registered — active slice defaults to itself.
    assert_eq!(client.get_active_slice(&slice_id), slice_id);
}

// ── Unauthorized caller rejection ─────────────────────────────────────────────

/// A stranger (non-owner) cannot register a backup slice.
#[test]
fn test_register_backup_slice_rejects_non_owner() {
    let (env, _owner, _admin, client, vault_id) = setup();
    let stranger = Address::generate(&env);

    let err = client
        .try_register_backup_slice(&vault_id, &stranger, &1u64, &2u64, &3u32)
        .unwrap_err()
        .unwrap();
    assert_eq!(err, ContractError::NotOwner);
}

/// A stranger cannot record a slice failure.
#[test]
fn test_record_slice_failure_rejects_non_owner() {
    let (env, owner, _admin, client, vault_id) = setup();
    let stranger = Address::generate(&env);

    // Register config so the check doesn't fail for a different reason.
    client.register_backup_slice(&vault_id, &owner, &1u64, &2u64, &3u32);

    let err = client
        .try_record_slice_failure(
            &vault_id,
            &stranger,
            &1u64,
            &slice_failover::FailoverReason::Timeout,
        )
        .unwrap_err()
        .unwrap();
    assert_eq!(err, ContractError::NotOwner);
}

/// A stranger cannot explicitly activate failover.
#[test]
fn test_activate_failover_rejects_non_owner() {
    let (env, owner, _admin, client, vault_id) = setup();
    let stranger = Address::generate(&env);

    client.register_backup_slice(&vault_id, &owner, &1u64, &2u64, &10u32);

    let err = client
        .try_activate_failover(
            &vault_id,
            &stranger,
            &1u64,
            &2u64,
            &slice_failover::FailoverReason::ExplicitFailure,
        )
        .unwrap_err()
        .unwrap();
    assert_eq!(err, ContractError::NotOwner);
}

/// A stranger cannot revert failover.
#[test]
fn test_revert_failover_rejects_non_owner() {
    let (env, owner, _admin, client, vault_id) = setup();
    let stranger = Address::generate(&env);

    client.register_backup_slice(&vault_id, &owner, &1u64, &2u64, &10u32);
    client.activate_failover(
        &vault_id,
        &owner,
        &1u64,
        &2u64,
        &slice_failover::FailoverReason::ExplicitFailure,
    );

    let err = client
        .try_revert_failover(&vault_id, &stranger, &1u64, &2u64)
        .unwrap_err()
        .unwrap();
    assert_eq!(err, ContractError::NotOwner);
}

/// Registering a backup slice with primary == backup is rejected with InvalidSlice.
#[test]
fn test_register_backup_same_as_primary_fails() {
    let (_env, owner, _admin, client, vault_id) = setup();

    let err = client
        .try_register_backup_slice(&vault_id, &owner, &5u64, &5u64, &2u32)
        .unwrap_err()
        .unwrap();
    assert_eq!(err, ContractError::InvalidSlice);
}

// ── Regression tests for previously fixed slice-failover bugs ─────────────────
//
// Issue #424 — each test below documents a bug that was fixed and must not
// regress.  The comment header identifies the root-cause that prompted the fix.

/// Regression: threshold=1 must fire immediately on the very first failure.
///
/// Previously the threshold check used `>` instead of `>=`, so a threshold
/// of 1 required *two* failures before promotion.
#[test]
fn regression_threshold_one_activates_on_first_failure() {
    let (_env, owner, _admin, client, vault_id) = setup();

    let primary_id = 50u64;
    let backup_id = 51u64;

    client.register_backup_slice(&vault_id, &owner, &primary_id, &backup_id, &1u32);

    let activated = client.record_slice_failure(
        &vault_id,
        &owner,
        &primary_id,
        &slice_failover::FailoverReason::ThresholdExceeded,
    );

    assert!(activated, "threshold=1 must activate on the very first failure");
    assert_eq!(
        client.get_active_slice(&primary_id),
        backup_id,
        "active slice must be backup after threshold=1 failover"
    );
}

/// Regression: failure count must keep incrementing even after failover is
/// already active (so operators can see how many times the primary was hit).
///
/// Previously the counter was frozen once `is_active == true`.
#[test]
fn regression_failure_count_increments_after_failover_is_active() {
    let (_env, owner, _admin, client, vault_id) = setup();

    let primary_id = 52u64;
    let backup_id = 53u64;
    let threshold = 2u32;

    client.register_backup_slice(&vault_id, &owner, &primary_id, &backup_id, &threshold);

    // Drive count to threshold → activates failover.
    for _ in 0..threshold {
        client.record_slice_failure(
            &vault_id,
            &owner,
            &primary_id,
            &slice_failover::FailoverReason::Timeout,
        );
    }
    assert_eq!(client.get_active_slice(&primary_id), backup_id);

    // One more failure while failover is active.
    client.record_slice_failure(
        &vault_id,
        &owner,
        &primary_id,
        &slice_failover::FailoverReason::Timeout,
    );

    assert_eq!(
        client.get_failure_count(&primary_id),
        threshold + 1,
        "failure count must keep incrementing even when failover is already active"
    );
}

/// Regression: when multiple backups are registered the *first* one (lowest
/// index / earliest registration) takes priority on automatic failover.
///
/// Previously backups were iterated in reverse order, promoting the last-
/// registered backup instead of the first.
#[test]
fn regression_multiple_backups_first_registered_takes_priority() {
    let (_env, owner, _admin, client, vault_id) = setup();

    let primary_id = 54u64;
    let backup_first = 55u64;
    let backup_second = 56u64;
    let threshold = 1u32;

    // Register two backups — first_backup is priority.
    client.register_backup_slice(&vault_id, &owner, &primary_id, &backup_first, &threshold);
    client.register_backup_slice(&vault_id, &owner, &primary_id, &backup_second, &10u32);

    client.record_slice_failure(
        &vault_id,
        &owner,
        &primary_id,
        &slice_failover::FailoverReason::ThresholdExceeded,
    );

    assert_eq!(
        client.get_active_slice(&primary_id),
        backup_first,
        "first registered backup must be promoted, not the second"
    );
}

/// Regression: after reverting a failover, the failure counter must be reset
/// so that subsequent failures start counting from zero and can re-trigger
/// failover at the same threshold.
///
/// Previously revert_failover didn't reset the counter, so one additional
/// failure after a revert immediately re-activated failover regardless of
/// threshold.
#[test]
fn regression_revert_resets_counter_allowing_refailover_at_threshold() {
    let (_env, owner, _admin, client, vault_id) = setup();

    let primary_id = 57u64;
    let backup_id = 58u64;
    let threshold = 3u32;

    client.register_backup_slice(&vault_id, &owner, &primary_id, &backup_id, &threshold);

    // Trigger failover.
    for _ in 0..threshold {
        client.record_slice_failure(
            &vault_id,
            &owner,
            &primary_id,
            &slice_failover::FailoverReason::Timeout,
        );
    }
    assert_eq!(client.get_active_slice(&primary_id), backup_id);

    // Revert.
    client.revert_failover(&vault_id, &owner, &primary_id, &backup_id);
    assert_eq!(
        client.get_failure_count(&primary_id),
        0,
        "revert must reset the failure counter to zero"
    );
    assert_eq!(
        client.get_active_slice(&primary_id),
        primary_id,
        "active slice must return to primary after revert"
    );

    // Record threshold-1 new failures — must NOT re-activate.
    for _ in 0..(threshold - 1) {
        let activated = client.record_slice_failure(
            &vault_id,
            &owner,
            &primary_id,
            &slice_failover::FailoverReason::Timeout,
        );
        assert!(
            !activated,
            "should not re-activate below threshold after revert"
        );
    }

    // The threshold-th failure must re-activate.
    let reactivated = client.record_slice_failure(
        &vault_id,
        &owner,
        &primary_id,
        &slice_failover::FailoverReason::Timeout,
    );
    assert!(
        reactivated,
        "threshold-th failure after revert must re-activate failover"
    );
    assert_eq!(client.get_active_slice(&primary_id), backup_id);
}

/// Regression: get_failure_count on a slice that has never been touched must
/// return 0, not panic.
///
/// Previously the function returned an error on missing storage entries.
#[test]
fn regression_get_failure_count_unknown_slice_returns_zero() {
    let (_env, _owner, _admin, client, _vault_id) = setup();

    let unknown_slice_id = 99_999u64;
    assert_eq!(
        client.get_failure_count(&unknown_slice_id),
        0u32,
        "failure count for unknown slice must default to zero"
    );
}

/// Regression: recording a failure on a primary that has NO backup registered
/// must return false immediately and must not panic.
///
/// Previously this path called unwrap() on an empty Vec, causing a runtime
/// trap in the contract.
#[test]
fn regression_record_failure_with_no_backup_returns_false() {
    let (_env, owner, _admin, client, vault_id) = setup();

    let primary_id = 60u64;
    // Deliberately do NOT register a backup.

    let activated = client.record_slice_failure(
        &vault_id,
        &owner,
        &primary_id,
        &slice_failover::FailoverReason::Timeout,
    );

    assert!(
        !activated,
        "recording failure with no backup must return false, not panic"
    );

    // Failure count must still be tracked.
    assert_eq!(
        client.get_failure_count(&primary_id),
        1,
        "failure count must be incremented even when no backup is registered"
    );
}

/// Regression: activate_failover must emit a FAILOVER_ACTIVATED event and a
/// FAILOVER_EVENT event, and revert_failover must emit a FAILOVER_REVERTED
/// event and a FAILOVER_EVENT event.
///
/// Previously duplicate-publish paths caused double-emission, confusing
/// off-chain indexers.  We verify the correct topics are present and count
/// only the new events added by each call.
#[test]
fn regression_failover_events_emitted_exactly_once() {
    use soroban_sdk::testutils::Events;
    use soroban_sdk::{IntoVal, TryIntoVal, Val};

    let (env, owner, _admin, client, vault_id) = setup();

    let primary_id = 61u64;
    let backup_id = 62u64;

    client.register_backup_slice(&vault_id, &owner, &primary_id, &backup_id, &10u32);

    // Record how many events have been emitted so far (by registration).
    let events_before_activate = env.events().all().len();

    client.activate_failover(
        &vault_id,
        &owner,
        &primary_id,
        &backup_id,
        &slice_failover::FailoverReason::ExplicitFailure,
    );

    let all_events = env.events().all();
    // activate_failover must add exactly 2 events: FAILOVER_ACTIVATED + FAILOVER_EVENT
    let activate_events_count = all_events.len() - events_before_activate;
    assert_eq!(
        activate_events_count, 2,
        "activate_failover must emit exactly 2 events (activated + event topic), got {}",
        activate_events_count
    );

    // Verify FAILOVER_ACTIVATED_TOPIC is present somewhere in all events.
    let saw_fail_act = all_events.iter().any(|e| {
        let topics: soroban_sdk::Vec<Val> = e.1.clone().into_val(&env);
        topics
            .get(0)
            .and_then(|t| t.try_into_val::<_, soroban_sdk::Symbol>(&env).ok())
            .map(|s| s == slice_failover::FAILOVER_ACTIVATED_TOPIC)
            .unwrap_or(false)
    });
    assert!(saw_fail_act, "FAILOVER_ACTIVATED_TOPIC must be emitted by activate_failover");

    let events_before_revert = env.events().all().len();

    client.revert_failover(&vault_id, &owner, &primary_id, &backup_id);

    let all_events_after_revert = env.events().all();
    let revert_events_count = all_events_after_revert.len() - events_before_revert;
    assert_eq!(
        revert_events_count, 2,
        "revert_failover must emit exactly 2 events (reverted + event topic), got {}",
        revert_events_count
    );

    // Verify FAILOVER_REVERTED_TOPIC is present somewhere in all events.
    let saw_fail_rev = all_events_after_revert.iter().any(|e| {
        let topics: soroban_sdk::Vec<Val> = e.1.clone().into_val(&env);
        topics
            .get(0)
            .and_then(|t| t.try_into_val::<_, soroban_sdk::Symbol>(&env).ok())
            .map(|s| s == slice_failover::FAILOVER_REVERTED_TOPIC)
            .unwrap_or(false)
    });
    assert!(saw_fail_rev, "FAILOVER_REVERTED_TOPIC must be emitted by revert_failover");
}

/// Regression: explicit activate_failover with an unregistered (primary,backup)
/// pair must return false and must not modify the active slice.
///
/// Previously the function returned true on missing config, giving callers a
/// false success signal.
#[test]
fn regression_activate_failover_with_unregistered_config_returns_false() {
    let (_env, owner, _admin, client, vault_id) = setup();

    let primary_id = 63u64;
    let backup_id = 64u64;
    // No register_backup_slice call.

    let activated = client.activate_failover(
        &vault_id,
        &owner,
        &primary_id,
        &backup_id,
        &slice_failover::FailoverReason::ExplicitFailure,
    );

    assert!(
        !activated,
        "activate_failover must return false when no config is registered"
    );
    assert_eq!(
        client.get_active_slice(&primary_id),
        primary_id,
        "active slice must remain as primary when activate returns false"
    );
}
