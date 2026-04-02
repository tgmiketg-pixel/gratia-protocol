//! Gratia Wallet — key management, transaction signing, and recovery.
//!
//! This crate provides the wallet layer for the Gratia protocol:
//!
//! - **keystore** — Ed25519 key generation, storage (trait-based for secure enclave),
//!   address/NodeId derivation, signing and verification.
//! - **transactions** — Build, sign, and verify Transfer/Stake/Unstake transactions.
//! - **recovery** — Behavioral matching recovery, optional seed phrase backup,
//!   optional inheritance with dead-man switch.
//!
//! The `WalletManager` struct ties these together into a high-level API
//! suitable for use by the mobile app layer via UniFFI.

pub mod keystore;
pub mod nfc;
pub mod transactions;
pub mod recovery;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tracing::{info, warn};

use gratia_core::error::GratiaError;
use gratia_core::types::{Address, Lux, Transaction, TransactionPayload};

use crate::keystore::{FileKeystore, Keystore, SoftwareKeystore};
use crate::recovery::{InheritanceConfig, RecoveryClaim, SeedPhrase};
use crate::transactions::TransactionBuilder;

// ============================================================================
// Wallet Manager
// ============================================================================

/// High-level wallet interface that ties together key management,
/// transaction building, and recovery.
///
/// In the mobile app, a single `WalletManager` instance is created at startup
/// and accessed through the UniFFI bridge.
pub struct WalletManager<K: Keystore = SoftwareKeystore> {
    keystore: K,
    /// Current transaction nonce. Incremented after each successful transaction.
    /// WHY: On-chain nonce tracking prevents replay attacks. The local counter
    /// is a best-effort cache — the true nonce comes from the network state.
    nonce: u64,
    /// Cached balance in Lux. Updated by sync with the network.
    /// WHY: Placeholder — real balance queries go through gratia-state.
    balance: Lux,
    /// Data directory for file-based persistence (balance, nonce).
    /// None for in-memory only (SoftwareKeystore in tests).
    data_dir: Option<String>,
    /// Transaction history (local cache).
    /// WHY: Placeholder — full history is stored in gratia-state / on-chain.
    history: Vec<TransactionRecord>,
    /// Active recovery claim against this wallet, if any.
    active_recovery: Option<RecoveryClaim>,
    /// Optional inheritance configuration.
    inheritance: Option<InheritanceConfig>,
}

/// A simplified transaction record for the local history cache.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TransactionRecord {
    pub hash: String,
    pub direction: TransactionDirection,
    pub amount: Lux,
    pub counterparty: Option<Address>,
    pub timestamp: DateTime<Utc>,
    pub status: TransactionStatus,
}

/// Whether a transaction was sent or received.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TransactionDirection {
    Sent,
    Received,
}

/// Confirmation status of a transaction.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TransactionStatus {
    /// Broadcast to the network, awaiting inclusion in a block.
    Pending,
    /// Included in a block and finalized.
    Confirmed,
    /// Transaction failed (insufficient balance, nonce conflict, etc.).
    Failed,
}

impl WalletManager<SoftwareKeystore> {
    /// Create a new WalletManager with a software keystore (dev/testing).
    pub fn new_software() -> Self {
        WalletManager {
            keystore: SoftwareKeystore::new(),
            nonce: 0,
            balance: 0,
            data_dir: None,
            history: Vec::new(),
            active_recovery: None,
            inheritance: None,
        }
    }
}

impl WalletManager<FileKeystore> {
    /// Create a WalletManager with file-backed persistence.
    ///
    /// WHY: On Android, the app's private data directory is sandboxed by the OS.
    /// Persisting the Ed25519 key to a file in that directory means the wallet
    /// address survives app restarts without requiring the secure enclave (which
    /// is the production target but not yet implemented).
    ///
    /// If a key file already exists at `{data_dir}/wallet_key.bin`, it is loaded
    /// automatically — the wallet is ready to use without calling `create_wallet()`.
    pub fn with_file_keystore(data_dir: &str) -> Self {
        let keystore = FileKeystore::new(data_dir);
        // Load persisted balance if it exists
        let balance = Self::load_balance(data_dir);
        WalletManager {
            keystore,
            nonce: 0,
            balance,
            data_dir: Some(data_dir.to_string()),
            history: Vec::new(),
            active_recovery: None,
            inheritance: None,
        }
    }

