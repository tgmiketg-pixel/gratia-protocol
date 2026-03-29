//! Gratia Consensus Engine
//!
//! Implements the Gratia blockchain consensus mechanism:
//! - **VRF-based block producer selection** weighted by Composite Presence Score
//! - **21-validator committee** with epoch-based rotation
//! - **14/21 finality threshold** (67% Byzantine fault tolerance)
//! - **3-5 second block time** with 256 KB maximum block size
//!
//! The consensus engine processes incoming blocks, determines when this node
//! should produce a block, tracks finality, and manages committee epochs.

pub mod vrf;
pub mod committee;
pub mod block_production;
pub mod validation;
pub mod sharded_consensus;
pub mod sync;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tracing::{debug, info, warn};

use gratia_core::crypto::Keypair;
use gratia_core::error::GratiaError;
use gratia_core::types::{
    Block, BlockHash, BlockHeader, NodeId, ProofOfLifeAttestation,
    Transaction, ValidatorSignature,
};

use crate::block_production::{BlockProducer, PendingBlock, sign_block};
use crate::committee::{
    EligibleNode, ValidatorCommittee,
};
use crate::validation::ValidationContext;
use crate::vrf::VrfSecretKey;

// ============================================================================
// Constants
// ============================================================================

/// Target block time in seconds.
/// WHY: 4 seconds (middle of the 3-5 second range) balances finality speed
/// against propagation time on mobile networks.
pub const TARGET_BLOCK_TIME_SECS: u64 = 4;

/// Maximum number of finalized blocks to keep in memory.
/// WHY: Mobile devices have limited RAM. Older blocks are persisted to
/// RocksDB by the state layer and pruned from memory.
const MAX_RECENT_BLOCKS: usize = 128;

// ============================================================================
// Consensus Engine
// ============================================================================

/// The current state of the consensus engine.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ConsensusState {
    /// Engine is initializing, syncing state.
    Syncing,
    /// Engine is caught up and participating in consensus.
    Active,
    /// Engine is producing a block for the current slot.
    Producing,
    /// Engine has stopped (e.g., PoL expired, node shutting down).
    Stopped,
}

/// The main consensus engine that coordinates block production,
/// validation, committee management, and finality tracking.
pub struct ConsensusEngine {
    /// This node's identity.
    node_id: NodeId,
    /// This node's VRF secret key (derived from Ed25519 identity key).
    vrf_secret_key: VrfSecretKey,
    /// The block producer for this node.
    block_producer: BlockProducer,
    /// The current validator committee.
    current_committee: Option<ValidatorCommittee>,
    /// Current slot number.
    current_slot: u64,
    /// Current block height (height of the last finalized block).
    current_height: u64,
    /// Hash of the last finalized block.
    last_finalized_hash: BlockHash,
    /// Timestamp of the last finalized block (for monotonicity validation).
    last_finalized_timestamp: Option<DateTime<Utc>>,
    /// Recent finalized block hashes for fork detection.
    recent_block_hashes: Vec<BlockHash>,
    /// Pending block awaiting signatures (if this node is producing).
    /// WHY: pub so the FFI layer can inspect signature count and block header
    /// for BFT finality tracking (checking sigs, reading header for co-signing).
    pub pending_block: Option<PendingBlock>,
    /// Current engine state.
    state: ConsensusState,
    /// This node's Composite Presence Score.
    presence_score: u8,
    /// Whether trust-based filtering is enabled for committee selection.
    /// WHY: When true, the engine logs trust-tier statistics during committee
    /// initialization (e.g., how many nodes are committee-eligible at 30+ days).
    /// Can be disabled in test harnesses that don't need trust-tier awareness.
    pub trust_aware: bool,
    /// Timestamp when the engine started. Used for uptime tracking (Phase 2).
    #[allow(dead_code)]
    started_at: DateTime<Utc>,
}

