/// Issue #558 — Vault compression for inactive accounts tests
///
/// ⚠️ WARNING: These tests are implementation-only and have **not** been run
/// as part of this change. Do not add integration/end-to-end assertions here.
#![cfg(test)]

extern crate alloc;

use super::*;
use crate::vault_compression::DEFAULT_INACTIVITY_DAYS;
use soroban_sdk::{
    testutils::{Address as _, Ledger},
    token::StellarAssetClient,
    Env,
};

const DAY: u64 = 86_400;

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
    env.ledger().with_mut(|l| l.timestamp = 1_000);
    (env, owner, beneficiary, client)
}

fn go_idle(env: &Env, days: u64) {
    env.ledger()
        .with_mut(|l| l.timestamp = 1_000 + days * DAY);
}

/// Long interval so the vault is not expired while idle in these tests.
const INTERVAL: u64 = 365 * DAY;

#[test]
fn test_active_vault_is_not_compressed() {
    let (env, owner, beneficiary, client) = setup();
    let vault_id = client.create_vault(&owner, &beneficiary, &INTERVAL, &None);
    go_idle(&env, u64::from(DEFAULT_INACTIVITY_DAYS) - 1);
    assert!(!client.compress_vault(&vault_id));
    assert!(!client.is_vault_compressed(&vault_id));
}

#[test]
fn test_inactive_vault_is_compressed_and_readable() {
    let (env, owner, beneficiary, client) = setup();
    let vault_id = client.create_vault(&owner, &beneficiary, &INTERVAL, &None);
    client.deposit(&vault_id, &owner, &12_345);
    let before = client.get_vault(&vault_id);

    go_idle(&env, u64::from(DEFAULT_INACTIVITY_DAYS));
    assert!(client.compress_vault(&vault_id));
    assert!(client.is_vault_compressed(&vault_id));

    let info = client.get_vault_compression_info(&vault_id).unwrap();
    assert!(info.compressed_size < info.original_size);

    // Uncompressed entry is gone; reads decompress on demand.
    env.as_contract(&client.address, || {
        assert!(!env.storage().persistent().has(&DataKey::Vault(vault_id)));
    });
    let after = client.get_vault(&vault_id);
    assert_eq!(after.owner, before.owner);
    assert_eq!(after.balance, before.balance);
    assert_eq!(after.last_check_in, before.last_check_in);
    assert!(client.vault_exists(&vault_id));
}

#[test]
fn test_compress_twice_returns_false() {
    let (env, owner, beneficiary, client) = setup();
    let vault_id = client.create_vault(&owner, &beneficiary, &INTERVAL, &None);
    go_idle(&env, u64::from(DEFAULT_INACTIVITY_DAYS));
    assert!(client.compress_vault(&vault_id));
    assert!(!client.compress_vault(&vault_id));
}

#[test]
fn test_explicit_decompress_restores_vault() {
    let (env, owner, beneficiary, client) = setup();
    let vault_id = client.create_vault(&owner, &beneficiary, &INTERVAL, &None);
    go_idle(&env, u64::from(DEFAULT_INACTIVITY_DAYS));
    client.compress_vault(&vault_id);

    assert!(client.decompress_vault(&vault_id));
    assert!(!client.is_vault_compressed(&vault_id));
    env.as_contract(&client.address, || {
        assert!(env.storage().persistent().has(&DataKey::Vault(vault_id)));
    });
    assert!(!client.decompress_vault(&vault_id));
}

#[test]
fn test_write_to_compressed_vault_decompresses_it() {
    let (env, owner, beneficiary, client) = setup();
    let vault_id = client.create_vault(&owner, &beneficiary, &INTERVAL, &None);
    go_idle(&env, u64::from(DEFAULT_INACTIVITY_DAYS));
    client.compress_vault(&vault_id);

    client.deposit(&vault_id, &owner, &500);
    assert!(!client.is_vault_compressed(&vault_id));
    assert_eq!(client.get_vault(&vault_id).balance, 500);
}

#[test]
fn test_configurable_inactivity_days() {
    let (env, owner, beneficiary, client) = setup();
    let vault_id = client.create_vault(&owner, &beneficiary, &INTERVAL, &None);

    client.set_compression_inactivity_days(&7);
    assert_eq!(client.get_compression_inactivity_days(), 7);

    go_idle(&env, 6);
    assert!(!client.compress_vault(&vault_id));
    go_idle(&env, 7);
    assert!(client.compress_vault(&vault_id));

    assert!(client.try_set_compression_inactivity_days(&0).is_err());
}

#[test]
fn test_compress_missing_vault_errors() {
    let (_env, _, _, client) = setup();
    let err = client.try_compress_vault(&999).unwrap_err().unwrap();
    assert_eq!(
        err,
        soroban_sdk::Error::from_contract_error(ContractError::VaultNotFound as u32)
    );
}

#[test]
fn test_corrupted_payload_is_detected() {
    let (env, owner, beneficiary, client) = setup();
    let vault_id = client.create_vault(&owner, &beneficiary, &INTERVAL, &None);
    go_idle(&env, u64::from(DEFAULT_INACTIVITY_DAYS));
    client.compress_vault(&vault_id);

    env.as_contract(&client.address, || {
        let key = vault_compression::CompressionKey::Compressed(vault_id);
        let mut record: vault_compression::CompressedVault =
            env.storage().persistent().get(&key).unwrap();
        record.content_hash = BytesN::from_array(&env, &[7u8; 32]);
        env.storage().persistent().set(&key, &record);
    });
    let err = client.try_get_vault(&vault_id).unwrap_err().unwrap();
    assert_eq!(
        err,
        soroban_sdk::Error::from_contract_error(ContractError::CompressionCorrupted as u32)
    );
}