    /// Load persisted balance from file.
    fn load_balance(data_dir: &str) -> Lux {
        let path = format!("{}/wallet_balance.bin", data_dir);
        match std::fs::read(&path) {
            Ok(bytes) if bytes.len() == 8 => {
                let balance = u64::from_le_bytes(bytes.try_into().unwrap());
                tracing::info!(balance_lux = balance, "Loaded persisted balance");
                balance
            }
            _ => 0,
        }
    }

}

impl<K: Keystore> WalletManager<K> {
    /// Create a WalletManager with a custom keystore implementation.
    ///
    /// Used in production to inject the platform-specific secure enclave keystore.
    pub fn with_keystore(keystore: K) -> Self {
        WalletManager {
            keystore,
            nonce: 0,
            balance: 0,
            data_dir: None,
            history: Vec::new(),
            active_recovery: None,
            inheritance: None,
        }
    }

    /// Generate a new wallet keypair. Returns the wallet address.
    pub fn create_wallet(&mut self) -> Result<Address, GratiaError> {
        if self.keystore.has_keypair() {
            return Err(GratiaError::Other(
                "wallet already exists — use recovery to change devices".into(),
            ));
        }

        let pubkey = self.keystore.generate_keypair()?;
        let address = keystore::address_from_pubkey_bytes(&pubkey)?;
        info!("wallet created: {}", address);
        Ok(address)
    }

    /// Get the wallet address. Returns an error if no wallet has been created.
    pub fn address(&self) -> Result<Address, GratiaError> {
        let pubkey = self.keystore.public_key_bytes()?;
        keystore::address_from_pubkey_bytes(&pubkey)
    }

    /// Get the cached balance (in Lux).
    ///
    /// # Note
    /// This is a local cache. Call `sync_balance` to refresh from the network.
    pub fn balance(&self) -> Lux {
        self.balance
    }

    /// Get the current transaction nonce.
    pub fn nonce(&self) -> u64 {
        self.nonce
    }

    /// Set the nonce from on-chain state (used to sync after restart).
    pub fn set_nonce(&mut self, nonce: u64) {
        self.nonce = nonce;
    }

    /// Update the cached balance from network state.
    ///
    /// Placeholder — in production, this queries gratia-state for the
    /// confirmed balance at the wallet's address.
    pub fn sync_balance(&mut self, confirmed_balance: Lux) {
        self.balance = confirmed_balance;
        // WHY: Persist balance to disk so mining rewards survive app restarts.
        // In production, the balance comes from on-chain state. For Phase 1,
        // this file-based persistence prevents the frustrating UX of losing
        // mined GRAT on every restart.
        if self.data_dir.is_some() {
            self.save_balance_to_file();
        }
    }

    /// Save balance to file (called internally by sync_balance).
    fn save_balance_to_file(&self) {
        if let Some(ref dir) = self.data_dir {
            let path = format!("{}/wallet_balance.bin", dir);
            let bytes = self.balance.to_le_bytes();
            if let Err(e) = std::fs::write(&path, bytes) {
                tracing::warn!("Failed to persist balance: {}", e);
            }
        }
    }

    /// Update the nonce from network state.
    ///
    /// Placeholder — in production, this queries gratia-state for the
    /// latest confirmed nonce for this address.
    pub fn sync_nonce(&mut self, confirmed_nonce: u64) {
        self.nonce = confirmed_nonce;
    }

