#![no_main]
//! Fuzz target: Bulletproofs proof generation with arbitrary inputs.
//!
//! Feeds fuzzed u64 values as PolProofInput fields and arbitrary thresholds
//! to generate_pol_proof(). Must not panic — should return Ok for valid
//! inputs (value >= threshold) and Err for invalid ones.

use libfuzzer_sys::fuzz_target;
use arbitrary::Arbitrary;

use gratia_zk::{PolProofInput, PolThresholds, generate_pol_proof, verify_pol_proof};

/// Structured fuzz input for proof generation.
/// Using Arbitrary gives the fuzzer structured coverage over the input space.
#[derive(Arbitrary, Debug)]
struct FuzzInput {
    unlock_count: u64,
    unlock_spread_hours: u64,
    interaction_sessions: u64,
    bt_environments: u64,
    min_unlocks: u64,
    min_spread: u64,
    min_interactions: u64,
    min_bt_envs: u64,
    epoch_day: u32,
}

fuzz_target!(|input: FuzzInput| {
    let data = PolProofInput {
        unlock_count: input.unlock_count,
        unlock_spread_hours: input.unlock_spread_hours,
        interaction_sessions: input.interaction_sessions,
        bt_environments: input.bt_environments,
    };

    let thresholds = PolThresholds {
        min_unlocks: input.min_unlocks,
        min_spread: input.min_spread,
        min_interactions: input.min_interactions,
        min_bt_envs: input.min_bt_envs,
    };

    // Attempt proof generation. This must not panic regardless of input values.
    // - If any value < threshold, should return Err(InvalidInput).
    // - If values are astronomically large, should still not panic.
    // - If thresholds are 0, should still work.
    match generate_pol_proof(&data, &thresholds, input.epoch_day) {
        Ok(proof) => {
            // If generation succeeded, verify the proof — this roundtrip
            // must also not panic and should return Ok(true).
            let _ = verify_pol_proof(&proof, &thresholds, input.epoch_day);

            // Also verify with a different epoch_day — should return Err.
            let _ = verify_pol_proof(&proof, &thresholds, input.epoch_day.wrapping_add(1));
        }
        Err(_) => {
            // Expected for invalid inputs (value < threshold, overflow, etc.)
        }
    }
});
