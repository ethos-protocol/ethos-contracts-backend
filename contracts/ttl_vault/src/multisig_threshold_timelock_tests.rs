//! Tests for the multi-sig approval threshold change timelock — Issue #400.
//!
//! This module verifies:
//!   1. A proposed threshold change is NOT applied immediately.
//!   2. `apply_multisig_threshold` fails before the 24-hour timelock elapses.
//!   3. `apply_multisig_threshold` succeeds after the timelock elapses.
//!   4. Any co-signer (or the owner) can cancel a pending proposal.
//!   5. Cancellation prevents application.
//!   6. Only one pending proposal is allowed at a time.
//!   7. Proposing an invalid threshold is rejected up-front.

#![cfg(test)]

extern crate alloc;

use super::*;
use soroban_sdk::{
    testutils::{Address as _, Ledger},
    token::StellarAssetClient,
    vec, Address, Env,
};

// ── Test helpers ─────────────────────────────────────────────────────────────

fn setup() -> (
    Env,
    Address, // owner
    Address, // cosigner1
    Address, // cosigner2
    Address, // contract address
    TtlVaultContractClient<'static>,
    u64,     // vault_id
) {
    let env = Env::default();
    env.mock_all_auths();

    let admin = Address::generate(&env);
    let owner = Address::generate(&env);
    let cosigner1 = Address::generate(&env);
    let cosigner2 = Address::generate(&env);

    let token_admin = Address::generate(&env);
    let token_address = env
        .register_stellar_asset_contract_v2(token_admin)
        .address();
    StellarAssetClient::new(&env, &token_address).mint(&owner, &1_000_000);

    let contract_address = env.register_contract(None, TtlVaultContract);
    let client = TtlVaultContractClient::new(&env, &contract_address);
    client.initialize(&token_address, &admin);

    // Create a vault with a 7-day check-in interval.
    let vault_id = client.create_vault(&owner, &cosigner1, &604_800u64, &None);

    // Configure 2-of-3 multi-sig (owner + cosigner1 + cosigner2, threshold = 2).
    client.configure_multisig(
        &vault_id,
        &owner,
        &vec![&env, cosigner1.clone(), cosigner2.clone()],
        &2u32,
    );

    let client: TtlVaultContractClient<'static> = unsafe { core::mem::transmute(client) };
    (env, owner, cosigner1, cosigner2, contract_address, client, vault_id)
}

// ── Timelock enforcement tests ────────────────────────────────────────────────

/// Proposing a threshold change stores a pending proposal and does NOT
/// apply the change immediately.
#[test]
fn propose_threshold_does_not_change_config_immediately() {
    let (env, owner, _cs1, _cs2, _ca, client, vault_id) = setup();

    // Current threshold is 2.
    let config_before = client.get_multisig_config(&vault_id).unwrap();
    assert_eq!(config_before.threshold, 2);

    // Propose lowering to 1.
    client.propose_multisig_threshold(&vault_id, &owner, &1u32);

    // Config threshold must still be 2.
    let config_after = client.get_multisig_config(&vault_id).unwrap();
    assert_eq!(config_after.threshold, 2, "threshold must not change before timelock elapses");

    // A pending proposal must be stored.
    let pending = client.get_pending_multisig_threshold(&vault_id);
    assert!(pending.is_some(), "pending proposal should exist after propose");
    assert_eq!(pending.unwrap().new_threshold, 1);
}

/// Applying a threshold change BEFORE the 24-hour timelock fails with
/// `ThresholdChangeTimeLocked`.
#[test]
#[should_panic]
fn apply_threshold_before_timelock_panics() {
    let (env, owner, _cs1, _cs2, _ca, client, vault_id) = setup();
    client.propose_multisig_threshold(&vault_id, &owner, &1u32);

    // Advance time by only 23 hours — still within the lock window.
    env.ledger()
        .with_mut(|l| l.timestamp = l.timestamp + 82_800);

    // This should panic (ThresholdChangeTimeLocked).
    client.apply_multisig_threshold(&vault_id, &owner);
}

/// Applying a threshold change AFTER the 24-hour timelock succeeds and the
/// config is updated.
#[test]
fn apply_threshold_after_timelock_updates_config() {
    let (env, owner, _cs1, _cs2, _ca, client, vault_id) = setup();
    client.propose_multisig_threshold(&vault_id, &owner, &1u32);

    // Advance time by exactly 24 hours.
    env.ledger()
        .with_mut(|l| l.timestamp = l.timestamp + 86_400);

    client.apply_multisig_threshold(&vault_id, &owner);

    // Threshold must now be 1.
    let config = client.get_multisig_config(&vault_id).unwrap();
    assert_eq!(config.threshold, 1, "threshold should be updated after timelock elapses");

    // Pending proposal must be cleared.
    assert!(
        client.get_pending_multisig_threshold(&vault_id).is_none(),
        "pending proposal should be cleared after apply"
    );
}

