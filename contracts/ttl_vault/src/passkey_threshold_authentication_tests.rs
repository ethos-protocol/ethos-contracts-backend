#![cfg(test)]

extern crate alloc;

use super::*;
use soroban_sdk::{
    testutils::{Address as _, Ledger},
    token::StellarAssetClient,
    vec, Address, BytesN, Env, IntoVal,
};

fn setup() -> (Env, Address, Address, TtlVaultContractClient<'static>) {
    let env = Env::default();
    env.mock_all_auths();

    let owner = Address::generate(&env);
    let beneficiary = Address::generate(&env);
    let admin = Address::generate(&env);

    let token_admin = Address::generate(&env);
    let token_address = env
        .register_stellar_asset_contract_v2(token_admin)
        .address();
    StellarAssetClient::new(&env, &token_address).mint(&owner, &1_000_000);

    let contract_address = env.register_contract(None, TtlVaultContract);
    let client = TtlVaultContractClient::new(&env, &contract_address);
    client.initialize(&token_address, &admin);

    let client: TtlVaultContractClient<'static> = unsafe { core::mem::transmute(client) };
    (env, owner, beneficiary, client)
}

/// Test: Set passkey threshold successfully
#[test]
fn test_set_passkey_threshold_success() {
    let (env, owner, beneficiary, client) = setup();
    let vault_id = client.create_vault(&owner, &beneficiary, &1_000u64, &None);

    let hash1 = BytesN::<32>::from_array(&env, &[1u8; 32]);
    let hash2 = BytesN::<32>::from_array(&env, &[2u8; 32]);
    let hash3 = BytesN::<32>::from_array(&env, &[3u8; 32]);

    client.add_passkey(&vault_id, &owner, &hash1);
    client.add_passkey(&vault_id, &owner, &hash2);
    client.add_passkey(&vault_id, &owner, &hash3);

    client.set_passkey_threshold(&vault_id, &owner, &2);

    let threshold = client.get_passkey_threshold(&vault_id);
    assert_eq!(threshold, Some(2));
}

/// Test: Owner can set threshold
#[test]
fn test_set_passkey_threshold_owner_only() {
    let (env, owner, beneficiary, client) = setup();
    let vault_id = client.create_vault(&owner, &beneficiary, &1_000u64, &None);
    let other = Address::generate(&env);

    let hash1 = BytesN::<32>::from_array(&env, &[1u8; 32]);
    let hash2 = BytesN::<32>::from_array(&env, &[2u8; 32]);

    client.add_passkey(&vault_id, &owner, &hash1);
    client.add_passkey(&vault_id, &owner, &hash2);

    let result = client.try_set_passkey_threshold(&vault_id, &other, &2);
    assert!(result.is_err());
}

/// Test: Threshold must be valid (1 <= threshold <= total_passkeys)
#[test]
fn test_set_passkey_threshold_validation() {
    let (env, owner, beneficiary, client) = setup();
    let vault_id = client.create_vault(&owner, &beneficiary, &1_000u64, &None);

    let hash1 = BytesN::<32>::from_array(&env, &[1u8; 32]);
    let hash2 = BytesN::<32>::from_array(&env, &[2u8; 32]);

    client.add_passkey(&vault_id, &owner, &hash1);
    client.add_passkey(&vault_id, &owner, &hash2);

    // Threshold of 0 should fail
    let result = client.try_set_passkey_threshold(&vault_id, &owner, &0);
    assert!(result.is_err());

    // Threshold > total passkeys should fail
    let result = client.try_set_passkey_threshold(&vault_id, &owner, &3);
    assert!(result.is_err());
}

/// Test: Check-in requires threshold approvals
#[test]
fn test_check_in_requires_threshold_approvals() {
    let (env, owner, beneficiary, client) = setup();
    let vault_id = client.create_vault(&owner, &beneficiary, &1_000u64, &None);

    let hash1 = BytesN::<32>::from_array(&env, &[1u8; 32]);
    let hash2 = BytesN::<32>::from_array(&env, &[2u8; 32]);
    let hash3 = BytesN::<32>::from_array(&env, &[3u8; 32]);

    client.add_passkey(&vault_id, &owner, &hash1);
    client.add_passkey(&vault_id, &owner, &hash2);
    client.add_passkey(&vault_id, &owner, &hash3);

    client.set_passkey_threshold(&vault_id, &owner, &2);

    // First approval
    client.check_in(&vault_id, &owner, &hash1, &0);

    // Second approval should succeed
    client.check_in(&vault_id, &owner, &hash2, &0);

    let vault = client.get_vault(&vault_id);
    assert!(vault.passkey_threshold_met);
}

