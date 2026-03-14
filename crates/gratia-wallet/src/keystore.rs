//! Ed25519 key management for the Gratia wallet.
//!
//! This module provides a trait-based keystore abstraction so that the
//! actual key storage backend can be swapped between a software implementation
//! (used for testing and development) and a hardware secure enclave
//! (Android Keystore/StrongBox, iOS Secure Enclave) on real devices.
//!
//! Keys never leave the secure enclave in production. The software keystore
//! is provided only for environments where hardware security is unavailable.

use ed25519_dalek::{SigningKey, VerifyingKey, Signature, Signer, Verifier};
use rand::rngs::OsRng;
use sha2::{Sha256, Digest};
use serde::{Deserialize, Serialize};

use gratia_core::error::GratiaError;
use gratia_core::types::{Address, NodeId};

// ============================================================================
// Keystore Trait
// ============================================================================

/// Abstraction over key storage backends.
///
/// On mobile devices, implementations delegate to the hardware secure enclave
/// (Android Keystore/StrongBox or iOS Secure Enclave). In tests and on desktop,
/// the `SoftwareKeystore` provides an in-memory implementation.
pub trait Keystore: Send + Sync {
    /// Generate a new keypair and store it. Returns the public key bytes.
    fn generate_keypair(&mut self) -> Result<Vec<u8>, GratiaError>;

    /// Sign a message using the stored private key.
    fn sign(&self, message: &[u8]) -> Result<Vec<u8>, GratiaError>;

    /// Get the public key bytes (Ed25519, 32 bytes).
    fn public_key_bytes(&self) -> Result<Vec<u8>, GratiaError>;

    /// Check whether a keypair has been generated and is available.
    fn has_keypair(&self) -> bool;

    /// Export the raw secret key bytes (32 bytes).
    ///
    /// # Security Warning
    /// This is only available on software keystores for backup/seed-phrase
    /// purposes. Hardware secure enclave implementations MUST return
    /// `GratiaError::WalletLocked` — the private key never leaves the chip.
    fn export_secret_key(&self) -> Result<Vec<u8>, GratiaError>;

    /// Import a keypair from raw secret key bytes (32 bytes).
    ///
    /// Used for seed-phrase recovery on software keystores.
    /// Hardware implementations MUST return an error.
    fn import_secret_key(&mut self, secret: &[u8]) -> Result<Vec<u8>, GratiaError>;
}

// ============================================================================
// Key Derivation Helpers
// ============================================================================

/// Derive a wallet `Address` from Ed25519 public key bytes.
pub fn address_from_pubkey_bytes(pubkey: &[u8]) -> Result<Address, GratiaError> {
    let verifying_key = VerifyingKey::from_bytes(
        pubkey
            .try_into()
            .map_err(|_| GratiaError::InvalidSignature)?,
    )
    .map_err(|_| GratiaError::InvalidSignature)?;
    Ok(Address::from_public_key(&verifying_key))
}

/// Derive a `NodeId` from Ed25519 public key bytes.
pub fn node_id_from_pubkey_bytes(pubkey: &[u8]) -> Result<NodeId, GratiaError> {
    let verifying_key = VerifyingKey::from_bytes(
        pubkey
            .try_into()
            .map_err(|_| GratiaError::InvalidSignature)?,
    )
    .map_err(|_| GratiaError::InvalidSignature)?;
    Ok(NodeId::from_public_key(&verifying_key))
}

/// Verify an Ed25519 signature over a message given public key bytes.
pub fn verify_signature(
    pubkey: &[u8],
    message: &[u8],
    signature_bytes: &[u8],
) -> Result<(), GratiaError> {
    gratia_core::crypto::verify_signature(pubkey, message, signature_bytes)
}

// ============================================================================
// Software Keystore (development / testing / optional seed-phrase backup)
// ============================================================================

/// In-memory software keystore. Suitable for testing and desktop environments.
///
/// # Security Note
/// This stores the private key in process memory. On production mobile builds,
/// use the platform-specific secure enclave keystore instead.
#[derive(Default)]
pub struct SoftwareKeystore {
    signing_key: Option<SigningKey>,
}

impl SoftwareKeystore {
    pub fn new() -> Self {
        Self { signing_key: None }
    }

    /// Convenience: get the `Address` for the stored keypair.
    pub fn address(&self) -> Result<Address, GratiaError> {
        let pubkey = self.public_key_bytes()?;
        address_from_pubkey_bytes(&pubkey)
    }

    /// Convenience: get the `NodeId` for the stored keypair.
    pub fn node_id(&self) -> Result<NodeId, GratiaError> {
        let pubkey = self.public_key_bytes()?;
        node_id_from_pubkey_bytes(&pubkey)
    }
}

impl Keystore for SoftwareKeystore {
    fn generate_keypair(&mut self) -> Result<Vec<u8>, GratiaError> {
        let signing_key = SigningKey::generate(&mut OsRng);
        let pubkey = signing_key.verifying_key().as_bytes().to_vec();
        self.signing_key = Some(signing_key);
        Ok(pubkey)
    }

