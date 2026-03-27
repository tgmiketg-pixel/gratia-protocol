//! gratia-state — Blockchain state management for the Gratia protocol.
//!
//! This crate handles all persistent state for the Gratia blockchain:
//! - Block and transaction storage via RocksDB (or in-memory for testing)
//! - Merkle tree state roots for integrity verification
//! - State pruning to keep the database within mobile storage limits (2-5 GB)
//! - Geographic sharding for horizontal scaling (~2,000 TPS across 10 shards)
//!
//! The storage layer uses a trait-based design (`StateStore`) so that the
//! RocksDB backend can be swapped for an in-memory backend on platforms
//! where RocksDB does not compile.

pub mod db;
pub mod merkle;
pub mod pruning;
pub mod sharding;

use std::sync::Arc;

use sha2::{Digest, Sha256};
use tracing;

use gratia_core::error::GratiaError;
use gratia_core::types::{
    Address, Block, BlockHash, Lux, Transaction, TransactionPayload, TxHash,
};

use crate::db::{AccountState, StateDb, StateStore};
use crate::merkle::MerkleTree;
use crate::pruning::PruningPolicy;

// ============================================================================
// StateManager
// ============================================================================

/// Top-level state manager that ties together storage, Merkle trees,
/// pruning, and block application.
///
/// This is the primary interface for the consensus layer to interact
/// with blockchain state. It ensures that state transitions are atomic
/// and consistent.
pub struct StateManager {
    /// Typed database access.
    db: StateDb,
    /// Pruning policy for mobile storage limits.
    pruning_policy: PruningPolicy,
}

impl StateManager {
    /// Create a new StateManager with the given store backend and default pruning policy.
    pub fn new(store: Arc<dyn StateStore>) -> Self {
        StateManager {
            db: StateDb::new(store),
            pruning_policy: PruningPolicy::default(),
        }
    }

    /// Create a new StateManager with a custom pruning policy.
    pub fn with_pruning_policy(
        store: Arc<dyn StateStore>,
        pruning_policy: PruningPolicy,
    ) -> Self {
        StateManager {
            db: StateDb::new(store),
            pruning_policy,
        }
    }

    /// Get a reference to the underlying StateDb for direct access.
    pub fn db(&self) -> &StateDb {
        &self.db
    }

    // ========================================================================
    // Block Application
    // ========================================================================

    /// Apply a block's transactions to state.
    ///
    /// This is the core state transition function. It:
    /// 1. Validates that the block follows the current chain tip
    /// 2. Applies each transaction to account state
    /// 3. Stores the block and its transactions
    /// 4. Updates the state root via Merkle tree
    /// 5. Advances the chain tip
    ///
    /// All changes are batched atomically — if any step fails, no state is modified.
    pub fn apply_block(&self, block: &Block) -> Result<(), GratiaError> {
        let current_height = self.db.get_block_height()?;
        let current_tip = self.db.get_chain_tip()?;

        // Validate chain continuity.
        // WHY: Genesis block must be height 1, all subsequent blocks must be
        // exactly current_height + 1. Without this check, an attacker could
        // submit a genesis block at any height on an empty chain.
        let expected_height = current_height + 1;
        if block.header.height != expected_height {
            return Err(GratiaError::BlockValidationFailed {
                reason: format!(
                    "expected height {}, got {}",
                    expected_height,
                    block.header.height
                ),
            });
        }

        // For non-genesis blocks, verify parent hash matches current tip.
        if current_height > 0 {
            if let Some(tip) = &current_tip {
                if block.header.parent_hash != *tip {
                    return Err(GratiaError::BlockValidationFailed {
                        reason: "parent hash does not match chain tip".to_string(),
                    });
                }
            }
        }

        // Apply each transaction to account state.
        for tx in &block.transactions {
            self.apply_transaction(tx)?;
        }

        // Store the block.
        self.db.put_block(block)?;

        // Store individual transactions for indexed lookup.
        for tx in &block.transactions {
            self.db.put_transaction(tx)?;
        }

        // Store attestations.
        for att in &block.attestations {
            self.db.put_attestation(att)?;
        }

        // Compute and store the new state root.
        let state_root = self.compute_state_root()?;
        self.db.set_state_root(&state_root)?;

        // Advance chain tip and height.
        let block_hash = block.header.hash()?;
        self.db.set_chain_tip(&block_hash)?;
        self.db.set_block_height(block.header.height)?;

        tracing::debug!(
            height = block.header.height,
            hash = %block_hash,
            txs = block.transactions.len(),
            "Applied block"
        );

        // Check if pruning is needed after applying the block.
        if pruning::should_prune(&self.db, &self.pruning_policy)? {
            tracing::info!("Database exceeds size target, running pruning cycle");
            pruning::run_pruning_cycle(&self.db, &self.pruning_policy, block.header.height)?;
        }

        Ok(())
    }