    /// Send a transfer transaction.
    ///
    /// Validates the balance locally (best-effort check), builds and signs
    /// the transaction, and returns it for broadcast to the network.
    pub fn send_transfer(
        &mut self,
        to: Address,
        amount: Lux,
        fee: Lux,
    ) -> Result<Transaction, GratiaError> {
        self.check_not_frozen()?;

        // WHY: Zero-amount transfers have no economic purpose and waste block
        // space. Reject them at the wallet layer before signing, rather than
        // only catching them during block validation.
        if amount == 0 {
            return Err(GratiaError::Other(
                "Transfer amount cannot be zero".into(),
            ));
        }

        let total_cost = amount.checked_add(fee).ok_or(GratiaError::Other(
            "amount + fee overflow".into(),
        ))?;

        if self.balance < total_cost {
            return Err(GratiaError::InsufficientBalance {
                available: self.balance,
                required: total_cost,
            });
        }

        let builder = TransactionBuilder::new(&self.keystore, self.nonce, fee);
        let tx = builder.build_transfer(to, amount)?;

        // WHY: Deduct from local cache and advance nonce. This is optimistic —
        // if the transaction is reverted (fork, network partition), the local
        // nonce will be out of sync with on-chain state. The FFI layer calls
        // sync_nonce_from_chain() after fork resolution to reconcile. Users
        // cannot send another transaction until the previous one is confirmed
        // or the nonce is re-synced from on-chain state.
        self.balance -= total_cost;
        self.nonce += 1;

        let hash_hex = hex::encode(&tx.hash.0);
        info!("transfer sent: {} -> {} ({} Lux)", hash_hex, to, amount);

        // WHY: In Phase 1 (no consensus engine / no block production), transactions
        // are confirmed immediately because there's no block to include them in.
        // The balance has already been deducted locally. In Phase 2, this will
        // revert to Pending and wait for block inclusion.
        self.history.push(TransactionRecord {
            hash: hash_hex,
            direction: TransactionDirection::Sent,
            amount,
            counterparty: Some(to),
            timestamp: tx.timestamp,
            status: TransactionStatus::Confirmed,
        });

        Ok(tx)
    }

    /// Send a stake transaction.
    pub fn send_stake(&mut self, amount: Lux, fee: Lux) -> Result<Transaction, GratiaError> {
        self.check_not_frozen()?;

        let total_cost = amount.checked_add(fee).ok_or(GratiaError::Other(
            "amount + fee overflow".into(),
        ))?;

        if self.balance < total_cost {
            return Err(GratiaError::InsufficientBalance {
                available: self.balance,
                required: total_cost,
            });
        }

        let builder = TransactionBuilder::new(&self.keystore, self.nonce, fee);
        let tx = builder.build_stake(amount)?;

        self.balance -= total_cost;
        self.nonce += 1;

        let hash_hex = hex::encode(&tx.hash.0);
        info!("stake sent: {} ({} Lux)", hash_hex, amount);

        self.history.push(TransactionRecord {
            hash: hash_hex,
            direction: TransactionDirection::Sent,
            amount,
            counterparty: None,
            timestamp: tx.timestamp,
            status: TransactionStatus::Confirmed,
        });

        Ok(tx)
    }

    /// Send an unstake transaction.
    pub fn send_unstake(&mut self, amount: Lux, fee: Lux) -> Result<Transaction, GratiaError> {
        self.check_not_frozen()?;

        if self.balance < fee {
            return Err(GratiaError::InsufficientBalance {
                available: self.balance,
                required: fee,
            });
        }

        let builder = TransactionBuilder::new(&self.keystore, self.nonce, fee);
        let tx = builder.build_unstake(amount)?;

        self.balance -= fee;
        self.nonce += 1;

        let hash_hex = hex::encode(&tx.hash.0);
        info!("unstake sent: {} ({} Lux)", hash_hex, amount);

        self.history.push(TransactionRecord {
            hash: hash_hex,
            direction: TransactionDirection::Sent,
            amount,
            counterparty: None,
            timestamp: tx.timestamp,
            status: TransactionStatus::Confirmed,
        });

        Ok(tx)
    }

