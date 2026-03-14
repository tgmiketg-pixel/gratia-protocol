//! State storage abstraction layer.
//!
//! Provides a trait-based approach to state storage so that the protocol can
//! use RocksDB on devices that support it, while falling back to an in-memory
//! store for testing and development environments where RocksDB may not compile.

use std::collections::BTreeMap;
use std::sync::{Arc, RwLock};

use serde::{Deserialize, Serialize};

use gratia_core::error::GratiaError;
use gratia_core::types::{
    Address, Block, BlockHash, Lux, NodeId, ProofOfLifeAttestation, Transaction, TxHash,
};

// ============================================================================
// Column Family Names
// ============================================================================

/// Column family for block headers and bodies.
pub const CF_BLOCKS: &str = "blocks";
/// Column family for transactions indexed by hash.
pub const CF_TRANSACTIONS: &str = "transactions";
/// Column family for account state (balance, nonce, staking, PoL status).
pub const CF_ACCOUNTS: &str = "accounts";
/// Column family for general protocol state (e.g., chain tip, config).
pub const CF_STATE: &str = "state";
/// Column family for Proof of Life attestations.
pub const CF_ATTESTATIONS: &str = "attestations";

/// All column families used by the state database.
pub const ALL_COLUMN_FAMILIES: &[&str] = &[
    CF_BLOCKS,
    CF_TRANSACTIONS,
    CF_ACCOUNTS,
    CF_STATE,
    CF_ATTESTATIONS,
];

// ============================================================================
// Well-Known State Keys
// ============================================================================

/// Key for the current chain tip block hash in the STATE column family.
pub const STATE_KEY_CHAIN_TIP: &[u8] = b"chain_tip";
/// Key for the current block height in the STATE column family.
pub const STATE_KEY_BLOCK_HEIGHT: &[u8] = b"block_height";
/// Key for the current state root in the STATE column family.
pub const STATE_KEY_STATE_ROOT: &[u8] = b"state_root";

// ============================================================================
// Account State
// ============================================================================

/// On-chain account state stored in the accounts column family.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AccountState {
    /// Balance in Lux (1 GRAT = 1,000,000 Lux).
    pub balance: Lux,
    /// Transaction nonce — incremented with each outgoing transaction to prevent replay.
    pub nonce: u64,
    /// Amount currently staked for mining eligibility.
    pub staked: Lux,
    /// Amount that overflowed to the Network Security Pool (above per-node cap).
    pub overflow_stake: Lux,
    /// Whether this account has a valid Proof of Life for the current day.
    pub pol_valid: bool,
    /// Consecutive days of valid Proof of Life (for governance eligibility checks).
    pub pol_consecutive_days: u64,
    /// Timestamp of last PoL attestation.
    pub last_pol_date: Option<chrono::NaiveDate>,
    /// Node ID associated with this account (if this is a mining node).
    pub node_id: Option<NodeId>,
}

impl Default for AccountState {
    fn default() -> Self {
        AccountState {
            balance: 0,
            nonce: 0,
            staked: 0,
            overflow_stake: 0,
            pol_valid: false,
            pol_consecutive_days: 0,
            last_pol_date: None,
            node_id: None,
        }
    }
}

// ============================================================================
// StateStore Trait
// ============================================================================

/// Abstraction over the key-value state storage backend.
///
/// All state operations go through this trait, allowing swappable backends:
/// - `InMemoryStore` for testing (always compiles)
/// - `RocksDbStore` for production (requires rocksdb feature)
pub trait StateStore: Send + Sync {
    /// Put a key-value pair into the specified column family.
    fn put(&self, cf: &str, key: &[u8], value: &[u8]) -> Result<(), GratiaError>;

    /// Get a value by key from the specified column family.
    fn get(&self, cf: &str, key: &[u8]) -> Result<Option<Vec<u8>>, GratiaError>;

    /// Delete a key from the specified column family.
    fn delete(&self, cf: &str, key: &[u8]) -> Result<(), GratiaError>;

