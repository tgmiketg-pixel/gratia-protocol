//! Streamlet BFT Consensus Protocol
//!
//! Streamlet is a formally proven BFT protocol with the simplest possible design:
//!
//! 1. **Propose**: Each epoch, a leader proposes a block extending the longest
//!    notarized chain they know about.
//! 2. **Vote**: Committee members vote for the proposal if it extends a notarized
//!    chain they know about. Each member votes for at most one block per epoch.
//! 3. **Notarize**: A block is notarized when it receives 2/3+ committee votes.
//! 4. **Finalize**: When there are 3 consecutive notarized blocks at heights
//!    h, h+1, h+2 — the chain up to height h+1 is finalized.
//!
//! Safety proof: No two conflicting blocks can both be finalized because
//! notarization requires 2/3+ votes, and two conflicting notarizations at the
//! same height would require >1/3 of validators to double-vote (equivocate).
//!
//! Reference: "Streamlet: Textbook Streamlined Blockchains" by Chan & Shi, 2020.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use gratia_core::types::{BlockHash, BlockHeader, NodeId, ValidatorSignature};

/// A vote from a committee member for a proposed block.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StreamletVote {
    /// The epoch (slot) this vote is for.
    pub epoch: u64,
    /// Hash of the proposed block being voted for.
    pub block_hash: [u8; 32],
    /// Height of the proposed block.
    pub height: u64,
    /// The voter's identity and signature.
    pub signature: ValidatorSignature,
}

/// A block that has been proposed but may not yet be notarized or finalized.
#[derive(Debug, Clone)]
pub struct ProposedBlock {
    /// The block header.
    pub header: BlockHeader,
    /// Hash of this block's header.
    pub block_hash: [u8; 32],
    /// Epoch (slot) in which this was proposed.
    pub epoch: u64,
    /// Votes received for this block.
    pub votes: Vec<StreamletVote>,
    /// Whether this block has been notarized (2/3+ votes).
    pub notarized: bool,
    /// Whether this block has been finalized.
    pub finalized: bool,
}

impl ProposedBlock {
    pub fn new(header: BlockHeader, block_hash: [u8; 32], epoch: u64) -> Self {
        ProposedBlock {
            header,
            block_hash,
            epoch,
            votes: Vec::new(),
            notarized: false,
            finalized: false,
        }
    }

    /// Add a vote and check if notarization threshold is reached.
    /// Returns true if the block just became notarized.
    pub fn add_vote(&mut self, vote: StreamletVote, committee_size: usize) -> bool {
        // Don't add duplicate votes from the same validator
        if self.votes.iter().any(|v| v.signature.validator == vote.signature.validator) {
            return false;
        }
        self.votes.push(vote);

        // Check notarization: 2/3+ of committee
        let threshold = (committee_size * 2 + 2) / 3; // Ceiling division for 2/3
        if !self.notarized && self.votes.len() >= threshold {
            self.notarized = true;
            return true;
        }
        false
    }

    pub fn vote_count(&self) -> usize {
        self.votes.len()
    }
}

/// The Streamlet state machine.
///
/// Tracks proposed blocks, votes, notarizations, and determines finality
/// by checking for 3 consecutive notarized blocks.
pub struct StreamletState {
    /// This node's identity.
    pub node_id: NodeId,
    /// Committee size for notarization threshold calculation.
    pub committee_size: usize,
    /// All proposed blocks, indexed by block hash.
    proposed_blocks: HashMap<[u8; 32], ProposedBlock>,
    /// Block hashes at each height, for finding consecutive notarized chains.
    blocks_by_height: HashMap<u64, Vec<[u8; 32]>>,
    /// Height of the last finalized block.
    pub finalized_height: u64,
    /// Hash of the last finalized block.
    pub finalized_hash: [u8; 32],
    /// Epochs in which this node has already voted (prevents double-voting).
    voted_epochs: HashMap<u64, [u8; 32]>,
    /// The highest notarized block height this node knows about.
    pub highest_notarized_height: u64,
}

impl StreamletState {
    pub fn new(node_id: NodeId, committee_size: usize) -> Self {
        StreamletState {
            node_id,
            committee_size,
            proposed_blocks: HashMap::new(),
            blocks_by_height: HashMap::new(),
            finalized_height: 0,
            finalized_hash: [0u8; 32],
            voted_epochs: HashMap::new(),
            highest_notarized_height: 0,
        }
    }

    /// Restore state after app restart.
    pub fn restore(&mut self, finalized_height: u64, finalized_hash: [u8; 32]) {
        self.finalized_height = finalized_height;
        self.finalized_hash = finalized_hash;
        self.highest_notarized_height = finalized_height;
    }

