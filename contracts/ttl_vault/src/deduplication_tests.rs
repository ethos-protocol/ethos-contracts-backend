/// Issue #561 — Data deduplication for similar vaults tests
///
/// ⚠️ WARNING: These tests are implementation-only and do **not** run the full
/// contract binary.  Do not add integration/end-to-end assertions here.
#![cfg(test)]

extern crate alloc;

use super::*;
use soroban_sdk::{
    testutils::Address as _,
    token::StellarAssetClient,
    BytesN, Env,
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

fn dummy_hash(env: &Env) -> BytesN<32> {
    BytesN::from_array(env, &[0xABu8; 32])
}

// ── register_vault_config_template ───────────────────────────────────────────

#[test]
fn test_register_template_returns_incrementing_ids() {
    let (env, _, _, _, client) = setup();
    let hash = dummy_hash(&env);

    let id1 = client.register_vault_config_template(&1000u64, &hash).unwrap();
    let id2 = client.register_vault_config_template(&2000u64, &hash).unwrap();
    let id3 = client.register_vault_config_template(&3000u64, &hash).unwrap();

    assert_eq!(id1, 1);
    assert_eq!(id2, 2);
    assert_eq!(id3, 3);
    let _ = env;
}

#[test]
fn test_register_template_stores_correct_interval() {
    let (env, _, _, _, client) = setup();
    let hash = dummy_hash(&env);

    let id = client.register_vault_config_template(&86400u64, &hash).unwrap();
    let tmpl = client.get_vault_config_template(&id).unwrap();

    assert_eq!(tmpl.template_id, id);
    assert_eq!(tmpl.check_in_interval, 86400u64);
    assert_eq!(tmpl.ref_count, 0);
    let _ = env;
}

#[test]
fn test_register_template_rejects_zero_interval() {
    let (env, _, _, _, client) = setup();
    let hash = dummy_hash(&env);

    let result = client.try_register_vault_config_template(&0u64, &hash);
    assert!(result.is_err());
    let _ = env;
}

#[test]
fn test_get_template_not_found_returns_none() {
    let (env, _, _, _, client) = setup();

    let result = client.get_vault_config_template(&9999u64);
    assert!(result.is_none());
    let _ = env;
}

// ── set_vault_template_ref ────────────────────────────────────────────────────

#[test]
fn test_set_vault_template_ref_increments_ref_count() {
    let (env, owner, beneficiary, _, client) = setup();
    let hash = dummy_hash(&env);

    let tmpl_id = client.register_vault_config_template(&1000u64, &hash).unwrap();
    let vault_id = client.create_vault(&owner, &beneficiary, &1000u64, &None);

    client.set_vault_template_ref(&vault_id, &owner, &tmpl_id).unwrap();

    let tmpl = client.get_vault_config_template(&tmpl_id).unwrap();
    assert_eq!(tmpl.ref_count, 1);

    let stored_ref = client.get_vault_template_ref(&vault_id);
    assert_eq!(stored_ref, Some(tmpl_id));
    let _ = env;
}

#[test]
fn test_set_vault_template_ref_multiple_vaults() {
    let (env, owner, beneficiary, _, client) = setup();
    let hash = dummy_hash(&env);

    let tmpl_id = client.register_vault_config_template(&1000u64, &hash).unwrap();

    let v1 = client.create_vault(&owner, &beneficiary, &1000u64, &None);
    let v2 = client.create_vault(&owner, &beneficiary, &1000u64, &None);
    let v3 = client.create_vault(&owner, &beneficiary, &1000u64, &None);

    client.set_vault_template_ref(&v1, &owner, &tmpl_id).unwrap();
    client.set_vault_template_ref(&v2, &owner, &tmpl_id).unwrap();
    client.set_vault_template_ref(&v3, &owner, &tmpl_id).unwrap();

    let tmpl = client.get_vault_config_template(&tmpl_id).unwrap();
    assert_eq!(tmpl.ref_count, 3);
    let _ = env;
}

#[test]
fn test_set_vault_template_ref_vault_not_found() {
    let (env, owner, _, _, client) = setup();
    let hash = dummy_hash(&env);

    let tmpl_id = client.register_vault_config_template(&1000u64, &hash).unwrap();
    let result = client.try_set_vault_template_ref(&9999u64, &owner, &tmpl_id);
    assert!(result.is_err());
    let _ = env;
}

#[test]
fn test_set_vault_template_ref_template_not_found() {
    let (env, owner, beneficiary, _, client) = setup();

    let vault_id = client.create_vault(&owner, &beneficiary, &1000u64, &None);
    let result = client.try_set_vault_template_ref(&vault_id, &owner, &9999u64);
    assert!(result.is_err());
    let _ = env;
}

#[test]
fn test_set_vault_template_ref_not_owner_rejected() {
    let (env, owner, beneficiary, _, client) = setup();
    let other = Address::generate(&env);
    let hash = dummy_hash(&env);

    let tmpl_id = client.register_vault_config_template(&1000u64, &hash).unwrap();
    let vault_id = client.create_vault(&owner, &beneficiary, &1000u64, &None);

    let result = client.try_set_vault_template_ref(&vault_id, &other, &tmpl_id);
    assert!(result.is_err());
    let _ = env;
}

#[test]
fn test_get_vault_template_ref_no_ref_returns_none() {
    let (env, owner, beneficiary, _, client) = setup();

    let vault_id = client.create_vault(&owner, &beneficiary, &1000u64, &None);
    assert!(client.get_vault_template_ref(&vault_id).is_none());
    let _ = env;
}

#[test]
fn test_set_vault_template_ref_switching_decrements_old_ref_count() {
    let (env, owner, beneficiary, _, client) = setup();
    let hash = dummy_hash(&env);

    let tmpl1 = client.register_vault_config_template(&1000u64, &hash).unwrap();
    let tmpl2 = client.register_vault_config_template(&2000u64, &hash).unwrap();

    let vault_id = client.create_vault(&owner, &beneficiary, &1000u64, &None);

    client.set_vault_template_ref(&vault_id, &owner, &tmpl1).unwrap();
    assert_eq!(client.get_vault_config_template(&tmpl1).unwrap().ref_count, 1);

    // Switch to tmpl2
    client.set_vault_template_ref(&vault_id, &owner, &tmpl2).unwrap();
    assert_eq!(client.get_vault_config_template(&tmpl1).unwrap().ref_count, 0);
    assert_eq!(client.get_vault_config_template(&tmpl2).unwrap().ref_count, 1);
    let _ = env;
}

// ── get_vaults_by_template ────────────────────────────────────────────────────

#[test]
fn test_get_vaults_by_template_returns_matching_vaults() {
    let (env, owner, beneficiary, _, client) = setup();
    let hash = dummy_hash(&env);

    let tmpl_id = client.register_vault_config_template(&1000u64, &hash).unwrap();

    let v1 = client.create_vault(&owner, &beneficiary, &1000u64, &None);
    let v2 = client.create_vault(&owner, &beneficiary, &1000u64, &None);
    let v3 = client.create_vault(&owner, &beneficiary, &1000u64, &None);

    client.set_vault_template_ref(&v1, &owner, &tmpl_id).unwrap();
    client.set_vault_template_ref(&v3, &owner, &tmpl_id).unwrap();
    // v2 is not associated with the template

    let result = client.get_vaults_by_template(&tmpl_id, &0u32, &10u32);
    assert_eq!(result.len(), 2);
    // v2 must not appear
    let result_ids: alloc::vec::Vec<u64> = result.iter().collect();
    assert!(!result_ids.contains(&v2));
    assert!(result_ids.contains(&v1));
    assert!(result_ids.contains(&v3));
    let _ = env;
}

#[test]
fn test_get_vaults_by_template_nonexistent_template_returns_empty() {
    let (env, _, _, _, client) = setup();

    let result = client.get_vaults_by_template(&9999u64, &0u32, &10u32);
    assert_eq!(result.len(), 0);
    let _ = env;
}

#[test]
fn test_get_vaults_by_template_pagination() {
    let (env, owner, beneficiary, _, client) = setup();
    let hash = dummy_hash(&env);

    let tmpl_id = client.register_vault_config_template(&1000u64, &hash).unwrap();

    for _ in 0..5 {
        let v = client.create_vault(&owner, &beneficiary, &1000u64, &None);
        client.set_vault_template_ref(&v, &owner, &tmpl_id).unwrap();
    }

    let page1 = client.get_vaults_by_template(&tmpl_id, &0u32, &3u32);
    let page2 = client.get_vaults_by_template(&tmpl_id, &1u32, &3u32);

    assert_eq!(page1.len(), 3);
    assert_eq!(page2.len(), 2);
    let _ = env;
}
