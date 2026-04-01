//! Consensus-level state sync protocol.
//!
//! This module manages the state machine for syncing a node that has fallen
//! behind the network. It sits above the network-layer sync (which handles
//! transport and peer tracking) and below the consensus engine (which uses
//! sync state to decide when to participate in block production).
//!
//! Responsibilities:
//! - Track local vs. network height and decide when sync is needed
//! - Generate batched sync requests (50 blocks at a time)
//! - Validate and buffer incoming sync responses
//! - Report sync progress for the UI layer

use serde::{Deserialize, Serialize};
use tracing::{debug, info, warn};

use gratia_core::types::{Block, NodeId};

// ============================================================================
// Constants
// ============================================================================

/// Number of blocks to request per sync batch.
/// WHY: Each block is up to 256 KB. 50 blocks = ~12.5 MB max, which keeps
/// individual gossipsub messages under reasonable size limits on mobile
/// networks without requiring too many round-trips.
pub const SYNC_BATCH_SIZE: u64 = 50;

/// Number of blocks behind the network before triggering sync.
/// WHY: Small gaps (1-4 blocks) are normal during brief connectivity hiccups
/// and get resolved by regular block gossip. Sync protocol overhead is only
/// worthwhile when we are meaningfully behind.
const SYNC_THRESHOLD: u64 = 5;

/// Maximum number of blocks to buffer before applying.
/// WHY: On mobile devices with limited RAM, we cap the buffer to prevent
/// memory exhaustion. 200 blocks * 256 KB max = ~50 MB worst case, but
/// typical blocks are much smaller (< 10 KB early network).
const MAX_BUFFER_SIZE: usize = 200;

// ============================================================================
// Sync State Machine
// ============================================================================

/// The state of the consensus-level sync protocol.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SyncState {
    /// Not syncing. Either fully caught up or no peers to compare against.
    Idle,

    /// A sync request has been sent and we are waiting for a response.
    Requesting,

    /// Actively downloading blocks from the network.
    Downloading {
        /// The block height we are trying to reach.
        target_height: u64,
        /// The highest block we have received so far during this sync.
        current: u64,
    },

    /// Downloaded blocks are being validated and applied to the chain.
    Applying,

    /// Fully synchronized with the network.
    Synced,
}

impl SyncState {
    /// Whether we are fully caught up with the network.
    pub fn is_synced(&self) -> bool {
        matches!(self, SyncState::Synced | SyncState::Idle)
    }

    /// Progress as a value from 0.0 to 1.0 for UI display.
    pub fn progress(&self) -> f64 {
        match self {
            SyncState::Idle | SyncState::Synced => 1.0,
            SyncState::Requesting => 0.0,
            SyncState::Downloading { target_height, current } => {
                if *target_height == 0 {
                    return 1.0;
                }
                (*current as f64 / *target_height as f64).min(1.0)
            }
            SyncState::Applying => 0.99,
        }
    }
}

// ============================================================================
// Sync Messages
// ============================================================================

/// A request from this node asking a peer for a range of blocks.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SyncRequest {
    /// First block height to fetch (inclusive).
    pub from_height: u64,
    /// Last block height to fetch (inclusive).
    pub to_height: u64,
    /// The node making the request.
    pub requester: NodeId,
}

/// A response from a peer containing the requested blocks.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SyncResponse {
    /// The blocks in ascending height order.
    pub blocks: Vec<Block>,
    /// The starting height of this batch (mirrors the request).
    pub from_height: u64,
    /// Whether the responding peer has more blocks beyond this batch.
    pub has_more: bool,
}

// ============================================================================
// Sync Protocol
// ============================================================================

/// Manages the consensus-level sync state machine.
///
/// `SyncProtocol` tracks this node's height against the best known network
/// height, generates batched block requests, validates incoming responses,
/// and reports progress.
pub struct SyncProtocol {
    /// Current state of the sync state machine.
    state: SyncState,
    /// This node's identity.
    node_id: NodeId,
    /// Our current confirmed block height.
    our_height: u64,
    /// Best known height from the network (updated via block gossip).
    network_height: u64,
    /// The height we are trying to reach in the current sync session.
    /// WHY: Separate from network_height because the network may advance
    /// while we are syncing. We sync to a fixed target and then re-evaluate.
    sync_target: u64,
    /// Buffer of blocks received from sync responses, not yet applied.
    blocks_received: Vec<Block>,
}

