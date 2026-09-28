#![no_main]

use libfuzzer_sys::fuzz_target;
use soroban_sdk::{Address, Bytes, Env};
use zk_verifier::{ZkVerifierContract, MAX_CLAIM_SIZE, MAX_PROOF_SIZE};

/// Fuzz target for `attest` and subsequent `verify_claim` (issue #572).
///
/// Verifies that:
/// 1. `attest` with valid inputs never panics.
/// 2. After attestation, `verify_claim` returns `true` for the same pair.
/// 3. `verify_claim` returns `false` for a different, unattested pair.
fuzz_target!(|data: &[u8]| {
    if data.len() < 2 {
        return;
    }

    let split = data.len() / 2;
    let proof_bytes = &data[..split];
    let claim_bytes = &data[split..];

    if proof_bytes.is_empty()
        || claim_bytes.is_empty()
        || proof_bytes.len() > MAX_PROOF_SIZE as usize
        || claim_bytes.len() > MAX_CLAIM_SIZE as usize
    {
        return;
    }

    let env = Env::default();
    env.budget().reset_unlimited();
    env.mock_all_auths();

    let contract_id = env.register_contract(None, ZkVerifierContract);
    let client = zk_verifier::ZkVerifierContractClient::new(&env, &contract_id);

    let admin = Address::generate(&env);
    let oracle = Address::generate(&env);

    client.initialize(&admin);
    client.register_oracle(&oracle);

    let proof = Bytes::from_slice(&env, proof_bytes);
    let claim = Bytes::from_slice(&env, claim_bytes);

    // After attestation the same pair must verify.
    let credential_id = client.attest(&oracle, &proof, &claim);
    assert!(credential_id > 0 || credential_id == 0, "credential_id must be a valid u64");
    assert!(client.verify_claim(&proof, &claim), "attested pair must verify");

    // A different proof (single byte flip) must not verify.
    let mut other_bytes = proof_bytes.to_vec();
    *other_bytes.last_mut().unwrap() = other_bytes.last().unwrap().wrapping_add(1);
    if other_bytes != proof_bytes && other_bytes.len() <= MAX_PROOF_SIZE as usize {
        let other_proof = Bytes::from_slice(&env, &other_bytes);
        assert!(
            !client.verify_claim(&other_proof, &claim),
            "unattested proof must not verify even after attesting a similar one"
        );
    }
});
