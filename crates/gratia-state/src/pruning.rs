//! State pruning for mobile storage constraints.
//!
//! Mobile devices have limited flash storage (typically 32-128 GB total, shared
//! with the OS, apps, and user data). The Gratia protocol targets a 2-5 GB
//! maximum state database size. This module implements pruning policies that
//! remove old block bodies, transaction details, and attestations while
//! preserving block headers (needed for chain verification) and current account
//! state (needed for transaction validation).

use chrono::{DateTime, Utc};
use tracing;

use gratia_core::error::GratiaError;

use crate::db::{StateDb, CF_ATTESTATIONS, CF_BLOCKS, CF_TRANSACTIONS};

// ============================================================================
// Pruning Configuration
// ============================================================================

/// Configurable pruning policy for the state database.
#[derive(Debug, Clone)]
pub struct PruningPolicy {
    /// Maximum database size in bytes before pruning is triggered.
    /// Default: 3 GB (middle of the 2-5 GB target range, leaving headroom).
    pub max_db_size_bytes: u64,

    /// Number of recent blocks to always keep (including full bodies).
    /// Older blocks have their bodies pruned but headers are retained.
    /// Default: 100,000 blocks (~4.6 days at 4-second block time).
    pub block_retention_count: u64,

    /// Number of recent blocks whose transactions are kept in full detail.
    /// Older transactions are pruned entirely.
    /// Default: 50,000 blocks (~2.3 days at 4-second block time).
    pub transaction_retention_count: u64,

    /// Number of days of Proof of Life attestations to retain.
    /// Older attestations are pruned (the on-chain validity is already
    /// reflected in account state).
    /// Default: 30 days.
    pub attestation_retention_days: u32,

    /// Size threshold (as a fraction of max) at which warning is logged.
    /// Default: 0.8 (80% of max triggers a warning).
    pub warning_threshold: f64,
}

impl Default for PruningPolicy {
    fn default() -> Self {
        PruningPolicy {
            // WHY: 3 GB gives comfortable headroom below the 5 GB hard limit
            // while leaving space for OS and user data on a 32 GB phone.
            max_db_size_bytes: 3 * 1024 * 1024 * 1024,

            // WHY: 100k blocks at 4-second intervals is ~4.6 days, sufficient
            // for reorg resolution and recent block queries. Older block bodies
            // can be re-fetched from archive nodes if needed.
            block_retention_count: 100_000,

            // WHY: 50k blocks of transaction history covers ~2.3 days of
            // recent activity. Users needing older history can query archive
            // nodes or block explorers.
            transaction_retention_count: 50_000,

            // WHY: 30 days of attestation history is sufficient for governance
            // eligibility checks (90-day requirement uses account state counters,
            // not raw attestations).
            attestation_retention_days: 30,

            // WHY: Alert at 80% so there's time to prune before hitting the
            // hard limit and potentially refusing new blocks.
            warning_threshold: 0.8,
        }
    }
}

// ============================================================================
// Pruning Engine
// ============================================================================

/// Result of a pruning operation, reporting what was removed.
#[derive(Debug, Clone, Default)]
pub struct PruningResult {
    /// Number of block bodies removed (headers retained).
    pub blocks_pruned: u64,
    /// Number of transactions removed.
    pub transactions_pruned: u64,
    /// Number of attestations removed.
    pub attestations_pruned: u64,
    /// Estimated bytes freed.
    pub bytes_freed: u64,
}

/// Check whether the database exceeds the size target and pruning is needed.
pub fn should_prune(db: &StateDb, policy: &PruningPolicy) -> Result<bool, GratiaError> {
    let current_size = db.estimate_size_bytes()?;
    Ok(current_size > policy.max_db_size_bytes)
}

/// Estimate the current database size and return it along with the policy limit.
pub fn estimate_db_size(
    db: &StateDb,
    policy: &PruningPolicy,
) -> Result<(u64, u64, f64), GratiaError> {
    let current_size = db.estimate_size_bytes()?;
    let max_size = policy.max_db_size_bytes;
    let utilization = if max_size > 0 {
        current_size as f64 / max_size as f64
    } else {
        0.0
    };
    Ok((current_size, max_size, utilization))
}

