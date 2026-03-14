//! Validator committee management.
//!
//! The Gratia consensus uses a 21-validator committee that produces blocks
//! via VRF-based selection. The committee rotates at epoch boundaries.
//! Selection is weighted by Composite Presence Score (40-100), which affects
//! selection probability but NOT rewards.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use gratia_core::error::GratiaError;
use gratia_core::types::NodeId;

use crate::vrf::{self, VrfProof, VrfPublicKey, VrfSecretKey};

// ============================================================================
// Constants
// ============================================================================

/// Number of validators per committee.
/// WHY: 21 balances decentralization (enough nodes for geographic diversity)
/// against finality speed (14/21 signatures can be collected quickly on mobile networks).
pub const COMMITTEE_SIZE: usize = 21;

/// Number of committee signatures required for finality.
/// WHY: 14/21 = 66.7%, just above the 2/3 Byzantine fault tolerance threshold.
pub const FINALITY_THRESHOLD: usize = 14;

/// Number of slots per epoch before committee rotation.
/// WHY: ~900 slots at 4 seconds each = ~1 hour epochs. Frequent enough to
/// rotate out misbehaving validators, long enough to amortize selection cost.
pub const SLOTS_PER_EPOCH: u64 = 900;

/// Domain separator for committee selection VRF input.
const COMMITTEE_SELECTION_DOMAIN: &[u8] = b"gratia-committee-select-v1";

// ============================================================================
// Types
// ============================================================================

/// Information about a node eligible for committee membership.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EligibleNode {
    /// The node's identity.
    pub node_id: NodeId,
    /// The node's VRF public key.
    pub vrf_pubkey: VrfPublicKey,
    /// Composite Presence Score (40-100).
    pub presence_score: u8,
    /// Whether the node has valid Proof of Life for today.
    pub has_valid_pol: bool,
    /// Whether the node meets the minimum stake requirement.
    pub meets_minimum_stake: bool,
}

impl EligibleNode {
    /// Check if this node is fully eligible for committee membership.
    pub fn is_eligible(&self) -> bool {
        self.has_valid_pol
            && self.meets_minimum_stake
            && self.presence_score >= 40
    }
}

/// A member of the validator committee, with their selection proof.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommitteeMember {
    /// The node's identity.
    pub node_id: NodeId,
    /// The node's VRF public key.
    pub vrf_pubkey: VrfPublicKey,
    /// Composite Presence Score at time of selection.
    pub presence_score: u8,
    /// The VRF proof used during committee selection.
    pub selection_proof: VrfProof,
    /// The weighted selection value (lower = higher priority).
    pub selection_value: f64,
}

/// Tracks the current epoch and its committee.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommitteeEpoch {
    /// The epoch number (0-indexed from genesis).
    pub epoch_number: u64,
    /// The slot at which this epoch started.
    pub start_slot: u64,
    /// The slot at which this epoch ends (exclusive).
    pub end_slot: u64,
    /// Timestamp when this epoch was established.
    pub established_at: DateTime<Utc>,
    /// The randomness seed used for this epoch's committee selection
    /// (hash of the last block in the previous epoch).
    pub seed: [u8; 32],
}

/// The active 21-validator committee for the current epoch.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ValidatorCommittee {
    /// The current epoch info.
    pub epoch: CommitteeEpoch,
    /// The 21 committee members, sorted by selection value (lowest first).
    pub members: Vec<CommitteeMember>,
}

impl ValidatorCommittee {
    /// Check if a node is a member of this committee.
    pub fn is_committee_member(&self, node_id: &NodeId) -> bool {
        self.members.iter().any(|m| m.node_id == *node_id)
    }

    /// Get a committee member by node ID.
    pub fn get_member(&self, node_id: &NodeId) -> Option<&CommitteeMember> {
        self.members.iter().find(|m| m.node_id == *node_id)
    }

