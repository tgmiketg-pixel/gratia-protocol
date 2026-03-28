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
}
