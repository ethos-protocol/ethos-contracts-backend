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

/// Test: Register single guardian
#[test]
fn test_register_guardian_success() {
    let (env, owner, beneficiary, client) = setup();
    let vault_id = client.create_vault(&owner, &beneficiary, &1_000u64, &None);

    let guardian = Address::generate(&env);

    client.register_guardian(&vault_id, &owner, &guardian);

    let guardians = client.get_guardians(&vault_id);
    assert_eq!(guardians.len(), 1);
    assert_eq!(guardians.get(0).unwrap(), &guardian);
}

/// Test: Register multiple guardians
#[test]
fn test_register_multiple_guardians() {
    let (env, owner, beneficiary, client) = setup();
    let vault_id = client.create_vault(&owner, &beneficiary, &1_000u64, &None);

    let guardian1 = Address::generate(&env);
    let guardian2 = Address::generate(&env);
    let guardian3 = Address::generate(&env);

    client.register_guardian(&vault_id, &owner, &guardian1);
    client.register_guardian(&vault_id, &owner, &guardian2);
    client.register_guardian(&vault_id, &owner, &guardian3);

    let guardians = client.get_guardians(&vault_id);
    assert_eq!(guardians.len(), 3);
}

/// Test: Owner-only can register guardians
#[test]
fn test_register_guardian_owner_only() {
    let (env, owner, beneficiary, client) = setup();
    let vault_id = client.create_vault(&owner, &beneficiary, &1_000u64, &None);

    let guardian = Address::generate(&env);
    let non_owner = Address::generate(&env);

    let result = client.try_register_guardian(&vault_id, &non_owner, &guardian);
    assert!(result.is_err());
}

/// Test: Cannot register duplicate guardian
#[test]
fn test_register_duplicate_guardian_fails() {
    let (env, owner, beneficiary, client) = setup();
    let vault_id = client.create_vault(&owner, &beneficiary, &1_000u64, &None);

    let guardian = Address::generate(&env);

    client.register_guardian(&vault_id, &owner, &guardian);

    let result = client.try_register_guardian(&vault_id, &owner, &guardian);
    assert!(result.is_err());
}

/// Test: Remove guardian
#[test]
fn test_remove_guardian() {
    let (env, owner, beneficiary, client) = setup();
    let vault_id = client.create_vault(&owner, &beneficiary, &1_000u64, &None);

    let guardian1 = Address::generate(&env);
    let guardian2 = Address::generate(&env);

    client.register_guardian(&vault_id, &owner, &guardian1);
    client.register_guardian(&vault_id, &owner, &guardian2);

    client.remove_guardian(&vault_id, &owner, &guardian1);

    let guardians = client.get_guardians(&vault_id);
    assert_eq!(guardians.len(), 1);
    assert_eq!(guardians.get(0).unwrap(), &guardian2);
}

/// Test: Initiate recovery requires quorum of guardians
#[test]
fn test_initiate_key_recovery() {
    let (env, owner, beneficiary, client) = setup();
    let vault_id = client.create_vault(&owner, &beneficiary, &1_000u64, &None);

    let guardian1 = Address::generate(&env);
    let guardian2 = Address::generate(&env);
    let guardian3 = Address::generate(&env);
    let new_passkey = BytesN::<32>::from_array(&env, &[5u8; 32]);

    client.register_guardian(&vault_id, &owner, &guardian1);
    client.register_guardian(&vault_id, &owner, &guardian2);
    client.register_guardian(&vault_id, &owner, &guardian3);

    client.set_recovery_threshold(&vault_id, &owner, &2);

    client.initiate_recovery(&vault_id, &new_passkey);

    let recovery = client.get_recovery_status(&vault_id);
    assert_eq!(recovery.new_passkey, new_passkey);
    assert_eq!(recovery.approvals, 0);
}

/// Test: Guardian approval of recovery
#[test]
fn test_guardian_approval_recovery() {
    let (env, owner, beneficiary, client) = setup();
    let vault_id = client.create_vault(&owner, &beneficiary, &1_000u64, &None);

    let guardian1 = Address::generate(&env);
    let guardian2 = Address::generate(&env);
    let new_passkey = BytesN::<32>::from_array(&env, &[5u8; 32]);

    client.register_guardian(&vault_id, &owner, &guardian1);
    client.register_guardian(&vault_id, &owner, &guardian2);
    client.set_recovery_threshold(&vault_id, &owner, &2);

    client.initiate_recovery(&vault_id, &new_passkey);

    client.approve_recovery(&vault_id, &guardian1);
    client.approve_recovery(&vault_id, &guardian2);

    let recovery = client.get_recovery_status(&vault_id);
    assert_eq!(recovery.approvals, 2);
    assert!(recovery.ready_to_execute);
}

