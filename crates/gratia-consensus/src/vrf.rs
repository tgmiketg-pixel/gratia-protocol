//! ECVRF (Verifiable Random Function) for block producer selection.
//!
//! Simplified ECVRF using Schnorr-like proofs on Ristretto points (curve25519-dalek).
//! The VRF guarantees that:
//! 1. The output is deterministic given a secret key and input.
//! 2. The output is pseudorandom (indistinguishable from random without the secret key).
//! 3. Anyone with the public key can verify the proof without learning the secret key.
//!
//! This is a PoC implementation suitable for testnet. A full RFC 9381 implementation
//! would be used for mainnet after security audit.

use curve25519_dalek::{
    constants::RISTRETTO_BASEPOINT_POINT,
    ristretto::{CompressedRistretto, RistrettoPoint},
    scalar::Scalar,
};
use sha2::{Sha512, Digest};
use serde::{Deserialize, Serialize};
use rand::RngCore;

use gratia_core::error::GratiaError;

// ============================================================================
// Constants
// ============================================================================

/// Domain separator for VRF hash-to-point.
/// WHY: Prevents cross-protocol attacks where a proof from another system
/// could be replayed against Gratia's VRF.
const VRF_HASH_TO_POINT_DOMAIN: &[u8] = b"gratia-vrf-h2c-v1";

/// Domain separator for the Schnorr challenge hash.
const VRF_CHALLENGE_DOMAIN: &[u8] = b"gratia-vrf-challenge-v1";

/// Domain separator for deriving the VRF output from the proof.
const VRF_OUTPUT_DOMAIN: &[u8] = b"gratia-vrf-output-v1";

/// Size of a serialized VRF proof in bytes.
/// 32 (Gamma compressed) + 32 (challenge scalar) + 32 (response scalar) = 96 bytes.
pub const VRF_PROOF_SIZE: usize = 96;

// ============================================================================
// Types
// ============================================================================

/// A VRF proof that a particular output was correctly derived from
/// a secret key and input. The proof can be verified by anyone who
/// knows the corresponding public key.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VrfProof {
    /// The VRF output hash (32 bytes). This is the pseudorandom value
    /// used for block producer selection.
    pub output: [u8; 32],
    /// The serialized proof bytes (96 bytes):
    /// [0..32]  = Gamma point (compressed Ristretto)
    /// [32..64] = challenge scalar (little-endian)
    /// [64..96] = response scalar (little-endian)
    pub proof_bytes: Vec<u8>,
}

/// A VRF secret key (a scalar on the Ristretto group).
#[derive(Clone)]
pub struct VrfSecretKey {
    scalar: Scalar,
}

/// A VRF public key (a Ristretto point, the scalar * basepoint).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct VrfPublicKey {
    /// Compressed Ristretto point bytes.
    pub bytes: [u8; 32],
}

// ============================================================================
// Key Generation
// ============================================================================

impl VrfSecretKey {
    /// Generate a new random VRF secret key.
    pub fn generate<R: RngCore + rand::CryptoRng>(rng: &mut R) -> Self {
        let mut key_bytes = [0u8; 64];
        rng.fill_bytes(&mut key_bytes);
        // WHY: Using from_bytes_mod_order_wide to ensure uniform distribution
        // over the scalar field, avoiding modular bias.
        let scalar = Scalar::from_bytes_mod_order_wide(&key_bytes);
        VrfSecretKey { scalar }
    }

    /// Derive a VRF secret key from Ed25519 signing key bytes.
    /// WHY: Allows nodes to use their existing Ed25519 identity key
    /// to derive a VRF key, avoiding the need for separate key management.
    pub fn from_ed25519_bytes(signing_key_bytes: &[u8; 32]) -> Self {
        let mut hasher = Sha512::new();
        hasher.update(b"gratia-vrf-keygen-v1:");
        hasher.update(signing_key_bytes);
        let hash = hasher.finalize();
        let mut wide = [0u8; 64];
        wide.copy_from_slice(&hash);
        let scalar = Scalar::from_bytes_mod_order_wide(&wide);
        VrfSecretKey { scalar }
    }

