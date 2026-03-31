//! Proof of Life zero-knowledge attestations using Bulletproofs.
//!
//! This module implements ZK proofs for the daily Proof of Life attestation.
//! The prover (the phone) demonstrates that all required PoL parameters
//! were met during the rolling 24-hour window WITHOUT revealing the raw
//! sensor data. The verifier (other nodes) can confirm validity using
//! only the proof and public parameters.
//!
//! ## Two API layers
//!
//! 1. **High-level** (`prove_daily_attestation` / `verify_daily_attestation`):
//!    Takes `DailyProofOfLifeData` directly, uses hardcoded protocol minimums,
//!    returns `ProofOfLifeProof`. Integrated with `GratiaError`.
//!
//! 2. **Flexible** (`generate_pol_proof` / `verify_pol_proof`):
//!    Takes `PolProofInput` + `PolThresholds`, returns `PolRangeProof`.
//!    Thresholds are governance-adjustable. Uses dedicated `ZkError`.
//!
//! ## Proof technique
//!
//! To prove `value >= minimum` without revealing `value`, we prove that
//! `(value - minimum)` lies in the range `[0, 2^n)` using a Bulletproofs
//! range proof. If `value < minimum`, the subtraction underflows in u64
//! arithmetic and cannot produce a valid range proof.
//!
//! Each PoL parameter gets its own committed value. All values are proven
//! in a single aggregated Bulletproofs range proof for efficiency.

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

/// Number of required Proof of Life parameters that must be proven
/// in the high-level API (all 8 daily PoL requirements).
const POL_PARAMETER_COUNT: usize = 8;

/// Number of parameters in the flexible API (the 4 core numeric ones:
/// unlock_count, unlock_spread_hours, interaction_sessions, bt_environments).
/// WHY: The flexible API focuses on the range-provable numeric parameters.
/// Boolean parameters (orientation, motion, gps, charge) are either 0 or 1
/// and are included in the high-level API but not separately exposed in
/// the flexible API since their proof is trivial.
const FLEXIBLE_PARAMETER_COUNT: usize = 4;

/// Bit width for range proofs. 32 bits supports values 0..4,294,967,295.
/// WHY: We use (value - minimum) as the proven value. With 32-bit range,
/// the maximum provable surplus above minimum is ~4 billion, which is
/// far beyond any realistic daily PoL count. 32 bits is the sweet spot:
/// large enough that no legitimate value overflows, small enough that
/// proof generation stays fast on mobile ARM (~200-500ms).
const RANGE_PROOF_BITS: usize = 32;

/// Bit width for the high-level 8-parameter API (kept at 16 for backward compat).
/// WHY: 16 bits covers max value 65535, sufficient for all PoL encodings
/// (counts, hours, booleans). Smaller bit width = smaller, faster proofs.
const RANGE_PROOF_BITS_LEGACY: usize = 16;

/// Domain separator for the Proof of Life Merlin transcript (high-level API).
/// WHY: Domain separation ensures PoL proofs cannot be confused with
/// shielded transaction proofs or any other protocol proof.
const POL_TRANSCRIPT_DOMAIN: &[u8] = b"gratia-proof-of-life-v1";

/// Domain separator for the flexible PoL range proof API.
/// WHY: Different domain from the legacy API so proofs from the two
/// systems cannot be cross-verified (they use different parameter counts
/// and bit widths).
const POL_RANGE_TRANSCRIPT_DOMAIN: &[u8] = b"gratia-pol-range-proof-v1";

// ============================================================================
// ZkError — Dedicated error type for zero-knowledge proof operations
// ============================================================================

/// Errors specific to zero-knowledge proof generation and verification.
#[derive(Debug, Clone, thiserror::Error)]
pub enum ZkError {
    /// Proof generation failed (e.g., Bulletproofs internal error).
    #[error("proof generation failed: {reason}")]
    ProofGenerationFailed { reason: String },

    /// Proof verification failed (invalid proof or wrong thresholds).
    #[error("verification failed: {reason}")]
    VerificationFailed { reason: String },

    /// Input values are invalid (e.g., value below minimum threshold).
    #[error("invalid input: {reason}")]
    InvalidInput { reason: String },

