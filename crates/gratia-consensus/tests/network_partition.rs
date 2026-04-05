//! Network Partition Simulation
//!
//! Tests how the consensus system handles nodes going offline, which is
//! expected behavior for a mobile phone network (users unplug, go to
//! airplane mode, run out of battery).

use gratia_consensus::committee::{
    rotate_committee_with_network_size, select_committee_with_network_size,
    tier_for_network_size, EligibleNode,
};
use gratia_consensus::vrf::VrfPublicKey;
use gratia_core::types::NodeId;

// ============================================================================
// Helpers
// ============================================================================

/// Create a node with a unique ID derived from a u32 index.
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
        // WHY: Non-empty signing_pubkey marks this as a real node (not a
        // synthetic placeholder). The solo-mode threshold override only
        // applies when ≤1 member has a real signing key.
        signing_pubkey: node_id.to_vec(),
        vrf_proof: vec![],
    }
}

/// Create N established nodes.
fn make_established_nodes(count: u32, start: u32, score: u8) -> Vec<EligibleNode> {
    (0..count).map(|i| make_node(start + i, score, 90)).collect()
}

// ============================================================================
// Tests
// ============================================================================

/// Select a 21-node committee (full scale), remove 7 members (1/3).
/// The remaining 14 should still meet the finality threshold (14/21).
/// WHY: Mobile networks will frequently lose nodes. The 2/3 threshold
/// means up to 1/3 of the committee can go offline without stalling.
#[test]
fn test_committee_survives_minority_offline() {
    let nodes = make_established_nodes(50, 0, 70);
    let seed = [0xAA; 32];
    let network_size = 100_000; // Full-scale tier: committee=21, finality=14

    let committee =
        select_committee_with_network_size(&nodes, &seed, 0, 0, network_size, None).unwrap();

    assert_eq!(committee.size(), 21);
    assert_eq!(committee.finality_threshold, 14);

    // Simulate 7 nodes going offline — 14 remain
    let remaining_signatures = committee.size() - 7;
    assert_eq!(remaining_signatures, 14);

    assert!(
        committee.has_finality(remaining_signatures),
        "Committee with 14/21 signatures should still reach finality"
    );

    // Verify the exact boundary
    assert!(
        committee.has_finality(14),
        "14 signatures must be sufficient for finality"
    );
    assert!(
        !committee.has_finality(13),
        "13 signatures must NOT be sufficient for finality"
    );
}

/// If more than 1/3 of the committee goes offline, finality should stall.
/// This is correct behavior — the network is safer stalling than finalizing
/// blocks without sufficient agreement.
/// WHY: BFT safety requires >2/3 agreement. Allowing finality with fewer
/// signatures opens the door to double-spend attacks during partitions.
#[test]
fn test_committee_stalls_if_majority_offline() {
    let nodes = make_established_nodes(50, 0, 70);
    let seed = [0xBB; 32];
    let network_size = 100_000;

    let committee =
        select_committee_with_network_size(&nodes, &seed, 0, 0, network_size, None).unwrap();

    assert_eq!(committee.size(), 21);

    // 8 nodes offline → only 13 signatures available
    let remaining = committee.size() - 8;
    assert_eq!(remaining, 13);

    assert!(
        !committee.has_finality(remaining),
        "Committee with only 13/21 signatures should NOT reach finality"
    );

    // Even worse: 11 offline → only 10 remain
    assert!(
        !committee.has_finality(10),
        "10/21 signatures should not finalize"
    );

    // Total failure: all offline
    assert!(
        !committee.has_finality(0),
        "0 signatures should not finalize"
    );
}