    /// Build and sign an arbitrary transaction payload.
    ///
    /// For advanced use — governance proposals, contract calls, polls, etc.
    /// Does not perform balance checks (caller is responsible).
    pub fn sign_transaction(
        &mut self,
        payload: TransactionPayload,
        fee: Lux,
    ) -> Result<Transaction, GratiaError> {
        self.check_not_frozen()?;

        let builder = TransactionBuilder::new(&self.keystore, self.nonce, fee);
        let tx = builder.build_and_sign(payload)?;
        self.nonce += 1;
        Ok(tx)
    }

    /// Get the transaction history (local cache).
    pub fn history(&self) -> &[TransactionRecord] {
        &self.history
    }

    /// Record an incoming transfer received from the network.
    ///
    /// WHY: When a peer sends us GRAT via gossipsub, we credit the balance
    /// via `sync_balance`, but we also need a history entry so the UI shows
    /// "Received X GRAT" in the transaction list. Without this, the balance
    /// goes up but the user sees no explanation.
    pub fn record_incoming_transfer(
        &mut self,
        hash_hex: String,
        from: Address,
        amount: Lux,
        timestamp: chrono::DateTime<chrono::Utc>,
    ) {
        // WHY: Guard against duplicate history entries. The gossip layer
        // deduplicates at the network level, but this is a belt-and-suspenders
        // check in case the same tx reaches us through different code paths.
        if self.history.iter().any(|r| r.hash == hash_hex) {
            return;
        }
        self.history.push(TransactionRecord {
            hash: hash_hex,
            direction: TransactionDirection::Received,
            amount,
            counterparty: Some(from),
            timestamp,
            status: TransactionStatus::Confirmed,
        });
    }

    // --- Recovery ---

    /// Check if the wallet is currently frozen due to a recovery claim.
    pub fn is_frozen(&self) -> bool {
        self.active_recovery
            .as_ref()
            .map_or(false, |r| r.wallet_is_frozen())
    }

    /// Set an active recovery claim (received from the network).
    pub fn set_recovery_claim(&mut self, claim: RecoveryClaim) {
        warn!("recovery claim active on wallet {}", self.address().unwrap_or(Address([0u8; 32])));
        self.active_recovery = Some(claim);
    }

    /// Reject an active recovery claim from this (original) device.
    pub fn reject_recovery_claim(&mut self) -> Result<(), GratiaError> {
        let claim = self.active_recovery.as_mut().ok_or(GratiaError::Other(
            "no active recovery claim to reject".into(),
        ))?;
        claim.owner_reject()?;
        info!("recovery claim rejected by owner");
        Ok(())
    }

    /// Get the active recovery claim, if any.
    pub fn recovery_claim(&self) -> Option<&RecoveryClaim> {
        self.active_recovery.as_ref()
    }

    // --- Seed Phrase ---

    /// Generate a seed phrase backup from the current wallet's secret key.
    ///
    /// Only available on software keystores. Hardware enclave implementations
    /// will return `WalletLocked`.
    pub fn export_seed_phrase(&self) -> Result<SeedPhrase, GratiaError> {
        let secret = self.keystore.export_secret_key()?;
        SeedPhrase::from_secret_key(&secret)
    }

    /// Restore a wallet from a seed phrase.
    pub fn import_seed_phrase(&mut self, phrase: &SeedPhrase) -> Result<Address, GratiaError> {
        let pubkey = self.keystore.import_secret_key(phrase.to_secret_key_bytes())?;
        let address = keystore::address_from_pubkey_bytes(&pubkey)?;
        info!("wallet restored from seed phrase: {}", address);
        Ok(address)
    }

    // --- Inheritance ---

    /// Enable inheritance with a beneficiary address.
    pub fn set_inheritance(&mut self, beneficiary: Address) -> Result<(), GratiaError> {
        self.inheritance = Some(InheritanceConfig::new(beneficiary));
        info!("inheritance enabled for beneficiary: {}", beneficiary);
        Ok(())
    }

