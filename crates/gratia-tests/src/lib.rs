//! Security attack simulation tests for the Gratia protocol.
//!
//! This crate exercises the three-pillar security model against realistic
//! attack scenarios. Each module targets a different threat vector:
//!
//! ## Simulation Tests
//! - `phone_farm_attack` — Proof of Life vs. phone farms (co-located multi-device attacks)
//! - `sybil_resistance` — Sybil attack resistance across PoL, staking, VRF, and governance
//! - `network_partition` — Network resilience under splits, disconnects, and shard failures
//!
//! ## Security Tests
//! - `behavioral_spoofing` — PoL behavioral analysis vs. replay, synthetic data, and recovery theft
//! - `emulator_detection` — TEE and energy layer vs. emulators, VMs, and cloud farms
//! - `stake_manipulation` — Staking edge cases: caps, overflow, cooldowns, whale scenarios

#[cfg(test)]
pub mod phone_farm_attack;
#[cfg(test)]
pub mod sybil_resistance;
#[cfg(test)]
pub mod network_partition;
#[cfg(test)]
pub mod behavioral_spoofing;
#[cfg(test)]
pub mod emulator_detection;
#[cfg(test)]
pub mod stake_manipulation;