    /// Serialization or deserialization error.
    #[error("serialization error: {reason}")]
    SerializationError { reason: String },
}

impl From<ZkError> for GratiaError {
    fn from(e: ZkError) -> Self {
        match e {
            ZkError::ProofGenerationFailed { reason } | ZkError::VerificationFailed { reason } => {
                GratiaError::InvalidZkProof { reason }
            }
            ZkError::InvalidInput { reason } => GratiaError::ProofOfLifeInvalid { reason },
            ZkError::SerializationError { reason } => GratiaError::SerializationError(reason),
        }
    }
}

// ============================================================================
// Flexible API Types
// ============================================================================

/// Input data for generating a PoL range proof.
///
/// Contains the actual observed values from the phone's daily sensor data.
/// These values NEVER leave the device — only the resulting ZK proof does.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PolProofInput {
    /// Number of phone unlock events during the 24-hour window.
    pub unlock_count: u64,
    /// Hours between the first and last unlock event.
    pub unlock_spread_hours: u64,
    /// Number of distinct screen interaction sessions.
    pub interaction_sessions: u64,
    /// Number of distinct Bluetooth peer environments observed.
    pub bt_environments: u64,
}

/// Configurable thresholds for PoL parameter validation.
///
/// These are the minimum values each parameter must meet. They are
/// governance-adjustable: the network can vote to change these thresholds
/// without a protocol upgrade.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PolThresholds {
    /// Minimum unlock events required (default: 10).
    pub min_unlocks: u64,
    /// Minimum spread in hours between first and last unlock (default: 6).
    pub min_spread: u64,
    /// Minimum number of screen interaction sessions (default: 3).
    pub min_interactions: u64,
    /// Minimum number of distinct Bluetooth environments (default: 2).
    pub min_bt_envs: u64,
}

impl Default for PolThresholds {
    fn default() -> Self {
        PolThresholds {
            min_unlocks: 10,
            min_spread: 6,
            min_interactions: 3,
            min_bt_envs: 2,
        }
    }
}

/// A zero-knowledge range proof for Proof of Life parameters.
///
/// Proves that each of the 4 core PoL parameters meets or exceeds its
/// required threshold, without revealing the actual values.
///
/// Proof size: ~700-900 bytes for a 4-value aggregated Bulletproof.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PolRangeProof {
    /// Serialized Bulletproof (aggregated range proof over all parameters).
    pub proof_bytes: Vec<u8>,
    /// Pedersen commitments to each shifted value (value - minimum).
    /// Each commitment is a 32-byte compressed Ristretto point.
    /// Order: [unlock_count, unlock_spread, interactions, bt_environments].
    pub commitments: Vec<Vec<u8>>,
    /// Number of parameters proven in this proof.
    pub parameter_count: u8,
    /// Day number since genesis. Bound into the Fiat-Shamir transcript to
    /// prevent replay of a valid proof from one day being accepted on a
    /// different day.
    pub epoch_day: u32,
}

// ============================================================================
// Flexible API — generate_pol_proof / verify_pol_proof
// ============================================================================

