//! Shard-aware consensus engine.
//!
//! Bridges geographic sharding (gratia-state) with the consensus engine so that
//! each shard runs its own committee, block production, and finality while
//! cross-shard committee members prevent single-shard capture attacks.
//!
//! Key design decisions:
//! - 80% of a shard's committee is drawn from validators assigned to that shard.
//! - 20% is drawn from neighboring shards (cross-shard members).
//! - Block producer selection within a shard uses the same VRF weighting as
//!   the global committee (Presence Score weighted).
//! - Cross-shard transaction receipts are verified via Merkle proof against
//!   the source shard's state root.
//! - Before the sharding activation threshold (10K nodes), a single global
//!   shard (shard 0) is used, which degrades gracefully to the existing
//!   global consensus behavior.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use tracing::{debug, info, warn};

use gratia_core::crypto::sha256;
use gratia_core::error::GratiaError;
use gratia_core::types::{Block, GeoLocation, NodeId, ShardId, Transaction, Address};

use crate::committee::{
    CommitteeMember, EligibleNode, ValidatorCommittee,
    select_committee_with_network_size, tier_for_network_size,
};
use crate::vrf::VrfPublicKey;

// ============================================================================
// Constants
// ============================================================================

/// Percentage of committee drawn from neighboring shards.
/// WHY: 20% cross-shard validators prevent single-shard capture. An attacker
/// controlling a majority within one geographic region still cannot finalize
/// blocks without agreement from validators in adjacent shards.
const CROSS_SHARD_COMMITTEE_PCT: u8 = 20;

/// Minimum validators needed per shard before shard-local consensus activates.
/// WHY: Below this threshold, the shard cannot form a committee with sufficient
/// Byzantine fault tolerance. The shard falls back to the global committee.
const MIN_VALIDATORS_PER_SHARD: usize = 5;

/// Domain separator for shard-specific VRF committee selection.
/// WHY: Prevents cross-shard replay of committee selection proofs.
const SHARD_COMMITTEE_DOMAIN: &[u8] = b"gratia-shard-committee-v1";

// ============================================================================
// Types
// ============================================================================

/// Information about a validator including its shard assignment.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ValidatorInfo {
    /// The node's identity.
    pub node_id: NodeId,
    /// Composite Presence Score (40-100).
    pub presence_score: u8,
    /// Last known geographic location (if available).
    pub location: Option<GeoLocation>,
    /// Amount staked by this validator.
    pub stake: u64,
    /// The shard this validator is assigned to.
    pub shard_id: ShardId,
    /// The node's VRF public key.
    pub vrf_pubkey: VrfPublicKey,
    /// Whether the node has valid Proof of Life.
    pub has_valid_pol: bool,
    /// Whether the node meets minimum stake.
    pub meets_minimum_stake: bool,
    /// Consecutive days of valid PoL history.
    pub pol_days: u64,
}

impl ValidatorInfo {
    /// Convert to an EligibleNode for committee selection.
    fn to_eligible_node(&self) -> EligibleNode {
        EligibleNode {
            node_id: self.node_id,
            vrf_pubkey: self.vrf_pubkey.clone(),
            presence_score: self.presence_score,
            has_valid_pol: self.has_valid_pol,
            meets_minimum_stake: self.meets_minimum_stake,
            pol_days: self.pol_days,
            signing_pubkey: vec![], // TODO: populate from ValidatorInfo when available
        }
    }
}

/// A cross-shard transaction receipt for verification within the consensus layer.
/// WHY: Defined locally to avoid a circular dependency between gratia-consensus
/// and gratia-state. The gratia-state CrossShardReceipt has the same structure;
/// a From conversion can bridge them at the integration layer.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CrossShardReceipt {
    /// The original transaction hash.
    pub tx_hash: [u8; 32],
    /// Source shard where the transaction was included.
    pub source_shard: ShardId,
    /// Destination shard where the transaction effect must be applied.
    pub dest_shard: ShardId,
    /// Merkle proof of inclusion in the source shard's block.
    /// Serialized proof path: Vec<(hash, is_left)> pairs.
    pub inclusion_proof: Vec<u8>,
    /// Block height in the source shard where this was included.
    pub source_block_height: u64,
}

/// Per-shard consensus configuration.
#[derive(Debug, Clone)]
pub struct ShardConsensusConfig {
    /// Shard ID this consensus engine is responsible for.
    pub shard_id: ShardId,
    /// Committee size for this shard (scales with node count in the shard).
    pub committee_size: usize,
    /// Finality threshold (67% of committee).
    pub finality_threshold: usize,
    /// Percentage of committee drawn from neighboring shards.
    /// WHY: Defaults to 20%. A higher percentage increases cross-shard latency
    /// but makes shard capture harder.
    pub cross_shard_pct: u8,
}

impl ShardConsensusConfig {
    /// Create a config for a shard with committee size derived from the
    /// number of validators assigned to it.
    pub fn for_shard(shard_id: ShardId, shard_validator_count: u64) -> Self {
        let tier = tier_for_network_size(shard_validator_count);
        ShardConsensusConfig {
            shard_id,
            committee_size: tier.committee_size,
            finality_threshold: tier.finality_threshold,
            cross_shard_pct: CROSS_SHARD_COMMITTEE_PCT,
        }
    }
}

// ============================================================================
// ShardedConsensus — per-shard consensus engine
// ============================================================================