impl ConsensusEngine {
    /// Create a new consensus engine for this node.
    pub fn new(
        node_id: NodeId,
        signing_key_bytes: &[u8; 32],
        presence_score: u8,
    ) -> Self {
        let vrf_secret_key = VrfSecretKey::from_ed25519_bytes(signing_key_bytes);

        ConsensusEngine {
            node_id,
            vrf_secret_key,
            block_producer: BlockProducer::new(node_id, 0, 0),
            current_committee: None,
            current_slot: 0,
            current_height: 0,
            last_finalized_hash: BlockHash::default(),
            last_finalized_timestamp: None,
            recent_block_hashes: Vec::new(),
            pending_block: None,
            state: ConsensusState::Syncing,
            presence_score,
            trust_aware: true,
            started_at: Utc::now(),
        }
    }

    /// Get the current consensus state.
    pub fn state(&self) -> ConsensusState {
        self.state
    }

    /// Get the current slot number.
    pub fn current_slot(&self) -> u64 {
        self.current_slot
    }

    /// Get the current block height.
    pub fn current_height(&self) -> u64 {
        self.current_height
    }

    /// Get the hash of the last finalized block.
    pub fn last_finalized_hash(&self) -> &BlockHash {
        &self.last_finalized_hash
    }

    /// Get the current committee, if one is active.
    pub fn committee(&self) -> Option<&ValidatorCommittee> {
        self.current_committee.as_ref()
    }

    /// Check if this node is on the current validator committee.
    pub fn is_committee_member(&self) -> bool {
        self.current_committee
            .as_ref()
            .map(|c| c.is_committee_member(&self.node_id))
            .unwrap_or(false)
    }

    /// Initialize the committee from a set of eligible nodes and a seed.
    /// Called during startup or after syncing to the chain tip.
    pub fn initialize_committee(
        &mut self,
        eligible_nodes: &[EligibleNode],
        epoch_seed: &[u8; 32],
        epoch_number: u64,
        start_slot: u64,
    ) -> Result<(), GratiaError> {
        // Log trust-tier breakdown when trust-aware mode is active.
        // WHY: Operators need visibility into how many nodes in the pool
        // actually meet the 30-day committee-eligibility threshold vs total
        // submitted, to detect trust-tier distribution issues early.
        if self.trust_aware {
            let committee_eligible = eligible_nodes
                .iter()
                .filter(|n| n.is_committee_eligible())
                .count();
            info!(
                total = eligible_nodes.len(),
                committee_eligible = committee_eligible,
                below_threshold = eligible_nodes.len() - committee_eligible,
                "Trust-aware committee pool breakdown (30+ day threshold)",
            );
        }

        let committee = committee::select_committee(
            eligible_nodes,
            epoch_seed,
            epoch_number,
            start_slot,
        )?;

        info!(
            epoch = epoch_number,
            members = committee.size(),
            is_member = committee.is_committee_member(&self.node_id),
            "Committee initialized",
        );

        self.current_committee = Some(committee);
        self.current_slot = start_slot;
        self.state = ConsensusState::Active;
        Ok(())
    }

    /// Advance to the next slot. Called by the slot timer (every ~4 seconds).
    ///
    /// Returns `true` if this node should produce a block in the new slot.
    pub fn advance_slot(&mut self) -> bool {
        self.current_slot += 1;
        self.block_producer.set_slot(self.current_slot);

        // Check if committee rotation is needed
        if let Some(ref committee) = self.current_committee {
            if committee::should_rotate(committee, self.current_slot) {
                debug!(
                    slot = self.current_slot,
                    epoch = committee.epoch.epoch_number,
                    "Committee rotation needed",
                );
                // Rotation happens via rotate_committee(), called externally
                // with the last block hash when available.
            }
        }

        // Check if this node should produce
        if let Some(ref committee) = self.current_committee {
            if self.block_producer.should_produce_block(committee) {
                self.state = ConsensusState::Producing;
                info!(
                    slot = self.current_slot,
                    "This node is the block producer for this slot",
                );
                return true;
            }
        }

        false
    }