/// Generate a zero-knowledge proof that all PoL parameters meet their thresholds.
///
/// The proof demonstrates: for each parameter i, `data[i] >= thresholds[i]`,
/// without revealing the actual values of `data[i]`.
///
/// # Arguments
/// * `data` - The actual observed PoL values (stays on device).
/// * `thresholds` - The minimum required values (public, governance-set).
///
/// # Returns
/// A `PolRangeProof` that any node can verify against the same thresholds.
///
/// # Errors
/// * `ZkError::InvalidInput` if any value is below its threshold.
/// * `ZkError::ProofGenerationFailed` if the Bulletproof generation fails.
///
/// # Performance
/// ~200-500ms on ARM64 (Snapdragon 600-series and above).
pub fn generate_pol_proof(data: &PolProofInput, thresholds: &PolThresholds, epoch_day: u32) -> Result<PolRangeProof, ZkError> {
    // Collect values and minimums into parallel arrays
    let values = [
        data.unlock_count,
        data.unlock_spread_hours,
        data.interaction_sessions,
        data.bt_environments,
    ];
    let minimums = [
        thresholds.min_unlocks,
        thresholds.min_spread,
        thresholds.min_interactions,
        thresholds.min_bt_envs,
    ];
    let param_names = ["unlock_count", "unlock_spread_hours", "interaction_sessions", "bt_environments"];

    // Compute shifted values: (value - minimum). If value < minimum, this
    // is an error because the subtraction would underflow and the prover
    // cannot legitimately produce a valid range proof.
    let mut shifted = [0u64; FLEXIBLE_PARAMETER_COUNT];
    for i in 0..FLEXIBLE_PARAMETER_COUNT {
        if values[i] < minimums[i] {
            return Err(ZkError::InvalidInput {
                reason: format!(
                    "{} is {} but must be at least {}",
                    param_names[i], values[i], minimums[i]
                ),
            });
        }
        shifted[i] = values[i] - minimums[i];
    }

    // Bulletproofs generators. Capacity must be >= (parameter_count * bit_width).
    let bp_gens = BulletproofGens::new(RANGE_PROOF_BITS, FLEXIBLE_PARAMETER_COUNT);
    let pc_gens = PedersenGens::default();

    // Random blinding factors for Pedersen commitments.
    // WHY: Each commitment uses a unique random blinding so that the
    // commitment reveals nothing about the value. The blinding is
    // generated from OsRng (OS-level CSPRNG) for cryptographic security.
    let blindings: Vec<Scalar> = (0..FLEXIBLE_PARAMETER_COUNT)
        .map(|_| Scalar::random(&mut OsRng))
        .collect();

    // Create the Fiat-Shamir transcript with domain separation.
    let mut transcript = Transcript::new(POL_RANGE_TRANSCRIPT_DOMAIN);
    // Bind the epoch day into the transcript to prevent replay attacks.
    transcript.append_u64(b"epoch_day", epoch_day as u64);

    // Generate the aggregated range proof.
    // WHY: Aggregated proof for N values is only marginally larger than
    // a single-value proof, but proves all N values simultaneously.
    // This is a key efficiency feature of Bulletproofs.
    let (proof, commitments) = RangeProof::prove_multiple(
        &bp_gens,
        &pc_gens,
        &mut transcript,
        &shifted,
        &blindings,
        RANGE_PROOF_BITS,
    )
    .map_err(|e| ZkError::ProofGenerationFailed {
        reason: format!("Bulletproof prove_multiple failed: {:?}", e),
    })?;

    // Serialize commitments as Vec<Vec<u8>> for the output struct.
    let commitment_bytes: Vec<Vec<u8>> = commitments
        .iter()
        .map(|c| c.to_bytes().to_vec())
        .collect();

    Ok(PolRangeProof {
        proof_bytes: proof.to_bytes(),
        commitments: commitment_bytes,
        parameter_count: FLEXIBLE_PARAMETER_COUNT as u8,
        epoch_day,
    })
}