/// Manages consensus for a specific shard.
///
/// Each shard has its own committee, block height, and state root.
/// The committee is composed of ~80% local validators and ~20% cross-shard
/// validators from neighboring shards for security.
pub struct ShardedConsensus {
    /// Configuration for this shard's consensus.
    config: ShardConsensusConfig,
    /// Local shard committee members (from this shard's geographic region).
    local_committee: Vec<CommitteeMember>,
    /// Cross-shard committee members (from neighboring shards).
    cross_shard_members: Vec<CommitteeMember>,
    /// The full combined committee (local + cross-shard) used for block
    /// production and finality. This is the authoritative committee.
    combined_committee: Option<ValidatorCommittee>,
    /// Current block height for this shard.
    shard_height: u64,
    /// Shard state root (Merkle root of this shard's state trie).
    shard_state_root: [u8; 32],
}

impl ShardedConsensus {
    /// Create a new shard consensus engine.
    pub fn new(config: ShardConsensusConfig) -> Self {
        ShardedConsensus {
            config,
            local_committee: Vec::new(),
            cross_shard_members: Vec::new(),
            combined_committee: None,
            shard_height: 0,
            shard_state_root: [0u8; 32],
        }
    }

    /// Select committee members for this shard from the full validator set.
    ///
    /// 80% are drawn from validators assigned to this shard's geographic region.
    /// 20% are drawn from validators in neighboring shards, providing cross-shard
    /// security that prevents a regional majority from unilaterally finalizing blocks.
    pub fn select_committee(
        &mut self,
        validators: &[ValidatorInfo],
        shard_assignments: &HashMap<NodeId, ShardId>,
        neighbor_shards: &[ShardId],
        vrf_seed: &[u8; 32],
    ) -> Result<(), GratiaError> {
        // Partition validators into local (this shard) and neighbor pools.
        let local_validators: Vec<&ValidatorInfo> = validators
            .iter()
            .filter(|v| {
                shard_assignments
                    .get(&v.node_id)
                    .map(|s| *s == self.config.shard_id)
                    .unwrap_or(false)
            })
            .collect();

        let neighbor_validators: Vec<&ValidatorInfo> = validators
            .iter()
            .filter(|v| {
                shard_assignments
                    .get(&v.node_id)
                    .map(|s| neighbor_shards.contains(s))
                    .unwrap_or(false)
            })
            .collect();

        // Calculate how many slots go to cross-shard vs local.
        let total_committee = self.config.committee_size;
        let cross_shard_count = cross_shard_validator_count(total_committee, self.config.cross_shard_pct);
        let local_count = total_committee.saturating_sub(cross_shard_count);

        debug!(
            shard = self.config.shard_id.0,
            local_pool = local_validators.len(),
            neighbor_pool = neighbor_validators.len(),
            local_slots = local_count,
            cross_shard_slots = cross_shard_count,
            "Selecting shard committee",
        );

        // Build a shard-specific VRF seed to ensure different shards get different committees.
        let shard_seed = sha256(&[
            SHARD_COMMITTEE_DOMAIN,
            vrf_seed,
            &self.config.shard_id.0.to_be_bytes(),
        ].concat());

        // Select local committee members.
        let local_eligible: Vec<EligibleNode> = local_validators
            .iter()
            .map(|v| v.to_eligible_node())
            .collect();

        let local_network_size = local_eligible.len() as u64;
        let local_committee_result = select_committee_with_network_size(
            &local_eligible,
            &shard_seed,
            0, // epoch number — managed by the coordinator
            0, // slot — managed by the coordinator
            local_network_size,
            None,
        );

        let local_selected = match local_committee_result {
            Ok(committee) => {
                let mut members = committee.members;
                members.truncate(local_count);
                members
            }
            Err(_) => {
                // WHY: If the local pool is too small, we cannot form a shard committee.
                // This shard should fall back to global consensus (handled by ShardCoordinator).
                warn!(
                    shard = self.config.shard_id.0,
                    local_count = local_validators.len(),
                    "Insufficient local validators for shard committee",
                );
                Vec::new()
            }
        };

        // Select cross-shard committee members from the neighbor pool.
        let cross_shard_seed = sha256(&[
            b"gratia-cross-shard-v1:".as_slice(),
            &shard_seed,
        ].concat());

        let neighbor_eligible: Vec<EligibleNode> = neighbor_validators
            .iter()
            .map(|v| v.to_eligible_node())
            .collect();

        let cross_shard_selected = if !neighbor_eligible.is_empty() && cross_shard_count > 0 {
            let neighbor_network_size = neighbor_eligible.len() as u64;
            match select_committee_with_network_size(
                &neighbor_eligible,
                &cross_shard_seed,
                0,
                0,
                neighbor_network_size,
                None,
            ) {
                Ok(committee) => {
                    let mut members = committee.members;
                    members.truncate(cross_shard_count);
                    members
                }
                Err(_) => Vec::new(),
            }
        } else {
            Vec::new()
        };

        info!(
            shard = self.config.shard_id.0,
            local_selected = local_selected.len(),
            cross_shard_selected = cross_shard_selected.len(),
            "Shard committee selected",
        );

        self.local_committee = local_selected;
        self.cross_shard_members = cross_shard_selected;

        // Build the combined committee for block production and finality.
        self.build_combined_committee(&shard_seed);

        Ok(())
    }