    /// Enable inheritance with a custom timeout.
    pub fn set_inheritance_with_timeout(
        &mut self,
        beneficiary: Address,
        timeout_days: u32,
    ) -> Result<(), GratiaError> {
        self.inheritance =
            Some(InheritanceConfig::with_timeout(beneficiary, timeout_days)?);
        info!(
            "inheritance enabled for beneficiary: {} (timeout: {} days)",
            beneficiary, timeout_days
        );
        Ok(())
    }

    /// Disable inheritance.
    pub fn clear_inheritance(&mut self) {
        self.inheritance = None;
        info!("inheritance disabled");
    }

    /// Get the inheritance configuration, if set.
    pub fn inheritance(&self) -> Option<&InheritanceConfig> {
        self.inheritance.as_ref()
    }

    /// Export the raw signing key as 32 bytes.
    ///
    /// WHY: Needed by the consensus engine to derive the VRF secret key
    /// for block producer selection. Returns an error if no wallet exists
    /// or if the keystore doesn't support key export (hardware enclaves).
    pub fn signing_key_bytes(&self) -> Result<[u8; 32], GratiaError> {
        let bytes = self.keystore.export_secret_key()?;
        let arr: [u8; 32] = bytes.as_slice()
            .try_into()
            .map_err(|_| GratiaError::Other("signing key is not 32 bytes".into()))?;
        Ok(arr)
    }

    /// Record a Proof of Life event to reset the dead-man switch.
    pub fn record_proof_of_life(&mut self) {
        if let Some(ref mut config) = self.inheritance {
            config.record_proof_of_life();
        }
    }

    // --- Internal Helpers ---