    /// Atomically write a batch of operations.
    /// Each operation is (column_family, key, optional_value).
    /// If value is None, the key is deleted; if Some, it is put.
    fn batch_write(
        &self,
        operations: Vec<(String, Vec<u8>, Option<Vec<u8>>)>,
    ) -> Result<(), GratiaError>;

    /// Iterate over all key-value pairs in a column family.
    /// Returns pairs in key order.
    fn iter_cf(&self, cf: &str) -> Result<Vec<(Vec<u8>, Vec<u8>)>, GratiaError>;

    /// Count the number of keys in a column family.
    fn count_keys(&self, cf: &str) -> Result<u64, GratiaError>;

    /// Estimate the total size of all data in bytes.
    /// This is an approximation — exact size depends on the backend.
    fn estimate_size_bytes(&self) -> Result<u64, GratiaError>;
}

// ============================================================================
// InMemoryStore — always-available testing backend
// ============================================================================

/// In-memory state store for testing and development.
/// Data is lost when the process exits. Thread-safe via RwLock.
pub struct InMemoryStore {
    /// Map from column family name to sorted key-value store.
    data: RwLock<BTreeMap<String, BTreeMap<Vec<u8>, Vec<u8>>>>,
}

impl InMemoryStore {
    /// Create a new empty in-memory store with all column families initialized.
    pub fn new() -> Self {
        let mut data = BTreeMap::new();
        for cf in ALL_COLUMN_FAMILIES {
            data.insert(cf.to_string(), BTreeMap::new());
        }
        InMemoryStore {
            data: RwLock::new(data),
        }
    }
}

impl Default for InMemoryStore {
    fn default() -> Self {
        Self::new()
    }
}

impl StateStore for InMemoryStore {
    fn put(&self, cf: &str, key: &[u8], value: &[u8]) -> Result<(), GratiaError> {
        let mut data = self.data.write().map_err(|e| {
            GratiaError::StorageError(format!("lock poisoned: {}", e))
        })?;
        let cf_map = data
            .get_mut(cf)
            .ok_or_else(|| GratiaError::StorageError(format!("unknown column family: {}", cf)))?;
        cf_map.insert(key.to_vec(), value.to_vec());
        Ok(())
    }

    fn get(&self, cf: &str, key: &[u8]) -> Result<Option<Vec<u8>>, GratiaError> {
        let data = self.data.read().map_err(|e| {
            GratiaError::StorageError(format!("lock poisoned: {}", e))
        })?;
        let cf_map = data
            .get(cf)
            .ok_or_else(|| GratiaError::StorageError(format!("unknown column family: {}", cf)))?;
        Ok(cf_map.get(key).cloned())
    }

    fn delete(&self, cf: &str, key: &[u8]) -> Result<(), GratiaError> {
        let mut data = self.data.write().map_err(|e| {
            GratiaError::StorageError(format!("lock poisoned: {}", e))
        })?;
        let cf_map = data
            .get_mut(cf)
            .ok_or_else(|| GratiaError::StorageError(format!("unknown column family: {}", cf)))?;
        cf_map.remove(key);
        Ok(())
    }

    fn batch_write(
        &self,
        operations: Vec<(String, Vec<u8>, Option<Vec<u8>>)>,
    ) -> Result<(), GratiaError> {
        let mut data = self.data.write().map_err(|e| {
            GratiaError::StorageError(format!("lock poisoned: {}", e))
        })?;
        for (cf, key, value) in operations {
            let cf_map = data
                .get_mut(&cf)
                .ok_or_else(|| GratiaError::StorageError(format!("unknown column family: {}", cf)))?;
            match value {
                Some(v) => {
                    cf_map.insert(key, v);
                }
                None => {
                    cf_map.remove(&key);
                }
            }
        }
        Ok(())
    }

    fn iter_cf(&self, cf: &str) -> Result<Vec<(Vec<u8>, Vec<u8>)>, GratiaError> {
        let data = self.data.read().map_err(|e| {
            GratiaError::StorageError(format!("lock poisoned: {}", e))
        })?;
        let cf_map = data
            .get(cf)
            .ok_or_else(|| GratiaError::StorageError(format!("unknown column family: {}", cf)))?;
        Ok(cf_map.iter().map(|(k, v)| (k.clone(), v.clone())).collect())
    }

