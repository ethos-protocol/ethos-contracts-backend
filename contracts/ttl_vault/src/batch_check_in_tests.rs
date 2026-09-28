/// Issue #562 — Check-in batch accumulation tests
///
/// ⚠️ WARNING: These tests are implementation-only and do **not** run the full
/// contract binary.  Do not add integration/end-to-end assertions here.
#![cfg(test)]

extern crate alloc;

use super::*;
use soroban_sdk::{
    testutils::{Address as _, Ledger},
    token::StellarAssetClient,
    vec, Env,
};

fn setup() -> (
    Env,
    Address,
    Address,
    Address,
    TtlVaultContractClient<'static>,
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

    let client: TtlVaultContractClient<'static> = unsafe { core::mem::transmute(client) };
    (env, owner, beneficiary, admin, client)
}

// ── set_batch_flush_interval ──────────────────────────────────────────────────

#[test]
fn test_set_batch_flush_interval_stores_value() {
    let (env, _, _, _, client) = setup();

    assert!(client.get_batch_flush_interval().is_none());
    client.set_batch_flush_interval(&3600u64).unwrap();
    assert_eq!(client.get_batch_flush_interval(), Some(3600u64));
    let _ = env;
}

#[test]
fn test_set_batch_flush_interval_rejects_zero() {
    let (env, _, _, _, client) = setup();

    let result = client.try_set_batch_flush_interval(&0u64);
    assert!(result.is_err());
    let _ = env;
}

#[test]
fn test_set_batch_flush_interval_update() {
    let (env, _, _, _, client) = setup();

    client.set_batch_flush_interval(&3600u64).unwrap();
    client.set_batch_flush_interval(&7200u64).unwrap();
    assert_eq!(client.get_batch_flush_interval(), Some(7200u64));
    let _ = env;
}

// ── batch_check_ins ───────────────────────────────────────────────────────────

#[test]
fn test_batch_check_ins_all_succeed() {
    let (env, owner, beneficiary, _, client) = setup();

    let v1 = client.create_vault(&owner, &beneficiary, &1000u64, &None);
    let v2 = client.create_vault(&owner, &beneficiary, &1000u64, &None);
    let v3 = client.create_vault(&owner, &beneficiary, &1000u64, &None);

    env.ledger().with_mut(|l| l.timestamp += 500);
    let ids = vec![&env, v1, v2, v3];
    let results = client.batch_check_ins(&ids, &owner);

    assert_eq!(results.len(), 3);
    for result in results.iter() {
        assert!(result.success, "vault {} should have succeeded", result.vault_id);
        assert_eq!(result.error_code, 0);
    }
    let _ = env;
}

#[test]
fn test_batch_check_ins_continues_after_not_owner_error() {
    let (env, owner, beneficiary, _, client) = setup();
    let other_owner = Address::generate(&env);
    let token_admin = Address::generate(&env);
    let token = env
        .register_stellar_asset_contract_v2(token_admin)
        .address();
    StellarAssetClient::new(&env, &token).mint(&other_owner, &1_000_000);

    let v1 = client.create_vault(&owner, &beneficiary, &1000u64, &None);
    let v2 = client.create_vault(&owner, &beneficiary, &1000u64, &None);

    // v_other is owned by other_owner, not owner
    let v_other = client.create_vault(&other_owner, &beneficiary, &1000u64, &None);

    env.ledger().with_mut(|l| l.timestamp += 500);

    // owner tries to check in all three; v_other should fail with NotOwner
    let ids = vec![&env, v1, v_other, v2];
    let results = client.batch_check_ins(&ids, &owner);

    assert_eq!(results.len(), 3);
    assert!(results.get(0).unwrap().success);
    let failed = results.get(1).unwrap();
    assert!(!failed.success);
    assert_eq!(failed.error_code, ContractError::NotOwner as u32);
    assert!(results.get(2).unwrap().success);
    let _ = env;
}

