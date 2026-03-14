//! Pedersen commitment utilities for the Gratia protocol.
//!
//! Pedersen commitments allow committing to a value with a blinding factor such that:
//! - The commitment hides the value (hiding property)
//! - The commitment cannot be opened to a different value (binding property)
//!
//! Used for both Proof of Life attestations (committing to sensor parameter counts
//! without revealing them) and shielded transactions (committing to transfer amounts).
//!
//! Commitment scheme: C = v * B + r * B_blinding
//! where v is the value, r is the blinding factor, B is the value base point,
//! and B_blinding is the blinding base point.

use bulletproofs::PedersenGens;
use curve25519_dalek::ristretto::{CompressedRistretto, RistrettoPoint};
use curve25519_dalek::scalar::Scalar;
use rand::rngs::OsRng;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use gratia_core::GratiaError;

// ============================================================================
// Types
// ============================================================================

/// A Pedersen commitment to a single value.
///
/// The commitment is a compressed Ristretto point, which is 32 bytes.
/// It hides the committed value and blinding factor while being
/// computationally binding — the committer cannot open it to a different value.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PedersenCommitment {
    /// The compressed Ristretto point representing C = v*B + r*B_blinding.
    pub point: [u8; 32],
}

/// The opening data needed to verify a Pedersen commitment.
///
/// This is kept secret by the committer and revealed only during verification.
/// In the Gratia protocol, openings are used in ZK proofs — the verifier never
/// sees the raw opening, only a proof that it is consistent.
#[derive(Debug, Clone)]
pub struct PedersenOpening {
    /// The committed value.
    pub value: u64,
    /// The blinding factor (random scalar).
    pub blinding: Scalar,
}

// ============================================================================
// Core Functions
// ============================================================================

/// Commit to a value using a random blinding factor.
///
/// Returns both the commitment (safe to share) and the opening (must be kept secret).
/// The blinding factor is generated from a cryptographically secure RNG.
pub fn commit(value: u64) -> (PedersenCommitment, PedersenOpening) {
    let blinding = Scalar::random(&mut OsRng);
    commit_with_blinding(value, blinding)
}

/// Commit to a value using a specific blinding factor.
///
/// Useful when the blinding factor needs to be derived deterministically
/// (e.g., from a seed for reproducible proofs in testing) or when
/// constructing aggregate commitments.
pub fn commit_with_blinding(value: u64, blinding: Scalar) -> (PedersenCommitment, PedersenOpening) {
    let gens = PedersenGens::default();
    let value_scalar = Scalar::from(value);

    // C = v * B + r * B_blinding
    let point = gens.commit(value_scalar, blinding);
    let compressed = point.compress();

    let commitment = PedersenCommitment {
        point: compressed.to_bytes(),
    };

    let opening = PedersenOpening { value, blinding };

    (commitment, opening)
}

/// Verify that a commitment opens to the claimed value with the given blinding factor.
///
/// Returns Ok(()) if the commitment is valid, or an error if it does not match.
pub fn verify(commitment: &PedersenCommitment, opening: &PedersenOpening) -> Result<(), GratiaError> {
    let gens = PedersenGens::default();
    let value_scalar = Scalar::from(opening.value);

    // Recompute: C' = v * B + r * B_blinding
    let expected_point = gens.commit(value_scalar, opening.blinding);
    let expected_compressed = expected_point.compress();

    let committed_point = CompressedRistretto::from_slice(&commitment.point)
        .map_err(|_| GratiaError::InvalidZkProof {
            reason: "invalid commitment point encoding".into(),
        })?;

    if expected_compressed == committed_point {
        Ok(())
    } else {
        Err(GratiaError::InvalidZkProof {
            reason: "commitment verification failed: value/blinding mismatch".into(),
        })
    }
}

/// Compute the Ristretto point from a commitment's bytes.
///
/// Useful for performing arithmetic on commitments (e.g., checking that
/// input commitments sum to output commitments in a shielded transaction).
pub fn decompress(commitment: &PedersenCommitment) -> Result<RistrettoPoint, GratiaError> {
    CompressedRistretto::from_slice(&commitment.point)
        .map_err(|_| GratiaError::InvalidZkProof {
            reason: "invalid commitment point encoding".into(),
        })?
        .decompress()
        .ok_or_else(|| GratiaError::InvalidZkProof {
            reason: "commitment point decompression failed".into(),
        })
}