    fn count_keys(&self, cf: &str) -> Result<u64, GratiaError> {
        let data = self.data.read().map_err(|e| {
            GratiaError::StorageError(format!("lock poisoned: {}", e))
        })?;
        let cf_map = data
            .get(cf)
            .ok_or_else(|| GratiaError::StorageError(format!("unknown column family: {}", cf)))?;
        Ok(cf_map.len() as u64)
    }

    fn estimate_size_bytes(&self) -> Result<u64, GratiaError> {
        let data = self.data.read().map_err(|e| {
            GratiaError::StorageError(format!("lock poisoned: {}", e))
        })?;
        let mut total: u64 = 0;
        for cf_map in data.values() {
            for (k, v) in cf_map.iter() {
                total += k.len() as u64 + v.len() as u64;
            }
        }
        Ok(total)
    }
}

// ============================================================================
// RocksDbStore — production backend (feature-gated)
// ============================================================================

#[cfg(feature = "rocksdb-backend")]
pub struct RocksDbStore {
    db: rocksdb::DB,
}

#[cfg(feature = "rocksdb-backend")]
impl RocksDbStore {
    /// Open or create a RocksDB database at the given path.
    ///
    /// Column families are created if they do not already exist.
    /// Options are tuned for mobile flash storage:
    /// - Limited write buffer size to reduce memory pressure
    /// - Smaller block cache to stay within mobile RAM constraints
    /// - Level compaction optimized for NAND flash write patterns
    pub fn open(path: &str) -> Result<Self, GratiaError> {
        use rocksdb::{Options, DB};

        let mut opts = Options::default();
        opts.create_if_missing(true);
        opts.create_missing_column_families(true);

        // WHY: 16 MB write buffer keeps memory usage low on mobile devices
        // while still providing reasonable batching for write performance.
        opts.set_write_buffer_size(16 * 1024 * 1024);

        // WHY: 2 write buffers limits memory to ~32 MB for the write path,
        // leaving room for the rest of the protocol on memory-constrained phones.
        opts.set_max_write_buffer_number(2);

        // WHY: Level compaction is better for flash storage than universal compaction
        // because it produces more predictable write amplification patterns.
        opts.set_level_compaction_dynamic_level_bytes(true);

        let cf_descriptors: Vec<rocksdb::ColumnFamilyDescriptor> = ALL_COLUMN_FAMILIES
            .iter()
            .map(|name| {
                let cf_opts = Options::default();
                rocksdb::ColumnFamilyDescriptor::new(*name, cf_opts)
            })
            .collect();

        let db = DB::open_cf_descriptors(&opts, path, cf_descriptors)
            .map_err(|e| GratiaError::StorageError(format!("RocksDB open failed: {}", e)))?;

        Ok(RocksDbStore { db })
    }
}

#[cfg(feature = "rocksdb-backend")]
impl StateStore for RocksDbStore {
    fn put(&self, cf: &str, key: &[u8], value: &[u8]) -> Result<(), GratiaError> {
        let handle = self.db.cf_handle(cf).ok_or_else(|| {
            GratiaError::StorageError(format!("unknown column family: {}", cf))
        })?;
        self.db
            .put_cf(&handle, key, value)
            .map_err(|e| GratiaError::StorageError(format!("put failed: {}", e)))
    }

    fn get(&self, cf: &str, key: &[u8]) -> Result<Option<Vec<u8>>, GratiaError> {
        let handle = self.db.cf_handle(cf).ok_or_else(|| {
            GratiaError::StorageError(format!("unknown column family: {}", cf))
        })?;
        self.db
            .get_cf(&handle, key)
            .map_err(|e| GratiaError::StorageError(format!("get failed: {}", e)))
    }

