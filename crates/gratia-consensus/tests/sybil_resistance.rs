//! Sybil Resistance Simulation
//!
//! Tests the graduated committee system against Sybil attacks at various
//! network sizes, verifying the security threshold analysis from
//! sybil-economic-model.md.

use gratia_consensus::committee::{
    select_committee_with_network_size, tier_for_network_size, EligibleNode, SCALING_TIERS,
};
use gratia_consensus::vrf::VrfPublicKey;
use gratia_core::types::NodeId;

// ============================================================================
// Helpers
// ============================================================================

/// Create a node with a unique ID derived from a u32 index.
/// WHY: Sybil tests need large node counts; u32 gives 4 billion unique IDs.
fn make_node(index: u32, presence_score: u8, pol_days: u64) -> EligibleNode {
    let mut node_id = [0u8; 32];
    node_id[0..4].copy_from_slice(&index.to_le_bytes());
    EligibleNode {
        node_id: NodeId(node_id),
        vrf_pubkey: VrfPublicKey {
            bytes: {
                let mut b = [0u8; 32];
                b[0..4].copy_from_slice(&index.to_le_bytes());
                b
            },
        },
        presence_score,
        has_valid_pol: true,
        meets_minimum_stake: true,
        pol_days,
        signing_pubkey: vec![],
    }
}

/// Create N established nodes (90 days, given score).
fn make_established_nodes(count: u32, start: u32, score: u8) -> Vec<EligibleNode> {
    (0..count).map(|i| make_node(start + i, score, 90)).collect()
}

// ============================================================================
// Tests
// ============================================================================

/// Verify that committee size increases at each tier boundary as the network
/// grows from 10 nodes to 200K nodes.
/// WHY: The graduated scaling curve is the core defense against small-network
/// attacks. This test verifies every tier boundary triggers correctly.
#[test]
fn test_committee_tier_progression() {
    let test_cases: Vec<(u64, usize)> = vec![
        (10, 3),
        (50, 3),
        (99, 3),
        (100, 5),
        (250, 5),
        (499, 5),
        (500, 7),
        (1_000, 7),
        (2_499, 7),
        (2_500, 11),
        (5_000, 11),
        (9_999, 11),
        (10_000, 15),
        (25_000, 15),
        (49_999, 15),
        (50_000, 19),
        (75_000, 19),
        (99_999, 19),
        (100_000, 21),
        (200_000, 21),
    ];

    for (network_size, expected_committee_size) in test_cases {
        let tier = tier_for_network_size(network_size);
        assert_eq!(
            tier.committee_size, expected_committee_size,
            "At network_size={}, expected committee_size={}, got {}",
            network_size, expected_committee_size, tier.committee_size
        );
    }
}

/// At every tier, the finality threshold must be at least 66% of committee size.
/// WHY: BFT consensus requires >2/3 agreement for safety. If any tier drops
/// below this, the network is vulnerable to a 1/3 attack at that scale.
#[test]
fn test_committee_finality_always_above_two_thirds() {
    for tier in &SCALING_TIERS {
        let ratio = tier.finality_threshold as f64 / tier.committee_size as f64;
        assert!(
            ratio >= 0.66,
            "Tier (network_size >= {}) has finality ratio {:.4} ({}/{}), which is below 2/3",
            tier.min_network_size,
            ratio,
            tier.finality_threshold,
            tier.committee_size
        );
    }
}

/// The same set of eligible nodes with different seeds must produce different
/// committees. If an attacker could predict the committee from static data,
/// they could target those specific nodes.
/// WHY: Unpredictability is essential — the committee must not be knowable
/// before the epoch seed (last block hash) is finalized.
#[test]
fn test_attacker_cannot_predict_committee() {
    let nodes = make_established_nodes(50, 0, 70);
    let network_size = 50;

    let mut committees = Vec::new();
    for seed_byte in 0..20u8 {
        let seed = [seed_byte; 32];
        let committee =
            select_committee_with_network_size(&nodes, &seed, 0, 0, network_size, None).unwrap();
        let ids: Vec<NodeId> = committee.members.iter().map(|m| m.node_id).collect();
        committees.push(ids);
    }

    // Count how many unique committee compositions we got
    let mut unique = committees.clone();
    unique.sort_by(|a, b| format!("{:?}", a).cmp(&format!("{:?}", b)));
    unique.dedup_by(|a, b| format!("{:?}", a) == format!("{:?}", b));

    // With 20 different seeds, we should get at least 10 distinct committees
    // (50 choose 3 = 19,600 possible committees, so collisions are near-impossible)
    assert!(
        unique.len() >= 10,
        "Only got {} unique committees out of 20 seeds — selection may be too predictable",
        unique.len()
    );
}