    /// Produce a block for the current slot.
    ///
    /// Should only be called when `advance_slot()` returns `true`.
    pub fn produce_block(
        &mut self,
        transactions: Vec<Transaction>,
        attestations: Vec<ProofOfLifeAttestation>,
        state_root: [u8; 32],
    ) -> Result<&PendingBlock, GratiaError> {
        if self.state != ConsensusState::Producing {
            return Err(GratiaError::BlockValidationFailed {
                reason: "Not in producing state".into(),
            });
        }

        let height = self.current_height + 1;

        let committee = self.current_committee.as_ref().ok_or_else(|| {
            GratiaError::BlockValidationFailed {
                reason: "No active committee for block production".into(),
            }
        })?;

        let pending = self.block_producer.produce_block(
            transactions,
            attestations,
            self.last_finalized_hash,
            height,
            state_root,
            &self.vrf_secret_key,
            committee,
        )?;

        info!(
            height = height,
            slot = self.current_slot,
            tx_count = pending.block.transactions.len(),
            "Block produced",
        );

        self.pending_block = Some(pending);
        Ok(self.pending_block.as_ref().unwrap())
    }

    /// Get the finality threshold for the pending block (if any).
    pub fn pending_finality_threshold(&self) -> usize {
        self.pending_block
            .as_ref()
            .map(|p| p.finality_threshold)
            .unwrap_or(0)
    }

    /// Add a committee member's signature to the pending block.
    pub fn add_block_signature(
        &mut self,
        signature: ValidatorSignature,
    ) -> Result<bool, GratiaError> {
        let pending = self.pending_block.as_mut().ok_or_else(|| {
            GratiaError::BlockValidationFailed {
                reason: "No pending block to sign".into(),
            }
        })?;

        // Verify the signer is a committee member
        if let Some(ref committee) = self.current_committee {
            if !committee.is_committee_member(&signature.validator) {
                return Err(GratiaError::BlockValidationFailed {
                    reason: format!(
                        "Signer {} is not a committee member",
                        signature.validator,
                    ),
                });
            }
        }

        pending.add_signature(signature)?;

        if pending.is_finalized() {
            debug!(
                slot = self.current_slot,
                signatures = pending.signatures.len(),
                "Block reached finality",
            );
            return Ok(true);
        }

        Ok(false)
    }

    /// Finalize the pending block and advance the chain.
    pub fn finalize_pending_block(&mut self) -> Result<Block, GratiaError> {
        let pending = self.pending_block.take().ok_or_else(|| {
            GratiaError::BlockValidationFailed {
                reason: "No pending block to finalize".into(),
            }
        })?;

        let block = pending.finalize()?;
        let block_hash = block.header.hash()?;

        self.current_height = block.header.height;
        self.last_finalized_hash = block_hash;
        self.last_finalized_timestamp = Some(block.header.timestamp);
        self.recent_block_hashes.push(block_hash);

        // Prune old hashes to bound memory usage
        if self.recent_block_hashes.len() > MAX_RECENT_BLOCKS {
            let drain_count = self.recent_block_hashes.len() - MAX_RECENT_BLOCKS;
            self.recent_block_hashes.drain(..drain_count);
        }

        self.state = ConsensusState::Active;

        info!(
            height = self.current_height,
            hash = %block_hash,
            "Block finalized",
        );

        Ok(block)
    }