    /// Revert the most recent block (for chain reorganizations).
    ///
    /// This undoes the state changes from the block at the current chain tip.
    /// It restores account balances and nonces to their pre-block values.
    ///
    /// Note: This is a simplified implementation. A production version would
    /// store undo logs or use a journaling approach for efficient reversals.
    pub fn revert_block(&self) -> Result<(), GratiaError> {
        let current_height = self.db.get_block_height()?;
        if current_height == 0 {
            return Err(GratiaError::BlockValidationFailed {
                reason: "cannot revert genesis block".to_string(),
            });
        }

        let tip = self.db.get_chain_tip()?.ok_or_else(|| {
            GratiaError::StorageError("no chain tip found".to_string())
        })?;

        let block = self.db.get_block(&tip)?.ok_or_else(|| {
            GratiaError::StorageError("chain tip block not found in storage".to_string())
        })?;

        // Reverse each transaction in reverse order.
        for tx in block.transactions.iter().rev() {
            self.revert_transaction(tx)?;
        }

        // Remove the block's individual transactions from the index.
        for tx in &block.transactions {
            self.db.delete_transaction(&tx.hash)?;
        }

        // Delete the block itself.
        self.db.delete_block(&tip)?;

        // Restore the chain tip to the parent block.
        self.db.set_chain_tip(&block.header.parent_hash)?;
        self.db.set_block_height(current_height - 1)?;

        // Recompute the state root after reversal.
        let state_root = self.compute_state_root()?;
        self.db.set_state_root(&state_root)?;

        tracing::info!(
            reverted_height = current_height,
            new_tip = %block.header.parent_hash,
            "Reverted block"
        );

        Ok(())
    }

    // ========================================================================
    // Query Methods
    // ========================================================================

    /// Get the account state for an address.
    pub fn get_account(&self, address: &Address) -> Result<AccountState, GratiaError> {
        self.db.get_account(address)
    }

    /// Get the balance of an address in Lux.
    pub fn get_balance(&self, address: &Address) -> Result<Lux, GratiaError> {
        self.db.get_balance(address)
    }

    /// Get the nonce of an address.
    pub fn get_nonce(&self, address: &Address) -> Result<u64, GratiaError> {
        self.db.get_nonce(address)
    }

    /// Get a block by its hash.
    pub fn get_block(&self, hash: &BlockHash) -> Result<Option<Block>, GratiaError> {
        self.db.get_block(hash)
    }

    /// Get a block by its height.
    pub fn get_block_by_height(&self, height: u64) -> Result<Option<Block>, GratiaError> {
        self.db.get_block_by_height(height)
    }

    /// Get a transaction by its hash.
    pub fn get_transaction(&self, hash: &TxHash) -> Result<Option<Transaction>, GratiaError> {
        self.db.get_transaction(hash)
    }

    /// Get the current chain tip block hash.
    pub fn chain_tip(&self) -> Result<Option<BlockHash>, GratiaError> {
        self.db.get_chain_tip()
    }

    /// Get the current block height.
    pub fn block_height(&self) -> Result<u64, GratiaError> {
        self.db.get_block_height()
    }

