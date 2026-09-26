#![cfg(test)]

//! Executable mirror of the contract examples in
//! `docs/zk-proof-verification-guide.md` §10 (issue #580).
//!
//! Each test corresponds to a snippet or table row in the guide. If you
//! change an example there, change the matching test here (and vice versa).

use soroban_sdk::{testutils::Address as _, Address, Bytes, Env};
use zk_verifier::{ZkVerifierContract, ZkVerifierContractClient, MAX_CLAIM_SIZE, MAX_PROOF_SIZE};

/// Groth16 proof size on BN254: A (64) || B (128) || C (64). Guide §8.2.
const GROTH16_PROOF_BYTES: usize = 256;

struct Fixture {
    env: Env,
    oracle: Address,
    client: ZkVerifierContractClient<'static>,
}

/// Guide §10.2 setup: initialize, register one oracle.
fn setup() -> Fixture {
    let env = Env::default();
    env.mock_all_auths();
    let admin = Address::generate(&env);
    let oracle = Address::generate(&env);

    let contract_id = env.register_contract(None, ZkVerifierContract);
    let client = ZkVerifierContractClient::new(&env, &contract_id);
    client.initialize(&admin);
    client.register_oracle(&oracle);
    let client: ZkVerifierContractClient<'static> = unsafe { core::mem::transmute(client) };

    Fixture { env, oracle, client }
}

/// The guide's placeholder 256-byte proof.
fn groth16_proof(env: &Env) -> Bytes {
    Bytes::from_array(env, &[0x11; GROTH16_PROOF_BYTES])
}

/// One 32-byte big-endian public input (guide §8.3).
fn public_input(env: &Env, value: u8) -> Bytes {
    let mut claim = [0u8; 32];
    claim[31] = value;
    Bytes::from_array(env, &claim)
}

#[test]
fn guide_sizes_fit_contract_limits() {
    assert!(GROTH16_PROOF_BYTES as u32 <= MAX_PROOF_SIZE);
    // Guide §8.3: at most MAX_CLAIM_SIZE / 32 = 32 public inputs.
    assert_eq!(MAX_CLAIM_SIZE / 32, 32);
}

#[test]
fn attested_groth16_proof_verifies() {
    let f = setup();
    let proof = groth16_proof(&f.env);
    let claim = public_input(&f.env, 35);

    let credential_id = f.client.attest(&f.oracle, &proof, &claim);
    // Re-attesting the same pair returns the same stable id.
    assert_eq!(f.client.attest(&f.oracle, &proof, &claim), credential_id);

    assert!(f.client.verify_claim(&proof, &claim));
}

#[test]
fn unattested_proof_does_not_verify() {
    let f = setup();
    assert!(!f
        .client
        .verify_claim(&groth16_proof(&f.env), &public_input(&f.env, 35)));
}

#[test]
fn different_public_input_does_not_verify() {
    let f = setup();
    let proof = groth16_proof(&f.env);
    f.client.attest(&f.oracle, &proof, &public_input(&f.env, 35));

    assert!(!f.client.verify_claim(&proof, &public_input(&f.env, 36)));
}

#[test]
fn revoked_oracle_attestations_no_longer_verify() {
    let f = setup();
    let proof = groth16_proof(&f.env);
    let claim = public_input(&f.env, 35);
    f.client.attest(&f.oracle, &proof, &claim);
    assert!(f.client.verify_claim(&proof, &claim));

    f.client.revoke_oracle(&f.oracle);
    assert!(!f.client.verify_claim(&proof, &claim));
}

#[test]
#[should_panic(expected = "Error(Contract, #1)")]
fn empty_proof_panics_with_empty_proof() {
    let f = setup();
    f.client
        .verify_claim(&Bytes::new(&f.env), &public_input(&f.env, 35));
}

#[test]
#[should_panic(expected = "Error(Contract, #2)")]
fn empty_claim_panics_with_empty_claim() {
    let f = setup();
    f.client
        .verify_claim(&groth16_proof(&f.env), &Bytes::new(&f.env));
}

#[test]
#[should_panic(expected = "Error(Contract, #3)")]
fn oversized_proof_panics_with_proof_too_large() {
    let f = setup();
    let proof = Bytes::from_slice(&f.env, &[0x11; (MAX_PROOF_SIZE as usize) + 1]);
    f.client.verify_claim(&proof, &public_input(&f.env, 35));
}

#[test]
#[should_panic(expected = "Error(Contract, #4)")]
fn oversized_claim_panics_with_claim_too_large() {
    let f = setup();
    let claim = Bytes::from_slice(&f.env, &[0x01; (MAX_CLAIM_SIZE as usize) + 1]);
    f.client.verify_claim(&groth16_proof(&f.env), &claim);
}