    /// Process an incoming block from the network.
    ///
    /// Validates the block and, if valid, updates the chain state.
    pub fn process_incoming_block(&mut self, block: Block) -> Result<(), GratiaError> {
        let incoming_height = block.header.height;

        // WHY: In a multi-node demo where both phones produce blocks independently,
        // chains can diverge. Rather than strict validation that rejects all out-of-
        // sequence blocks, we handle common cases gracefully:
        //
        // 1. Block at expected height → full validation and accept
        // 2. Block at or below our height → skip (we're ahead or already have it)
        // 3. Block ahead of us → accept with relaxed validation (fast-forward sync)
        //
        // This makes the 2-phone demo work smoothly without a full sync protocol.

        let expected_height = self.current_height + 1;

        if incoming_height <= self.current_height {
            // Already at or past this height — skip silently.
            debug!(
                incoming = incoming_height,
                local = self.current_height,
                "Skipping incoming block at or below our height",
            );
            return Ok(());
        }

        let committee = self.current_committee.as_ref().ok_or_else(|| {
            GratiaError::BlockValidationFailed {
                reason: "No active committee for validation".into(),
            }
        })?;

        if incoming_height == expected_height {
            // Normal case: block at expected next height.
            let ctx = ValidationContext {
                current_height: expected_height,
                previous_block_hash: self.last_finalized_hash.0,
                committee: committee.clone(),
                max_block_size: validation::MAX_BLOCK_SIZE,
                min_transaction_fee: validation::MIN_TRANSACTION_FEE,
                previous_block_timestamp: self.last_finalized_timestamp,
                pol_thresholds: gratia_zk::PolThresholds::default(),
            };
            validation::validate_block(&block, &ctx)?;
        } else {
            // WHY: Block is ahead of us (height gap). In Phase 1 demo mode,
            // accept with relaxed validation — just check producer is in
            // committee, finality signatures exist, and size is OK. Skip
            // strict height/parent hash checks. A full sync protocol (Phase 2)
            // would request the intermediate blocks.
            let block_bytes = bincode::serialize(&block)
                .map_err(|e| GratiaError::SerializationError(e.to_string()))?;
            if block_bytes.len() > validation::MAX_BLOCK_SIZE {
                return Err(GratiaError::BlockValidationFailed {
                    reason: "Block too large".into(),
                });
            }
            if !committee.is_committee_member(&block.header.producer) {
                return Err(GratiaError::BlockValidationFailed {
                    reason: format!(
                        "Block producer {} is not a committee member",
                        block.header.producer,
                    ),
                });
            }
            let sig_count = block.validator_signatures.len();
            if !committee.has_finality(sig_count) {
                return Err(GratiaError::InsufficientSignatures {
                    count: sig_count,
                    required: crate::committee::FINALITY_THRESHOLD,
                });
            }
            info!(
                incoming = incoming_height,
                local = self.current_height,
                gap = incoming_height - self.current_height,
                "Fast-forwarding to peer block (skipping {} heights)",
                incoming_height - self.current_height,
            );
        }

        // Block is valid — update state
        let block_hash = block.header.hash()?;
        self.current_height = block.header.height;
        self.last_finalized_hash = block_hash;
        self.last_finalized_timestamp = Some(block.header.timestamp);
        self.recent_block_hashes.push(block_hash);

        if self.recent_block_hashes.len() > MAX_RECENT_BLOCKS {
            let drain_count = self.recent_block_hashes.len() - MAX_RECENT_BLOCKS;
            self.recent_block_hashes.drain(..drain_count);
        }

        // If we were producing, cancel our pending block
        if self.pending_block.is_some() {
            warn!(
                slot = self.current_slot,
                "Received valid block while producing — cancelling our block",
            );
            self.pending_block = None;
        }

        self.state = ConsensusState::Active;

        debug!(
            height = self.current_height,
            hash = %block_hash,
            producer = %block.header.producer,
            "Accepted incoming block",
        );

        Ok(())
    }

    /// Rotate the committee for a new epoch.
    pub fn rotate_committee(
        &mut self,
        eligible_nodes: &[EligibleNode],
    ) -> Result<(), GratiaError> {
        let current = self.current_committee.as_ref().ok_or_else(|| {
            GratiaError::BlockValidationFailed {
                reason: "No current committee to rotate from".into(),
            }
        })?;

        let new_committee = committee::rotate_committee(
            eligible_nodes,
            current,
            &self.last_finalized_hash.0,
        )?;

        info!(
            old_epoch = current.epoch.epoch_number,
            new_epoch = new_committee.epoch.epoch_number,
            is_member = new_committee.is_committee_member(&self.node_id),
            "Committee rotated",
        );

        self.current_committee = Some(new_committee);
        Ok(())
    }

