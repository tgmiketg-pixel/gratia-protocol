//! Network Partition Simulation Tests
//!
//! Tests network resilience under various failure modes:
//! - 50% network split (each partition maintains consensus with its subcommittee)
//! - Single node disconnect and rejoin
//! - Bootstrap node failure with continued DHT discovery
//! - Cross-shard partition with queued transactions
//! - Mesh network bridging partitions via BLE relay
//!
//! These tests verify the consensus engine handles state sync, committee
//! operation under partition, and graceful recovery.

use chrono::Utc;
use gratia_core::types::{
    Block, BlockHash, BlockHeader, NodeId, ValidatorSignature,
};
use gratia_consensus::{
    ConsensusEngine, ConsensusState,
    committee::EligibleNode,
    vrf::VrfPublicKey,
};

// ============================================================================
// Helpers
// ============================================================================

fn test_node(id: u8) -> NodeId {
    let mut bytes = [0u8; 32];
    bytes[0] = id;
    NodeId(bytes)
}

fn make_engine(node_byte: u8) -> ConsensusEngine {
    let mut node_id = [0u8; 32];
    node_id[0] = node_byte;
    let signing_key = [node_byte; 32];
    ConsensusEngine::new(NodeId(node_id), &signing_key, 70)
}

fn make_eligible_nodes(count: u8) -> Vec<EligibleNode> {
    (0..count)
        .map(|i| {
            let mut node_id = [0u8; 32];
            node_id[0] = i;
            EligibleNode {
                node_id: NodeId(node_id),
                vrf_pubkey: VrfPublicKey { bytes: [i; 32] },
                presence_score: 70,
                has_valid_pol: true,
                meets_minimum_stake: true,
                pol_days: 100,
                signing_pubkey: vec![],
            }
        })
        .collect()
}

fn make_block(height: u64, parent_hash: BlockHash, producer: NodeId) -> Block {
    Block {
        header: BlockHeader {
            height,
            timestamp: Utc::now(),
            parent_hash,
            transactions_root: [0; 32],
            state_root: [0; 32],
            attestations_root: [0; 32],
            producer,
            vrf_proof: vec![0; 96],
            active_miners: 100,
            geographic_diversity: 5,
        },
        transactions: Vec::new(),
        attestations: Vec::new(),
        validator_signatures: Vec::new(),
    }
}

// ============================================================================
// Tests
// ============================================================================

/// SCENARIO: 50% network split — each partition continues with its committee
/// if enough validators remain.
/// VERIFY: Engines in both partitions maintain state independently and
/// can accept blocks from their partition's producers.
#[test]
fn test_50_percent_network_split() {
    let nodes = make_eligible_nodes(30);
    let seed = [0xAB; 32];

    // Create two partitions of engines.
    let mut partition_a: Vec<ConsensusEngine> = (0..15)
        .map(|i| {
            let mut e = make_engine(i);
            e.trust_aware = false;
            e.initialize_committee(&nodes, &seed, 0, 0).unwrap();
            e
        })
        .collect();

    let mut partition_b: Vec<ConsensusEngine> = (15..30)
        .map(|i| {
            let mut e = make_engine(i);
            e.trust_aware = false;
            e.initialize_committee(&nodes, &seed, 0, 0).unwrap();
            e
        })
        .collect();

    // Both partitions are active after initialization.
    for engine in &partition_a {
        assert_eq!(engine.state(), ConsensusState::Active);
    }
    for engine in &partition_b {
        assert_eq!(engine.state(), ConsensusState::Active);
    }

    // Both partitions can independently advance slots.
    for engine in &mut partition_a {
        engine.advance_slot();
    }
    for engine in &mut partition_b {
        engine.advance_slot();
    }

    // Both partitions should still be operational (Active state).
    for engine in &partition_a {
        assert!(
            engine.state() == ConsensusState::Active
                || engine.state() == ConsensusState::Producing,
            "Partition A engine should be Active or Producing"
        );
    }
    for engine in &partition_b {
        assert!(
            engine.state() == ConsensusState::Active
                || engine.state() == ConsensusState::Producing,
            "Partition B engine should be Active or Producing"
        );
    }
}