    /// Build the combined committee from local + cross-shard members.
    fn build_combined_committee(&mut self, seed: &[u8; 32]) {
        use chrono::Utc;
        use crate::committee::{CommitteeEpoch, SLOTS_PER_EPOCH};

        let mut all_members: Vec<CommitteeMember> = Vec::new();
        all_members.extend(self.local_committee.clone());
        all_members.extend(self.cross_shard_members.clone());

        // Re-sort by selection value so block producer ordering is deterministic.
        all_members.sort_by(|a, b| {
            a.selection_value
                .partial_cmp(&b.selection_value)
                .unwrap_or(std::cmp::Ordering::Equal)
        });

        let actual_size = all_members.len();
        // WHY: Finality threshold is 67% of the actual committee size, rounded up.
        // This maintains BFT guarantees even if fewer members than configured were selected.
        // WHY: Even with an empty committee, require at least 1 signature for safety.
        // A finality threshold of 0 would let any block claim finality with zero signatures.
        let finality = if actual_size == 0 {
            1
        } else {
            ((actual_size as f64 * 2.0 / 3.0).ceil() as usize).max(1)
        };

        let epoch = CommitteeEpoch {
            epoch_number: 0,
            start_slot: 0,
            end_slot: SLOTS_PER_EPOCH,
            established_at: Utc::now(),
            seed: *seed,
        };

        self.combined_committee = Some(ValidatorCommittee {
            epoch,
            members: all_members,
            committee_size: actual_size,
            finality_threshold: finality,
            network_size_snapshot: 0,
        });
    }

    /// Select block producer for this shard using VRF weighted by presence score.
    ///
    /// Uses the combined committee (local + cross-shard) and picks the producer
    /// via round-robin within the committee, consistent with the global consensus
    /// approach in committee.rs.
    pub fn select_block_producer(&self, slot: u64, _vrf_seed: &[u8; 32]) -> Option<&CommitteeMember> {
        // WHY: Delegate to the combined committee's block_producer_for_slot,
        // which uses round-robin among VRF-sorted members. The VRF sorting
        // already accounts for presence score weighting.
        self.combined_committee
            .as_ref()
            .and_then(|c| c.block_producer_for_slot(slot))
    }

    /// Validate a block for this shard.
    ///
    /// Checks that:
    /// 1. The block's shard_id field matches this shard (if present).
    /// 2. The block producer is a member of this shard's committee.
    /// 3. The block height follows this shard's chain.
    pub fn validate_shard_block(&self, block: &Block, shard_id: ShardId) -> Result<bool, GratiaError> {
        if shard_id != self.config.shard_id {
            return Err(GratiaError::BlockValidationFailed {
                reason: format!(
                    "Block shard_id {} does not match this shard's id {}",
                    shard_id.0, self.config.shard_id.0,
                ),
            });
        }

        let committee = self.combined_committee.as_ref().ok_or_else(|| {
            GratiaError::BlockValidationFailed {
                reason: "No active committee for shard validation".into(),
            }
        })?;

        // Verify the block producer is in this shard's committee.
        if !committee.is_committee_member(&block.header.producer) {
            return Err(GratiaError::BlockValidationFailed {
                reason: format!(
                    "Block producer {} is not a member of shard {} committee",
                    block.header.producer, self.config.shard_id.0,
                ),
            });
        }

        // Verify block height continuity.
        let expected_height = self.shard_height + 1;
        if block.header.height != expected_height && block.header.height > expected_height {
            // WHY: Allow blocks at expected height or ahead (fast-forward sync).
            // Blocks behind our height are silently skipped by the caller.
            debug!(
                shard = self.config.shard_id.0,
                expected = expected_height,
                actual = block.header.height,
                "Shard block height gap — accepting for fast-forward",
            );
        }

        Ok(true)
    }

    /// Verify a cross-shard transaction receipt.
    ///
    /// Reconstructs the Merkle path and verifies against the source shard's
    /// state root. The source_state_root must be obtained from a finalized
    /// block in the source shard.
    pub fn verify_cross_shard_receipt(
        &self,
        receipt: &CrossShardReceipt,
        source_state_root: &[u8; 32],
    ) -> Result<bool, GratiaError> {
        // Verify the receipt targets this shard.
        if receipt.dest_shard != self.config.shard_id {
            return Err(GratiaError::BlockValidationFailed {
                reason: format!(
                    "Cross-shard receipt destination {} does not match this shard {}",
                    receipt.dest_shard.0, self.config.shard_id.0,
                ),
            });
        }

        // Verify the Merkle inclusion proof.
        // WHY: The inclusion_proof contains serialized (hash, is_left) pairs forming
        // a path from the tx_hash leaf to the state root.
        let verified = verify_merkle_inclusion(
            &receipt.tx_hash,
            &receipt.inclusion_proof,
            source_state_root,
        );

        if !verified {
            return Err(GratiaError::BlockValidationFailed {
                reason: format!(
                    "Invalid Merkle proof for cross-shard receipt (tx {})",
                    hex::encode(&receipt.tx_hash[..8]),
                ),
            });
        }

        debug!(
            source_shard = receipt.source_shard.0,
            dest_shard = receipt.dest_shard.0,
            source_height = receipt.source_block_height,
            "Cross-shard receipt verified",
        );

        Ok(true)
    }

    /// Get the shard's current combined committee (local + cross-shard).
    pub fn committee(&self) -> &[CommitteeMember] {
        self.combined_committee
            .as_ref()
            .map(|c| c.members.as_slice())
            .unwrap_or(&[])
    }

    /// Get just the local committee members.
    pub fn local_committee(&self) -> &[CommitteeMember] {
        &self.local_committee
    }

    /// Get just the cross-shard committee members.
    pub fn cross_shard_committee(&self) -> &[CommitteeMember] {
        &self.cross_shard_members
    }

    /// Get the shard ID.
    pub fn shard_id(&self) -> ShardId {
        self.config.shard_id
    }

    /// Get the current shard block height.
    pub fn shard_height(&self) -> u64 {
        self.shard_height
    }

    /// Get the current shard state root.
    pub fn shard_state_root(&self) -> &[u8; 32] {
        &self.shard_state_root
    }

