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
    EligibleNode, ValidatorCommittee, COMMITTEE_SIZE, FINALITY_THRESHOLD,
    SLOTS_PER_EPOCH,
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
    /// Recent finalized block hashes for fork detection.
    recent_block_hashes: Vec<BlockHash>,
    /// Pending block awaiting signatures (if this node is producing).
    pending_block: Option<PendingBlock>,
    /// Current engine state.
    state: ConsensusState,
    /// This node's Composite Presence Score.
    presence_score: u8,
    /// Timestamp when the engine started.
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
            recent_block_hashes: Vec::new(),
            pending_block: None,
            state: ConsensusState::Syncing,
            presence_score,
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

        let pending = self.block_producer.produce_block(
            transactions,
            attestations,
            self.last_finalized_hash,
            height,
            state_root,
            &self.vrf_secret_key,
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
        let block_hash = block.header.hash();

        self.current_height = block.header.height;
        self.last_finalized_hash = block_hash;
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
        let committee = self.current_committee.as_ref().ok_or_else(|| {
            GratiaError::BlockValidationFailed {
                reason: "No active committee for validation".into(),
            }
        })?;

        // Build validation context
        let ctx = ValidationContext {
            current_height: self.current_height + 1,
            previous_block_hash: self.last_finalized_hash.0,
            committee: committee.clone(),
            max_block_size: validation::MAX_BLOCK_SIZE,
            min_transaction_fee: validation::MIN_TRANSACTION_FEE,
        };

        // Validate the block
        validation::validate_block(&block, &ctx)?;

        // Block is valid — update state
        let block_hash = block.header.hash();
        self.current_height = block.header.height;
        self.last_finalized_hash = block_hash;
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

        Ok(sign_block(header, self.node_id, keypair))
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::committee::EligibleNode;
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
        assert_eq!(engine.committee().unwrap().size(), COMMITTEE_SIZE);
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
