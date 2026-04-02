//! gratia-zk — Zero-knowledge proof system for the Gratia protocol.
//!
//! This crate provides all ZK proof functionality used by Gratia:
//!
//! - **Bulletproofs** (`bulletproofs` module): Proof of Life attestations that prove
//!   all 8 required daily parameters were met without revealing raw sensor data.
//!   No trusted setup. Compact proofs (~1 KB). Proven on ARM devices.
//!
//! - **Pedersen commitments** (`pedersen` module): Cryptographic commitments that
//!   hide values while remaining additively homomorphic. Used as building blocks
//!   for both PoL attestations and shielded transactions.
//!
//! - **Shielded transactions** (`shielded_tx` module): Optional privacy-preserving
//!   transfers where the amount is hidden behind a Pedersen commitment with a
//!   Bulletproofs range proof. Users choose per-transaction whether to use
//!   transparent or shielded mode.
//!
//! All proofs are designed for mobile ARM devices:
//! - Proof generation: 200ms-5s depending on proof type
//! - Proof verification: 50-100ms (fast enough for block validation)
//! - No trusted setup required for Bulletproofs/Pedersen; Groth16 uses a CRS
//!
//! - **Groth16** (`groth16` module): Sigma-protocol-based ZK-SNARK for complex
//!   smart contract interactions. Uses R1CS constraint systems with Pedersen-style
//!   commitments on Ristretto. Includes pre-built circuits for range proofs,
//!   Merkle inclusion proofs, and balance conservation proofs.

pub mod bulletproofs;
pub mod groth16;
pub mod pedersen;
pub mod shielded_tx;

// Re-export primary types for convenience
pub use bulletproofs::{ProofOfLifeProof, prove_daily_attestation, verify_daily_attestation};
pub use bulletproofs::{
    PolRangeProof, PolProofInput, PolThresholds, ZkError,
    generate_pol_proof, verify_pol_proof,
};
pub use groth16::{
    Groth16Proof, Groth16Error, Circuit, ConstraintSystem, SetupParameters, VerificationKey,
    prove_range, prove_merkle_inclusion, prove_balance_conservation,
};
pub use pedersen::{PedersenCommitment, PedersenOpening};
pub use shielded_tx::{ShieldedTransactionProof, ShieldedTransferSecret, ShieldedTransferContext, prove_transfer, verify_transfer, verify_value_conservation, verify_shielded_transaction};
