#![cfg(test)]

use soroban_sdk::{
    bytes,
    testutils::{Address as _, Events as _, Ledger},
    vec, Address, Bytes, Env, Map, Vec,
};

mod tests {
    use super::*;

    /// Represents a threshold proof verification scheme
    #[derive(Clone)]
    struct VerifierContribution {
        verifier_id: u32,
        proof: Bytes,
        verified: bool,
    }

    struct ThresholdProofScheme {
        proofs: Vec<VerifierContribution>,
        threshold: u32,
        total_verifiers: u32,
    }

    impl ThresholdProofScheme {
        fn new(env: &Env, threshold: u32, total_verifiers: u32) -> Self {
            Self {
                proofs: Vec::new(env),
                threshold,
                total_verifiers,
            }
        }

        fn add_proof(&mut self, env: &Env, verifier_id: u32, proof: Bytes) -> Result<(), String> {
            if verifier_id >= self.total_verifiers {
                return Err("Verifier ID out of range".to_string());
            }

            for contribution in self.proofs.iter() {
                if contribution.verifier_id == verifier_id {
                    return Err("Verifier already contributed".to_string());
                }
            }

            self.proofs.push_back(VerifierContribution {
                verifier_id,
                proof,
                verified: true,
            });

            Ok(())
        }

        fn get_contribution_count(&self) -> u32 {
            self.proofs.len() as u32
        }

        fn verify_threshold(&self) -> bool {
            self.get_contribution_count() >= self.threshold
        }

        fn verify_threshold_proof(&self) -> Result<bool, String> {
            if self.get_contribution_count() < self.threshold {
                return Err(format!(
                    "Insufficient proofs: {} < {}",
                    self.get_contribution_count(),
                    self.threshold
                ));
            }

            let mut verified_count = 0u32;
            for contribution in self.proofs.iter() {
                if contribution.verified {
                    verified_count += 1;
                }
            }

            Ok(verified_count >= self.threshold)
        }

        fn get_threshold(&self) -> u32 {
            self.threshold
        }

        fn get_total_verifiers(&self) -> u32 {
            self.total_verifiers
        }

        fn is_threshold_valid(&self) -> bool {
            self.threshold > 0 && self.threshold <= self.total_verifiers
        }

        fn get_contribution_percentage(&self) -> f64 {
            if self.total_verifiers == 0 {
                return 0.0;
            }
            (self.get_contribution_count() as f64 / self.total_verifiers as f64) * 100.0
        }
    }

    #[test]
    fn test_threshold_scheme_creation() {
        let env = Env::default();
        let scheme = ThresholdProofScheme::new(&env, 3, 5);

        assert_eq!(scheme.get_threshold(), 3);
        assert_eq!(scheme.get_total_verifiers(), 5);
    }

    #[test]
    fn test_add_proof_to_scheme() {
        let env = Env::default();
        let mut scheme = ThresholdProofScheme::new(&env, 3, 5);

        let proof = bytes!(&env, 0xdeadbeef);
        assert!(scheme.add_proof(&env, 0, proof).is_ok());
        assert_eq!(scheme.get_contribution_count(), 1);
    }

    #[test]
    fn test_multiple_proofs_added() {
        let env = Env::default();
        let mut scheme = ThresholdProofScheme::new(&env, 3, 5);

        for i in 0..3 {
            let proof = bytes!(&env, (i as u8));
            assert!(scheme.add_proof(&env, i, proof).is_ok());
        }

        assert_eq!(scheme.get_contribution_count(), 3);
    }

    #[test]
    fn test_duplicate_verifier_rejected() {
        let env = Env::default();
        let mut scheme = ThresholdProofScheme::new(&env, 3, 5);

        let proof1 = bytes!(&env, 0x01);
        let proof2 = bytes!(&env, 0x02);

        assert!(scheme.add_proof(&env, 0, proof1).is_ok());
        assert!(scheme.add_proof(&env, 0, proof2).is_err());
    }

    #[test]
    fn test_invalid_verifier_id_rejected() {
        let env = Env::default();
        let mut scheme = ThresholdProofScheme::new(&env, 3, 5);

        let proof = bytes!(&env, 0xdeadbeef);
        assert!(scheme.add_proof(&env, 10, proof).is_err());
    }

    #[test]
    fn test_threshold_not_met() {
        let env = Env::default();
        let mut scheme = ThresholdProofScheme::new(&env, 3, 5);

        for i in 0..2 {
            let proof = bytes!(&env, (i as u8));
            scheme.add_proof(&env, i, proof).ok();
        }

        assert!(!scheme.verify_threshold());
    }

    #[test]
    fn test_threshold_exactly_met() {
        let env = Env::default();
        let mut scheme = ThresholdProofScheme::new(&env, 3, 5);

        for i in 0..3 {
            let proof = bytes!(&env, (i as u8));
            scheme.add_proof(&env, i, proof).ok();
        }

        assert!(scheme.verify_threshold());
    }

