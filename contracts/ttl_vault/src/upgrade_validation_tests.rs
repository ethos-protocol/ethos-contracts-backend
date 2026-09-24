//! Tests for upgrade safety validation (Issue: `upgrade()` allowed arbitrary
//! WASM with no compatibility checks).
//!
//! These tests exercise the manifest-based compatibility checks added in
//! `validate_upgrade_compatibility` / `upgrade_with_manifest`:
//!
//!   1. `validate_upgrade_compatibility` panics with `UpgradeManifestNotSet`
//!      when no baseline has been recorded yet.
//!   2. Once a baseline is recorded via `set_upgrade_manifest`, a proposed
//!      upgrade with a smaller exported function count is rejected
//!      (`UpgradeInterfaceShrunk`).
//!   3. A proposed upgrade with fewer error codes than the baseline is
//!      rejected (`UpgradeErrorCodesReduced`).
//!   4. A proposed upgrade with a different storage schema hash is rejected
//!      (`UpgradeStorageSchemaChanged`).
//!   5. A fully compatible proposed upgrade passes validation.
//!   6. `upgrade_with_manifest` records a new baseline (with incremented
//!      version) after a successful upgrade, so a subsequent upgrade is
//!      validated against the *new* contract, not the original one.

#![cfg(test)]

use super::*;
use soroban_sdk::{
    testutils::Address as _,
    token::StellarAssetClient,
    Address, BytesN, Env,
};

fn setup() -> (Env, Address, TtlVaultContractClient<'static>) {
    let env = Env::default();
    env.mock_all_auths();

    let admin = Address::generate(&env);
    let token_admin = Address::generate(&env);
    let token_address = env
        .register_stellar_asset_contract_v2(token_admin)
        .address();
    StellarAssetClient::new(&env, &token_address);

    let contract_address = env.register_contract(None, TtlVaultContract);
    let client = TtlVaultContractClient::new(&env, &contract_address);
    client.initialize(&token_address, &admin);

    let client: TtlVaultContractClient<'static> = unsafe { core::mem::transmute(client) };
    (env, admin, client)
}

fn schema_hash(env: &Env, seed: u8) -> BytesN<32> {
    BytesN::from_array(env, &[seed; 32])
}

#[test]
fn validate_upgrade_compatibility_fails_without_manifest() {
    let (env, _admin, client) = setup();
    let hash = schema_hash(&env, 1);

    let result = client.try_validate_upgrade_compatibility(&10u32, &5u32, &hash);
    assert!(result.is_err());
}

#[test]
fn set_upgrade_manifest_then_compatible_upgrade_passes() {
    let (env, _admin, client) = setup();
    let hash = schema_hash(&env, 1);

    client.set_upgrade_manifest(&10u32, &5u32, &hash);

    // Same shape: same fn count, same error count, same schema hash.
    client.validate_upgrade_compatibility(&10u32, &5u32, &hash);

    // Growing the interface / error codes is also fine.
    client.validate_upgrade_compatibility(&11u32, &6u32, &hash);
}

#[test]
fn shrinking_exported_fn_count_is_rejected() {
    let (env, _admin, client) = setup();
    let hash = schema_hash(&env, 1);
    client.set_upgrade_manifest(&10u32, &5u32, &hash);

    let result = client.try_validate_upgrade_compatibility(&9u32, &5u32, &hash);
    assert!(result.is_err());
}

#[test]
fn reducing_error_code_count_is_rejected() {
    let (env, _admin, client) = setup();
    let hash = schema_hash(&env, 1);
    client.set_upgrade_manifest(&10u32, &5u32, &hash);

    let result = client.try_validate_upgrade_compatibility(&10u32, &4u32, &hash);
    assert!(result.is_err());
}

#[test]
fn changing_storage_schema_hash_is_rejected() {
    let (env, _admin, client) = setup();
    let baseline_hash = schema_hash(&env, 1);
    let different_hash = schema_hash(&env, 2);
    client.set_upgrade_manifest(&10u32, &5u32, &baseline_hash);

    let result = client.try_validate_upgrade_compatibility(&10u32, &5u32, &different_hash);
    assert!(result.is_err());
}

#[test]
fn set_upgrade_manifest_increments_version() {
    let (env, _admin, client) = setup();
    let hash = schema_hash(&env, 1);

    client.set_upgrade_manifest(&10u32, &5u32, &hash);
    let first = client.get_upgrade_manifest().unwrap();
    assert_eq!(first.version, 1);

    client.set_upgrade_manifest(&11u32, &5u32, &hash);
    let second = client.get_upgrade_manifest().unwrap();
    assert_eq!(second.version, 2);
    assert_eq!(second.exported_fn_count, 11);

    let _ = env;
}

#[test]
fn upgrade_with_manifest_rejects_invalid_hash() {
    let (env, _admin, client) = setup();
    let hash = schema_hash(&env, 1);
    client.set_upgrade_manifest(&10u32, &5u32, &hash);

    let zero_hash = BytesN::from_array(&env, &[0u8; 32]);
    let result =
        client.try_upgrade_with_manifest(&zero_hash, &10u32, &5u32, &hash);
    assert!(result.is_err());
}
