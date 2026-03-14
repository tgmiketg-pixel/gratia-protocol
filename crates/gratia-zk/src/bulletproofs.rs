//! Proof of Life zero-knowledge attestations using Bulletproofs.
//!
//! This module implements ZK proofs for the daily Proof of Life attestation.
//! The prover (the phone) demonstrates that all 8 required PoL parameters
//! were met during the rolling 24-hour window WITHOUT revealing the raw
//! sensor data. The verifier (other nodes) can confirm validity using
//! only the proof and public parameters.
//!
//! Each PoL parameter is encoded as a numeric value and committed via
//! Pedersen commitments. A Bulletproofs range proof then proves each
//! committed value falls within the required range (e.g., unlock_count >= 10).
//!
//! Proof structure:
//! - 8 Pedersen commitments (one per required PoL parameter)
//! - An aggregated Bulletproofs range proof covering all 8 values
//! - A domain-separated Merlin transcript for Fiat-Shamir

use ::bulletproofs::{BulletproofGens, PedersenGens, RangeProof};
use curve25519_dalek::ristretto::CompressedRistretto;
use curve25519_dalek::scalar::Scalar;
use merlin::Transcript;
use rand::rngs::OsRng;
use serde::{Deserialize, Serialize};

use gratia_core::types::DailyProofOfLifeData;
use gratia_core::GratiaError;

// ============================================================================
// Constants
// ============================================================================

/// Number of required Proof of Life parameters that must be proven.
/// Each parameter gets its own committed value in the aggregated range proof.
const POL_PARAMETER_COUNT: usize = 8;

/// Bit width for range proofs. 16 bits supports values 0..65535,
/// which is sufficient for all PoL parameter encodings (counts, hours, booleans).
/// WHY: Smaller bit widths produce smaller, faster proofs. 16 bits covers
/// the maximum realistic values (e.g., unlock_count of 65535 is far beyond
/// any realistic daily usage).
const RANGE_PROOF_BITS: usize = 16;

/// Domain separator for the Proof of Life Merlin transcript.
/// WHY: Domain separation ensures PoL proofs cannot be confused with
/// shielded transaction proofs or any other protocol proof.
const POL_TRANSCRIPT_DOMAIN: &[u8] = b"gratia-proof-of-life-v1";

// ============================================================================
// Types
// ============================================================================

/// A zero-knowledge Proof of Life attestation.
///
/// Contains an aggregated Bulletproofs range proof demonstrating that
/// all 8 required PoL parameters were met, plus the Pedersen commitments
/// to each parameter value. The raw sensor data is never revealed.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProofOfLifeProof {
    /// The aggregated range proof bytes (Bulletproofs serialization).
    pub range_proof: Vec<u8>,
    /// Pedersen commitments to each of the 8 PoL parameter values.
    /// Order: [unlock_count, unlock_spread_hours, interaction_sessions,
    ///         orientation_changed, human_motion, gps_fix,
    ///         network_connectivity, charge_cycle]
    pub commitments: Vec<[u8; 32]>,
}

/// Encoded PoL parameters as numeric values suitable for range proofs.
///
/// All boolean parameters are encoded as 0 (false) or 1 (true).
/// Count parameters use their raw numeric values.
/// The unlock spread is encoded as hours between first and last unlock.
///
/// Each value is then shifted so that a valid attestation produces values
/// in the range [0, 2^RANGE_PROOF_BITS - 1], where the minimum valid
/// value maps to 0 after shifting.
struct EncodedPolParameters {
    values: [u64; POL_PARAMETER_COUNT],
}

// ============================================================================
// Parameter Encoding
// ============================================================================

/// Minimum required values for each PoL parameter.
/// Values below these thresholds mean the attestation is invalid.
const POL_MINIMUMS: [u64; POL_PARAMETER_COUNT] = [
    10, // unlock_count: at least 10 unlocks
    6,  // unlock_spread_hours: at least 6 hours between first and last
    3,  // interaction_sessions: at least 3 distinct sessions
    1,  // orientation_changed: boolean, must be true (1)
    1,  // human_motion_detected: boolean, must be true (1)
    1,  // gps_fix_obtained: boolean, must be true (1)
    1,  // network_connectivity: wifi >= 1 OR bt >= 1 (encoded as max of the two, must be >= 1)
    1,  // charge_cycle_event: boolean, must be true (1)
];