    fn delete(&self, cf: &str, key: &[u8]) -> Result<(), GratiaError> {
        let handle = self.db.cf_handle(cf).ok_or_else(|| {
            GratiaError::StorageError(format!("unknown column family: {}", cf))
        })?;
        self.db
            .delete_cf(&handle, key)
            .map_err(|e| GratiaError::StorageError(format!("delete failed: {}", e)))
    }

    fn batch_write(
        &self,
        operations: Vec<(String, Vec<u8>, Option<Vec<u8>>)>,
    ) -> Result<(), GratiaError> {
        let mut batch = rocksdb::WriteBatch::default();
        for (cf, key, value) in &operations {
            let handle = self.db.cf_handle(cf).ok_or_else(|| {
                GratiaError::StorageError(format!("unknown column family: {}", cf))
            })?;
            match value {
                Some(v) => batch.put_cf(&handle, key, v),
                None => batch.delete_cf(&handle, key),
            }
        }
        self.db
            .write(batch)
            .map_err(|e| GratiaError::StorageError(format!("batch write failed: {}", e)))
    }

    fn iter_cf(&self, cf: &str) -> Result<Vec<(Vec<u8>, Vec<u8>)>, GratiaError> {
        let handle = self.db.cf_handle(cf).ok_or_else(|| {
            GratiaError::StorageError(format!("unknown column family: {}", cf))
        })?;
        let iter = self.db.iterator_cf(&handle, rocksdb::IteratorMode::Start);
        let mut result = Vec::new();
        for item in iter {
            let (k, v) = item.map_err(|e| {
                GratiaError::StorageError(format!("iterator error: {}", e))
            })?;
            result.push((k.to_vec(), v.to_vec()));
        }
        Ok(result)
    }

    fn count_keys(&self, cf: &str) -> Result<u64, GratiaError> {
        // WHY: RocksDB does not have an efficient count operation, so we iterate.
        // For production use, consider maintaining a counter in the STATE cf.
        let pairs = self.iter_cf(cf)?;
        Ok(pairs.len() as u64)
    }

    fn estimate_size_bytes(&self) -> Result<u64, GratiaError> {
        // WHY: RocksDB's property-based size estimation is approximate but fast,
        // avoiding a full scan of the database.
        let mut total: u64 = 0;
        for cf_name in ALL_COLUMN_FAMILIES {
            if let Some(handle) = self.db.cf_handle(cf_name) {
                if let Some(size_str) = self
                    .db
                    .property_value_cf(&handle, "rocksdb.estimate-live-data-size")
                    .map_err(|e| GratiaError::StorageError(format!("property error: {}", e)))?
                {
                    if let Ok(size) = size_str.parse::<u64>() {
                        total += size;
                    }
                }
            }
        }
        Ok(total)
    }
}

// ============================================================================
// Typed Helpers — convenience methods built on top of StateStore
// ============================================================================

/// High-level state database providing typed access to blocks, transactions, and accounts.
///
/// Wraps a `StateStore` backend and handles serialization/deserialization
/// of protocol types to and from raw bytes.
pub struct StateDb {
    store: Arc<dyn StateStore>,
}

impl StateDb {
    /// Create a new StateDb wrapping the given store backend.
    pub fn new(store: Arc<dyn StateStore>) -> Self {
        StateDb { store }
    }

    /// Get a reference to the underlying store (for direct key-value access).
    pub fn store(&self) -> &dyn StateStore {
        self.store.as_ref()
    }

    // --- Block operations ---

    /// Store a block, keyed by its header hash.
    pub fn put_block(&self, block: &Block) -> Result<(), GratiaError> {
        let hash = block.header.hash();
        let encoded = bincode::serialize(block)
            .map_err(|e| GratiaError::SerializationError(e.to_string()))?;
        self.store.put(CF_BLOCKS, &hash.0, &encoded)?;

        // Also index block by height for fast height-based lookups.
        let height_key = block.header.height.to_be_bytes();
        self.store.put(CF_BLOCKS, &height_key, &hash.0)?;

        Ok(())
    }

