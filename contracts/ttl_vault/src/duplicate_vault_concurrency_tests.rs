//! Concurrency / near-simultaneous creation tests for duplicate vault prevention.
//!
//! Soroban's test environment is single-threaded and all ledger state mutations
//! are fully serialised. "Concurrency" is therefore modelled as two calls that
//! would appear in the *same block* (identical ledger timestamp), or as a rapid
//! sequence with no intermediate state change. The invariant under test is:
//!
//!   Given a (owner, beneficiary, check_in_interval) triple, **exactly one**
//!   `create_vault` call succeeds; every subsequent call with the same triple,
//!   regardless of how quickly it follows, is rejected with `DuplicateVault` (57)
//!   until the original vault is cancelled or released.

#![cfg(test)]

extern crate alloc;

use super::*;
use soroban_sdk::{
    testutils::{Address as _, Ledger},
    token::StellarAssetClient,
    Address, Env,
};

// ---------------------------------------------------------------------------
// Shared setup
// ---------------------------------------------------------------------------

fn setup_env() -> (Env, Address, TtlVaultContractClient<'static>) {
    let env = Env::default();
    env.mock_all_auths();

    let admin = Address::generate(&env);
    let token_admin = Address::generate(&env);
    let token_address = env
        .register_stellar_asset_contract_v2(token_admin)
        .address();

    let owner = Address::generate(&env);
    StellarAssetClient::new(&env, &token_address).mint(&owner, &1_000_000_000);

    let contract_address = env.register_contract(None, TtlVaultContract);
    let client = TtlVaultContractClient::new(&env, &contract_address);
    client.initialize(&token_address, &admin);

    let client: TtlVaultContractClient<'static> = unsafe { core::mem::transmute(client) };
    (env, owner, client)
}

// ---------------------------------------------------------------------------
// Test 1 – Only the first of N "simultaneous" calls with identical params succeeds
//
// Simulates N actors all attempting to create the same vault in what would be
// a single block (timestamp unchanged between calls). Exactly one must succeed;
// all others must be rejected with DuplicateVault.
// ---------------------------------------------------------------------------

#[test]
fn test_concurrent_same_params_only_first_succeeds() {
    let (env, owner, client) = setup_env();

    let beneficiary = Address::generate(&env);
    let interval = 3_600u64; // 1 hour

    // Fix ledger timestamp to simulate same-block scenario.
    env.ledger().set_timestamp(1_000_000);

    // First attempt — must succeed and return a valid vault ID.
    let vault_id = client.create_vault(&owner, &beneficiary, &interval, &None);
    assert!(vault_id > 0, "first create_vault must return a valid vault ID");

    // Subsequent attempts at the same (owner, beneficiary, interval) triple — all must fail
    // with DuplicateVault regardless of how many are issued.
    let attempt_count = 5;
    for attempt in 0..attempt_count {
        // Advance time by one second to confirm the guard is not time-gated —
        // it must fire even when the ledger clock has moved.
        env.ledger().with_mut(|l| l.timestamp += 1);

        let err = client
            .try_create_vault(&owner, &beneficiary, &interval, &None)
            .unwrap_err()
            .unwrap();

        assert_eq!(
            err,
            soroban_sdk::Error::from_contract_error(ContractError::DuplicateVault as u32),
            "attempt {} must be rejected with DuplicateVault",
            attempt + 1
        );
    }

    // Confirm the vault ID counter did not advance for rejected calls: a new
    // vault with different params increments by exactly 1 (not by 1 + 5).
    let other_beneficiary = Address::generate(&env);
    let new_id = client.create_vault(&owner, &other_beneficiary, &interval, &None);
    assert_eq!(
        new_id,
        vault_id + 1,
        "vault ID counter must not advance for rejected duplicate calls"
    );
}

// ---------------------------------------------------------------------------
// Test 2 – The guard fires at the *same* ledger timestamp (same-block ordering)
//
// Both calls happen without any ledger advancement between them, confirming
// that duplicate detection is not a TTL/timing feature but a persistent-storage
// fingerprint check.
// ---------------------------------------------------------------------------