    /// Get blocks in a height range (inclusive), stopping at the first gap.
    /// WHY: Used by the sync protocol to serve block ranges to peers.
    /// Caps at 50 blocks per call to bound response size for mobile.
    pub fn get_blocks_by_height_range(&self, from: u64, to: u64) -> Vec<Block> {
        let mut blocks = Vec::new();
        for height in from..=to.min(from + 49) {
            match self.get_block_by_height(height) {
                Ok(Some(block)) => blocks.push(block),
                _ => break,
            }
        }
        blocks
    }

    /// Get the current state root.
    pub fn state_root(&self) -> Result<[u8; 32], GratiaError> {
        self.db.get_state_root()
    }

    // ========================================================================
    // Internal Helpers
    // ========================================================================

    /// Apply a single transaction to account state.
    fn apply_transaction(&self, tx: &Transaction) -> Result<(), GratiaError> {
        let sender_address = address_from_pubkey(&tx.sender_pubkey)?;

        match &tx.payload {
            TransactionPayload::Transfer { to, amount } => {
                self.apply_transfer(&sender_address, to, *amount, tx.fee, tx.nonce)?;
            }
            TransactionPayload::Stake { amount } => {
                self.apply_stake(&sender_address, *amount, tx.fee, tx.nonce)?;
            }
            TransactionPayload::Unstake { amount } => {
                self.apply_unstake(&sender_address, *amount, tx.fee, tx.nonce)?;
            }
            // Other transaction types (contracts, governance, polls) affect state
            // through their respective crates. Here we only handle the fee and nonce.
            _ => {
                self.apply_fee_and_nonce(&sender_address, tx.fee, tx.nonce)?;
            }
        }

        Ok(())
    }

    /// Apply a standard transfer: debit sender, credit recipient, charge fee, bump nonce.
    fn apply_transfer(
        &self,
        sender: &Address,
        recipient: &Address,
        amount: Lux,
        fee: Lux,
        nonce: u64,
    ) -> Result<(), GratiaError> {
        let mut sender_acct = self.db.get_account(sender)?;
        let total_debit = amount
            .checked_add(fee)
            .ok_or_else(|| GratiaError::Other("transfer amount overflow".to_string()))?;

        if sender_acct.balance < total_debit {
            return Err(GratiaError::InsufficientBalance {
                available: sender_acct.balance,
                required: total_debit,
            });
        }

        if sender_acct.nonce != nonce {
            return Err(GratiaError::NonceMismatch {
                expected: sender_acct.nonce,
                got: nonce,
            });
        }

        sender_acct.balance -= total_debit;
        sender_acct.nonce += 1;
        self.db.put_account(sender, &sender_acct)?;

        // Credit recipient.
        let mut recipient_acct = self.db.get_account(recipient)?;
        recipient_acct.balance = recipient_acct
            .balance
            .checked_add(amount)
            .ok_or_else(|| GratiaError::Other("recipient balance overflow".to_string()))?;
        self.db.put_account(recipient, &recipient_acct)?;

        // WHY: Fees are burned (not given to validators) per the tokenomics design.
        // This makes GRAT deflationary. The fee simply disappears from total supply.

        Ok(())
    }

    /// Apply a stake operation: move balance to staked amount.
    fn apply_stake(
        &self,
        sender: &Address,
        amount: Lux,
        fee: Lux,
        nonce: u64,
    ) -> Result<(), GratiaError> {
        let mut acct = self.db.get_account(sender)?;
        let total_debit = amount
            .checked_add(fee)
            .ok_or_else(|| GratiaError::Other("stake amount overflow".to_string()))?;

        if acct.balance < total_debit {
            return Err(GratiaError::InsufficientBalance {
                available: acct.balance,
                required: total_debit,
            });
        }

        if acct.nonce != nonce {
            return Err(GratiaError::NonceMismatch {
                expected: acct.nonce,
                got: nonce,
            });
        }

        acct.balance -= total_debit;
        acct.staked = acct
            .staked
            .checked_add(amount)
            .ok_or_else(|| GratiaError::Other("staked amount overflow".to_string()))?;
        acct.nonce += 1;
        self.db.put_account(sender, &acct)?;

        Ok(())
    }