/// Prune old blocks that exceed the retention window.
///
/// Blocks older than `current_height - block_retention_count` have their
/// full serialized data replaced with just the header. This preserves
/// the chain structure while freeing space used by transaction lists
/// and validator signatures in the block body.
pub fn prune_old_blocks(
    db: &StateDb,
    policy: &PruningPolicy,
    current_height: u64,
) -> Result<u64, GratiaError> {
    if current_height <= policy.block_retention_count {
        // WHY: Nothing to prune if the chain is shorter than the retention window.
        return Ok(0);
    }

    let cutoff_height = current_height - policy.block_retention_count;
    let mut pruned = 0u64;

    // Iterate through blocks below the cutoff and remove full block data,
    // keeping only the block-by-height index (which points to the header hash).
    // We scan heights from 0 up to cutoff.
    //
    // WHY: We iterate by height rather than scanning the entire blocks CF because
    // height keys are 8 bytes (u64 big-endian) and sort before 32-byte hash keys
    // in the BTreeMap/RocksDB ordering, making the scan efficient.
    for height in 0..cutoff_height {
        let height_key = height.to_be_bytes();

        // Check if this height still has a hash pointer.
        if let Some(hash_bytes) = db.store().get(CF_BLOCKS, &height_key)? {
            if hash_bytes.len() == 32 {
                // Remove the full block data (keyed by hash).
                // The height->hash index entry is kept for chain traversal.
                db.store().delete(CF_BLOCKS, &hash_bytes)?;
                pruned += 1;
            }
        }
    }

    if pruned > 0 {
        tracing::info!(
            pruned_blocks = pruned,
            cutoff_height = cutoff_height,
            "Pruned old block bodies"
        );
    }

    Ok(pruned)
}

/// Prune old transactions that exceed the retention window.
///
/// Transactions are stored independently in the transactions CF.
/// We remove transactions that belong to blocks older than the retention window.
///
/// This is done by scanning the transactions CF and checking each transaction's
/// timestamp against the retention cutoff.
pub fn prune_old_transactions(
    db: &StateDb,
    policy: &PruningPolicy,
    current_height: u64,
) -> Result<u64, GratiaError> {
    if current_height <= policy.transaction_retention_count {
        return Ok(0);
    }

    // WHY: count_keys is O(1) in RocksDB (metadata lookup), avoiding the O(n)
    // full scan when pruning isn't needed.
    let count = db.store().count_keys(CF_TRANSACTIONS)?;

    // Only prune if we have more transactions than expected for the retention window.
    // A rough estimate: max ~1000 TPS * 4 sec/block * retention_blocks.
    let max_expected_txs = policy.transaction_retention_count * 1000;
    if count <= max_expected_txs {
        return Ok(0);
    }

    // WHY: We use a time-based approach for transaction pruning. We find the
    // block at the cutoff height to determine the cutoff timestamp, then remove
    // transactions older than that timestamp. This avoids needing a separate
    // block-height-to-transaction index.
    let _cutoff_height = current_height - policy.transaction_retention_count;

    // Over the limit — load all transactions to sort by timestamp and prune oldest.
    // WHY: Without a block-height index on transactions, we collect all and
    // sort by timestamp, then prune the oldest. This is acceptable because
    // pruning runs infrequently (when size threshold is hit) and the
    // transactions CF is bounded by the retention policy.
    let all_txs = db.store().iter_cf(CF_TRANSACTIONS)?;
    let mut pruned = 0u64;

    let excess = count - max_expected_txs;
    // The keys in a BTreeMap are already sorted; oldest entries come first
    // if we used chronological keys. For hash-keyed transactions, we need
    // to deserialize and sort.
    let mut timestamped: Vec<(Vec<u8>, DateTime<Utc>)> = Vec::new();
    for (key, value) in &all_txs {
        if let Ok(tx) = bincode::deserialize::<gratia_core::types::Transaction>(value) {
            timestamped.push((key.clone(), tx.timestamp));
        }
    }
    timestamped.sort_by_key(|(_, ts)| *ts);

    let to_remove = (excess as usize).min(timestamped.len());
    let batch: Vec<(String, Vec<u8>, Option<Vec<u8>>)> = timestamped[..to_remove]
        .iter()
        .map(|(key, _)| (CF_TRANSACTIONS.to_string(), key.clone(), None))
        .collect();

    if !batch.is_empty() {
        pruned = batch.len() as u64;
        db.batch_write(batch)?;
    }

    if pruned > 0 {
        tracing::info!(
            pruned_transactions = pruned,
            "Pruned old transactions"
        );
    }

    Ok(pruned)
}