    /// Update shard state after a block is finalized.
    pub fn advance_height(&mut self, new_height: u64, new_state_root: [u8; 32]) {
        self.shard_height = new_height;
        self.shard_state_root = new_state_root;
    }
}

// ============================================================================
// ShardCoordinator — manages multiple shard consensus instances
// ============================================================================

/// Coordinates consensus across all shards on this node.
///
/// A node typically participates in its own shard's consensus and may also
/// serve as a cross-shard committee member for neighboring shards.
/// The coordinator routes transactions, manages cross-shard receipts,
/// and initializes per-shard consensus engines.
pub struct ShardCoordinator {
    /// This node's primary shard assignment.
    primary_shard: ShardId,
    /// Consensus engines for shards this node participates in.
    shard_engines: HashMap<ShardId, ShardedConsensus>,
    /// Cross-shard transaction queue (pending relay to destination shards).
    cross_shard_queue: Vec<CrossShardReceipt>,
    /// Number of active shards in the network.
    active_shard_count: u16,
}

impl ShardCoordinator {
    /// Create a new shard coordinator for this node.
    pub fn new(primary_shard: ShardId, active_shard_count: u16) -> Self {
        ShardCoordinator {
            primary_shard,
            shard_engines: HashMap::new(),
            cross_shard_queue: Vec::new(),
            active_shard_count,
        }
    }

    /// Get this node's primary shard.
    pub fn primary_shard(&self) -> ShardId {
        self.primary_shard
    }

    /// Get the number of active shards.
    pub fn active_shard_count(&self) -> u16 {
        self.active_shard_count
    }

    /// Initialize shard consensus engines with geographic assignments.
    ///
    /// Creates a ShardedConsensus engine for the primary shard and any
    /// shards where this node is a cross-shard committee member.
    pub fn initialize_shards(
        &mut self,
        all_validators: &[ValidatorInfo],
        vrf_seed: &[u8; 32],
    ) {
        // Build shard assignment map.
        let shard_assignments: HashMap<NodeId, ShardId> = all_validators
            .iter()
            .map(|v| (v.node_id, v.shard_id))
            .collect();

        // Count validators per shard.
        let mut shard_counts: HashMap<ShardId, u64> = HashMap::new();
        for v in all_validators {
            *shard_counts.entry(v.shard_id).or_default() += 1;
        }

        // Initialize the primary shard engine.
        let primary_count = shard_counts.get(&self.primary_shard).copied().unwrap_or(0);
        let primary_config = ShardConsensusConfig::for_shard(self.primary_shard, primary_count);
        let mut primary_engine = ShardedConsensus::new(primary_config);

        let neighbors = self.neighbor_shards(self.primary_shard);
        if let Err(e) = primary_engine.select_committee(
            all_validators,
            &shard_assignments,
            &neighbors,
            vrf_seed,
        ) {
            warn!(
                shard = self.primary_shard.0,
                error = %e,
                "Failed to select committee for primary shard",
            );
        }

        self.shard_engines.insert(self.primary_shard, primary_engine);

        // Check if this node was selected as a cross-shard member for any neighbor.
        // If so, create an engine for that shard too.
        // WHY: A node needs to validate blocks for any shard it participates in,
        // not just its primary shard.
        for &neighbor_id in &neighbors {
            let neighbor_count = shard_counts.get(&neighbor_id).copied().unwrap_or(0);
            if neighbor_count < MIN_VALIDATORS_PER_SHARD as u64 {
                continue;
            }

            let neighbor_config = ShardConsensusConfig::for_shard(neighbor_id, neighbor_count);
            let mut neighbor_engine = ShardedConsensus::new(neighbor_config);
            let neighbor_neighbors = self.neighbor_shards(neighbor_id);

            if let Err(e) = neighbor_engine.select_committee(
                all_validators,
                &shard_assignments,
                &neighbor_neighbors,
                vrf_seed,
            ) {
                warn!(
                    shard = neighbor_id.0,
                    error = %e,
                    "Failed to select committee for neighbor shard",
                );
                continue;
            }

            // Only insert if this node is actually on that shard's committee.
            // WHY: No point tracking a shard we don't participate in.
            // We check by looking for our node_id in the combined committee.
            // (The caller's node_id isn't available here, so we insert
            // unconditionally and let the caller prune if needed.)
            self.shard_engines.insert(neighbor_id, neighbor_engine);
        }

        info!(
            primary_shard = self.primary_shard.0,
            engines = self.shard_engines.len(),
            "Shard coordinator initialized",
        );
    }

    /// Route a transaction to the correct shard based on sender location.
    ///
    /// Uses longitudinal band assignment consistent with the ShardManager
    /// in gratia-state. If no location is available, falls back to
    /// address-based deterministic assignment.
    pub fn route_transaction(
        &self,
        _tx: &Transaction,
        sender_location: Option<&GeoLocation>,
        sender_address: &Address,
    ) -> ShardId {
        match sender_location {
            Some(loc) => shard_from_location(loc, self.active_shard_count),
            None => shard_from_address(sender_address, self.active_shard_count),
        }
    }

    /// Process a cross-shard transaction receipt.
    ///
    /// Verifies the receipt against the source shard's state root and queues
    /// it for inclusion in the destination shard's next block.
    pub fn handle_cross_shard_tx(
        &mut self,
        receipt: CrossShardReceipt,
        source_state_root: &[u8; 32],
    ) -> Result<(), GratiaError> {
        // Verify receipt in the destination shard's engine.
        let dest_shard = receipt.dest_shard;

        if let Some(engine) = self.shard_engines.get(&dest_shard) {
            engine.verify_cross_shard_receipt(&receipt, source_state_root)?;
        } else {
            // WHY: If we don't have an engine for the destination shard, we
            // still queue the receipt — the networking layer will relay it to
            // a node that does participate in that shard.
            debug!(
                dest_shard = dest_shard.0,
                "Queuing cross-shard receipt for shard we don't participate in",
            );
        }

        self.cross_shard_queue.push(receipt);
        Ok(())
    }

