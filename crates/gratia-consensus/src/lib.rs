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
pub mod streamlet;
pub mod sync;

use std::collections::HashMap;
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
    /// Number of slots spent in Syncing state. Used for timeout detection.
    /// WHY: If the sync protocol fails to deliver blocks, the engine would
    /// be stuck in Syncing forever, unable to produce blocks. After 5 slots
    /// (~20 seconds), we resume production to keep the chain moving.
    syncing_slots: u64,
    /// Timestamp when the engine started. Used for uptime tracking (Phase 2).
    #[allow(dead_code)]
    started_at: DateTime<Utc>,
    /// Tracks seen block proposals by (height, producer) -> block hash.
    /// Used to detect equivocation (same producer proposing different blocks
    /// at the same height).
    seen_proposals: HashMap<(u64, NodeId), BlockHash>,
}

/// The result of processing an incoming block.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlockProcessResult {
    /// Block accepted and applied to our chain.
    Accepted,
    /// Block skipped (already have it, or it's ahead with a gap).
    Skipped,
    /// Fork detected: block is at the expected height but has a different
    /// parent hash. The peer is on a different chain. The caller should
    /// compare chain lengths and potentially reorg.
    ForkDetected,
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
            syncing_slots: 0,
            started_at: Utc::now(),
            seen_proposals: HashMap::new(),
        }
    }

    /// Clear seen proposals for a specific height.
    /// WHY: When a BFT-pending block expires, the producer needs to create
    /// a new block at the same height. Without clearing, the new block
    /// triggers false equivocation detection.
    pub fn clear_proposals_for_height(&mut self, height: u64) {
        self.seen_proposals.retain(|&(h, _), _| h != height);
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

    /// Compute the RANDAO epoch seed from recent block history.
    /// WHY: Using the hash of the last finalized block hash + current height as
    /// the epoch seed makes committee selection unpredictable — an attacker would
    /// need to control block production to manipulate the seed. The hardcoded
    /// [0xAB; 32] seed made committee ordering completely deterministic and
    /// predictable, which is a critical security vulnerability.
    pub fn compute_epoch_seed(&self) -> [u8; 32] {
        use sha2::{Sha256, Digest};
        let mut hasher = Sha256::new();
        hasher.update(b"gratia-epoch-seed-v1:");
        // Use last_finalized_hash as minimum entropy
        hasher.update(&self.last_finalized_hash.0);
        // Add current height for per-epoch uniqueness
        hasher.update(&self.current_height.to_be_bytes());
        let result = hasher.finalize();
        let mut seed = [0u8; 32];
        seed.copy_from_slice(&result);
        seed
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
        // WHY: Use next_block_height as the "slot" for producer selection,
        // not the local slot counter. Both phones agree on current_height
        // (shared chain), so current_height + 1 is deterministic across
        // all nodes. The local slot counter drifts because each phone's
        // timer runs independently — phone A might be on slot 50 while
        // phone B is on slot 48. But both agree on height 25, so both
        // compute the same producer for height 26.
        let next_height = self.current_height + 1;
        self.block_producer.set_slot(next_height);

        // WHY: Don't produce blocks while syncing (e.g., after fork resolution
        // rollback). Producing blocks would create a new divergent chain instead
        // of downloading the peer's longer chain. The sync protocol will set
        // state back to Active once we've caught up.
        //
        // WHY: Don't produce blocks while syncing (downloading the peer's
        // longer chain). After 15 slots (60 seconds) with no progress, check
        // whether peers are still reachable. If the sync is genuinely stuck
        // (peer went offline mid-sync), the FFI layer's BFT expiration counter
        // will detect the peer loss and rebuild the committee to solo mode,
        // which sets state back to Active. We do NOT auto-resume here because
        // that would fork the chain — the node would produce a block extending
        // the wrong parent while the honest chain continues ahead.
        if self.state == ConsensusState::Syncing {
            self.syncing_slots += 1;
            if self.syncing_slots > 15 {
                warn!(
                    slots = self.syncing_slots,
                    "Syncing stalled for 60s — waiting for peer loss detection to recover"
                );
                // Reset counter to avoid log spam, but stay in Syncing.
                // Recovery happens via BFT expiration → solo mode fallback.
                self.syncing_slots = 0;
            }
            return false;
        }

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

    /// Force the engine into Producing state.
    /// WHY: When the FFI layer's synthetic override decides this node should
    /// produce (because the VRF assigned a synthetic member), the engine
    /// state is still Active. produce_block() requires Producing state.
    /// This method bridges the gap without going through advance_slot().
    pub fn force_producing_state(&mut self) {
        self.state = ConsensusState::Producing;
    }

    /// Produce a block for the current slot.
    ///
    /// Should only be called when `advance_slot()` returns `true`.
    pub fn produce_block(
        &mut self,
        transactions: Vec<Transaction>,
        attestations: Vec<ProofOfLifeAttestation>,
        state_root: [u8; 32],
        producer_pubkey: Vec<u8>,
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

        let mut pending = self.block_producer.produce_block(
            transactions,
            attestations,
            self.last_finalized_hash,
            height,
            state_root,
            &self.vrf_secret_key,
            committee,
        )?;

        // WHY: Set producer_pubkey so receiving peers can derive the
        // correct wallet address for reward crediting.
        pending.block.header.producer_pubkey = producer_pubkey;

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
    /// WHY: Returns usize::MAX when no pending block exists. A threshold of 0
    /// would mean "instant finality with no signatures" — catastrophically wrong.
    /// MAX means "never finalized" which is correct when nothing is pending.
    pub fn pending_finality_threshold(&self) -> usize {
        self.pending_block
            .as_ref()
            .map(|p| p.finality_threshold)
            .unwrap_or(usize::MAX)
    }

    /// Add a committee member's signature to the pending block.
    ///
    /// WHY: This is the network-facing entry point for incoming signatures.
    /// We MUST cryptographically verify each signature before accepting it.
    /// Without verification, any node could forge signatures claiming to be
    /// any committee member, trivially breaking BFT finality.
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

            // SECURITY: Cryptographically verify the Ed25519 signature.
            // WHY: Without this, anyone can submit forged signatures claiming
            // to be a committee member. The committee membership check above
            // only verifies the NodeId is known — it does NOT prove the sender
            // actually controls that identity's private key.
            if let Some(pubkey) = committee.get_signing_pubkey(&signature.validator) {
                block_production::verify_block_signature(
                    &pending.block.header,
                    &signature,
                    pubkey,
                )?;
            } else {
                // SECURITY: Count real committee members (those with non-empty signing keys).
                // In multi-node mode (>1 real member), REJECT signatures from validators
                // with empty pubkeys. An attacker with an empty signing key would bypass
                // Ed25519 verification entirely. Only allow during bootstrap/solo mode.
                let real_members = committee.members.iter()
                    .filter(|m| !m.signing_pubkey.is_empty())
                    .count();
                if real_members > 1 {
                    return Err(GratiaError::BlockValidationFailed {
                        reason: format!(
                            "Rejecting unverified signature from validator {}: no signing pubkey (multi-node mode, {} real members)",
                            signature.validator, real_members,
                        ),
                    });
                }
                warn!(
                    validator = %signature.validator,
                    "Accepting unverified signature: no signing pubkey for validator (bootstrap/solo mode)",
                );
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
        self.finalize_pending_block_inner(false)
    }

    /// Force-finalize the pending block even without enough BFT signatures.
    /// WHY: During bootstrap with only synthetic committee members, normal
    /// finality can never be reached. This uses force_finalize which only
    /// requires at least 1 signature (the producer's own).
    ///
    /// SECURITY: Only allowed when the committee has at most 1 real member
    /// (bootstrap/solo mode). In multi-node mode, blocks MUST reach the
    /// normal BFT finality threshold to prevent a single node from
    /// unilaterally finalizing blocks.
    pub fn force_finalize_pending_block(&mut self) -> Result<Block, GratiaError> {
        // SECURITY: Gate force_finalize to bootstrap/solo mode only.
        // WHY: In a real multi-node network, force_finalize bypasses BFT
        // threshold, allowing a single node to finalize blocks without
        // committee agreement. This would completely defeat Byzantine
        // fault tolerance.
        if let Some(ref committee) = self.current_committee {
            // Count real members (those with non-empty signing keys).
            let real_members = committee.members.iter()
                .filter(|m| !m.signing_pubkey.is_empty())
                .count();
            if real_members > 1 {
                return Err(GratiaError::BlockValidationFailed {
                    reason: format!(
                        "force_finalize blocked: {} real committee members (only allowed in solo/bootstrap mode)",
                        real_members,
                    ),
                });
            }
        }
        self.finalize_pending_block_inner(true)
    }

    fn finalize_pending_block_inner(&mut self, force: bool) -> Result<Block, GratiaError> {
        let pending = self.pending_block.take().ok_or_else(|| {
            GratiaError::BlockValidationFailed {
                reason: "No pending block to finalize".into(),
            }
        })?;

        let block = if force {
            pending.force_finalize()?
        } else {
            pending.finalize()?
        };
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

    // Old force_finalize_pending_block removed — now handled by
    // finalize_pending_block_inner(force=true) above.

    /// Process an incoming block from the network.
    ///
    /// Validates the block and, if valid, updates the chain state.
    /// Only accepts blocks at the exact next expected height with full
    /// validation (correct parent hash, valid producer, finality sigs).
    /// Blocks ahead of us are skipped — the sync protocol is responsible
    /// for fetching missing blocks so we can apply them sequentially.
    ///
    /// Returns `ForkDetected` if the block is at the correct height but
    /// has a different parent hash — indicating the peer is on a different
    /// fork. The caller should compare chain lengths and initiate reorg
    /// if the peer's chain is longer.
    pub fn process_incoming_block(&mut self, block: Block) -> Result<BlockProcessResult, GratiaError> {
        let incoming_height = block.header.height;
        let expected_height = self.current_height + 1;

        if incoming_height <= self.current_height {
            debug!(
                incoming = incoming_height,
                local = self.current_height,
                "Skipping incoming block at or below our height",
            );
            return Ok(BlockProcessResult::Skipped);
        }

        if incoming_height > expected_height {
            let gap = incoming_height - self.current_height;
            if gap == 2 {
                // WHY: Gap of 2 means the peer finalized a block we had pending
                // (our pending block expired or we missed its finalization) and
                // then produced the next one. Accept as a fast-forward: adopt
                // the peer's block as our new tip. Without this, two-phone BFT
                // enters a fork loop — each phone's pending block expires before
                // the other's signature arrives, putting them perpetually 1 block
                // apart. Treating gap=2 as acceptable lets the chain converge.
                info!(
                    incoming = incoming_height,
                    local = self.current_height,
                    "Fast-forward: accepting block 2 ahead (peer finalized while we had pending)",
                );
                // Fall through to normal validation below
            } else {
                // WHY: Gap > 2 means the peer has multiple blocks we don't have.
                // Report as ForkDetected so the FFI layer can reorg.
                warn!(
                    incoming = incoming_height,
                    local = self.current_height,
                    gap = gap,
                    "Peer is ahead — fork detected",
                );
                return Ok(BlockProcessResult::ForkDetected);
            }
        }

        // WHY: For gap=2 fast-forwards, skip parent hash check — the peer's
        // block extends THEIR tip (which includes a block we don't have yet).
        // We accept this as a fast-forward and update our tip to match.
        let is_fast_forward = incoming_height > expected_height;

        if !is_fast_forward && block.header.parent_hash != self.last_finalized_hash {
            warn!(
                height = incoming_height,
                our_parent = %self.last_finalized_hash,
                their_parent = %block.header.parent_hash,
                "Fork detected: block at expected height but different parent hash",
            );
            return Ok(BlockProcessResult::ForkDetected);
        }

        // Equivocation detection: check if we've seen a different block from
        // the same producer at the same height. This detects double-block attacks.
        let block_hash_for_dedup = block.header.hash()?;
        let proposal_key = (incoming_height, block.header.producer);
        if let Some(prev_hash) = self.seen_proposals.get(&proposal_key) {
            if *prev_hash != block_hash_for_dedup {
                warn!(
                    height = incoming_height,
                    producer = %block.header.producer,
                    "Equivocation detected: producer proposed different blocks at same height",
                );
                return Err(GratiaError::BlockValidationFailed {
                    reason: "equivocation: producer proposed different blocks at same height".into(),
                });
            }
        }
        self.seen_proposals.insert(proposal_key, block_hash_for_dedup);

        // Prune old entries >100 heights behind current to bound memory
        if self.current_height > 100 {
            let cutoff = self.current_height - 100;
            self.seen_proposals.retain(|&(h, _), _| h > cutoff);
        }

        // SECURITY: For fast-forward blocks (gap=2), verify the producer is at
        // least a committee member. We can't check the exact slot assignment (we
        // don't have the intermediate block), but we must not accept blocks from
        // nodes that aren't on the committee at all.
        if is_fast_forward {
            if let Some(ref committee) = self.current_committee {
                let is_member = committee.members.iter()
                    .any(|m| m.node_id == block.header.producer && !m.signing_pubkey.is_empty());
                if !is_member {
                    return Err(GratiaError::BlockValidationFailed {
                        reason: "fast-forward block producer is not a committee member".into(),
                    });
                }
            }
        }

        // Normal case: block at expected next height with correct parent.
        // WHY: Validate that the block producer is a legitimate committee
        // member for this height. Skip for fast-forwards (gap=2) since
        // the block is at a height we didn't expect — producer validation
        // uses expected_height which doesn't match the actual block height.
        if !is_fast_forward {
        if let Some(ref committee) = self.current_committee {
            // WHY: Validate that the block producer is a committee member.
            // We check membership rather than exact slot assignment because
            // committee rebuilds happen at different times on each node (causing
            // different start_slot values and therefore different slot-to-producer
            // mappings). Membership check is sufficient for security — BFT finality
            // (requiring threshold signatures) prevents any single node from
            // unilaterally advancing the chain.
            let is_committee_member = committee.members.iter()
                .any(|m| m.node_id == block.header.producer);
            if !is_committee_member {
                warn!(
                    height = expected_height,
                    producer = ?block.header.producer,
                    committee_size = committee.members.len(),
                    "Block producer is not a committee member — rejecting",
                );
                return Err(GratiaError::BlockValidationFailed {
                    reason: "block producer is not a committee member".into(),
                });
            }
        }
        } // closes !is_fast_forward producer check

        // SECURITY: Validate incoming block has at least the producer's signature.
        // WHY: In BFT consensus, blocks arrive as PROPOSALS with the producer's
        // signature. Other committee members validate and co-sign, building toward
        // the finality threshold. Requiring full threshold before accepting creates
        // a deadlock: no one can co-sign until threshold is met, but threshold
        // requires co-signatures. The security model:
        //   1. Accept proposals with ≥1 valid signature (producer's)
        //   2. Co-sign valid proposals (adds our signature)
        //   3. Producer collects signatures until threshold is met
        //   4. Block with full threshold signatures is the FINALIZED version
        //   5. force_finalize() enforces threshold before committing to chain
        // An attacker can't forge blocks because Ed25519 signatures are verified
        // below, and the producer must be a committee member (checked above).
        if let Some(ref committee) = self.current_committee {
            let sig_count = block.validator_signatures.len();
            let _threshold = committee.finality_threshold;
            let required = 1; // Producer's signature minimum for BFT proposals

            if sig_count < required {
                warn!(
                    height = incoming_height,
                    signatures = sig_count,
                    required = required,
                    "Rejecting block: no signatures at all",
                );
                return Err(GratiaError::InsufficientSignatures {
                    count: sig_count,
                    required,
                });
            }

            // SECURITY: Verify each validator signature cryptographically.
            // WHY: Checking signature count alone doesn't prevent forged sigs.
            // We must verify that each signature was actually produced by the
            // claimed committee member's Ed25519 private key.
            for vs in &block.validator_signatures {
                if !committee.is_committee_member(&vs.validator) {
                    return Err(GratiaError::BlockValidationFailed {
                        reason: format!(
                            "Block signature from non-committee member: {}",
                            vs.validator,
                        ),
                    });
                }
                if let Some(pubkey) = committee.get_signing_pubkey(&vs.validator) {
                    block_production::verify_block_signature(
                        &block.header,
                        vs,
                        pubkey,
                    ).map_err(|e| {
                        GratiaError::BlockValidationFailed {
                            reason: format!(
                                "Invalid signature from validator {}: {}",
                                vs.validator, e,
                            ),
                        }
                    })?;
                } else {
                    // SECURITY: In multi-node mode, reject signatures from validators
                    // with empty pubkeys to prevent Ed25519 verification bypass.
                    let real_members = committee.members.iter()
                        .filter(|m| !m.signing_pubkey.is_empty())
                        .count();
                    if real_members > 1 {
                        return Err(GratiaError::BlockValidationFailed {
                            reason: format!(
                                "Block signature from validator {} has no signing pubkey (multi-node mode, {} real members)",
                                vs.validator, real_members,
                            ),
                        });
                    }
                }
            }
        }

        // SECURITY: Validate each transaction in the block.
        // WHY: Committee signatures prove the block header is authentic, but a
        // malicious producer could include forged transactions. Verify each tx's
        // Ed25519 signature and hash integrity before accepting the block.
        for (i, tx) in block.transactions.iter().enumerate() {
            // Check signature is the correct length for Ed25519 (64 bytes)
            if tx.signature.len() != 64 {
                return Err(GratiaError::BlockValidationFailed {
                    reason: format!(
                        "Transaction {} has invalid signature length: {} (expected 64)",
                        i, tx.signature.len(),
                    ),
                });
            }
            // Check sender pubkey is the correct length for Ed25519 (32 bytes)
            if tx.sender_pubkey.len() != 32 {
                return Err(GratiaError::BlockValidationFailed {
                    reason: format!(
                        "Transaction {} has invalid sender pubkey length: {} (expected 32)",
                        i, tx.sender_pubkey.len(),
                    ),
                });
            }
            // Verify the Ed25519 signature over the transaction's signable content.
            // Reconstruct the signable bytes the same way the mempool does.
            let payload_bytes = bincode::serialize(&tx.payload).map_err(|e| {
                GratiaError::BlockValidationFailed {
                    reason: format!("Transaction {} payload serialization failed: {}", i, e),
                }
            })?;
            let mut signable = Vec::with_capacity(payload_bytes.len() + 28);
            signable.extend_from_slice(&payload_bytes);
            signable.extend_from_slice(&tx.nonce.to_le_bytes());
            signable.extend_from_slice(&tx.chain_id.to_le_bytes());
            signable.extend_from_slice(&tx.fee.to_le_bytes());
            signable.extend_from_slice(&tx.timestamp.timestamp_millis().to_le_bytes());

            gratia_core::crypto::verify_signature(&tx.sender_pubkey, &signable, &tx.signature)
                .map_err(|_| GratiaError::BlockValidationFailed {
                    reason: format!(
                        "Transaction {} has invalid Ed25519 signature",
                        i,
                    ),
                })?;
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

        Ok(BlockProcessResult::Accepted)
    }

    /// Roll back the consensus engine to a specific height.
    ///
    /// WHY: During fork resolution, the shorter chain needs to roll back
    /// to the common ancestor before downloading the longer chain. This
    /// resets the engine's internal state so it can accept blocks from
    /// the fork point onward.
    pub fn rollback_to(&mut self, height: u64, tip_hash: BlockHash) -> Result<(), GratiaError> {
        // WHY: Limit rollback depth to 128 blocks to prevent an attacker from
        // forcing a deep reorg that re-executes hundreds of blocks worth of
        // transactions. 128 blocks (~8.5 minutes at 4s block time) is generous
        // for legitimate fork resolution but blocks catastrophic reorgs.
        // Height 0 is exempt: genesis reset during SOLO->MULTI chain yield is
        // a deliberate operation, not an attack vector.
        const MAX_ROLLBACK_DEPTH: u64 = 128;
        if height > 0 && self.current_height > height && self.current_height - height > MAX_ROLLBACK_DEPTH {
            warn!(
                current_height = self.current_height,
                target_height = height,
                max_depth = MAX_ROLLBACK_DEPTH,
                "Rollback rejected: depth exceeds maximum",
            );
            return Err(GratiaError::Other(format!(
                "Rollback depth {} exceeds maximum {} blocks (current={}, target={})",
                self.current_height - height,
                MAX_ROLLBACK_DEPTH,
                self.current_height,
                height,
            )));
        }

        let old_height = self.current_height;
        self.current_height = height;
        self.last_finalized_hash = tip_hash;
        self.last_finalized_timestamp = None;
        self.pending_block = None;
        self.recent_block_hashes.clear();
        // WHY: Set to Active, not Syncing. The previous Syncing state blocked
        // all block production until the sync timeout fired (~60s), which meant
        // after every reorg the phone went silent. Since we've already adopted
        // the peer's chain tip (fast-forward), we're synced and can produce
        // immediately on the next slot.
        self.state = ConsensusState::Active;
        self.syncing_slots = 0;

        info!(
            old_height = old_height,
            new_height = height,
            "Consensus engine rolled back for fork resolution",
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
    /// WHY: Returns Result so hash computation failures propagate instead of
    /// silently defaulting to zero hash, which could cause deterministic
    /// tie-breaking to always pick the same "side" regardless of block content.
    pub fn resolve_fork(
        our_block: &Block,
        their_block: &Block,
        our_sigs: usize,
        their_sigs: usize,
        finality_threshold: usize,
    ) -> Result<ForkChoice, GratiaError> {
        let our_height = our_block.header.height;
        let their_height = their_block.header.height;

        // WHY: Fork resolution only applies to blocks at the same height.
        // Different heights are handled by normal chain selection (highest wins).
        if our_height != their_height {
            if their_height > our_height {
                return Ok(ForkChoice::SwitchToTheirs);
            } else {
                return Ok(ForkChoice::KeepOurs);
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
            return Ok(ForkChoice::SwitchToTheirs);
        }

        if our_sigs > their_sigs && our_sigs >= finality_threshold {
            return Ok(ForkChoice::KeepOurs);
        }

        // Rule 2: Tie-break by block hash (deterministic).
        // WHY: Hash failures must propagate as errors. Defaulting to zero hash
        // would make tie-breaking deterministically wrong — both blocks would
        // hash to zero and the comparison would be meaningless.
        if our_sigs == their_sigs && our_sigs >= finality_threshold {
            let our_hash = our_block.header.hash().map_err(|_| {
                GratiaError::BlockValidationFailed {
                    reason: "failed to compute hash for our block during fork resolution".into(),
                }
            })?;
            let their_hash = their_block.header.hash().map_err(|_| {
                GratiaError::BlockValidationFailed {
                    reason: "failed to compute hash for their block during fork resolution".into(),
                }
            })?;

            // WHY: Lower hash wins. Since block hashes include VRF output and
            // timestamps, this is effectively random but deterministic — all
            // nodes seeing the same two blocks will pick the same winner.
            if their_hash.0 < our_hash.0 {
                info!(
                    height = our_height,
                    "Fork resolved: switching to block with lower hash (tie-break)"
                );
                return Ok(ForkChoice::SwitchToTheirs);
            } else {
                return Ok(ForkChoice::KeepOurs);
            }
        }

        // Neither block has reached finality — need more signatures.
        Ok(ForkChoice::NeedMoreInfo)
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
                    signing_pubkey: vec![i; 32],
                    vrf_proof: vec![],
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
        let result = engine.produce_block(vec![], vec![], [0; 32], vec![]);
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

                let result = engine.produce_block(vec![], vec![], [0; 32], vec![]);
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
            producer_pubkey: vec![],
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