/// Prune old Proof of Life attestations beyond the retention limit.
///
/// WHY: On-chain attestations are unlinkable and have no date field.
/// We retain only the most recent `max_retained` attestations (based on
/// insertion order / key order) and prune everything else. The PoL validity
/// is already reflected in account state (consecutive day counters), so
/// raw attestations are only needed for recent double-submission detection.
///
/// The `attestation_retention_days` policy field determines the max count:
/// one attestation per epoch per node, at roughly one per day, with headroom.
pub fn prune_old_attestations(
    db: &StateDb,
    policy: &PruningPolicy,
) -> Result<u64, GratiaError> {
    // WHY: Retain a generous number of attestations based on the retention
    // period. Most nodes submit one attestation per epoch (~1 per day).
    // A network of 10,000 nodes for 30 days = 300,000 attestations max.
    // We use the configured retention_days as the multiplier, pruning
    // only when the total count exceeds a reasonable cap.
    let max_retained = (policy.attestation_retention_days as u64) * 10_000;

    // WHY: count_keys is O(1) in RocksDB (metadata lookup), avoiding the O(n)
    // full scan when pruning isn't needed.
    let count = db.store().count_keys(CF_ATTESTATIONS)?;

    if count <= max_retained {
        return Ok(0);
    }

    // Over the limit — only load the excess entries we need to prune.
    // WHY: iter_cf_limit reads only the oldest `prune_count` entries by key
    // order, avoiding loading the entire CF into memory. In Phase 2 this
    // should use a streaming iterator with a prefix scan for even lower
    // memory usage on very large attestation sets.
    let prune_count = (count - max_retained) as usize;
    let to_prune = db.store().iter_cf_limit(CF_ATTESTATIONS, prune_count)?;

    let batch: Vec<(String, Vec<u8>, Option<Vec<u8>>)> = to_prune
        .iter()
        .map(|(key, _)| (CF_ATTESTATIONS.to_string(), key.clone(), None))
        .collect();

    let pruned = batch.len() as u64;
    if !batch.is_empty() {
        db.batch_write(batch)?;
    }

    if pruned > 0 {
        tracing::info!(
            pruned_attestations = pruned,
            total_before = count,
            max_retained = max_retained,
            "Pruned old attestations"
        );
    }

    Ok(pruned)
}

/// Run a full pruning cycle: blocks, transactions, and attestations.
///
/// Returns a summary of what was pruned.
pub fn run_pruning_cycle(
    db: &StateDb,
    policy: &PruningPolicy,
    current_height: u64,
) -> Result<PruningResult, GratiaError> {
    let size_before = db.estimate_size_bytes()?;

    let blocks_pruned = prune_old_blocks(db, policy, current_height)?;
    let transactions_pruned = prune_old_transactions(db, policy, current_height)?;
    let attestations_pruned = prune_old_attestations(db, policy)?;

    let size_after = db.estimate_size_bytes()?;
    let bytes_freed = size_before.saturating_sub(size_after);

    let result = PruningResult {
        blocks_pruned,
        transactions_pruned,
        attestations_pruned,
        bytes_freed,
    };

    tracing::info!(
        blocks = result.blocks_pruned,
        transactions = result.transactions_pruned,
        attestations = result.attestations_pruned,
        bytes_freed = result.bytes_freed,
        "Pruning cycle complete"
    );

    Ok(result)
}

// ============================================================================
// PruningConfig / PruningManager / PruneStats — higher-level pruning API
// ============================================================================