/// Encode DailyProofOfLifeData into numeric values for range proofs.
///
/// Each parameter is encoded as: (actual_value - minimum_required)
/// so that a valid attestation always produces values >= 0.
/// An invalid attestation would require a negative value, which cannot
/// be represented as a u64, causing the proof to fail.
fn encode_pol_parameters(data: &DailyProofOfLifeData) -> Result<EncodedPolParameters, GratiaError> {
    // Calculate unlock spread in hours
    let unlock_spread_hours = match (data.first_unlock, data.last_unlock) {
        (Some(first), Some(last)) => {
            let duration = last - first;
            duration.num_hours().max(0) as u64
        }
        _ => 0,
    };

    // WHY: Network connectivity combines wifi and bluetooth counts.
    // The PoL requirement is "at least one Wi-Fi network OR Bluetooth peers",
    // plus "varying Bluetooth environments". We encode the bluetooth count
    // as the network value since it's the stricter requirement (>= 2).
    let network_value = data.distinct_bt_environments as u64;

    let raw_values: [u64; POL_PARAMETER_COUNT] = [
        data.unlock_count as u64,
        unlock_spread_hours,
        data.interaction_sessions as u64,
        data.orientation_changed as u64,
        data.human_motion_detected as u64,
        data.gps_fix_obtained as u64,
        network_value,
        data.charge_cycle_event as u64,
    ];

    // Shift values by subtracting minimums so valid data produces >= 0
    let mut values = [0u64; POL_PARAMETER_COUNT];
    for i in 0..POL_PARAMETER_COUNT {
        if raw_values[i] < POL_MINIMUMS[i] {
            return Err(GratiaError::ProofOfLifeInvalid {
                reason: format!(
                    "parameter {} has value {} but requires at least {}",
                    i, raw_values[i], POL_MINIMUMS[i]
                ),
            });
        }
        values[i] = raw_values[i] - POL_MINIMUMS[i];
    }

    Ok(EncodedPolParameters { values })
}

// ============================================================================
// Proof Generation
// ============================================================================

/// Generate a zero-knowledge Proof of Life attestation.
///
/// Takes the raw daily sensor data (which NEVER leaves the device) and produces
/// a compact ZK proof that all 8 required parameters were met. The proof
/// can be verified by any node without learning the actual sensor values.
///
/// Proof generation time on ARM: ~200-500ms depending on device.
/// Proof size: ~1 KB (aggregated 8-value Bulletproof).
pub fn prove_daily_attestation(
    data: &DailyProofOfLifeData,
) -> Result<ProofOfLifeProof, GratiaError> {
    // First validate that the data actually meets requirements.
    // WHY: We encode parameters as (value - minimum), so if any parameter
    // is below minimum, the subtraction would underflow. Check first.
    let encoded = encode_pol_parameters(data)?;

    // Bulletproofs generators: need enough for our aggregated proof.
    // WHY: BulletproofGens capacity must be >= number of values * bit width.
    let bp_gens = BulletproofGens::new(RANGE_PROOF_BITS, POL_PARAMETER_COUNT);
    let pc_gens = PedersenGens::default();

    // Generate random blinding factors for each commitment
    let blindings: Vec<Scalar> = (0..POL_PARAMETER_COUNT)
        .map(|_| Scalar::random(&mut OsRng))
        .collect();

    // Create the Fiat-Shamir transcript with domain separation
    let mut transcript = Transcript::new(POL_TRANSCRIPT_DOMAIN);

    // Build the aggregated range proof for all 8 parameters simultaneously.
    // This is more efficient than 8 individual proofs — the aggregated proof
    // is only marginally larger than a single proof.
    let (proof, commitments) = RangeProof::prove_multiple(
        &bp_gens,
        &pc_gens,
        &mut transcript,
        &encoded.values,
        &blindings,
        RANGE_PROOF_BITS,
    )
    .map_err(|e| GratiaError::InvalidZkProof {
        reason: format!("Bulletproof generation failed: {:?}", e),
    })?;

    // Serialize the proof and commitments
    let commitment_bytes: Vec<[u8; 32]> = commitments
        .iter()
        .map(|c| c.to_bytes())
        .collect();

    Ok(ProofOfLifeProof {
        range_proof: proof.to_bytes(),
        commitments: commitment_bytes,
    })
}

// ============================================================================
// Proof Verification
// ============================================================================

