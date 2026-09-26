//! Tests for withdrawal rollback capability — Issue #515.
//!
//! This module verifies:
//!   1. A vault can enable withdrawal rollback capability.
//!   2. Executed withdrawals can be rolled back within a 1-hour window.
//!   3. Rollback restores funds to the vault.
//!   4. Rollback is only available to the beneficiary who initiated the withdrawal.
//!   5. Rollbacks outside the 1-hour window are rejected.
//!   6. Multiple rollbacks can occur within the grace period.

#![cfg(test)]

use super::*;
use soroban_sdk::{
    testutils::{Address as _, Ledger},
    token::StellarAssetClient,
    vec, Address, Env,
};

fn setup() -> (
    Env,
    Address, // owner
    Address, // beneficiary
    Address, // contract address
    TtlVaultContractClient<'static>,
    u64,     // vault_id
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

    let vault_id = client.create_vault(&owner, &beneficiary, &604_800u64, &None);

    let client: TtlVaultContractClient<'static> = unsafe { core::mem::transmute(client) };
    (env, owner, beneficiary, contract_address, client, vault_id)
}

/// Enabling rollback capability on a vault stores the configuration.
#[test]
fn test_enable_withdrawal_rollback() {
    let (_env, owner, _beneficiary, _ca, client, vault_id) = setup();

    // Enable rollback on the vault
    client.enable_withdrawal_rollback(&vault_id, &owner);

    // Verify rollback is enabled
    let vault = client.get_vault(&vault_id);
    assert!(vault.rollback_enabled);
}

/// A withdrawal can be rolled back within the 1-hour grace period.
#[test]
fn test_rollback_withdrawal_within_grace_period() {
    let (env, owner, beneficiary, _ca, client, vault_id) = setup();

    // Enable rollback on the vault
    client.enable_withdrawal_rollback(&vault_id, &owner);

    // Deposit 5000 tokens
    client.deposit(&owner, &vault_id, &5000i128);
    assert_eq!(client.get_vault(&vault_id).balance, 5000);

    // Beneficiary withdraws 1000 tokens
    let withdrawal_id = client.withdraw(&beneficiary, &vault_id, &1000i128, &beneficiary);
    assert_eq!(client.get_vault(&vault_id).balance, 4000);

    // Beneficiary rolls back the withdrawal within the 1-hour window
    client.rollback_withdrawal(&vault_id, &beneficiary, &withdrawal_id);

    // Verify funds are restored
    assert_eq!(client.get_vault(&vault_id).balance, 5000);
}

/// A rollback outside the 1-hour grace period should be rejected.
#[test]
#[should_panic]
fn test_rollback_outside_grace_period() {
    let (env, owner, beneficiary, _ca, client, vault_id) = setup();

    // Enable rollback on the vault
    client.enable_withdrawal_rollback(&vault_id, &owner);

    // Deposit 5000 tokens
    client.deposit(&owner, &vault_id, &5000i128);

    // Beneficiary withdraws 1000 tokens
    let withdrawal_id = client.withdraw(&beneficiary, &vault_id, &1000i128, &beneficiary);
    assert_eq!(client.get_vault(&vault_id).balance, 4000);

    // Advance time by 1+ hours (3600 seconds)
    let current_timestamp = env.ledger().timestamp();
    env.ledger().set_timestamp(current_timestamp + 3601);

    // Attempt to rollback after grace period — should fail
    client.rollback_withdrawal(&vault_id, &beneficiary, &withdrawal_id);
}

/// Only the beneficiary who initiated the withdrawal can roll it back.
#[test]
#[should_panic]
fn test_rollback_only_by_original_beneficiary() {
    let (env, owner, beneficiary, _ca, client, vault_id) = setup();
    let other_beneficiary = Address::generate(&env);

    // Enable rollback on the vault
    client.enable_withdrawal_rollback(&vault_id, &owner);

    // Deposit 5000 tokens
    client.deposit(&owner, &vault_id, &5000i128);

    // Beneficiary withdraws 1000 tokens
    let withdrawal_id = client.withdraw(&beneficiary, &vault_id, &1000i128, &beneficiary);
    assert_eq!(client.get_vault(&vault_id).balance, 4000);

    // Another beneficiary attempts to rollback — should fail
    client.rollback_withdrawal(&vault_id, &other_beneficiary, &withdrawal_id);
}

/// Multiple withdrawals can be rolled back sequentially within the grace period.
#[test]
fn test_multiple_rollbacks_within_grace_period() {
    let (env, owner, beneficiary, _ca, client, vault_id) = setup();

    // Enable rollback on the vault
    client.enable_withdrawal_rollback(&vault_id, &owner);

    // Deposit 10000 tokens
    client.deposit(&owner, &vault_id, &10000i128);

    // First withdrawal: 1000 tokens
    let withdrawal_id1 = client.withdraw(&beneficiary, &vault_id, &1000i128, &beneficiary);
    assert_eq!(client.get_vault(&vault_id).balance, 9000);

    // Second withdrawal: 2000 tokens
    let withdrawal_id2 = client.withdraw(&beneficiary, &vault_id, &2000i128, &beneficiary);
    assert_eq!(client.get_vault(&vault_id).balance, 7000);

    // Rollback first withdrawal
    client.rollback_withdrawal(&vault_id, &beneficiary, &withdrawal_id1);
    assert_eq!(client.get_vault(&vault_id).balance, 8000);

    // Rollback second withdrawal
    client.rollback_withdrawal(&vault_id, &beneficiary, &withdrawal_id2);
    assert_eq!(client.get_vault(&vault_id).balance, 10000);
}

