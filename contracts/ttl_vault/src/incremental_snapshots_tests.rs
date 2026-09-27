/// Issue #559 — Incremental state snapshots tests
///
/// ⚠️ WARNING: These tests are implementation-only and have **not** been run
/// as part of this change. Do not add integration/end-to-end assertions here.
#![cfg(test)]

extern crate alloc;

use super::*;
use crate::incremental_snapshots::FULL_SNAPSHOT_INTERVAL;
use soroban_sdk::{
    testutils::{Address as _, Ledger},
    token::StellarAssetClient,
    Env,
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
    StellarAssetClient::new(&env, &token_address).mint(&owner, &10_000_000);

    let contract_address = env.register_contract(None, TtlVaultContract);
    let client = TtlVaultContractClient::new(&env, &contract_address);
    client.initialize(&token_address, &admin);

    let client: TtlVaultContractClient<'static> = unsafe { core::mem::transmute(client) };
    (env, owner, beneficiary, client)
}

fn assert_same_vault(a: &Vault, b: &Vault) {
    assert_eq!(a.owner, b.owner);
    assert_eq!(a.beneficiary, b.beneficiary);
    assert_eq!(a.balance, b.balance);
    assert_eq!(a.check_in_interval, b.check_in_interval);
    assert_eq!(a.last_check_in, b.last_check_in);
    assert_eq!(a.status, b.status);
    assert_eq!(a.metadata, b.metadata);
    assert_eq!(a.is_paused, b.is_paused);
}

// ── diff_codec ────────────────────────────────────────────────────────────────

#[test]
fn test_rle_roundtrip() {
    let input: alloc::vec::Vec<u8> = alloc::vec![0, 0, 0, 0, 7, 8, 0, 9, 0, 0, 0, 0, 0, 1];
    let encoded = diff_codec::rle_encode_buf(&input);
    assert!(encoded.len() < input.len());
    assert_eq!(diff_codec::rle_decode_buf(&encoded).unwrap(), input);
}

#[test]
fn test_rle_roundtrip_long_runs_and_literals() {
    let mut input = alloc::vec![0u8; 600];
    input.extend((0..600u32).map(|i| (i % 251 + 1) as u8));
    let encoded = diff_codec::rle_encode_buf(&input);
    assert_eq!(diff_codec::rle_decode_buf(&encoded).unwrap(), input);
}

#[test]
fn test_rle_decode_rejects_malformed() {
    assert!(diff_codec::rle_decode_buf(&[0x02, 1]).is_none());
    assert!(diff_codec::rle_decode_buf(&[0x01, 5, 1, 2]).is_none());
    assert!(diff_codec::rle_decode_buf(&[0x00]).is_none());
}

#[test]
fn test_diff_compress_roundtrip() {
    let env = Env::default();
    let prev = Bytes::from_slice(&env, &[1, 2, 3, 4, 5, 6, 7, 8]);
    let next = Bytes::from_slice(&env, &[1, 2, 3, 9, 5, 6, 7, 8, 10]);
    let delta = diff_codec::diff_compress(&env, &prev, &next);
    let out = diff_codec::diff_decompress(&env, &prev, &delta, next.len()).unwrap();
    assert_eq!(out, next);
}

// ── snapshot chain ───────────────────────────────────────────────────────────

#[test]
fn test_first_snapshot_is_full_checkpoint() {
    let (_env, owner, beneficiary, client) = setup();
    let vault_id = client.create_vault(&owner, &beneficiary, &86_400u64, &None);

    let seq = client.create_incremental_snapshot(&vault_id);
    assert_eq!(seq, 0);
    let snap = client.get_incremental_snapshot(&vault_id, &0).unwrap();
    assert!(snap.is_full);
    assert_eq!(snap.deltas.len(), incremental_snapshots::VAULT_FIELD_COUNT);
    assert_eq!(client.get_incremental_snapshot_count(&vault_id), 1);
}