    /// Apply an unstake operation: move staked amount back to balance.
    fn apply_unstake(
        &self,
        sender: &Address,
        amount: Lux,
        fee: Lux,
        nonce: u64,
    ) -> Result<(), GratiaError> {
        let mut acct = self.db.get_account(sender)?;

        if acct.balance < fee {
            return Err(GratiaError::InsufficientBalance {
                available: acct.balance,
                required: fee,
            });
        }

        if acct.staked < amount {
            return Err(GratiaError::InsufficientStake {
                amount: acct.staked,
                required: amount,
            });
        }

        if acct.nonce != nonce {
            return Err(GratiaError::NonceMismatch {
                expected: acct.nonce,
                got: nonce,
            });
        }

        acct.balance = acct.balance - fee + amount;
        acct.staked -= amount;
        acct.nonce += 1;
        self.db.put_account(sender, &acct)?;

        Ok(())
    }

    /// Apply just the fee and nonce increment (for non-transfer transaction types).
    fn apply_fee_and_nonce(
        &self,
        sender: &Address,
        fee: Lux,
        nonce: u64,
    ) -> Result<(), GratiaError> {
        let mut acct = self.db.get_account(sender)?;

        if acct.balance < fee {
            return Err(GratiaError::InsufficientBalance {
                available: acct.balance,
                required: fee,
            });
        }

        if acct.nonce != nonce {
            return Err(GratiaError::NonceMismatch {
                expected: acct.nonce,
                got: nonce,
            });
        }

        acct.balance -= fee;
        acct.nonce += 1;
        self.db.put_account(sender, &acct)?;

        Ok(())
    }

    /// Reverse a single transaction (for block reversion).
    fn revert_transaction(&self, tx: &Transaction) -> Result<(), GratiaError> {
        let sender_address = address_from_pubkey(&tx.sender_pubkey)?;

        match &tx.payload {
            TransactionPayload::Transfer { to, amount } => {
                // Reverse: credit sender, debit recipient, refund fee, decrement nonce.
                let mut sender_acct = self.db.get_account(&sender_address)?;
                sender_acct.balance += amount + tx.fee;
                sender_acct.nonce -= 1;
                self.db.put_account(&sender_address, &sender_acct)?;

                let mut recipient_acct = self.db.get_account(to)?;
                recipient_acct.balance = recipient_acct.balance.saturating_sub(*amount);
                self.db.put_account(to, &recipient_acct)?;
            }
            TransactionPayload::Stake { amount } => {
                let mut acct = self.db.get_account(&sender_address)?;
                acct.balance += amount + tx.fee;
                acct.staked = acct.staked.saturating_sub(*amount);
                acct.nonce -= 1;
                self.db.put_account(&sender_address, &acct)?;
            }
            TransactionPayload::Unstake { amount } => {
                let mut acct = self.db.get_account(&sender_address)?;
                acct.balance = acct.balance.saturating_sub(*amount);
                acct.balance += tx.fee;
                acct.staked += amount;
                acct.nonce -= 1;
                self.db.put_account(&sender_address, &acct)?;
            }
            _ => {
                // Reverse fee and nonce only.
                let mut acct = self.db.get_account(&sender_address)?;
                acct.balance += tx.fee;
                acct.nonce -= 1;
                self.db.put_account(&sender_address, &acct)?;
            }
        }

        Ok(())
    }

    /// Compute the current state root from all account states.
    ///
    /// Builds a Merkle tree from the sorted list of (address, account_state) pairs.
    /// The root hash commits to the entire state at the current block height.
    fn compute_state_root(&self) -> Result<[u8; 32], GratiaError> {
        let accounts = self.db.store().iter_cf(db::CF_ACCOUNTS)?;

        if accounts.is_empty() {
            return Ok([0u8; 32]);
        }

        // Hash each (key, value) pair to create leaf hashes.
        let leaves: Vec<[u8; 32]> = accounts
            .iter()
            .map(|(k, v)| {
                let mut hasher = Sha256::new();
                hasher.update(k);
                hasher.update(v);
                let result = hasher.finalize();
                let mut hash = [0u8; 32];
                hash.copy_from_slice(&result);
                hash
            })
            .collect();

        let tree = MerkleTree::build(&leaves);
        Ok(tree.root())
    }
}