    /// Retrieve a block by its hash.
    pub fn get_block(&self, hash: &BlockHash) -> Result<Option<Block>, GratiaError> {
        match self.store.get(CF_BLOCKS, &hash.0)? {
            Some(data) => {
                let block: Block = bincode::deserialize(&data)
                    .map_err(|e| GratiaError::SerializationError(e.to_string()))?;
                Ok(Some(block))
            }
            None => Ok(None),
        }
    }

    /// Retrieve a block by its height.
    pub fn get_block_by_height(&self, height: u64) -> Result<Option<Block>, GratiaError> {
        let height_key = height.to_be_bytes();
        match self.store.get(CF_BLOCKS, &height_key)? {
            Some(hash_bytes) => {
                if hash_bytes.len() != 32 {
                    return Err(GratiaError::StorageError(
                        "corrupt block height index".to_string(),
                    ));
                }
                let mut hash = [0u8; 32];
                hash.copy_from_slice(&hash_bytes);
                self.get_block(&BlockHash(hash))
            }
            None => Ok(None),
        }
    }

    /// Delete a block by its hash and remove its height index entry.
    pub fn delete_block(&self, hash: &BlockHash) -> Result<(), GratiaError> {
        // First retrieve to find the height for index cleanup.
        if let Some(block) = self.get_block(hash)? {
            let height_key = block.header.height.to_be_bytes();
            self.store.delete(CF_BLOCKS, &height_key)?;
        }
        self.store.delete(CF_BLOCKS, &hash.0)
    }

    // --- Transaction operations ---

    /// Store a transaction, keyed by its hash.
    pub fn put_transaction(&self, tx: &Transaction) -> Result<(), GratiaError> {
        let encoded = bincode::serialize(tx)
            .map_err(|e| GratiaError::SerializationError(e.to_string()))?;
        self.store.put(CF_TRANSACTIONS, &tx.hash.0, &encoded)
    }

    /// Retrieve a transaction by its hash.
    pub fn get_transaction(&self, hash: &TxHash) -> Result<Option<Transaction>, GratiaError> {
        match self.store.get(CF_TRANSACTIONS, &hash.0)? {
            Some(data) => {
                let tx: Transaction = bincode::deserialize(&data)
                    .map_err(|e| GratiaError::SerializationError(e.to_string()))?;
                Ok(Some(tx))
            }
            None => Ok(None),
        }
    }

    /// Delete a transaction by its hash.
    pub fn delete_transaction(&self, hash: &TxHash) -> Result<(), GratiaError> {
        self.store.delete(CF_TRANSACTIONS, &hash.0)
    }

    // --- Account operations ---

    /// Store account state for an address.
    pub fn put_account(&self, address: &Address, state: &AccountState) -> Result<(), GratiaError> {
        let encoded = bincode::serialize(state)
            .map_err(|e| GratiaError::SerializationError(e.to_string()))?;
        self.store.put(CF_ACCOUNTS, &address.0, &encoded)
    }

    /// Retrieve account state for an address.
    /// Returns a default (zero-balance) account if the address has never been seen.
    pub fn get_account(&self, address: &Address) -> Result<AccountState, GratiaError> {
        match self.store.get(CF_ACCOUNTS, &address.0)? {
            Some(data) => {
                let state: AccountState = bincode::deserialize(&data)
                    .map_err(|e| GratiaError::SerializationError(e.to_string()))?;
                Ok(state)
            }
            None => Ok(AccountState::default()),
        }
    }

    /// Get the balance of an address in Lux.
    pub fn get_balance(&self, address: &Address) -> Result<Lux, GratiaError> {
        Ok(self.get_account(address)?.balance)
    }

    /// Get the nonce of an address.
    pub fn get_nonce(&self, address: &Address) -> Result<u64, GratiaError> {
        Ok(self.get_account(address)?.nonce)
    }

    // --- Attestation operations ---

    /// Store a Proof of Life attestation.
    /// Keyed by (node_id || date) for unique daily attestations per node.
    pub fn put_attestation(&self, attestation: &ProofOfLifeAttestation) -> Result<(), GratiaError> {
        let key = attestation_key(&attestation.node_id, &attestation.date);
        let encoded = bincode::serialize(attestation)
            .map_err(|e| GratiaError::SerializationError(e.to_string()))?;
        self.store.put(CF_ATTESTATIONS, &key, &encoded)
    }