    /// Get the corresponding public key.
    pub fn public_key(&self) -> VrfPublicKey {
        let point = self.scalar * RISTRETTO_BASEPOINT_POINT;
        VrfPublicKey {
            bytes: point.compress().to_bytes(),
        }
    }
}

impl VrfPublicKey {
    /// Decompress the public key to a Ristretto point.
    fn to_point(&self) -> Result<RistrettoPoint, GratiaError> {
        CompressedRistretto::from_slice(&self.bytes)
            .map_err(|_| GratiaError::InvalidVrfProof)?
            .decompress()
            .ok_or(GratiaError::InvalidVrfProof)
    }
}

// ============================================================================
// VRF Operations
// ============================================================================

/// Hash an arbitrary input to a Ristretto point.
/// Uses the try-and-increment method with domain separation.
fn hash_to_point(input: &[u8]) -> RistrettoPoint {
    // WHY: Using hash-to-point via SHA-512 + from_uniform_bytes ensures
    // the resulting point is uniformly distributed on the Ristretto group,
    // which is required for VRF security.
    let mut hasher = Sha512::new();
    hasher.update(VRF_HASH_TO_POINT_DOMAIN);
    hasher.update(input);
    let hash = hasher.finalize();
    let mut wide = [0u8; 64];
    wide.copy_from_slice(&hash);
    RistrettoPoint::from_uniform_bytes(&wide)
}

/// Compute the Schnorr-like challenge scalar.
fn compute_challenge(
    public_key: &RistrettoPoint,
    h: &RistrettoPoint,
    gamma: &RistrettoPoint,
    u: &RistrettoPoint,
    v: &RistrettoPoint,
) -> Scalar {
    let mut hasher = Sha512::new();
    hasher.update(VRF_CHALLENGE_DOMAIN);
    hasher.update(public_key.compress().as_bytes());
    hasher.update(h.compress().as_bytes());
    hasher.update(gamma.compress().as_bytes());
    hasher.update(u.compress().as_bytes());
    hasher.update(v.compress().as_bytes());
    let hash = hasher.finalize();
    let mut wide = [0u8; 64];
    wide.copy_from_slice(&hash);
    Scalar::from_bytes_mod_order_wide(&wide)
}

/// Derive the VRF output hash from the Gamma point.
fn gamma_to_output(gamma: &RistrettoPoint) -> [u8; 32] {
    // WHY: The output is derived by hashing the Gamma point with domain
    // separation. This ensures the output is uniformly distributed even
    // if the Gamma point has structure.
    let mut hasher = Sha512::new();
    hasher.update(VRF_OUTPUT_DOMAIN);
    hasher.update(gamma.compress().as_bytes());
    let hash = hasher.finalize();
    let mut output = [0u8; 32];
    output.copy_from_slice(&hash[..32]);
    output
}