/// Applying a threshold change more than 24 hours after the proposal also works.
#[test]
fn apply_threshold_well_after_timelock_succeeds() {
    let (env, owner, _cs1, _cs2, _ca, client, vault_id) = setup();
    client.propose_multisig_threshold(&vault_id, &owner, &3u32);

    // Advance time by 48 hours.
    env.ledger()
        .with_mut(|l| l.timestamp = l.timestamp + 172_800);

    client.apply_multisig_threshold(&vault_id, &owner);
    let config = client.get_multisig_config(&vault_id).unwrap();
    assert_eq!(config.threshold, 3);
}

// ── Cancellation tests ────────────────────────────────────────────────────────

/// The owner can cancel a pending proposal.
#[test]
fn owner_can_cancel_pending_threshold_change() {
    let (_env, owner, _cs1, _cs2, _ca, client, vault_id) = setup();
    client.propose_multisig_threshold(&vault_id, &owner, &1u32);

    client.cancel_multisig_threshold(&vault_id, &owner);

    assert!(
        client.get_pending_multisig_threshold(&vault_id).is_none(),
        "pending proposal should be removed after cancel"
    );
}

/// A co-signer can cancel a pending proposal.
#[test]
fn cosigner_can_cancel_pending_threshold_change() {
    let (_env, owner, cosigner1, _cs2, _ca, client, vault_id) = setup();
    client.propose_multisig_threshold(&vault_id, &owner, &1u32);

    // cosigner1 cancels.
    client.cancel_multisig_threshold(&vault_id, &cosigner1);

    assert!(
        client.get_pending_multisig_threshold(&vault_id).is_none(),
        "co-signer should be able to cancel a pending threshold proposal"
    );
}

/// After cancellation, applying the (now removed) proposal panics.
#[test]
#[should_panic]
fn apply_after_cancel_panics() {
    let (env, owner, _cs1, _cs2, _ca, client, vault_id) = setup();
    client.propose_multisig_threshold(&vault_id, &owner, &1u32);
    client.cancel_multisig_threshold(&vault_id, &owner);

    // Advance past timelock.
    env.ledger()
        .with_mut(|l| l.timestamp = l.timestamp + 86_400);

    // Should panic: NoPendingThresholdChange.
    client.apply_multisig_threshold(&vault_id, &owner);
}

// ── Duplicate proposal guard ───────────────────────────────────────────────────

/// Proposing a second threshold change while one is pending panics with
/// `ThresholdChangePending`.
#[test]
#[should_panic]
fn second_propose_while_pending_panics() {
    let (_env, owner, _cs1, _cs2, _ca, client, vault_id) = setup();
    client.propose_multisig_threshold(&vault_id, &owner, &1u32);
    // Second proposal should panic.
    client.propose_multisig_threshold(&vault_id, &owner, &3u32);
}

// ── Invalid threshold guard ───────────────────────────────────────────────────

/// Proposing threshold = 0 panics with `InvalidThreshold`.
#[test]
#[should_panic]
fn propose_threshold_zero_panics() {
    let (_env, owner, _cs1, _cs2, _ca, client, vault_id) = setup();
    client.propose_multisig_threshold(&vault_id, &owner, &0u32);
}

/// Proposing a threshold greater than total signers (owner + 2 co-signers = 3)
/// panics with `InvalidThreshold`.
#[test]
#[should_panic]
fn propose_threshold_above_total_signers_panics() {
    let (_env, owner, _cs1, _cs2, _ca, client, vault_id) = setup();
    // Total signers = 3 (owner + cosigner1 + cosigner2); 4 is invalid.
    client.propose_multisig_threshold(&vault_id, &owner, &4u32);
}

// ── Query: no pending change ──────────────────────────────────────────────────

/// When no proposal exists, `get_pending_multisig_threshold` returns None.
#[test]
fn get_pending_threshold_returns_none_when_no_proposal() {
    let (_env, _owner, _cs1, _cs2, _ca, client, vault_id) = setup();
    assert!(client.get_pending_multisig_threshold(&vault_id).is_none());
}