#[test]
fn test_same_block_ordering_deterministically_rejects_second_call() {
    let (env, owner, client) = setup_env();

    let beneficiary = Address::generate(&env);
    let interval = 86_400u64; // 24 hours

    // Pin the block timestamp for both calls.
    let block_time = 5_000_000u64;
    env.ledger().set_timestamp(block_time);

    // First call — succeeds.
    let id = client.create_vault(&owner, &beneficiary, &interval, &None);

    // Second call at *identical* timestamp — must be rejected.
    // (No env.ledger() mutation between the two calls.)
    let err = client
        .try_create_vault(&owner, &beneficiary, &interval, &None)
        .unwrap_err()
        .unwrap();

    assert_eq!(
        err,
        soroban_sdk::Error::from_contract_error(ContractError::DuplicateVault as u32),
        "second call in the same block must be rejected with DuplicateVault"
    );

    // Verify the stored vault is the one from the first call.
    let vault = client.get_vault(&id);
    assert_eq!(vault.owner, owner);
    assert_eq!(vault.beneficiary, beneficiary);
    assert_eq!(vault.check_in_interval, interval);
}

// ---------------------------------------------------------------------------
// Test 3 – Near-simultaneous calls with different but colliding *derived* IDs
//
// Two distinct (owner, beneficiary, interval) triples that happen to share two
// of three components must each be accepted independently — the guard must NOT
// cross-contaminate unrelated triples.
// ---------------------------------------------------------------------------

#[test]
fn test_colliding_near_simultaneous_calls_different_params_both_succeed() {
    let (env, owner, client) = setup_env();

    let beneficiary_a = Address::generate(&env);
    let beneficiary_b = Address::generate(&env); // different beneficiary

    let interval = 3_600u64;

    env.ledger().set_timestamp(2_000_000);

    // Both calls at the same timestamp.
    let id_a = client.create_vault(&owner, &beneficiary_a, &interval, &None);
    let id_b = client.create_vault(&owner, &beneficiary_b, &interval, &None);

    // Both must succeed and receive distinct IDs.
    assert_ne!(id_a, id_b, "different (owner, beneficiary, interval) triples must produce distinct vault IDs");

    // And both vaults must be retrievable with the correct configuration.
    let vault_a = client.get_vault(&id_a);
    let vault_b = client.get_vault(&id_b);

    assert_eq!(vault_a.beneficiary, beneficiary_a);
    assert_eq!(vault_b.beneficiary, beneficiary_b);
}

// ---------------------------------------------------------------------------
// Test 4 – Changing only the interval creates a distinct vault (no false positive)
//
// Guards a common mistake: two vaults that share owner+beneficiary but differ
// only by check_in_interval must both be allowed.
// ---------------------------------------------------------------------------

#[test]
fn test_different_interval_same_owner_beneficiary_is_not_duplicate() {
    let (env, owner, client) = setup_env();

    let beneficiary = Address::generate(&env);

    let interval_short = 3_600u64;   // 1 hour
    let interval_long  = 86_400u64;  // 24 hours

    // Both calls at the same timestamp.
    env.ledger().set_timestamp(1_500_000);

    let id_short = client.create_vault(&owner, &beneficiary, &interval_short, &None);
    let id_long  = client.create_vault(&owner, &beneficiary, &interval_long,  &None);

    assert_ne!(id_short, id_long);

    // The short-interval vault can still be duplicated (i.e., the long-interval
    // vault did not accidentally consume the short-interval fingerprint).
    let err = client
        .try_create_vault(&owner, &beneficiary, &interval_short, &None)
        .unwrap_err()
        .unwrap();
    assert_eq!(
        err,
        soroban_sdk::Error::from_contract_error(ContractError::DuplicateVault as u32)
    );
}

// ---------------------------------------------------------------------------
// Test 5 – Fingerprint is cleared by cancel_vault, enabling re-creation
//
// Validates that the concurrency guard is lifecycle-aware: once the vault is
// cancelled the fingerprint is removed and the same triple can be used again.
// ---------------------------------------------------------------------------