    /// Retrieve a Proof of Life attestation for a specific node and date.
    pub fn get_attestation(
        &self,
        node_id: &NodeId,
        date: &chrono::NaiveDate,
    ) -> Result<Option<ProofOfLifeAttestation>, GratiaError> {
        let key = attestation_key(node_id, date);
        match self.store.get(CF_ATTESTATIONS, &key)? {
            Some(data) => {
                let att: ProofOfLifeAttestation = bincode::deserialize(&data)
                    .map_err(|e| GratiaError::SerializationError(e.to_string()))?;
                Ok(Some(att))
            }
            None => Ok(None),
        }
    }

    // --- General state operations ---

    /// Store the current chain tip (latest finalized block hash).
    pub fn set_chain_tip(&self, hash: &BlockHash) -> Result<(), GratiaError> {
        self.store.put(CF_STATE, STATE_KEY_CHAIN_TIP, &hash.0)
    }

    /// Get the current chain tip block hash.
    pub fn get_chain_tip(&self) -> Result<Option<BlockHash>, GratiaError> {
        match self.store.get(CF_STATE, STATE_KEY_CHAIN_TIP)? {
            Some(data) => {
                if data.len() != 32 {
                    return Err(GratiaError::StorageError("corrupt chain tip".to_string()));
                }
                let mut hash = [0u8; 32];
                hash.copy_from_slice(&data);
                Ok(Some(BlockHash(hash)))
            }
            None => Ok(None),
        }
    }

    /// Store the current block height.
    pub fn set_block_height(&self, height: u64) -> Result<(), GratiaError> {
        self.store
            .put(CF_STATE, STATE_KEY_BLOCK_HEIGHT, &height.to_be_bytes())
    }

    /// Get the current block height. Returns 0 if not set (genesis).
    pub fn get_block_height(&self) -> Result<u64, GratiaError> {
        match self.store.get(CF_STATE, STATE_KEY_BLOCK_HEIGHT)? {
            Some(data) => {
                if data.len() != 8 {
                    return Err(GratiaError::StorageError(
                        "corrupt block height".to_string(),
                    ));
                }
                Ok(u64::from_be_bytes(data.try_into().unwrap()))
            }
            None => Ok(0),
        }
    }

    /// Store the current state root hash.
    pub fn set_state_root(&self, root: &[u8; 32]) -> Result<(), GratiaError> {
        self.store.put(CF_STATE, STATE_KEY_STATE_ROOT, root)
    }

    /// Get the current state root hash.
    pub fn get_state_root(&self) -> Result<[u8; 32], GratiaError> {
        match self.store.get(CF_STATE, STATE_KEY_STATE_ROOT)? {
            Some(data) => {
                if data.len() != 32 {
                    return Err(GratiaError::StorageError(
                        "corrupt state root".to_string(),
                    ));
                }
                let mut root = [0u8; 32];
                root.copy_from_slice(&data);
                Ok(root)
            }
            // WHY: An all-zeros root represents an empty state trie (genesis).
            None => Ok([0u8; 32]),
        }
    }

    /// Atomically write a batch of operations.
    pub fn batch_write(
        &self,
        operations: Vec<(String, Vec<u8>, Option<Vec<u8>>)>,
    ) -> Result<(), GratiaError> {
        self.store.batch_write(operations)
    }

    /// Estimate the current database size in bytes.
    pub fn estimate_size_bytes(&self) -> Result<u64, GratiaError> {
        self.store.estimate_size_bytes()
    }
}