/// High-presence-score nodes should be selected more frequently than
/// low-score nodes across many committee selections.
/// WHY: Presence score weighting ensures nodes with stronger liveness proofs
/// have higher selection probability, making it more expensive to attack.
#[test]
fn test_presence_score_weighting_works() {
    // 25 nodes with score 45 (just above threshold) and 25 with score 95
    let mut nodes = make_established_nodes(25, 0, 45);
    nodes.extend(make_established_nodes(25, 25, 95));

    let network_size = 50;
    let mut high_score_selections = 0u64;
    let mut low_score_selections = 0u64;

    for trial in 0..500u64 {
        let mut seed = [0u8; 32];
        seed[0..8].copy_from_slice(&trial.to_le_bytes());

        let committee =
            select_committee_with_network_size(&nodes, &seed, trial, 0, network_size, None)
                .unwrap();

        for member in &committee.members {
            if member.presence_score >= 90 {
                high_score_selections += 1;
            } else {
                low_score_selections += 1;
            }
        }
    }

    // High-score nodes should be selected more often than low-score nodes
    assert!(
        high_score_selections > low_score_selections,
        "High-score nodes selected {} times vs low-score {} times — weighting not working",
        high_score_selections, low_score_selections
    );
}

/// Identical inputs must always produce the same committee.
/// WHY: Determinism is required so that every node independently computes
/// the same committee. Non-determinism would cause consensus forks.
#[test]
fn test_committee_selection_is_deterministic() {
    let nodes = make_established_nodes(100, 0, 65);
    let seed = [0xDE; 32];
    let network_size = 100;

    let committee1 =
        select_committee_with_network_size(&nodes, &seed, 5, 4500, network_size, None).unwrap();
    let committee2 =
        select_committee_with_network_size(&nodes, &seed, 5, 4500, network_size, None).unwrap();

    assert_eq!(
        committee1.members.len(),
        committee2.members.len(),
        "Committees have different sizes on identical inputs"
    );

    for (m1, m2) in committee1.members.iter().zip(committee2.members.iter()) {
        assert_eq!(
            m1.node_id, m2.node_id,
            "Member mismatch on identical inputs: {:?} vs {:?}",
            m1.node_id, m2.node_id
        );
        assert_eq!(
            m1.selection_value, m2.selection_value,
            "Selection value mismatch on identical inputs"
        );
    }
}

/// When the eligible pool is smaller than the desired committee size, the
/// system should gracefully degrade and form a committee from all available
/// nodes rather than failing.
/// WHY: During bootstrap or after a mass disconnect, the network must keep
/// producing blocks even with fewer validators than the tier wants.
#[test]
fn test_minimum_selection_pool_respected() {
    // Only 4 nodes, but network_size says 100K (wants 21-member committee)
    let nodes = make_established_nodes(4, 0, 70);
    let seed = [0xFF; 32];
    let network_size = 100_000;

    let committee =
        select_committee_with_network_size(&nodes, &seed, 0, 0, network_size, None).unwrap();

    // Should gracefully form a committee with all 4 available nodes
    assert_eq!(
        committee.size(),
        4,
        "Expected committee of 4 (all available nodes), got {}",
        committee.size()
    );

    // The committee_size field records the *desired* size from the tier
    assert_eq!(
        committee.committee_size, 21,
        "Tier should still indicate 21 as desired committee size"
    );

    // With only 2 nodes, committee should still form
    let tiny_nodes = make_established_nodes(2, 0, 70);
    let tiny_committee =
        select_committee_with_network_size(&tiny_nodes, &seed, 0, 0, network_size, None).unwrap();

    assert_eq!(
        tiny_committee.size(),
        2,
        "Expected committee of 2 from 2 available nodes"
    );

    // With 1 node, committee should still form
    let solo_nodes = make_established_nodes(1, 0, 70);
    let solo_committee =
        select_committee_with_network_size(&solo_nodes, &seed, 0, 0, 1, None).unwrap();

    assert_eq!(
        solo_committee.size(),
        1,
        "Expected committee of 1 from single node"
    );
}