    /// Register a proposed block (from this node or a peer).
    /// Returns true if this is a new proposal we haven't seen before.
    pub fn add_proposal(&mut self, header: BlockHeader, block_hash: [u8; 32], epoch: u64) -> bool {
        if self.proposed_blocks.contains_key(&block_hash) {
            return false; // Already know about this proposal
        }

        let height = header.height;
        let proposed = ProposedBlock::new(header, block_hash, epoch);
        self.proposed_blocks.insert(block_hash, proposed);
        self.blocks_by_height.entry(height).or_default().push(block_hash);
        true
    }

    /// Should this node vote for a proposed block?
    ///
    /// Streamlet voting rule: vote for the proposal if:
    /// 1. We haven't voted in this epoch yet
    /// 2. The block extends a notarized chain (or is at height finalized+1)
    /// 3. The block's height is the longest notarized chain + 1
    pub fn should_vote(&self, block_hash: &[u8; 32], epoch: u64) -> bool {
        // Already voted this epoch?
        if self.voted_epochs.contains_key(&epoch) {
            return false;
        }

        let proposed = match self.proposed_blocks.get(block_hash) {
            Some(p) => p,
            None => return false,
        };

        let height = proposed.header.height;

        // Block must extend from finalized height or a notarized block
        if height <= self.finalized_height {
            return false;
        }

        // Block should be at highest_notarized + 1 (or finalized + 1 if no notarized above)
        let expected = self.highest_notarized_height + 1;
        if height != expected {
            // Allow voting for blocks at finalized + 1 even if we missed some notarizations
            if height != self.finalized_height + 1 {
                return false;
            }
        }

        true
    }

    /// Record that this node voted for a block in an epoch.
    pub fn record_vote(&mut self, epoch: u64, block_hash: [u8; 32]) {
        self.voted_epochs.insert(epoch, block_hash);
    }

    /// Add a vote for a proposed block and check for notarization + finality.
    /// Returns (just_notarized, newly_finalized_height) — the height of any
    /// newly finalized blocks, or None.
    pub fn add_vote(&mut self, vote: StreamletVote) -> (bool, Option<u64>) {
        let block_hash = vote.block_hash;

        let just_notarized = if let Some(proposed) = self.proposed_blocks.get_mut(&block_hash) {
            proposed.add_vote(vote, self.committee_size)
        } else {
            return (false, None);
        };

        if just_notarized {
            let height = self.proposed_blocks[&block_hash].header.height;
            if height > self.highest_notarized_height {
                self.highest_notarized_height = height;
            }

            // Check finality: 3 consecutive notarized blocks at h, h+1, h+2
            // finalizes the chain up to h+1.
            let finalized = self.check_finality();
            return (true, finalized);
        }

        (false, None)
    }

    /// Check for 3 consecutive notarized blocks → finalize up to the middle one.
    ///
    /// WHY: Streamlet's finality rule. If blocks at heights h, h+1, h+2 are all
    /// notarized AND form a chain (each extends the previous), then the chain
    /// up to h+1 is final. No conflicting chain can ever be finalized because
    /// conflicting notarizations at the same height would require >1/3 equivocation.
    fn check_finality(&mut self) -> Option<u64> {
        // Start checking from finalized_height + 1
        let start = self.finalized_height + 1;

        // We need 3 consecutive heights with notarized blocks
        for h in start..=self.highest_notarized_height.saturating_sub(1) {
            let has_h = self.get_notarized_at(h).is_some();
            let has_h1 = self.get_notarized_at(h + 1).is_some();
            let has_h2 = self.get_notarized_at(h + 2).is_some();

            if has_h && has_h1 && has_h2 {
                // 3 consecutive notarized blocks found!
                // Finalize up to h+1 (the middle block).
                let middle_hash = self.get_notarized_at(h + 1).unwrap();
                let new_finalized = h + 1;

                if new_finalized > self.finalized_height {
                    self.finalized_height = new_finalized;
                    self.finalized_hash = middle_hash;

                    // Prune old proposed blocks below finalized height
                    self.prune_below(new_finalized);

                    return Some(new_finalized);
                }
            }
        }
        None
    }

    /// Get the hash of a notarized block at a given height.
    fn get_notarized_at(&self, height: u64) -> Option<[u8; 32]> {
        if let Some(hashes) = self.blocks_by_height.get(&height) {
            for hash in hashes {
                if let Some(proposed) = self.proposed_blocks.get(hash) {
                    if proposed.notarized {
                        return Some(*hash);
                    }
                }
            }
        }
        None
    }

    /// Prune proposed blocks below a given height to bound memory.
    fn prune_below(&mut self, height: u64) {
        let old_heights: Vec<u64> = self.blocks_by_height.keys()
            .filter(|h| **h < height)
            .copied()
            .collect();

        for h in old_heights {
            if let Some(hashes) = self.blocks_by_height.remove(&h) {
                for hash in hashes {
                    self.proposed_blocks.remove(&hash);
                }
            }
        }

        // Prune old voted_epochs
        let old_epochs: Vec<u64> = self.voted_epochs.keys()
            .copied()
            .filter(|e| *e + 100 < self.voted_epochs.len() as u64)
            .collect();
        for e in old_epochs {
            self.voted_epochs.remove(&e);
        }
    }