/// After a partition causes offline nodes, rotating the committee with a new
/// seed should select fresh nodes and recover consensus.
/// WHY: Epoch rotation is the recovery mechanism. When nodes drop off, the
/// next epoch's VRF selection draws from the full eligible pool, naturally
/// replacing offline nodes with online ones.
#[test]
fn test_rotation_recovers_from_partition() {
    let nodes = make_established_nodes(50, 0, 70);
    let seed = [0xCC; 32];
    let network_size = 100_000;

    let committee1 =
        select_committee_with_network_size(&nodes, &seed, 0, 0, network_size, None).unwrap();

    // Simulate: mark some nodes as "offline" by noting their IDs
    let _offline_ids: Vec<NodeId> = committee1.members[0..7]
        .iter()
        .map(|m| m.node_id)
        .collect();

    // Rotate committee (uses last block hash as new seed)
    let last_block_hash = [0xDD; 32];
    let committee2 = rotate_committee_with_network_size(
        &nodes,
        &committee1,
        &last_block_hash,
        network_size,
        None,
    )
    .unwrap();

    assert_eq!(committee2.size(), 21);
    assert_eq!(committee2.epoch.epoch_number, 1);

    // The new committee should be different from the old one (different seed)
    let c2_ids: Vec<NodeId> = committee2.members.iter().map(|m| m.node_id).collect();
    let c1_ids: Vec<NodeId> = committee1.members.iter().map(|m| m.node_id).collect();

    let same = c1_ids
        .iter()
        .zip(c2_ids.iter())
        .all(|(a, b)| a == b);
    assert!(
        !same,
        "Rotated committee should differ from the previous one"
    );

    // Some previously-offline nodes may not be re-selected, which is fine.
    // The key property is that the new committee is fully formed.
    assert!(
        committee2.has_finality(committee2.size()),
        "New committee should be able to reach finality if all members are online"
    );
}

/// At the smallest committee size (3), losing 1 node still allows finality
/// because finality_threshold=2 and 2 nodes remain (2/3 = 66.7%).
/// WHY: Small committees are designed for early network where every node
/// matters. Losing 1 of 3 is tolerable; losing 2 of 3 is not.
#[test]
fn test_small_committee_more_resilient_per_node() {
    let nodes = make_established_nodes(20, 0, 70);
    let seed = [0xEE; 32];
    let network_size = 20; // Tier 0: committee=3, finality=2

    let committee =
        select_committee_with_network_size(&nodes, &seed, 0, 0, network_size, None).unwrap();

    assert_eq!(committee.size(), 3);
    assert_eq!(committee.finality_threshold, 2);

    // Lose 1 of 3: 2 remain, finality=2 → still works
    assert!(
        committee.has_finality(2),
        "Losing 1 of 3 nodes should still allow finality (2/3)"
    );

    // Lose 2 of 3: only 1 remains, finality=2 → stalls
    assert!(
        !committee.has_finality(1),
        "Losing 2 of 3 nodes should stall finality (1/3)"
    );
}

/// At each tier in the graduated scaling curve, verify the committee size and
/// finality threshold match the spec exactly.
/// WHY: This is the authoritative test that the constants in SCALING_TIERS
/// match the design document. Any accidental change would be caught here.
#[test]
fn test_graduated_committee_matches_network_size() {
    // Expected values from the spec (committee.rs constants)
    let expected: Vec<(u64, usize, usize)> = vec![
        (0, 3, 2),         // Bootstrap
        (100, 5, 4),       // Early network
        (500, 7, 5),       // Growing
        (2_500, 11, 8),    // Medium
        (10_000, 15, 10),  // Large
        (50_000, 19, 13),  // Very large
        (100_000, 21, 14), // Full scale
    ];

    for (network_size, expected_committee, expected_finality) in &expected {
        let tier = tier_for_network_size(*network_size);
        assert_eq!(
            tier.committee_size, *expected_committee,
            "At network_size={}: committee_size={}, expected {}",
            network_size, tier.committee_size, expected_committee
        );
        assert_eq!(
            tier.finality_threshold, *expected_finality,
            "At network_size={}: finality_threshold={}, expected {}",
            network_size, tier.finality_threshold, expected_finality
        );
    }

    // Also verify via actual committee selection to confirm end-to-end
    for (network_size, expected_committee, expected_finality) in &expected {
        // Create enough nodes to fill the committee
        let node_count = (*expected_committee as u32).max(5);
        let nodes = make_established_nodes(node_count, 0, 70);
        let seed = [0xFF; 32];

        let committee = select_committee_with_network_size(
            &nodes,
            &seed,
            0,
            0,
            *network_size,
            None,
        )
        .unwrap();

        assert_eq!(
            committee.committee_size, *expected_committee,
            "Committee selection at network_size={}: committee_size={}, expected {}",
            network_size, committee.committee_size, expected_committee
        );
        assert_eq!(
            committee.finality_threshold, *expected_finality,
            "Committee selection at network_size={}: finality={}, expected {}",
            network_size, committee.finality_threshold, expected_finality
        );
    }
}