/// Test: Threshold not met with insufficient approvals
#[test]
fn test_check_in_insufficient_approvals() {
    let (env, owner, beneficiary, client) = setup();
    let vault_id = client.create_vault(&owner, &beneficiary, &1_000u64, &None);

    let hash1 = BytesN::<32>::from_array(&env, &[1u8; 32]);
    let hash2 = BytesN::<32>::from_array(&env, &[2u8; 32]);

    client.add_passkey(&vault_id, &owner, &hash1);
    client.add_passkey(&vault_id, &owner, &hash2);

    client.set_passkey_threshold(&vault_id, &owner, &2);

    // Only one approval
    client.check_in(&vault_id, &owner, &hash1, &0);

    let vault = client.get_vault(&vault_id);
    assert!(!vault.passkey_threshold_met);
}

/// Test: Threshold is cleared on vault release
#[test]
fn test_threshold_cleared_on_release() {
    let (env, owner, beneficiary, client) = setup();
    let vault_id = client.create_vault(&owner, &beneficiary, &1_000u64, &None);

    let hash1 = BytesN::<32>::from_array(&env, &[1u8; 32]);
    let hash2 = BytesN::<32>::from_array(&env, &[2u8; 32]);

    client.add_passkey(&vault_id, &owner, &hash1);
    client.add_passkey(&vault_id, &owner, &hash2);

    client.set_passkey_threshold(&vault_id, &owner, &2);

    client.check_in(&vault_id, &owner, &hash1, &0);
    client.check_in(&vault_id, &owner, &hash2, &0);

    // Advance time and release
    env.ledger().set_timestamp(1_000_000_000);
    client.trigger_release(&vault_id);

    // Threshold should be cleared
    let vault = client.get_vault(&vault_id);
    assert!(!vault.passkey_threshold_met);
}

/// Test: Threshold can be updated
#[test]
fn test_update_passkey_threshold() {
    let (env, owner, beneficiary, client) = setup();
    let vault_id = client.create_vault(&owner, &beneficiary, &1_000u64, &None);

    let hash1 = BytesN::<32>::from_array(&env, &[1u8; 32]);
    let hash2 = BytesN::<32>::from_array(&env, &[2u8; 32]);
    let hash3 = BytesN::<32>::from_array(&env, &[3u8; 32]);

    client.add_passkey(&vault_id, &owner, &hash1);
    client.add_passkey(&vault_id, &owner, &hash2);
    client.add_passkey(&vault_id, &owner, &hash3);

    client.set_passkey_threshold(&vault_id, &owner, &2);
    assert_eq!(client.get_passkey_threshold(&vault_id), Some(2));

    client.set_passkey_threshold(&vault_id, &owner, &3);
    assert_eq!(client.get_passkey_threshold(&vault_id), Some(3));
}

/// Test: Passkey removal validates threshold
#[test]
fn test_remove_passkey_validates_threshold() {
    let (env, owner, beneficiary, client) = setup();
    let vault_id = client.create_vault(&owner, &beneficiary, &1_000u64, &None);

    let hash1 = BytesN::<32>::from_array(&env, &[1u8; 32]);
    let hash2 = BytesN::<32>::from_array(&env, &[2u8; 32]);
    let hash3 = BytesN::<32>::from_array(&env, &[3u8; 32]);

    client.add_passkey(&vault_id, &owner, &hash1);
    client.add_passkey(&vault_id, &owner, &hash2);
    client.add_passkey(&vault_id, &owner, &hash3);

    client.set_passkey_threshold(&vault_id, &owner, &2);

    // Remove one passkey - threshold should still be valid
    client.remove_passkey(&vault_id, &owner, &hash3);

    let threshold = client.get_passkey_threshold(&vault_id);
    assert_eq!(threshold, Some(2));
}

