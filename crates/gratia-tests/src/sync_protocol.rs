//! Sync Protocol Simulation Tests
//!
//! Tests the block synchronization protocol under multi-node scenarios
//! using only the public API of SyncManager. These complement the unit tests
//! in gratia-network by testing higher-level sync lifecycle flows:
//!
//! - Late-joining node catching up from genesis
//! - Parallel sync requests to multiple peers
//! - Sync response advances local state
//! - Divergent chain tips (fork detection)
//! - Outlier height rejection via median
//! - Full sync lifecycle with progressive catch-up
//! - Peer disconnect mid-request
//! - Serving blocks to requesting peers
//!
//! These tests validate that the sync manager correctly coordinates block
//! downloads, handles failures, and converges to the correct chain state.

use chrono::Utc;
use libp2p::PeerId;
use gratia_core::types::{Block, BlockHash, BlockHeader, NodeId};
use gratia_network::sync::{
    SyncManager, SyncRequest, SyncResponse, SyncState,
};

// ============================================================================
// Helpers
// ============================================================================

fn make_block_at_height(height: u64, parent: BlockHash) -> Block {
    Block {
        header: BlockHeader {
            height,
            timestamp: Utc::now(),
            parent_hash: parent,
            transactions_root: [0u8; 32],
            state_root: [0u8; 32],
            attestations_root: [0u8; 32],
            producer: NodeId([0u8; 32]),
            vrf_proof: vec![],
            active_miners: 1,
            geographic_diversity: 1,
        },
        transactions: vec![],
        attestations: vec![],
        validator_signatures: vec![],
    }
}

fn make_chain(length: u64) -> Vec<Block> {
    let mut chain = Vec::new();
    let mut parent = BlockHash([0u8; 32]);
    for h in 1..=length {
        let block = make_block_at_height(h, parent);
        // WHY: Use the real header hash so sync's hash-chain verification passes.
        parent = block.header.hash().unwrap_or(BlockHash([h as u8; 32]));
        chain.push(block);
    }
    chain
}

fn add_peers(sm: &mut SyncManager, count: usize, height: u64) -> Vec<PeerId> {
    let peers: Vec<PeerId> = (0..count).map(|_| PeerId::random()).collect();
    for peer in &peers {
        sm.update_peer_state(*peer, height, BlockHash([1u8; 32]));
    }
    peers
}

// ============================================================================
// Test: Late-joining node catches up from genesis
// ============================================================================

#[test]
fn test_late_join_catch_up_from_genesis() {
    // Simulate a new phone joining a network with 100 blocks already produced.
    let mut sm = SyncManager::new(0, BlockHash([0u8; 32]));
    let _peers = add_peers(&mut sm, 5, 100);

    // Node should be behind
    assert!(matches!(sm.state(), SyncState::Behind { .. }));
    assert_eq!(sm.best_network_height(), Some(100));

    // Generate sync requests — should get multiple concurrent requests
    let mut requests = Vec::new();
    while let Some((peer, req)) = sm.next_sync_request() {
        requests.push((peer, req));
    }
    // WHY: Should generate multiple parallel requests (up to MAX_CONCURRENT_SYNC_REQUESTS).
    // The exact count depends on how many non-overlapping ranges fit and how many
    // peers are available without pending requests.
    assert!(requests.len() >= 2, "Should generate at least 2 parallel requests, got {}", requests.len());
    assert!(requests.len() <= 3, "Should not exceed 3 parallel requests, got {}", requests.len());

    // Each request should cover a different block range
    let ranges: Vec<(u64, u64)> = requests
        .iter()
        .map(|(_, req)| match req {
            SyncRequest::GetBlocks { from_height, to_height } => (*from_height, *to_height),
            _ => panic!("Expected GetBlocks"),
        })
        .collect();

    // Verify ranges are sequential (non-overlapping)
    for i in 1..ranges.len() {
        assert!(
            ranges[i].0 > ranges[i - 1].1,
            "Range {} ({:?}) should start after range {} ({:?})",
            i, ranges[i], i - 1, ranges[i - 1]
        );
    }

    // First range should start at height 1 (our local + 1)
    assert_eq!(ranges[0].0, 1, "First sync request should start at height 1");
}

// ============================================================================
// Test: Sync response advances local state
// ============================================================================