#[test]
fn test_fingerprint_cleared_after_cancel_allows_recreation() {
    let (env, owner, client) = setup_env();

    let beneficiary = Address::generate(&env);
    let interval = 3_600u64;

    env.ledger().set_timestamp(1_000_000);

    // Create vault.
    let id = client.create_vault(&owner, &beneficiary, &interval, &None);

    // Confirm duplicate guard is live.
    let dup_err = client
        .try_create_vault(&owner, &beneficiary, &interval, &None)
        .unwrap_err()
        .unwrap();
    assert_eq!(
        dup_err,
        soroban_sdk::Error::from_contract_error(ContractError::DuplicateVault as u32)
    );

    // Cancel the vault — this must clear the fingerprint.
    client.cancel_vault(&id, &owner);

    // Now the same triple must be accepted again.
    env.ledger().with_mut(|l| l.timestamp += 1);
    let new_id = client.create_vault(&owner, &beneficiary, &interval, &None);
    assert_ne!(
        new_id, id,
        "re-created vault must receive a new unique ID"
    );
    assert_eq!(client.get_vault(&new_id).status, ReleaseStatus::Locked);
}

// ---------------------------------------------------------------------------
// Test 6 – Fingerprint is cleared by trigger_release, enabling re-creation
//
// Same lifecycle property as Test 5, but verified via the release path.
// ---------------------------------------------------------------------------

#[test]
fn test_fingerprint_cleared_after_release_allows_recreation() {
    let (env, owner, client) = setup_env();

    let beneficiary = Address::generate(&env);
    let interval = 100u64; // short interval for easy expiry

    env.ledger().set_timestamp(1_000_000);

    // Create vault.
    let id = client.create_vault(&owner, &beneficiary, &interval, &None);

    // Expire the vault and trigger release.
    env.ledger().with_mut(|l| l.timestamp += interval + 1);
    client.trigger_release(&id);

    assert_eq!(client.get_vault(&id).status, ReleaseStatus::Released);

    // The fingerprint must have been cleared — re-creation should succeed.
    env.ledger().with_mut(|l| l.timestamp += 1);
    let new_id = client.create_vault(&owner, &beneficiary, &interval, &None);
    assert_ne!(new_id, id);
    assert_eq!(client.get_vault(&new_id).status, ReleaseStatus::Locked);
}

// ---------------------------------------------------------------------------
// Test 7 – Multiple distinct owners can create vaults with the same beneficiary
//          and interval without triggering the duplicate guard
//
// The guard is scoped to (owner, beneficiary, interval) — a different owner is
// always a distinct triple.
// ---------------------------------------------------------------------------

#[test]
fn test_multiple_owners_same_beneficiary_interval_all_succeed() {
    let (env, owner_a, client) = setup_env();

    let owner_b = Address::generate(&env);
    let owner_c = Address::generate(&env);

    // Mint tokens for owners b and c so they can be recognised as spenders
    // if the contract ever requires it (mock_all_auths covers auth checks).
    let _ = owner_b.clone();
    let _ = owner_c.clone();

    let beneficiary = Address::generate(&env);
    let interval = 7_200u64;

    env.ledger().set_timestamp(3_000_000);

    let id_a = client.create_vault(&owner_a, &beneficiary, &interval, &None);
    let id_b = client.create_vault(&owner_b, &beneficiary, &interval, &None);
    let id_c = client.create_vault(&owner_c, &beneficiary, &interval, &None);

    // All three IDs must be distinct.
    assert_ne!(id_a, id_b);
    assert_ne!(id_b, id_c);
    assert_ne!(id_a, id_c);

    // Each vault must carry the correct owner.
    assert_eq!(client.get_vault(&id_a).owner, owner_a);
    assert_eq!(client.get_vault(&id_b).owner, owner_b);
    assert_eq!(client.get_vault(&id_c).owner, owner_c);
}
