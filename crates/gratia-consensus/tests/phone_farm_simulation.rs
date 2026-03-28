//! Phone Farm Attack Simulation
//!
//! Simulates an attacker operating a phone farm — multiple phones controlled
//! by the same entity — and verifies that:
//! 1. Graduated committee scaling limits attacker's committee representation
//! 2. Trust-tier filtering excludes fresh attack nodes from committees
//! 3. Cooldown tracking prevents the same nodes from appearing repeatedly

use gratia_consensus::committee::{
    select_committee, select_committee_with_network_size, CooldownTracker, EligibleNode,
};
use gratia_consensus::vrf::VrfPublicKey;
use gratia_core::types::NodeId;

// ============================================================================
// Helpers
// ============================================================================

/// Create a node with a unique ID derived from a u16 index.
/// WHY: u8 only gives 256 unique IDs; phone farm tests need up to 1000+.
fn make_node(index: u16, presence_score: u8, pol_days: u64) -> EligibleNode {
    let mut node_id = [0u8; 32];
    node_id[0] = (index & 0xFF) as u8;
    node_id[1] = (index >> 8) as u8;
    EligibleNode {
        node_id: NodeId(node_id),
        vrf_pubkey: VrfPublicKey {
            bytes: {
                let mut b = [0u8; 32];
                b[0] = (index & 0xFF) as u8;
                b[1] = (index >> 8) as u8;
                b
            },
        },
        presence_score,
        has_valid_pol: true,
        meets_minimum_stake: true,
        pol_days,
    }
}

/// Create N honest nodes with established trust (90 days PoL, score 70).
fn make_honest_nodes(count: u16, start_index: u16) -> Vec<EligibleNode> {
    (0..count)
        .map(|i| make_node(start_index + i, 70, 90))
        .collect()
}

/// Create N farm/attacker nodes with minimal trust (given days, score 50).
fn make_farm_nodes(count: u16, start_index: u16, pol_days: u64) -> Vec<EligibleNode> {
    (0..count)
        .map(|i| make_node(start_index + i, 50, pol_days))
        .collect()
}

/// Check if a node_id belongs to a set of nodes by index range.
fn is_in_range(node_id: &NodeId, start_index: u16, count: u16) -> bool {
    let idx = node_id.0[0] as u16 | ((node_id.0[1] as u16) << 8);
    idx >= start_index && idx < start_index + count
}

// ============================================================================
// Tests
// ============================================================================

/// Farm nodes with only 2 days of PoL history should be excluded from
/// committee selection when enough established (30+ day) nodes exist.
/// WHY: Progressive trust is the first line of defense against phone farms.
/// An attacker who just set up 50 phones cannot influence consensus.
#[test]
fn test_farm_nodes_excluded_by_trust_tier() {
    // 100 honest nodes with 90 days PoL
    let mut nodes = make_honest_nodes(100, 0);
    // 50 farm nodes with only 2 days PoL (below the 30-day committee threshold)
    nodes.extend(make_farm_nodes(50, 100, 2));

    let seed = [0xAA; 32];
    let network_size = 150;
    let committee =
        select_committee_with_network_size(&nodes, &seed, 0, 0, network_size, None).unwrap();

    // Verify: no farm node made it onto the committee
    for member in &committee.members {
        assert!(
            !is_in_range(&member.node_id, 100, 50),
            "Farm node {:?} should not be on the committee — it only has 2 days PoL",
            member.node_id
        );
    }
}

/// When the network is tiny and there are NOT enough established nodes to
/// fill the committee, the system falls back to using all eligible nodes
/// (including fresh ones). This prevents the network from stalling during
/// bootstrap.
#[test]
fn test_farm_nodes_used_as_fallback_when_needed() {
    // Only 2 established nodes — not enough for even the smallest committee (3)
    let mut nodes = make_honest_nodes(2, 0);
    // 50 farm nodes with 2 days PoL
    nodes.extend(make_farm_nodes(50, 2, 2));

    let seed = [0xBB; 32];
    let committee = select_committee(&nodes, &seed, 0, 0).unwrap();

    // Committee should still form — system falls back to basic eligibility
    assert!(
        committee.size() >= 3,
        "Committee should have formed with fallback nodes, got size {}",
        committee.size()
    );

    // At least some farm nodes must be present since there aren't enough established
    let farm_count = committee
        .members
        .iter()
        .filter(|m| is_in_range(&m.node_id, 2, 50))
        .count();
    assert!(
        farm_count > 0,
        "Expected farm nodes as fallback, but none were selected"
    );
}