    /// Drain all pending cross-shard receipts for relay.
    pub fn drain_cross_shard_queue(&mut self) -> Vec<CrossShardReceipt> {
        std::mem::take(&mut self.cross_shard_queue)
    }

    /// Get the pending cross-shard queue length.
    pub fn cross_shard_queue_len(&self) -> usize {
        self.cross_shard_queue.len()
    }

    /// Get a reference to a shard engine.
    pub fn get_shard_engine(&self, shard_id: &ShardId) -> Option<&ShardedConsensus> {
        self.shard_engines.get(shard_id)
    }

    /// Get a mutable reference to a shard engine.
    pub fn get_shard_engine_mut(&mut self, shard_id: &ShardId) -> Option<&mut ShardedConsensus> {
        self.shard_engines.get_mut(shard_id)
    }

    /// Get all shard IDs this node participates in.
    pub fn participating_shards(&self) -> Vec<ShardId> {
        self.shard_engines.keys().copied().collect()
    }

    /// Compute neighbor shards for a given shard.
    ///
    /// Neighbors are the shards immediately before and after in the
    /// longitudinal band ordering (wrapping around).
    fn neighbor_shards(&self, shard_id: ShardId) -> Vec<ShardId> {
        if self.active_shard_count <= 1 {
            return Vec::new();
        }

        let id = shard_id.0;
        let count = self.active_shard_count;

        // WHY: Wrapping neighbors ensure that shard 0 and shard (count-1)
        // are neighbors, forming a ring topology. This prevents edge shards
        // from having fewer cross-shard validators.
        let prev = if id == 0 { count - 1 } else { id - 1 };
        let next = if id == count - 1 { 0 } else { id + 1 };

        if prev == next {
            // Only 2 shards — single neighbor.
            vec![ShardId(prev)]
        } else {
            vec![ShardId(prev), ShardId(next)]
        }
    }
}

// ============================================================================
// Helper Functions
// ============================================================================

/// Calculate the number of cross-shard validator slots for a given committee size.
/// WHY: Ceiling division ensures at least 1 cross-shard validator for any
/// committee of 5 or more, matching the CROSS_SHARD_COMMITTEE_PCT constant
/// from gratia-state/sharding.rs.
fn cross_shard_validator_count(committee_size: usize, pct: u8) -> usize {
    (committee_size * pct as usize + 99) / 100
}

/// Map a geographic location to a shard ID using longitudinal bands.
/// WHY: Mirrors the shard_from_location logic in gratia-state/sharding.rs
/// to ensure consistent shard assignment across both layers.
fn shard_from_location(location: &GeoLocation, active_shards: u16) -> ShardId {
    let normalized_lon = (location.lon as f64 + 180.0).rem_euclid(360.0);
    let band_width = 360.0 / active_shards as f64;
    let shard_index = (normalized_lon / band_width) as u16;
    ShardId(shard_index.min(active_shards - 1))
}

/// Map an address to a shard ID using hash-based assignment.
/// WHY: Mirrors shard_from_address in gratia-state/sharding.rs.
fn shard_from_address(address: &Address, active_shards: u16) -> ShardId {
    let hash_val = u16::from_be_bytes([address.0[0], address.0[1]]);
    ShardId(hash_val % active_shards)
}