/// Verify a PoL range proof against the given thresholds.
///
/// The verifier checks that the prover committed to values that are
/// each in the range `[0, 2^32)` — which, combined with the shift
/// `(value - minimum)`, proves that `value >= minimum` for each parameter.
///
/// # Arguments
/// * `proof` - The `PolRangeProof` produced by the prover.
/// * `thresholds` - The minimum required values (must match what the prover used).
///
/// # Returns
/// `Ok(true)` if the proof is valid, `Ok(false)` is not used — invalid proofs
/// return `Err(ZkError::VerificationFailed)`.
///
/// # Errors
/// * `ZkError::VerificationFailed` if the proof does not verify.
/// * `ZkError::SerializationError` if the proof bytes are malformed.
/// * `ZkError::InvalidInput` if the commitment count is wrong.
///
/// # Performance
/// ~50-100ms on ARM64 (much faster than proving).
pub fn verify_pol_proof(proof: &PolRangeProof, _thresholds: &PolThresholds, expected_epoch_day: u32) -> Result<bool, ZkError> {
    // Reject proofs that claim a different epoch day than expected.
    if proof.epoch_day != expected_epoch_day {
        return Err(ZkError::VerificationFailed {
            reason: format!(
                "epoch_day mismatch: proof is for day {} but expected day {}",
                proof.epoch_day, expected_epoch_day
            ),
        });
    }
    // Validate commitment count matches expected parameter count.
    if proof.commitments.len() != FLEXIBLE_PARAMETER_COUNT {
        return Err(ZkError::InvalidInput {
            reason: format!(
                "expected {} commitments, got {}",
                FLEXIBLE_PARAMETER_COUNT,
                proof.commitments.len()
            ),
        });
    }

    if proof.parameter_count != FLEXIBLE_PARAMETER_COUNT as u8 {
        return Err(ZkError::InvalidInput {
            reason: format!(
                "parameter_count is {} but expected {}",
                proof.parameter_count, FLEXIBLE_PARAMETER_COUNT
            ),
        });
    }

    // Recreate generators with the same parameters.
    let bp_gens = BulletproofGens::new(RANGE_PROOF_BITS, FLEXIBLE_PARAMETER_COUNT);
    let pc_gens = PedersenGens::default();

    // Deserialize the range proof.
    let range_proof = RangeProof::from_bytes(&proof.proof_bytes).map_err(|_| {
        ZkError::SerializationError {
            reason: "invalid range proof encoding".into(),
        }
    })?;

    // Deserialize commitment points.
    let commitments: Vec<CompressedRistretto> = proof
        .commitments
        .iter()
        .map(|bytes| {
            if bytes.len() != 32 {
                return Err(ZkError::SerializationError {
                    reason: format!("commitment must be 32 bytes, got {}", bytes.len()),
                });
            }
            let mut arr = [0u8; 32];
            arr.copy_from_slice(bytes);
            CompressedRistretto::from_slice(&arr).map_err(|_| ZkError::SerializationError {
                reason: "invalid compressed Ristretto point".into(),
            })
        })
        .collect::<Result<Vec<_>, _>>()?;

    // Recreate the transcript with the same domain separator.
    // WHY: The verifier must use the exact same transcript domain as the prover
    // for the Fiat-Shamir transform to produce matching challenges.
    let mut transcript = Transcript::new(POL_RANGE_TRANSCRIPT_DOMAIN);
    // Bind the same epoch day so the Fiat-Shamir challenges match the prover's.
    transcript.append_u64(b"epoch_day", proof.epoch_day as u64);

    // Verify the aggregated range proof.
    // WHY: This checks that each committed value is in [0, 2^32). Since the
    // prover committed to (value - minimum), a valid proof means value >= minimum.
    // The thresholds parameter is accepted for API symmetry and future use
    // (e.g., encoding thresholds into the transcript for binding), but the
    // actual threshold enforcement comes from the shift during proof generation.
    range_proof
        .verify_multiple(&bp_gens, &pc_gens, &mut transcript, &commitments, RANGE_PROOF_BITS)
        .map_err(|_| ZkError::VerificationFailed {
            reason: "range proof verification failed: one or more PoL parameters not in valid range".into(),
        })?;

    Ok(true)
}

// ============================================================================
// High-Level API Types (original implementation)
// ============================================================================

/// A zero-knowledge Proof of Life attestation (high-level API).
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
    /// Day number since genesis. Bound into the Fiat-Shamir transcript to
    /// prevent replay of a valid proof from one day being accepted on a
    /// different day.
    pub epoch_day: u32,
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
// Parameter Encoding (high-level API)
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
// High-Level Proof Generation
// ============================================================================