/// Verify that two commitments are additive: C_sum == C_a + C_b.
///
/// This is used in shielded transactions to verify that input amounts
/// equal output amounts without revealing any values.
/// Specifically: commit(a) + commit(b) == commit(a + b) when blinding factors
/// are handled correctly.
pub fn verify_additive(
    commitment_a: &PedersenCommitment,
    commitment_b: &PedersenCommitment,
    commitment_sum: &PedersenCommitment,
) -> Result<(), GratiaError> {
    let point_a = decompress(commitment_a)?;
    let point_b = decompress(commitment_b)?;
    let point_sum = decompress(commitment_sum)?;

    // WHY: Pedersen commitments are additively homomorphic.
    // commit(a, r_a) + commit(b, r_b) = commit(a+b, r_a+r_b)
    // So we can check value conservation without knowing the values.
    if point_a + point_b == point_sum {
        Ok(())
    } else {
        Err(GratiaError::InvalidZkProof {
            reason: "additive commitment verification failed".into(),
        })
    }
}

/// Derive a deterministic blinding factor from a seed.
///
/// Used when a blinding factor needs to be reproducible (e.g., for
/// wallet recovery where the user can re-derive their commitment openings
/// from a master seed).
pub fn blinding_from_seed(seed: &[u8], domain: &[u8]) -> Scalar {
    let mut hasher = Sha256::new();
    // WHY: Domain separation prevents cross-context reuse of blinding factors.
    // A blinding factor derived for a PoL attestation must not collide with
    // one derived for a shielded transaction.
    hasher.update(b"gratia-pedersen-blinding-v1:");
    hasher.update(domain);
    hasher.update(b":");
    hasher.update(seed);
    let hash = hasher.finalize();

    let mut bytes = [0u8; 32];
    bytes.copy_from_slice(&hash);
    Scalar::from_bytes_mod_order(bytes)
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_commit_and_verify() {
        let value = 42u64;
        let (commitment, opening) = commit(value);

        assert!(verify(&commitment, &opening).is_ok());
    }

    #[test]
    fn test_commit_wrong_value_fails() {
        let (commitment, mut opening) = commit(42);
        opening.value = 43;

        assert!(verify(&commitment, &opening).is_err());
    }

    #[test]
    fn test_commit_wrong_blinding_fails() {
        let (commitment, mut opening) = commit(42);
        opening.blinding = Scalar::random(&mut OsRng);

        assert!(verify(&commitment, &opening).is_err());
    }

    #[test]
    fn test_deterministic_blinding() {
        let seed = b"test-seed-12345";
        let domain = b"pol-attestation";

        let b1 = blinding_from_seed(seed, domain);
        let b2 = blinding_from_seed(seed, domain);
        assert_eq!(b1, b2);

        // Different domain produces different blinding
        let b3 = blinding_from_seed(seed, b"shielded-tx");
        assert_ne!(b1, b3);
    }

    #[test]
    fn test_commitment_with_specific_blinding() {
        let blinding = Scalar::from(999u64);
        let (commitment, opening) = commit_with_blinding(100, blinding);

        assert_eq!(opening.value, 100);
        assert_eq!(opening.blinding, blinding);
        assert!(verify(&commitment, &opening).is_ok());
    }

    #[test]
    fn test_additive_homomorphism() {
        let value_a = 100u64;
        let value_b = 200u64;
        let blinding_a = Scalar::random(&mut OsRng);
        let blinding_b = Scalar::random(&mut OsRng);

        let (commitment_a, _) = commit_with_blinding(value_a, blinding_a);
        let (commitment_b, _) = commit_with_blinding(value_b, blinding_b);

        // The sum commitment must use the sum of blinding factors
        let blinding_sum = blinding_a + blinding_b;
        let (commitment_sum, _) = commit_with_blinding(value_a + value_b, blinding_sum);

        assert!(verify_additive(&commitment_a, &commitment_b, &commitment_sum).is_ok());
    }

    #[test]
    fn test_additive_wrong_sum_fails() {
        let blinding_a = Scalar::random(&mut OsRng);
        let blinding_b = Scalar::random(&mut OsRng);

        let (commitment_a, _) = commit_with_blinding(100, blinding_a);
        let (commitment_b, _) = commit_with_blinding(200, blinding_b);

        // Wrong sum value
        let blinding_sum = blinding_a + blinding_b;
        let (commitment_wrong_sum, _) = commit_with_blinding(999, blinding_sum);

        assert!(verify_additive(&commitment_a, &commitment_b, &commitment_wrong_sum).is_err());
    }

    #[test]
    fn test_commitment_serialization() {
        let (commitment, _) = commit(42);
        assert_eq!(commitment.point.len(), 32);

        // Verify it can be deserialized back to a valid point
        assert!(decompress(&commitment).is_ok());
    }
}