/// Build the attestation storage key: node_id bytes || date string bytes.
/// This ensures one attestation per node per day.
fn attestation_key(node_id: &NodeId, date: &chrono::NaiveDate) -> Vec<u8> {
    let mut key = Vec::with_capacity(32 + 10);
    key.extend_from_slice(&node_id.0);
    key.extend_from_slice(date.format("%Y-%m-%d").to_string().as_bytes());
    key
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn make_test_store() -> Arc<InMemoryStore> {
        Arc::new(InMemoryStore::new())
    }

    #[test]
    fn test_put_get_delete() {
        let store = make_test_store();
        store.put(CF_STATE, b"key1", b"value1").unwrap();
        assert_eq!(
            store.get(CF_STATE, b"key1").unwrap(),
            Some(b"value1".to_vec())
        );
        store.delete(CF_STATE, b"key1").unwrap();
        assert_eq!(store.get(CF_STATE, b"key1").unwrap(), None);
    }

    #[test]
    fn test_unknown_column_family() {
        let store = make_test_store();
        assert!(store.put("nonexistent", b"k", b"v").is_err());
        assert!(store.get("nonexistent", b"k").is_err());
    }

    #[test]
    fn test_batch_write() {
        let store = make_test_store();
        let ops = vec![
            (CF_STATE.to_string(), b"a".to_vec(), Some(b"1".to_vec())),
            (CF_STATE.to_string(), b"b".to_vec(), Some(b"2".to_vec())),
            (CF_STATE.to_string(), b"c".to_vec(), Some(b"3".to_vec())),
        ];
        store.batch_write(ops).unwrap();
        assert_eq!(store.get(CF_STATE, b"a").unwrap(), Some(b"1".to_vec()));
        assert_eq!(store.get(CF_STATE, b"b").unwrap(), Some(b"2".to_vec()));
        assert_eq!(store.get(CF_STATE, b"c").unwrap(), Some(b"3".to_vec()));

        // Delete via batch
        let ops = vec![(CF_STATE.to_string(), b"b".to_vec(), None)];
        store.batch_write(ops).unwrap();
        assert_eq!(store.get(CF_STATE, b"b").unwrap(), None);
    }

    #[test]
    fn test_iter_and_count() {
        let store = make_test_store();
        store.put(CF_ACCOUNTS, b"addr1", b"data1").unwrap();
        store.put(CF_ACCOUNTS, b"addr2", b"data2").unwrap();

        let pairs = store.iter_cf(CF_ACCOUNTS).unwrap();
        assert_eq!(pairs.len(), 2);
        assert_eq!(store.count_keys(CF_ACCOUNTS).unwrap(), 2);
    }

    #[test]
    fn test_state_db_account_roundtrip() {
        let store = make_test_store();
        let db = StateDb::new(store);

        let addr = Address([42u8; 32]);

        // Default account for unknown address
        let acct = db.get_account(&addr).unwrap();
        assert_eq!(acct.balance, 0);
        assert_eq!(acct.nonce, 0);

        // Store and retrieve
        let acct = AccountState {
            balance: 1_000_000,
            nonce: 5,
            staked: 500_000,
            overflow_stake: 0,
            pol_valid: true,
            pol_consecutive_days: 30,
            last_pol_date: None,
            node_id: None,
        };
        db.put_account(&addr, &acct).unwrap();

        let retrieved = db.get_account(&addr).unwrap();
        assert_eq!(retrieved.balance, 1_000_000);
        assert_eq!(retrieved.nonce, 5);
        assert_eq!(retrieved.staked, 500_000);
        assert!(retrieved.pol_valid);
    }

    #[test]
    fn test_state_db_chain_tip_and_height() {
        let store = make_test_store();
        let db = StateDb::new(store);

        assert_eq!(db.get_chain_tip().unwrap(), None);
        assert_eq!(db.get_block_height().unwrap(), 0);

        let hash = BlockHash([99u8; 32]);
        db.set_chain_tip(&hash).unwrap();
        db.set_block_height(42).unwrap();

        assert_eq!(db.get_chain_tip().unwrap(), Some(hash));
        assert_eq!(db.get_block_height().unwrap(), 42);
    }

    #[test]
    fn test_estimate_size() {
        let store = make_test_store();
        store.put(CF_STATE, b"hello", b"world").unwrap();
        let size = store.estimate_size_bytes().unwrap();
        // "hello" (5) + "world" (5) = 10 bytes minimum
        assert!(size >= 10);
    }
}
