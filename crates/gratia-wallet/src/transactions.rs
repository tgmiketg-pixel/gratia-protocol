//! Transaction creation, signing, and verification.
//!
//! Builds `Transaction` structs using core types from `gratia-core`,
//! signs them with the keystore, and verifies transaction signatures.
//! Supports Transfer, Stake, and Unstake payload types (Phase 1).

use chrono::Utc;
use sha2::{Sha256, Digest};

use gratia_core::error::GratiaError;
use gratia_core::types::{Address, Lux, Transaction, TransactionPayload, TxHash};

use crate::keystore::Keystore;

// ============================================================================
// Transaction Builder
// ============================================================================

/// Builds and signs transactions using a keystore.
pub struct TransactionBuilder<'a, K: Keystore> {
    keystore: &'a K,
    nonce: u64,
    fee: Lux,
}

impl<'a, K: Keystore> TransactionBuilder<'a, K> {
    /// Create a new builder bound to the given keystore.
    pub fn new(keystore: &'a K, nonce: u64, fee: Lux) -> Self {
        Self {
            keystore,
            nonce,
            fee,
        }
    }

    /// Build a transparent transfer transaction.
    pub fn build_transfer(&self, to: Address, amount: Lux) -> Result<Transaction, GratiaError> {
        let payload = TransactionPayload::Transfer { to, amount };
        self.build_and_sign(payload)
    }

    /// Build a stake transaction.
    pub fn build_stake(&self, amount: Lux) -> Result<Transaction, GratiaError> {
        let payload = TransactionPayload::Stake { amount };
        self.build_and_sign(payload)
    }

    /// Build an unstake transaction.
    pub fn build_unstake(&self, amount: Lux) -> Result<Transaction, GratiaError> {
        let payload = TransactionPayload::Unstake { amount };
        self.build_and_sign(payload)
    }

    /// Build and sign a transaction with an arbitrary payload.
    ///
    /// This is the general-purpose entry point. The typed helpers above
    /// (`build_transfer`, `build_stake`, `build_unstake`) delegate here.
    pub fn build_and_sign(&self, payload: TransactionPayload) -> Result<Transaction, GratiaError> {
        let sender_pubkey = self.keystore.public_key_bytes()?;
        let timestamp = Utc::now();

        // Serialize the signable content: payload + nonce + fee + timestamp.
        // WHY: We include nonce and timestamp in the signed blob to prevent
        // replay attacks and ensure each signature is unique even for
        // identical payloads.
        let signable = signable_bytes(&payload, self.nonce, self.fee, &timestamp)?;

        let signature = self.keystore.sign(&signable)?;

        let hash = compute_tx_hash(&sender_pubkey, &signable, &signature);

        Ok(Transaction {
            hash,
            payload,
            sender_pubkey,
            signature,
            nonce: self.nonce,
            fee: self.fee,
            timestamp,
        })
    }
}

// ============================================================================
// Verification
// ============================================================================

/// Verify that a transaction's signature is valid.
///
/// Checks that:
/// 1. The signature matches the sender's public key over the signable content.
/// 2. The transaction hash is consistent with the signed data.
pub fn verify_transaction(tx: &Transaction) -> Result<(), GratiaError> {
    let signable = signable_bytes(&tx.payload, tx.nonce, tx.fee, &tx.timestamp)?;

    // Verify the Ed25519 signature
    gratia_core::crypto::verify_signature(&tx.sender_pubkey, &signable, &tx.signature)?;

    // Verify the hash matches
    let expected_hash = compute_tx_hash(&tx.sender_pubkey, &signable, &tx.signature);
    if tx.hash.0 != expected_hash.0 {
        return Err(GratiaError::BlockValidationFailed {
            reason: "transaction hash mismatch".into(),
        });
    }

    Ok(())
}

/// Extract the sender `Address` from a transaction's public key.
pub fn sender_address(tx: &Transaction) -> Result<Address, GratiaError> {
    crate::keystore::address_from_pubkey_bytes(&tx.sender_pubkey)
}

// ============================================================================
// Internal Helpers
// ============================================================================

/// Produce the canonical byte sequence that gets signed.
///
/// WHY: Canonical serialization prevents signature malleability. Using bincode
/// gives deterministic byte order for the same logical payload.
fn signable_bytes(
    payload: &TransactionPayload,
    nonce: u64,
    fee: Lux,
    timestamp: &chrono::DateTime<Utc>,
) -> Result<Vec<u8>, GratiaError> {
    let payload_bytes = bincode::serialize(payload)
        .map_err(|e| GratiaError::SerializationError(e.to_string()))?;

    let mut buf = Vec::with_capacity(payload_bytes.len() + 8 + 8 + 8);
    buf.extend_from_slice(&payload_bytes);
    buf.extend_from_slice(&nonce.to_le_bytes());
    buf.extend_from_slice(&fee.to_le_bytes());
    buf.extend_from_slice(&timestamp.timestamp_millis().to_le_bytes());
    Ok(buf)
}

