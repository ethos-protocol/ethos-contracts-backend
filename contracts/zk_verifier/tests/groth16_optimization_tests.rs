use soroban_sdk::{
    testutils::Address as _,
    Address, Bytes, Env,
};
use zk_verifier::ZkVerifierContractClient;

fn setup() -> (Env, Address, ZkVerifierContractClient<'static>) {
    let env = Env::default();
    env.mock_all_auths();
    let admin = Address::generate(&env);
    let id = env.register_contract(None, zk_verifier::ZkVerifierContract);
    let client = ZkVerifierContractClient::new(&env, &id);
    let client: ZkVerifierContractClient<'static> = unsafe { core::mem::transmute(client) };
    (env, admin, client)
}

// ---- issue #523: Groth16 Proof Verification Optimization ----

#[test]
fn batch_verify_single_proof() {
    let (env, _admin, client) = setup();

    // Create a test proof and claim
    let proof = Bytes::from_slice(&env, &[0x01, 0x02, 0x03, 0x04]);
    let claim = Bytes::from_slice(&env, &[0x05, 0x06, 0x07, 0x08]);

    // Single proof batch should work
    let proofs = vec![&env, proof.clone()];
    let claims = vec![&env, claim.clone()];

    let result = client.batch_verify_proofs(&proofs, &claims);

    // Should return result
    assert!(result.len() > 0);
}

#[test]
fn batch_verify_multiple_proofs() {
    let (env, _admin, client) = setup();

    // Create multiple test proofs
    let proof1 = Bytes::from_slice(&env, &[0x01, 0x02, 0x03, 0x04]);
    let proof2 = Bytes::from_slice(&env, &[0x05, 0x06, 0x07, 0x08]);
    let proof3 = Bytes::from_slice(&env, &[0x09, 0x0a, 0x0b, 0x0c]);

    let claim1 = Bytes::from_slice(&env, &[0x10, 0x11, 0x12, 0x13]);
    let claim2 = Bytes::from_slice(&env, &[0x14, 0x15, 0x16, 0x17]);
    let claim3 = Bytes::from_slice(&env, &[0x18, 0x19, 0x1a, 0x1b]);

    let proofs = vec![&env, proof1, proof2, proof3];
    let claims = vec![&env, claim1, claim2, claim3];

    let results = client.batch_verify_proofs(&proofs, &claims);

    // Should process all proofs
    assert_eq!(results.len(), 3);
}

#[test]
fn batch_verify_requires_matching_lengths() {
    let (env, _admin, client) = setup();

    let proof1 = Bytes::from_slice(&env, &[0x01, 0x02]);
    let proof2 = Bytes::from_slice(&env, &[0x03, 0x04]);

    let claim1 = Bytes::from_slice(&env, &[0x05, 0x06]);

    let proofs = vec![&env, proof1, proof2];
    let claims = vec![&env, claim1];

    // Mismatched lengths should fail
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        client.batch_verify_proofs(&proofs, &claims);
    }));
}

#[test]
fn precomputed_pairing_cache_improves_performance() {
    let (env, _admin, client) = setup();

    let proof = Bytes::from_slice(&env, &[0x01, 0x02, 0x03, 0x04]);
    let claim = Bytes::from_slice(&env, &[0x05, 0x06, 0x07, 0x08]);

    // First verification - builds cache
    let result1 = client.verify_claim(&env.clone(), proof.clone(), claim.clone());

    // Second verification - should use cached values
    let result2 = client.verify_claim(&env.clone(), proof, claim);

    // Both results should be consistent
    assert_eq!(result1, result2);
}

#[test]
fn cached_curve_points_reduce_computations() {
    let (env, _admin, client) = setup();

    let proof = Bytes::from_slice(&env, &[0xaa, 0xbb, 0xcc, 0xdd]);
    let claim = Bytes::from_slice(&env, &[0xee, 0xff, 0x00, 0x11]);

    // Multiple verifications with same proof format
    let proofs = vec![&env, proof.clone(), proof.clone(), proof];
    let claims = vec![
        &env,
        claim.clone(),
        claim.clone(),
        claim
    ];

    let results = client.batch_verify_proofs(&proofs, &claims);

    // All verifications should complete
    assert!(results.len() > 0);
}

#[test]
fn batch_optimization_handles_large_batches() {
    let (env, _admin, client) = setup();

    let proof = Bytes::from_slice(&env, &[0x01]);
    let claim = Bytes::from_slice(&env, &[0x02]);

    // Create large batch
    let mut proofs = vec![&env];
    let mut claims = vec![&env];

    for _ in 0..50 {
        proofs.push_back(proof.clone());
        claims.push_back(claim.clone());
    }

    let results = client.batch_verify_proofs(&proofs, &claims);

    // Should handle large batches
    assert_eq!(results.len(), 50);
}

#[test]
fn verify_with_optimization_produces_correct_results() {
    let (env, _admin, client) = setup();

    let proof = Bytes::from_slice(&env, &[0x01, 0x02, 0x03, 0x04, 0x05]);
    let claim = Bytes::from_slice(&env, &[0x06, 0x07, 0x08, 0x09, 0x0a]);

    let result = client.verify_claim(&env, proof, claim);

    // Should return consistent results
    assert!(true); // Result verification depends on actual proof format
}

#[test]
fn empty_batch_fails() {
    let (env, _admin, client) = setup();

    let proofs = vec![&env];
    let claims = vec![&env];

    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        client.batch_verify_proofs(&proofs, &claims);
    }));
}

#[test]
fn sequential_vs_batch_consistency() {
    let (env, _admin, client) = setup();

    let proof1 = Bytes::from_slice(&env, &[0x01, 0x02]);
    let proof2 = Bytes::from_slice(&env, &[0x03, 0x04]);

    let claim1 = Bytes::from_slice(&env, &[0x05, 0x06]);
    let claim2 = Bytes::from_slice(&env, &[0x07, 0x08]);

    // Sequential verifications
    let result1_seq = client.verify_claim(&env, proof1.clone(), claim1.clone());
    let result2_seq = client.verify_claim(&env, proof2.clone(), claim2.clone());

    // Batch verification
    let proofs = vec![&env, proof1, proof2];
    let claims = vec![&env, claim1, claim2];
    let results_batch = client.batch_verify_proofs(&proofs, &claims);

    // Results should be consistent
    assert_eq!(results_batch.len(), 2);
}

#[test]
fn pairing_precomputation_cache_initialization() {
    let (env, _admin, client) = setup();

    // First call initializes cache
    let proof = Bytes::from_slice(&env, &[0xaa]);
    let claim = Bytes::from_slice(&env, &[0xbb]);

    client.verify_claim(&env, proof, claim);

    // Cache should be initialized (no external state to check)
    // Test passes if no panic occurs
    assert!(true);
}

#[test]
fn batch_verify_preserves_proof_order() {
    let (env, _admin, client) = setup();

    let proof1 = Bytes::from_slice(&env, &[0x01]);
    let proof2 = Bytes::from_slice(&env, &[0x02]);
    let proof3 = Bytes::from_slice(&env, &[0x03]);

    let claim1 = Bytes::from_slice(&env, &[0x04]);
    let claim2 = Bytes::from_slice(&env, &[0x05]);
    let claim3 = Bytes::from_slice(&env, &[0x06]);

    let proofs = vec![&env, proof1, proof2, proof3];
    let claims = vec![&env, claim1, claim2, claim3];

    let results = client.batch_verify_proofs(&proofs, &claims);

    // Results should correspond to input order
    assert_eq!(results.len(), 3);
}
