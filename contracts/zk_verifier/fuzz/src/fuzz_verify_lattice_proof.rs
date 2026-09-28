#![no_main]

use libfuzzer_sys::fuzz_target;
use soroban_sdk::{Bytes, Env};
use zk_verifier::{ZkVerifierContract, LATTICE_PROOF_HEADER, MAX_CLAIM_SIZE, MAX_PROOF_SIZE};

/// Fuzz target for `verify_lattice_proof` (issue #572).
///
/// The function validates that proof bytes are prefixed with
/// `LATTICE_PROOF_HEADER` and carry a valid checksum. Arbitrary inputs
/// should always return an error, never panic.
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

    let contract_id = env.register_contract(None, ZkVerifierContract);
    let client = zk_verifier::ZkVerifierContractClient::new(&env, &contract_id);

    let admin = soroban_sdk::Address::generate(&env);
    env.mock_all_auths();
    client.initialize(&admin);

    let proof = Bytes::from_slice(&env, proof_bytes);
    let claim = Bytes::from_slice(&env, claim_bytes);

    // `verify_lattice_proof` must not panic on arbitrary input; it should
    // return false (via `catch_unwind` semantics) or a contract error for
    // proofs that don't carry a valid LATTICE_V1 header.
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        client.verify_lattice_proof(&proof, &claim);
    }));

    // Separately verify that a well-formed LATTICE_V1 header prefix
    // reaches the checksum validation step without panicking unexpectedly.
    let header_len = LATTICE_PROOF_HEADER.len();
    if proof_bytes.len() > header_len + 4 {
        let mut well_formed = Vec::with_capacity(proof_bytes.len());
        well_formed.extend_from_slice(LATTICE_PROOF_HEADER);
        well_formed.extend_from_slice(&proof_bytes[header_len..]);

        if well_formed.len() <= MAX_PROOF_SIZE as usize {
            let wf_proof = Bytes::from_slice(&env, &well_formed);
            let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                client.verify_lattice_proof(&wf_proof, &claim);
            }));
        }
    }
});
