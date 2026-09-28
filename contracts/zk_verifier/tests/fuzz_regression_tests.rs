//! Tests for ZK verifier fuzz targets (issue #572).
//!
//! These are unit-level regression tests that encode the properties
//! exercised by the libfuzzer harnesses in `contracts/zk_verifier/fuzz/`.
//! They run under the normal `cargo test` suite without requiring the
//! nightly fuzzer toolchain.

use soroban_sdk::{testutils::Address as _, Address, Bytes, Env};
use zk_verifier::{
    ZkVerifierContract, ZkVerifierContractClient, LATTICE_PROOF_HEADER, MAX_CLAIM_SIZE,
    MAX_PROOF_SIZE,
};

fn setup() -> (Env, Address, ZkVerifierContractClient<'static>) {
    let env = Env::default();
    env.mock_all_auths();
    let admin = Address::generate(&env);
    let id = env.register_contract(None, ZkVerifierContract);
    let client = ZkVerifierContractClient::new(&env, &id);
    client.initialize(&admin);
    // SAFETY: the Env and client are used only within this test's scope.
    let client: ZkVerifierContractClient<'static> = unsafe { core::mem::transmute(client) };
    (env, admin, client)
}

// ── fuzz_verify_claim regression tests ───────────────────────────────────────

/// Unattested proof must return false, not panic.
#[test]
fn fuzz_verify_claim_unattested_returns_false() {
    let (env, _, client) = setup();
    let proof = Bytes::from_slice(&env, &[0x01, 0x02, 0x03, 0x04]);
    let claim = Bytes::from_slice(&env, &[0x05, 0x06, 0x07, 0x08]);
    assert!(!client.verify_claim(&proof, &claim));
}

/// Maximum-size proof and claim must not panic.
#[test]
fn fuzz_verify_claim_max_size_inputs() {
    let (env, _, client) = setup();
    let proof_data = vec![0xabu8; MAX_PROOF_SIZE as usize];
    let claim_data = vec![0xcdu8; MAX_CLAIM_SIZE as usize];
    let proof = Bytes::from_slice(&env, &proof_data);
    let claim = Bytes::from_slice(&env, &claim_data);
    assert!(!client.verify_claim(&proof, &claim));
}

/// Single-byte proof and claim must not panic.
#[test]
fn fuzz_verify_claim_minimal_inputs() {
    let (env, _, client) = setup();
    let proof = Bytes::from_slice(&env, &[0xff]);
    let claim = Bytes::from_slice(&env, &[0x00]);
    assert!(!client.verify_claim(&proof, &claim));
}

// ── fuzz_verify_lattice_proof regression tests ────────────────────────────────

/// Arbitrary bytes without LATTICE_V1 header must not return true.
#[test]
fn fuzz_verify_lattice_proof_no_header_rejected() {
    let (env, _, client) = setup();
    let proof = Bytes::from_slice(&env, &[0xde, 0xad, 0xbe, 0xef, 0x00, 0x01, 0x02, 0x03]);
    let claim = Bytes::from_slice(&env, &[0xca, 0xfe]);

    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        client.verify_lattice_proof(&proof, &claim)
    }));
    // Either panics with a contract error or returns false; must never be true.
    if let Ok(r) = result {
        assert!(!r, "lattice proof without header must not verify");
    }
}

/// LATTICE_V1 header prefix with invalid checksum must not return true.
#[test]
fn fuzz_verify_lattice_proof_bad_checksum_rejected() {
    let (env, _, client) = setup();

    let mut proof_data = Vec::new();
    proof_data.extend_from_slice(LATTICE_PROOF_HEADER);
    proof_data.extend_from_slice(&[0x00u8; 20]); // body with invalid checksum

    let proof = Bytes::from_slice(&env, &proof_data);
    let claim = Bytes::from_slice(&env, &[0x01, 0x02]);

    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        client.verify_lattice_proof(&proof, &claim)
    }));
    if let Ok(r) = result {
        assert!(!r, "lattice proof with bad checksum must not verify");
    }
}

// ── fuzz_attest regression tests ─────────────────────────────────────────────

/// Attested pair must verify; a byte-flipped proof must not.
#[test]
fn fuzz_attest_attested_verifies_unattested_does_not() {
    let (env, _, client) = setup();
    let oracle = Address::generate(&env);
    client.register_oracle(&oracle);

    let proof = Bytes::from_slice(&env, &[0x10, 0x20, 0x30, 0x40]);
    let claim = Bytes::from_slice(&env, &[0x50, 0x60, 0x70, 0x80]);

    client.attest(&oracle, &proof, &claim);
    assert!(client.verify_claim(&proof, &claim));

    let other_proof = Bytes::from_slice(&env, &[0x10, 0x20, 0x30, 0x41]);
    assert!(!client.verify_claim(&other_proof, &claim));
}

/// Re-attesting the same pair must return the same credential_id.
#[test]
fn fuzz_attest_idempotent_credential_id() {
    let (env, _, client) = setup();
    let oracle = Address::generate(&env);
    client.register_oracle(&oracle);

    let proof = Bytes::from_slice(&env, &[0xaa, 0xbb]);
    let claim = Bytes::from_slice(&env, &[0xcc, 0xdd]);

    let id1 = client.attest(&oracle, &proof, &claim);
    let id2 = client.attest(&oracle, &proof, &claim);
    assert_eq!(id1, id2, "re-attesting the same pair must return the same credential_id");
}