impl SyncProtocol {
    /// Create a new SyncProtocol for the given node.
    pub fn new(node_id: NodeId, initial_height: u64) -> Self {
        SyncProtocol {
            state: SyncState::Idle,
            node_id,
            our_height: initial_height,
            network_height: 0,
            sync_target: 0,
            blocks_received: Vec::new(),
        }
    }

    /// Get the current sync state.
    pub fn state(&self) -> SyncState {
        self.state
    }

    /// Get the current local height.
    pub fn our_height(&self) -> u64 {
        self.our_height
    }

    /// Get the best known network height.
    pub fn network_height(&self) -> u64 {
        self.network_height
    }

    /// Check if this node needs to sync.
    ///
    /// Returns true if the node is behind by `SYNC_THRESHOLD` or more blocks.
    /// Small gaps are expected to resolve via normal block gossip.
    pub fn needs_sync(our_height: u64, network_height: u64) -> bool {
        network_height > our_height && (network_height - our_height) >= SYNC_THRESHOLD
    }

    /// Create the next sync request for the current sync session.
    ///
    /// Requests up to `SYNC_BATCH_SIZE` blocks starting from our current
    /// height + 1 up to the sync target.
    pub fn create_sync_request(&mut self) -> Option<SyncRequest> {
        if !Self::needs_sync(self.our_height, self.network_height) {
            // We are close enough — no sync needed.
            self.state = SyncState::Synced;
            return None;
        }

        // Set the sync target if this is a new sync session.
        if self.sync_target <= self.our_height {
            self.sync_target = self.network_height;
            info!(
                our_height = self.our_height,
                target = self.sync_target,
                gap = self.sync_target - self.our_height,
                "Starting sync session",
            );
        }

        let from_height = self.our_height + 1;
        let to_height = (from_height + SYNC_BATCH_SIZE - 1).min(self.sync_target);

        self.state = SyncState::Requesting;

        debug!(
            from = from_height,
            to = to_height,
            batch_size = to_height - from_height + 1,
            "Creating sync request",
        );

        Some(SyncRequest {
            from_height,
            to_height,
            requester: self.node_id,
        })
    }

    /// Process a sync response from a peer.
    ///
    /// Validates that the blocks are contiguous and in the expected range,
    /// buffers them, and returns the validated blocks for the caller to
    /// apply to the chain.
    pub fn process_sync_response(&mut self, response: SyncResponse) -> Result<Vec<Block>, SyncError> {
        if response.blocks.is_empty() {
            warn!("Received empty sync response");
            self.state = if self.our_height >= self.sync_target {
                SyncState::Synced
            } else {
                SyncState::Idle
            };
            return Ok(Vec::new());
        }

        // Validate: first block should be at our_height + 1
        let first_height = response.blocks.first()
            .ok_or_else(|| SyncError::InvalidBlock {
                height: 0,
                reason: "First block missing in sync response".into(),
            })?
            .header.height;
        let expected_start = self.our_height + 1;
        if first_height != expected_start {
            return Err(SyncError::UnexpectedHeight {
                expected: expected_start,
                got: first_height,
            });
        }

        // Validate: blocks must be contiguous (each height = previous + 1)
        for window in response.blocks.windows(2) {
            let prev = window[0].header.height;
            let next = window[1].header.height;
            if next != prev + 1 {
                return Err(SyncError::NonContiguousBlocks {
                    expected: prev + 1,
                    got: next,
                });
            }
        }

        // Validate: each block's parent_hash should reference the previous block.
        // WHY: This catches peers sending random blocks that aren't part of the
        // same chain. The first block's parent is checked against our local tip
        // by the caller when applying blocks.
        for window in response.blocks.windows(2) {
            let prev_hash = match window[0].header.hash() {
                Ok(h) => h,
                Err(_) => {
                    return Err(SyncError::InvalidBlock {
                        height: window[0].header.height,
                        reason: "Failed to compute block hash".into(),
                    });
                }
            };
            if window[1].header.parent_hash != prev_hash {
                return Err(SyncError::InvalidBlock {
                    height: window[1].header.height,
                    reason: format!(
                        "Parent hash mismatch: expected {}, got {}",
                        prev_hash, window[1].header.parent_hash,
                    ),
                });
            }
        }

        let block_count = response.blocks.len();
        let last_height = response.blocks.last()
            .ok_or_else(|| SyncError::InvalidBlock {
                height: 0,
                reason: "Sync response contained no blocks after validation".into(),
            })?
            .header.height;

        // Buffer the blocks (with overflow protection)
        if self.blocks_received.len() + block_count > MAX_BUFFER_SIZE {
            return Err(SyncError::BufferFull {
                current: self.blocks_received.len(),
                incoming: block_count,
                max: MAX_BUFFER_SIZE,
            });
        }

        self.blocks_received.extend(response.blocks.clone());

        // Update state
        self.state = SyncState::Downloading {
            target_height: self.sync_target,
            current: last_height,
        };

        info!(
            received = block_count,
            last_height = last_height,
            target = self.sync_target,
            has_more = response.has_more,
            "Processed sync response",
        );

        Ok(response.blocks)
    }