/// Simulate 1000 committee selections where 10% of the network is controlled
/// by an attacker. Verify the attacker gets a committee majority less than 1%
/// of the time.
/// WHY: This is the core security guarantee — even with 10% of eligible nodes,
/// an attacker should almost never capture a committee majority.
#[test]
fn test_attacker_percentage_vs_committee_capture() {
    // 90 honest nodes, 10 attacker nodes — all established (30+ days) to be fair
    let mut nodes = make_honest_nodes(90, 0);
    // Give attacker nodes 90 days too so trust filtering is not the defense here —
    // we are testing the statistical committee selection properties.
    nodes.extend(make_farm_nodes(10, 90, 90));
    // Bump attacker scores to match honest to isolate the ratio effect
    for node in nodes.iter_mut().skip(90) {
        node.presence_score = 70;
    }

    let network_size = 100;
    let mut attacker_majority_count = 0;

    for trial in 0..1000u64 {
        // Use trial number to derive a unique seed per round
        let mut seed = [0u8; 32];
        seed[0..8].copy_from_slice(&trial.to_le_bytes());

        let committee =
            select_committee_with_network_size(&nodes, &seed, trial, 0, network_size, None)
                .unwrap();

        let attacker_count = committee
            .members
            .iter()
            .filter(|m| is_in_range(&m.node_id, 90, 10))
            .count();

        // Majority = more than half the committee
        if attacker_count > committee.size() / 2 {
            attacker_majority_count += 1;
        }
    }

    // With 10% of nodes, attacker should get majority < 1% of the time
    let capture_rate = attacker_majority_count as f64 / 1000.0;
    assert!(
        capture_rate < 0.01,
        "Attacker captured committee majority in {:.1}% of trials (expected < 1%)",
        capture_rate * 100.0
    );
}

/// With 50 attacker nodes in a 100-node network, the graduated scaling curve
/// limits the committee to only 3 validators. Verify the attacker does not
/// consistently dominate even at 50% network share.
/// WHY: Graduated scaling keeps committee small when the network is small,
/// making it harder for an attacker to flood the selection pool.
#[test]
fn test_large_farm_limited_by_graduated_scaling() {
    let mut nodes = make_honest_nodes(50, 0);
    // 50 attacker nodes — all established to bypass trust filtering
    nodes.extend(make_farm_nodes(50, 50, 90));
    for node in nodes.iter_mut().skip(50) {
        node.presence_score = 70;
    }

    let network_size = 100;

    // At 100 nodes, committee is 5 (tier 2)
    let tier = gratia_consensus::committee::tier_for_network_size(network_size);
    assert_eq!(tier.committee_size, 5);

    // Run 500 trials
    let mut attacker_full_capture = 0;
    for trial in 0..500u64 {
        let mut seed = [0u8; 32];
        seed[0..8].copy_from_slice(&trial.to_le_bytes());

        let committee =
            select_committee_with_network_size(&nodes, &seed, trial, 0, network_size, None)
                .unwrap();

        assert_eq!(
            committee.size(),
            5,
            "Committee should be 5 at 100-node tier"
        );

        let attacker_count = committee
            .members
            .iter()
            .filter(|m| is_in_range(&m.node_id, 50, 50))
            .count();

        // Full capture = all committee seats
        if attacker_count == committee.size() {
            attacker_full_capture += 1;
        }
    }

    // At 50/50 split, full capture should be rare (binomial: (0.5)^5 = 3.1%)
    let full_capture_rate = attacker_full_capture as f64 / 500.0;
    assert!(
        full_capture_rate < 0.10,
        "Attacker fully captured committee in {:.1}% of trials (expected < 10%)",
        full_capture_rate * 100.0
    );
}

/// Select a committee, record it in the cooldown tracker, then select again.
/// At the smallest tier (cooldown_rounds=5), recently-served nodes should be
/// excluded from the next committee.
/// WHY: Cooldown prevents a small group of attacker nodes from being selected
/// in back-to-back committees, forcing diversity.
#[test]
fn test_cooldown_prevents_repeated_selection() {
    // 20 nodes — enough to rotate through even with cooldown
    let nodes = make_honest_nodes(20, 0);

    let network_size = 20; // Tier 0: committee=3, cooldown=5

    let seed1 = [0xAA; 32];
    let committee1 =
        select_committee_with_network_size(&nodes, &seed1, 0, 0, network_size, None).unwrap();

    // Record committee1 in tracker
    let mut tracker = CooldownTracker::new();
    tracker.record_committee(&committee1.members);

    let c1_ids: Vec<NodeId> = committee1.members.iter().map(|m| m.node_id).collect();

    // Select committee2 with cooldown applied
    let seed2 = [0xBB; 32];
    let committee2 = select_committee_with_network_size(
        &nodes,
        &seed2,
        1,
        900,
        network_size,
        Some(&tracker),
    )
    .unwrap();

    let c2_ids: Vec<NodeId> = committee2.members.iter().map(|m| m.node_id).collect();

    // With cooldown=5, all members from committee1 should be excluded
    let overlap: Vec<&NodeId> = c2_ids.iter().filter(|id| c1_ids.contains(id)).collect();
    assert!(
        overlap.is_empty(),
        "Expected zero overlap with cooldown=5 and 20 nodes, but found {} overlapping: {:?}",
        overlap.len(),
        overlap
    );
}
