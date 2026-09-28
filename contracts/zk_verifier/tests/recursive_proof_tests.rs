#![cfg(test)]

use soroban_sdk::{
    bytes,
    testutils::{Address as _, Events as _, Ledger},
    vec, Address, Bytes, Env, Vec,
};

mod tests {
    use super::*;

    /// Represents a recursive proof structure with nested proofs
    #[derive(Clone)]
    struct RecursiveProof {
        proof_data: Bytes,
        depth: u32,
        nested_proof: Option<Box<RecursiveProof>>,
    }

    impl RecursiveProof {
        fn new(env: &Env, proof_data: Bytes) -> Self {
            Self {
                proof_data,
                depth: 0,
                nested_proof: None,
            }
        }

        fn with_nested_proof(env: &Env, proof_data: Bytes, nested: RecursiveProof) -> Self {
            Self {
                proof_data,
                depth: nested.depth + 1,
                nested_proof: Some(Box::new(nested)),
            }
        }

        fn get_depth(&self) -> u32 {
            self.depth
        }

        fn is_valid_depth(&self, max_depth: u32) -> bool {
            self.depth <= max_depth
        }

        fn get_nested(&self) -> Option<&RecursiveProof> {
            self.nested_proof.as_ref().map(|b| b.as_ref())
        }

        fn verify_recursive_structure(&self, max_depth: u32) -> Result<bool, String> {
            if self.depth > max_depth {
                return Err(format!(
                    "Recursion depth {} exceeds maximum {}",
                    self.depth, max_depth
                ));
            }

            if let Some(nested) = &self.nested_proof {
                nested.verify_recursive_structure(max_depth)?;
            }

            Ok(true)
        }

        fn flatten_proofs(&self) -> Vec<Bytes> {
            let env = Env::default();
            let mut proofs = vec![&env];
            proofs.push_back(self.proof_data.clone());

            if let Some(nested) = &self.nested_proof {
                for proof in nested.flatten_proofs() {
                    proofs.push_back(proof);
                }
            }

            proofs
        }

        fn count_proofs(&self) -> u32 {
            let mut count = 1u32;
            if let Some(nested) = &self.nested_proof {
                count += nested.count_proofs();
            }
            count
        }
    }

    #[test]
    fn test_single_proof_depth_zero() {
        let env = Env::default();
        let proof = bytes!(&env, 0xdeadbeef);
        let recursive_proof = RecursiveProof::new(&env, proof);

        assert_eq!(recursive_proof.get_depth(), 0);
    }

    #[test]
    fn test_nested_proof_depth_one() {
        let env = Env::default();
        let inner_proof = bytes!(&env, 0xdeadbeef);
        let outer_proof = bytes!(&env, 0xcafebabe);

        let inner_recursive = RecursiveProof::new(&env, inner_proof);
        let outer_recursive = RecursiveProof::with_nested_proof(&env, outer_proof, inner_recursive);

        assert_eq!(outer_recursive.get_depth(), 1);
    }

    #[test]
    fn test_deeply_nested_proof() {
        let env = Env::default();
        let mut current = RecursiveProof::new(&env, bytes!(&env, 0x01));

        for i in 1..=5 {
            let proof = bytes!(&env, (i as u8));
            current = RecursiveProof::with_nested_proof(&env, proof, current);
        }

        assert_eq!(current.get_depth(), 5);
    }

    #[test]
    fn test_depth_validation_within_limit() {
        let env = Env::default();
        let inner = RecursiveProof::new(&env, bytes!(&env, 0x01));
        let outer = RecursiveProof::with_nested_proof(&env, bytes!(&env, 0x02), inner);

        assert!(outer.is_valid_depth(2));
        assert!(outer.is_valid_depth(1));
    }

    #[test]
    fn test_depth_validation_exceeds_limit() {
        let env = Env::default();
        let inner = RecursiveProof::new(&env, bytes!(&env, 0x01));
        let outer = RecursiveProof::with_nested_proof(&env, bytes!(&env, 0x02), inner);

        assert!(!outer.is_valid_depth(0));
    }