// ============================================================================
// Helper Functions
// ============================================================================

/// Derive an Address from a public key byte slice.
fn address_from_pubkey(pubkey_bytes: &[u8]) -> Result<Address, GratiaError> {
    if pubkey_bytes.len() != 32 {
        return Err(GratiaError::InvalidSignature);
    }
    let verifying_key = ed25519_dalek::VerifyingKey::from_bytes(
        pubkey_bytes
            .try_into()
            .map_err(|_| GratiaError::InvalidSignature)?,
    )
    .map_err(|_| GratiaError::InvalidSignature)?;
    Ok(Address::from_public_key(&verifying_key))
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::InMemoryStore;
    use chrono::Utc;
    use gratia_core::types::*;

    /// Helper: create a StateManager backed by in-memory storage.
    fn make_manager() -> StateManager {
        StateManager::new(Arc::new(InMemoryStore::new()))
    }

    /// Helper: create a keypair and derive the address (for test transactions).
    fn make_test_keypair() -> (ed25519_dalek::SigningKey, Address) {
        use ed25519_dalek::SigningKey;
        use rand::rngs::OsRng;

        let signing_key = SigningKey::generate(&mut OsRng);
        let verifying_key = signing_key.verifying_key();
        let address = Address::from_public_key(&verifying_key);
        (signing_key, address)
    }

    /// Helper: create a minimal valid block at the given height with the given transactions.
    fn make_block(
        height: u64,
        parent_hash: BlockHash,
        transactions: Vec<Transaction>,
    ) -> Block {
        let header = BlockHeader {
            height,
            timestamp: Utc::now(),
            parent_hash,
            transactions_root: [0u8; 32],
            state_root: [0u8; 32],
            attestations_root: [0u8; 32],
            producer: NodeId([0u8; 32]),
            vrf_proof: vec![],
            active_miners: 1,
            geographic_diversity: 1,
        };
        Block {
            header,
            transactions,
            attestations: vec![],
            validator_signatures: vec![],
        }
    }

    /// Helper: create a transfer transaction.
    fn make_transfer_tx(
        signing_key: &ed25519_dalek::SigningKey,
        to: Address,
        amount: Lux,
        fee: Lux,
        nonce: u64,
    ) -> Transaction {
        use ed25519_dalek::Signer;

        let payload = TransactionPayload::Transfer { to, amount };
        let payload_bytes = bincode::serialize(&payload).unwrap();
        let signature = signing_key.sign(&payload_bytes);

        let mut hasher = Sha256::new();
        hasher.update(&payload_bytes);
        hasher.update(&nonce.to_be_bytes());
        let hash_result = hasher.finalize();
        let mut tx_hash = [0u8; 32];
        tx_hash.copy_from_slice(&hash_result);

        Transaction {
            hash: TxHash(tx_hash),
            payload,
            sender_pubkey: signing_key.verifying_key().as_bytes().to_vec(),
            signature: signature.to_bytes().to_vec(),
            nonce,
            fee,
            timestamp: Utc::now(),
        }
    }

    #[test]
    fn test_initial_state() {
        let mgr = make_manager();
        assert_eq!(mgr.block_height().unwrap(), 0);
        assert_eq!(mgr.chain_tip().unwrap(), None);
        assert_eq!(mgr.state_root().unwrap(), [0u8; 32]);
    }

    #[test]
    fn test_get_default_account() {
        let mgr = make_manager();
        let addr = Address([1u8; 32]);
        let acct = mgr.get_account(&addr).unwrap();
        assert_eq!(acct.balance, 0);
        assert_eq!(acct.nonce, 0);
    }

    #[test]
    fn test_apply_genesis_block() {
        let mgr = make_manager();

        // Genesis block has no transactions and height 1.
        let genesis = make_block(1, BlockHash([0u8; 32]), vec![]);
        mgr.apply_block(&genesis).unwrap();

        assert_eq!(mgr.block_height().unwrap(), 1);
        assert!(mgr.chain_tip().unwrap().is_some());
    }

    #[test]
    fn test_apply_block_with_transfer() {
        let mgr = make_manager();

        let (sender_key, sender_addr) = make_test_keypair();
        let (_, recipient_addr) = make_test_keypair();

        // Pre-fund the sender account.
        let sender_acct = AccountState {
            balance: 10_000_000, // 10 GRAT
            nonce: 0,
            ..Default::default()
        };
        mgr.db().put_account(&sender_addr, &sender_acct).unwrap();

        // Create a transfer transaction.
        let tx = make_transfer_tx(
            &sender_key,
            recipient_addr,
            5_000_000, // 5 GRAT
            1_000,     // 0.001 GRAT fee
            0,         // nonce
        );

        let block = make_block(1, BlockHash([0u8; 32]), vec![tx]);
        mgr.apply_block(&block).unwrap();

        // Verify balances.
        let sender_balance = mgr.get_balance(&sender_addr).unwrap();
        assert_eq!(sender_balance, 10_000_000 - 5_000_000 - 1_000);

        let recipient_balance = mgr.get_balance(&recipient_addr).unwrap();
        assert_eq!(recipient_balance, 5_000_000);

        // Sender nonce should be incremented.
        assert_eq!(mgr.get_nonce(&sender_addr).unwrap(), 1);
    }

    #[test]
    fn test_apply_block_insufficient_balance() {
        let mgr = make_manager();

        let (sender_key, sender_addr) = make_test_keypair();
        let (_, recipient_addr) = make_test_keypair();

        // Sender has only 1 GRAT.
        let sender_acct = AccountState {
            balance: 1_000_000,
            nonce: 0,
            ..Default::default()
        };
        mgr.db().put_account(&sender_addr, &sender_acct).unwrap();

        // Try to transfer 5 GRAT.
        let tx = make_transfer_tx(&sender_key, recipient_addr, 5_000_000, 1_000, 0);
        let block = make_block(1, BlockHash([0u8; 32]), vec![tx]);

        let result = mgr.apply_block(&block);
        assert!(result.is_err());
        match result.unwrap_err() {
            GratiaError::InsufficientBalance { .. } => {}
            e => panic!("expected InsufficientBalance, got {:?}", e),
        }
    }

    #[test]
    fn test_apply_block_wrong_nonce() {
        let mgr = make_manager();

        let (sender_key, sender_addr) = make_test_keypair();
        let (_, recipient_addr) = make_test_keypair();

        let sender_acct = AccountState {
            balance: 10_000_000,
            nonce: 5, // Account nonce is 5
            ..Default::default()
        };
        mgr.db().put_account(&sender_addr, &sender_acct).unwrap();

        // Transaction uses nonce 0 (wrong).
        let tx = make_transfer_tx(&sender_key, recipient_addr, 1_000_000, 1_000, 0);
        let block = make_block(1, BlockHash([0u8; 32]), vec![tx]);

        let result = mgr.apply_block(&block);
        assert!(result.is_err());
        match result.unwrap_err() {
            GratiaError::NonceMismatch { expected: 5, got: 0 } => {}
            e => panic!("expected NonceMismatch, got {:?}", e),
        }
    }

    #[test]
    fn test_apply_and_revert_block() {
        let mgr = make_manager();

        let (sender_key, sender_addr) = make_test_keypair();
        let (_, recipient_addr) = make_test_keypair();

        // Pre-fund sender.
        let initial_balance = 10_000_000u64;
        let sender_acct = AccountState {
            balance: initial_balance,
            nonce: 0,
            ..Default::default()
        };
        mgr.db().put_account(&sender_addr, &sender_acct).unwrap();

        // Apply a block with a transfer.
        let amount = 3_000_000u64;
        let fee = 1_000u64;
        let tx = make_transfer_tx(&sender_key, recipient_addr, amount, fee, 0);
        let block = make_block(1, BlockHash([0u8; 32]), vec![tx]);
        mgr.apply_block(&block).unwrap();

        // Verify post-apply state.
        assert_eq!(
            mgr.get_balance(&sender_addr).unwrap(),
            initial_balance - amount - fee
        );
        assert_eq!(mgr.get_balance(&recipient_addr).unwrap(), amount);
        assert_eq!(mgr.get_nonce(&sender_addr).unwrap(), 1);
        assert_eq!(mgr.block_height().unwrap(), 1);

        // Revert the block.
        mgr.revert_block().unwrap();

        // Verify state is restored.
        assert_eq!(mgr.get_balance(&sender_addr).unwrap(), initial_balance);
        assert_eq!(mgr.get_balance(&recipient_addr).unwrap(), 0);
        assert_eq!(mgr.get_nonce(&sender_addr).unwrap(), 0);
        assert_eq!(mgr.block_height().unwrap(), 0);
    }

    #[test]
    fn test_state_root_changes_with_state() {
        let mgr = make_manager();

        let root_empty = mgr.state_root().unwrap();

        // Add an account and recompute.
        let addr = Address([7u8; 32]);
        let acct = AccountState {
            balance: 42,
            ..Default::default()
        };
        mgr.db().put_account(&addr, &acct).unwrap();

        // Manually trigger state root computation via a block.
        let block = make_block(1, BlockHash([0u8; 32]), vec![]);
        mgr.apply_block(&block).unwrap();

        let root_with_account = mgr.state_root().unwrap();
        assert_ne!(root_empty, root_with_account);
    }

    #[test]
    fn test_revert_genesis_fails() {
        let mgr = make_manager();
        let result = mgr.revert_block();
        assert!(result.is_err());
    }

    #[test]
    fn test_block_retrieval() {
        let mgr = make_manager();

        let block = make_block(1, BlockHash([0u8; 32]), vec![]);
        let block_hash = block.header.hash().unwrap();
        mgr.apply_block(&block).unwrap();

        // Retrieve by hash.
        let retrieved = mgr.get_block(&block_hash).unwrap();
        assert!(retrieved.is_some());
        assert_eq!(retrieved.unwrap().header.height, 1);

        // Retrieve by height.
        let retrieved = mgr.get_block_by_height(1).unwrap();
        assert!(retrieved.is_some());
    }

    #[test]
    fn test_stake_and_unstake() {
        let mgr = make_manager();

        let (sender_key, sender_addr) = make_test_keypair();

        // Pre-fund.
        let acct = AccountState {
            balance: 10_000_000,
            nonce: 0,
            ..Default::default()
        };
        mgr.db().put_account(&sender_addr, &acct).unwrap();

        // Stake transaction.
        let stake_payload = TransactionPayload::Stake {
            amount: 5_000_000,
        };
        let payload_bytes = bincode::serialize(&stake_payload).unwrap();
        let signature = {
            use ed25519_dalek::Signer;
            sender_key.sign(&payload_bytes)
        };
        let mut hasher = Sha256::new();
        hasher.update(&payload_bytes);
        let hash_result = hasher.finalize();
        let mut tx_hash = [0u8; 32];
        tx_hash.copy_from_slice(&hash_result);

        let stake_tx = Transaction {
            hash: TxHash(tx_hash),
            payload: stake_payload,
            sender_pubkey: sender_key.verifying_key().as_bytes().to_vec(),
            signature: signature.to_bytes().to_vec(),
            nonce: 0,
            fee: 1_000,
            timestamp: Utc::now(),
        };

        let block = make_block(1, BlockHash([0u8; 32]), vec![stake_tx]);
        mgr.apply_block(&block).unwrap();

        let acct = mgr.get_account(&sender_addr).unwrap();
        assert_eq!(acct.balance, 10_000_000 - 5_000_000 - 1_000);
        assert_eq!(acct.staked, 5_000_000);
        assert_eq!(acct.nonce, 1);
    }
}
