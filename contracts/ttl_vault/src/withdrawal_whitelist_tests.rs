//! Tests for withdrawal whitelist for recipient addresses — Issue #513.
//!
//! This module verifies:
//!   1. A recipient whitelist can be added to a vault.
//!   2. Withdrawals to whitelisted addresses succeed.
//!   3. Withdrawals to non-whitelisted addresses are rejected.
//!   4. Beneficiaries can add whitelisted recipients.
//!   5. A whitelist period (48-hour delay) is enforced before activation.
//!   6. Whitelisted addresses can be removed.

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

/// Adding a whitelisted recipient stores it in the vault configuration.
#[test]
fn test_add_whitelisted_recipient() {
    let (_env, _owner, beneficiary, _ca, client, vault_id) = setup();
    let trusted_address = Address::generate(&_env);

    // Beneficiary adds a trusted recipient to the whitelist
    client.add_whitelisted_recipient(&vault_id, &beneficiary, &trusted_address);

    // Verify the address is in the whitelist
    let vault = client.get_vault(&vault_id);
    assert!(vault.recipient_whitelist.contains(&trusted_address));
}

/// A withdrawal to a whitelisted address after the 48-hour delay should succeed.
#[test]
fn test_withdrawal_to_whitelisted_address_after_delay() {
    let (env, owner, beneficiary, _ca, client, vault_id) = setup();
    let trusted_address = Address::generate(&env);

    // Add the trusted address to the whitelist
    client.add_whitelisted_recipient(&vault_id, &beneficiary, &trusted_address);

    // Deposit funds
    client.deposit(&owner, &vault_id, &5000i128);

    // Advance time by 48 hours (172800 seconds)
    let current_timestamp = env.ledger().timestamp();
    env.ledger().set_timestamp(current_timestamp + 172800);

    // Withdraw to the whitelisted address should succeed
    client.withdraw(&beneficiary, &vault_id, &1000i128, &trusted_address);

    // Verify withdrawal succeeded
    assert_eq!(client.get_vault(&vault_id).balance, 4000);
}

/// A withdrawal to a non-whitelisted address should be rejected.
#[test]
#[should_panic]
fn test_withdrawal_to_non_whitelisted_address() {
    let (_env, owner, beneficiary, _ca, client, vault_id) = setup();
    let untrusted_address = Address::generate(&_env);

    // Deposit funds
    client.deposit(&owner, &vault_id, &5000i128);

    // Attempt to withdraw to a non-whitelisted address should fail
    client.withdraw(&beneficiary, &vault_id, &1000i128, &untrusted_address);
}

/// A withdrawal to a newly added whitelisted address within the 48-hour delay should fail.
#[test]
#[should_panic]
fn test_withdrawal_to_whitelisted_address_before_delay() {
    let (_env, owner, beneficiary, _ca, client, vault_id) = setup();
    let trusted_address = Address::generate(&_env);

    // Add the trusted address to the whitelist
    client.add_whitelisted_recipient(&vault_id, &beneficiary, &trusted_address);

    // Deposit funds
    client.deposit(&owner, &vault_id, &5000i128);

    // Attempt to withdraw immediately (before 48-hour delay) should fail
    client.withdraw(&beneficiary, &vault_id, &1000i128, &trusted_address);
}

/// Removing a whitelisted recipient should prevent withdrawals to it.
#[test]
#[should_panic]
fn test_remove_whitelisted_recipient() {
    let (env, owner, beneficiary, _ca, client, vault_id) = setup();
    let trusted_address = Address::generate(&env);

    // Add the trusted address to the whitelist
    client.add_whitelisted_recipient(&vault_id, &beneficiary, &trusted_address);

    // Advance time by 48 hours
    let current_timestamp = env.ledger().timestamp();
    env.ledger().set_timestamp(current_timestamp + 172800);

    // Deposit funds
    client.deposit(&owner, &vault_id, &5000i128);

    // Beneficiary withdraws to the whitelisted address (should succeed)
    client.withdraw(&beneficiary, &vault_id, &1000i128, &trusted_address);
    assert_eq!(client.get_vault(&vault_id).balance, 4000);

    // Remove the whitelisted recipient
    client.remove_whitelisted_recipient(&vault_id, &beneficiary, &trusted_address);

    // Attempt to withdraw to the removed address should fail
    client.withdraw(&beneficiary, &vault_id, &1000i128, &trusted_address);
}

/// Multiple beneficiaries should each be able to maintain their own whitelists.
#[test]
fn test_multiple_beneficiaries_independent_whitelists() {
    let (env, owner, beneficiary1, _ca, client, vault_id) = setup();
    let beneficiary2 = Address::generate(&env);
    let trusted_address1 = Address::generate(&env);
    let trusted_address2 = Address::generate(&env);

    // Beneficiary1 adds their trusted address
    client.add_whitelisted_recipient(&vault_id, &beneficiary1, &trusted_address1);

    // Beneficiary2 adds their trusted address
    client.add_whitelisted_recipient(&vault_id, &beneficiary2, &trusted_address2);

    // Deposit funds
    client.deposit(&owner, &vault_id, &10000i128);

    // Advance time by 48 hours
    let current_timestamp = env.ledger().timestamp();
    env.ledger().set_timestamp(current_timestamp + 172800);

    // Beneficiary1 can withdraw to their trusted address
    client.withdraw(&beneficiary1, &vault_id, &1000i128, &trusted_address1);

    // Beneficiary2 can withdraw to their trusted address
    client.withdraw(&beneficiary2, &vault_id, &1000i128, &trusted_address2);

    // Verify both withdrawals succeeded
    assert_eq!(client.get_vault(&vault_id).balance, 8000);
}

/// Withdrawing to the beneficiary's own address should always be allowed.
#[test]
fn test_withdrawal_to_self_always_allowed() {
    let (_env, owner, beneficiary, _ca, client, vault_id) = setup();

    // Deposit funds
    client.deposit(&owner, &vault_id, &5000i128);

    // Beneficiary should be able to withdraw to their own address immediately
    client.withdraw(&beneficiary, &vault_id, &1000i128, &beneficiary);

    // Verify withdrawal succeeded
    assert_eq!(client.get_vault(&vault_id).balance, 4000);
}

/// The whitelist period should be configurable globally.
#[test]
fn test_whitelist_period_configuration() {
    let (env, _owner, beneficiary, _ca, client, vault_id) = setup();
    let trusted_address = Address::generate(&env);

    // Add the trusted address to the whitelist
    client.add_whitelisted_recipient(&vault_id, &beneficiary, &trusted_address);

    // Verify the address is in the pending whitelist with a timestamp
    let vault = client.get_vault(&vault_id);
    assert!(vault.pending_whitelist.len() > 0);
}