    /// Get the block producer for a given slot within this epoch.
    /// The producer is selected by VRF among committee members, with the
    /// slot number providing per-slot randomness.
    ///
    /// For PoC, we use a simple round-robin among committee members based
    /// on slot offset, with VRF determining the starting position.
    pub fn block_producer_for_slot(&self, slot: u64) -> Option<&CommitteeMember> {
        if self.members.is_empty() {
            return None;
        }
        // WHY: Simple modular index within the committee. The committee is already
        // VRF-selected and sorted, so round-robin within the epoch is fair.
        // A more sophisticated per-slot VRF selection could be added for mainnet.
        let offset = (slot - self.epoch.start_slot) as usize;
        let index = offset % self.members.len();
        self.members.get(index)
    }

    /// Check if enough signatures have been collected for finality.
    pub fn has_finality(&self, signature_count: usize) -> bool {
        signature_count >= FINALITY_THRESHOLD
    }

    /// Check if a slot belongs to this epoch.
    pub fn contains_slot(&self, slot: u64) -> bool {
        slot >= self.epoch.start_slot && slot < self.epoch.end_slot
    }

    /// Get the number of members.
    pub fn size(&self) -> usize {
        self.members.len()
    }
}

// ============================================================================
// Committee Selection
// ============================================================================

