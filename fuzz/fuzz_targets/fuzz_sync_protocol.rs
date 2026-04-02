#![no_main]
//! Fuzz target: Sync protocol message deserialization and handling.
//!
//! Feeds random bytes to SyncRequest, SyncResponse, and SyncProtocolMessage
//! deserialization. Also tests SyncManager::handle_blocks_response with
//! fuzzed block data. Must not panic on any input.

use libfuzzer_sys::fuzz_target;

use gratia_network::sync::{
    SyncManager, SyncProtocolMessage, SyncRequest, SyncResponse,
};
use gratia_core::types::{Block, BlockHash};

fuzz_target!(|data: &[u8]| {
    // Deserialize as each sync protocol type
    let _ = SyncRequest::from_bytes(data);
    let _ = SyncResponse::from_bytes(data);
    let _ = SyncProtocolMessage::from_bytes(data);

    // Also try raw bincode paths
    let _ = bincode::deserialize::<SyncRequest>(data);
    let _ = bincode::deserialize::<SyncResponse>(data);
    let _ = bincode::deserialize::<SyncProtocolMessage>(data);

    // If we can deserialize as a SyncResponse containing blocks,
    // feed those blocks to SyncManager::handle_blocks_response.
    if let Ok(SyncResponse::Blocks(blocks)) = bincode::deserialize::<SyncResponse>(data) {
        let mut sm = SyncManager::new(0, BlockHash([0u8; 32]));
        // We need a pending request for handle_blocks_response to work.
        // Use a deterministic PeerId from the fuzz data.
        let peer = libp2p::PeerId::random();
        sm.update_peer_state(peer, 1000, BlockHash([1u8; 32]));
        // Manually trigger a sync request to register the peer
        let _ = sm.next_sync_request();
        // Now feed the fuzzed blocks
        let _ = sm.handle_blocks_response(&peer, blocks);
    }

    // If we can deserialize blocks directly, try handle_blocks_response
    if let Ok(blocks) = bincode::deserialize::<Vec<Block>>(data) {
        if !blocks.is_empty() {
            let mut sm = SyncManager::new(0, BlockHash([0u8; 32]));
            let peer = libp2p::PeerId::random();
            sm.update_peer_state(peer, 1000, BlockHash([1u8; 32]));
            let _ = sm.next_sync_request();
            let _ = sm.handle_blocks_response(&peer, blocks);
        }
    }
});
