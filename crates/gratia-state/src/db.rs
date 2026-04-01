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

    /// Iterate up to `limit` key-value pairs from a column family.
    /// WHY: For pruning operations, we often only need to read a subset
    /// of entries. This avoids loading the entire CF into memory.
    fn iter_cf_limit(&self, cf: &str, limit: usize) -> Result<Vec<(Vec<u8>, Vec<u8>)>, GratiaError>;

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

    /// Save the entire store to a file using bincode serialization.
    ///
    /// WHY: On mobile, RocksDB cross-compilation requires LLVM/libclang which
    /// may not be available. This file-based persistence is a pragmatic
    /// alternative that gives state durability (survives app restarts) without
    /// the C++ compilation dependency. The entire BTreeMap is serialized
    /// atomically — write to a temp file, then rename for crash safety.
    pub fn save_to_file(&self, path: &str) -> Result<(), GratiaError> {
        let data = self.data.read().map_err(|e| {
            GratiaError::StorageError(format!("lock poisoned: {}", e))
        })?;
        let bytes = bincode::serialize(&*data)
            .map_err(|e| GratiaError::SerializationError(e.to_string()))?;
        let tmp_path = format!("{}.tmp", path);
        std::fs::write(&tmp_path, &bytes)
            .map_err(|e| GratiaError::StorageError(format!("write failed: {}", e)))?;
        std::fs::rename(&tmp_path, path)
            .map_err(|e| GratiaError::StorageError(format!("rename failed: {}", e)))?;
        Ok(())
    }

    /// Load the store from a previously saved file.
    ///
    /// Returns a new InMemoryStore populated with the saved data.
    /// If the file doesn't exist or is corrupted, returns a fresh empty store.
    pub fn load_from_file(path: &str) -> Self {
        match std::fs::read(path) {
            Ok(bytes) => {
                match bincode::deserialize::<BTreeMap<String, BTreeMap<Vec<u8>, Vec<u8>>>>(&bytes) {
                    Ok(mut data) => {
                        // WHY: Ensure all column families exist even if the saved
                        // file predates a schema change that added new CFs.
                        for cf in ALL_COLUMN_FAMILIES {
                            data.entry(cf.to_string()).or_insert_with(BTreeMap::new);
                        }
                        tracing::info!("Loaded state from {}", path);
                        InMemoryStore {
                            data: RwLock::new(data),
                        }
                    }
                    Err(e) => {
                        tracing::warn!("Failed to deserialize state file: {} — starting fresh", e);
                        Self::new()
                    }
                }
            }
            Err(_) => {
                tracing::info!("No state file at {} — starting fresh", path);
                Self::new()
            }
        }
    }

    /// Get the approximate serialized size (for logging).
    pub fn data_size_estimate(&self) -> usize {
        self.data.read().map(|d| {
            d.values().map(|cf| cf.len()).sum::<usize>()
        }).unwrap_or(0)
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

    fn iter_cf_limit(&self, cf: &str, limit: usize) -> Result<Vec<(Vec<u8>, Vec<u8>)>, GratiaError> {
        let data = self.data.read().map_err(|e| {
            GratiaError::StorageError(format!("lock poisoned: {}", e))
        })?;
        let cf_map = data
            .get(cf)
            .ok_or_else(|| GratiaError::StorageError(format!("unknown column family: {}", cf)))?;
        Ok(cf_map.iter().take(limit).map(|(k, v)| (k.clone(), v.clone())).collect())
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

    fn iter_cf_limit(&self, cf: &str, limit: usize) -> Result<Vec<(Vec<u8>, Vec<u8>)>, GratiaError> {
        let handle = self.db.cf_handle(cf).ok_or_else(|| {
            GratiaError::StorageError(format!("unknown column family: {}", cf))
        })?;
        let iter = self.db.iterator_cf(&handle, rocksdb::IteratorMode::Start);
        let mut result = Vec::new();
        for item in iter.take(limit) {
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
        let hash = block.header.hash()?;
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

    /// Store a Proof of Life attestation (on-chain, unlinkable form).
    /// WHY: Keyed by nullifier — unique per node per epoch, prevents
    /// double-submission detection without revealing node identity.
    pub fn put_attestation(&self, attestation: &ProofOfLifeAttestation) -> Result<(), GratiaError> {
        let key = attestation.nullifier.to_vec();
        let encoded = bincode::serialize(attestation)
            .map_err(|e| GratiaError::SerializationError(e.to_string()))?;
        self.store.put(CF_ATTESTATIONS, &key, &encoded)
    }

    /// Retrieve a Proof of Life attestation by its nullifier.
    /// WHY: On-chain attestations are unlinkable — the only way to look
    /// one up is by nullifier, not by node_id or date.
    pub fn get_attestation_by_nullifier(
        &self,
        nullifier: &[u8; 32],
    ) -> Result<Option<ProofOfLifeAttestation>, GratiaError> {
        let key = nullifier.to_vec();
        match self.store.get(CF_ATTESTATIONS, &key)? {
            Some(data) => {
                let att: ProofOfLifeAttestation = bincode::deserialize(&data)
                    .map_err(|e| GratiaError::SerializationError(e.to_string()))?;
                Ok(Some(att))
            }
            None => Ok(None),
        }
    }

    /// Check if a nullifier has already been submitted (double-submission guard).
    pub fn has_nullifier(&self, nullifier: &[u8; 32]) -> Result<bool, GratiaError> {
        let key = nullifier.to_vec();
        Ok(self.store.get(CF_ATTESTATIONS, &key)?.is_some())
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
                // WHY: The length check above guarantees exactly 8 bytes,
                // but use map_err instead of unwrap for defense-in-depth.
                let bytes: [u8; 8] = data.try_into().map_err(|_| {
                    GratiaError::StorageError("corrupt block height bytes".to_string())
                })?;
                Ok(u64::from_be_bytes(bytes))
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

// NOTE: The old attestation_key(node_id, date) function was removed as part of
// the privacy hardening. On-chain attestations are now keyed by nullifier only,
// which prevents identity linkage. Local attestation records (LocalProofOfLifeRecord)
// can still be keyed by (node_id, date) in on-device storage.

// ============================================================================
// Storage Backend Factory
// ============================================================================

/// Configuration for which storage backend to use.
///
/// WHY: The FFI needs a single entry point that picks the right backend
/// based on compile-time features and runtime config. This avoids hardcoding
/// InMemoryStore in the FFI and makes the transition to RocksDB a config change,
/// not a code change.
#[derive(Debug, Clone)]
pub enum StorageBackendConfig {
    /// In-memory store with optional file-based persistence.
    /// Path is the file to save/load state from. If None, data is ephemeral.
    InMemory { persistence_path: Option<String> },
    /// RocksDB backend (requires `rocksdb-backend` feature).
    /// Path is the directory for the RocksDB database.
    #[cfg(feature = "rocksdb-backend")]
    RocksDb { db_path: String },
}

/// Result of opening a storage backend.
///
/// WHY: The caller needs both the trait object (for StateManager/StateDb) and
/// optionally the concrete InMemoryStore handle (for file-based save_to_file).
/// With RocksDB, persistence is automatic; with InMemoryStore, the caller must
/// periodically call save_to_file() on the concrete handle.
pub struct StorageBackend {
    /// The storage backend as a trait object.
    pub store: Arc<dyn StateStore>,
    /// If using InMemoryStore with file persistence, this holds the concrete
    /// handle for calling save_to_file(). None when using RocksDB (which
    /// persists automatically on every write).
    pub in_memory_handle: Option<Arc<InMemoryStore>>,
    /// The persistence path (for logging and save operations).
    pub persistence_path: Option<String>,
}

impl StorageBackend {
    /// Persist the current state to disk (if applicable).
    ///
    /// For InMemoryStore: serializes the BTreeMap to the persistence file.
    /// For RocksDB: no-op (writes are already durable).
    pub fn persist(&self) -> Result<(), GratiaError> {
        if let (Some(handle), Some(path)) = (&self.in_memory_handle, &self.persistence_path) {
            handle.save_to_file(path)?;
        }
        // WHY: RocksDB writes are already durable via WAL, so no explicit persist needed.
        Ok(())
    }

    /// Get the approximate data size in bytes.
    pub fn estimate_size(&self) -> Result<u64, GratiaError> {
        self.store.estimate_size_bytes()
    }
}

/// Open a storage backend based on the given configuration.
///
/// WHY: Single entry point for creating storage backends. The FFI calls this
/// once during initialization. When `rocksdb-backend` is compiled in, RocksDB
/// is used for production (automatic persistence, efficient iteration, compaction).
/// Otherwise, InMemoryStore with file persistence is used (works everywhere,
/// no C++ dependency, adequate for testnets up to ~100K blocks).
pub fn open_storage(config: StorageBackendConfig) -> Result<StorageBackend, GratiaError> {
    match config {
        StorageBackendConfig::InMemory { persistence_path } => {
            let store = match &persistence_path {
                Some(path) => Arc::new(InMemoryStore::load_from_file(path)),
                None => Arc::new(InMemoryStore::new()),
            };
            Ok(StorageBackend {
                store: store.clone() as Arc<dyn StateStore>,
                in_memory_handle: Some(store),
                persistence_path,
            })
        }
        #[cfg(feature = "rocksdb-backend")]
        StorageBackendConfig::RocksDb { db_path } => {
            let store = Arc::new(RocksDbStore::open(&db_path)?);
            tracing::info!("Opened RocksDB backend at {}", db_path);
            Ok(StorageBackend {
                store: store as Arc<dyn StateStore>,
                in_memory_handle: None,
                persistence_path: Some(db_path),
            })
        }
    }
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

    #[test]
    fn test_open_storage_in_memory_ephemeral() {
        let config = StorageBackendConfig::InMemory { persistence_path: None };
        let backend = open_storage(config).unwrap();
        assert!(backend.in_memory_handle.is_some());
        assert!(backend.persistence_path.is_none());
        // Writes should work
        backend.store.put(CF_STATE, b"key", b"val").unwrap();
        assert_eq!(backend.store.get(CF_STATE, b"key").unwrap(), Some(b"val".to_vec()));
        // Persist is a no-op without a path
        backend.persist().unwrap();
    }

    #[test]
    fn test_open_storage_in_memory_with_persistence() {
        let dir = std::env::temp_dir();
        let path = dir.join(format!("gratia_backend_test_{}.db", std::process::id()));
        let path_str = path.to_str().unwrap().to_string();

        // Open, write, persist
        {
            let config = StorageBackendConfig::InMemory {
                persistence_path: Some(path_str.clone()),
            };
            let backend = open_storage(config).unwrap();
            backend.store.put(CF_ACCOUNTS, b"addr1", b"data1").unwrap();
            backend.persist().unwrap();
        }

        // Re-open and verify data survived
        {
            let config = StorageBackendConfig::InMemory {
                persistence_path: Some(path_str.clone()),
            };
            let backend = open_storage(config).unwrap();
            assert_eq!(
                backend.store.get(CF_ACCOUNTS, b"addr1").unwrap(),
                Some(b"data1".to_vec())
            );
        }

        // Cleanup
        let _ = std::fs::remove_file(&path_str);
    }

    #[test]
    fn test_storage_backend_estimate_size() {
        let config = StorageBackendConfig::InMemory { persistence_path: None };
        let backend = open_storage(config).unwrap();
        backend.store.put(CF_STATE, b"key", b"value").unwrap();
        let size = backend.estimate_size().unwrap();
        assert!(size >= 8); // "key" (3) + "value" (5)
    }
}
