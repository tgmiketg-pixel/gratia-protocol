#![no_main]
//! Fuzz target: ZK proof verification with malformed proofs.
//!
//! Constructs PolRangeProof structs from fuzzed bytes and calls
//! verify_pol_proof(). Must not panic — should return Err for
//! all invalid proofs.

use libfuzzer_sys::fuzz_target;
use arbitrary::Arbitrary;

use gratia_zk::{PolRangeProof, PolThresholds, verify_pol_proof};

/// Arbitrary-derived struct to generate structured fuzz input.
/// This gives the fuzzer better coverage than raw bytes by producing
/// structurally valid PolRangeProof instances with fuzzed field values.
#[derive(Arbitrary, Debug)]
struct FuzzInput {
    proof_bytes: Vec<u8>,
    /// Number of commitment blobs to generate (capped in the target).
    num_commitments: u8,
    /// Raw bytes to slice into commitment blobs.
    commitment_data: Vec<u8>,
    parameter_count: u8,
    epoch_day: u32,
    /// Fuzzed thresholds
    min_unlocks: u64,
    min_spread: u64,
    min_interactions: u64,
    min_bt_envs: u64,
    /// The epoch_day to pass to the verifier (may differ from proof's epoch_day).
    verify_epoch_day: u32,
}

fuzz_target!(|input: FuzzInput| {
    // Build commitments from the fuzz data.
    // Each commitment should be 32 bytes (compressed Ristretto point).
    let num_commitments = (input.num_commitments % 8) as usize; // cap at 8
    let mut commitments = Vec::with_capacity(num_commitments);
    for i in 0..num_commitments {
        let start = (i * 32) % (input.commitment_data.len().max(1));
        let end = (start + 32).min(input.commitment_data.len());
        commitments.push(input.commitment_data[start..end].to_vec());
    }

    let proof = PolRangeProof {
        proof_bytes: input.proof_bytes,
        commitments,
        parameter_count: input.parameter_count,
        epoch_day: input.epoch_day,
    };

    let thresholds = PolThresholds {
        min_unlocks: input.min_unlocks,
        min_spread: input.min_spread,
        min_interactions: input.min_interactions,
        min_bt_envs: input.min_bt_envs,
    };

    // Verify with matching epoch_day
    let _ = verify_pol_proof(&proof, &thresholds, input.epoch_day);

    // Verify with potentially mismatched epoch_day
    let _ = verify_pol_proof(&proof, &thresholds, input.verify_epoch_day);

    // Also try deserializing raw bytes as PolRangeProof
    let raw_bytes: Vec<u8> = bincode::serialize(&proof).unwrap_or_default();
    if let Ok(deserialized) = bincode::deserialize::<PolRangeProof>(&raw_bytes) {
        let _ = verify_pol_proof(&deserialized, &thresholds, input.epoch_day);
    }
});