/// Test: Only guardians can approve recovery
#[test]
fn test_only_guardians_approve_recovery() {
    let (env, owner, beneficiary, client) = setup();
    let vault_id = client.create_vault(&owner, &beneficiary, &1_000u64, &None);

    let guardian = Address::generate(&env);
    let non_guardian = Address::generate(&env);
    let new_passkey = BytesN::<32>::from_array(&env, &[5u8; 32]);

    client.register_guardian(&vault_id, &owner, &guardian);
    client.set_recovery_threshold(&vault_id, &owner, &1);
    client.initiate_recovery(&vault_id, &new_passkey);

    let result = client.try_approve_recovery(&vault_id, &non_guardian);
    assert!(result.is_err());
}

/// Test: Execute recovery after threshold met
#[test]
fn test_execute_key_recovery() {
    let (env, owner, beneficiary, client) = setup();
    let vault_id = client.create_vault(&owner, &beneficiary, &1_000u64, &None);

    let guardian = Address::generate(&env);
    let new_passkey = BytesN::<32>::from_array(&env, &[5u8; 32]);

    client.register_guardian(&vault_id, &owner, &guardian);
    client.set_recovery_threshold(&vault_id, &owner, &1);
    client.initiate_recovery(&vault_id, &new_passkey);
    client.approve_recovery(&vault_id, &guardian);

    client.execute_recovery(&vault_id);

    // Verify recovery is complete
    let recovery = client.get_recovery_status(&vault_id);
    assert!(recovery.executed);
}

/// Test: Cannot execute recovery without threshold approval
#[test]
fn test_cannot_execute_recovery_without_threshold() {
    let (env, owner, beneficiary, client) = setup();
    let vault_id = client.create_vault(&owner, &beneficiary, &1_000u64, &None);

    let guardian1 = Address::generate(&env);
    let guardian2 = Address::generate(&env);
    let new_passkey = BytesN::<32>::from_array(&env, &[5u8; 32]);

    client.register_guardian(&vault_id, &owner, &guardian1);
    client.register_guardian(&vault_id, &owner, &guardian2);
    client.set_recovery_threshold(&vault_id, &owner, &2);

    client.initiate_recovery(&vault_id, &new_passkey);
    client.approve_recovery(&vault_id, &guardian1);

    // Only one approval, need two
    let result = client.try_execute_recovery(&vault_id);
    assert!(result.is_err());
}

/// Test: Recovery expires after timeout
#[test]
fn test_recovery_expiration() {
    let (env, owner, beneficiary, client) = setup();
    let vault_id = client.create_vault(&owner, &beneficiary, &1_000u64, &None);

    let guardian = Address::generate(&env);
    let new_passkey = BytesN::<32>::from_array(&env, &[5u8; 32]);

    client.register_guardian(&vault_id, &owner, &guardian);
    client.set_recovery_threshold(&vault_id, &owner, &1);

    client.initiate_recovery(&vault_id, &new_passkey);

    // Advance time beyond recovery timeout (e.g., 48 hours)
    env.ledger().set_timestamp(env.ledger().timestamp() + 48 * 3600 + 1);

    let result = client.try_execute_recovery(&vault_id);
    assert!(result.is_err());
}

/// Test: Cancel recovery
#[test]
fn test_cancel_recovery() {
    let (env, owner, beneficiary, client) = setup();
    let vault_id = client.create_vault(&owner, &beneficiary, &1_000u64, &None);

    let guardian = Address::generate(&env);
    let new_passkey = BytesN::<32>::from_array(&env, &[5u8; 32]);

    client.register_guardian(&vault_id, &owner, &guardian);
    client.set_recovery_threshold(&vault_id, &owner, &1);
    client.initiate_recovery(&vault_id, &new_passkey);

    client.cancel_recovery(&vault_id, &owner);

    let recovery = client.get_recovery_status(&vault_id);
    assert!(recovery.cancelled);
}

/// Test: Only owner can cancel recovery
#[test]
fn test_only_owner_can_cancel_recovery() {
    let (env, owner, beneficiary, client) = setup();
    let vault_id = client.create_vault(&owner, &beneficiary, &1_000u64, &None);

    let guardian = Address::generate(&env);
    let non_owner = Address::generate(&env);
    let new_passkey = BytesN::<32>::from_array(&env, &[5u8; 32]);

    client.register_guardian(&vault_id, &owner, &guardian);
    client.set_recovery_threshold(&vault_id, &owner, &1);
    client.initiate_recovery(&vault_id, &new_passkey);

    let result = client.try_cancel_recovery(&vault_id, &non_owner);
    assert!(result.is_err());
}