/// Select a committee of up to 21 validators from all eligible nodes.
///
/// Each eligible node generates a VRF proof using the epoch seed. The nodes
/// with the lowest weighted selection values (VRF output weighted by Presence
/// Score) are chosen for the committee.
///
/// This function is called by each node independently. Because VRF outputs
/// are deterministic and verifiable, all honest nodes will agree on the
/// same committee.
pub fn select_committee(
    eligible_nodes: &[EligibleNode],
    epoch_seed: &[u8; 32],
    epoch_number: u64,
    current_slot: u64,
) -> Result<ValidatorCommittee, GratiaError> {
    // Filter to truly eligible nodes
    let eligible: Vec<&EligibleNode> = eligible_nodes
        .iter()
        .filter(|n| n.is_eligible())
        .collect();

    if eligible.is_empty() {
        return Err(GratiaError::BlockValidationFailed {
            reason: "No eligible nodes for committee selection".into(),
        });
    }

    // Build the VRF input for committee selection
    let mut selection_input = Vec::new();
    selection_input.extend_from_slice(COMMITTEE_SELECTION_DOMAIN);
    selection_input.extend_from_slice(epoch_seed);
    selection_input.extend_from_slice(&epoch_number.to_be_bytes());

    // Each node's "ticket" is their VRF output weighted by presence score.
    // WHY: We simulate each node's VRF evaluation here for committee selection.
    // In a real deployment, each node would submit their own proof and the
    // network would collect and sort them. For PoC, we compute deterministically
    // from the public information.
    let mut candidates: Vec<CommitteeMember> = Vec::with_capacity(eligible.len());

    for node in &eligible {
        // Deterministic pseudo-VRF for committee selection using node ID as entropy.
        // WHY: In a real network, each node would generate their own VRF proof
        // with their secret key. For selection purposes, we use a deterministic
        // hash of the node ID + seed so that all nodes compute the same committee.
        let mut node_input = selection_input.clone();
        node_input.extend_from_slice(&node.node_id.0);

        let output = gratia_core::crypto::sha256(&node_input);

        // Create a synthetic proof for the selection record
        let selection_proof = VrfProof {
            output,
            proof_bytes: Vec::new(), // Placeholder — real proofs submitted by nodes
        };

        let selection_value = vrf::vrf_output_to_selection(&output, node.presence_score);

        candidates.push(CommitteeMember {
            node_id: node.node_id,
            vrf_pubkey: node.vrf_pubkey.clone(),
            presence_score: node.presence_score,
            selection_proof,
            selection_value,
        });
    }

    // Sort by selection value (lowest first = highest priority)
    candidates.sort_by(|a, b| {
        a.selection_value
            .partial_cmp(&b.selection_value)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    // Take the top COMMITTEE_SIZE (or fewer if not enough eligible nodes)
    let committee_size = COMMITTEE_SIZE.min(candidates.len());
    candidates.truncate(committee_size);

    let epoch = CommitteeEpoch {
        epoch_number,
        start_slot: current_slot,
        end_slot: current_slot + SLOTS_PER_EPOCH,
        established_at: Utc::now(),
        seed: *epoch_seed,
    };

    Ok(ValidatorCommittee {
        epoch,
        members: candidates,
    })
}

/// Determine if the committee should rotate (current slot is at or past the epoch end).
pub fn should_rotate(committee: &ValidatorCommittee, current_slot: u64) -> bool {
    current_slot >= committee.epoch.end_slot
}

/// Rotate the committee for a new epoch.
///
/// Uses the hash of the last block in the previous epoch as the new seed,
/// ensuring the committee selection is unpredictable until that block is finalized.
pub fn rotate_committee(
    eligible_nodes: &[EligibleNode],
    previous_committee: &ValidatorCommittee,
    last_block_hash: &[u8; 32],
) -> Result<ValidatorCommittee, GratiaError> {
    let new_epoch_number = previous_committee.epoch.epoch_number + 1;
    let new_start_slot = previous_committee.epoch.end_slot;

    // WHY: Using the last block's hash as the seed ensures the committee
    // selection cannot be predicted or manipulated before that block is finalized.
    let new_seed = gratia_core::crypto::sha256_multi(&[
        b"gratia-epoch-seed-v1:",
        last_block_hash,
        &new_epoch_number.to_be_bytes(),
    ]);

    select_committee(eligible_nodes, &new_seed, new_epoch_number, new_start_slot)
}

/// Verify that a node was legitimately selected for a committee by checking
/// their VRF proof against the epoch seed.
pub fn verify_committee_membership(
    node: &CommitteeMember,
    epoch_seed: &[u8; 32],
    epoch_number: u64,
    vrf_input_override: Option<&[u8]>,
) -> Result<(), GratiaError> {
    // If the proof bytes are empty, this is a deterministic selection (PoC mode)
    if node.selection_proof.proof_bytes.is_empty() {
        // Verify using deterministic hash instead
        let mut selection_input = Vec::new();
        selection_input.extend_from_slice(COMMITTEE_SELECTION_DOMAIN);
        selection_input.extend_from_slice(epoch_seed);
        selection_input.extend_from_slice(&epoch_number.to_be_bytes());
        selection_input.extend_from_slice(&node.node_id.0);

        let expected_output = gratia_core::crypto::sha256(&selection_input);
        if node.selection_proof.output != expected_output {
            return Err(GratiaError::InvalidVrfProof);
        }
        return Ok(());
    }

    // Full VRF proof verification
    let input = match vrf_input_override {
        Some(i) => i.to_vec(),
        None => {
            let mut v = Vec::new();
            v.extend_from_slice(COMMITTEE_SELECTION_DOMAIN);
            v.extend_from_slice(epoch_seed);
            v.extend_from_slice(&epoch_number.to_be_bytes());
            v
        }
    };

    vrf::verify_vrf_proof(&node.vrf_pubkey, &input, &node.selection_proof)?;
    Ok(())
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vrf::VrfPublicKey;

    fn make_eligible_node(id_byte: u8, score: u8) -> EligibleNode {
        let mut node_id = [0u8; 32];
        node_id[0] = id_byte;
        EligibleNode {
            node_id: NodeId(node_id),
            vrf_pubkey: VrfPublicKey { bytes: [id_byte; 32] },
            presence_score: score,
            has_valid_pol: true,
            meets_minimum_stake: true,
        }
    }

    #[test]
    fn test_select_committee_basic() {
        let nodes: Vec<EligibleNode> = (0..30)
            .map(|i| make_eligible_node(i, 50 + (i % 60).min(50)))
            .collect();

        let seed = [0xAB; 32];
        let committee = select_committee(&nodes, &seed, 0, 0).unwrap();

        assert_eq!(committee.size(), COMMITTEE_SIZE);
        assert_eq!(committee.epoch.epoch_number, 0);
        assert_eq!(committee.epoch.start_slot, 0);
        assert_eq!(committee.epoch.end_slot, SLOTS_PER_EPOCH);
    }

    #[test]
    fn test_select_committee_fewer_than_21() {
        let nodes: Vec<EligibleNode> = (0..5)
            .map(|i| make_eligible_node(i, 60))
            .collect();

        let seed = [0xCD; 32];
        let committee = select_committee(&nodes, &seed, 0, 0).unwrap();

        // Should include all 5 nodes since there aren't enough for a full committee
        assert_eq!(committee.size(), 5);
    }

    #[test]
    fn test_select_committee_filters_ineligible() {
        let mut nodes: Vec<EligibleNode> = (0..25)
            .map(|i| make_eligible_node(i, 60))
            .collect();

        // Make some ineligible
        nodes[0].has_valid_pol = false;
        nodes[1].meets_minimum_stake = false;
        nodes[2].presence_score = 30; // Below threshold

        let seed = [0xEF; 32];
        let committee = select_committee(&nodes, &seed, 0, 0).unwrap();

        // Should have 21 from the 22 eligible nodes
        assert_eq!(committee.size(), COMMITTEE_SIZE);

        // Ineligible nodes should not be in the committee
        assert!(!committee.is_committee_member(&nodes[0].node_id));
        assert!(!committee.is_committee_member(&nodes[1].node_id));
        assert!(!committee.is_committee_member(&nodes[2].node_id));
    }

    #[test]
    fn test_select_committee_no_eligible_nodes() {
        let mut nodes = vec![make_eligible_node(0, 60)];
        nodes[0].has_valid_pol = false;

        let seed = [0x00; 32];
        let result = select_committee(&nodes, &seed, 0, 0);
        assert!(result.is_err());
    }

    #[test]
    fn test_is_committee_member() {
        let nodes: Vec<EligibleNode> = (0..25)
            .map(|i| make_eligible_node(i, 60))
            .collect();

        let seed = [0xAB; 32];
        let committee = select_committee(&nodes, &seed, 0, 0).unwrap();

        // All members should be found
        for member in &committee.members {
            assert!(committee.is_committee_member(&member.node_id));
        }

        // A non-member should not be found (one of the 4 excluded nodes)
        let non_member_count = nodes.iter()
            .filter(|n| !committee.is_committee_member(&n.node_id))
            .count();
        assert_eq!(non_member_count, 4); // 25 - 21 = 4
    }

    #[test]
    fn test_block_producer_for_slot() {
        let nodes: Vec<EligibleNode> = (0..25)
            .map(|i| make_eligible_node(i, 60))
            .collect();

        let seed = [0xAB; 32];
        let committee = select_committee(&nodes, &seed, 0, 0).unwrap();

        // Slot 0 should map to member 0
        let producer_0 = committee.block_producer_for_slot(0).unwrap();
        assert_eq!(producer_0.node_id, committee.members[0].node_id);

        // Slot 1 should map to member 1
        let producer_1 = committee.block_producer_for_slot(1).unwrap();
        assert_eq!(producer_1.node_id, committee.members[1].node_id);

        // Slot 21 should wrap back to member 0
        let producer_21 = committee.block_producer_for_slot(21).unwrap();
        assert_eq!(producer_21.node_id, committee.members[0].node_id);
    }

    #[test]
    fn test_deterministic_selection() {
        let nodes: Vec<EligibleNode> = (0..30)
            .map(|i| make_eligible_node(i, 50 + (i % 50)))
            .collect();

        let seed = [0xAB; 32];

        let committee1 = select_committee(&nodes, &seed, 0, 0).unwrap();
        let committee2 = select_committee(&nodes, &seed, 0, 0).unwrap();

        // Same inputs should produce the same committee
        assert_eq!(committee1.members.len(), committee2.members.len());
        for (m1, m2) in committee1.members.iter().zip(committee2.members.iter()) {
            assert_eq!(m1.node_id, m2.node_id);
        }
    }

    #[test]
    fn test_different_seeds_different_committees() {
        let nodes: Vec<EligibleNode> = (0..30)
            .map(|i| make_eligible_node(i, 50 + (i % 50)))
            .collect();

        let committee1 = select_committee(&nodes, &[0xAA; 32], 0, 0).unwrap();
        let committee2 = select_committee(&nodes, &[0xBB; 32], 0, 0).unwrap();

        // Different seeds should (almost certainly) produce different orderings
        let same_order = committee1.members.iter()
            .zip(committee2.members.iter())
            .all(|(m1, m2)| m1.node_id == m2.node_id);
        assert!(!same_order, "Different seeds should produce different committees");
    }

    #[test]
    fn test_should_rotate() {
        let nodes: Vec<EligibleNode> = (0..25)
            .map(|i| make_eligible_node(i, 60))
            .collect();

        let seed = [0xAB; 32];
        let committee = select_committee(&nodes, &seed, 0, 0).unwrap();

        assert!(!should_rotate(&committee, 0));
        assert!(!should_rotate(&committee, SLOTS_PER_EPOCH - 1));
        assert!(should_rotate(&committee, SLOTS_PER_EPOCH));
        assert!(should_rotate(&committee, SLOTS_PER_EPOCH + 1));
    }

    #[test]
    fn test_rotate_committee() {
        let nodes: Vec<EligibleNode> = (0..25)
            .map(|i| make_eligible_node(i, 60))
            .collect();

        let seed = [0xAB; 32];
        let committee = select_committee(&nodes, &seed, 0, 0).unwrap();

        let last_block_hash = [0xDD; 32];
        let new_committee = rotate_committee(&nodes, &committee, &last_block_hash).unwrap();

        assert_eq!(new_committee.epoch.epoch_number, 1);
        assert_eq!(new_committee.epoch.start_slot, SLOTS_PER_EPOCH);
        assert_eq!(new_committee.epoch.end_slot, SLOTS_PER_EPOCH * 2);
    }

    #[test]
    fn test_contains_slot() {
        let nodes: Vec<EligibleNode> = (0..25)
            .map(|i| make_eligible_node(i, 60))
            .collect();

        let committee = select_committee(&nodes, &[0xAB; 32], 0, 100).unwrap();

        assert!(!committee.contains_slot(99));
        assert!(committee.contains_slot(100));
        assert!(committee.contains_slot(100 + SLOTS_PER_EPOCH - 1));
        assert!(!committee.contains_slot(100 + SLOTS_PER_EPOCH));
    }

    #[test]
    fn test_has_finality() {
        let nodes: Vec<EligibleNode> = (0..25)
            .map(|i| make_eligible_node(i, 60))
            .collect();

        let committee = select_committee(&nodes, &[0xAB; 32], 0, 0).unwrap();

        assert!(!committee.has_finality(0));
        assert!(!committee.has_finality(13));
        assert!(committee.has_finality(14));
        assert!(committee.has_finality(21));
    }

    #[test]
    fn test_higher_score_more_likely_selected() {
        // Create nodes where half have score 40 and half have score 100
        let mut nodes: Vec<EligibleNode> = Vec::new();
        for i in 0..50 {
            let score = if i < 25 { 40 } else { 100 };
            nodes.push(make_eligible_node(i, score));
        }

        let seed = [0xAB; 32];
        let committee = select_committee(&nodes, &seed, 0, 0).unwrap();

        // Count how many high-score nodes made the committee
        let high_score_count = committee.members.iter()
            .filter(|m| m.presence_score == 100)
            .count();

        // With score weighting, high-score nodes should be over-represented
        // (they get lower selection values). Not a hard guarantee due to
        // randomness, but statistically very likely.
        assert!(high_score_count > 10,
            "Expected >10 high-score nodes in committee, got {}",
            high_score_count);
    }

    #[test]
    fn test_verify_committee_membership_deterministic() {
        let nodes: Vec<EligibleNode> = (0..25)
            .map(|i| make_eligible_node(i, 60))
            .collect();

        let seed = [0xAB; 32];
        let committee = select_committee(&nodes, &seed, 0, 0).unwrap();

        // All members should verify successfully
        for member in &committee.members {
            assert!(verify_committee_membership(member, &seed, 0, None).is_ok());
        }
    }
}
