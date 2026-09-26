#![cfg(test)]

use soroban_sdk::{
    bytes,
    testutils::{Address as _, Events as _, Ledger},
    vec, Address, Bytes, Env,
};

mod tests {
    use super::*;

    /// Helper to compute compression ratio
    fn compute_compression_ratio(original_size: u32, compressed_size: u32) -> f64 {
        if original_size == 0 {
            return 0.0;
        }
        (compressed_size as f64 / original_size as f64) * 100.0
    }

    /// Mock compression function using simple RLE-like approach
    fn mock_compress_proof(env: &Env, proof: &Bytes) -> Result<Bytes, String> {
        if proof.is_empty() {
            return Err("Empty proof".to_string());
        }

        let mut compressed = Bytes::new(env);
        compressed.push_back(0xC0);

        let mut i: u32 = 0;
        let len = proof.len();

        while i < len {
            let current = proof.get(i).unwrap();
            let mut run: u32 = 1;

            while run < 255 && (i + run) < len && proof.get(i + run).unwrap() == current {
                run += 1;
            }

            compressed.push_back(run as u8);
            compressed.push_back(current);
            i += run;
        }

        Ok(compressed)
    }

    /// Mock decompression function
    fn mock_decompress_proof(env: &Env, compressed: &Bytes, max_size: u32) -> Result<Bytes, String> {
        if compressed.is_empty() {
            return Err("Empty compressed proof".to_string());
        }

        if compressed.get(0).unwrap() != 0xC0 {
            return Err("Invalid magic byte".to_string());
        }

        let mut decompressed = Bytes::new(env);
        let mut i: u32 = 1;
        let compressed_len = compressed.len();

        while i < compressed_len {
            if i + 1 >= compressed_len {
                return Err("Truncated compressed data".to_string());
            }

            let count = compressed.get(i).unwrap();
            if count == 0 {
                return Err("Invalid run count".to_string());
            }

            let byte = compressed.get(i + 1).unwrap();
            for _ in 0..count {
                if decompressed.len() >= max_size {
                    return Err("Decompressed size exceeds limit".to_string());
                }
                decompressed.push_back(byte);
            }

            i += 2;
        }

        Ok(decompressed)
    }

    #[test]
    fn test_compress_repeated_bytes() {
        let env = Env::default();
        let proof = bytes!(&env, 0xaa, 0xaa, 0xaa, 0xaa);

        let compressed = mock_compress_proof(&env, &proof).unwrap();
        assert!(compressed.len() < proof.len());
        assert_eq!(compressed.get(0).unwrap(), 0xC0);
    }

    #[test]
    fn test_compress_diverse_bytes() {
        let env = Env::default();
        let proof = bytes!(&env, 0xaa, 0xbb, 0xcc, 0xdd);

        let compressed = mock_compress_proof(&env, &proof).unwrap();
        assert!(compressed.len() > proof.len());
    }

    #[test]
    fn test_compress_empty_proof_fails() {
        let env = Env::default();
        let proof = bytes!(&env,);

        let result = mock_compress_proof(&env, &proof);
        assert!(result.is_err());
    }

    #[test]
    fn test_decompress_returns_original() {
        let env = Env::default();
        let original = bytes!(&env, 0xaa, 0xaa, 0xaa, 0xaa);

        let compressed = mock_compress_proof(&env, &original).unwrap();
        let decompressed = mock_decompress_proof(&env, &compressed, 4096).unwrap();

        assert_eq!(original.len(), decompressed.len());
    }

    #[test]
    fn test_decompress_invalid_magic_fails() {
        let env = Env::default();
        let invalid = bytes!(&env, 0xFF, 0x01, 0xaa);

        let result = mock_decompress_proof(&env, &invalid, 4096);
        assert!(result.is_err());
    }

    #[test]
    fn test_decompress_empty_fails() {
        let env = Env::default();
        let empty = bytes!(&env,);

        let result = mock_decompress_proof(&env, &empty, 4096);
        assert!(result.is_err());
    }