    /// Notify the sync protocol that a block at the given height was observed.
    ///
    /// Called when a block arrives via gossip (not sync). Updates the tracked
    /// network height so the sync protocol knows whether it needs to catch up.
    pub fn on_block_received(&mut self, height: u64) {
        if height > self.network_height {
            self.network_height = height;
        }

        // If we were idle or synced and the network moved ahead, re-evaluate.
        if matches!(self.state, SyncState::Idle | SyncState::Synced) {
            if Self::needs_sync(self.our_height, self.network_height) {
                self.state = SyncState::Idle;
                debug!(
                    our = self.our_height,
                    network = self.network_height,
                    "Network advanced beyond sync threshold — sync may be needed",
                );
            }
        }
    }

    /// Notify the sync protocol that blocks have been applied to the chain.
    ///
    /// Called after the caller validates and commits the blocks returned by
    /// `process_sync_response`. Advances `our_height` and transitions state.
    pub fn mark_blocks_applied(&mut self, new_height: u64) {
        self.our_height = new_height;

        // Drain applied blocks from the buffer
        self.blocks_received.retain(|b| b.header.height > new_height);

        if self.our_height >= self.sync_target {
            // Reached the target — check if the network has moved further.
            if Self::needs_sync(self.our_height, self.network_height) {
                // Network moved ahead while we were syncing. Start a new session.
                self.sync_target = self.network_height;
                self.state = SyncState::Idle;
                info!(
                    our = self.our_height,
                    new_target = self.sync_target,
                    "Sync target reached but network advanced — re-syncing",
                );
            } else {
                self.state = SyncState::Synced;
                info!(
                    height = self.our_height,
                    "Sync complete — node is caught up",
                );
            }
        } else {
            // More batches needed
            self.state = SyncState::Downloading {
                target_height: self.sync_target,
                current: self.our_height,
            };
        }
    }

    /// Get the current sync progress as (current_height, target_height).
    ///
    /// Returns the local height and the sync target for UI display.
    /// When not syncing, both values equal our_height.
    pub fn sync_progress(&self) -> (u64, u64) {
        match self.state {
            SyncState::Idle | SyncState::Synced => (self.our_height, self.our_height),
            SyncState::Requesting => (self.our_height, self.sync_target),
            SyncState::Downloading { current, target_height } => (current, target_height),
            SyncState::Applying => (self.our_height, self.sync_target),
        }
    }

    /// Reset the sync state (e.g., after a chain wipe or restart).
    pub fn reset(&mut self, height: u64) {
        self.our_height = height;
        self.network_height = 0;
        self.sync_target = 0;
        self.blocks_received.clear();
        self.state = SyncState::Idle;
    }

    /// Number of blocks currently buffered and awaiting application.
    pub fn buffered_block_count(&self) -> usize {
        self.blocks_received.len()
    }
}

