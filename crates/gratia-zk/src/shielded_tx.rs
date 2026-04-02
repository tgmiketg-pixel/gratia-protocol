//! Shielded transaction proofs using Bulletproofs + Pedersen commitments.
//!
//! Shielded transactions allow users to transfer GRAT without revealing
//! the amount on chain. The proof demonstrates:
//! 1. The transfer amount is non-negative (range proof)
//! 2. The sender has sufficient balance (range proof on balance - amount)
//! 3. Value is conserved: input commitment = output commitment + change commitment
//!
//! Users choose per transaction whether to use standard (transparent) or
//! shielded (ZK) transfers. Default is standard; shielded is one tap away.
//!
//! Shielded transaction creation is computationally heavier (~2-5 seconds on ARM)
//! and is designed to run during Mining Mode when the phone is plugged in.

use bulletproofs::{BulletproofGens, PedersenGens, RangeProof};
use curve25519_dalek::ristretto::CompressedRistretto;
use curve25519_dalek::scalar::Scalar;
use merlin::Transcript;
use rand::rngs::OsRng;
use serde::{Deserialize, Serialize};

use gratia_core::types::Lux;
use gratia_core::GratiaError;

use crate::pedersen::{self, PedersenCommitment};

// ============================================================================
// Constants
// ============================================================================

/// Bit width for shielded transaction range proofs.
/// 64 bits covers the full Lux range (u64).
/// WHY: Unlike PoL parameters which are small counts, transaction amounts
/// can be up to the full u64 range. 64-bit range proofs are larger (~1.2 KB)
/// but necessary to cover all valid amounts.
const TX_RANGE_PROOF_BITS: usize = 64;

/// Domain separator for shielded transaction Merlin transcripts.
const TX_TRANSCRIPT_DOMAIN: &[u8] = b"gratia-shielded-transfer-v1";

/// Number of values proven in each shielded transfer.
/// We prove two values: the transfer amount and the remaining balance (change).
/// Both must be non-negative (in range [0, 2^64)).
const TX_PROOF_VALUES: usize = 2;

// ============================================================================
// Types
// ============================================================================

/// A zero-knowledge proof for a shielded transaction.
///
/// Proves that a transfer amount is valid without revealing:
/// - The transfer amount
/// - The sender's balance
/// - The change returned to sender
///
/// Size: ~1.5-2 KB including commitments and range proof.
/// Generation time: ~2-5 seconds on ARM.
/// Verification time: ~50-100ms on ARM.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShieldedTransactionProof {
    /// Pedersen commitment to the transfer amount.
    pub amount_commitment: PedersenCommitment,
    /// Pedersen commitment to the change (balance - amount - fee).
    pub change_commitment: PedersenCommitment,
    /// Aggregated range proof proving both amount and change are non-negative.
    /// This implicitly proves the sender has sufficient balance.
    pub range_proof: Vec<u8>,
}

/// Secret data the sender needs to construct and later reference a shielded transfer.
///
/// This data stays on the sender's device and is NEVER transmitted.
/// The recipient receives only the amount commitment and must be told
/// the amount through an encrypted side channel.
#[derive(Debug, Clone)]
pub struct ShieldedTransferSecret {
    /// The actual transfer amount in Lux.
    pub amount: Lux,
    /// Blinding factor for the amount commitment.
    pub amount_blinding: Scalar,
    /// The change amount (sender_balance - amount - fee).
    pub change: Lux,
    /// Blinding factor for the change commitment.
    pub change_blinding: Scalar,
}

// ============================================================================
// Proof Generation
// ============================================================================

/// Context that binds a shielded proof to a specific transaction,
/// preventing proof detachment and replay.
#[derive(Debug, Clone)]
pub struct ShieldedTransferContext {
    /// Sender's public key (32 bytes).
    pub sender_pubkey: Vec<u8>,
    /// Recipient address (32 bytes).
    pub recipient: [u8; 32],
    /// Transaction nonce (prevents replay within the same chain).
    pub nonce: u64,
    /// Chain ID (prevents replay across networks: 1=mainnet, 2=testnet).
    pub chain_id: u32,
}

