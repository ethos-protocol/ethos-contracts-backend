#![no_main]

use libfuzzer_sys::fuzz_target;
use soroban_sdk::{Bytes, Env};
use zk_verifier::{ZkVerifierContract, MAX_CLAIM_SIZE, MAX_PROOF_SIZE};

/// Fuzz target for `verify_claim` (issue #572).
///
/// Feeds arbitrary byte pairs as `(proof, claim)` to `verify_claim`.
/// The function must never panic on any input within the valid domain
/// (non-empty, within size limits); it must always return `false` for
/// unattested inputs.
fuzz_target!(|data: &[u8]| {
    // Need at least 2 bytes: 1 for proof, 1 for claim.
    if data.len() < 2 {
        return;
    }

    // Split the input: first half → proof, second half → claim.
    let split = data.len() / 2;
    let proof_bytes = &data[..split];
    let claim_bytes = &data[split..];

    // Skip empty slices or slices exceeding the contract's size limits.
    if proof_bytes.is_empty()
        || claim_bytes.is_empty()
        || proof_bytes.len() > MAX_PROOF_SIZE as usize
        || claim_bytes.len() > MAX_CLAIM_SIZE as usize
    {
        return;
    }

    let env = Env::default();
    env.budget().reset_unlimited();

    let contract_id = env.register_contract(None, ZkVerifierContract);
    let client = zk_verifier::ZkVerifierContractClient::new(&env, &contract_id);

    let admin = soroban_sdk::Address::generate(&env);
    env.mock_all_auths();
    client.initialize(&admin);

    let proof = Bytes::from_slice(&env, proof_bytes);
    let claim = Bytes::from_slice(&env, claim_bytes);

    // An unattested proof must return false, never panic.
    let result = client.verify_claim(&proof, &claim);
    assert!(!result, "unattested proof must not verify");
});