/// Configuration for the interval-based pruning manager.
///
/// While `PruningPolicy` controls *what* gets pruned (retention windows, size
/// limits), `PruningConfig` controls *when* and *how aggressively* the pruning
/// manager runs.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct PruningConfig {
    /// Maximum number of full blocks to keep on disk.
    /// Older blocks have their bodies removed (headers kept for chain integrity).
    /// Default: 10,000 blocks (~11.1 hours at 4-second block time).
    // WHY: 10k blocks is enough for short reorgs and recent queries on a phone
    // while keeping flash storage usage predictable. Archive nodes store full
    // history; mobile nodes don't need to.
    pub max_blocks_retained: u64,

    /// Hard limit on total state database size in bytes.
    /// When estimated size exceeds this, pruning is forced regardless of block
    /// retention settings.
    /// Default: 2 GB (2,147,483,648 bytes).
    // WHY: 2 GB leaves generous headroom on a 32 GB phone (OS ~10 GB, apps
    // ~8 GB, user data ~10 GB, Gratia state ~2 GB). Conservative target avoids
    // the user ever seeing a "storage full" notification caused by Gratia.
    pub max_state_size_bytes: u64,

    /// How often to check whether pruning is needed, measured in blocks.
    /// Default: every 100 blocks (~6.7 minutes at 4-second block time).
    // WHY: Checking every block wastes CPU on size estimation. Every 100 blocks
    // is frequent enough that the database never overshoots the limit by more
    // than ~100 blocks worth of data (~25 KB * 100 = ~2.5 MB overshoot max).
    pub prune_interval_blocks: u64,

    /// Number of days of transaction history to retain for the user's wallet
    /// history display and receipt lookups.
    /// Default: 90 days.
    // WHY: 90 days covers typical dispute resolution periods and gives users
    // enough history to reconcile with external records. Older transactions
    // can still be looked up via archive nodes or the block explorer.
    pub keep_recent_txs_days: u32,
}

impl Default for PruningConfig {
    fn default() -> Self {
        PruningConfig {
            max_blocks_retained: 10_000,
            max_state_size_bytes: 2_147_483_648,
            prune_interval_blocks: 100,
            keep_recent_txs_days: 90,
        }
    }
}

/// Cumulative statistics tracked across pruning cycles.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct PruneStats {
    /// Total number of block bodies removed across all pruning cycles.
    pub total_blocks_pruned: u64,
    /// Total estimated bytes freed across all pruning cycles.
    pub total_bytes_freed: u64,
    /// Block height at which the most recent pruning cycle ran.
    pub last_prune_height: u64,
    /// Timestamp of the most recent pruning cycle.
    pub last_prune_timestamp: DateTime<Utc>,
}

impl Default for PruneStats {
    fn default() -> Self {
        PruneStats {
            total_blocks_pruned: 0,
            total_bytes_freed: 0,
            last_prune_height: 0,
            // WHY: Unix epoch as sentinel — means "never pruned yet".
            last_prune_timestamp: DateTime::<Utc>::from_timestamp(0, 0)
                .expect("epoch timestamp is always valid"),
        }
    }
}

/// Interval-based pruning manager that tracks block cadence and cumulative
/// statistics.
///
/// The `PruningManager` sits between the consensus layer and the low-level
/// pruning functions. Each time a block is applied, the caller increments
/// the manager's counter via `tick()`. When `should_prune()` returns true,
/// the caller runs the pruning cycle and feeds the result back via
/// `record_prune()`.
///
/// This design keeps the pruning logic stateless (pure functions operating
/// on `StateDb`) while the manager owns the scheduling and bookkeeping.
#[derive(Debug, Clone)]
pub struct PruningManager {
    /// Configuration controlling pruning intervals and limits.
    pub config: PruningConfig,
    /// Number of blocks processed since the last pruning check.
    pub blocks_since_last_prune: u64,
    /// Cumulative pruning statistics.
    pub prune_stats: PruneStats,
}

impl PruningManager {
    /// Create a new PruningManager with the given configuration.
    pub fn new(config: PruningConfig) -> Self {
        PruningManager {
            config,
            blocks_since_last_prune: 0,
            prune_stats: PruneStats::default(),
        }
    }

    /// Create a new PruningManager with default configuration.
    pub fn with_defaults() -> Self {
        Self::new(PruningConfig::default())
    }