/// Generate a shielded transfer proof.
///
/// Proves that:
/// 1. `amount` is in range [0, 2^64) — non-negative
/// 2. `sender_balance - amount - fee` is in range [0, 2^64) — sufficient balance
///
/// The proof is bound to the transaction context (sender, recipient, nonce,
/// chain_id) via the Merlin transcript, preventing proof detachment and replay.
///
/// # Arguments
/// * `amount` - Transfer amount in Lux
/// * `sender_balance` - Sender's current balance in Lux (known only to sender)
/// * `fee` - Transaction fee in Lux (public, burned)
/// * `context` - Transaction-specific binding data (sender, recipient, nonce, chain_id)
///
/// # Returns
/// * `ShieldedTransactionProof` - The ZK proof (goes on chain)
/// * `ShieldedTransferSecret` - Secret data for the sender (stays on device)
pub fn prove_transfer(
    amount: Lux,
    sender_balance: Lux,
    fee: Lux,
    context: &ShieldedTransferContext,
) -> Result<(ShieldedTransactionProof, ShieldedTransferSecret), GratiaError> {
    // Validate inputs
    let total_debit = amount
        .checked_add(fee)
        .ok_or(GratiaError::InvalidZkProof {
            reason: "amount + fee overflow".into(),
        })?;

    if sender_balance < total_debit {
        return Err(GratiaError::InsufficientBalance {
            available: sender_balance,
            required: total_debit,
        });
    }

    let change = sender_balance - total_debit;

    // Generate random blinding factors
    let amount_blinding = Scalar::random(&mut OsRng);
    let change_blinding = Scalar::random(&mut OsRng);

    // Create Bulletproof generators
    let bp_gens = BulletproofGens::new(TX_RANGE_PROOF_BITS, TX_PROOF_VALUES);
    let pc_gens = PedersenGens::default();

    // Create transcript with domain separation
    let mut transcript = Transcript::new(TX_TRANSCRIPT_DOMAIN);

    // WHY: Bind the proof to this specific transaction's context so it cannot
    // be detached and replayed in a different transaction. Without these bindings,
    // an attacker could take a valid proof from one transaction and attach it to
    // a different one with the same amount.
    transcript.append_message(b"sender_pubkey", &context.sender_pubkey);
    transcript.append_message(b"recipient", &context.recipient);
    transcript.append_message(b"nonce", &context.nonce.to_le_bytes());
    transcript.append_message(b"chain_id", &context.chain_id.to_le_bytes());

    // Prove both values simultaneously in an aggregated range proof.
    // WHY: Aggregated proofs are more compact and faster to verify than
    // two individual proofs. The verifier learns nothing about either value
    // except that both are in [0, 2^64).
    let values = [amount, change];
    let blindings = [amount_blinding, change_blinding];

    let (proof, commitments) = RangeProof::prove_multiple(
        &bp_gens,
        &pc_gens,
        &mut transcript,
        &values,
        &blindings,
        TX_RANGE_PROOF_BITS,
    )
    .map_err(|e| GratiaError::InvalidZkProof {
        reason: format!("shielded transfer proof generation failed: {:?}", e),
    })?;

    let amount_commitment = PedersenCommitment {
        point: commitments[0].to_bytes(),
    };
    let change_commitment = PedersenCommitment {
        point: commitments[1].to_bytes(),
    };

    let proof = ShieldedTransactionProof {
        amount_commitment,
        change_commitment,
        range_proof: proof.to_bytes(),
    };

    let secret = ShieldedTransferSecret {
        amount,
        amount_blinding,
        change,
        change_blinding,
    };

    Ok((proof, secret))
}

// ============================================================================
// Proof Verification
// ============================================================================

/// Verify a shielded transfer proof.
///
/// Confirms that both the transfer amount and the change are non-negative
/// (in range [0, 2^64)), which implicitly proves the sender had sufficient
/// balance. The verifier learns nothing about the actual values.
///
/// The proof is verified against the same transaction context that was used
/// during proof generation. A proof generated for a different transaction
/// will fail verification.
///
/// # Arguments
/// * `proof` - The shielded transaction proof from the chain
/// * `context` - Transaction-specific binding data (must match what was used in prove_transfer)
///
/// # Returns
/// * `Ok(())` if the proof is valid
/// * `Err(GratiaError)` if the proof is invalid
pub fn verify_transfer(
    proof: &ShieldedTransactionProof,
    context: &ShieldedTransferContext,
) -> Result<(), GratiaError> {
    let bp_gens = BulletproofGens::new(TX_RANGE_PROOF_BITS, TX_PROOF_VALUES);
    let pc_gens = PedersenGens::default();

    // Deserialize the range proof
    let range_proof = RangeProof::from_bytes(&proof.range_proof).map_err(|_| {
        GratiaError::InvalidZkProof {
            reason: "invalid shielded transfer range proof encoding".into(),
        }
    })?;

    // Deserialize commitment points
    let amount_point =
        CompressedRistretto::from_slice(&proof.amount_commitment.point).map_err(|_| {
            GratiaError::InvalidZkProof {
                reason: "invalid amount commitment encoding".into(),
            }
        })?;

    let change_point =
        CompressedRistretto::from_slice(&proof.change_commitment.point).map_err(|_| {
            GratiaError::InvalidZkProof {
                reason: "invalid change commitment encoding".into(),
            }
        })?;

    let commitments = vec![amount_point, change_point];

    // Recreate transcript with same domain separator and context bindings
    let mut transcript = Transcript::new(TX_TRANSCRIPT_DOMAIN);
    transcript.append_message(b"sender_pubkey", &context.sender_pubkey);
    transcript.append_message(b"recipient", &context.recipient);
    transcript.append_message(b"nonce", &context.nonce.to_le_bytes());
    transcript.append_message(b"chain_id", &context.chain_id.to_le_bytes());

    // Verify the aggregated range proof
    range_proof
        .verify_multiple(
            &bp_gens,
            &pc_gens,
            &mut transcript,
            &commitments,
            TX_RANGE_PROOF_BITS,
        )
        .map_err(|_| GratiaError::InvalidZkProof {
            reason: "shielded transfer range proof verification failed".into(),
        })
}

