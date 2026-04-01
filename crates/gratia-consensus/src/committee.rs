//! Validator committee management with graduated scaling.
//!
//! The Gratia consensus uses a validator committee that scales with network size,
//! from 3 validators at bootstrap to 21 at full scale. The committee produces blocks
//! via VRF-based selection and rotates at epoch boundaries.
//! Selection is weighted by Composite Presence Score (40-100), which affects
//! selection probability but NOT rewards.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use gratia_core::error::GratiaError;
use gratia_core::types::NodeId;

use crate::vrf::{self, VrfProof, VrfPublicKey};

// ============================================================================
// Constants
// ============================================================================

/// Maximum committee size at full network scale (100K+ nodes).
/// WHY: 21 balances decentralization (enough nodes for geographic diversity)
/// against finality speed (14/21 signatures can be collected quickly on mobile networks).
pub const MAX_COMMITTEE_SIZE: usize = 21;

/// Legacy alias — code referencing COMMITTEE_SIZE gets the max.
pub const COMMITTEE_SIZE: usize = MAX_COMMITTEE_SIZE;

/// Legacy alias — finality threshold for a full 21-validator committee.
pub const FINALITY_THRESHOLD: usize = 14;

/// Number of slots per epoch before committee rotation.
/// WHY: ~900 slots at 4 seconds each = ~1 hour epochs. Frequent enough to
/// rotate out misbehaving validators, long enough to amortize selection cost.
pub const SLOTS_PER_EPOCH: u64 = 900;

/// Domain separator for committee selection VRF input.
const COMMITTEE_SELECTION_DOMAIN: &[u8] = b"gratia-committee-select-v1";

/// Number of days a network size must persist below a tier before the committee
/// downsizes. Prevents attacker-induced shrinking.
/// WHY: 7 days prevents an attacker from knocking nodes offline to force a
/// committee shrink, which would make capture easier.
#[allow(dead_code)] // Phase 2: used when epoch-boundary downsizing is implemented
const DOWNSIZE_PERSISTENCE_DAYS: u64 = 7;

// ============================================================================
// Graduated Scaling
// ============================================================================

/// A tier in the graduated committee scaling curve.
/// WHY: Odd committee sizes prevent tie conditions in voting. Finality
/// threshold stays near 67% at every level for BFT consistency.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CommitteeTier {
    /// Minimum total network nodes to use this tier.
    pub min_network_size: u64,
    /// Number of validators in the committee at this tier.
    pub committee_size: usize,
    /// Number of signatures required for finality.
    pub finality_threshold: usize,
    /// Minimum eligible nodes required in the VRF selection pool.
    pub min_selection_pool: u64,
    /// Cooldown: how many consecutive rounds a node must sit out after serving.
    /// WHY: At low node counts, cooldowns prevent a small set of attacker nodes
    /// from appearing in back-to-back committees.
    pub cooldown_rounds: u64,
}

/// The 7-tier graduated scaling curve.
/// WHY: The curve is deliberately conservative — the committee stays small
/// until the network has significant depth in its selection pool.
pub const SCALING_TIERS: [CommitteeTier; 7] = [
    CommitteeTier {
        min_network_size: 0,
        committee_size: 3,
        finality_threshold: 2,
        min_selection_pool: 10,
        // WHY: At <100 nodes, a cooldown of 5 rounds ensures at least 15 distinct
        // nodes are needed to fill committees across 5 consecutive rounds.
        cooldown_rounds: 5,
    },
    CommitteeTier {
        min_network_size: 100,
        committee_size: 5,
        finality_threshold: 4,
        min_selection_pool: 50,
        cooldown_rounds: 3,
    },
    CommitteeTier {
        min_network_size: 500,
        committee_size: 7,
        finality_threshold: 5,
        min_selection_pool: 100,
        cooldown_rounds: 2,
    },
    CommitteeTier {
        min_network_size: 2_500,
        committee_size: 11,
        finality_threshold: 8,
        min_selection_pool: 500,
        cooldown_rounds: 1,
    },
    CommitteeTier {
        min_network_size: 10_000,
        committee_size: 15,
        finality_threshold: 10,
        min_selection_pool: 2_000,
        cooldown_rounds: 1,
    },
    CommitteeTier {
        min_network_size: 50_000,
        committee_size: 19,
        finality_threshold: 13,
        min_selection_pool: 10_000,
        cooldown_rounds: 1,
    },
    CommitteeTier {
        min_network_size: 100_000,
        committee_size: 21,
        finality_threshold: 14,
        min_selection_pool: 20_000,
        cooldown_rounds: 1,
    },
];

