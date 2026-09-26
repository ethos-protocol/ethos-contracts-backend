#![cfg(test)]

use soroban_sdk::{
    bytes,
    testutils::{Address as _, Events as _, Ledger},
    vec, Address, Bytes, BytesN, Env, Map, Vec,
};

mod tests {
    use super::*;

    /// Mock cache structure to track proof caching behavior
    #[derive(Clone)]
    struct ProofCache {
        cached_proofs: Map<BytesN<32>, Bytes>,
        cache_size: u32,
        max_cache_size: u32,
    }

    impl ProofCache {
        fn new(max_size: u32) -> Self {
            Self {
                cached_proofs: Map::new(&Env::default()),
                cache_size: 0,
                max_cache_size: max_size,
            }
        }

        fn cache_proof(&mut self, env: &Env, proof: &Bytes) -> Result<BytesN<32>, String> {
            let proof_hash = compute_proof_hash(proof);

            if self.cached_proofs.contains_key(&proof_hash) {
                return Ok(proof_hash);
            }

            if self.cache_size + proof.len() as u32 > self.max_cache_size {
                return Err("Cache full".to_string());
            }

            self.cache_size += proof.len() as u32;
            Ok(proof_hash)
        }

        fn is_cached(&self, proof_hash: &BytesN<32>) -> bool {
            self.cached_proofs.contains_key(proof_hash)
        }

        fn invalidate_cache(&mut self, proof_hash: &BytesN<32>) -> bool {
            if self.cached_proofs.contains_key(proof_hash) {
                self.cached_proofs.remove(*proof_hash);
                true
            } else {
                false
            }
        }

        fn get_cache_size(&self) -> u32 {
            self.cache_size
        }
    }

    fn compute_proof_hash(proof: &Bytes) -> BytesN<32> {
        let mut hash = [0u8; 32];
        for (i, byte) in proof.iter().enumerate() {
            if i < 32 {
                hash[i] = byte;
            }
        }
        BytesN::from_array(&Env::default(), &hash)
    }

    #[test]
    fn test_cache_proof_adds_to_cache() {
        let env = Env::default();
        let mut cache = ProofCache::new(4096);

        let proof = bytes!(&env, 0xdeadbeef);
        let result = cache.cache_proof(&env, &proof);

        assert!(result.is_ok());
        let proof_hash = result.unwrap();
        assert!(cache.is_cached(&proof_hash));
    }

    #[test]
    fn test_cache_proof_increments_cache_size() {
        let env = Env::default();
        let mut cache = ProofCache::new(4096);

        let proof = bytes!(&env, 0xdeadbeef);
        cache.cache_proof(&env, &proof).ok();

        assert_eq!(cache.get_cache_size(), 4);
    }

    #[test]
    fn test_cache_respects_size_limits() {
        let env = Env::default();
        let mut cache = ProofCache::new(10);

        let proof1 = bytes!(&env, 0xde, 0xad, 0xbe, 0xef);
        let proof2 = bytes!(&env, 0xca, 0xfe, 0xba, 0xbe);
        let proof3 = bytes!(&env, 0x12, 0x34, 0x56, 0x78);

        assert!(cache.cache_proof(&env, &proof1).is_ok());
        assert!(cache.cache_proof(&env, &proof2).is_ok());
        assert!(cache.cache_proof(&env, &proof3).is_err());
    }

    #[test]
    fn test_cache_same_proof_twice() {
        let env = Env::default();
        let mut cache = ProofCache::new(4096);

        let proof = bytes!(&env, 0xdeadbeef);
        let result1 = cache.cache_proof(&env, &proof);
        let result2 = cache.cache_proof(&env, &proof);

        assert!(result1.is_ok());
        assert!(result2.is_ok());
        assert_eq!(result1.unwrap(), result2.unwrap());
        assert_eq!(cache.get_cache_size(), 4);
    }

    #[test]
    fn test_cache_invalidation() {
        let env = Env::default();
        let mut cache = ProofCache::new(4096);

        let proof = bytes!(&env, 0xdeadbeef);
        let cached_result = cache.cache_proof(&env, &proof).unwrap();

        assert!(cache.is_cached(&cached_result));
        assert!(cache.invalidate_cache(&cached_result));
        assert!(!cache.is_cached(&cached_result));
    }

    #[test]
    fn test_cache_invalidation_on_nonexistent_proof() {
        let env = Env::default();
        let cache = ProofCache::new(4096);

        let hash = BytesN::from_array(&env, &[0u8; 32]);
        assert!(!cache.invalidate_cache(&hash));
    }

    #[test]
    fn test_multiple_proofs_cache_independently() {
        let env = Env::default();
        let mut cache = ProofCache::new(4096);

        let proof1 = bytes!(&env, 0xdeadbeef);
        let proof2 = bytes!(&env, 0xcafebabe);

        let hash1 = cache.cache_proof(&env, &proof1).unwrap();
        let hash2 = cache.cache_proof(&env, &proof2).unwrap();

        assert_ne!(hash1, hash2);
        assert!(cache.is_cached(&hash1));
        assert!(cache.is_cached(&hash2));
    }

    #[test]
    fn test_cache_invalidation_affects_only_target_proof() {
        let env = Env::default();
        let mut cache = ProofCache::new(4096);

        let proof1 = bytes!(&env, 0xdeadbeef);
        let proof2 = bytes!(&env, 0xcafebabe);

        let hash1 = cache.cache_proof(&env, &proof1).unwrap();
        let hash2 = cache.cache_proof(&env, &proof2).unwrap();

        cache.invalidate_cache(&hash1);
        assert!(!cache.is_cached(&hash1));
        assert!(cache.is_cached(&hash2));
    }

    #[test]
    fn test_cache_size_after_multiple_operations() {
        let env = Env::default();
        let mut cache = ProofCache::new(4096);

        let proof1 = bytes!(&env, 0xde);
        let proof2 = bytes!(&env, 0xad);
        let proof3 = bytes!(&env, 0xbe);

        cache.cache_proof(&env, &proof1).ok();
        assert_eq!(cache.get_cache_size(), 1);

        cache.cache_proof(&env, &proof2).ok();
        assert_eq!(cache.get_cache_size(), 2);

        cache.cache_proof(&env, &proof3).ok();
        assert_eq!(cache.get_cache_size(), 3);
    }

    #[test]
    fn test_cache_boundary_exact_size() {
        let env = Env::default();
        let mut cache = ProofCache::new(4);

        let proof = bytes!(&env, 0xde, 0xad, 0xbe, 0xef);
        assert!(cache.cache_proof(&env, &proof).is_ok());
    }

    #[test]
    fn test_cache_boundary_exceeds_by_one_byte() {
        let env = Env::default();
        let mut cache = ProofCache::new(3);

        let proof = bytes!(&env, 0xde, 0xad, 0xbe, 0xef);
        assert!(cache.cache_proof(&env, &proof).is_err());
    }
}