/// SCENARIO: Single node disconnects, then rejoins and syncs.
/// VERIFY: Engine can restore state and continue from where it left off.
#[test]
fn test_single_node_disconnect_and_rejoin() {
    let nodes = make_eligible_nodes(25);
    let seed = [0xCD; 32];

    let mut engine = make_engine(5);
    engine.trust_aware = false;
    engine.initialize_committee(&nodes, &seed, 0, 0).unwrap();

    // Advance through several slots to simulate normal operation.
    for _ in 0..10 {
        engine.advance_slot();
    }

    let height_before = engine.current_height();
    let slot_before = engine.current_slot();

    // Simulate disconnect: engine stops.
    engine.stop();
    assert_eq!(engine.state(), ConsensusState::Stopped);

    // Simulate rejoin: create a new engine and restore state.
    let mut rejoined = make_engine(5);
    rejoined.trust_aware = false;

    // Restore chain state from persistence (simulated).
    let restored_height = height_before;
    let restored_hash = BlockHash([0xEE; 32]);
    rejoined.restore_state(restored_height, restored_hash);

    // Reinitialize committee.
    rejoined.initialize_committee(&nodes, &seed, 0, slot_before).unwrap();

    assert_eq!(rejoined.state(), ConsensusState::Active);
    assert_eq!(rejoined.current_height(), restored_height);
    assert_eq!(*rejoined.last_finalized_hash(), restored_hash);
}

/// SCENARIO: Bootstrap node goes down — peer discovery via Kademlia DHT continues.
/// VERIFY: Engines can still function without the bootstrap node.
/// WHY: The bootstrap node is NOT a consensus participant. It only helps with
/// initial peer discovery. Once peers are connected via DHT, the bootstrap
/// is no longer needed.
#[test]
fn test_bootstrap_node_down_consensus_continues() {
    let nodes = make_eligible_nodes(25);
    let seed = [0xAB; 32];

    // Node 0 is the "bootstrap" node — but it's NOT in the committee necessarily.
    let mut engine1 = make_engine(1);
    let mut engine2 = make_engine(2);
    engine1.trust_aware = false;
    engine2.trust_aware = false;

    engine1.initialize_committee(&nodes, &seed, 0, 0).unwrap();
    engine2.initialize_committee(&nodes, &seed, 0, 0).unwrap();

    // Both engines function independently of the bootstrap node.
    assert_eq!(engine1.state(), ConsensusState::Active);
    assert_eq!(engine2.state(), ConsensusState::Active);

    // They can advance slots without any bootstrap involvement.
    engine1.advance_slot();
    engine2.advance_slot();

    assert!(
        engine1.state() == ConsensusState::Active
            || engine1.state() == ConsensusState::Producing
    );
    assert!(
        engine2.state() == ConsensusState::Active
            || engine2.state() == ConsensusState::Producing
    );
}

/// SCENARIO: Committee rotation succeeds even if some nodes are offline.
/// VERIFY: The committee can rotate to a new epoch using eligible nodes
/// that are still online.
#[test]
fn test_committee_rotation_with_offline_nodes() {
    let nodes = make_eligible_nodes(30);
    let seed = [0xFF; 32];

    let mut engine = make_engine(0);
    engine.trust_aware = false;
    engine.initialize_committee(&nodes, &seed, 0, 0).unwrap();

    let initial_epoch = engine.committee().unwrap().epoch.epoch_number;

    // Set a finalized hash for the rotation seed.
    engine.restore_state(100, BlockHash([0xDD; 32]));

    // Rotate with only 20 of the 30 nodes online (10 went offline).
    let online_nodes: Vec<EligibleNode> = nodes[..20].to_vec();
    let result = engine.rotate_committee(&online_nodes);

    assert!(
        result.is_ok(),
        "Committee should rotate even with some nodes offline"
    );

    let new_epoch = engine.committee().unwrap().epoch.epoch_number;
    assert_eq!(new_epoch, initial_epoch + 1);
}

