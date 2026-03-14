//! Cryptographic primitives for the Gratia protocol.
//!
//! This module provides key generation, signing, verification, and hashing
//! utilities. On mobile devices, key operations are delegated to the
//! hardware secure enclave via the FFI layer. This module provides the
//! software fallback and common interfaces.

use ed25519_dalek::{SigningKey, VerifyingKey, Signature, Signer, Verifier};
use sha2::{Sha256, Digest};
use rand::rngs::OsRng;

use crate::types::{NodeId, Address};
use crate::error::GratiaError;

/// A keypair for signing transactions and attestations.
/// On real devices, the private key lives in the secure enclave
/// and is never exposed. This struct is used for testing and
/// for operations where the secure enclave provides a signing interface.
pub struct Keypair {
    signing_key: SigningKey,
    verifying_key: VerifyingKey,
}

impl Keypair {
    /// Generate a new random keypair.
    /// In production, this is called within the secure enclave.
    pub fn generate() -> Self {
        let signing_key = SigningKey::generate(&mut OsRng);
        let verifying_key = signing_key.verifying_key();
        Keypair {
            signing_key,
            verifying_key,
        }
    }

    /// Get the public (verifying) key.
    pub fn public_key(&self) -> &VerifyingKey {
        &self.verifying_key
    }

    /// Get the node ID derived from this keypair.
    pub fn node_id(&self) -> NodeId {
        NodeId::from_public_key(&self.verifying_key)
    }

    /// Get the wallet address derived from this keypair.
    pub fn address(&self) -> Address {
        Address::from_public_key(&self.verifying_key)
    }

    /// Sign a message.
    pub fn sign(&self, message: &[u8]) -> Vec<u8> {
        let signature = self.signing_key.sign(message);
        signature.to_bytes().to_vec()
    }

    /// Get the public key bytes for serialization.
    pub fn public_key_bytes(&self) -> Vec<u8> {
        self.verifying_key.as_bytes().to_vec()
    }
}

/// Verify an Ed25519 signature.
pub fn verify_signature(
    public_key_bytes: &[u8],
    message: &[u8],
    signature_bytes: &[u8],
) -> Result<(), GratiaError> {
    let public_key = VerifyingKey::from_bytes(
        public_key_bytes
            .try_into()
            .map_err(|_| GratiaError::InvalidSignature)?,
    )
    .map_err(|_| GratiaError::InvalidSignature)?;

    let signature = Signature::from_bytes(
        signature_bytes
            .try_into()
            .map_err(|_| GratiaError::InvalidSignature)?,
    );

    public_key
        .verify(message, &signature)
        .map_err(|_| GratiaError::InvalidSignature)
}

/// Compute SHA-256 hash of data.
pub fn sha256(data: &[u8]) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(data);
    let result = hasher.finalize();
    let mut hash = [0u8; 32];
    hash.copy_from_slice(&result);
    hash
}

/// Compute SHA-256 hash of multiple data segments.
pub fn sha256_multi(segments: &[&[u8]]) -> [u8; 32] {
    let mut hasher = Sha256::new();
    for segment in segments {
        hasher.update(segment);
    }
    let result = hasher.finalize();
    let mut hash = [0u8; 32];
    hash.copy_from_slice(&result);
    hash
}

/// Compute a simple Merkle root from a list of hashes.
pub fn merkle_root(hashes: &[[u8; 32]]) -> [u8; 32] {
    if hashes.is_empty() {
        return [0u8; 32];
    }
    if hashes.len() == 1 {
        return hashes[0];
    }

    let mut current_level: Vec<[u8; 32]> = hashes.to_vec();

    while current_level.len() > 1 {
        let mut next_level = Vec::new();
        for chunk in current_level.chunks(2) {
            if chunk.len() == 2 {
                next_level.push(sha256_multi(&[&chunk[0], &chunk[1]]));
            } else {
                // Odd number of nodes: duplicate the last one
                next_level.push(sha256_multi(&[&chunk[0], &chunk[0]]));
            }
        }
        current_level = next_level;
    }

    current_level[0]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_keypair_generation() {
        let keypair = Keypair::generate();
        let node_id = keypair.node_id();
        let address = keypair.address();

        // NodeId and Address should be different (different domain separators)
        assert_ne!(node_id.0, address.0);
    }

    #[test]
    fn test_sign_and_verify() {
        let keypair = Keypair::generate();
        let message = b"hello gratia";
        let signature = keypair.sign(message);

        assert!(verify_signature(
            &keypair.public_key_bytes(),
            message,
            &signature,
        )
        .is_ok());
    }

    #[test]
    fn test_verify_wrong_message() {
        let keypair = Keypair::generate();
        let signature = keypair.sign(b"correct message");

        assert!(verify_signature(
            &keypair.public_key_bytes(),
            b"wrong message",
            &signature,
        )
        .is_err());
    }

    #[test]
    fn test_merkle_root_deterministic() {
        let hashes = vec![
            sha256(b"tx1"),
            sha256(b"tx2"),
            sha256(b"tx3"),
        ];

        let root1 = merkle_root(&hashes);
        let root2 = merkle_root(&hashes);
        assert_eq!(root1, root2);
    }

    #[test]
    fn test_merkle_root_empty() {
        let root = merkle_root(&[]);
        assert_eq!(root, [0u8; 32]);
    }

    #[test]
    fn test_proof_of_life_validation() {
        use crate::types::{DailyProofOfLifeData, OptionalSensorData};
        use chrono::Utc;

        // Valid PoL data
        let now = Utc::now();
        let valid_data = DailyProofOfLifeData {
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
        };
        assert!(valid_data.is_valid());

        // Invalid: too few unlocks
        let mut invalid = valid_data.clone();
        invalid.unlock_count = 5;
        assert!(!invalid.is_valid());

        // Invalid: no charge cycle
        let mut invalid = valid_data.clone();
        invalid.charge_cycle_event = false;
        assert!(!invalid.is_valid());

        // Invalid: no BT variation
        let mut invalid = valid_data.clone();
        invalid.distinct_bt_environments = 1;
        assert!(!invalid.is_valid());
    }
}