    /// Block operations if the wallet is frozen due to a recovery claim.
    fn check_not_frozen(&self) -> Result<(), GratiaError> {
        if self.is_frozen() {
            return Err(GratiaError::RecoveryClaimPending);
        }
        Ok(())
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use gratia_core::types::LUX_PER_GRAT;

    #[test]
    fn test_create_wallet_and_get_address() {
        let mut wm = WalletManager::new_software();
        let addr = wm.create_wallet().unwrap();
        let addr2 = wm.address().unwrap();
        assert_eq!(addr, addr2);
    }

    #[test]
    fn test_cannot_create_wallet_twice() {
        let mut wm = WalletManager::new_software();
        wm.create_wallet().unwrap();
        let result = wm.create_wallet();
        assert!(result.is_err());
    }

    #[test]
    fn test_send_transfer_deducts_balance() {
        let mut wm = WalletManager::new_software();
        wm.create_wallet().unwrap();
        wm.sync_balance(10 * LUX_PER_GRAT);

        let recipient = Address([42u8; 32]);
        let fee = 1000;
        let amount = 5 * LUX_PER_GRAT;

        let tx = wm.send_transfer(recipient, amount, fee).unwrap();
        transactions::verify_transaction(&tx).unwrap();

        assert_eq!(wm.balance(), 10 * LUX_PER_GRAT - amount - fee);
        assert_eq!(wm.history().len(), 1);
    }

    #[test]
    fn test_send_transfer_insufficient_balance() {
        let mut wm = WalletManager::new_software();
        wm.create_wallet().unwrap();
        wm.sync_balance(1000); // Very small balance

        let result = wm.send_transfer(Address([1u8; 32]), 5 * LUX_PER_GRAT, 100);
        assert!(result.is_err());
    }

    #[test]
    fn test_send_stake_and_unstake() {
        let mut wm = WalletManager::new_software();
        wm.create_wallet().unwrap();
        wm.sync_balance(200 * LUX_PER_GRAT);

        let stake_tx = wm.send_stake(100 * LUX_PER_GRAT, 500).unwrap();
        transactions::verify_transaction(&stake_tx).unwrap();

        let unstake_tx = wm.send_unstake(50 * LUX_PER_GRAT, 500).unwrap();
        transactions::verify_transaction(&unstake_tx).unwrap();

        assert_eq!(wm.history().len(), 2);
    }

    #[test]
    fn test_frozen_wallet_blocks_transactions() {
        let mut wm = WalletManager::new_software();
        wm.create_wallet().unwrap();
        wm.sync_balance(10 * LUX_PER_GRAT);

        // Set a recovery claim — wallet should freeze
        let claim = RecoveryClaim::new(wm.address().unwrap(), Address([99u8; 32]));
        wm.set_recovery_claim(claim);

        assert!(wm.is_frozen());

        let result = wm.send_transfer(Address([1u8; 32]), 1_000_000, 100);
        assert!(result.is_err());
        match result.unwrap_err() {
            GratiaError::RecoveryClaimPending => {}
            other => panic!("expected RecoveryClaimPending, got {:?}", other),
        }
    }

    #[test]
    fn test_reject_recovery_unfreezes_wallet() {
        let mut wm = WalletManager::new_software();
        wm.create_wallet().unwrap();
        wm.sync_balance(10 * LUX_PER_GRAT);

        let claim = RecoveryClaim::new(wm.address().unwrap(), Address([99u8; 32]));
        wm.set_recovery_claim(claim);
        assert!(wm.is_frozen());

        wm.reject_recovery_claim().unwrap();
        assert!(!wm.is_frozen());

        // Transactions should work again
        let tx = wm.send_transfer(Address([1u8; 32]), 1_000_000, 100).unwrap();
        transactions::verify_transaction(&tx).unwrap();
    }

    #[test]
    fn test_seed_phrase_export_and_import() {
        let mut wm1 = WalletManager::new_software();
        let addr1 = wm1.create_wallet().unwrap();

        let phrase = wm1.export_seed_phrase().unwrap();

        // Import into a fresh wallet
        let mut wm2 = WalletManager::new_software();
        let addr2 = wm2.import_seed_phrase(&phrase).unwrap();

        assert_eq!(addr1, addr2);
    }

    #[test]
    fn test_inheritance_setup_and_proof_of_life() {
        let mut wm = WalletManager::new_software();
        wm.create_wallet().unwrap();

        assert!(wm.inheritance().is_none());

        wm.set_inheritance(Address([88u8; 32])).unwrap();
        assert!(wm.inheritance().is_some());
        assert!(!wm.inheritance().unwrap().is_triggered());

        wm.record_proof_of_life();
        assert!(wm.inheritance().unwrap().days_remaining() > 360);

        wm.clear_inheritance();
        assert!(wm.inheritance().is_none());
    }

    #[test]
    fn test_nonce_increments_per_transaction() {
        let mut wm = WalletManager::new_software();
        wm.create_wallet().unwrap();
        wm.sync_balance(100 * LUX_PER_GRAT);

        let tx1 = wm.send_transfer(Address([1u8; 32]), 1_000_000, 100).unwrap();
        assert_eq!(tx1.nonce, 0);

        let tx2 = wm.send_transfer(Address([1u8; 32]), 1_000_000, 100).unwrap();
        assert_eq!(tx2.nonce, 1);

        let tx3 = wm.send_stake(1_000_000, 100).unwrap();
        assert_eq!(tx3.nonce, 2);
    }

    #[test]
    fn test_sync_nonce_overrides_local() {
        let mut wm = WalletManager::new_software();
        wm.create_wallet().unwrap();
        wm.sync_balance(100 * LUX_PER_GRAT);

        wm.send_transfer(Address([1u8; 32]), 1_000_000, 100).unwrap();
        wm.send_transfer(Address([1u8; 32]), 1_000_000, 100).unwrap();

        // Network says the confirmed nonce is 5 (e.g., after a state sync)
        wm.sync_nonce(5);

        let tx = wm.send_transfer(Address([1u8; 32]), 1_000_000, 100).unwrap();
        assert_eq!(tx.nonce, 5);
    }
}