    #[test]
    fn test_decompress_truncated_fails() {
        let env = Env::default();
        let truncated = bytes!(&env, 0xC0, 0x04);

        let result = mock_decompress_proof(&env, &truncated, 4096);
        assert!(result.is_err());
    }

    #[test]
    fn test_decompress_zero_count_fails() {
        let env = Env::default();
        let invalid = bytes!(&env, 0xC0, 0x00, 0xaa);

        let result = mock_decompress_proof(&env, &invalid, 4096);
        assert!(result.is_err());
    }

    #[test]
    fn test_compress_decompress_roundtrip() {
        let env = Env::default();
        let original = bytes!(&env, 0xaa, 0xaa, 0xbb, 0xbb, 0xcc);

        let compressed = mock_compress_proof(&env, &original).unwrap();
        let decompressed = mock_decompress_proof(&env, &compressed, 4096).unwrap();

        assert_eq!(original.len(), decompressed.len());
        for i in 0..original.len() {
            assert_eq!(original.get(i).unwrap(), decompressed.get(i).unwrap());
        }
    }

    #[test]
    fn test_compression_ratio_repeated_pattern() {
        let env = Env::default();
        let proof = bytes!(&env, 0xaa, 0xaa, 0xaa, 0xaa, 0xaa, 0xaa, 0xaa, 0xaa);

        let compressed = mock_compress_proof(&env, &proof).unwrap();
        let ratio = compute_compression_ratio(proof.len(), compressed.len());

        assert!(ratio < 100.0);
    }

    #[test]
    fn test_compression_size_limit_enforcement() {
        let env = Env::default();
        let large_proof = {
            let mut p = Bytes::new(&env);
            for _ in 0..300 {
                p.push_back(0xaa);
            }
            p
        };

        let compressed = mock_compress_proof(&env, &large_proof).unwrap();
        assert!(compressed.len() < 4096);
    }

    #[test]
    fn test_decompress_with_size_limit() {
        let env = Env::default();
        let original = bytes!(&env, 0xaa, 0xaa, 0xaa, 0xaa);
        let compressed = mock_compress_proof(&env, &original).unwrap();

        let decompressed = mock_decompress_proof(&env, &compressed, 100);
        assert!(decompressed.is_ok());
    }

    #[test]
    fn test_decompress_exceeds_size_limit() {
        let env = Env::default();
        let original = bytes!(&env, 0xaa, 0xaa, 0xaa, 0xaa);
        let compressed = mock_compress_proof(&env, &original).unwrap();

        let decompressed = mock_decompress_proof(&env, &compressed, 2);
        assert!(decompressed.is_err());
    }

    #[test]
    fn test_single_byte_proof_compression() {
        let env = Env::default();
        let proof = bytes!(&env, 0xaa);

        let compressed = mock_compress_proof(&env, &proof).unwrap();
        let decompressed = mock_decompress_proof(&env, &compressed, 4096).unwrap();

        assert_eq!(decompressed.len(), 1);
        assert_eq!(decompressed.get(0).unwrap(), 0xaa);
    }

    #[test]
    fn test_mixed_run_and_diverse_bytes() {
        let env = Env::default();
        let proof = bytes!(&env, 0xaa, 0xaa, 0xbb, 0xbb, 0xcc, 0xcc, 0xdd);

        let compressed = mock_compress_proof(&env, &proof).unwrap();
        let decompressed = mock_decompress_proof(&env, &compressed, 4096).unwrap();

        assert_eq!(proof.len(), decompressed.len());
    }

    #[test]
    fn test_long_run_exceeding_byte_max() {
        let env = Env::default();
        let mut proof = Bytes::new(&env);
        for _ in 0..300 {
            proof.push_back(0xaa);
        }

        let compressed = mock_compress_proof(&env, &proof).unwrap();
        let decompressed = mock_decompress_proof(&env, &compressed, 4096).unwrap();

        assert_eq!(proof.len(), decompressed.len());
    }
}