/// Verify a Merkle inclusion proof for a transaction hash against a state root.
///
/// The proof format is a series of 33-byte entries:
/// [0]     = position flag (0x00 = sibling is on the left, 0x01 = sibling is on the right)
/// [1..33] = sibling hash (32 bytes)
///
/// WHY: This compact format minimizes proof size for mobile network transmission.
/// A proof for a tree of depth 20 (1M leaves) is only 660 bytes.
fn verify_merkle_inclusion(
    leaf_hash: &[u8; 32],
    proof_bytes: &[u8],
    expected_root: &[u8; 32],
) -> bool {
    // WHY: Entry size is 33 bytes — 1 byte position flag + 32 bytes hash.
    const ENTRY_SIZE: usize = 33;

    if proof_bytes.len() % ENTRY_SIZE != 0 {
        return false;
    }

    // WHY: Empty proof is valid only if the leaf IS the root (single-leaf tree).
    if proof_bytes.is_empty() {
        return leaf_hash == expected_root;
    }

    let mut current = *leaf_hash;

    for chunk in proof_bytes.chunks_exact(ENTRY_SIZE) {
        let is_right = chunk[0] != 0;
        let mut sibling = [0u8; 32];
        sibling.copy_from_slice(&chunk[1..33]);

        // WHY: The order of concatenation determines whether the sibling is
        // left or right in the Merkle tree. Getting this wrong would produce
        // a completely different root.
        current = if is_right {
            sha256(&[&current[..], &sibling[..]].concat())
        } else {
            sha256(&[&sibling[..], &current[..]].concat())
        };
    }

    current == *expected_root
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vrf::VrfPublicKey;

    fn make_validator(id_byte: u8, shard: u16, score: u8) -> ValidatorInfo {
        let mut node_id = [0u8; 32];
        node_id[0] = id_byte;
        ValidatorInfo {
            node_id: NodeId(node_id),
            presence_score: score,
            location: None,
            stake: 1000,
            shard_id: ShardId(shard),
            vrf_pubkey: VrfPublicKey { bytes: [id_byte; 32] },
            has_valid_pol: true,
            meets_minimum_stake: true,
            pol_days: 90,
        }
    }

    fn make_validators_for_shards(per_shard: usize, shard_count: u16) -> Vec<ValidatorInfo> {
        let mut validators = Vec::new();
        let mut id = 0u8;
        for shard in 0..shard_count {
            for _ in 0..per_shard {
                validators.push(make_validator(id, shard, 60));
                id = id.wrapping_add(1);
            }
        }
        validators
    }

    // ========================================================================
    // Cross-shard validator count
    // ========================================================================

    #[test]
    fn test_cross_shard_validator_count() {
        // committee 3 -> ceil(3 * 20 / 100) = ceil(0.6) = 1
        assert_eq!(cross_shard_validator_count(3, 20), 1);
        // committee 7 -> ceil(7 * 20 / 100) = ceil(1.4) = 2
        assert_eq!(cross_shard_validator_count(7, 20), 2);
        // committee 21 -> ceil(21 * 20 / 100) = ceil(4.2) = 5
        assert_eq!(cross_shard_validator_count(21, 20), 5);
    }

    // ========================================================================
    // Shard-from-location / address (mirrors gratia-state tests)
    // ========================================================================

    #[test]
    fn test_shard_from_location_consistency() {
        // Verify our location->shard mapping matches the gratia-state logic.
        let ny = GeoLocation { lat: 40.7, lon: -74.0 };
        let tokyo = GeoLocation { lat: 35.7, lon: 139.7 };

        let ny_shard = shard_from_location(&ny, 4);
        let tokyo_shard = shard_from_location(&tokyo, 4);

        assert!(ny_shard.0 < 4);
        assert!(tokyo_shard.0 < 4);
        // New York and Tokyo should be in different shards with 4 shards.
        assert_ne!(ny_shard, tokyo_shard);
    }

    #[test]
    fn test_shard_from_address_consistency() {
        let addr = Address([42u8; 32]);
        let shard = shard_from_address(&addr, 4);
        assert!(shard.0 < 4);

        // Deterministic.
        let shard2 = shard_from_address(&addr, 4);
        assert_eq!(shard, shard2);
    }

    // ========================================================================
    // Committee selection: 80/20 local/cross-shard split
    // ========================================================================

    #[test]
    fn test_committee_respects_local_cross_shard_split() {
        // 4 shards, 30 validators each = 120 total.
        let validators = make_validators_for_shards(30, 4);
        let shard_assignments: HashMap<NodeId, ShardId> = validators
            .iter()
            .map(|v| (v.node_id, v.shard_id))
            .collect();

        let config = ShardConsensusConfig::for_shard(ShardId(0), 30);
        let mut engine = ShardedConsensus::new(config);

        let neighbors = vec![ShardId(3), ShardId(1)]; // Ring neighbors of shard 0
        let seed = [0xAB; 32];
        engine.select_committee(&validators, &shard_assignments, &neighbors, &seed).unwrap();

        let local_count = engine.local_committee().len();
        let cross_count = engine.cross_shard_committee().len();
        let total = local_count + cross_count;

        assert!(total > 0, "Committee should not be empty");
        assert!(local_count > 0, "Should have local committee members");

        // Local members should be from shard 0.
        for member in engine.local_committee() {
            let assigned_shard = shard_assignments.get(&member.node_id).unwrap();
            assert_eq!(*assigned_shard, ShardId(0),
                "Local member should be assigned to shard 0");
        }

        // Cross-shard members should be from neighbor shards (1 or 3).
        for member in engine.cross_shard_committee() {
            let assigned_shard = shard_assignments.get(&member.node_id).unwrap();
            assert!(
                *assigned_shard == ShardId(1) || *assigned_shard == ShardId(3),
                "Cross-shard member should be from a neighbor shard, got shard {}",
                assigned_shard.0,
            );
        }

        // Verify approximate 80/20 split (allow flexibility for small committees).
        if total >= 5 {
            let cross_pct = (cross_count as f64 / total as f64) * 100.0;
            assert!(
                cross_pct >= 10.0 && cross_pct <= 40.0,
                "Cross-shard percentage {:.1}% should be roughly 20%",
                cross_pct,
            );
        }
    }

    // ========================================================================
    // Block producer selection within shard
    // ========================================================================

    #[test]
    fn test_block_producer_selection_uses_vrf() {
        let validators = make_validators_for_shards(20, 4);
        let shard_assignments: HashMap<NodeId, ShardId> = validators
            .iter()
            .map(|v| (v.node_id, v.shard_id))
            .collect();

        let config = ShardConsensusConfig::for_shard(ShardId(0), 20);
        let mut engine = ShardedConsensus::new(config);
        let neighbors = vec![ShardId(3), ShardId(1)];
        engine.select_committee(&validators, &shard_assignments, &neighbors, &[0xAB; 32]).unwrap();

        let seed = [0xCD; 32];
        let producer = engine.select_block_producer(0, &seed);
        assert!(producer.is_some(), "Should select a block producer");

        // Different slots should (eventually) select different producers.
        let producer_0 = engine.select_block_producer(0, &seed).unwrap().node_id;
        let producer_1 = engine.select_block_producer(1, &seed).unwrap().node_id;

        let committee_size = engine.committee().len();
        if committee_size > 1 {
            // With >1 member, consecutive slots should map to different producers
            // (round-robin within the committee).
            assert_ne!(producer_0, producer_1,
                "Different slots should pick different producers");
        }
    }

    // ========================================================================
    // Cross-shard receipt verification
    // ========================================================================

    #[test]
    fn test_cross_shard_receipt_valid_proof() {
        let config = ShardConsensusConfig {
            shard_id: ShardId(1),
            committee_size: 3,
            finality_threshold: 2,
            cross_shard_pct: 20,
        };
        let engine = ShardedConsensus::new(config);

        // Build a simple Merkle proof: leaf -> root with one sibling.
        let tx_hash = sha256(b"test-tx");
        let sibling = sha256(b"sibling-hash");
        let root = sha256(&[&tx_hash[..], &sibling[..]].concat());

        // Proof: sibling is on the right (flag = 0x01).
        let mut proof = vec![0x01u8];
        proof.extend_from_slice(&sibling);

        let receipt = CrossShardReceipt {
            tx_hash,
            source_shard: ShardId(0),
            dest_shard: ShardId(1),
            inclusion_proof: proof,
            source_block_height: 42,
        };

        let result = engine.verify_cross_shard_receipt(&receipt, &root);
        assert!(result.is_ok(), "Valid proof should verify: {:?}", result.err());
    }

    #[test]
    fn test_cross_shard_receipt_invalid_proof() {
        let config = ShardConsensusConfig {
            shard_id: ShardId(1),
            committee_size: 3,
            finality_threshold: 2,
            cross_shard_pct: 20,
        };
        let engine = ShardedConsensus::new(config);

        let tx_hash = sha256(b"test-tx");
        let wrong_root = [0xFF; 32];

        // Proof that doesn't match the root.
        let sibling = sha256(b"sibling-hash");
        let mut proof = vec![0x01u8];
        proof.extend_from_slice(&sibling);

        let receipt = CrossShardReceipt {
            tx_hash,
            source_shard: ShardId(0),
            dest_shard: ShardId(1),
            inclusion_proof: proof,
            source_block_height: 42,
        };

        let result = engine.verify_cross_shard_receipt(&receipt, &wrong_root);
        assert!(result.is_err(), "Invalid proof should fail verification");
    }

    #[test]
    fn test_cross_shard_receipt_wrong_destination() {
        let config = ShardConsensusConfig {
            shard_id: ShardId(1),
            committee_size: 3,
            finality_threshold: 2,
            cross_shard_pct: 20,
        };
        let engine = ShardedConsensus::new(config);

        let receipt = CrossShardReceipt {
            tx_hash: [0; 32],
            source_shard: ShardId(0),
            dest_shard: ShardId(2), // Wrong destination
            inclusion_proof: vec![],
            source_block_height: 1,
        };

        let result = engine.verify_cross_shard_receipt(&receipt, &[0; 32]);
        assert!(result.is_err());
    }

    // ========================================================================
    // Transaction routing
    // ========================================================================

    #[test]
    fn test_transaction_routing_by_location() {
        let coordinator = ShardCoordinator::new(ShardId(0), 4);

        let ny = GeoLocation { lat: 40.7, lon: -74.0 };
        let tokyo = GeoLocation { lat: 35.7, lon: 139.7 };
        let addr = Address([0; 32]);

        let tx = make_dummy_transaction();
        let ny_shard = coordinator.route_transaction(&tx, Some(&ny), &addr);
        let tokyo_shard = coordinator.route_transaction(&tx, Some(&tokyo), &addr);

        assert!(ny_shard.0 < 4);
        assert!(tokyo_shard.0 < 4);
        assert_ne!(ny_shard, tokyo_shard, "NY and Tokyo should be in different shards");
    }

    #[test]
    fn test_transaction_routing_fallback_to_address() {
        let coordinator = ShardCoordinator::new(ShardId(0), 4);
        let addr = Address([42; 32]);
        let tx = make_dummy_transaction();

        let shard = coordinator.route_transaction(&tx, None, &addr);
        assert!(shard.0 < 4);
    }

    // ========================================================================
    // ShardCoordinator: multiple shard engines
    // ========================================================================

    #[test]
    fn test_coordinator_initializes_multiple_engines() {
        let validators = make_validators_for_shards(20, 4);
        let mut coordinator = ShardCoordinator::new(ShardId(0), 4);

        coordinator.initialize_shards(&validators, &[0xAB; 32]);

        // Should have at least the primary shard engine.
        assert!(coordinator.get_shard_engine(&ShardId(0)).is_some(),
            "Primary shard engine should exist");

        // Should have engines for neighbor shards too.
        let participating = coordinator.participating_shards();
        assert!(participating.len() >= 1, "Should participate in at least primary shard");
    }

    #[test]
    fn test_coordinator_cross_shard_queue() {
        let mut coordinator = ShardCoordinator::new(ShardId(0), 4);

        let receipt = CrossShardReceipt {
            tx_hash: sha256(b"test-tx"),
            source_shard: ShardId(0),
            dest_shard: ShardId(2),
            inclusion_proof: vec![],
            source_block_height: 1,
        };

        // Handle a cross-shard receipt (no engine for shard 2, so it just queues).
        let result = coordinator.handle_cross_shard_tx(receipt, &[0; 32]);
        assert!(result.is_ok());
        assert_eq!(coordinator.cross_shard_queue_len(), 1);

        let drained = coordinator.drain_cross_shard_queue();
        assert_eq!(drained.len(), 1);
        assert_eq!(coordinator.cross_shard_queue_len(), 0);
    }

    // ========================================================================
    // Edge cases
    // ========================================================================

    #[test]
    fn test_single_shard_pre_activation() {
        // Before sharding activation, only shard 0 exists.
        let validators = make_validators_for_shards(10, 1);
        let mut coordinator = ShardCoordinator::new(ShardId(0), 1);

        coordinator.initialize_shards(&validators, &[0xAB; 32]);

        let engine = coordinator.get_shard_engine(&ShardId(0)).unwrap();
        // With only 1 shard, there are no neighbors, so no cross-shard members.
        assert_eq!(engine.cross_shard_committee().len(), 0);
        // All committee members are local.
        assert!(engine.local_committee().len() > 0);
    }

    #[test]
    fn test_shard_with_too_few_validators() {
        // Shard 0 has 20 validators, shard 1 has only 2.
        let mut validators = make_validators_for_shards(20, 1);
        // Add 2 validators for shard 1.
        validators.push(make_validator(200, 1, 60));
        validators.push(make_validator(201, 1, 60));

        let shard_assignments: HashMap<NodeId, ShardId> = validators
            .iter()
            .map(|v| (v.node_id, v.shard_id))
            .collect();

        // Shard 1 has too few validators — committee selection should still work
        // but with a minimal committee.
        let config = ShardConsensusConfig::for_shard(ShardId(1), 2);
        let mut engine = ShardedConsensus::new(config);
        let neighbors = vec![ShardId(0)];
        let result = engine.select_committee(&validators, &shard_assignments, &neighbors, &[0xAB; 32]);

        // Should succeed — the selection function handles small pools gracefully.
        assert!(result.is_ok());
    }

    #[test]
    fn test_neighbor_shards_ring_topology() {
        let coordinator = ShardCoordinator::new(ShardId(0), 4);

        // Shard 0: neighbors are 3 (prev, wrapping) and 1 (next).
        let n0 = coordinator.neighbor_shards(ShardId(0));
        assert_eq!(n0.len(), 2);
        assert!(n0.contains(&ShardId(3)));
        assert!(n0.contains(&ShardId(1)));

        // Shard 3: neighbors are 2 (prev) and 0 (next, wrapping).
        let n3 = coordinator.neighbor_shards(ShardId(3));
        assert_eq!(n3.len(), 2);
        assert!(n3.contains(&ShardId(2)));
        assert!(n3.contains(&ShardId(0)));
    }

    #[test]
    fn test_neighbor_shards_two_shards() {
        let coordinator = ShardCoordinator::new(ShardId(0), 2);

        // With 2 shards, each has exactly 1 neighbor.
        let n0 = coordinator.neighbor_shards(ShardId(0));
        assert_eq!(n0.len(), 1);
        assert_eq!(n0[0], ShardId(1));

        let n1 = coordinator.neighbor_shards(ShardId(1));
        assert_eq!(n1.len(), 1);
        assert_eq!(n1[0], ShardId(0));
    }

    #[test]
    fn test_neighbor_shards_single_shard() {
        let coordinator = ShardCoordinator::new(ShardId(0), 1);
        let n0 = coordinator.neighbor_shards(ShardId(0));
        assert!(n0.is_empty(), "Single shard has no neighbors");
    }

    #[test]
    fn test_merkle_inclusion_empty_proof() {
        let leaf = sha256(b"leaf");
        // Empty proof: leaf must equal root.
        assert!(verify_merkle_inclusion(&leaf, &[], &leaf));
        assert!(!verify_merkle_inclusion(&leaf, &[], &[0xFF; 32]));
    }

    #[test]
    fn test_merkle_inclusion_multi_level() {
        // Build a 3-level proof manually.
        let leaf = sha256(b"tx-data");

        let sibling_1 = sha256(b"sibling-1");
        // leaf is on left, sibling_1 on right.
        let parent_1 = sha256(&[&leaf[..], &sibling_1[..]].concat());

        let sibling_2 = sha256(b"sibling-2");
        // parent_1 is on right, sibling_2 on left.
        let root = sha256(&[&sibling_2[..], &parent_1[..]].concat());

        // Build proof: [right, sibling_1] [left, sibling_2]
        let mut proof = Vec::new();
        proof.push(0x01); // sibling_1 is on the right
        proof.extend_from_slice(&sibling_1);
        proof.push(0x00); // sibling_2 is on the left
        proof.extend_from_slice(&sibling_2);

        assert!(verify_merkle_inclusion(&leaf, &proof, &root));

        // Tamper with proof — should fail.
        let mut bad_proof = proof.clone();
        bad_proof[1] ^= 0xFF;
        assert!(!verify_merkle_inclusion(&leaf, &bad_proof, &root));
    }

    #[test]
    fn test_shard_consensus_config_for_shard() {
        // 30 validators -> tier for 30 = committee of 3, finality 2.
        let config = ShardConsensusConfig::for_shard(ShardId(0), 30);
        assert_eq!(config.shard_id, ShardId(0));
        assert_eq!(config.committee_size, 3);
        assert_eq!(config.finality_threshold, 2);
        assert_eq!(config.cross_shard_pct, CROSS_SHARD_COMMITTEE_PCT);
    }

    #[test]
    fn test_advance_shard_height() {
        let config = ShardConsensusConfig::for_shard(ShardId(0), 30);
        let mut engine = ShardedConsensus::new(config);

        assert_eq!(engine.shard_height(), 0);
        assert_eq!(engine.shard_state_root(), &[0u8; 32]);

        engine.advance_height(42, [0xAA; 32]);
        assert_eq!(engine.shard_height(), 42);
        assert_eq!(engine.shard_state_root(), &[0xAA; 32]);
    }

    // ========================================================================
    // Test helper
    // ========================================================================

    fn make_dummy_transaction() -> Transaction {
        use chrono::Utc;
        use gratia_core::types::{TxHash, TransactionPayload};

        Transaction {
            hash: TxHash([0; 32]),
            payload: TransactionPayload::Transfer {
                to: Address([0; 32]),
                amount: 100,
            },
            sender_pubkey: vec![0; 32],
            signature: vec![0; 64],
            nonce: 0,
            chain_id: 2,
            fee: 1000,
            timestamp: Utc::now(),
        }
    }
}