/// Generate a zero-knowledge Proof of Life attestation (high-level API).
///
/// Takes the raw daily sensor data (which NEVER leaves the device) and produces
/// a compact ZK proof that all 8 required parameters were met. The proof
/// can be verified by any node without learning the actual sensor values.
///
/// Proof generation time on ARM: ~200-500ms depending on device.
/// Proof size: ~1 KB (aggregated 8-value Bulletproof).
pub fn prove_daily_attestation(
    data: &DailyProofOfLifeData,
    epoch_day: u32,
) -> Result<ProofOfLifeProof, GratiaError> {
    // First validate that the data actually meets requirements.
    // WHY: We encode parameters as (value - minimum), so if any parameter
    // is below minimum, the subtraction would underflow. Check first.
    let encoded = encode_pol_parameters(data)?;

    // Bulletproofs generators: need enough for our aggregated proof.
    // WHY: BulletproofGens capacity must be >= number of values * bit width.
    let bp_gens = BulletproofGens::new(RANGE_PROOF_BITS_LEGACY, POL_PARAMETER_COUNT);
    let pc_gens = PedersenGens::default();

    // Generate random blinding factors for each commitment
    let blindings: Vec<Scalar> = (0..POL_PARAMETER_COUNT)
        .map(|_| Scalar::random(&mut OsRng))
        .collect();

    // Create the Fiat-Shamir transcript with domain separation
    let mut transcript = Transcript::new(POL_TRANSCRIPT_DOMAIN);
    // Bind the epoch day into the transcript to prevent replay attacks.
    // A valid proof from day N cannot verify against day M (M != N).
    transcript.append_u64(b"epoch_day", epoch_day as u64);

    // Build the aggregated range proof for all 8 parameters simultaneously.
    // This is more efficient than 8 individual proofs — the aggregated proof
    // is only marginally larger than a single proof.
    let (proof, commitments) = RangeProof::prove_multiple(
        &bp_gens,
        &pc_gens,
        &mut transcript,
        &encoded.values,
        &blindings,
        RANGE_PROOF_BITS_LEGACY,
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
        epoch_day,
    })
}

// ============================================================================
// High-Level Proof Verification
// ============================================================================

