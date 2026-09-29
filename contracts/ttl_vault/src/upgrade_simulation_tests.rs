//! WASM-level upgrade simulation. CI builds the contract artifact and runs
//! this ignored test explicitly with `TTL_VAULT_WASM_PATH` set.

#![cfg(test)]

use super::*;
use soroban_sdk::{testutils::Address as _, Address, BytesN, Env};

#[test]
#[ignore = "run with the built contract WASM via TTL_VAULT_WASM_PATH"]
fn successful_wasm_upgrade_preserves_initialized_state() {
    let wasm_path = std::env::var("TTL_VAULT_WASM_PATH")
        .expect("TTL_VAULT_WASM_PATH must point to the built ttl_vault.wasm artifact");
    let wasm = std::fs::read(&wasm_path)
        .unwrap_or_else(|error| panic!("failed to read contract WASM at {wasm_path}: {error}"));

    let env = Env::default();
    env.mock_all_auths();

    let admin = Address::generate(&env);
    let token_admin = Address::generate(&env);
    let token_address = env
        .register_stellar_asset_contract_v2(token_admin)
        .address();
    let contract_address = env.register_contract_wasm(None, wasm.as_slice());
    let client = TtlVaultContractClient::new(&env, &contract_address);

    client.initialize(&token_address, &admin);

    let storage_hash = BytesN::from_array(&env, &[1; 32]);
    let signature_hash = BytesN::from_array(&env, &[2; 32]);
    let candidate_hash = env.deployer().upload_contract_wasm(wasm.as_slice());
    client.set_upgrade_manifest_with_signatures(&10u32, &5u32, &storage_hash, &signature_hash);

    client.upgrade_with_manifest_and_signatures(
        &candidate_hash,
        &10u32,
        &5u32,
        &storage_hash,
        &signature_hash,
    );

    let manifest = client.get_upgrade_manifest().unwrap();
    assert_eq!(manifest.version, 2);
    assert_eq!(manifest.exported_fn_count, 10);
    assert_eq!(manifest.error_code_count, 5);
    assert_eq!(manifest.storage_schema_hash, storage_hash);
    assert_eq!(
        client.get_upgrade_function_signatures_hash(),
        Some(signature_hash.clone())
    );

    // This admin-only call proves initialization state survived the WASM swap.
    client.set_upgrade_manifest_with_signatures(&11u32, &6u32, &storage_hash, &signature_hash);
    client.validate_upgrade_compatibility_with_signatures(
        &11u32,
        &6u32,
        &storage_hash,
        &signature_hash,
    );
    assert_eq!(client.get_upgrade_manifest().unwrap().version, 3);
}
