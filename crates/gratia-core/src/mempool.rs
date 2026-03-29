//! Validated transaction mempool for the Gratia protocol.
//!
//! Transactions entering the mempool are validated before acceptance:
//! signature verification, hash integrity, duplicate detection, and
//! capacity enforcement. This replaces the previous `Vec<Transaction>`
//! approach in the FFI layer which accepted transactions without any
//! pre-block validation.

use std::collections::HashSet;

use sha2::{Sha256, Digest};

use crate::crypto::verify_signature;
use crate::types::{Transaction, TxHash};

// ============================================================================
// Error Type
// ============================================================================

/// Errors that can occur when adding a transaction to the mempool.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MempoolError {
    /// Ed25519 signature verification failed — transaction is forged or corrupt.
    InvalidSignature,
    /// Transaction hash does not match the recomputed hash from its contents.
    InvalidHash,
    /// A transaction with this hash has already been seen (replay or duplicate).
    DuplicateTransaction,
    /// The mempool has reached its maximum capacity. The caller should retry
    /// after the next block drains some transactions.
    MempoolFull,
}

impl std::fmt::Display for MempoolError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            MempoolError::InvalidSignature => write!(f, "invalid transaction signature"),
            MempoolError::InvalidHash => write!(f, "transaction hash does not match content"),
            MempoolError::DuplicateTransaction => write!(f, "duplicate transaction"),
            MempoolError::MempoolFull => write!(f, "mempool is full"),
        }
    }
}

impl std::error::Error for MempoolError {}

// ============================================================================
// Validated Mempool
// ============================================================================

/// A mempool that validates transactions before accepting them.
///
/// Enforces:
/// - Ed25519 signature validity
/// - Hash integrity (recomputed hash must match `tx.hash`)
/// - No duplicates (tracked via `seen_hashes`)
/// - Capacity limit (prevents memory exhaustion on mobile devices)
pub struct ValidatedMempool {
    /// Validated transactions waiting for inclusion in the next block.
    pending: Vec<Transaction>,
    /// Set of all transaction hashes we have ever seen (accepted or confirmed).
    /// WHY: Prevents replay of transactions that were already included in a
    /// block or that we rejected as duplicates. Grows monotonically — in
    /// production this should be pruned periodically, but for Phase 1 the
    /// bounded mempool size keeps memory reasonable.
    seen_hashes: HashSet<[u8; 32]>,
    /// Maximum number of transactions allowed in the pending pool.
    /// WHY: Mobile devices have limited RAM. A $50 phone from 2018 may have
    /// only 2 GB. Capping the mempool prevents unbounded memory growth from
    /// transaction spam.
    max_size: usize,
}

/// Default mempool capacity.
/// WHY: 1000 transactions at ~250 bytes each = ~250 KB, well within the
/// memory budget of even low-end phones. At 3-5 second block times and
/// 512 tx/block, this holds roughly 2 blocks worth of transactions.
const DEFAULT_MAX_SIZE: usize = 1000;

impl ValidatedMempool {
    /// Create a new mempool with the specified maximum capacity.
    pub fn new(max_size: usize) -> Self {
        Self {
            pending: Vec::with_capacity(max_size.min(1024)),
            seen_hashes: HashSet::with_capacity(max_size.min(1024)),
            max_size,
        }
    }