// ============================================================================
// Errors
// ============================================================================

/// Errors that can occur during the sync protocol.
#[derive(Debug, Clone)]
pub enum SyncError {
    /// Received blocks starting at an unexpected height.
    UnexpectedHeight {
        expected: u64,
        got: u64,
    },
    /// Blocks in the response are not contiguous.
    NonContiguousBlocks {
        expected: u64,
        got: u64,
    },
    /// A block in the response is invalid.
    InvalidBlock {
        height: u64,
        reason: String,
    },
    /// The block buffer is full — apply existing blocks before requesting more.
    BufferFull {
        current: usize,
        incoming: usize,
        max: usize,
    },
}

impl std::fmt::Display for SyncError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SyncError::UnexpectedHeight { expected, got } => {
                write!(f, "Expected blocks starting at height {}, got {}", expected, got)
            }
            SyncError::NonContiguousBlocks { expected, got } => {
                write!(f, "Non-contiguous blocks: expected height {}, got {}", expected, got)
            }
            SyncError::InvalidBlock { height, reason } => {
                write!(f, "Invalid block at height {}: {}", height, reason)
            }
            SyncError::BufferFull { current, incoming, max } => {
                write!(
                    f,
                    "Block buffer full: {} buffered + {} incoming exceeds max {}",
                    current, incoming, max
                )
            }
        }
    }
}

