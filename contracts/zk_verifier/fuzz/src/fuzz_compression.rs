#![no_main]

use libfuzzer_sys::fuzz_target;
use soroban_sdk::{Bytes, Env};
use zk_verifier::{compression, MAX_PROOF_SIZE};

fuzz_target!(|data: &[u8]| {
    // These are the proof bytes accepted by the verifier: non-empty and no
    // larger than MAX_PROOF_SIZE. Inputs outside that domain are not relevant
    // to the roundtrip invariant.
    if data.is_empty() || data.len() > MAX_PROOF_SIZE as usize {
        return;
    }

    let env = Env::default();
    // The default testutils host budget is exhausted by a worst-case
    // roundtrip (4096 incompressible bytes), which would abort the harness
    // before it can check the invariant. Lift the budget so the roundtrip
    // property is tested over the whole valid input domain.
    env.budget().reset_unlimited();
    let original = Bytes::from_slice(&env, data);

    // Assert on the error return rather than panicking: a non-empty proof
    // within the size limit must be compressible, so any `Err` is a failure.
    let compressed = match compression::compress_proof(&env, &original) {
        Ok(compressed) => compressed,
        Err(err) => panic!("a non-empty proof must be compressible, got error: {err:?}"),
    };

    // Likewise, a compressed proof within the size limit must be
    // decompressible; assert on the error return instead of panicking.
    let decompressed = match compression::decompress_proof(&env, &compressed, MAX_PROOF_SIZE) {
        Ok(decompressed) => decompressed,
        Err(err) => panic!(
            "a compressed proof within the size limit must be decompressible, got error: {err:?}"
        ),
    };

    assert_eq!(decompressed.len(), original.len());
    for i in 0..original.len() {
        assert_eq!(decompressed.get(i), original.get(i), "byte mismatch at index {i}");
    }
});