/// Generate a VRF proof for the given input.
///
/// The input should be `previous_block_hash || slot_number` to ensure
/// each slot has a unique, unpredictable selection value.
pub fn generate_vrf_proof(
    secret_key: &VrfSecretKey,
    input: &[u8],
) -> VrfProof {
    let public_key_point = secret_key.scalar * RISTRETTO_BASEPOINT_POINT;

    // H = hash_to_point(input)
    let h = hash_to_point(input);

    // Gamma = secret_key * H  (the VRF output point)
    let gamma = secret_key.scalar * h;

    // Generate nonce deterministically from secret key and input.
    // WHY: Deterministic nonce prevents nonce reuse attacks that would
    // leak the secret key (analogous to RFC 6979 for ECDSA).
    let k = {
        let mut hasher = Sha512::new();
        hasher.update(b"gratia-vrf-nonce-v1:");
        hasher.update(secret_key.scalar.as_bytes());
        hasher.update(input);
        let hash = hasher.finalize();
        let mut wide = [0u8; 64];
        wide.copy_from_slice(&hash);
        Scalar::from_bytes_mod_order_wide(&wide)
    };

    // U = k * G  (Schnorr commitment on basepoint)
    let u = k * RISTRETTO_BASEPOINT_POINT;
    // V = k * H  (Schnorr commitment on hash point)
    let v = k * h;

    // Challenge
    let c = compute_challenge(&public_key_point, &h, &gamma, &u, &v);

    // Response: s = k - c * secret_key
    let s = k - c * secret_key.scalar;

    // Output
    let output = gamma_to_output(&gamma);

    // Serialize proof: Gamma || c || s
    let mut proof_bytes = Vec::with_capacity(VRF_PROOF_SIZE);
    proof_bytes.extend_from_slice(gamma.compress().as_bytes());
    proof_bytes.extend_from_slice(c.as_bytes());
    proof_bytes.extend_from_slice(s.as_bytes());

    VrfProof {
        output,
        proof_bytes,
    }
}

/// Verify a VRF proof against a public key and input.
///
/// Returns the VRF output if the proof is valid, or an error otherwise.
pub fn verify_vrf_proof(
    public_key: &VrfPublicKey,
    input: &[u8],
    proof: &VrfProof,
) -> Result<[u8; 32], GratiaError> {
    if proof.proof_bytes.len() != VRF_PROOF_SIZE {
        return Err(GratiaError::InvalidVrfProof);
    }

    // Deserialize proof components
    let gamma = CompressedRistretto::from_slice(&proof.proof_bytes[0..32])
        .map_err(|_| GratiaError::InvalidVrfProof)?
        .decompress()
        .ok_or(GratiaError::InvalidVrfProof)?;

    let c: Scalar = {
        let mut bytes = [0u8; 32];
        bytes.copy_from_slice(&proof.proof_bytes[32..64]);
        // WHY: from_canonical_bytes ensures the scalar is in the valid range,
        // preventing malleability attacks with non-canonical encodings.
        let opt: Option<Scalar> = Scalar::from_canonical_bytes(bytes).into();
        opt.ok_or(GratiaError::InvalidVrfProof)?
    };

    let s: Scalar = {
        let mut bytes = [0u8; 32];
        bytes.copy_from_slice(&proof.proof_bytes[64..96]);
        let opt: Option<Scalar> = Scalar::from_canonical_bytes(bytes).into();
        opt.ok_or(GratiaError::InvalidVrfProof)?
    };

    let public_key_point = public_key.to_point()?;
    let h = hash_to_point(input);

    // Recompute U = s * G + c * public_key
    let u = s * RISTRETTO_BASEPOINT_POINT + c * public_key_point;
    // Recompute V = s * H + c * Gamma
    let v = s * h + c * gamma;

    // Recompute challenge
    let c_prime = compute_challenge(&public_key_point, &h, &gamma, &u, &v);

    // Verify challenge matches
    if c != c_prime {
        return Err(GratiaError::InvalidVrfProof);
    }

    // Verify the output matches
    let expected_output = gamma_to_output(&gamma);
    if proof.output != expected_output {
        return Err(GratiaError::InvalidVrfProof);
    }

    Ok(expected_output)
}