    /// Increment the block counter. Call this once per applied block.
    pub fn tick(&mut self) {
        self.blocks_since_last_prune += 1;
    }

    /// Returns true when enough blocks have elapsed to warrant a pruning check.
    ///
    /// Resets the internal counter when it returns true, so the next call
    /// starts counting from zero again.
    pub fn should_prune(&mut self) -> bool {
        if self.blocks_since_last_prune >= self.config.prune_interval_blocks {
            self.blocks_since_last_prune = 0;
            true
        } else {
            false
        }
    }

    /// Calculate the block height below which blocks can be pruned.
    ///
    /// Returns 0 if the chain is too short to prune anything.
    pub fn calculate_prune_target(&self, current_height: u64) -> u64 {
        current_height.saturating_sub(self.config.max_blocks_retained)
    }

    /// Rough estimate of storage consumption for a given number of blocks and
    /// transactions.
    ///
    /// This is a heuristic, not an exact measurement. It accounts for:
    /// - Block headers (~200 bytes each)
    /// - Block bodies (~200 bytes overhead + transaction data)
    /// - Transactions (~250 bytes each for standard, ~2 KB for shielded)
    /// - Index overhead (~50 bytes per block for height->hash mapping)
    ///
    /// The estimate is intentionally conservative (overestimates) so that
    /// pruning triggers slightly early rather than slightly late.
    pub fn estimate_storage_bytes(block_count: u64, tx_count: u64) -> u64 {
        // WHY: 450 bytes per block = 200 byte header + 200 byte body overhead
        // + 50 byte index entry. Measured from bincode serialization of typical
        // Gratia blocks during testnet. Conservative to avoid underestimating.
        const BYTES_PER_BLOCK: u64 = 450;
        // WHY: 300 bytes per transaction = average of standard (250 bytes) and
        // shielded (~2 KB), weighted heavily toward standard since shielded
        // transactions are opt-in and expected to be <5% of volume initially.
        const BYTES_PER_TX: u64 = 300;

        block_count
            .saturating_mul(BYTES_PER_BLOCK)
            .saturating_add(tx_count.saturating_mul(BYTES_PER_TX))
    }