    /// Update this node's presence score.
    pub fn set_presence_score(&mut self, score: u8) {
        self.presence_score = score;
    }

    /// Update network statistics for block production.
    pub fn update_network_stats(&mut self, active_miners: u64, geographic_diversity: u16) {
        self.block_producer
            .update_network_stats(active_miners, geographic_diversity);
    }

    /// Restore chain state from persistence.
    /// WHY: On app restart, the chain height and tip hash are loaded from
    /// file storage so the consensus engine continues from where it left off
    /// instead of restarting from genesis.
    pub fn restore_state(&mut self, height: u64, tip_hash: BlockHash) {
        self.current_height = height;
        self.last_finalized_hash = tip_hash;
        info!(
            height = height,
            hash = %tip_hash,
            "Consensus state restored from persistence"
        );
    }

    /// Stop the consensus engine.
    pub fn stop(&mut self) {
        info!("Consensus engine stopping");
        self.state = ConsensusState::Stopped;
        self.pending_block = None;
    }

    /// Check if a block hash is in our recent history (for fork detection).
    pub fn has_recent_block(&self, hash: &BlockHash) -> bool {
        self.recent_block_hashes.contains(hash)
    }

    /// Get the number of slots remaining in the current epoch.
    pub fn slots_remaining_in_epoch(&self) -> Option<u64> {
        self.current_committee.as_ref().map(|c| {
            if self.current_slot >= c.epoch.end_slot {
                0
            } else {
                c.epoch.end_slot - self.current_slot
            }
        })
    }

    /// Sign a block as a committee member (for blocks produced by other nodes).
    pub fn sign_block_as_validator(
        &self,
        header: &BlockHeader,
        keypair: &Keypair,
    ) -> Result<ValidatorSignature, GratiaError> {
        if !self.is_committee_member() {
            return Err(GratiaError::BlockValidationFailed {
                reason: "This node is not a committee member".into(),
            });
        }

        sign_block(header, self.node_id, keypair)
    }
}

// ============================================================================
// Fork Resolution
// ============================================================================

/// The outcome of a fork resolution comparison.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ForkChoice {
    /// Keep our current block — it has equal or stronger finality.
    KeepOurs,
    /// Switch to the alternative block — it has stronger finality.
    SwitchToTheirs,
    /// Neither block has enough information to decide. Wait for more signatures.
    NeedMoreInfo,
}

/// Resolves forks by comparing competing blocks at the same height.
///
/// WHY: In a BFT system, two producers may occasionally propose blocks at the
/// same height (e.g., network partition, slot timing skew). The fork resolver
/// provides a deterministic rule so all honest nodes converge on the same chain:
///
/// 1. Block with MORE valid committee signatures wins (stronger finality).
/// 2. Tie-break: lower block hash wins (deterministic, unpredictable).
/// 3. If neither block is finalized, wait for more signatures.
pub struct ForkResolver;