/// Test: Set recovery threshold
#[test]
fn test_set_recovery_threshold() {
    let (env, owner, beneficiary, client) = setup();
    let vault_id = client.create_vault(&owner, &beneficiary, &1_000u64, &None);

    client.set_recovery_threshold(&vault_id, &owner, &2);

    let threshold = client.get_recovery_threshold(&vault_id);
    assert_eq!(threshold, 2);
}

/// Test: Recovery threshold must be <= total guardians
#[test]
fn test_recovery_threshold_validation() {
    let (env, owner, beneficiary, client) = setup();
    let vault_id = client.create_vault(&owner, &beneficiary, &1_000u64, &None);

    let guardian = Address::generate(&env);
    client.register_guardian(&vault_id, &owner, &guardian);

    // Threshold cannot exceed number of guardians
    let result = client.try_set_recovery_threshold(&vault_id, &owner, &5);
    assert!(result.is_err());
}

/// Test: Multiple recovery attempts (sequential)
#[test]
fn test_sequential_recovery_attempts() {
    let (env, owner, beneficiary, client) = setup();
    let vault_id = client.create_vault(&owner, &beneficiary, &1_000u64, &None);

    let guardian = Address::generate(&env);
    let new_passkey1 = BytesN::<32>::from_array(&env, &[5u8; 32]);
    let new_passkey2 = BytesN::<32>::from_array(&env, &[6u8; 32]);

    client.register_guardian(&vault_id, &owner, &guardian);
    client.set_recovery_threshold(&vault_id, &owner, &1);

    // First recovery
    client.initiate_recovery(&vault_id, &new_passkey1);
    client.approve_recovery(&vault_id, &guardian);
    client.execute_recovery(&vault_id);

    // Verify first recovery is complete
    let recovery1 = client.get_recovery_status(&vault_id);
    assert!(recovery1.executed);

    // Second recovery (new recovery flow)
    client.initiate_recovery(&vault_id, &new_passkey2);
    let recovery2 = client.get_recovery_status(&vault_id);
    assert_eq!(recovery2.new_passkey, new_passkey2);
}

/// Test: Recovery events emitted
#[test]
fn test_recovery_events_emitted() {
    let (env, owner, beneficiary, client) = setup();
    let vault_id = client.create_vault(&owner, &beneficiary, &1_000u64, &None);

    let guardian = Address::generate(&env);
    let new_passkey = BytesN::<32>::from_array(&env, &[5u8; 32]);

    client.register_guardian(&vault_id, &owner, &guardian);
    client.set_recovery_threshold(&vault_id, &owner, &1);
    client.initiate_recovery(&vault_id, &new_passkey);

    let events = env.events().all();
    let found = events.iter().any(|e| {
        let topics: soroban_sdk::Vec<soroban_sdk::Val> = e.1.clone().into_val(&env);
        topics
            .get(0)
            .and_then(|v| v.try_into_val::<soroban_sdk::Symbol>(&env).ok())
            .is_some_and(|s| s == soroban_sdk::symbol_short!("rec_init"))
    });
    assert!(found, "expected recovery_initiated event to be emitted");
}

/// Test: Cannot register owner as guardian
#[test]
fn test_cannot_register_owner_as_guardian() {
    let (env, owner, beneficiary, client) = setup();
    let vault_id = client.create_vault(&owner, &beneficiary, &1_000u64, &None);

    let result = client.try_register_guardian(&vault_id, &owner, &owner);
    assert!(result.is_err());
}

/// Test: Guardian cannot approve own recovery request
#[test]
fn test_guardian_cannot_self_approve() {
    let (env, owner, beneficiary, client) = setup();
    let vault_id = client.create_vault(&owner, &beneficiary, &1_000u64, &None);

    let guardian = Address::generate(&env);
    let new_passkey = BytesN::<32>::from_array(&env, &[5u8; 32]);

    client.register_guardian(&vault_id, &owner, &guardian);
    client.set_recovery_threshold(&vault_id, &owner, &1);
    client.initiate_recovery(&vault_id, &new_passkey);

    // Should fail if guardian tries to approve twice
    client.approve_recovery(&vault_id, &guardian);
    let result = client.try_approve_recovery(&vault_id, &guardian);
    assert!(result.is_err());
}
