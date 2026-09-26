/// Issue #560 — Merkle tree for efficient vault history proofs tests
///
/// ⚠️ WARNING: These tests are implementation-only and do **not** run the full
/// contract binary.  Do not add integration/end-to-end assertions here.
#![cfg(test)]

extern crate alloc;

use super::*;
use soroban_sdk::{
    testutils::Address as _,
    token::StellarAssetClient,
    Bytes, Env,
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

fn topic(env: &Env, s: &str) -> Bytes {
    Bytes::from_slice(env, s.as_bytes())
}

// ── append_history_leaf ───────────────────────────────────────────────────────

#[test]
fn test_append_history_leaf_returns_sequential_indices() {
    let (env, owner, beneficiary, _, client) = setup();

    let vault_id = client.create_vault(&owner, &beneficiary, &1000u64, &None);
    let t = topic(&env, "check_in");

    let i0 = client.append_history_leaf(&vault_id, &owner, &t).unwrap();
    let i1 = client.append_history_leaf(&vault_id, &owner, &t).unwrap();
    let i2 = client.append_history_leaf(&vault_id, &owner, &t).unwrap();

    assert_eq!(i0, 0);
    assert_eq!(i1, 1);
    assert_eq!(i2, 2);
    let _ = env;
}

#[test]
fn test_append_history_leaf_updates_leaf_count() {
    let (env, owner, beneficiary, _, client) = setup();

    let vault_id = client.create_vault(&owner, &beneficiary, &1000u64, &None);
    assert_eq!(client.get_history_leaf_count(&vault_id), 0);

    for _ in 0..4 {
        client.append_history_leaf(&vault_id, &owner, &topic(&env, "deposit")).unwrap();
    }

    assert_eq!(client.get_history_leaf_count(&vault_id), 4);
    let _ = env;
}

#[test]
fn test_append_history_leaf_updates_root() {
    let (env, owner, beneficiary, _, client) = setup();

    let vault_id = client.create_vault(&owner, &beneficiary, &1000u64, &None);
    assert!(client.get_history_root(&vault_id).is_none());

    client.append_history_leaf(&vault_id, &owner, &topic(&env, "deposit")).unwrap();
    let root1 = client.get_history_root(&vault_id);
    assert!(root1.is_some());

    client.append_history_leaf(&vault_id, &owner, &topic(&env, "check_in")).unwrap();
    let root2 = client.get_history_root(&vault_id);
    assert!(root2.is_some());

    // Root must change with each new leaf
    assert_ne!(root1, root2);
    let _ = env;
}

#[test]
fn test_append_history_leaf_vault_not_found() {
    let (env, owner, _, _, client) = setup();

    let result = client.try_append_history_leaf(&9999u64, &owner, &topic(&env, "check_in"));
    assert!(result.is_err());
    let _ = env;
}

#[test]
fn test_append_history_leaf_not_owner() {
    let (env, owner, beneficiary, _, client) = setup();
    let other = Address::generate(&env);

    let vault_id = client.create_vault(&owner, &beneficiary, &1000u64, &None);
    let result = client.try_append_history_leaf(&vault_id, &other, &topic(&env, "check_in"));
    assert!(result.is_err());
    let _ = env;
}

// ── get_history_proof ─────────────────────────────────────────────────────────

#[test]
fn test_get_history_proof_returns_none_for_empty_history() {
    let (env, owner, beneficiary, _, client) = setup();

    let vault_id = client.create_vault(&owner, &beneficiary, &1000u64, &None);
    let proof = client.get_history_proof(&vault_id, &0u32);
    assert!(proof.is_none());
    let _ = env;
}

#[test]
fn test_get_history_proof_returns_none_for_out_of_bounds_index() {
    let (env, owner, beneficiary, _, client) = setup();

    let vault_id = client.create_vault(&owner, &beneficiary, &1000u64, &None);
    client.append_history_leaf(&vault_id, &owner, &topic(&env, "deposit")).unwrap();

    let proof = client.get_history_proof(&vault_id, &1u32);
    assert!(proof.is_none());
    let _ = env;
}

#[test]
fn test_get_history_proof_single_leaf_verifies() {
    let (env, owner, beneficiary, _, client) = setup();

    let vault_id = client.create_vault(&owner, &beneficiary, &1000u64, &None);
    client.append_history_leaf(&vault_id, &owner, &topic(&env, "deposit")).unwrap();

    let proof = client.get_history_proof(&vault_id, &0u32).unwrap();
    assert_eq!(proof.vault_id, vault_id);
    assert_eq!(proof.event_index, 0);

    let valid = client.verify_history_proof(&proof);
    assert!(valid, "single-leaf proof must verify");
    let _ = env;
}

#[test]
fn test_get_history_proof_multiple_leaves_all_verify() {
    let (env, owner, beneficiary, _, client) = setup();

    let vault_id = client.create_vault(&owner, &beneficiary, &1000u64, &None);
    for i in 0u32..5 {
        let t = if i % 2 == 0 { topic(&env, "deposit") } else { topic(&env, "check_in") };
        client.append_history_leaf(&vault_id, &owner, &t).unwrap();
    }

    for i in 0u32..5 {
        let proof = client.get_history_proof(&vault_id, &i).unwrap();
        assert_eq!(proof.event_index, i);
        let valid = client.verify_history_proof(&proof);
        assert!(valid, "proof for event {i} must verify");
    }
    let _ = env;
}

#[test]
fn test_get_history_proof_power_of_two_leaf_count() {
    let (env, owner, beneficiary, _, client) = setup();

    let vault_id = client.create_vault(&owner, &beneficiary, &1000u64, &None);
    // 8 leaves == exact power of two
    for _ in 0..8 {
        client.append_history_leaf(&vault_id, &owner, &topic(&env, "deposit")).unwrap();
    }

    for i in 0u32..8 {
        let proof = client.get_history_proof(&vault_id, &i).unwrap();
        assert!(client.verify_history_proof(&proof), "proof for leaf {i} must verify");
    }
    let _ = env;
}

// ── verify_history_proof ──────────────────────────────────────────────────────

#[test]
fn test_verify_history_proof_tampered_leaf_hash_fails() {
    let (env, owner, beneficiary, _, client) = setup();

    let vault_id = client.create_vault(&owner, &beneficiary, &1000u64, &None);
    client.append_history_leaf(&vault_id, &owner, &topic(&env, "deposit")).unwrap();

    let mut proof = client.get_history_proof(&vault_id, &0u32).unwrap();
    // Corrupt the leaf hash
    proof.leaf_hash = soroban_sdk::BytesN::from_array(&env, &[0xFFu8; 32]);

    let valid = client.verify_history_proof(&proof);
    assert!(!valid, "tampered leaf hash must fail verification");
    let _ = env;
}

#[test]
fn test_verify_history_proof_wrong_root_fails() {
    let (env, owner, beneficiary, _, client) = setup();

    let vault_id = client.create_vault(&owner, &beneficiary, &1000u64, &None);
    client.append_history_leaf(&vault_id, &owner, &topic(&env, "deposit")).unwrap();

    let mut proof = client.get_history_proof(&vault_id, &0u32).unwrap();
    proof.root = soroban_sdk::BytesN::from_array(&env, &[0xEEu8; 32]);

    let valid = client.verify_history_proof(&proof);
    assert!(!valid, "wrong root must fail verification");
    let _ = env;
}

// ── get_history_root ──────────────────────────────────────────────────────────

#[test]
fn test_get_history_root_none_before_first_leaf() {
    let (env, owner, beneficiary, _, client) = setup();

    let vault_id = client.create_vault(&owner, &beneficiary, &1000u64, &None);
    assert!(client.get_history_root(&vault_id).is_none());
    let _ = env;
}

#[test]
fn test_get_history_root_changes_with_each_leaf() {
    let (env, owner, beneficiary, _, client) = setup();

    let vault_id = client.create_vault(&owner, &beneficiary, &1000u64, &None);
    let mut roots: alloc::vec::Vec<soroban_sdk::BytesN<32>> = alloc::vec::Vec::new();

    for _ in 0..5 {
        client.append_history_leaf(&vault_id, &owner, &topic(&env, "check_in")).unwrap();
        roots.push(client.get_history_root(&vault_id).unwrap());
    }

    // All roots must be distinct (each leaf changes the root)
    let unique: std::collections::BTreeSet<[u8; 32]> = roots
        .iter()
        .map(|r| r.to_array())
        .collect();
    assert_eq!(unique.len(), 5, "each new leaf must produce a unique root");
    let _ = env;
}

#[test]
fn test_get_history_leaf_count_starts_at_zero() {
    let (env, owner, beneficiary, _, client) = setup();

    let vault_id = client.create_vault(&owner, &beneficiary, &1000u64, &None);
    assert_eq!(client.get_history_leaf_count(&vault_id), 0u32);
    let _ = env;
}