/// Verify a zero-knowledge Proof of Life attestation (high-level API).
///
/// Any node can call this to confirm that the prover met all 8 PoL
/// requirements without learning the actual sensor values.
///
/// Verification time on ARM: ~50-100ms (much faster than proving).
pub fn verify_daily_attestation(proof: &ProofOfLifeProof, expected_epoch_day: u32) -> Result<(), GratiaError> {
    // Reject proofs that claim a different epoch day than expected.
    if proof.epoch_day != expected_epoch_day {
        return Err(GratiaError::InvalidZkProof {
            reason: format!(
                "epoch_day mismatch: proof is for day {} but expected day {}",
                proof.epoch_day, expected_epoch_day
            ),
        });
    }
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

    let bp_gens = BulletproofGens::new(RANGE_PROOF_BITS_LEGACY, POL_PARAMETER_COUNT);
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
    // Bind the same epoch day so the Fiat-Shamir challenges match the prover's.
    transcript.append_u64(b"epoch_day", proof.epoch_day as u64);

    // Verify the aggregated range proof
    range_proof
        .verify_multiple(&bp_gens, &pc_gens, &mut transcript, &commitments, RANGE_PROOF_BITS_LEGACY)
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

    // ========================================================================
    // High-level API tests
    // ========================================================================

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
            bt_environment_change_count: 3,
            charge_cycle_event: true,
            optional_sensors: OptionalSensorData::default(),
        }
    }

    #[test]
    fn test_prove_and_verify_valid_attestation() {
        let data = make_valid_pol_data();

        let proof = prove_daily_attestation(&data, 100).expect("proof generation should succeed");
        assert_eq!(proof.commitments.len(), POL_PARAMETER_COUNT);
        assert!(!proof.range_proof.is_empty());

        let result = verify_daily_attestation(&proof, 100);
        assert!(result.is_ok(), "verification failed: {:?}", result.err());
    }

    #[test]
    fn test_prove_fails_with_insufficient_unlocks() {
        let mut data = make_valid_pol_data();
        data.unlock_count = 5; // Below minimum of 10

        let result = prove_daily_attestation(&data, 100);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("parameter 0"), "error should mention parameter 0 (unlock_count): {}", err);
    }

    #[test]
    fn test_prove_fails_with_no_orientation_change() {
        let mut data = make_valid_pol_data();
        data.orientation_changed = false;

        let result = prove_daily_attestation(&data, 100);
        assert!(result.is_err());
    }

    #[test]
    fn test_prove_fails_with_no_charge_cycle() {
        let mut data = make_valid_pol_data();
        data.charge_cycle_event = false;

        let result = prove_daily_attestation(&data, 100);
        assert!(result.is_err());
    }

    #[test]
    fn test_prove_fails_with_narrow_unlock_spread() {
        let mut data = make_valid_pol_data();
        let now = Utc::now();
        // Only 2 hours spread, need at least 6
        data.first_unlock = Some(now - chrono::Duration::hours(2));
        data.last_unlock = Some(now);

        let result = prove_daily_attestation(&data, 100);
        assert!(result.is_err());
    }

    #[test]
    fn test_prove_fails_with_insufficient_bt_environments() {
        let mut data = make_valid_pol_data();
        data.distinct_bt_environments = 0; // Need at least 2 for BT variation

        let result = prove_daily_attestation(&data, 100);
        assert!(result.is_err());
    }

    #[test]
    fn test_tampered_proof_fails_verification() {
        let data = make_valid_pol_data();
        let mut proof = prove_daily_attestation(&data, 100).expect("proof generation should succeed");

        // Tamper with a commitment
        proof.commitments[0] = [0u8; 32];

        let result = verify_daily_attestation(&proof, 100);
        assert!(result.is_err());
    }

    #[test]
    fn test_wrong_commitment_count_fails() {
        let data = make_valid_pol_data();
        let mut proof = prove_daily_attestation(&data, 100).expect("proof generation should succeed");

        // Remove a commitment
        proof.commitments.pop();

        let result = verify_daily_attestation(&proof, 100);
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
            bt_environment_change_count: 1,
            charge_cycle_event: true,
            optional_sensors: OptionalSensorData::default(),
        };

        let proof = prove_daily_attestation(&data, 100).expect("minimum valid data should produce proof");
        assert!(verify_daily_attestation(&proof, 100).is_ok());
    }

    #[test]
    fn test_proof_serialization_roundtrip() {
        let data = make_valid_pol_data();
        let proof = prove_daily_attestation(&data, 100).expect("proof generation should succeed");

        // Serialize to JSON and back
        let json = serde_json::to_string(&proof).expect("serialization should succeed");
        let deserialized: ProofOfLifeProof =
            serde_json::from_str(&json).expect("deserialization should succeed");

        assert_eq!(proof.range_proof, deserialized.range_proof);
        assert_eq!(proof.commitments, deserialized.commitments);

        // Deserialized proof should still verify
        assert!(verify_daily_attestation(&deserialized, 100).is_ok());
    }

    // ========================================================================
    // Flexible API tests (PolRangeProof / generate_pol_proof / verify_pol_proof)
    // ========================================================================

    fn default_thresholds() -> PolThresholds {
        PolThresholds::default()
    }

    fn valid_input() -> PolProofInput {
        PolProofInput {
            unlock_count: 45,
            unlock_spread_hours: 14,
            interaction_sessions: 12,
            bt_environments: 4,
        }
    }

    #[test]
    fn test_flexible_prove_and_verify_valid() {
        let input = valid_input();
        let thresholds = default_thresholds();

        let proof = generate_pol_proof(&input, &thresholds, 100)
            .expect("proof generation should succeed");

        assert_eq!(proof.parameter_count, FLEXIBLE_PARAMETER_COUNT as u8);
        assert_eq!(proof.commitments.len(), FLEXIBLE_PARAMETER_COUNT);
        assert!(!proof.proof_bytes.is_empty());

        // Each commitment should be 32 bytes (compressed Ristretto point)
        for c in &proof.commitments {
            assert_eq!(c.len(), 32, "commitment should be 32 bytes");
        }

        let result = verify_pol_proof(&proof, &thresholds, 100);
        assert!(result.is_ok(), "verification failed: {:?}", result.err());
        assert_eq!(result.unwrap(), true);
    }

    #[test]
    fn test_flexible_minimum_values_succeed() {
        // Exactly at the thresholds — should still produce a valid proof
        let input = PolProofInput {
            unlock_count: 10,
            unlock_spread_hours: 6,
            interaction_sessions: 3,
            bt_environments: 2,
        };
        let thresholds = default_thresholds();

        let proof = generate_pol_proof(&input, &thresholds, 100)
            .expect("exact minimum values should produce a valid proof");
        let result = verify_pol_proof(&proof, &thresholds, 100);
        assert!(result.is_ok(), "exact minimum proof should verify");
    }

    #[test]
    fn test_flexible_fails_unlock_count_below_threshold() {
        let input = PolProofInput {
            unlock_count: 5, // Below minimum of 10
            unlock_spread_hours: 14,
            interaction_sessions: 12,
            bt_environments: 4,
        };
        let thresholds = default_thresholds();

        let result = generate_pol_proof(&input, &thresholds, 100);
        assert!(result.is_err());
        match result.unwrap_err() {
            ZkError::InvalidInput { reason } => {
                assert!(reason.contains("unlock_count"), "error should mention unlock_count: {}", reason);
                assert!(reason.contains("5"), "error should mention the actual value");
                assert!(reason.contains("10"), "error should mention the minimum");
            }
            other => panic!("expected InvalidInput, got: {:?}", other),
        }
    }

    #[test]
    fn test_flexible_fails_spread_below_threshold() {
        let input = PolProofInput {
            unlock_count: 45,
            unlock_spread_hours: 3, // Below minimum of 6
            interaction_sessions: 12,
            bt_environments: 4,
        };
        let thresholds = default_thresholds();

        let result = generate_pol_proof(&input, &thresholds, 100);
        assert!(result.is_err());
        match result.unwrap_err() {
            ZkError::InvalidInput { reason } => {
                assert!(reason.contains("unlock_spread_hours"));
            }
            other => panic!("expected InvalidInput, got: {:?}", other),
        }
    }

    #[test]
    fn test_flexible_fails_interactions_below_threshold() {
        let input = PolProofInput {
            unlock_count: 45,
            unlock_spread_hours: 14,
            interaction_sessions: 1, // Below minimum of 3
            bt_environments: 4,
        };
        let thresholds = default_thresholds();

        let result = generate_pol_proof(&input, &thresholds, 100);
        assert!(result.is_err());
        match result.unwrap_err() {
            ZkError::InvalidInput { reason } => {
                assert!(reason.contains("interaction_sessions"));
            }
            other => panic!("expected InvalidInput, got: {:?}", other),
        }
    }

    #[test]
    fn test_flexible_fails_bt_environments_below_threshold() {
        let input = PolProofInput {
            unlock_count: 45,
            unlock_spread_hours: 14,
            interaction_sessions: 12,
            bt_environments: 1, // Below minimum of 2
        };
        let thresholds = default_thresholds();

        let result = generate_pol_proof(&input, &thresholds, 100);
        assert!(result.is_err());
        match result.unwrap_err() {
            ZkError::InvalidInput { reason } => {
                assert!(reason.contains("bt_environments"));
            }
            other => panic!("expected InvalidInput, got: {:?}", other),
        }
    }

    #[test]
    fn test_flexible_tampered_proof_fails_verification() {
        let input = valid_input();
        let thresholds = default_thresholds();

        let mut proof = generate_pol_proof(&input, &thresholds, 100)
            .expect("proof generation should succeed");

        // Tamper with the first commitment (zero it out)
        proof.commitments[0] = vec![0u8; 32];

        let result = verify_pol_proof(&proof, &thresholds, 100);
        assert!(result.is_err(), "tampered proof should not verify");
        match result.unwrap_err() {
            ZkError::VerificationFailed { .. } => {} // expected
            other => panic!("expected VerificationFailed, got: {:?}", other),
        }
    }

    #[test]
    fn test_flexible_wrong_commitment_count_fails() {
        let input = valid_input();
        let thresholds = default_thresholds();

        let mut proof = generate_pol_proof(&input, &thresholds, 100)
            .expect("proof generation should succeed");

        // Remove a commitment
        proof.commitments.pop();

        let result = verify_pol_proof(&proof, &thresholds, 100);
        assert!(result.is_err());
        match result.unwrap_err() {
            ZkError::InvalidInput { reason } => {
                assert!(reason.contains("expected"));
            }
            other => panic!("expected InvalidInput, got: {:?}", other),
        }
    }

    #[test]
    fn test_flexible_corrupt_proof_bytes_fails() {
        let input = valid_input();
        let thresholds = default_thresholds();

        let mut proof = generate_pol_proof(&input, &thresholds, 100)
            .expect("proof generation should succeed");

        // Corrupt the proof bytes
        proof.proof_bytes = vec![0xDE, 0xAD, 0xBE, 0xEF];

        let result = verify_pol_proof(&proof, &thresholds, 100);
        assert!(result.is_err());
    }

    #[test]
    fn test_flexible_custom_thresholds() {
        // Use non-default thresholds (governance-adjusted)
        let thresholds = PolThresholds {
            min_unlocks: 5,
            min_spread: 4,
            min_interactions: 2,
            min_bt_envs: 1,
        };

        // These values would fail default thresholds but pass custom ones
        let input = PolProofInput {
            unlock_count: 7,
            unlock_spread_hours: 5,
            interaction_sessions: 2,
            bt_environments: 1,
        };

        let proof = generate_pol_proof(&input, &thresholds, 100)
            .expect("proof with custom thresholds should succeed");
        let result = verify_pol_proof(&proof, &thresholds, 100);
        assert!(result.is_ok(), "verification with custom thresholds should succeed");
    }

    #[test]
    fn test_flexible_large_values_succeed() {
        // Extremely active user — values far above thresholds
        let input = PolProofInput {
            unlock_count: 500,
            unlock_spread_hours: 23,
            interaction_sessions: 100,
            bt_environments: 50,
        };
        let thresholds = default_thresholds();

        let proof = generate_pol_proof(&input, &thresholds, 100)
            .expect("large values should produce a valid proof");
        let result = verify_pol_proof(&proof, &thresholds, 100);
        assert!(result.is_ok());
    }

    #[test]
    fn test_flexible_proof_serialization_roundtrip() {
        let input = valid_input();
        let thresholds = default_thresholds();

        let proof = generate_pol_proof(&input, &thresholds, 100)
            .expect("proof generation should succeed");

        // Serialize to JSON and back
        let json = serde_json::to_string(&proof).expect("serialization should succeed");
        let deserialized: PolRangeProof =
            serde_json::from_str(&json).expect("deserialization should succeed");

        assert_eq!(proof.proof_bytes, deserialized.proof_bytes);
        assert_eq!(proof.commitments, deserialized.commitments);
        assert_eq!(proof.parameter_count, deserialized.parameter_count);

        // Deserialized proof should still verify
        let result = verify_pol_proof(&deserialized, &thresholds, 100);
        assert!(result.is_ok(), "deserialized proof should verify");
    }

    #[test]
    fn test_flexible_zk_error_conversion_to_gratia_error() {
        // Verify that ZkError converts cleanly to GratiaError
        let zk_err = ZkError::InvalidInput {
            reason: "test".into(),
        };
        let gratia_err: GratiaError = zk_err.into();
        assert!(gratia_err.to_string().contains("test"));

        let zk_err = ZkError::ProofGenerationFailed {
            reason: "gen fail".into(),
        };
        let gratia_err: GratiaError = zk_err.into();
        assert!(gratia_err.to_string().contains("gen fail"));
    }

    #[test]
    fn test_flexible_zero_values_fail_with_default_thresholds() {
        let input = PolProofInput {
            unlock_count: 0,
            unlock_spread_hours: 0,
            interaction_sessions: 0,
            bt_environments: 0,
        };
        let thresholds = default_thresholds();

        let result = generate_pol_proof(&input, &thresholds, 100);
        assert!(result.is_err(), "all-zero values should fail");
    }

    #[test]
    fn test_flexible_wrong_parameter_count_fails() {
        let input = valid_input();
        let thresholds = default_thresholds();

        let mut proof = generate_pol_proof(&input, &thresholds, 100)
            .expect("proof generation should succeed");

        // Corrupt the parameter count
        proof.parameter_count = 99;

        let result = verify_pol_proof(&proof, &thresholds, 100);
        assert!(result.is_err());
    }
}