    fn sign(&self, message: &[u8]) -> Result<Vec<u8>, GratiaError> {
        let key = self.signing_key.as_ref().ok_or(GratiaError::WalletLocked)?;
        let signature = key.sign(message);
        Ok(signature.to_bytes().to_vec())
    }

    fn public_key_bytes(&self) -> Result<Vec<u8>, GratiaError> {
        let key = self.signing_key.as_ref().ok_or(GratiaError::WalletLocked)?;
        Ok(key.verifying_key().as_bytes().to_vec())
    }

    fn has_keypair(&self) -> bool {
        self.signing_key.is_some()
    }

    fn export_secret_key(&self) -> Result<Vec<u8>, GratiaError> {
        let key = self.signing_key.as_ref().ok_or(GratiaError::WalletLocked)?;
        Ok(key.to_bytes().to_vec())
    }

    fn import_secret_key(&mut self, secret: &[u8]) -> Result<Vec<u8>, GratiaError> {
        let bytes: [u8; 32] = secret
            .try_into()
            .map_err(|_| GratiaError::Other("secret key must be exactly 32 bytes".into()))?;
        let signing_key = SigningKey::from_bytes(&bytes);
        let pubkey = signing_key.verifying_key().as_bytes().to_vec();
        self.signing_key = Some(signing_key);
        Ok(pubkey)
    }
}

// ============================================================================
// Serializable key material (for encrypted-at-rest persistence)
// ============================================================================

/// Encrypted key material for on-disk storage.
///
/// In production, the secure enclave handles persistence. This struct is used
/// only by the software keystore when the user opts into seed-phrase backup
/// or when persisting keys to disk in development builds.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EncryptedKeyMaterial {
    /// The encrypted secret key bytes (32 bytes encrypted).
    pub ciphertext: Vec<u8>,
    /// Salt used for key derivation from the encryption passphrase.
    pub salt: Vec<u8>,
    /// Nonce / IV for the symmetric cipher.
    pub nonce: Vec<u8>,
    /// Wallet address (derived from the public key, stored in plaintext
    /// so the wallet can be identified without decryption).
    pub address: Address,
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_generate_and_sign() {
        let mut ks = SoftwareKeystore::new();
        assert!(!ks.has_keypair());

        let pubkey = ks.generate_keypair().unwrap();
        assert!(ks.has_keypair());
        assert_eq!(pubkey.len(), 32);

        let message = b"hello gratia wallet";
        let sig = ks.sign(message).unwrap();
        assert_eq!(sig.len(), 64); // Ed25519 signatures are 64 bytes

        // Verify with our helper
        verify_signature(&pubkey, message, &sig).unwrap();
    }

    #[test]
    fn test_sign_without_keypair_fails() {
        let ks = SoftwareKeystore::new();
        let result = ks.sign(b"test");
        assert!(result.is_err());
    }

    #[test]
    fn test_address_and_node_id_derivation() {
        let mut ks = SoftwareKeystore::new();
        ks.generate_keypair().unwrap();

        let address = ks.address().unwrap();
        let node_id = ks.node_id().unwrap();

        // Address and NodeId use different domain separators, so they must differ
        assert_ne!(address.0, node_id.0);

        // Address display starts with "grat:"
        let addr_str = format!("{}", address);
        assert!(addr_str.starts_with("grat:"));
    }

    #[test]
    fn test_export_and_import_roundtrip() {
        let mut ks1 = SoftwareKeystore::new();
        ks1.generate_keypair().unwrap();

        let secret = ks1.export_secret_key().unwrap();
        let pubkey1 = ks1.public_key_bytes().unwrap();

        // Import into a fresh keystore
        let mut ks2 = SoftwareKeystore::new();
        let pubkey2 = ks2.import_secret_key(&secret).unwrap();

        assert_eq!(pubkey1, pubkey2);

        // Both should produce the same signature
        let msg = b"deterministic check";
        let sig1 = ks1.sign(msg).unwrap();
        let sig2 = ks2.sign(msg).unwrap();
        assert_eq!(sig1, sig2);
    }

    #[test]
    fn test_import_invalid_key_length() {
        let mut ks = SoftwareKeystore::new();
        let result = ks.import_secret_key(&[0u8; 16]); // Wrong length
        assert!(result.is_err());
    }

    #[test]
    fn test_verify_wrong_signature_fails() {
        let mut ks = SoftwareKeystore::new();
        ks.generate_keypair().unwrap();

        let pubkey = ks.public_key_bytes().unwrap();
        let sig = ks.sign(b"correct message").unwrap();

        let result = verify_signature(&pubkey, b"wrong message", &sig);
        assert!(result.is_err());
    }

    #[test]
    fn test_address_from_pubkey_bytes_invalid() {
        let result = address_from_pubkey_bytes(&[0u8; 16]);
        assert!(result.is_err());
    }
}