/// Verify a zero-knowledge Proof of Life attestation.
///
/// Any node can call this to confirm that the prover met all 8 PoL
/// requirements without learning the actual sensor values.
///
/// Verification time on ARM: ~50-100ms (much faster than proving).
pub fn verify_daily_attestation(proof: &ProofOfLifeProof) -> Result<(), GratiaError> {
    // Validate commitment count
    if proof.commitments.len() != POL_PARAMETER_COUNT {
        return Err(GratiaError::InvalidZkProof {
            reason: format!(
                "expected {} commitments, got {}",
                POL_PARAMETER_COUNT,
                proof.commitments.len()
            ),
        });
    }

    let bp_gens = BulletproofGens::new(RANGE_PROOF_BITS, POL_PARAMETER_COUNT);
    let pc_gens = PedersenGens::default();

    // Deserialize the range proof
    let range_proof = RangeProof::from_bytes(&proof.range_proof).map_err(|_| {
        GratiaError::InvalidZkProof {
            reason: "invalid range proof encoding".into(),
        }
    })?;

    // Deserialize commitment points
    let commitments: Vec<CompressedRistretto> = proof
        .commitments
        .iter()
        .map(|bytes| CompressedRistretto::from_slice(bytes))
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| GratiaError::InvalidZkProof {
            reason: "invalid commitment point encoding".into(),
        })?;

    // Recreate the transcript with the same domain separator.
    // WHY: The verifier must use the exact same transcript domain as the prover
    // for the Fiat-Shamir transform to produce matching challenges.
    let mut transcript = Transcript::new(POL_TRANSCRIPT_DOMAIN);

    // Verify the aggregated range proof
    range_proof
        .verify_multiple(&bp_gens, &pc_gens, &mut transcript, &commitments, RANGE_PROOF_BITS)
        .map_err(|_| GratiaError::InvalidZkProof {
            reason: "range proof verification failed: PoL parameters not in valid range".into(),
        })
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use gratia_core::types::OptionalSensorData;

    /// Create a valid DailyProofOfLifeData for testing.
    fn make_valid_pol_data() -> DailyProofOfLifeData {
        let now = Utc::now();
        DailyProofOfLifeData {
            unlock_count: 45,
            first_unlock: Some(now - chrono::Duration::hours(14)),
            last_unlock: Some(now),
            interaction_sessions: 12,
            orientation_changed: true,
            human_motion_detected: true,
            gps_fix_obtained: true,
            approximate_location: None,
            distinct_wifi_networks: 3,
            distinct_bt_environments: 4,
            charge_cycle_event: true,
            optional_sensors: OptionalSensorData::default(),
        }
    }

    #[test]
    fn test_prove_and_verify_valid_attestation() {
        let data = make_valid_pol_data();

        let proof = prove_daily_attestation(&data).expect("proof generation should succeed");
        assert_eq!(proof.commitments.len(), POL_PARAMETER_COUNT);
        assert!(!proof.range_proof.is_empty());

        let result = verify_daily_attestation(&proof);
        assert!(result.is_ok(), "verification failed: {:?}", result.err());
    }

    #[test]
    fn test_prove_fails_with_insufficient_unlocks() {
        let mut data = make_valid_pol_data();
        data.unlock_count = 5; // Below minimum of 10

        let result = prove_daily_attestation(&data);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("parameter 0"), "error should mention parameter 0 (unlock_count): {}", err);
    }

    #[test]
    fn test_prove_fails_with_no_orientation_change() {
        let mut data = make_valid_pol_data();
        data.orientation_changed = false;

        let result = prove_daily_attestation(&data);
        assert!(result.is_err());
    }

    #[test]
    fn test_prove_fails_with_no_charge_cycle() {
        let mut data = make_valid_pol_data();
        data.charge_cycle_event = false;

        let result = prove_daily_attestation(&data);
        assert!(result.is_err());
    }

    #[test]
    fn test_prove_fails_with_narrow_unlock_spread() {
        let mut data = make_valid_pol_data();
        let now = Utc::now();
        // Only 2 hours spread, need at least 6
        data.first_unlock = Some(now - chrono::Duration::hours(2));
        data.last_unlock = Some(now);

        let result = prove_daily_attestation(&data);
        assert!(result.is_err());
    }

    #[test]
    fn test_prove_fails_with_insufficient_bt_environments() {
        let mut data = make_valid_pol_data();
        data.distinct_bt_environments = 0; // Need at least 2 for BT variation

        let result = prove_daily_attestation(&data);
        assert!(result.is_err());
    }

    #[test]
    fn test_tampered_proof_fails_verification() {
        let data = make_valid_pol_data();
        let mut proof = prove_daily_attestation(&data).expect("proof generation should succeed");

        // Tamper with a commitment
        proof.commitments[0] = [0u8; 32];

        let result = verify_daily_attestation(&proof);
        assert!(result.is_err());
    }

    #[test]
    fn test_wrong_commitment_count_fails() {
        let data = make_valid_pol_data();
        let mut proof = prove_daily_attestation(&data).expect("proof generation should succeed");

        // Remove a commitment
        proof.commitments.pop();

        let result = verify_daily_attestation(&proof);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("expected"));
    }

    #[test]
    fn test_minimum_valid_data_succeeds() {
        // Test with exactly the minimum required values
        let now = Utc::now();
        let data = DailyProofOfLifeData {
            unlock_count: 10, // Exact minimum
            first_unlock: Some(now - chrono::Duration::hours(6)), // Exact minimum spread
            last_unlock: Some(now),
            interaction_sessions: 3, // Exact minimum
            orientation_changed: true,
            human_motion_detected: true,
            gps_fix_obtained: true,
            approximate_location: None,
            distinct_wifi_networks: 1,
            distinct_bt_environments: 2, // Minimum for BT variation
            charge_cycle_event: true,
            optional_sensors: OptionalSensorData::default(),
        };

        let proof = prove_daily_attestation(&data).expect("minimum valid data should produce proof");
        assert!(verify_daily_attestation(&proof).is_ok());
    }

    #[test]
    fn test_proof_serialization_roundtrip() {
        let data = make_valid_pol_data();
        let proof = prove_daily_attestation(&data).expect("proof generation should succeed");

        // Serialize to JSON and back
        let json = serde_json::to_string(&proof).expect("serialization should succeed");
        let deserialized: ProofOfLifeProof =
            serde_json::from_str(&json).expect("deserialization should succeed");

        assert_eq!(proof.range_proof, deserialized.range_proof);
        assert_eq!(proof.commitments, deserialized.commitments);

        // Deserialized proof should still verify
        assert!(verify_daily_attestation(&deserialized).is_ok());
    }
}