#[test]
fn test_delta_snapshot_stores_only_changed_fields() {
    let (_env, owner, beneficiary, client) = setup();
    let vault_id = client.create_vault(&owner, &beneficiary, &86_400u64, &None);
    client.create_incremental_snapshot(&vault_id);

    client.deposit(&vault_id, &owner, &5_000);
    let seq = client.create_incremental_snapshot(&vault_id);
    let snap = client.get_incremental_snapshot(&vault_id, &seq).unwrap();

    assert!(!snap.is_full);
    assert!(snap.deltas.len() < incremental_snapshots::VAULT_FIELD_COUNT);
    assert!(snap.deltas.iter().any(|d| d.field == 2)); // balance
    assert!(snap.stored_size < snap.full_size);
}

#[test]
fn test_unchanged_vault_produces_empty_delta() {
    let (_env, owner, beneficiary, client) = setup();
    let vault_id = client.create_vault(&owner, &beneficiary, &86_400u64, &None);
    client.create_incremental_snapshot(&vault_id);
    let seq = client.create_incremental_snapshot(&vault_id);
    let snap = client.get_incremental_snapshot(&vault_id, &seq).unwrap();
    assert_eq!(snap.deltas.len(), 0);
    assert_eq!(snap.stored_size, 0);
}

#[test]
fn test_reconstruct_each_snapshot_matches_state_at_time() {
    let (_env, owner, beneficiary, client) = setup();
    let vault_id = client.create_vault(&owner, &beneficiary, &86_400u64, &None);

    let mut expected = alloc::vec::Vec::new();
    for i in 0..5 {
        client.deposit(&vault_id, &owner, &(100 * (i + 1)));
        client.create_incremental_snapshot(&vault_id);
        expected.push(client.get_vault(&vault_id));
    }

    for (seq, vault) in expected.iter().enumerate() {
        let rebuilt = client.reconstruct_vault_snapshot(&vault_id, &(seq as u32));
        assert_same_vault(&rebuilt, vault);
    }
}

#[test]
fn test_new_checkpoint_after_interval() {
    let (_env, owner, beneficiary, client) = setup();
    let vault_id = client.create_vault(&owner, &beneficiary, &86_400u64, &None);

    for _ in 0..=FULL_SNAPSHOT_INTERVAL {
        client.deposit(&vault_id, &owner, &10);
        client.create_incremental_snapshot(&vault_id);
    }
    let checkpoint = client
        .get_incremental_snapshot(&vault_id, &FULL_SNAPSHOT_INTERVAL)
        .unwrap();
    assert!(checkpoint.is_full);

    let rebuilt = client.reconstruct_vault_snapshot(&vault_id, &FULL_SNAPSHOT_INTERVAL);
    assert_same_vault(&rebuilt, &client.get_vault(&vault_id));

    let before = client.reconstruct_vault_snapshot(&vault_id, &(FULL_SNAPSHOT_INTERVAL - 1));
    assert_eq!(before.balance, rebuilt.balance - 10);
}

#[test]
fn test_reconstruct_at_timestamp() {
    let (env, owner, beneficiary, client) = setup();
    let vault_id = client.create_vault(&owner, &beneficiary, &86_400u64, &None);

    env.ledger().with_mut(|l| l.timestamp = 1_000);
    client.deposit(&vault_id, &owner, &100);
    client.create_incremental_snapshot(&vault_id);

    env.ledger().with_mut(|l| l.timestamp = 2_000);
    client.deposit(&vault_id, &owner, &100);
    client.create_incremental_snapshot(&vault_id);

    assert_eq!(
        client.reconstruct_vault_snapshot_at(&vault_id, &1_500).balance,
        100
    );
    assert_eq!(
        client.reconstruct_vault_snapshot_at(&vault_id, &5_000).balance,
        200
    );
    assert!(client
        .try_reconstruct_vault_snapshot_at(&vault_id, &500)
        .is_err());
}

#[test]
fn test_reconstruct_missing_snapshot_errors() {
    let (_env, owner, beneficiary, client) = setup();
    let vault_id = client.create_vault(&owner, &beneficiary, &86_400u64, &None);
    let err = client
        .try_reconstruct_vault_snapshot(&vault_id, &3)
        .unwrap_err()
        .unwrap();
    assert_eq!(
        err,
        soroban_sdk::Error::from_contract_error(ContractError::SnapshotNotFound as u32)
    );
}