    #[test]
    fn test_threshold_exceeded() {
        let env = Env::default();
        let mut scheme = ThresholdProofScheme::new(&env, 3, 5);

        for i in 0..5 {
            let proof = bytes!(&env, (i as u8));
            scheme.add_proof(&env, i, proof).ok();
        }

        assert!(scheme.verify_threshold());
        assert_eq!(scheme.get_contribution_count(), 5);
    }

    #[test]
    fn test_verify_threshold_proof_success() {
        let env = Env::default();
        let mut scheme = ThresholdProofScheme::new(&env, 3, 5);

        for i in 0..3 {
            let proof = bytes!(&env, (i as u8));
            scheme.add_proof(&env, i, proof).ok();
        }

        assert!(scheme.verify_threshold_proof().is_ok());
        assert!(scheme.verify_threshold_proof().unwrap());
    }

    #[test]
    fn test_verify_threshold_proof_insufficient() {
        let env = Env::default();
        let mut scheme = ThresholdProofScheme::new(&env, 3, 5);

        for i in 0..2 {
            let proof = bytes!(&env, (i as u8));
            scheme.add_proof(&env, i, proof).ok();
        }

        assert!(scheme.verify_threshold_proof().is_err());
    }

    #[test]
    fn test_threshold_1_of_1() {
        let env = Env::default();
        let mut scheme = ThresholdProofScheme::new(&env, 1, 1);

        let proof = bytes!(&env, 0xdeadbeef);
        scheme.add_proof(&env, 0, proof).ok();

        assert!(scheme.verify_threshold());
    }

    #[test]
    fn test_threshold_1_of_n() {
        let env = Env::default();
        let mut scheme = ThresholdProofScheme::new(&env, 1, 5);

        let proof = bytes!(&env, 0xdeadbeef);
        scheme.add_proof(&env, 0, proof).ok();

        assert!(scheme.verify_threshold());
    }

    #[test]
    fn test_threshold_n_of_n() {
        let env = Env::default();
        let mut scheme = ThresholdProofScheme::new(&env, 5, 5);

        for i in 0..5 {
            let proof = bytes!(&env, (i as u8));
            scheme.add_proof(&env, i, proof).ok();
        }

        assert!(scheme.verify_threshold());
    }

    #[test]
    fn test_is_threshold_valid() {
        let env = Env::default();
        let scheme1 = ThresholdProofScheme::new(&env, 3, 5);
        let scheme2 = ThresholdProofScheme::new(&env, 0, 5);
        let scheme3 = ThresholdProofScheme::new(&env, 6, 5);

        assert!(scheme1.is_threshold_valid());
        assert!(!scheme2.is_threshold_valid());
        assert!(!scheme3.is_threshold_valid());
    }

    #[test]
    fn test_contribution_percentage_zero() {
        let env = Env::default();
        let scheme = ThresholdProofScheme::new(&env, 3, 5);

        assert_eq!(scheme.get_contribution_percentage(), 0.0);
    }

    #[test]
    fn test_contribution_percentage_half() {
        let env = Env::default();
        let mut scheme = ThresholdProofScheme::new(&env, 2, 4);

        for i in 0..2 {
            let proof = bytes!(&env, (i as u8));
            scheme.add_proof(&env, i, proof).ok();
        }

        assert_eq!(scheme.get_contribution_percentage(), 50.0);
    }

    #[test]
    fn test_contribution_percentage_full() {
        let env = Env::default();
        let mut scheme = ThresholdProofScheme::new(&env, 5, 5);

        for i in 0..5 {
            let proof = bytes!(&env, (i as u8));
            scheme.add_proof(&env, i, proof).ok();
        }

        assert_eq!(scheme.get_contribution_percentage(), 100.0);
    }

    #[test]
    fn test_multiple_threshold_levels() {
        let env = Env::default();
        let mut scheme2_of_5 = ThresholdProofScheme::new(&env, 2, 5);
        let mut scheme3_of_5 = ThresholdProofScheme::new(&env, 3, 5);

        for i in 0..2 {
            let proof = bytes!(&env, (i as u8));
            scheme2_of_5.add_proof(&env, i, proof.clone()).ok();
            scheme3_of_5.add_proof(&env, i, proof).ok();
        }

        assert!(scheme2_of_5.verify_threshold());
        assert!(!scheme3_of_5.verify_threshold());
    }

    #[test]
    fn test_sequential_threshold_progression() {
        let env = Env::default();
        let mut scheme = ThresholdProofScheme::new(&env, 5, 10);

        for i in 0..5 {
            let proof = bytes!(&env, (i as u8));
            scheme.add_proof(&env, i, proof).ok();

            if i < 4 {
                assert!(!scheme.verify_threshold());
            } else {
                assert!(scheme.verify_threshold());
            }
        }
    }

    #[test]
    fn test_all_verifiers_contribute() {
        let env = Env::default();
        let mut scheme = ThresholdProofScheme::new(&env, 3, 3);

        for i in 0..3 {
            let proof = bytes!(&env, (i as u8));
            assert!(scheme.add_proof(&env, i, proof).is_ok());
        }

        assert_eq!(scheme.get_contribution_count(), 3);
        assert_eq!(scheme.get_total_verifiers(), 3);
        assert!(scheme.verify_threshold());
    }
}