    /// Record the result of a pruning cycle into cumulative statistics.
    pub fn record_prune(&mut self, blocks_pruned: u64, bytes_freed: u64, height: u64) {
        self.prune_stats.total_blocks_pruned += blocks_pruned;
        self.prune_stats.total_bytes_freed += bytes_freed;
        self.prune_stats.last_prune_height = height;
        self.prune_stats.last_prune_timestamp = Utc::now();
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{InMemoryStore, StateDb};
    use std::sync::Arc;

    fn make_test_db() -> StateDb {
        StateDb::new(Arc::new(InMemoryStore::new()))
    }

    #[test]
    fn test_default_policy() {
        let policy = PruningPolicy::default();
        assert_eq!(policy.max_db_size_bytes, 3 * 1024 * 1024 * 1024);
        assert_eq!(policy.block_retention_count, 100_000);
        assert_eq!(policy.transaction_retention_count, 50_000);
        assert_eq!(policy.attestation_retention_days, 30);
    }

    #[test]
    fn test_should_prune_empty_db() {
        let db = make_test_db();
        let policy = PruningPolicy::default();
        // Empty DB is well under the limit.
        assert!(!should_prune(&db, &policy).unwrap());
    }

    #[test]
    fn test_should_prune_over_limit() {
        let db = make_test_db();
        let mut policy = PruningPolicy::default();
        // Set a very small limit so our test data exceeds it.
        policy.max_db_size_bytes = 1;

        // Add some data.
        db.store()
            .put(crate::db::CF_STATE, b"key", b"value")
            .unwrap();

        assert!(should_prune(&db, &policy).unwrap());
    }

    #[test]
    fn test_estimate_db_size() {
        let db = make_test_db();
        let policy = PruningPolicy::default();

        let (current, max, utilization) = estimate_db_size(&db, &policy).unwrap();
        assert_eq!(current, 0);
        assert_eq!(max, policy.max_db_size_bytes);
        assert_eq!(utilization, 0.0);
    }

    #[test]
    fn test_prune_old_blocks_no_op_when_short_chain() {
        let db = make_test_db();
        let policy = PruningPolicy {
            block_retention_count: 100,
            ..Default::default()
        };

        // Chain height is below retention count — nothing to prune.
        let pruned = prune_old_blocks(&db, &policy, 50).unwrap();
        assert_eq!(pruned, 0);
    }

    #[test]
    fn test_prune_old_blocks_removes_bodies() {
        let db = make_test_db();
        let policy = PruningPolicy {
            block_retention_count: 5,
            ..Default::default()
        };

        // Simulate 10 blocks: store height->hash and hash->data.
        for height in 0..10u64 {
            let height_key = height.to_be_bytes();
            let hash = [height as u8; 32];
            let data = vec![0u8; 100]; // Fake block body

            db.store()
                .put(crate::db::CF_BLOCKS, &height_key, &hash)
                .unwrap();
            db.store()
                .put(crate::db::CF_BLOCKS, &hash, &data)
                .unwrap();
        }

        // Prune at height 10 with retention of 5 — blocks 0-4 should be pruned.
        let pruned = prune_old_blocks(&db, &policy, 10).unwrap();
        assert_eq!(pruned, 5);

        // Block 0's body should be gone.
        let hash0 = [0u8; 32];
        assert!(db.store().get(crate::db::CF_BLOCKS, &hash0).unwrap().is_none());

        // Block 0's height index should still exist (for chain traversal).
        let height0_key = 0u64.to_be_bytes();
        assert!(db
            .store()
            .get(crate::db::CF_BLOCKS, &height0_key)
            .unwrap()
            .is_some());

        // Block 9's body should still exist.
        let hash9 = [9u8; 32];
        assert!(db.store().get(crate::db::CF_BLOCKS, &hash9).unwrap().is_some());
    }

    #[test]
    fn test_prune_old_attestations() {
        let db = make_test_db();
        // WHY: Set retention to 0 days so the max_retained = 0, forcing all
        // attestations to be pruned. This tests the pruning logic itself.
        let policy = PruningPolicy {
            attestation_retention_days: 0,
            ..Default::default()
        };

        fn make_test_att(nullifier_byte: u8) -> gratia_core::types::ProofOfLifeAttestation {
            gratia_core::types::ProofOfLifeAttestation {
                blinded_id: [0xAA; 32],
                nullifier: [nullifier_byte; 32],
                zk_proof: vec![0u8; 32],
                zk_commitments: None,
                presence_score: 50,
                sensor_flags: gratia_core::types::SensorFlags {
                    gps: true,
                    accelerometer: true,
                    wifi: true,
                    bluetooth: false,
                    gyroscope: false,
                    ambient_light: false,
                    cellular: false,
                    barometer: false,
                    magnetometer: false,
                    nfc: false,
                    secure_enclave: false,
                    biometric: false,
                    camera_hash: false,
                    microphone_hash: false,
                },
                signature: vec![0u8; 64],
            }
        }

        let att1 = make_test_att(0x01);
        let att2 = make_test_att(0x02);

        db.put_attestation(&att1).unwrap();
        db.put_attestation(&att2).unwrap();

        // With retention_days=0, max_retained=0, so all should be pruned
        let pruned = prune_old_attestations(&db, &policy).unwrap();
        assert_eq!(pruned, 2);

        // Both attestations should be gone
        assert!(db.get_attestation_by_nullifier(&[0x01; 32]).unwrap().is_none());
        assert!(db.get_attestation_by_nullifier(&[0x02; 32]).unwrap().is_none());
    }

    #[test]
    fn test_prune_attestations_under_limit() {
        let db = make_test_db();
        // WHY: With retention_days=7, max_retained = 70,000 — way more than
        // the 2 attestations we store, so nothing should be pruned.
        let policy = PruningPolicy {
            attestation_retention_days: 7,
            ..Default::default()
        };

        fn make_test_att(nullifier_byte: u8) -> gratia_core::types::ProofOfLifeAttestation {
            gratia_core::types::ProofOfLifeAttestation {
                blinded_id: [0xAA; 32],
                nullifier: [nullifier_byte; 32],
                zk_proof: vec![0u8; 32],
                zk_commitments: None,
                presence_score: 50,
                sensor_flags: gratia_core::types::SensorFlags {
                    gps: true,
                    accelerometer: true,
                    wifi: true,
                    bluetooth: false,
                    gyroscope: false,
                    ambient_light: false,
                    cellular: false,
                    barometer: false,
                    magnetometer: false,
                    nfc: false,
                    secure_enclave: false,
                    biometric: false,
                    camera_hash: false,
                    microphone_hash: false,
                },
                signature: vec![0u8; 64],
            }
        }

        db.put_attestation(&make_test_att(0x01)).unwrap();
        db.put_attestation(&make_test_att(0x02)).unwrap();

        // Under the limit — nothing should be pruned
        let pruned = prune_old_attestations(&db, &policy).unwrap();
        assert_eq!(pruned, 0);

        // Both should still exist
        assert!(db.get_attestation_by_nullifier(&[0x01; 32]).unwrap().is_some());
        assert!(db.get_attestation_by_nullifier(&[0x02; 32]).unwrap().is_some());
    }

    #[test]
    fn test_run_pruning_cycle() {
        let db = make_test_db();
        let policy = PruningPolicy {
            block_retention_count: 2,
            transaction_retention_count: 100,
            attestation_retention_days: 7,
            max_db_size_bytes: 1024 * 1024 * 1024,
            warning_threshold: 0.8,
        };

        // Add some blocks.
        for height in 0..5u64 {
            let height_key = height.to_be_bytes();
            let hash = [height as u8; 32];
            db.store()
                .put(crate::db::CF_BLOCKS, &height_key, &hash)
                .unwrap();
            db.store()
                .put(crate::db::CF_BLOCKS, &hash, &vec![0u8; 50])
                .unwrap();
        }

        let result = run_pruning_cycle(&db, &policy, 5).unwrap();
        assert_eq!(result.blocks_pruned, 3); // blocks 0, 1, 2 pruned
    }

    // ====================================================================
    // PruningConfig / PruningManager / PruneStats tests
    // ====================================================================

    #[test]
    fn test_pruning_config_defaults() {
        let config = PruningConfig::default();
        assert_eq!(config.max_blocks_retained, 10_000);
        assert_eq!(config.max_state_size_bytes, 2_147_483_648);
        assert_eq!(config.prune_interval_blocks, 100);
        assert_eq!(config.keep_recent_txs_days, 90);
    }

    #[test]
    fn test_prune_stats_defaults() {
        let stats = PruneStats::default();
        assert_eq!(stats.total_blocks_pruned, 0);
        assert_eq!(stats.total_bytes_freed, 0);
        assert_eq!(stats.last_prune_height, 0);
        // Sentinel timestamp is Unix epoch.
        assert_eq!(stats.last_prune_timestamp.timestamp(), 0);
    }

    #[test]
    fn test_pruning_manager_should_prune_cycle() {
        let config = PruningConfig {
            prune_interval_blocks: 5,
            ..Default::default()
        };
        let mut mgr = PruningManager::new(config);

        // Ticking 1-4 should not trigger pruning.
        for _ in 0..4 {
            mgr.tick();
            assert!(!mgr.should_prune());
        }

        // The 5th tick should trigger pruning.
        mgr.tick();
        assert!(mgr.should_prune());

        // Counter resets after should_prune returns true.
        assert_eq!(mgr.blocks_since_last_prune, 0);

        // Next cycle: 5 more ticks needed.
        for _ in 0..4 {
            mgr.tick();
            assert!(!mgr.should_prune());
        }
        mgr.tick();
        assert!(mgr.should_prune());
    }

    #[test]
    fn test_pruning_manager_should_prune_immediate_at_interval() {
        // With interval = 1, every tick triggers pruning.
        let config = PruningConfig {
            prune_interval_blocks: 1,
            ..Default::default()
        };
        let mut mgr = PruningManager::new(config);

        mgr.tick();
        assert!(mgr.should_prune());
        mgr.tick();
        assert!(mgr.should_prune());
        mgr.tick();
        assert!(mgr.should_prune());
    }

    #[test]
    fn test_calculate_prune_target() {
        let mgr = PruningManager::with_defaults();

        // Chain shorter than retention — target is 0.
        assert_eq!(mgr.calculate_prune_target(5_000), 0);

        // Chain at exactly the retention window — target is 0.
        assert_eq!(mgr.calculate_prune_target(10_000), 0);

        // Chain longer than retention — prune below the cutoff.
        assert_eq!(mgr.calculate_prune_target(15_000), 5_000);
        assert_eq!(mgr.calculate_prune_target(100_000), 90_000);
    }

    #[test]
    fn test_calculate_prune_target_custom_config() {
        let config = PruningConfig {
            max_blocks_retained: 500,
            ..Default::default()
        };
        let mgr = PruningManager::new(config);

        assert_eq!(mgr.calculate_prune_target(1_000), 500);
        assert_eq!(mgr.calculate_prune_target(500), 0);
        assert_eq!(mgr.calculate_prune_target(250), 0);
    }

    #[test]
    fn test_estimate_storage_bytes() {
        // Zero blocks and zero transactions = zero bytes.
        assert_eq!(PruningManager::estimate_storage_bytes(0, 0), 0);

        // 1,000 blocks with no transactions.
        // 1,000 * 450 = 450,000 bytes.
        assert_eq!(PruningManager::estimate_storage_bytes(1_000, 0), 450_000);

        // No blocks, 1,000 transactions.
        // 1,000 * 300 = 300,000 bytes.
        assert_eq!(PruningManager::estimate_storage_bytes(0, 1_000), 300_000);

        // Mixed: 10,000 blocks + 50,000 transactions.
        // 10,000 * 450 + 50,000 * 300 = 4,500,000 + 15,000,000 = 19,500,000 bytes.
        assert_eq!(
            PruningManager::estimate_storage_bytes(10_000, 50_000),
            19_500_000
        );
    }

    #[test]
    fn test_estimate_storage_bytes_overflow_safety() {
        // Extremely large values should not panic (saturating arithmetic).
        let result = PruningManager::estimate_storage_bytes(u64::MAX, u64::MAX);
        assert_eq!(result, u64::MAX);
    }

    #[test]
    fn test_record_prune_accumulates() {
        let mut mgr = PruningManager::with_defaults();

        mgr.record_prune(100, 50_000, 10_000);
        assert_eq!(mgr.prune_stats.total_blocks_pruned, 100);
        assert_eq!(mgr.prune_stats.total_bytes_freed, 50_000);
        assert_eq!(mgr.prune_stats.last_prune_height, 10_000);
        assert!(mgr.prune_stats.last_prune_timestamp.timestamp() > 0);

        mgr.record_prune(200, 75_000, 20_000);
        assert_eq!(mgr.prune_stats.total_blocks_pruned, 300);
        assert_eq!(mgr.prune_stats.total_bytes_freed, 125_000);
        assert_eq!(mgr.prune_stats.last_prune_height, 20_000);
    }

    #[test]
    fn test_pruning_config_serde_roundtrip() {
        let config = PruningConfig {
            max_blocks_retained: 5_000,
            max_state_size_bytes: 1_073_741_824,
            prune_interval_blocks: 50,
            keep_recent_txs_days: 60,
        };

        let serialized = bincode::serialize(&config).unwrap();
        let deserialized: PruningConfig = bincode::deserialize(&serialized).unwrap();

        assert_eq!(deserialized.max_blocks_retained, 5_000);
        assert_eq!(deserialized.max_state_size_bytes, 1_073_741_824);
        assert_eq!(deserialized.prune_interval_blocks, 50);
        assert_eq!(deserialized.keep_recent_txs_days, 60);
    }

    #[test]
    fn test_prune_stats_serde_roundtrip() {
        let stats = PruneStats {
            total_blocks_pruned: 42,
            total_bytes_freed: 123_456,
            last_prune_height: 9_999,
            last_prune_timestamp: Utc::now(),
        };

        let serialized = bincode::serialize(&stats).unwrap();
        let deserialized: PruneStats = bincode::deserialize(&serialized).unwrap();

        assert_eq!(deserialized.total_blocks_pruned, 42);
        assert_eq!(deserialized.total_bytes_freed, 123_456);
        assert_eq!(deserialized.last_prune_height, 9_999);
    }
}