/// Convert a VRF output to a selection value weighted by Presence Score.
///
/// Returns a value in [0.0, 1.0) where higher Presence Scores shift
/// the distribution toward lower values, making those nodes more likely
/// to be selected as block producers.
///
/// The weighting formula: selection = vrf_uniform * (100 / presence_score)
/// This means a node with score 100 gets the raw VRF value, while a node
/// with score 40 (minimum) gets its value multiplied by 2.5x, making it
/// less likely to have the lowest selection value.
/// Integer-only committee selection value using u64 arithmetic.
///
/// SECURITY: Replaces the previous f64 version to ensure deterministic ordering
/// across ARM and x86 platforms. Floating-point rounding differences between
/// architectures can cause nodes to disagree on committee membership.
///
/// The weighting formula: selection = raw_hash / score
/// Higher presence_score → lower selection value → higher priority (selected first).
/// A score-100 node gets raw/100, a score-40 node gets raw/40 (2.5x larger).
pub fn vrf_output_to_selection(vrf_output: &[u8; 32], presence_score: u8) -> u64 {
    // WHY: Clamping to [40, 100] matches the protocol's Presence Score range.
    // Scores below 40 should never occur (they fail the consensus threshold).
    let score = presence_score.max(40).min(100) as u64;

    // Convert the first 8 bytes of VRF output to a u64 hash value.
    let mut bytes = [0u8; 8];
    bytes.copy_from_slice(&vrf_output[..8]);
    let raw = u64::from_le_bytes(bytes);

    // WHY: Integer division is deterministic across all platforms.
    // Higher score → smaller result → higher selection priority.
    // score is always >= 40, so division by zero is impossible.
    raw / score
}