/// Verify value conservation for a shielded transfer.
///
/// Given the sender's input commitment (their balance commitment) and
/// the output commitments (amount + change + fee), verify that:
///   C_input = C_amount + C_change + fee * G
///
/// The fee is public (not hidden) so it uses a commitment with zero blinding.
///
/// # Arguments
/// * `input_commitment` - Commitment to the sender's balance
/// * `proof` - The shielded transaction proof
/// * `fee` - The public transaction fee in Lux
pub fn verify_value_conservation(
    input_commitment: &PedersenCommitment,
    proof: &ShieldedTransactionProof,
    fee: Lux,
) -> Result<(), GratiaError> {
    let input_point = pedersen::decompress(input_commitment)?;
    let amount_point = pedersen::decompress(&proof.amount_commitment)?;
    let change_point = pedersen::decompress(&proof.change_commitment)?;

    // Compute the fee commitment: fee * B (no blinding, since fee is public)
    let pc_gens = PedersenGens::default();
    let fee_scalar = Scalar::from(fee);
    let zero_blinding = Scalar::ZERO;
    let fee_point = pc_gens.commit(fee_scalar, zero_blinding);

    // WHY: Pedersen commitments are additively homomorphic.
    // If input = amount + change + fee, then:
    // C(input, r_in) = C(amount, r_amt) + C(change, r_chg) + C(fee, 0)
    // This holds only if r_in = r_amt + r_chg (blinding factors sum correctly).
    let expected_input = amount_point + change_point + fee_point;

    if input_point == expected_input {
        Ok(())
    } else {
        Err(GratiaError::InvalidZkProof {
            reason: "value conservation check failed: inputs != outputs".into(),
        })
    }
}

// ============================================================================
// Combined Verification
// ============================================================================