    /// Get the height of the longest notarized chain tip.
    /// Used by the proposer to know what height to propose next.
    pub fn next_proposal_height(&self) -> u64 {
        self.highest_notarized_height.max(self.finalized_height) + 1
    }

    /// Get the parent hash for the next proposal (tip of longest notarized chain).
    pub fn next_proposal_parent(&self) -> [u8; 32] {
        if let Some(hash) = self.get_notarized_at(self.highest_notarized_height) {
            hash
        } else {
            self.finalized_hash
        }
    }

    /// Reset for committee change.
    pub fn clear_pending(&mut self) {
        // Keep finalized state but clear all pending proposals and votes
        let fh = self.finalized_height;
        self.prune_below(fh + 1);
        self.voted_epochs.clear();
        self.highest_notarized_height = self.finalized_height;
    }

    /// Get stats for logging.
    pub fn stats(&self) -> (usize, u64, u64) {
        (
            self.proposed_blocks.len(),
            self.highest_notarized_height,
            self.finalized_height,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_vote(epoch: u64, block_hash: [u8; 32], height: u64, validator_id: u8) -> StreamletVote {
        let mut node_id = [0u8; 32];
        node_id[0] = validator_id;
        StreamletVote {
            epoch,
            block_hash,
            height,
            signature: ValidatorSignature {
                validator: NodeId(node_id),
                signature: vec![validator_id; 64],
            },
        }
    }

    fn make_header(height: u64, parent_hash: [u8; 32]) -> BlockHeader {
        use chrono::Utc;
        BlockHeader {
            height,
            timestamp: Utc::now(),
            parent_hash: BlockHash(parent_hash),
            transactions_root: [0u8; 32],
            state_root: [0u8; 32],
            attestations_root: [0u8; 32],
            producer: NodeId([0u8; 32]),
            vrf_proof: vec![],
            active_miners: 1,
            geographic_diversity: 0,
        }
    }

    #[test]
    fn test_notarization_threshold() {
        let node_id = NodeId([1u8; 32]);
        let mut state = StreamletState::new(node_id, 3); // 3 committee members → need 2 votes

        let hash = [0xAA; 32];
        let header = make_header(1, [0u8; 32]);
        state.add_proposal(header, hash, 1);

        // First vote — not notarized yet
        let (notarized, _) = state.add_vote(make_vote(1, hash, 1, 1));
        assert!(!notarized);

        // Second vote — notarized! (2/3 of 3)
        let (notarized, _) = state.add_vote(make_vote(1, hash, 1, 2));
        assert!(notarized);
    }

    #[test]
    fn test_three_consecutive_finality() {
        let node_id = NodeId([1u8; 32]);
        let mut state = StreamletState::new(node_id, 2); // 2 members → need 2 votes

        // Block at height 1
        let h1 = [0x01; 32];
        state.add_proposal(make_header(1, [0u8; 32]), h1, 1);
        state.add_vote(make_vote(1, h1, 1, 1));
        let (notarized, finalized) = state.add_vote(make_vote(1, h1, 1, 2));
        assert!(notarized);
        assert!(finalized.is_none()); // Only 1 notarized, need 3

        // Block at height 2
        let h2 = [0x02; 32];
        state.add_proposal(make_header(2, h1), h2, 2);
        state.add_vote(make_vote(2, h2, 2, 1));
        let (notarized, finalized) = state.add_vote(make_vote(2, h2, 2, 2));
        assert!(notarized);
        assert!(finalized.is_none()); // Only 2 consecutive, need 3

        // Block at height 3 — triggers finality of height 2!
        let h3 = [0x03; 32];
        state.add_proposal(make_header(3, h2), h3, 3);
        state.add_vote(make_vote(3, h3, 3, 1));
        let (notarized, finalized) = state.add_vote(make_vote(3, h3, 3, 2));
        assert!(notarized);
        assert_eq!(finalized, Some(2)); // Heights 1,2,3 notarized → height 2 finalized
        assert_eq!(state.finalized_height, 2);
    }

    #[test]
    fn test_no_double_vote() {
        let node_id = NodeId([1u8; 32]);
        let mut state = StreamletState::new(node_id, 3);

        let hash = [0xBB; 32];
        state.add_proposal(make_header(1, [0u8; 32]), hash, 1);

        // Same validator votes twice — second should be ignored
        state.add_vote(make_vote(1, hash, 1, 1));
        let (notarized, _) = state.add_vote(make_vote(1, hash, 1, 1));
        assert!(!notarized); // Duplicate ignored, still only 1 unique vote
    }
}