impl std::error::Error for SyncError {}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use gratia_core::types::{BlockHash, BlockHeader, NodeId};

    fn test_node_id() -> NodeId {
        NodeId([1u8; 32])
    }

    fn make_block(height: u64, parent_hash: BlockHash) -> Block {
        Block {
            header: BlockHeader {
                height,
                timestamp: Utc::now(),
                parent_hash,
                transactions_root: [0u8; 32],
                state_root: [0u8; 32],
                attestations_root: [0u8; 32],
                producer: NodeId([0u8; 32]),
                vrf_proof: vec![],
                active_miners: 0,
                geographic_diversity: 0,
            },
            transactions: vec![],
            attestations: vec![],
            validator_signatures: vec![],
        }
    }

    /// Build a contiguous chain of blocks where each block's parent_hash
    /// is the hash of the previous block.
    fn make_chain(from: u64, to: u64) -> Vec<Block> {
        let mut blocks = Vec::new();
        let mut parent = BlockHash([0u8; 32]);

        for h in from..=to {
            let block = make_block(h, parent);
            parent = block.header.hash().unwrap();
            blocks.push(block);
        }

        blocks
    }

    #[test]
    fn test_needs_sync_threshold() {
        // Exactly at threshold
        assert!(SyncProtocol::needs_sync(0, 5));
        // Below threshold
        assert!(!SyncProtocol::needs_sync(0, 4));
        // Well above threshold
        assert!(SyncProtocol::needs_sync(10, 100));
        // Already caught up
        assert!(!SyncProtocol::needs_sync(100, 100));
        // Ahead of network (shouldn't happen, but handle it)
        assert!(!SyncProtocol::needs_sync(100, 50));
    }

    #[test]
    fn test_initial_state() {
        let sp = SyncProtocol::new(test_node_id(), 0);
        assert_eq!(sp.state(), SyncState::Idle);
        assert_eq!(sp.our_height(), 0);
        assert_eq!(sp.network_height(), 0);
        assert_eq!(sp.sync_progress(), (0, 0));
    }

    #[test]
    fn test_create_sync_request_when_behind() {
        let mut sp = SyncProtocol::new(test_node_id(), 0);
        sp.network_height = 100;

        let req = sp.create_sync_request().unwrap();
        assert_eq!(req.from_height, 1);
        assert_eq!(req.to_height, SYNC_BATCH_SIZE);
        assert_eq!(req.requester, test_node_id());
        assert_eq!(sp.state(), SyncState::Requesting);
    }

    #[test]
    fn test_create_sync_request_small_gap() {
        let mut sp = SyncProtocol::new(test_node_id(), 0);
        sp.network_height = 3; // Only 3 blocks behind — below threshold

        let req = sp.create_sync_request();
        assert!(req.is_none());
        assert_eq!(sp.state(), SyncState::Synced);
    }

    #[test]
    fn test_create_sync_request_clamps_to_target() {
        let mut sp = SyncProtocol::new(test_node_id(), 0);
        sp.network_height = 10; // 10 blocks behind, but less than batch size

        let req = sp.create_sync_request().unwrap();
        assert_eq!(req.from_height, 1);
        assert_eq!(req.to_height, 10); // Clamped to target, not full batch
    }

    #[test]
    fn test_process_valid_response() {
        let mut sp = SyncProtocol::new(test_node_id(), 0);
        sp.network_height = 10;
        sp.sync_target = 10;

        let blocks = make_chain(1, 5);
        let response = SyncResponse {
            blocks: blocks.clone(),
            from_height: 1,
            has_more: true,
        };

        let result = sp.process_sync_response(response).unwrap();
        assert_eq!(result.len(), 5);
        assert!(matches!(sp.state(), SyncState::Downloading { target_height: 10, current: 5 }));
    }

    #[test]
    fn test_process_response_wrong_start_height() {
        let mut sp = SyncProtocol::new(test_node_id(), 0);
        sp.network_height = 10;
        sp.sync_target = 10;

        // Blocks start at 5 instead of expected 1
        let blocks = make_chain(5, 8);
        let response = SyncResponse {
            blocks,
            from_height: 5,
            has_more: true,
        };

        let result = sp.process_sync_response(response);
        assert!(result.is_err());
        match result.unwrap_err() {
            SyncError::UnexpectedHeight { expected, got } => {
                assert_eq!(expected, 1);
                assert_eq!(got, 5);
            }
            _ => panic!("Expected UnexpectedHeight error"),
        }
    }

    #[test]
    fn test_process_response_non_contiguous() {
        let mut sp = SyncProtocol::new(test_node_id(), 0);
        sp.network_height = 10;
        sp.sync_target = 10;

        // Create blocks with a gap (height 1, then 3)
        let mut blocks = vec![make_block(1, BlockHash([0u8; 32]))];
        blocks.push(make_block(3, BlockHash([1u8; 32]))); // Gap!

        let response = SyncResponse {
            blocks,
            from_height: 1,
            has_more: true,
        };

        let result = sp.process_sync_response(response);
        assert!(result.is_err());
        match result.unwrap_err() {
            SyncError::NonContiguousBlocks { expected, got } => {
                assert_eq!(expected, 2);
                assert_eq!(got, 3);
            }
            _ => panic!("Expected NonContiguousBlocks error"),
        }
    }

    #[test]
    fn test_mark_blocks_applied_reaches_target() {
        let mut sp = SyncProtocol::new(test_node_id(), 0);
        sp.network_height = 10;
        sp.sync_target = 10;

        sp.mark_blocks_applied(10);
        assert_eq!(sp.state(), SyncState::Synced);
        assert_eq!(sp.our_height(), 10);
    }

    #[test]
    fn test_mark_blocks_applied_more_to_go() {
        let mut sp = SyncProtocol::new(test_node_id(), 0);
        sp.network_height = 100;
        sp.sync_target = 100;

        sp.mark_blocks_applied(50);
        assert!(matches!(
            sp.state(),
            SyncState::Downloading { target_height: 100, current: 50 }
        ));
    }

    #[test]
    fn test_on_block_received_updates_network_height() {
        let mut sp = SyncProtocol::new(test_node_id(), 0);
        assert_eq!(sp.network_height(), 0);

        sp.on_block_received(42);
        assert_eq!(sp.network_height(), 42);

        // Should not decrease
        sp.on_block_received(30);
        assert_eq!(sp.network_height(), 42);
    }

    #[test]
    fn test_sync_progress_during_download() {
        let mut sp = SyncProtocol::new(test_node_id(), 0);
        sp.network_height = 100;
        sp.sync_target = 100;
        sp.state = SyncState::Downloading {
            target_height: 100,
            current: 50,
        };

        let (current, target) = sp.sync_progress();
        assert_eq!(current, 50);
        assert_eq!(target, 100);
    }

    #[test]
    fn test_reset() {
        let mut sp = SyncProtocol::new(test_node_id(), 50);
        sp.network_height = 100;
        sp.sync_target = 100;
        sp.state = SyncState::Downloading {
            target_height: 100,
            current: 75,
        };

        sp.reset(0);
        assert_eq!(sp.state(), SyncState::Idle);
        assert_eq!(sp.our_height(), 0);
        assert_eq!(sp.network_height(), 0);
        assert_eq!(sp.buffered_block_count(), 0);
    }

    #[test]
    fn test_full_sync_cycle() {
        let mut sp = SyncProtocol::new(test_node_id(), 0);
        sp.network_height = 10;

        // Step 1: Create request
        let req = sp.create_sync_request().unwrap();
        assert_eq!(req.from_height, 1);
        assert_eq!(req.to_height, 10);

        // Step 2: Process response
        let blocks = make_chain(1, 10);
        let response = SyncResponse {
            blocks,
            from_height: 1,
            has_more: false,
        };
        let applied = sp.process_sync_response(response).unwrap();
        assert_eq!(applied.len(), 10);

        // Step 3: Mark applied
        sp.mark_blocks_applied(10);
        assert_eq!(sp.state(), SyncState::Synced);
        assert_eq!(sp.sync_progress(), (10, 10));
    }

    #[test]
    fn test_multi_batch_sync() {
        let mut sp = SyncProtocol::new(test_node_id(), 0);
        sp.network_height = 100;

        // Batch 1: blocks 1-50
        let req1 = sp.create_sync_request().unwrap();
        assert_eq!(req1.from_height, 1);
        assert_eq!(req1.to_height, SYNC_BATCH_SIZE);

        let blocks1 = make_chain(1, 50);
        let resp1 = SyncResponse {
            blocks: blocks1,
            from_height: 1,
            has_more: true,
        };
        sp.process_sync_response(resp1).unwrap();
        sp.mark_blocks_applied(50);

        // Batch 2: blocks 51-100
        let req2 = sp.create_sync_request().unwrap();
        assert_eq!(req2.from_height, 51);
        assert_eq!(req2.to_height, 100);

        let blocks2 = make_chain(51, 100);
        let resp2 = SyncResponse {
            blocks: blocks2,
            from_height: 51,
            has_more: false,
        };
        sp.process_sync_response(resp2).unwrap();
        sp.mark_blocks_applied(100);

        assert_eq!(sp.state(), SyncState::Synced);
    }

    #[test]
    fn test_network_advances_during_sync() {
        let mut sp = SyncProtocol::new(test_node_id(), 0);
        sp.network_height = 50;

        // Start syncing to 50
        let _req = sp.create_sync_request().unwrap();
        let blocks = make_chain(1, 50);
        let resp = SyncResponse {
            blocks,
            from_height: 1,
            has_more: false,
        };
        sp.process_sync_response(resp).unwrap();

        // Network advanced to 100 while we were syncing
        sp.on_block_received(100);
        sp.mark_blocks_applied(50);

        // Should detect the new gap and go back to Idle for another session
        assert_eq!(sp.state(), SyncState::Idle);
        assert!(SyncProtocol::needs_sync(sp.our_height(), sp.network_height()));
    }

    #[test]
    fn test_empty_response() {
        let mut sp = SyncProtocol::new(test_node_id(), 0);
        sp.network_height = 10;
        sp.sync_target = 10;

        let response = SyncResponse {
            blocks: vec![],
            from_height: 1,
            has_more: false,
        };
        let result = sp.process_sync_response(response).unwrap();
        assert!(result.is_empty());
    }

    #[test]
    fn test_sync_state_progress() {
        assert_eq!(SyncState::Idle.progress(), 1.0);
        assert_eq!(SyncState::Synced.progress(), 1.0);
        assert_eq!(SyncState::Requesting.progress(), 0.0);
        assert_eq!(SyncState::Applying.progress(), 0.99);

        let downloading = SyncState::Downloading {
            target_height: 100,
            current: 50,
        };
        assert!((downloading.progress() - 0.5).abs() < f64::EPSILON);
    }
}