/// Build the VRF input for a given slot.
/// Input = previous_block_hash || slot_number (big-endian).
pub fn build_vrf_input(previous_block_hash: &[u8; 32], slot_number: u64) -> Vec<u8> {
    let mut input = Vec::with_capacity(40);
    input.extend_from_slice(previous_block_hash);
    input.extend_from_slice(&slot_number.to_be_bytes());
    input
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use rand::rngs::OsRng;

    #[test]
    fn test_keygen_and_proof_roundtrip() {
        let sk = VrfSecretKey::generate(&mut OsRng);
        let pk = sk.public_key();
        let input = b"test-input-block-hash-and-slot";

        let proof = generate_vrf_proof(&sk, input);
        let result = verify_vrf_proof(&pk, input, &proof);
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), proof.output);
    }

    #[test]
    fn test_deterministic_output() {
        let sk = VrfSecretKey::generate(&mut OsRng);
        let input = b"same-input";

        let proof1 = generate_vrf_proof(&sk, input);
        let proof2 = generate_vrf_proof(&sk, input);

        // Same key + same input = same output (deterministic)
        assert_eq!(proof1.output, proof2.output);
        // Proofs should also be identical (deterministic nonce)
        assert_eq!(proof1.proof_bytes, proof2.proof_bytes);
    }

    #[test]
    fn test_different_keys_different_outputs() {
        let sk1 = VrfSecretKey::generate(&mut OsRng);
        let sk2 = VrfSecretKey::generate(&mut OsRng);
        let input = b"same-input";

        let proof1 = generate_vrf_proof(&sk1, input);
        let proof2 = generate_vrf_proof(&sk2, input);

        // Different keys should produce different outputs
        assert_ne!(proof1.output, proof2.output);
    }

    #[test]
    fn test_different_inputs_different_outputs() {
        let sk = VrfSecretKey::generate(&mut OsRng);

        let proof1 = generate_vrf_proof(&sk, b"input-1");
        let proof2 = generate_vrf_proof(&sk, b"input-2");

        assert_ne!(proof1.output, proof2.output);
    }

    #[test]
    fn test_wrong_public_key_fails() {
        let sk1 = VrfSecretKey::generate(&mut OsRng);
        let sk2 = VrfSecretKey::generate(&mut OsRng);
        let pk2 = sk2.public_key();
        let input = b"test-input";

        let proof = generate_vrf_proof(&sk1, input);
        let result = verify_vrf_proof(&pk2, input, &proof);
        assert!(result.is_err());
    }

    #[test]
    fn test_wrong_input_fails() {
        let sk = VrfSecretKey::generate(&mut OsRng);
        let pk = sk.public_key();

        let proof = generate_vrf_proof(&sk, b"correct-input");
        let result = verify_vrf_proof(&pk, b"wrong-input", &proof);
        assert!(result.is_err());
    }

    #[test]
    fn test_tampered_proof_fails() {
        let sk = VrfSecretKey::generate(&mut OsRng);
        let pk = sk.public_key();
        let input = b"test-input";

        let mut proof = generate_vrf_proof(&sk, input);
        // Tamper with a byte in the proof
        proof.proof_bytes[50] ^= 0xFF;
        let result = verify_vrf_proof(&pk, input, &proof);
        assert!(result.is_err());
    }

    #[test]
    fn test_tampered_output_fails() {
        let sk = VrfSecretKey::generate(&mut OsRng);
        let pk = sk.public_key();
        let input = b"test-input";

        let mut proof = generate_vrf_proof(&sk, input);
        // Tamper with the output
        proof.output[0] ^= 0xFF;
        let result = verify_vrf_proof(&pk, input, &proof);
        assert!(result.is_err());
    }

    #[test]
    fn test_vrf_output_to_selection_weighting() {
        // A fixed VRF output for deterministic testing
        let output = [0x80; 32]; // Roughly mid-range

        let selection_100 = vrf_output_to_selection(&output, 100);
        let selection_40 = vrf_output_to_selection(&output, 40);

        // Score 100 should produce a lower (better) selection value than score 40
        assert!(selection_100 < selection_40,
            "Score 100 ({}) should select lower than score 40 ({})",
            selection_100, selection_40);
    }

    #[test]
    fn test_vrf_output_to_selection_bounds() {
        let zero_output = [0u8; 32];
        let selection = vrf_output_to_selection(&zero_output, 100);
        // Zero input → zero selection value
        assert_eq!(selection, 0);

        let max_output = [0xFF; 32];
        let selection = vrf_output_to_selection(&max_output, 40);
        // u64::MAX / 40 — should be a large but valid u64
        assert!(selection > 0);
        assert_eq!(selection, u64::MAX / 40);
    }

    #[test]
    fn test_vrf_output_to_selection_deterministic() {
        // WHY: Integer arithmetic is deterministic across ARM and x86.
        // Verify the same inputs always produce the same output.
        let output = [0u8; 32];
        // Score 0 gets clamped to 40
        let selection = vrf_output_to_selection(&output, 0);
        let selection2 = vrf_output_to_selection(&output, 0);
        assert_eq!(selection, selection2);

        let max_output = [0xFF; 32];
        let selection = vrf_output_to_selection(&max_output, 40);
        let selection2 = vrf_output_to_selection(&max_output, 40);
        assert_eq!(selection, selection2);
    }

    #[test]
    fn test_build_vrf_input() {
        let hash = [0xABu8; 32];
        let slot = 42u64;
        let input = build_vrf_input(&hash, slot);
        assert_eq!(input.len(), 40);
        assert_eq!(&input[..32], &hash);
        assert_eq!(&input[32..], &slot.to_be_bytes());
    }

    #[test]
    fn test_from_ed25519_bytes() {
        let ed_key = [0x42u8; 32];
        let sk = VrfSecretKey::from_ed25519_bytes(&ed_key);
        let pk = sk.public_key();

        // Should produce a valid key that can generate and verify proofs
        let proof = generate_vrf_proof(&sk, b"test");
        assert!(verify_vrf_proof(&pk, b"test", &proof).is_ok());

        // Deterministic derivation
        let sk2 = VrfSecretKey::from_ed25519_bytes(&ed_key);
        let proof2 = generate_vrf_proof(&sk2, b"test");
        assert_eq!(proof.output, proof2.output);
    }

    #[test]
    fn test_proof_size() {
        let sk = VrfSecretKey::generate(&mut OsRng);
        let proof = generate_vrf_proof(&sk, b"test");
        assert_eq!(proof.proof_bytes.len(), VRF_PROOF_SIZE);
    }
}