/// Compute the transaction hash from its components.
///
/// H(sender_pubkey || signable_content || signature)
fn compute_tx_hash(sender_pubkey: &[u8], signable: &[u8], signature: &[u8]) -> TxHash {
    let mut hasher = Sha256::new();
    hasher.update(sender_pubkey);
    hasher.update(signable);
    hasher.update(signature);
    let result = hasher.finalize();
    let mut hash = [0u8; 32];
    hash.copy_from_slice(&result);
    TxHash(hash)
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keystore::SoftwareKeystore;
    use gratia_core::types::LUX_PER_GRAT;

    fn setup_keystore() -> SoftwareKeystore {
        let mut ks = SoftwareKeystore::new();
        ks.generate_keypair().unwrap();
        ks
    }

    #[test]
    fn test_build_and_verify_transfer() {
        let ks = setup_keystore();
        let builder = TransactionBuilder::new(&ks, 0, 1000);

        let recipient = Address([42u8; 32]);
        let tx = builder
            .build_transfer(recipient, 5 * LUX_PER_GRAT)
            .unwrap();

        // Signature should verify
        verify_transaction(&tx).unwrap();

        // Payload should be Transfer
        match &tx.payload {
            TransactionPayload::Transfer { to, amount } => {
                assert_eq!(*to, recipient);
                assert_eq!(*amount, 5 * LUX_PER_GRAT);
            }
            _ => panic!("expected Transfer payload"),
        }

        assert_eq!(tx.nonce, 0);
        assert_eq!(tx.fee, 1000);
    }

    #[test]
    fn test_build_and_verify_stake() {
        let ks = setup_keystore();
        let builder = TransactionBuilder::new(&ks, 1, 500);

        let tx = builder.build_stake(100 * LUX_PER_GRAT).unwrap();
        verify_transaction(&tx).unwrap();

        match &tx.payload {
            TransactionPayload::Stake { amount } => {
                assert_eq!(*amount, 100 * LUX_PER_GRAT);
            }
            _ => panic!("expected Stake payload"),
        }
    }

    #[test]
    fn test_build_and_verify_unstake() {
        let ks = setup_keystore();
        let builder = TransactionBuilder::new(&ks, 2, 500);

        let tx = builder.build_unstake(50 * LUX_PER_GRAT).unwrap();
        verify_transaction(&tx).unwrap();

        match &tx.payload {
            TransactionPayload::Unstake { amount } => {
                assert_eq!(*amount, 50 * LUX_PER_GRAT);
            }
            _ => panic!("expected Unstake payload"),
        }
    }

    #[test]
    fn test_tampered_signature_fails_verification() {
        let ks = setup_keystore();
        let builder = TransactionBuilder::new(&ks, 0, 1000);

        let mut tx = builder
            .build_transfer(Address([1u8; 32]), 1_000_000)
            .unwrap();

        // Flip a byte in the signature
        tx.signature[0] ^= 0xFF;

        let result = verify_transaction(&tx);
        assert!(result.is_err());
    }

    #[test]
    fn test_tampered_hash_fails_verification() {
        let ks = setup_keystore();
        let builder = TransactionBuilder::new(&ks, 0, 1000);

        let mut tx = builder
            .build_transfer(Address([1u8; 32]), 1_000_000)
            .unwrap();

        // Tamper with the hash
        tx.hash.0[0] ^= 0xFF;

        let result = verify_transaction(&tx);
        assert!(result.is_err());
    }

    #[test]
    fn test_sender_address_extraction() {
        let ks = setup_keystore();
        let expected_addr = crate::keystore::address_from_pubkey_bytes(
            &ks.public_key_bytes().unwrap(),
        )
        .unwrap();

        let builder = TransactionBuilder::new(&ks, 0, 0);
        let tx = builder
            .build_transfer(Address([0u8; 32]), 1)
            .unwrap();

        let addr = sender_address(&tx).unwrap();
        assert_eq!(addr, expected_addr);
    }

    #[test]
    fn test_different_nonces_produce_different_hashes() {
        let ks = setup_keystore();

        let tx1 = TransactionBuilder::new(&ks, 0, 1000)
            .build_transfer(Address([1u8; 32]), 100)
            .unwrap();
        let tx2 = TransactionBuilder::new(&ks, 1, 1000)
            .build_transfer(Address([1u8; 32]), 100)
            .unwrap();

        assert_ne!(tx1.hash.0, tx2.hash.0);
    }
}