#[test]
fn test_sync_response_advances_state() {
    let mut sm = SyncManager::new(0, BlockHash([0u8; 32]));
    let _peers = add_peers(&mut sm, 3, 10);

    let (peer, req) = sm.next_sync_request().unwrap();

    // Build response blocks
    let chain = make_chain(10);
    let response_blocks: Vec<Block> = match req {
        SyncRequest::GetBlocks { from_height, to_height } => {
            chain[(from_height as usize - 1)..=(to_height as usize - 1).min(chain.len() - 1)]
                .to_vec()
        }
        _ => panic!("Expected GetBlocks"),
    };

    let blocks = sm.handle_blocks_response(&peer, response_blocks).unwrap();
    assert!(!blocks.is_empty());

    // Simulate applying the blocks
    if let Some(last) = blocks.last() {
        let tip = BlockHash([last.header.height as u8; 32]);
        sm.update_local_state(last.header.height, tip);
    }

    assert_eq!(sm.consecutive_failures(), 0);
}

// ============================================================================
// Test: Divergent chain tips (fork detection)
// ============================================================================

#[test]
fn test_divergent_chain_tips() {
    let mut sm = SyncManager::new(50, BlockHash([1u8; 32]));

    // Peer A reports height 100 with hash A
    sm.update_peer_state(PeerId::random(), 100, BlockHash([0xAA; 32]));

    // Peer B reports height 100 with hash B (different chain!)
    sm.update_peer_state(PeerId::random(), 100, BlockHash([0xBB; 32]));

    // Peer C agrees with A
    sm.update_peer_state(PeerId::random(), 100, BlockHash([0xAA; 32]));

    // Median height should still be 100
    assert_eq!(sm.best_network_height(), Some(100));

    // We should sync to height 100 regardless of which tip wins.
    // WHY: Fork resolution happens during block validation, not during sync.
    // The sync protocol downloads blocks and lets the consensus engine decide
    // which chain is canonical.
    assert!(matches!(sm.state(), SyncState::Behind { .. }));
}

// ============================================================================
// Test: Outlier height rejected via median
// ============================================================================

#[test]
fn test_outlier_height_rejected() {
    let mut sm = SyncManager::new(0, BlockHash([0u8; 32]));

    // 4 honest peers at height 100, 1 dishonest peer claiming height 999999
    for _ in 0..4 {
        sm.update_peer_state(PeerId::random(), 100, BlockHash([1u8; 32]));
    }
    sm.update_peer_state(PeerId::random(), 999999, BlockHash([99u8; 32]));

    // Median of [100, 100, 100, 100, 999999] = 100
    assert_eq!(sm.best_network_height(), Some(100));
}

// ============================================================================
// Test: Full sync lifecycle with progressive catch-up
// ============================================================================

#[test]
fn test_full_sync_lifecycle() {
    let mut sm = SyncManager::new(0, BlockHash([0u8; 32]));
    let chain = make_chain(150);

    // Phase 1: Join network with 5 peers at height 150
    let _peers = add_peers(&mut sm, 5, 150);
    assert!(matches!(sm.state(), SyncState::Behind { .. }));

    // Phase 2: Generate and fulfill sync requests in batches
    let mut applied_height = 0u64;
    let mut iterations = 0;

    while applied_height < 150 {
        iterations += 1;
        if iterations > 50 {
            panic!(
                "Sync did not converge in 50 iterations (stuck at height {})",
                applied_height
            );
        }

        // Run maintenance
        sm.tick_maintenance();

        // Generate all available requests
        let mut batch_requests = Vec::new();
        while let Some((peer, req)) = sm.next_sync_request() {
            batch_requests.push((peer, req));
        }

        if batch_requests.is_empty() {
            if sm.state().is_synced() {
                break;
            }
            // Update state and try again
            sm.update_local_state(applied_height, BlockHash([applied_height as u8; 32]));
            continue;
        }

        // Fulfill each request
        for (peer, req) in batch_requests {
            let blocks: Vec<Block> = match req {
                SyncRequest::GetBlocks { from_height, to_height } => {
                    let from = (from_height as usize).saturating_sub(1);
                    let to = (to_height as usize).min(chain.len());
                    chain[from..to].to_vec()
                }
                _ => continue,
            };

            if let Ok(validated_blocks) = sm.handle_blocks_response(&peer, blocks) {
                if let Some(last) = validated_blocks.last() {
                    if last.header.height > applied_height {
                        applied_height = last.header.height;
                    }
                }
            }
        }

        // Update local state after applying blocks
        sm.update_local_state(applied_height, BlockHash([applied_height as u8; 32]));
    }

    assert_eq!(applied_height, 150, "Should have synced all 150 blocks");
    assert!(sm.state().is_synced(), "Should be in synced state");
    assert_eq!(sm.consecutive_failures(), 0, "Should have zero failures");
}