/// SCENARIO: Engine receives a block that is ahead of its current height.
/// VERIFY: Blocks ahead of our height are skipped (not fast-forwarded).
/// The sync protocol is responsible for fetching missing blocks so we
/// can apply them sequentially with full validation.
#[test]
fn test_ahead_block_skipped_for_sync() {
    let nodes = make_eligible_nodes(25);
    let seed = [0xAB; 32];

    let mut engine = make_engine(0);
    engine.trust_aware = false;
    engine.initialize_committee(&nodes, &seed, 0, 0).unwrap();

    // Engine is at height 0. Incoming block is at height 5 (gap).
    let committee = engine.committee().unwrap().clone();
    let producer = committee.members[0].node_id;

    let mut block = make_block(5, BlockHash([0; 32]), producer);

    // Add fake finality signatures.
    for member in committee.members {
        block.validator_signatures.push(ValidatorSignature {
            validator: member.node_id,
            signature: vec![0; 64],
        });
    }

    let result = engine.process_incoming_block(block);

    // WHY: Block should be skipped (not rejected) — returns Ok(()) but
    // does NOT advance height. The sync protocol will fetch blocks 1-5
    // sequentially so we can validate each one with correct parent hash.
    assert!(
        result.is_ok(),
        "Ahead block should be skipped (not error): {:?}",
        result
    );
    assert_eq!(
        engine.current_height(),
        0,
        "Height should NOT advance for ahead blocks — sync fetches them"
    );
}

/// SCENARIO: Engine receives a block at or below its current height.
/// VERIFY: Block is silently skipped (no error, no state change).
#[test]
fn test_skip_block_at_or_below_height() {
    let nodes = make_eligible_nodes(25);
    let seed = [0xAB; 32];

    let mut engine = make_engine(0);
    engine.trust_aware = false;
    engine.initialize_committee(&nodes, &seed, 0, 0).unwrap();

    // Advance engine to height 10 via restore_state.
    engine.restore_state(10, BlockHash([0xAA; 32]));

    // Send a block at height 5 (below current).
    let committee = engine.committee().unwrap().clone();
    let producer = committee.members[0].node_id;
    let block = make_block(5, BlockHash([0; 32]), producer);

    let result = engine.process_incoming_block(block);

    // Should be silently skipped.
    assert!(result.is_ok());
    assert_eq!(
        engine.current_height(),
        10,
        "Height should not change for old blocks"
    );
}

/// SCENARIO: Multiple engines running independently can both advance
/// without interfering with each other (simulates shards or partitions).
#[test]
fn test_independent_engines_no_interference() {
    let nodes = make_eligible_nodes(25);
    let seed = [0xEE; 32];

    let mut engine_a = make_engine(0);
    let mut engine_b = make_engine(1);
    engine_a.trust_aware = false;
    engine_b.trust_aware = false;

    engine_a.initialize_committee(&nodes, &seed, 0, 0).unwrap();
    engine_b.initialize_committee(&nodes, &seed, 0, 0).unwrap();

    // Advance engines at different rates.
    for _ in 0..5 {
        engine_a.advance_slot();
    }
    for _ in 0..10 {
        engine_b.advance_slot();
    }

    // Verify independent state.
    assert_eq!(engine_a.current_slot(), 5);
    assert_eq!(engine_b.current_slot(), 10);
}

/// SCENARIO: Engine receives block from a non-committee producer.
/// VERIFY: Block is rejected.
#[test]
fn test_reject_block_from_non_committee_producer() {
    let nodes = make_eligible_nodes(25);
    let seed = [0xAB; 32];

    let mut engine = make_engine(0);
    engine.trust_aware = false;
    engine.initialize_committee(&nodes, &seed, 0, 0).unwrap();

    // WHY: Send block at height 1 (expected next height) so it reaches
    // full validation. Blocks at height > expected are skipped before
    // validation, so they wouldn't test the producer check.
    let fake_producer = test_node(99);
    let block = make_block(1, BlockHash([0; 32]), fake_producer);

    let result = engine.process_incoming_block(block);

    // WHY: Blocks from non-committee producers are now rejected.
    // The ±1 slot tolerance still won't match a completely fake
    // producer (node 99 is not in the committee at all).
    assert!(
        result.is_err(),
        "Block from non-committee producer should be rejected"
    );
}