impl ForkResolver {
    /// Compare two competing blocks at the same height and decide which to keep.
    ///
    /// `our_sigs` and `their_sigs` are the number of valid committee signatures
    /// each block has collected.
    pub fn resolve_fork(
        our_block: &Block,
        their_block: &Block,
        our_sigs: usize,
        their_sigs: usize,
        finality_threshold: usize,
    ) -> ForkChoice {
        let our_height = our_block.header.height;
        let their_height = their_block.header.height;

        // WHY: Fork resolution only applies to blocks at the same height.
        // Different heights are handled by normal chain selection (highest wins).
        if our_height != their_height {
            if their_height > our_height {
                return ForkChoice::SwitchToTheirs;
            } else {
                return ForkChoice::KeepOurs;
            }
        }

        // Rule 1: More signatures wins (stronger finality evidence).
        if their_sigs > our_sigs && their_sigs >= finality_threshold {
            info!(
                height = our_height,
                our_sigs = our_sigs,
                their_sigs = their_sigs,
                "Fork resolved: switching to block with more signatures"
            );
            return ForkChoice::SwitchToTheirs;
        }

        if our_sigs > their_sigs && our_sigs >= finality_threshold {
            return ForkChoice::KeepOurs;
        }

        // Rule 2: Tie-break by block hash (deterministic).
        if our_sigs == their_sigs && our_sigs >= finality_threshold {
            let our_hash = our_block.header.hash().unwrap_or_default();
            let their_hash = their_block.header.hash().unwrap_or_default();

            // WHY: Lower hash wins. Since block hashes include VRF output and
            // timestamps, this is effectively random but deterministic — all
            // nodes seeing the same two blocks will pick the same winner.
            if their_hash.0 < our_hash.0 {
                info!(
                    height = our_height,
                    "Fork resolved: switching to block with lower hash (tie-break)"
                );
                return ForkChoice::SwitchToTheirs;
            } else {
                return ForkChoice::KeepOurs;
            }
        }

        // Neither block has reached finality — need more signatures.
        ForkChoice::NeedMoreInfo
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::committee::{EligibleNode, COMMITTEE_SIZE, SLOTS_PER_EPOCH};
    use crate::vrf::VrfPublicKey;

    fn make_eligible_nodes(count: u8) -> Vec<EligibleNode> {
        (0..count)
            .map(|i| {
                let mut node_id = [0u8; 32];
                node_id[0] = i;
                EligibleNode {
                    node_id: NodeId(node_id),
                    vrf_pubkey: VrfPublicKey { bytes: [i; 32] },
                    presence_score: 60,
                    has_valid_pol: true,
                    meets_minimum_stake: true,
                    pol_days: 90,
                }
            })
            .collect()
    }

    fn make_engine(node_byte: u8) -> ConsensusEngine {
        let mut node_id = [0u8; 32];
        node_id[0] = node_byte;
        let signing_key = [node_byte; 32];
        ConsensusEngine::new(NodeId(node_id), &signing_key, 60)
    }

    #[test]
    fn test_engine_initial_state() {
        let engine = make_engine(0);
        assert_eq!(engine.state(), ConsensusState::Syncing);
        assert_eq!(engine.current_slot(), 0);
        assert_eq!(engine.current_height(), 0);
        assert!(engine.committee().is_none());
        assert!(!engine.is_committee_member());
    }

    #[test]
    fn test_initialize_committee() {
        let mut engine = make_engine(0);
        let nodes = make_eligible_nodes(25);
        let seed = [0xAB; 32];

        let result = engine.initialize_committee(&nodes, &seed, 0, 0);
        assert!(result.is_ok());
        assert_eq!(engine.state(), ConsensusState::Active);
        assert!(engine.committee().is_some());
        // 25 eligible nodes → tier for 25 = committee of 3
        assert_eq!(engine.committee().unwrap().size(), 3);
    }

    #[test]
    fn test_advance_slot() {
        let mut engine = make_engine(0);
        let nodes = make_eligible_nodes(25);
        engine.initialize_committee(&nodes, &[0xAB; 32], 0, 0).unwrap();

        let initial_slot = engine.current_slot();
        engine.advance_slot();
        assert_eq!(engine.current_slot(), initial_slot + 1);
    }

    #[test]
    fn test_produce_block_requires_producing_state() {
        let mut engine = make_engine(0);
        let nodes = make_eligible_nodes(25);
        engine.initialize_committee(&nodes, &[0xAB; 32], 0, 0).unwrap();

        // Should fail because we haven't advanced to a producing slot
        let result = engine.produce_block(vec![], vec![], [0; 32]);
        assert!(result.is_err());
    }

    #[test]
    fn test_block_production_cycle() {
        let mut engine = make_engine(0);
        let nodes = make_eligible_nodes(25);
        engine.initialize_committee(&nodes, &[0xAB; 32], 0, 0).unwrap();

        // Find the slot where our node produces
        let committee = engine.committee().unwrap().clone();
        let mut producing_slot = None;
        for slot in 0..COMMITTEE_SIZE as u64 {
            if let Some(producer) = committee.block_producer_for_slot(slot) {
                if producer.node_id == engine.node_id {
                    producing_slot = Some(slot);
                    break;
                }
            }
        }

        if let Some(target_slot) = producing_slot {
            // Advance to the producing slot
            for _ in 0..target_slot {
                engine.advance_slot();
            }
            let should_produce = engine.advance_slot();

            if should_produce {
                assert_eq!(engine.state(), ConsensusState::Producing);

                let result = engine.produce_block(vec![], vec![], [0; 32]);
                assert!(result.is_ok());
            }
        }
        // If our node isn't in the committee, this test is a no-op (which is fine)
    }

    #[test]
    fn test_stop_engine() {
        let mut engine = make_engine(0);
        engine.stop();
        assert_eq!(engine.state(), ConsensusState::Stopped);
    }

    #[test]
    fn test_slots_remaining_in_epoch() {
        let mut engine = make_engine(0);
        assert!(engine.slots_remaining_in_epoch().is_none());

        let nodes = make_eligible_nodes(25);
        engine.initialize_committee(&nodes, &[0xAB; 32], 0, 0).unwrap();

        assert_eq!(engine.slots_remaining_in_epoch(), Some(SLOTS_PER_EPOCH));

        engine.advance_slot();
        assert_eq!(engine.slots_remaining_in_epoch(), Some(SLOTS_PER_EPOCH - 1));
    }

    #[test]
    fn test_recent_block_tracking() {
        let engine = make_engine(0);
        let hash = BlockHash([0xAA; 32]);
        assert!(!engine.has_recent_block(&hash));
    }

    #[test]
    fn test_set_presence_score() {
        let mut engine = make_engine(0);
        assert_eq!(engine.presence_score, 60);
        engine.set_presence_score(85);
        assert_eq!(engine.presence_score, 85);
    }

    #[test]
    fn test_update_network_stats() {
        let mut engine = make_engine(0);
        engine.update_network_stats(500, 8);
        // Stats are passed through to block_producer
        assert_eq!(engine.block_producer.active_miners, 500);
        assert_eq!(engine.block_producer.geographic_diversity, 8);
    }

    #[test]
    fn test_sign_block_requires_committee_membership() {
        let engine = make_engine(0);
        let keypair = Keypair::generate();
        let header = BlockHeader {
            height: 1,
            timestamp: Utc::now(),
            parent_hash: BlockHash([0; 32]),
            transactions_root: [0; 32],
            state_root: [0; 32],
            attestations_root: [0; 32],
            producer: NodeId([0; 32]),
            vrf_proof: vec![],
            active_miners: 100,
            geographic_diversity: 5,
        };

        // No committee yet, should fail
        let result = engine.sign_block_as_validator(&header, &keypair);
        assert!(result.is_err());
    }

    #[test]
    fn test_committee_rotation() {
        let mut engine = make_engine(0);
        let nodes = make_eligible_nodes(25);
        engine.initialize_committee(&nodes, &[0xAB; 32], 0, 0).unwrap();

        let initial_epoch = engine.committee().unwrap().epoch.epoch_number;

        // Set a finalized hash for the rotation seed
        engine.last_finalized_hash = BlockHash([0xDD; 32]);

        let result = engine.rotate_committee(&nodes);
        assert!(result.is_ok());

        let new_epoch = engine.committee().unwrap().epoch.epoch_number;
        assert_eq!(new_epoch, initial_epoch + 1);
    }

    #[test]
    fn test_engine_trust_aware_flag() {
        let engine = make_engine(0);
        // trust_aware should default to true per the progressive trust model
        assert!(engine.trust_aware);
    }

    #[test]
    fn test_add_signature_no_pending_block() {
        let mut engine = make_engine(0);
        let sig = ValidatorSignature {
            validator: NodeId([0; 32]),
            signature: vec![0; 64],
        };
        let result = engine.add_block_signature(sig);
        assert!(result.is_err());
    }
}
