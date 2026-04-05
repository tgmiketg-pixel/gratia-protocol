//! Ed25519 key management for the Gratia wallet.
//!
//! This module provides a trait-based keystore abstraction so that the
//! actual key storage backend can be swapped between a software implementation
//! (used for testing and development) and a hardware secure enclave
//! (Android Keystore/StrongBox, iOS Secure Enclave) on real devices.
//!
//! Keys never leave the secure enclave in production. The software keystore
//! is provided only for environments where hardware security is unavailable.

use ed25519_dalek::{SigningKey, Signer, VerifyingKey};
use rand::rngs::OsRng;
use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

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
    fn export_secret_key(&self) -> Result<Zeroizing<Vec<u8>>, GratiaError>;

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

    fn export_secret_key(&self) -> Result<Zeroizing<Vec<u8>>, GratiaError> {
        let key = self.signing_key.as_ref().ok_or(GratiaError::WalletLocked)?;
        Ok(Zeroizing::new(key.to_bytes().to_vec()))
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
// File-backed Keystore (development / testnet persistence)
// ============================================================================

/// File-backed keystore that persists the Ed25519 signing key to disk.
///
/// WHY: The SoftwareKeystore loses the key when the app restarts, generating
/// a new wallet address each time. FileKeystore writes the key to a file in
/// the app's private data directory. On Android, this is accessible only to
/// the app process (sandboxed by the OS).
///
/// For production, the secure enclave keystore should be used instead.
/// FileKeystore is suitable for testnet and development.
pub struct FileKeystore {
    signing_key: Option<SigningKey>,
    /// Path to the 32-byte key file on disk.
    key_path: String,
}

impl FileKeystore {
    /// Create a new FileKeystore. If the key file already exists at
    /// `{data_dir}/wallet_key.bin`, the key is loaded immediately.
    pub fn new(data_dir: &str) -> Self {
        let key_path = format!("{}/wallet_key.bin", data_dir);
        let signing_key = Self::load_key(&key_path);
        if signing_key.is_some() {
            tracing::info!("FileKeystore: loaded existing key from {}", key_path);
        }
        Self { signing_key, key_path }
    }

    /// Read the key file and construct a SigningKey.
    /// Returns None if the file doesn't exist or contains invalid data.
    ///
    /// Handles three formats in priority order:
    /// 1. AES-256-GCM encrypted (new format, ciphertext is 48 bytes)
    /// 2. XOR-obfuscated (legacy format, ciphertext is 32 bytes) — auto-upgrades to AES-GCM
    /// 3. Raw 32-byte plaintext (debug builds only)
    fn load_key(path: &str) -> Option<SigningKey> {
        let bytes = std::fs::read(path).ok()?;

        // Try encrypted format (AES-GCM or legacy XOR — decrypt_key_material handles both).
        if let Some((key, needs_upgrade)) = Self::load_encrypted_key(&bytes) {
            if needs_upgrade {
                // Auto-upgrade: re-save in AES-GCM format
                if let Err(e) = Self::save_key(path, &key) {
                    tracing::warn!(
                        "FileKeystore: loaded legacy XOR key but failed to upgrade to AES-GCM: {}",
                        e
                    );
                } else {
                    tracing::info!(
                        "FileKeystore: auto-upgraded key file from XOR to AES-256-GCM at {}",
                        path
                    );
                }
            }
            return Some(key);
        }

        #[cfg(debug_assertions)]
        {
            // SECURITY: Dev-only plaintext fallback. Release builds never reach here.
            if bytes.len() == 32 {
                tracing::warn!(
                    "FileKeystore: loaded UNENCRYPTED key from {}. \
                     This is only permitted in debug builds.",
                    path
                );
                let arr: [u8; 32] = bytes.try_into().ok()?;
                return Some(SigningKey::from_bytes(&arr));
            }
        }

        None
    }

    /// Attempt to load an EncryptedKeyMaterial JSON file and decrypt it.
    /// Returns (key, needs_upgrade) where needs_upgrade is true if the file
    /// was in the legacy XOR format and should be re-saved as AES-GCM.
    fn load_encrypted_key(bytes: &[u8]) -> Option<(SigningKey, bool)> {
        let ekm: EncryptedKeyMaterial = serde_json::from_slice(bytes).ok()?;
        let is_legacy = ekm.ciphertext.len() == 32;
        let decrypted = Self::decrypt_key_material(&ekm).ok()?;
        let arr: [u8; 32] = decrypted.try_into().ok()?;
        Some((SigningKey::from_bytes(&arr), is_legacy))
    }

    /// Derive a 32-byte encryption key from a domain string and salt using SHA-256.
    ///
    /// TODO(audit): Replace with a proper KDF (Argon2 or HKDF) and use a real
    /// device-bound key from Android Keystore / iOS Secure Enclave instead of
    /// a hardcoded domain string. The current approach provides authenticated
    /// encryption (AES-256-GCM) but not strong protection against a local
    /// attacker who can read the salt from the same file.
    fn derive_encryption_key(salt: &[u8]) -> [u8; 32] {
        use sha2::{Sha256, Digest};
        let mut hasher = Sha256::new();
        hasher.update(b"gratia-keystore-v1");
        hasher.update(salt);
        let result = hasher.finalize();
        let mut key = [0u8; 32];
        key.copy_from_slice(&result);
        key
    }

    /// Encrypt secret key bytes into EncryptedKeyMaterial using AES-256-GCM.
    ///
    /// Provides authenticated encryption: ciphertext integrity is protected by
    /// a 16-byte authentication tag. Any tampering (bit-flip, truncation) is
    /// detected on decryption.
    ///
    /// Format: salt (16 bytes) + nonce (12 bytes) stored in EncryptedKeyMaterial,
    /// ciphertext field contains encrypted data + 16-byte GCM auth tag.
    fn encrypt_key_material(
        secret_bytes: &[u8; 32],
        address: Address,
    ) -> EncryptedKeyMaterial {
        use ring::aead;

        // Generate random 16-byte salt and 12-byte nonce
        let mut salt = vec![0u8; 16];
        let mut nonce_bytes = [0u8; 12];
        let mut rng = OsRng;
        use rand::RngCore;
        rng.fill_bytes(&mut salt);
        rng.fill_bytes(&mut nonce_bytes);

        // Derive 256-bit key from salt
        let enc_key_bytes = Self::derive_encryption_key(&salt);
        let unbound_key = aead::UnboundKey::new(&aead::AES_256_GCM, &enc_key_bytes)
            .expect("AES-256-GCM key creation should not fail with 32-byte key");
        let sealing_key = aead::LessSafeKey::new(unbound_key);

        let nonce = aead::Nonce::assume_unique_for_key(nonce_bytes);
        let aad = aead::Aad::from(b"gratia-wallet-v2");

        // AES-GCM seal operates in-place: plaintext buffer is extended with the
        // 16-byte auth tag appended after encryption.
        let mut in_out = secret_bytes.to_vec();
        sealing_key
            .seal_in_place_append_tag(nonce, aad, &mut in_out)
            .expect("AES-256-GCM seal should not fail");

        // in_out is now 32 bytes ciphertext + 16 bytes tag = 48 bytes
        EncryptedKeyMaterial {
            ciphertext: in_out,
            salt,
            nonce: nonce_bytes.to_vec(),
            address,
        }
    }

    /// Decrypt an EncryptedKeyMaterial back to raw secret key bytes using AES-256-GCM.
    ///
    /// Returns a clear error if the authentication tag does not match (file
    /// corrupted or tampered with).
    fn decrypt_key_material(ekm: &EncryptedKeyMaterial) -> Result<Vec<u8>, GratiaError> {
        use ring::aead;

        // New AES-GCM format: ciphertext is 32 + 16 (tag) = 48 bytes
        if ekm.ciphertext.len() != 48 {
            // Fall back to legacy XOR format (ciphertext is exactly 32 bytes)
            if ekm.ciphertext.len() == 32 {
                return Self::decrypt_key_material_legacy(ekm);
            }
            return Err(GratiaError::Other("invalid ciphertext length".into()));
        }

        if ekm.nonce.len() != 12 {
            return Err(GratiaError::Other("invalid nonce length".into()));
        }

        let enc_key_bytes = Self::derive_encryption_key(&ekm.salt);
        let unbound_key = aead::UnboundKey::new(&aead::AES_256_GCM, &enc_key_bytes)
            .map_err(|_| GratiaError::Other("failed to create AES-256-GCM key".into()))?;
        let opening_key = aead::LessSafeKey::new(unbound_key);

        let nonce_bytes: [u8; 12] = ekm.nonce.as_slice().try_into()
            .map_err(|_| GratiaError::Other("invalid nonce length".into()))?;
        let nonce = aead::Nonce::assume_unique_for_key(nonce_bytes);
        let aad = aead::Aad::from(b"gratia-wallet-v2");

        let mut in_out = ekm.ciphertext.clone();
        let plaintext = opening_key
            .open_in_place(nonce, aad, &mut in_out)
            .map_err(|_| GratiaError::Other(
                "keystore file corrupted or tampered: AES-GCM authentication failed".into()
            ))?;

        Ok(plaintext.to_vec())
    }

    /// Legacy decryption for old XOR-encrypted key files.
    ///
    /// Exists solely for migration: if an old-format file is loaded, it is
    /// decrypted here and then re-saved in the new AES-GCM format by the
    /// caller.
    fn decrypt_key_material_legacy(ekm: &EncryptedKeyMaterial) -> Result<Vec<u8>, GratiaError> {
        use sha2::{Sha256, Digest};

        if ekm.ciphertext.len() != 32 {
            return Err(GratiaError::Other("invalid legacy ciphertext length".into()));
        }

        let enc_key = Self::derive_encryption_key(&ekm.salt);
        let mut hasher = Sha256::new();
        hasher.update(&enc_key);
        hasher.update(&ekm.nonce);
        let keystream = hasher.finalize();

        let mut plaintext = vec![0u8; 32];
        for i in 0..32 {
            plaintext[i] = ekm.ciphertext[i] ^ keystream[i];
        }

        Ok(plaintext)
    }

    /// Write the secret key to disk as encrypted EncryptedKeyMaterial JSON.
    fn save_key(path: &str, key: &SigningKey) -> Result<(), GratiaError> {
        // WHY: Create parent directories if they don't exist. On first launch
        // the data directory may not have been fully created yet.
        if let Some(parent) = std::path::Path::new(path).parent() {
            std::fs::create_dir_all(parent).map_err(|e| {
                GratiaError::Other(format!("failed to create key directory: {}", e))
            })?;
        }

        // Derive the address for the EncryptedKeyMaterial metadata
        let address = Address::from_public_key(&key.verifying_key());

        let ekm = Self::encrypt_key_material(&key.to_bytes(), address);
        let json = serde_json::to_vec(&ekm).map_err(|e| {
            GratiaError::Other(format!("failed to serialize encrypted key: {}", e))
        })?;

        // Atomic write: write to a temp file then rename, so a crash mid-write
        // doesn't leave a corrupt key file.
        let tmp_path = format!("{}.tmp", path);
        std::fs::write(&tmp_path, &json).map_err(|e| {
            GratiaError::Other(format!("failed to write temp key file: {}", e))
        })?;
        std::fs::rename(&tmp_path, path).map_err(|e| {
            // Clean up temp file on rename failure.
            let _ = std::fs::remove_file(&tmp_path);
            GratiaError::Other(format!("failed to rename temp key file: {}", e))
        })
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

impl Keystore for FileKeystore {
    fn generate_keypair(&mut self) -> Result<Vec<u8>, GratiaError> {
        let signing_key = SigningKey::generate(&mut OsRng);
        let pubkey = signing_key.verifying_key().as_bytes().to_vec();
        Self::save_key(&self.key_path, &signing_key)?;
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

    fn export_secret_key(&self) -> Result<Zeroizing<Vec<u8>>, GratiaError> {
        let key = self.signing_key.as_ref().ok_or(GratiaError::WalletLocked)?;
        Ok(Zeroizing::new(key.to_bytes().to_vec()))
    }

    fn import_secret_key(&mut self, secret: &[u8]) -> Result<Vec<u8>, GratiaError> {
        let bytes: [u8; 32] = secret
            .try_into()
            .map_err(|_| GratiaError::Other("secret key must be exactly 32 bytes".into()))?;
        let signing_key = SigningKey::from_bytes(&bytes);
        let pubkey = signing_key.verifying_key().as_bytes().to_vec();
        Self::save_key(&self.key_path, &signing_key)?;
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

    // ========================================================================
    // FileKeystore tests
    // ========================================================================

    /// Generate a unique temp directory for each test to avoid collisions.
    fn temp_keystore_dir() -> String {
        use std::time::{SystemTime, UNIX_EPOCH};
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .subsec_nanos();
        let tid = std::thread::current().id();
        let dir = format!("/tmp/gratia_test_keystore_{:?}_{}", tid, nanos);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn cleanup_dir(dir: &str) {
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn test_file_keystore_save_and_load() {
        let dir = temp_keystore_dir();

        // Generate a key and save it
        let pubkey1 = {
            let mut ks = FileKeystore::new(&dir);
            assert!(!ks.has_keypair());
            let pk = ks.generate_keypair().unwrap();
            assert!(ks.has_keypair());
            pk
        };

        // Create a new FileKeystore at the same path — should load the saved key
        let ks2 = FileKeystore::new(&dir);
        assert!(ks2.has_keypair());
        let pubkey2 = ks2.public_key_bytes().unwrap();

        assert_eq!(pubkey1, pubkey2);

        cleanup_dir(&dir);
    }

    #[test]
    fn test_file_keystore_no_file() {
        // Non-existent directory — has_keypair should return false
        let ks = FileKeystore::new("/tmp/gratia_test_keystore_nonexistent_dir_xyz");
        assert!(!ks.has_keypair());

        // Clean up in case the directory was somehow created
        let _ = std::fs::remove_dir_all("/tmp/gratia_test_keystore_nonexistent_dir_xyz");
    }

    #[test]
    fn test_file_keystore_roundtrip() {
        let dir = temp_keystore_dir();

        let message = b"roundtrip verification message";

        // Generate, sign
        let (pubkey, sig) = {
            let mut ks = FileKeystore::new(&dir);
            let pk = ks.generate_keypair().unwrap();
            let s = ks.sign(message).unwrap();
            (pk, s)
        };

        // Load from file, sign again — signatures must match (Ed25519 is deterministic)
        {
            let ks2 = FileKeystore::new(&dir);
            assert!(ks2.has_keypair());

            let pubkey2 = ks2.public_key_bytes().unwrap();
            assert_eq!(pubkey, pubkey2);

            let sig2 = ks2.sign(message).unwrap();
            assert_eq!(sig, sig2);

            // Verify with the helper
            verify_signature(&pubkey, message, &sig2).unwrap();
        }

        cleanup_dir(&dir);
    }
}