/// Verify a shielded transaction atomically: both range proof and value conservation.
///
/// This ensures that a caller cannot accidentally verify one check without the other,
/// which would leave a security gap (e.g., valid range proof but broken conservation
/// means tokens were created from nothing).
///
/// # Arguments
/// * `input_commitment` - Commitment to the sender's balance
/// * `proof` - The shielded transaction proof from the chain
/// * `fee` - The public transaction fee in Lux
/// * `context` - Transaction-specific binding data (must match what was used in prove_transfer)
///
/// # Returns
/// * `Ok(())` if both the range proof and value conservation are valid
/// * `Err(GratiaError)` if either check fails
pub fn verify_shielded_transaction(
    input_commitment: &PedersenCommitment,
    proof: &ShieldedTransactionProof,
    fee: Lux,
    context: &ShieldedTransferContext,
) -> Result<(), GratiaError> {
    // WHY: Both checks must pass atomically. A valid range proof with broken
    // conservation means tokens were created from nothing. Valid conservation
    // with a broken range proof means negative amounts could be hidden.
    verify_transfer(proof, context)?;
    verify_value_conservation(input_commitment, proof, fee)?;
    Ok(())
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use gratia_core::types::LUX_PER_GRAT;

    /// Create a test context for shielded transfer proofs.
    fn test_context() -> ShieldedTransferContext {
        ShieldedTransferContext {
            sender_pubkey: vec![1u8; 32],
            recipient: [2u8; 32],
            nonce: 42,
            chain_id: 2, // testnet
        }
    }

    #[test]
    fn test_prove_and_verify_shielded_transfer() {
        let amount = 50 * LUX_PER_GRAT; // 50 GRAT
        let balance = 1000 * LUX_PER_GRAT; // 1000 GRAT
        let fee = 1000; // 1000 Lux fee
        let ctx = test_context();

        let (proof, secret) =
            prove_transfer(amount, balance, fee, &ctx).expect("proof generation should succeed");

        assert_eq!(secret.amount, amount);
        assert_eq!(secret.change, balance - amount - fee);

        let result = verify_transfer(&proof, &ctx);
        assert!(result.is_ok(), "verification failed: {:?}", result.err());
    }

    #[test]
    fn test_transfer_insufficient_balance() {
        let amount = 1000 * LUX_PER_GRAT;
        let balance = 500 * LUX_PER_GRAT; // Not enough
        let fee = 1000;
        let ctx = test_context();

        let result = prove_transfer(amount, balance, fee, &ctx);
        assert!(result.is_err());
        match result.unwrap_err() {
            GratiaError::InsufficientBalance { .. } => {} // Expected
            e => panic!("expected InsufficientBalance, got: {:?}", e),
        }
    }

    #[test]
    fn test_transfer_exact_balance() {
        let fee = 1000;
        let amount = 100 * LUX_PER_GRAT;
        let balance = amount + fee; // Exactly enough, zero change
        let ctx = test_context();

        let (proof, secret) =
            prove_transfer(amount, balance, fee, &ctx).expect("exact balance should work");

        assert_eq!(secret.change, 0);
        assert!(verify_transfer(&proof, &ctx).is_ok());
    }

    #[test]
    fn test_transfer_zero_amount() {
        // Zero-amount transfers should be valid (used for some protocol operations)
        let ctx = test_context();
        let (proof, secret) =
            prove_transfer(0, 1000 * LUX_PER_GRAT, 1000, &ctx).expect("zero amount should work");

        assert_eq!(secret.amount, 0);
        assert!(verify_transfer(&proof, &ctx).is_ok());
    }

    #[test]
    fn test_tampered_proof_fails() {
        let ctx = test_context();
        let (mut proof, _) = prove_transfer(50 * LUX_PER_GRAT, 1000 * LUX_PER_GRAT, 1000, &ctx)
            .expect("proof generation should succeed");

        // Tamper with the amount commitment
        proof.amount_commitment.point = [0u8; 32];

        assert!(verify_transfer(&proof, &ctx).is_err());
    }

    #[test]
    fn test_proof_serialization_roundtrip() {
        let ctx = test_context();
        let (proof, _) = prove_transfer(50 * LUX_PER_GRAT, 1000 * LUX_PER_GRAT, 1000, &ctx)
            .expect("proof generation should succeed");

        let json = serde_json::to_string(&proof).expect("serialization should succeed");
        let deserialized: ShieldedTransactionProof =
            serde_json::from_str(&json).expect("deserialization should succeed");

        assert!(verify_transfer(&deserialized, &ctx).is_ok());
    }

    #[test]
    fn test_proof_with_wrong_context_fails() {
        let ctx = test_context();
        let (proof, _) = prove_transfer(50 * LUX_PER_GRAT, 1000 * LUX_PER_GRAT, 1000, &ctx)
            .expect("proof generation should succeed");

        // Verify with a different context (different nonce) — should fail
        let mut wrong_ctx = test_context();
        wrong_ctx.nonce = 999;
        assert!(verify_transfer(&proof, &wrong_ctx).is_err());
    }

    #[test]
    fn test_value_conservation() {
        let amount = 50 * LUX_PER_GRAT;
        let balance = 1000 * LUX_PER_GRAT;
        let fee = 1000_u64;
        let change = balance - amount - fee;

        // Create input commitment with known blinding
        let input_blinding = Scalar::random(&mut OsRng);
        let (input_commitment, _) = pedersen::commit_with_blinding(balance, input_blinding);

        // Create amount and change commitments with blindings that sum to input blinding
        let amount_blinding = Scalar::random(&mut OsRng);
        // WHY: For value conservation to hold, the blinding factors must satisfy:
        // r_input = r_amount + r_change (fee has zero blinding since it's public)
        let change_blinding = input_blinding - amount_blinding;

        let bp_gens = BulletproofGens::new(TX_RANGE_PROOF_BITS, TX_PROOF_VALUES);
        let pc_gens = PedersenGens::default();
        let mut transcript = Transcript::new(TX_TRANSCRIPT_DOMAIN);

        let (range_proof, commitments) = RangeProof::prove_multiple(
            &bp_gens,
            &pc_gens,
            &mut transcript,
            &[amount, change],
            &[amount_blinding, change_blinding],
            TX_RANGE_PROOF_BITS,
        )
        .expect("range proof should succeed");

        let proof = ShieldedTransactionProof {
            amount_commitment: PedersenCommitment {
                point: commitments[0].to_bytes(),
            },
            change_commitment: PedersenCommitment {
                point: commitments[1].to_bytes(),
            },
            range_proof: range_proof.to_bytes(),
        };

        // Value conservation should pass
        assert!(verify_value_conservation(&input_commitment, &proof, fee).is_ok());

        // Wrong fee should fail conservation
        assert!(verify_value_conservation(&input_commitment, &proof, fee + 1).is_err());
    }

    #[test]
    fn test_amount_plus_fee_overflow() {
        let ctx = test_context();
        let result = prove_transfer(u64::MAX, 0, 1, &ctx);
        assert!(result.is_err());
    }
}