    #[test]
    fn test_recursive_structure_validation_success() {
        let env = Env::default();
        let inner = RecursiveProof::new(&env, bytes!(&env, 0x01));
        let outer = RecursiveProof::with_nested_proof(&env, bytes!(&env, 0x02), inner);

        assert!(outer.verify_recursive_structure(5).is_ok());
    }

    #[test]
    fn test_recursive_structure_validation_exceeds_depth() {
        let env = Env::default();
        let mut current = RecursiveProof::new(&env, bytes!(&env, 0x01));

        for i in 1..=10 {
            current = RecursiveProof::with_nested_proof(&env, bytes!(&env, i as u8), current);
        }

        assert!(current.verify_recursive_structure(5).is_err());
    }

    #[test]
    fn test_nested_proof_access() {
        let env = Env::default();
        let inner = RecursiveProof::new(&env, bytes!(&env, 0x01));
        let outer = RecursiveProof::with_nested_proof(&env, bytes!(&env, 0x02), inner);

        assert!(outer.get_nested().is_some());
        assert_eq!(outer.get_nested().unwrap().get_depth(), 0);
    }

    #[test]
    fn test_flatten_proofs_single() {
        let env = Env::default();
        let proof = bytes!(&env, 0xdeadbeef);
        let recursive_proof = RecursiveProof::new(&env, proof.clone());

        let flattened = recursive_proof.flatten_proofs();
        assert_eq!(flattened.len(), 1);
    }

    #[test]
    fn test_flatten_proofs_nested() {
        let env = Env::default();
        let inner = RecursiveProof::new(&env, bytes!(&env, 0x01));
        let outer = RecursiveProof::with_nested_proof(&env, bytes!(&env, 0x02), inner);

        let flattened = outer.flatten_proofs();
        assert_eq!(flattened.len(), 2);
    }

    #[test]
    fn test_count_proofs_single() {
        let env = Env::default();
        let proof = bytes!(&env, 0xdeadbeef);
        let recursive_proof = RecursiveProof::new(&env, proof);

        assert_eq!(recursive_proof.count_proofs(), 1);
    }

    #[test]
    fn test_count_proofs_nested() {
        let env = Env::default();
        let inner = RecursiveProof::new(&env, bytes!(&env, 0x01));
        let outer = RecursiveProof::with_nested_proof(&env, bytes!(&env, 0x02), inner);

        assert_eq!(outer.count_proofs(), 2);
    }

    #[test]
    fn test_count_proofs_deeply_nested() {
        let env = Env::default();
        let mut current = RecursiveProof::new(&env, bytes!(&env, 0x01));

        for i in 1..=5 {
            current = RecursiveProof::with_nested_proof(&env, bytes!(&env, i as u8), current);
        }

        assert_eq!(current.count_proofs(), 6);
    }

    #[test]
    fn test_max_depth_constant_32() {
        let env = Env::default();
        let mut current = RecursiveProof::new(&env, bytes!(&env, 0x01));

        for i in 1..=32 {
            current = RecursiveProof::with_nested_proof(&env, bytes!(&env, i as u8), current);
        }

        assert!(current.is_valid_depth(32));
        assert!(!current.is_valid_depth(31));
    }

    #[test]
    fn test_multiple_independent_nested_chains() {
        let env = Env::default();
        let chain1 = {
            let inner = RecursiveProof::new(&env, bytes!(&env, 0x01));
            RecursiveProof::with_nested_proof(&env, bytes!(&env, 0x02), inner)
        };

        let chain2 = {
            let inner = RecursiveProof::new(&env, bytes!(&env, 0x03));
            RecursiveProof::with_nested_proof(&env, bytes!(&env, 0x04), inner)
        };

        assert_eq!(chain1.count_proofs(), 2);
        assert_eq!(chain2.count_proofs(), 2);
        assert_ne!(chain1.flatten_proofs()[0], chain2.flatten_proofs()[0]);
    }
}