    /// Add a transaction to the mempool after full validation.
    ///
    /// Validation order (cheapest checks first to fail fast):
    /// 1. Duplicate check (HashSet lookup — O(1))
    /// 2. Capacity check (comparison — O(1))
    /// 3. Hash integrity (SHA-256 — fast, no public key parsing)
    /// 4. Signature verification (Ed25519 verify — most expensive)
    pub fn add_transaction(&mut self, tx: Transaction) -> Result<(), MempoolError> {
        // 1. Check for duplicate — cheapest check, just a hash set lookup.
        if self.seen_hashes.contains(&tx.hash.0) {
            return Err(MempoolError::DuplicateTransaction);
        }

        // 2. Check capacity before doing expensive crypto work.
        if self.pending.len() >= self.max_size {
            return Err(MempoolError::MempoolFull);
        }

        // 3. Verify the transaction hash matches its contents.
        // WHY: A valid signature over tampered content could pass signature
        // verification but the hash would mismatch, catching content tampering
        // that preserves the original signature.
        let signable = compute_signable_bytes(&tx)?;
        let expected_hash = compute_tx_hash(&tx.sender_pubkey, &signable, &tx.signature);
        if tx.hash.0 != expected_hash.0 {
            return Err(MempoolError::InvalidHash);
        }

        // 4. Verify the Ed25519 signature over the signable content.
        // WHY: This is the most expensive check (~50-100us on ARM) so we
        // do it last to avoid wasting cycles on obviously bad transactions.
        verify_signature(&tx.sender_pubkey, &signable, &tx.signature)
            .map_err(|_| MempoolError::InvalidSignature)?;

        // All checks passed — accept the transaction.
        self.seen_hashes.insert(tx.hash.0);
        self.pending.push(tx);

        Ok(())
    }

    /// Remove and return up to `max_txs` transactions for block inclusion.
    ///
    /// Returns transactions in FIFO order (oldest first). The drained
    /// transactions remain in `seen_hashes` to prevent re-acceptance.
    pub fn drain_for_block(&mut self, max_txs: usize) -> Vec<Transaction> {
        let drain_count = self.pending.len().min(max_txs);
        self.pending.drain(..drain_count).collect()
    }

    /// Remove transactions that have been confirmed in a block.
    ///
    /// WHY: After a block is finalized, its transactions should be removed
    /// from the pending pool (if they are still there — e.g., a peer may
    /// have produced the block containing our pending transactions). The
    /// hashes stay in `seen_hashes` to prevent replaying confirmed
    /// transactions.
    pub fn remove_confirmed(&mut self, hashes: &[&[u8; 32]]) {
        if hashes.is_empty() {
            return;
        }

        let hash_set: HashSet<&[u8; 32]> = hashes.iter().copied().collect();
        self.pending.retain(|tx| !hash_set.contains(&tx.hash.0));
    }

    /// Number of transactions currently in the pending pool.
    pub fn len(&self) -> usize {
        self.pending.len()
    }

    /// Whether the pending pool is empty.
    pub fn is_empty(&self) -> bool {
        self.pending.is_empty()
    }

    /// Number of unique transaction hashes seen (includes confirmed/drained).
    pub fn seen_count(&self) -> usize {
        self.seen_hashes.len()
    }

    /// Maximum capacity of the pending pool.
    pub fn max_size(&self) -> usize {
        self.max_size
    }
}

impl Default for ValidatedMempool {
    fn default() -> Self {
        Self::new(DEFAULT_MAX_SIZE)
    }
}

// ============================================================================
// Internal Helpers
// ============================================================================

/// Produce the canonical signable byte sequence from a transaction.
///
/// Must match the signing logic in `gratia-wallet::transactions::signable_bytes`
/// exactly. The format is: bincode(payload) || nonce (LE) || chain_id (LE) || fee (LE) || timestamp_millis (LE).
///
/// WHY: Canonical serialization prevents signature malleability. Using bincode
/// gives deterministic byte order for the same logical payload. chain_id is
/// included to prevent cross-chain replay attacks (e.g., testnet tx replayed
/// on mainnet).
fn compute_signable_bytes(tx: &Transaction) -> Result<Vec<u8>, MempoolError> {
    let payload_bytes = bincode::serialize(&tx.payload)
        .map_err(|_| MempoolError::InvalidHash)?;

    // 8 (nonce) + 4 (chain_id) + 8 (fee) + 8 (timestamp) = 28 extra bytes
    let mut buf = Vec::with_capacity(payload_bytes.len() + 28);
    buf.extend_from_slice(&payload_bytes);
    buf.extend_from_slice(&tx.nonce.to_le_bytes());
    buf.extend_from_slice(&tx.chain_id.to_le_bytes());
    buf.extend_from_slice(&tx.fee.to_le_bytes());
    buf.extend_from_slice(&tx.timestamp.timestamp_millis().to_le_bytes());
    Ok(buf)
}