#[test]
fn test_batch_check_ins_skips_released_vault() {
    let (env, owner, beneficiary, _, client) = setup();

    let v1 = client.create_vault(&owner, &beneficiary, &1000u64, &None);
    let v2 = client.create_vault(&owner, &beneficiary, &1000u64, &None);

    // Expire vault v2 and trigger release
    env.ledger().with_mut(|l| l.timestamp += 2000);
    client.trigger_release(&v2);

    let ids = vec![&env, v1, v2];
    let results = client.batch_check_ins(&ids, &owner);

    assert_eq!(results.len(), 2);
    assert!(results.get(0).unwrap().success);
    let failed = results.get(1).unwrap();
    assert!(!failed.success);
    assert_eq!(failed.error_code, ContractError::AlreadyReleased as u32);
    let _ = env;
}

#[test]
fn test_batch_check_ins_nonexistent_vault_fails_gracefully() {
    let (env, owner, beneficiary, _, client) = setup();

    let v1 = client.create_vault(&owner, &beneficiary, &1000u64, &None);
    let nonexistent: u64 = 9999;

    env.ledger().with_mut(|l| l.timestamp += 500);
    let ids = vec![&env, v1, nonexistent];
    let results = client.batch_check_ins(&ids, &owner);

    assert_eq!(results.len(), 2);
    assert!(results.get(0).unwrap().success);
    let failed = results.get(1).unwrap();
    assert!(!failed.success);
    assert_eq!(failed.error_code, ContractError::VaultNotFound as u32);
    let _ = env;
}

#[test]
fn test_batch_check_ins_empty_list_returns_empty_results() {
    let (env, owner, _, _, client) = setup();

    let ids: soroban_sdk::Vec<u64> = soroban_sdk::Vec::new(&env);
    let results = client.batch_check_ins(&ids, &owner);
    assert_eq!(results.len(), 0);
    let _ = env;
}

#[test]
fn test_batch_check_ins_updates_last_check_in_timestamp() {
    let (env, owner, beneficiary, _, client) = setup();

    let v1 = client.create_vault(&owner, &beneficiary, &1000u64, &None);

    env.ledger().with_mut(|l| l.timestamp += 500);
    let ts = env.ledger().timestamp();
    let ids = vec![&env, v1];
    let results = client.batch_check_ins(&ids, &owner);

    assert!(results.get(0).unwrap().success);

    let vault = client.get_vault(&v1);
    assert_eq!(vault.last_check_in, ts);
    let _ = env;
}

#[test]
fn test_batch_check_ins_paused_vault_fails() {
    let (env, owner, beneficiary, admin, client) = setup();
    let _ = admin;

    let v1 = client.create_vault(&owner, &beneficiary, &1000u64, &None);
    let v2 = client.create_vault(&owner, &beneficiary, &1000u64, &None);

    // Pause vault v1
    client.pause_vault(&v1, &owner).unwrap();

    env.ledger().with_mut(|l| l.timestamp += 500);
    let ids = vec![&env, v1, v2];
    let results = client.batch_check_ins(&ids, &owner);

    assert_eq!(results.len(), 2);
    let failed = results.get(0).unwrap();
    assert!(!failed.success);
    assert_eq!(failed.error_code, ContractError::Paused as u32);
    assert!(results.get(1).unwrap().success);
    let _ = env;
}

#[test]
fn test_batch_check_ins_returns_vault_ids_in_order() {
    let (env, owner, beneficiary, _, client) = setup();

    let v1 = client.create_vault(&owner, &beneficiary, &1000u64, &None);
    let v2 = client.create_vault(&owner, &beneficiary, &1000u64, &None);
    let v3 = client.create_vault(&owner, &beneficiary, &1000u64, &None);

    env.ledger().with_mut(|l| l.timestamp += 500);
    let ids = vec![&env, v3, v1, v2];
    let results = client.batch_check_ins(&ids, &owner);

    assert_eq!(results.get(0).unwrap().vault_id, v3);
    assert_eq!(results.get(1).unwrap().vault_id, v1);
    assert_eq!(results.get(2).unwrap().vault_id, v2);
    let _ = env;
}