/// Determine the appropriate committee tier for a given network size.
pub fn tier_for_network_size(network_size: u64) -> &'static CommitteeTier {
    // Walk tiers in reverse to find the highest tier the network qualifies for.
    for tier in SCALING_TIERS.iter().rev() {
        if network_size >= tier.min_network_size {
            return tier;
        }
    }
    // Fallback: smallest tier (should be unreachable since tier 0 starts at 0).
    &SCALING_TIERS[0]
}

/// Determine committee size for a given network size.
pub fn committee_size_for_network(network_size: u64) -> usize {
    tier_for_network_size(network_size).committee_size
}

/// Determine finality threshold for a given network size.
pub fn finality_threshold_for_network(network_size: u64) -> usize {
    tier_for_network_size(network_size).finality_threshold
}

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
    /// Number of consecutive days of valid PoL history.
    /// WHY: Progressive trust model — only Established+ nodes (30+ days)
    /// are eligible for validator committees.
    pub pol_days: u64,
}

impl EligibleNode {
    /// Check if this node is fully eligible for committee membership.
    pub fn is_eligible(&self) -> bool {
        self.has_valid_pol
            && self.meets_minimum_stake
            && self.presence_score >= 40
    }

    /// Check if this node meets the progressive trust requirement for
    /// committee membership (Established tier: 30+ days PoL).
    /// WHY: Prevents an attacker from flooding the network with fresh nodes
    /// to influence consensus. Committee eligibility must be earned.
    pub fn is_committee_eligible(&self) -> bool {
        self.is_eligible() && self.pol_days >= 30
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

/// The active validator committee for the current epoch.
/// Committee size scales with network size per the graduated scaling curve.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ValidatorCommittee {
    /// The current epoch info.
    pub epoch: CommitteeEpoch,
    /// The committee members, sorted by selection value (lowest first).
    pub members: Vec<CommitteeMember>,
    /// The committee size used for this epoch (from graduated scaling).
    pub committee_size: usize,
    /// The finality threshold for this epoch.
    pub finality_threshold: usize,
    /// Network size snapshot used to determine the committee tier.
    pub network_size_snapshot: u64,
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
        signature_count >= self.finality_threshold
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
// Cooldown Tracking
// ============================================================================

/// Tracks recent committee membership for cooldown enforcement.
/// WHY: At low node counts, cooldowns prevent a small set of attacker nodes
/// from appearing in back-to-back committees.
#[derive(Debug, Clone, Default)]
pub struct CooldownTracker {
    /// Ring buffer of recent committee member sets (most recent first).
    /// Each entry is the set of NodeIds that served in that round.
    recent_committees: Vec<Vec<NodeId>>,
}

impl CooldownTracker {
    /// Create a new cooldown tracker.
    pub fn new() -> Self {
        Self {
            recent_committees: Vec::new(),
        }
    }

    /// Record a committee that was just selected.
    pub fn record_committee(&mut self, members: &[CommitteeMember]) {
        let ids: Vec<NodeId> = members.iter().map(|m| m.node_id).collect();
        self.recent_committees.insert(0, ids);
        // WHY: Keep at least max_cooldown + 2 entries to ensure cooldown checks
        // have sufficient history. The previous hard-coded limit of 10 was
        // fragile — if max cooldown was changed to >8, history would be pruned
        // before the cooldown window expired, allowing nodes to re-select early.
        // 20 rounds is generous for the maximum cooldown of 5, leaving headroom
        // for future governance increases.
        if self.recent_committees.len() > 20 {
            self.recent_committees.truncate(20);
        }
    }

    /// Check if a node is in cooldown and cannot serve on the next committee.
    pub fn is_in_cooldown(&self, node_id: &NodeId, cooldown_rounds: u64) -> bool {
        // A cooldown of 1 means the node must sit out the immediately next round
        // (standard behavior — just skip if served in the most recent committee).
        // A cooldown of 0 means no cooldown.
        if cooldown_rounds == 0 {
            return false;
        }
        let check_rounds = cooldown_rounds as usize;
        for recent in self.recent_committees.iter().take(check_rounds) {
            if recent.contains(node_id) {
                return true;
            }
        }
        false
    }
}

// ============================================================================
// Committee Selection
// ============================================================================

/// Select a committee of validators from all eligible nodes.
///
/// Committee size is determined by the graduated scaling curve based on
/// `network_size`. Each eligible node generates a VRF proof using the epoch
/// seed. The nodes with the lowest weighted selection values are chosen.
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
    // Use the count of all eligible nodes as network size for tier selection.
    let network_size = eligible_nodes.iter().filter(|n| n.is_eligible()).count() as u64;
    select_committee_with_network_size(
        eligible_nodes,
        epoch_seed,
        epoch_number,
        current_slot,
        network_size,
        None,
    )
}

/// Select a committee with an explicit network size and optional cooldown tracker.
///
/// This is the full-featured selection function used when the caller provides
/// the network size (e.g., from beacon chain state) and cooldown tracking.
pub fn select_committee_with_network_size(
    eligible_nodes: &[EligibleNode],
    epoch_seed: &[u8; 32],
    epoch_number: u64,
    current_slot: u64,
    network_size: u64,
    cooldown_tracker: Option<&CooldownTracker>,
) -> Result<ValidatorCommittee, GratiaError> {
    let tier = tier_for_network_size(network_size);

    // Filter to committee-eligible nodes (30+ days PoL for progressive trust).
    // WHY: If not enough committee-eligible nodes exist (early network), fall back
    // to basic eligibility. This prevents the network from stalling during bootstrap
    // when no one has 30 days of history yet.
    let mut eligible: Vec<&EligibleNode> = eligible_nodes
        .iter()
        .filter(|n| n.is_committee_eligible())
        .collect();

    if eligible.len() < tier.committee_size {
        // WHY: Fallback — use any eligible node if we can't fill the committee
        // with 30+ day nodes. This is expected during the first month of the network.
        eligible = eligible_nodes
            .iter()
            .filter(|n| n.is_eligible())
            .collect();
    }

    if eligible.is_empty() {
        return Err(GratiaError::BlockValidationFailed {
            reason: "No eligible nodes for committee selection".into(),
        });
    }

    // Apply cooldown filtering if tracker is provided.
    if let Some(tracker) = cooldown_tracker {
        let before_cooldown = eligible.len();
        eligible.retain(|n| !tracker.is_in_cooldown(&n.node_id, tier.cooldown_rounds));

        // WHY: If cooldown filtering removed too many candidates, fall back to
        // unfiltered. Better to reuse some validators than to have a tiny committee.
        if eligible.len() < tier.committee_size {
            tracing::warn!(
                before = before_cooldown,
                after = eligible.len(),
                required = tier.committee_size,
                "Cooldown filtering removed too many candidates, falling back to unfiltered list"
            );
            eligible = eligible_nodes
                .iter()
                .filter(|n| n.is_eligible())
                .collect();
        }
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

    // Sort by selection value (lowest first = highest priority).
    // WHY: Tie-break by node_id for determinism. Without this, two nodes
    // with identical selection values (common with synthetic padding nodes
    // or similar presence scores) can sort differently depending on input
    // order — causing phones to disagree on who produces which slot.
    candidates.sort_by(|a, b| {
        match a.selection_value.partial_cmp(&b.selection_value) {
            Some(std::cmp::Ordering::Equal) | None => {
                a.node_id.0.cmp(&b.node_id.0)
            }
            Some(ord) => ord,
        }
    });

    // Take the top committee_size (or fewer if not enough eligible nodes)
    let actual_size = tier.committee_size.min(candidates.len());
    candidates.truncate(actual_size);

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
        committee_size: tier.committee_size,
        finality_threshold: tier.finality_threshold,
        network_size_snapshot: network_size,
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

/// Rotate the committee with explicit network size and cooldown tracking.
pub fn rotate_committee_with_network_size(
    eligible_nodes: &[EligibleNode],
    previous_committee: &ValidatorCommittee,
    last_block_hash: &[u8; 32],
    network_size: u64,
    cooldown_tracker: Option<&CooldownTracker>,
) -> Result<ValidatorCommittee, GratiaError> {
    let new_epoch_number = previous_committee.epoch.epoch_number + 1;
    let new_start_slot = previous_committee.epoch.end_slot;

    let new_seed = gratia_core::crypto::sha256_multi(&[
        b"gratia-epoch-seed-v1:",
        last_block_hash,
        &new_epoch_number.to_be_bytes(),
    ]);

    select_committee_with_network_size(
        eligible_nodes,
        &new_seed,
        new_epoch_number,
        new_start_slot,
        network_size,
        cooldown_tracker,
    )
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
            pol_days: 90, // Default to Trusted tier for backward compat
        }
    }

    fn make_new_node(id_byte: u8, score: u8, days: u64) -> EligibleNode {
        let mut node = make_eligible_node(id_byte, score);
        node.pol_days = days;
        node
    }

    // ========================================================================
    // Graduated Scaling Tests
    // ========================================================================

    #[test]
    fn test_tier_for_network_size() {
        assert_eq!(tier_for_network_size(0).committee_size, 3);
        assert_eq!(tier_for_network_size(50).committee_size, 3);
        assert_eq!(tier_for_network_size(99).committee_size, 3);
        assert_eq!(tier_for_network_size(100).committee_size, 5);
        assert_eq!(tier_for_network_size(499).committee_size, 5);
        assert_eq!(tier_for_network_size(500).committee_size, 7);
        assert_eq!(tier_for_network_size(2_499).committee_size, 7);
        assert_eq!(tier_for_network_size(2_500).committee_size, 11);
        assert_eq!(tier_for_network_size(9_999).committee_size, 11);
        assert_eq!(tier_for_network_size(10_000).committee_size, 15);
        assert_eq!(tier_for_network_size(49_999).committee_size, 15);
        assert_eq!(tier_for_network_size(50_000).committee_size, 19);
        assert_eq!(tier_for_network_size(99_999).committee_size, 19);
        assert_eq!(tier_for_network_size(100_000).committee_size, 21);
        assert_eq!(tier_for_network_size(1_000_000).committee_size, 21);
    }

    #[test]
    fn test_finality_thresholds_are_above_two_thirds() {
        for tier in &SCALING_TIERS {
            let ratio = tier.finality_threshold as f64 / tier.committee_size as f64;
            assert!(
                ratio >= 0.66,
                "Tier with committee_size={} has finality ratio {:.2}, below 2/3",
                tier.committee_size, ratio
            );
        }
    }

    #[test]
    fn test_all_committee_sizes_are_odd() {
        // WHY: Odd sizes prevent tie conditions in voting.
        for tier in &SCALING_TIERS {
            assert!(
                tier.committee_size % 2 == 1,
                "Committee size {} is even",
                tier.committee_size
            );
        }
    }

    #[test]
    fn test_tiers_are_monotonically_increasing() {
        for i in 1..SCALING_TIERS.len() {
            assert!(
                SCALING_TIERS[i].min_network_size > SCALING_TIERS[i - 1].min_network_size,
                "Tiers not monotonically increasing at index {}",
                i
            );
            assert!(
                SCALING_TIERS[i].committee_size > SCALING_TIERS[i - 1].committee_size,
                "Committee sizes not monotonically increasing at index {}",
                i
            );
        }
    }

    // ========================================================================
    // Committee Selection Tests (adapted from original)
    // ========================================================================

    #[test]
    fn test_select_committee_basic() {
        let nodes: Vec<EligibleNode> = (0..30)
            .map(|i| make_eligible_node(i, 50 + (i % 60).min(50)))
            .collect();

        let seed = [0xAB; 32];
        let committee = select_committee(&nodes, &seed, 0, 0).unwrap();

        // 30 eligible nodes → tier for 30 = committee of 3
        assert_eq!(committee.committee_size, 3);
        assert_eq!(committee.size(), 3);
        assert_eq!(committee.epoch.epoch_number, 0);
        assert_eq!(committee.epoch.start_slot, 0);
        assert_eq!(committee.epoch.end_slot, SLOTS_PER_EPOCH);
    }

    #[test]
    fn test_select_committee_scales_with_network() {
        // 150 nodes → tier 2 (committee of 5)
        let nodes: Vec<EligibleNode> = (0..150)
            .map(|i| make_eligible_node(i as u8, 60))
            .collect();

        let seed = [0xAB; 32];
        let committee = select_committee_with_network_size(
            &nodes, &seed, 0, 0, 150, None,
        ).unwrap();

        assert_eq!(committee.committee_size, 5);
        assert_eq!(committee.size(), 5);
        assert_eq!(committee.finality_threshold, 4);
    }

    #[test]
    fn test_select_committee_full_scale() {
        // Simulate 100K+ network
        let nodes: Vec<EligibleNode> = (0..50)
            .map(|i| make_eligible_node(i, 60))
            .collect();

        let seed = [0xCD; 32];
        let committee = select_committee_with_network_size(
            &nodes, &seed, 0, 0, 100_000, None,
        ).unwrap();

        assert_eq!(committee.committee_size, 21);
        assert_eq!(committee.finality_threshold, 14);
        assert_eq!(committee.size(), 21);
        assert_eq!(committee.network_size_snapshot, 100_000);
    }

    #[test]
    fn test_select_committee_fewer_than_tier_size() {
        let nodes: Vec<EligibleNode> = (0..5)
            .map(|i| make_eligible_node(i, 60))
            .collect();

        let seed = [0xCD; 32];
        // 5 eligible nodes, network says 100K → wants 21 committee, gets 5
        let committee = select_committee_with_network_size(
            &nodes, &seed, 0, 0, 100_000, None,
        ).unwrap();

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
    fn test_block_producer_for_slot() {
        let nodes: Vec<EligibleNode> = (0..25)
            .map(|i| make_eligible_node(i, 60))
            .collect();

        let seed = [0xAB; 32];
        let committee = select_committee(&nodes, &seed, 0, 0).unwrap();
        let size = committee.size();

        // Slot 0 should map to member 0
        let producer_0 = committee.block_producer_for_slot(0).unwrap();
        assert_eq!(producer_0.node_id, committee.members[0].node_id);

        // Slot 1 should map to member 1
        let producer_1 = committee.block_producer_for_slot(1).unwrap();
        assert_eq!(producer_1.node_id, committee.members[1].node_id);

        // Slot wrapping
        let producer_wrap = committee.block_producer_for_slot(size as u64).unwrap();
        assert_eq!(producer_wrap.node_id, committee.members[0].node_id);
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
    fn test_has_finality_scaled() {
        // At 30 nodes, tier is committee=3, finality=2
        let nodes: Vec<EligibleNode> = (0..30)
            .map(|i| make_eligible_node(i, 60))
            .collect();

        let committee = select_committee(&nodes, &[0xAB; 32], 0, 0).unwrap();
        assert_eq!(committee.finality_threshold, 2);
        assert!(!committee.has_finality(1));
        assert!(committee.has_finality(2));
        assert!(committee.has_finality(3));
    }

    #[test]
    fn test_has_finality_full_scale() {
        let nodes: Vec<EligibleNode> = (0..50)
            .map(|i| make_eligible_node(i, 60))
            .collect();

        let committee = select_committee_with_network_size(
            &nodes, &[0xAB; 32], 0, 0, 100_000, None,
        ).unwrap();

        assert_eq!(committee.finality_threshold, 14);
        assert!(!committee.has_finality(13));
        assert!(committee.has_finality(14));
        assert!(committee.has_finality(21));
    }

    // ========================================================================
    // Progressive Trust Tests
    // ========================================================================

    #[test]
    fn test_new_nodes_excluded_from_committee_when_enough_established() {
        // 20 established nodes (90 days) + 20 new nodes (5 days)
        let mut nodes: Vec<EligibleNode> = (0..20)
            .map(|i| make_eligible_node(i, 60))
            .collect();
        for i in 20..40u8 {
            nodes.push(make_new_node(i, 60, 5));
        }

        let seed = [0xAB; 32];
        let committee = select_committee_with_network_size(
            &nodes, &seed, 0, 0, 40, None,
        ).unwrap();

        // All committee members should be from the established group (90 days)
        for member in &committee.members {
            let node = nodes.iter().find(|n| n.node_id == member.node_id).unwrap();
            assert!(
                node.pol_days >= 30,
                "Node with {} PoL days shouldn't be on committee",
                node.pol_days
            );
        }
    }

    #[test]
    fn test_new_nodes_used_as_fallback_when_not_enough_established() {
        // Only 2 established nodes but committee needs 3
        let mut nodes: Vec<EligibleNode> = (0..2)
            .map(|i| make_eligible_node(i, 60))
            .collect();
        for i in 2..10u8 {
            nodes.push(make_new_node(i, 60, 5));
        }

        let seed = [0xAB; 32];
        let committee = select_committee(&nodes, &seed, 0, 0).unwrap();

        // Committee should still form (fallback to all eligible)
        assert_eq!(committee.size(), 3);
    }

    // ========================================================================
    // Cooldown Tests
    // ========================================================================

    #[test]
    fn test_cooldown_tracker_basic() {
        let mut tracker = CooldownTracker::new();

        let node_a = NodeId([1u8; 32]);
        let node_b = NodeId([2u8; 32]);

        // No history — nothing in cooldown
        assert!(!tracker.is_in_cooldown(&node_a, 1));

        // Record node_a as a committee member
        tracker.record_committee(&[CommitteeMember {
            node_id: node_a,
            vrf_pubkey: VrfPublicKey { bytes: [1; 32] },
            presence_score: 60,
            selection_proof: VrfProof { output: [0; 32], proof_bytes: vec![] },
            selection_value: 0.5,
        }]);

        // node_a is in cooldown for 1 round, node_b is not
        assert!(tracker.is_in_cooldown(&node_a, 1));
        assert!(!tracker.is_in_cooldown(&node_b, 1));

        // Record another round without node_a
        tracker.record_committee(&[CommitteeMember {
            node_id: node_b,
            vrf_pubkey: VrfPublicKey { bytes: [2; 32] },
            presence_score: 60,
            selection_proof: VrfProof { output: [0; 32], proof_bytes: vec![] },
            selection_value: 0.5,
        }]);

        // node_a is no longer in cooldown for 1 round, but would be for 2
        assert!(!tracker.is_in_cooldown(&node_a, 1));
        assert!(tracker.is_in_cooldown(&node_a, 2));
    }

    #[test]
    fn test_cooldown_zero_means_no_cooldown() {
        let mut tracker = CooldownTracker::new();
        let node_a = NodeId([1u8; 32]);

        tracker.record_committee(&[CommitteeMember {
            node_id: node_a,
            vrf_pubkey: VrfPublicKey { bytes: [1; 32] },
            presence_score: 60,
            selection_proof: VrfProof { output: [0; 32], proof_bytes: vec![] },
            selection_value: 0.5,
        }]);

        assert!(!tracker.is_in_cooldown(&node_a, 0));
    }

    #[test]
    fn test_cooldown_applied_in_selection() {
        // Create 10 nodes, select committee, then select again with cooldown
        let nodes: Vec<EligibleNode> = (0..10)
            .map(|i| make_eligible_node(i, 60))
            .collect();

        let seed = [0xAB; 32];
        let committee1 = select_committee_with_network_size(
            &nodes, &seed, 0, 0, 10, None,
        ).unwrap();

        let mut tracker = CooldownTracker::new();
        tracker.record_committee(&committee1.members);

        // At <100 nodes, cooldown is 5 rounds — all 3 members should be excluded
        let committee2 = select_committee_with_network_size(
            &nodes, &[0xBB; 32], 1, SLOTS_PER_EPOCH, 10, Some(&tracker),
        ).unwrap();

        // With 10 nodes and cooldown excluding 3, we still have 7 eligible
        // Committee should form with 3 different members
        let c1_ids: Vec<NodeId> = committee1.members.iter().map(|m| m.node_id).collect();
        let c2_ids: Vec<NodeId> = committee2.members.iter().map(|m| m.node_id).collect();

        // At least some members should differ
        let overlap = c2_ids.iter().filter(|id| c1_ids.contains(id)).count();
        assert!(
            overlap < committee2.size(),
            "Expected cooldown to prevent full overlap, got {} of {} overlapping",
            overlap, committee2.size()
        );
    }

    // ========================================================================
    // Membership Verification Tests
    // ========================================================================

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

    #[test]
    fn test_higher_score_more_likely_selected() {
        // Create nodes where half have score 40 and half have score 100
        let mut nodes: Vec<EligibleNode> = Vec::new();
        for i in 0..50 {
            let score = if i < 25 { 40 } else { 100 };
            nodes.push(make_eligible_node(i, score));
        }

        let seed = [0xAB; 32];
        // Use large network size to get a bigger committee for statistical testing
        let committee = select_committee_with_network_size(
            &nodes, &seed, 0, 0, 100_000, None,
        ).unwrap();

        // Count how many high-score nodes made the committee
        let high_score_count = committee.members.iter()
            .filter(|m| m.presence_score == 100)
            .count();

        // With score weighting, high-score nodes should be over-represented
        assert!(high_score_count > 10,
            "Expected >10 high-score nodes in committee, got {}",
            high_score_count);
    }

    #[test]
    fn test_committee_stores_network_snapshot() {
        let nodes: Vec<EligibleNode> = (0..25)
            .map(|i| make_eligible_node(i, 60))
            .collect();

        let committee = select_committee_with_network_size(
            &nodes, &[0xAB; 32], 0, 0, 42_000, None,
        ).unwrap();

        assert_eq!(committee.network_size_snapshot, 42_000);
        assert_eq!(committee.committee_size, 15); // 10K-50K tier
        assert_eq!(committee.finality_threshold, 10);
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

        // Non-members should not be found
        let non_member_count = nodes.iter()
            .filter(|n| !committee.is_committee_member(&n.node_id))
            .count();
        // 25 total - 3 committee (tier for 25 nodes) = 22
        assert_eq!(non_member_count, 25 - committee.size());
    }
}