/// Compute the transaction hash: H(sender_pubkey || signable_content || signature).
///
/// Must match `gratia-wallet::transactions::compute_tx_hash` exactly.
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
    use crate::crypto::Keypair;
    use crate::types::{Address, TransactionPayload, LUX_PER_GRAT};
    use chrono::Utc;

    /// Build a valid signed transaction for testing.
    fn make_test_tx(keypair: &Keypair, nonce: u64, amount: u64) -> Transaction {
        let payload = TransactionPayload::Transfer {
            to: Address([42u8; 32]),
            amount,
        };
        let fee = 1000_u64;
        let timestamp = Utc::now();

        let chain_id: u32 = 2; // WHY: Testnet chain ID, must match compute_signable_bytes format.
        let payload_bytes = bincode::serialize(&payload).unwrap();
        let mut signable = Vec::with_capacity(payload_bytes.len() + 28);
        signable.extend_from_slice(&payload_bytes);
        signable.extend_from_slice(&nonce.to_le_bytes());
        signable.extend_from_slice(&chain_id.to_le_bytes());
        signable.extend_from_slice(&fee.to_le_bytes());
        signable.extend_from_slice(&timestamp.timestamp_millis().to_le_bytes());

        let sender_pubkey = keypair.public_key_bytes();
        let signature = keypair.sign(&signable);

        let mut hasher = Sha256::new();
        hasher.update(&sender_pubkey);
        hasher.update(&signable);
        hasher.update(&signature);
        let result = hasher.finalize();
        let mut hash = [0u8; 32];
        hash.copy_from_slice(&result);

        Transaction {
            hash: TxHash(hash),
            payload,
            sender_pubkey,
            signature,
            nonce,
            chain_id: 2, // WHY: Testnet chain ID. Matches gratia-wallet test default.
            fee,
            timestamp,
        }
    }

    #[test]
    fn test_add_valid_transaction() {
        let mut pool = ValidatedMempool::new(10);
        let kp = Keypair::generate();
        let tx = make_test_tx(&kp, 0, 5 * LUX_PER_GRAT);

        assert!(pool.add_transaction(tx).is_ok());
        assert_eq!(pool.len(), 1);
        assert!(!pool.is_empty());
    }

    #[test]
    fn test_reject_duplicate_transaction() {
        let mut pool = ValidatedMempool::new(10);
        let kp = Keypair::generate();
        let tx = make_test_tx(&kp, 0, 5 * LUX_PER_GRAT);
        let tx_clone = tx.clone();

        assert!(pool.add_transaction(tx).is_ok());
        assert_eq!(
            pool.add_transaction(tx_clone),
            Err(MempoolError::DuplicateTransaction)
        );
        assert_eq!(pool.len(), 1);
    }

    #[test]
    fn test_reject_when_full() {
        let mut pool = ValidatedMempool::new(2);
        let kp = Keypair::generate();

        let tx1 = make_test_tx(&kp, 0, 1_000_000);
        let tx2 = make_test_tx(&kp, 1, 2_000_000);
        let tx3 = make_test_tx(&kp, 2, 3_000_000);

        assert!(pool.add_transaction(tx1).is_ok());
        assert!(pool.add_transaction(tx2).is_ok());
        assert_eq!(pool.add_transaction(tx3), Err(MempoolError::MempoolFull));
        assert_eq!(pool.len(), 2);
    }

    #[test]
    fn test_reject_invalid_signature() {
        let mut pool = ValidatedMempool::new(10);
        let kp = Keypair::generate();
        let mut tx = make_test_tx(&kp, 0, 1_000_000);

        // Corrupt the signature
        tx.signature[0] ^= 0xFF;

        // WHY: The hash is computed over the original signature, so after
        // corrupting the signature the hash will also mismatch. This means
        // the hash check fires first. We recompute the hash to isolate the
        // signature check.
        let signable = compute_signable_bytes(&tx).unwrap();
        tx.hash = compute_tx_hash(&tx.sender_pubkey, &signable, &tx.signature);

        assert_eq!(
            pool.add_transaction(tx),
            Err(MempoolError::InvalidSignature)
        );
        assert_eq!(pool.len(), 0);
    }

    #[test]
    fn test_reject_invalid_hash() {
        let mut pool = ValidatedMempool::new(10);
        let kp = Keypair::generate();
        let mut tx = make_test_tx(&kp, 0, 1_000_000);

        // Corrupt the hash without changing anything else
        tx.hash.0[0] ^= 0xFF;

        assert_eq!(pool.add_transaction(tx), Err(MempoolError::InvalidHash));
        assert_eq!(pool.len(), 0);
    }

    #[test]
    fn test_drain_for_block() {
        let mut pool = ValidatedMempool::new(10);
        let kp = Keypair::generate();

        for i in 0..5 {
            let tx = make_test_tx(&kp, i, (i + 1) as u64 * LUX_PER_GRAT);
            pool.add_transaction(tx).unwrap();
        }
        assert_eq!(pool.len(), 5);

        // Drain 3 of the 5
        let drained = pool.drain_for_block(3);
        assert_eq!(drained.len(), 3);
        assert_eq!(pool.len(), 2);

        // Hashes should still be in seen set
        assert_eq!(pool.seen_count(), 5);
    }

    #[test]
    fn test_drain_more_than_available() {
        let mut pool = ValidatedMempool::new(10);
        let kp = Keypair::generate();

        let tx = make_test_tx(&kp, 0, LUX_PER_GRAT);
        pool.add_transaction(tx).unwrap();

        let drained = pool.drain_for_block(100);
        assert_eq!(drained.len(), 1);
        assert!(pool.is_empty());
    }

    #[test]
    fn test_remove_confirmed() {
        let mut pool = ValidatedMempool::new(10);
        let kp = Keypair::generate();

        let tx1 = make_test_tx(&kp, 0, 1_000_000);
        let tx2 = make_test_tx(&kp, 1, 2_000_000);
        let tx3 = make_test_tx(&kp, 2, 3_000_000);

        let hash1 = tx1.hash.0;
        let hash3 = tx3.hash.0;

        pool.add_transaction(tx1).unwrap();
        pool.add_transaction(tx2).unwrap();
        pool.add_transaction(tx3).unwrap();
        assert_eq!(pool.len(), 3);

        // Confirm tx1 and tx3
        pool.remove_confirmed(&[&hash1, &hash3]);
        assert_eq!(pool.len(), 1);

        // The remaining transaction should be tx2
        let remaining = pool.drain_for_block(10);
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].nonce, 1);
    }

    #[test]
    fn test_drained_tx_cannot_be_readded() {
        let mut pool = ValidatedMempool::new(10);
        let kp = Keypair::generate();

        let tx = make_test_tx(&kp, 0, LUX_PER_GRAT);
        let tx_clone = tx.clone();

        pool.add_transaction(tx).unwrap();
        let _ = pool.drain_for_block(1);
        assert!(pool.is_empty());

        // The hash is still in seen_hashes, so re-adding should fail
        assert_eq!(
            pool.add_transaction(tx_clone),
            Err(MempoolError::DuplicateTransaction)
        );
    }

    #[test]
    fn test_default_mempool() {
        let pool = ValidatedMempool::default();
        assert_eq!(pool.max_size(), DEFAULT_MAX_SIZE);
        assert!(pool.is_empty());
    }
}