/// Test: Cannot set threshold without enough passkeys
#[test]
fn test_set_threshold_requires_passkeys() {
    let (env, owner, beneficiary, client) = setup();
    let vault_id = client.create_vault(&owner, &beneficiary, &1_000u64, &None);

    // Try to set threshold on vault with no passkeys
    let result = client.try_set_passkey_threshold(&vault_id, &owner, &1);
    assert!(result.is_err());
}

/// Test: Threshold approval state per passkey
#[test]
fn test_threshold_approval_tracking() {
    let (env, owner, beneficiary, client) = setup();
    let vault_id = client.create_vault(&owner, &beneficiary, &1_000u64, &None);

    let hash1 = BytesN::<32>::from_array(&env, &[1u8; 32]);
    let hash2 = BytesN::<32>::from_array(&env, &[2u8; 32]);

    client.add_passkey(&vault_id, &owner, &hash1);
    client.add_passkey(&vault_id, &owner, &hash2);

    client.set_passkey_threshold(&vault_id, &owner, &2);

    client.check_in(&vault_id, &owner, &hash1, &0);

    // Get approval state
    let approvals = client.get_passkey_threshold_approvals(&vault_id);
    assert_eq!(approvals.len(), 1);
    assert_eq!(approvals.get(0).unwrap(), &hash1);
}

/// Test: Event emission for threshold setting
#[test]
fn test_passkey_threshold_event_emission() {
    let (env, owner, beneficiary, client) = setup();
    let vault_id = client.create_vault(&owner, &beneficiary, &1_000u64, &None);

    let hash1 = BytesN::<32>::from_array(&env, &[1u8; 32]);
    let hash2 = BytesN::<32>::from_array(&env, &[2u8; 32]);

    client.add_passkey(&vault_id, &owner, &hash1);
    client.add_passkey(&vault_id, &owner, &hash2);

    client.set_passkey_threshold(&vault_id, &owner, &2);

    let events = env.events().all();
    let found = events.iter().any(|e| {
        let topics: soroban_sdk::Vec<soroban_sdk::Val> = e.1.clone().into_val(&env);
        topics
            .get(0)
            .and_then(|v| v.try_into_val::<soroban_sdk::Symbol>(&env).ok())
            .is_some_and(|s| s == soroban_sdk::symbol_short!("pk_thold"))
    });
    assert!(found, "expected passkey_threshold event to be emitted");
}

/// Test: Threshold reset on add/remove passkey
#[test]
fn test_threshold_adjustment_on_passkey_changes() {
    let (env, owner, beneficiary, client) = setup();
    let vault_id = client.create_vault(&owner, &beneficiary, &1_000u64, &None);

    let hash1 = BytesN::<32>::from_array(&env, &[1u8; 32]);
    let hash2 = BytesN::<32>::from_array(&env, &[2u8; 32]);
    let hash3 = BytesN::<32>::from_array(&env, &[3u8; 32]);

    client.add_passkey(&vault_id, &owner, &hash1);
    client.add_passkey(&vault_id, &owner, &hash2);

    client.set_passkey_threshold(&vault_id, &owner, &2);

    // Add another passkey
    client.add_passkey(&vault_id, &owner, &hash3);

    // Threshold should still be 2 (valid with 3 passkeys)
    assert_eq!(client.get_passkey_threshold(&vault_id), Some(2));
}

/// Test: Single passkey vault with threshold of 1
#[test]
fn test_single_passkey_threshold() {
    let (env, owner, beneficiary, client) = setup();
    let vault_id = client.create_vault(&owner, &beneficiary, &1_000u64, &None);

    let hash1 = BytesN::<32>::from_array(&env, &[1u8; 32]);

    client.add_passkey(&vault_id, &owner, &hash1);
    client.set_passkey_threshold(&vault_id, &owner, &1);

    client.check_in(&vault_id, &owner, &hash1, &0);

    let vault = client.get_vault(&vault_id);
    assert!(vault.passkey_threshold_met);
}