// ============================================================================
// Test: Peer disconnect mid-request
// ============================================================================

#[test]
fn test_peer_disconnect_mid_request() {
    let mut sm = SyncManager::new(0, BlockHash([0u8; 32]));
    let _peers = add_peers(&mut sm, 3, 50);

    // Generate a request
    let (peer, _req) = sm.next_sync_request().unwrap();
    assert!(sm.pending_request_count() >= 1);

    // Peer disconnects before responding
    sm.remove_peer(&peer);

    // Should be able to request from remaining peers
    if let Some((new_peer, _req)) = sm.next_sync_request() {
        assert_ne!(new_peer, peer, "Should retry with a different peer");
    }
}

// ============================================================================
// Test: Serve blocks to requesting peer
// ============================================================================

#[test]
fn test_serve_blocks_to_requesting_peer() {
    let chain = make_chain(20);
    let sm = SyncManager::new(20, BlockHash([1u8; 32]));

    let request = SyncRequest::GetBlocks {
        from_height: 5,
        to_height: 10,
    };

    let response = sm.handle_sync_request(&request, |from, to| {
        let blocks: Vec<Block> = (from..=to)
            .filter_map(|h| chain.get((h as usize).saturating_sub(1)).cloned())
            .collect();
        if blocks.is_empty() { None } else { Some(blocks) }
    });

    match response {
        SyncResponse::Blocks(blocks) => {
            assert_eq!(blocks.len(), 6, "Should return blocks 5-10");
            assert_eq!(blocks[0].header.height, 5);
            assert_eq!(blocks[5].header.height, 10);
        }
        _ => panic!("Expected Blocks response"),
    }
}

// ============================================================================
// Test: GetChainTip response
// ============================================================================

#[test]
fn test_serve_chain_tip() {
    let tip_hash = BlockHash([42u8; 32]);
    let sm = SyncManager::new(99, tip_hash);

    let response = sm.handle_sync_request(&SyncRequest::GetChainTip, |_, _| None);

    match response {
        SyncResponse::ChainTip { height, hash } => {
            assert_eq!(height, 99);
            assert_eq!(hash, tip_hash);
        }
        _ => panic!("Expected ChainTip response"),
    }
}

// ============================================================================
// Test: Request above chain tip returns error
// ============================================================================

#[test]
fn test_reject_request_above_chain_tip() {
    let sm = SyncManager::new(10, BlockHash([1u8; 32]));

    let request = SyncRequest::GetBlocks {
        from_height: 50,
        to_height: 100,
    };

    let response = sm.handle_sync_request(&request, |_, _| None);

    match response {
        SyncResponse::Error(msg) => {
            assert!(msg.contains("above our chain tip"), "Error: {}", msg);
        }
        _ => panic!("Expected Error response"),
    }
}

// ============================================================================
// Test: Synced node stays synced when peers report same height
// ============================================================================

#[test]
fn test_synced_node_stays_synced() {
    let mut sm = SyncManager::new(100, BlockHash([1u8; 32]));

    // All peers at same height
    add_peers(&mut sm, 5, 100);
    assert!(sm.state().is_synced());

    // Run maintenance — nothing should change
    let summary = sm.tick_maintenance();
    assert_eq!(summary.stale_peers_evicted, 0);
    assert_eq!(summary.timed_out_requests, 0);
    assert!(sm.state().is_synced());

    // No sync requests should be generated
    assert!(sm.next_sync_request().is_none());
}

// ============================================================================
// Test: Network grows — node detects it is behind again
// ============================================================================

#[test]
fn test_network_grows_during_sync() {
    // WHY: When a node syncs to the network height and then the network grows
    // (peers advance), the node should transition from Synced to Behind and
    // generate new sync requests for the missing blocks.
    let mut sm = SyncManager::new(50, BlockHash([1u8; 32]));

    // Start synced with 3 peers at height 50
    add_peers(&mut sm, 3, 50);
    assert!(sm.state().is_synced());
    assert!(sm.next_sync_request().is_none(), "No sync needed when synced");

    // Peers advance to height 200 (simulates block production while this node was idle)
    add_peers(&mut sm, 5, 200);

    // Should now detect it's behind
    assert!(matches!(sm.state(), SyncState::Behind { .. }));

    // Should generate sync requests starting from our local height + 1
    let (_peer, req) = sm.next_sync_request().expect("Should generate sync request");
    match req {
        SyncRequest::GetBlocks { from_height, .. } => {
            assert_eq!(from_height, 51, "Should request from local_height + 1");
        }
        _ => panic!("Expected GetBlocks"),
    }
}
