#![cfg(test)]

extern crate alloc;

use super::*;
use soroban_sdk::{
    testutils::{Address as _, Events},
    token::StellarAssetClient,
    vec, Address, BytesN, Env, IntoVal, String as SorobanString, TryIntoVal,
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

/// Test: Audit log filters by operation type
#[test]
fn test_audit_log_filter_by_operation() {
    let (env, owner, beneficiary, client) = setup();
    let vault_id = client.create_vault(&owner, &beneficiary, &1_000u64, &None);

    let hash1 = BytesN::<32>::from_array(&env, &[1u8; 32]);
    let hash2 = BytesN::<32>::from_array(&env, &[2u8; 32]);

    // Add operations
    client.add_passkey(&vault_id, &owner, &hash1);
    client.add_passkey(&vault_id, &owner, &hash2);

    // Use operation
    client.check_in(&vault_id, &owner, &hash1, &0);

    // Get full log
    let log = client.get_passkey_audit_log(&vault_id);
    assert_eq!(log.len(), 3);

    // Filter adds
    let adds = log
        .iter()
        .filter(|e| e.operation == SorobanString::from_str(&env, "add"))
        .count();
    assert_eq!(adds, 2);

    // Filter uses
    let uses = log
        .iter()
        .filter(|e| e.operation == SorobanString::from_str(&env, "use"))
        .count();
    assert_eq!(uses, 1);
}

/// Test: Audit log tracks timestamp for each entry
#[test]
fn test_audit_log_timestamps() {
    let (env, owner, beneficiary, client) = setup();
    let vault_id = client.create_vault(&owner, &beneficiary, &1_000u64, &None);

    let hash1 = BytesN::<32>::from_array(&env, &[1u8; 32]);

    env.ledger().set_timestamp(100);
    client.add_passkey(&vault_id, &owner, &hash1);

    env.ledger().set_timestamp(200);
    client.check_in(&vault_id, &owner, &hash1, &0);

    let log = client.get_passkey_audit_log(&vault_id);
    assert!(log.get(0).unwrap().timestamp <= log.get(1).unwrap().timestamp);
}

/// Test: Audit log entry includes actor information
#[test]
fn test_audit_log_tracks_actor() {
    let (env, owner, beneficiary, client) = setup();
    let vault_id = client.create_vault(&owner, &beneficiary, &1_000u64, &None);

    let hash1 = BytesN::<32>::from_array(&env, &[1u8; 32]);

    client.add_passkey(&vault_id, &owner, &hash1);
    client.check_in(&vault_id, &owner, &hash1, &0);

    let log = client.get_passkey_audit_log(&vault_id);

    // Both operations should be by owner
    for entry in log.iter() {
        assert_eq!(entry.actor, owner);
    }
}

/// Test: Multiple actors tracked separately
#[test]
fn test_audit_log_multiple_actors() {
    let (env, owner, beneficiary, client) = setup();
    let vault_id = client.create_vault(&owner, &beneficiary, &1_000u64, &None);

    let hash1 = BytesN::<32>::from_array(&env, &[1u8; 32]);
    let delegate = Address::generate(&env);

    client.add_passkey(&vault_id, &owner, &hash1);

    // Delegate check-in (would require delegation first)
    client.delegate_checkin(&vault_id, &owner, &delegate);
    client.check_in(&vault_id, &delegate, &hash1, &0);

    let log = client.get_passkey_audit_log(&vault_id);
    assert!(log.iter().any(|e| e.actor == owner));
    assert!(log.iter().any(|e| e.actor == delegate));
}

/// Test: Audit log immutability - cannot modify entries
#[test]
fn test_audit_log_immutability() {
    let (env, owner, beneficiary, client) = setup();
    let vault_id = client.create_vault(&owner, &beneficiary, &1_000u64, &None);

    let hash1 = BytesN::<32>::from_array(&env, &[1u8; 32]);

    client.add_passkey(&vault_id, &owner, &hash1);
    let log_v1 = client.get_passkey_audit_log(&vault_id);

    client.check_in(&vault_id, &owner, &hash1, &0);
    let log_v2 = client.get_passkey_audit_log(&vault_id);

    // Original entry should be unchanged
    assert_eq!(log_v1.get(0).unwrap(), log_v2.get(0).unwrap());
}

/// Test: Audit log persists across vault operations
#[test]
fn test_audit_log_persistence() {
    let (env, owner, beneficiary, client) = setup();
    let vault_id = client.create_vault(&owner, &beneficiary, &1_000u64, &None);

    let hash1 = BytesN::<32>::from_array(&env, &[1u8; 32]);

    client.add_passkey(&vault_id, &owner, &hash1);

    // Update vault metadata
    client.set_name(&vault_id, &owner, &SorobanString::from_str(&env, "My Vault"));

    // Audit log should still be there
    let log = client.get_passkey_audit_log(&vault_id);
    assert_eq!(log.len(), 1);
    assert_eq!(log.get(0).unwrap().operation, SorobanString::from_str(&env, "add"));
}

/// Test: Audit log with rotation operations
#[test]
fn test_audit_log_rotation_entries() {
    let (env, owner, beneficiary, client) = setup();
    let vault_id = client.create_vault(&owner, &beneficiary, &1_000u64, &None);

    let old_hash = BytesN::<32>::from_array(&env, &[1u8; 32]);
    let new_hash = BytesN::<32>::from_array(&env, &[2u8; 32]);

    // Set initial passkey
    {
        let mut vault = client.get_vault(&vault_id);
        vault.passkey_hash = Some(soroban_sdk::Bytes::from_array(&env, &old_hash.to_array()));
        env.as_contract(&client.address, || {
            env.storage()
                .persistent()
                .set(&DataKey::Vault(vault_id), &vault);
        });
    }

    client.rotate_passkey(&vault_id, &owner, &old_hash, &new_hash);

    let log = client.get_passkey_audit_log(&vault_id);
    assert_eq!(log.len(), 2);
    assert_eq!(log.get(0).unwrap().operation, SorobanString::from_str(&env, "remove"));
    assert_eq!(log.get(1).unwrap().operation, SorobanString::from_str(&env, "add"));
}

/// Test: Audit log with many entries
#[test]
fn test_audit_log_many_entries() {
    let (env, owner, beneficiary, client) = setup();
    let vault_id = client.create_vault(&owner, &beneficiary, &1_000u64, &None);

    let mut hashes = Vec::new();
    for i in 0..10 {
        let mut arr = [0u8; 32];
        arr[0] = i as u8;
        let hash = BytesN::<32>::from_array(&env, &arr);
        hashes.push(hash);
        client.add_passkey(&vault_id, &owner, &hash);
    }

    // Use each passkey
    for hash in hashes.iter() {
        client.check_in(&vault_id, &owner, hash, &0);
    }

    let log = client.get_passkey_audit_log(&vault_id);
    assert_eq!(log.len(), 20); // 10 adds + 10 uses
}

/// Test: Audit log integrity after errors
#[test]
fn test_audit_log_on_failed_operations() {
    let (env, owner, beneficiary, client) = setup();
    let vault_id = client.create_vault(&owner, &beneficiary, &1_000u64, &None);

    let hash1 = BytesN::<32>::from_array(&env, &[1u8; 32]);

    client.add_passkey(&vault_id, &owner, &hash1);

    // Try to remove non-existent passkey (should fail)
    let bad_hash = BytesN::<32>::from_array(&env, &[99u8; 32]);
    let result = client.try_remove_passkey(&vault_id, &owner, &bad_hash);
    assert!(result.is_err());

    // Audit log should only have the successful add
    let log = client.get_passkey_audit_log(&vault_id);
    assert_eq!(log.len(), 1);
    assert_eq!(log.get(0).unwrap().operation, SorobanString::from_str(&env, "add"));
}

/// Test: Audit log with different passkey hashes
#[test]
fn test_audit_log_passkey_hash_tracking() {
    let (env, owner, beneficiary, client) = setup();
    let vault_id = client.create_vault(&owner, &beneficiary, &1_000u64, &None);

    let hash_a = BytesN::<32>::from_array(&env, &[10u8; 32]);
    let hash_b = BytesN::<32>::from_array(&env, &[20u8; 32]);
    let hash_c = BytesN::<32>::from_array(&env, &[30u8; 32]);

    client.add_passkey(&vault_id, &owner, &hash_a);
    client.add_passkey(&vault_id, &owner, &hash_b);
    client.add_passkey(&vault_id, &owner, &hash_c);

    let log = client.get_passkey_audit_log(&vault_id);

    // Verify each hash is tracked
    assert_eq!(log.get(0).unwrap().passkey_hash, hash_a);
    assert_eq!(log.get(1).unwrap().passkey_hash, hash_b);
    assert_eq!(log.get(2).unwrap().passkey_hash, hash_c);
}

/// Test: Audit log query performance
#[test]
fn test_audit_log_query_efficiency() {
    let (env, owner, beneficiary, client) = setup();
    let vault_id = client.create_vault(&owner, &beneficiary, &1_000u64, &None);

    let hash1 = BytesN::<32>::from_array(&env, &[1u8; 32]);

    // Generate many audit entries
    for _i in 0..50 {
        client.check_in(&vault_id, &owner, &hash1, &0);
    }

    let log = client.get_passkey_audit_log(&vault_id);
    assert_eq!(log.len(), 50);

    // Get specific entries
    assert_eq!(log.get(0).unwrap().operation, SorobanString::from_str(&env, "use"));
    assert_eq!(log.get(49).unwrap().operation, SorobanString::from_str(&env, "use"));
}