/// A withdrawal cannot be rolled back twice.
#[test]
#[should_panic]
fn test_rollback_twice_fails() {
    let (env, owner, beneficiary, _ca, client, vault_id) = setup();

    // Enable rollback on the vault
    client.enable_withdrawal_rollback(&vault_id, &owner);

    // Deposit 5000 tokens
    client.deposit(&owner, &vault_id, &5000i128);

    // Beneficiary withdraws 1000 tokens
    let withdrawal_id = client.withdraw(&beneficiary, &vault_id, &1000i128, &beneficiary);
    assert_eq!(client.get_vault(&vault_id).balance, 4000);

    // First rollback succeeds
    client.rollback_withdrawal(&vault_id, &beneficiary, &withdrawal_id);
    assert_eq!(client.get_vault(&vault_id).balance, 5000);

    // Second rollback of the same withdrawal should fail
    client.rollback_withdrawal(&vault_id, &beneficiary, &withdrawal_id);
}

/// Disabling rollback prevents further rollbacks.
#[test]
#[should_panic]
fn test_rollback_blocked_when_disabled() {
    let (env, owner, beneficiary, _ca, client, vault_id) = setup();

    // Enable rollback on the vault
    client.enable_withdrawal_rollback(&vault_id, &owner);

    // Deposit 5000 tokens
    client.deposit(&owner, &vault_id, &5000i128);

    // Beneficiary withdraws 1000 tokens
    let withdrawal_id = client.withdraw(&beneficiary, &vault_id, &1000i128, &beneficiary);
    assert_eq!(client.get_vault(&vault_id).balance, 4000);

    // Owner disables rollback
    client.disable_withdrawal_rollback(&vault_id, &owner);

    // Attempt to rollback should fail
    client.rollback_withdrawal(&vault_id, &beneficiary, &withdrawal_id);
}

/// Rollback is unavailable when not explicitly enabled.
#[test]
#[should_panic]
fn test_rollback_not_available_by_default() {
    let (_env, owner, beneficiary, _ca, client, vault_id) = setup();

    // Do NOT enable rollback

    // Deposit 5000 tokens
    client.deposit(&owner, &vault_id, &5000i128);

    // Beneficiary withdraws 1000 tokens
    let withdrawal_id = client.withdraw(&beneficiary, &vault_id, &1000i128, &beneficiary);
    assert_eq!(client.get_vault(&vault_id).balance, 4000);

    // Attempt to rollback should fail (not enabled)
    client.rollback_withdrawal(&vault_id, &beneficiary, &withdrawal_id);
}

/// Rolling back a large withdrawal restores the correct amount.
#[test]
fn test_rollback_large_withdrawal() {
    let (env, owner, beneficiary, _ca, client, vault_id) = setup();

    // Enable rollback on the vault
    client.enable_withdrawal_rollback(&vault_id, &owner);

    // Deposit 50000 tokens
    client.deposit(&owner, &vault_id, &50000i128);
    assert_eq!(client.get_vault(&vault_id).balance, 50000);

    // Beneficiary withdraws 25000 tokens
    let withdrawal_id = client.withdraw(&beneficiary, &vault_id, &25000i128, &beneficiary);
    assert_eq!(client.get_vault(&vault_id).balance, 25000);

    // Beneficiary rolls back the large withdrawal
    client.rollback_withdrawal(&vault_id, &beneficiary, &withdrawal_id);

    // Verify all funds are restored
    assert_eq!(client.get_vault(&vault_id).balance, 50000);
}

/// Rollback grace period should be configurable per vault.
#[test]
fn test_rollback_grace_period_configuration() {
    let (env, owner, beneficiary, _ca, client, vault_id) = setup();

    // Enable rollback with custom grace period
    client.enable_withdrawal_rollback_with_period(&vault_id, &owner, &7200u64); // 2 hours

    // Deposit 5000 tokens
    client.deposit(&owner, &vault_id, &5000i128);

    // Beneficiary withdraws 1000 tokens
    let withdrawal_id = client.withdraw(&beneficiary, &vault_id, &1000i128, &beneficiary);

    // Advance time by 1 hour (3600 seconds)
    let current_timestamp = env.ledger().timestamp();
    env.ledger().set_timestamp(current_timestamp + 3600);

    // Rollback should succeed with the extended 2-hour period
    client.rollback_withdrawal(&vault_id, &beneficiary, &withdrawal_id);
    assert_eq!(client.get_vault(&vault_id).balance, 5000);
}
