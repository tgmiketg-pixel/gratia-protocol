//! State synchronization protocol.
//!
//! Handles synchronizing blockchain state between peers. When a node
//! joins the network or falls behind, this module coordinates requesting
//! missing blocks from peers to catch up.

use std::collections::HashMap;

use chrono::{DateTime, Utc};
use libp2p::PeerId;
use serde::{Deserialize, Serialize};

use gratia_core::types::{Block, BlockHash};

use crate::error::NetworkError;

// ============================================================================
// Sync Protocol Message (gossipsub transport wrapper)
// ============================================================================

/// Wrapper for sync messages transported over gossipsub.
///
/// WHY: We don't have a dedicated request/response protocol yet, so sync
/// messages travel over gossipsub with embedded routing info. Each node
/// checks the target field and ignores messages not addressed to it.
/// This is acceptable for a small testnet (2-10 nodes). In Phase 3,
/// this should be replaced with libp2p's request-response protocol for
/// bandwidth efficiency.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SyncProtocolMessage {
    /// The source peer (as raw bytes from PeerId::to_bytes()).
    pub source: Vec<u8>,
    /// The target peer (as raw bytes from PeerId::to_bytes()).
    /// Empty vec means broadcast (e.g., chain tip announcements).
    pub target: Vec<u8>,
    /// The actual sync payload.
    pub payload: SyncPayload,
}

/// The payload inside a sync protocol message.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum SyncPayload {
    /// A sync request from a peer.
    Request(SyncRequest),
    /// A sync response to a peer.
    Response(SyncResponse),
}

impl SyncProtocolMessage {
    /// Serialize to bytes for gossipsub transport.
    pub fn to_bytes(&self) -> Result<Vec<u8>, NetworkError> {
        bincode::serialize(self).map_err(NetworkError::from)
    }

    /// Deserialize from bytes.
    pub fn from_bytes(data: &[u8]) -> Result<Self, NetworkError> {
        bincode::deserialize(data).map_err(NetworkError::from)
    }

    /// Check if this message is addressed to the given peer.
    /// Returns true if the target matches or if the target is empty (broadcast).
    pub fn is_for_peer(&self, peer_bytes: &[u8]) -> bool {
        self.target.is_empty() || self.target == peer_bytes
    }
}

// ============================================================================
// Sync State
// ============================================================================

/// The synchronization state of this node relative to the network.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyncState {
    /// Node is fully synchronized with the network.
    Synced,
    /// Node is actively downloading blocks to catch up.
    Syncing {
        /// Current local block height.
        local_height: u64,
        /// Best known height from peers.
        target_height: u64,
    },
    /// Node knows it is behind but has not started syncing.
    Behind {
        /// Current local block height.
        local_height: u64,
        /// Best known height from peers.
        network_height: u64,
    },
    /// Cannot determine sync state (e.g., no peers connected).
    Unknown,
}

impl SyncState {
    /// Check if the node is fully synced.
    pub fn is_synced(&self) -> bool {
        matches!(self, SyncState::Synced)
    }

    /// Get the sync progress as a percentage (0.0 to 1.0).
    /// Returns 1.0 if synced or unknown.
    pub fn progress(&self) -> f64 {
        match self {
            SyncState::Synced => 1.0,
            SyncState::Syncing {
                local_height,
                target_height,
            } => {
                if *target_height == 0 {
                    return 1.0;
                }
                *local_height as f64 / *target_height as f64
            }
            SyncState::Behind {
                local_height,
                network_height,
            } => {
                if *network_height == 0 {
                    return 1.0;
                }
                *local_height as f64 / *network_height as f64
            }
            SyncState::Unknown => 1.0,
        }
    }
}

// ============================================================================
// Sync Protocol Messages
// ============================================================================

/// Request/response messages for the sync protocol.
/// These are sent as direct peer-to-peer request/response (not gossip).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum SyncRequest {
    /// Request blocks within a height range (inclusive).
    GetBlocks {
        /// Starting block height.
        from_height: u64,
        /// Ending block height (inclusive).
        to_height: u64,
    },
    /// Request this peer's current chain tip (height + hash).
    GetChainTip,
    /// Request block headers only (lighter than full blocks).
    GetHeaders {
        from_height: u64,
        to_height: u64,
    },
}

/// Response to a sync request.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum SyncResponse {
    /// Response to GetBlocks.
    Blocks(Vec<Block>),
    /// Response to GetChainTip.
    ChainTip {
        height: u64,
        hash: BlockHash,
    },
    /// Response to GetHeaders (block height + hash pairs).
    Headers(Vec<(u64, BlockHash)>),
    /// Error response.
    Error(String),
}

impl SyncRequest {
    /// Serialize to bytes.
    pub fn to_bytes(&self) -> Result<Vec<u8>, NetworkError> {
        bincode::serialize(self).map_err(NetworkError::from)
    }

    /// Deserialize from bytes.
    pub fn from_bytes(data: &[u8]) -> Result<Self, NetworkError> {
        bincode::deserialize(data).map_err(NetworkError::from)
    }
}

impl SyncResponse {
    /// Serialize to bytes.
    pub fn to_bytes(&self) -> Result<Vec<u8>, NetworkError> {
        bincode::serialize(self).map_err(NetworkError::from)
    }

    /// Deserialize from bytes.
    pub fn from_bytes(data: &[u8]) -> Result<Self, NetworkError> {
        bincode::deserialize(data).map_err(NetworkError::from)
    }
}

// ============================================================================
// Peer Chain State Tracking
// ============================================================================

/// Tracks the reported chain tip of each peer.
#[derive(Debug, Clone)]
pub struct PeerChainState {
    /// The peer's reported block height.
    pub height: u64,
    /// Hash of the peer's tip block.
    pub tip_hash: BlockHash,
    /// When we last received this info.
    pub last_updated: DateTime<Utc>,
}

// ============================================================================
// Sync Manager
// ============================================================================

/// Maximum number of blocks to request in a single sync batch.
/// WHY: Each block is up to 256KB. 50 blocks = ~12.5MB max, which is
/// a reasonable download chunk on mobile networks without hogging bandwidth.
const MAX_BLOCKS_PER_REQUEST: u64 = 50;

/// Minimum number of peer chain tips needed before determining sync state.
/// WHY: During testnet with only 2-3 phones, requiring 3 peers would prevent
/// sync from ever starting. Set to 1 for Phase 2. In production (Phase 3),
/// this should be raised to 3+ to resist a single dishonest peer reporting
/// a false height. At that point, the network will have enough nodes.
const MIN_PEERS_FOR_SYNC_DECISION: usize = 1;

/// How often to check peer chain tips (seconds).
/// WHY: 30 seconds — frequent enough to detect new blocks quickly,
/// infrequent enough to avoid spamming peers on mobile connections.
/// Used by the sync event loop to periodically request chain tips from peers.
pub const CHAIN_TIP_POLL_INTERVAL_SECS: u64 = 30;

/// How long to wait for a sync response before considering the request timed out (seconds).
/// WHY: Mobile connections can be slow, but waiting more than 30 seconds for 50 blocks
/// (~12.5 MB) means the peer is likely unresponsive. Retrying with another peer is better
/// than waiting indefinitely on a dead connection.
const SYNC_REQUEST_TIMEOUT_SECS: i64 = 30;

/// How long before a peer's chain state report is considered stale (seconds).
/// WHY: On a 4-second block time, 5 minutes of silence means the peer has missed
/// ~75 blocks. They're either offline or partitioned. Evicting stale peers prevents
/// the sync manager from making decisions based on outdated information.
const PEER_STALE_TIMEOUT_SECS: i64 = 300;

/// Maximum number of concurrent sync requests to different peers.
/// WHY: Requesting blocks from multiple peers simultaneously speeds up initial sync
/// (each peer serves a different block range). But too many concurrent requests
/// wastes bandwidth on mobile. 3 is a pragmatic limit for testnet.
const MAX_CONCURRENT_SYNC_REQUESTS: usize = 3;

/// Manages state synchronization with network peers.
pub struct SyncManager {
    /// Current sync state.
    state: SyncState,
    /// Our local chain height.
    local_height: u64,
    /// Our local chain tip hash.
    local_tip: BlockHash,
    /// Chain state reported by each peer.
    peer_states: HashMap<PeerId, PeerChainState>,
    /// Blocks that have been requested but not yet received.
    /// Value is (from_height, to_height, request_timestamp).
    pending_requests: HashMap<PeerId, (u64, u64, DateTime<Utc>)>,
    /// Count of consecutive failed sync attempts (for backoff).
    /// WHY: If all peers are returning errors, exponential backoff prevents
    /// hammering them on every tick. Resets to 0 on any successful sync.
    consecutive_failures: u32,
}

impl SyncManager {
    /// Create a new SyncManager with the given local chain state.
    pub fn new(local_height: u64, local_tip: BlockHash) -> Self {
        SyncManager {
            state: SyncState::Unknown,
            local_height,
            local_tip,
            peer_states: HashMap::new(),
            pending_requests: HashMap::new(),
            consecutive_failures: 0,
        }
    }

    /// Get the current sync state.
    pub fn state(&self) -> SyncState {
        self.state
    }

    /// Get the local chain height.
    pub fn local_height(&self) -> u64 {
        self.local_height
    }

    /// Update our local chain state (e.g., after processing a new block).
    pub fn update_local_state(&mut self, height: u64, tip: BlockHash) {
        self.local_height = height;
        self.local_tip = tip;
        self.reevaluate_sync_state();
    }

    /// Record a peer's reported chain tip.
    pub fn update_peer_state(&mut self, peer: PeerId, height: u64, tip_hash: BlockHash) {
        self.peer_states.insert(
            peer,
            PeerChainState {
                height,
                tip_hash,
                last_updated: Utc::now(),
            },
        );
        self.reevaluate_sync_state();
    }

    /// Remove a peer's chain state (e.g., on disconnect).
    pub fn remove_peer(&mut self, peer: &PeerId) {
        self.peer_states.remove(peer);
        self.pending_requests.remove(peer);
        self.reevaluate_sync_state();
    }

    /// Evict peers whose chain state reports are older than PEER_STALE_TIMEOUT_SECS.
    ///
    /// WHY: On mobile networks, peers go offline without cleanly disconnecting.
    /// Stale peer data leads to incorrect sync decisions (e.g., thinking we're
    /// behind based on a height reported 10 minutes ago by a now-offline peer).
    /// Returns the number of peers evicted.
    pub fn evict_stale_peers(&mut self) -> usize {
        let now = Utc::now();
        let stale_cutoff = chrono::Duration::seconds(PEER_STALE_TIMEOUT_SECS);
        let stale_peers: Vec<PeerId> = self
            .peer_states
            .iter()
            .filter(|(_, state)| now - state.last_updated > stale_cutoff)
            .map(|(id, _)| *id)
            .collect();

        let count = stale_peers.len();
        for peer in &stale_peers {
            self.peer_states.remove(peer);
            self.pending_requests.remove(peer);
        }

        if count > 0 {
            self.reevaluate_sync_state();
        }
        count
    }

    /// Check for timed-out sync requests and cancel them.
    ///
    /// WHY: A peer that accepted a sync request but never responded holds the
    /// block range hostage — no other peer will be asked for those blocks.
    /// After SYNC_REQUEST_TIMEOUT_SECS, we cancel the request so
    /// next_sync_request() can retry with a different peer.
    /// Returns the list of peers whose requests timed out.
    pub fn cancel_timed_out_requests(&mut self) -> Vec<PeerId> {
        let now = Utc::now();
        let timeout = chrono::Duration::seconds(SYNC_REQUEST_TIMEOUT_SECS);
        let timed_out: Vec<PeerId> = self
            .pending_requests
            .iter()
            .filter(|(_, (_, _, requested_at))| now - *requested_at > timeout)
            .map(|(peer, _)| *peer)
            .collect();

        for peer in &timed_out {
            self.pending_requests.remove(peer);
        }

        if !timed_out.is_empty() {
            self.consecutive_failures += 1;
            self.reevaluate_sync_state();
        }
        timed_out
    }

    /// Determine the best known network height from peer reports.
    /// Uses the median of reported heights to resist outlier manipulation.
    pub fn best_network_height(&self) -> Option<u64> {
        if self.peer_states.len() < MIN_PEERS_FOR_SYNC_DECISION {
            return None;
        }

        let mut heights: Vec<u64> = self.peer_states.values().map(|s| s.height).collect();
        heights.sort_unstable();

        // WHY: Median instead of max. If one peer reports a bogus height of 999999,
        // the median ignores it. More resistant to dishonest peers.
        Some(heights[heights.len() / 2])
    }

    /// Re-evaluate the sync state based on local height and peer reports.
    fn reevaluate_sync_state(&mut self) {
        let network_height = match self.best_network_height() {
            Some(h) => h,
            None => {
                self.state = SyncState::Unknown;
                return;
            }
        };

        if self.local_height >= network_height {
            self.state = SyncState::Synced;
        } else if self.pending_requests.is_empty() {
            self.state = SyncState::Behind {
                local_height: self.local_height,
                network_height,
            };
        } else {
            self.state = SyncState::Syncing {
                local_height: self.local_height,
                target_height: network_height,
            };
        }
    }

    /// Generate the next sync request to send to a peer.
    /// Returns None if already synced, at max concurrent requests, or no suitable peer available.
    ///
    /// WHY: Supports parallel sync requests to different peers for different block ranges.
    /// Each request covers a non-overlapping range, so blocks arrive out of order but
    /// can be applied sequentially. This is critical for initial sync when a new phone
    /// joins a testnet with thousands of blocks.
    pub fn next_sync_request(&mut self) -> Option<(PeerId, SyncRequest)> {
        // Respect concurrent request limit
        if self.pending_requests.len() >= MAX_CONCURRENT_SYNC_REQUESTS {
            return None;
        }

        let network_height = self.best_network_height()?;

        if self.local_height >= network_height {
            return None;
        }

        // Find the highest block height we've already requested (pending or local)
        let highest_requested = self
            .pending_requests
            .values()
            .map(|(_, to, _)| *to)
            .max()
            .unwrap_or(self.local_height);

        let from = highest_requested + 1;
        if from > network_height {
            return None;
        }
        let to = (from + MAX_BLOCKS_PER_REQUEST - 1).min(network_height);

        // Find a peer that has blocks we need and that we haven't already
        // sent a pending request to
        let peer = self
            .peer_states
            .iter()
            .filter(|(peer_id, state)| {
                state.height >= to && !self.pending_requests.contains_key(peer_id)
            })
            .map(|(peer_id, _)| *peer_id)
            .next()?;

        self.pending_requests.insert(peer, (from, to, Utc::now()));
        self.state = SyncState::Syncing {
            local_height: self.local_height,
            target_height: network_height,
        };

        Some((peer, SyncRequest::GetBlocks {
            from_height: from,
            to_height: to,
        }))
    }

    /// Handle a completed sync request — peer responded with blocks.
    /// Returns the blocks for the caller to validate and apply.
    pub fn handle_blocks_response(
        &mut self,
        peer: &PeerId,
        blocks: Vec<Block>,
    ) -> Result<Vec<Block>, NetworkError> {
        let expected = self.pending_requests.remove(peer);

        if let Some((from, _to, _requested_at)) = expected {
            // Basic sanity check: are the blocks in the expected range?
            if let Some(first) = blocks.first() {
                if first.header.height != from {
                    self.consecutive_failures += 1;
                    return Err(NetworkError::SyncError(format!(
                        "Expected blocks starting at height {}, got {}",
                        from, first.header.height
                    )));
                }
            }

            // Verify blocks are contiguous
            for window in blocks.windows(2) {
                if window[1].header.height != window[0].header.height + 1 {
                    self.consecutive_failures += 1;
                    return Err(NetworkError::SyncError(
                        "Received non-contiguous blocks".to_string(),
                    ));
                }
            }
        }

        // Successful response resets failure counter
        self.consecutive_failures = 0;
        Ok(blocks)
    }

    /// Handle a sync request FROM another peer.
    /// The caller provides a function to fetch blocks from local storage.
    pub fn handle_sync_request<F>(
        &self,
        request: &SyncRequest,
        get_blocks: F,
    ) -> SyncResponse
    where
        F: Fn(u64, u64) -> Option<Vec<Block>>,
    {
        match request {
            SyncRequest::GetChainTip => SyncResponse::ChainTip {
                height: self.local_height,
                hash: self.local_tip,
            },
            SyncRequest::GetBlocks {
                from_height,
                to_height,
            } => {
                // Clamp the range to prevent abuse
                let clamped_to = (*to_height).min(*from_height + MAX_BLOCKS_PER_REQUEST - 1);

                if *from_height > self.local_height {
                    return SyncResponse::Error(format!(
                        "Requested height {} is above our chain tip {}",
                        from_height, self.local_height
                    ));
                }

                match get_blocks(*from_height, clamped_to) {
                    Some(blocks) => SyncResponse::Blocks(blocks),
                    None => SyncResponse::Error("Failed to retrieve blocks".to_string()),
                }
            }
            SyncRequest::GetHeaders {
                from_height,
                to_height,
            } => {
                // WHY: Header-only requests are much lighter than full blocks.
                // Useful for initial sync to find the fork point before
                // downloading full blocks.
                let clamped_to = (*to_height).min(*from_height + MAX_BLOCKS_PER_REQUEST - 1);

                match get_blocks(*from_height, clamped_to) {
                    Some(blocks) => {
                        let mut headers = Vec::with_capacity(blocks.len());
                        for b in blocks {
                            match b.header.hash() {
                                Ok(hash) => headers.push((b.header.height, hash)),
                                Err(e) => return SyncResponse::Error(
                                    format!("Failed to hash header at height {}: {}", b.header.height, e),
                                ),
                            }
                        }
                        SyncResponse::Headers(headers)
                    }
                    None => SyncResponse::Error("Failed to retrieve headers".to_string()),
                }
            }
        }
    }

    /// Number of tracked peers.
    pub fn tracked_peer_count(&self) -> usize {
        self.peer_states.len()
    }

    /// Number of pending sync requests.
    pub fn pending_request_count(&self) -> usize {
        self.pending_requests.len()
    }

    /// Number of consecutive sync failures (for backoff decisions).
    pub fn consecutive_failures(&self) -> u32 {
        self.consecutive_failures
    }

    /// Perform periodic maintenance: evict stale peers, cancel timed-out requests.
    ///
    /// WHY: Called from the slot timer on every chain tip poll interval.
    /// Consolidates all housekeeping into one call so the FFI doesn't need
    /// to remember to call multiple methods.
    /// Returns a summary of what happened.
    pub fn tick_maintenance(&mut self) -> SyncMaintenanceSummary {
        let stale_evicted = self.evict_stale_peers();
        let timed_out = self.cancel_timed_out_requests();
        SyncMaintenanceSummary {
            stale_peers_evicted: stale_evicted,
            timed_out_requests: timed_out.len(),
        }
    }
}

/// Summary of periodic sync maintenance operations.
#[derive(Debug, Clone)]
pub struct SyncMaintenanceSummary {
    /// Number of stale peers evicted.
    pub stale_peers_evicted: usize,
    /// Number of timed-out sync requests cancelled.
    pub timed_out_requests: usize,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sync_state_progress() {
        assert_eq!(SyncState::Synced.progress(), 1.0);
        assert_eq!(SyncState::Unknown.progress(), 1.0);

        let syncing = SyncState::Syncing {
            local_height: 50,
            target_height: 100,
        };
        assert!((syncing.progress() - 0.5).abs() < f64::EPSILON);

        let behind = SyncState::Behind {
            local_height: 75,
            network_height: 100,
        };
        assert!((behind.progress() - 0.75).abs() < f64::EPSILON);
    }

    #[test]
    fn test_sync_manager_initial_state() {
        let sm = SyncManager::new(0, BlockHash([0u8; 32]));
        assert_eq!(sm.state(), SyncState::Unknown);
        assert_eq!(sm.local_height(), 0);
    }

    #[test]
    fn test_sync_manager_needs_min_peers() {
        let mut sm = SyncManager::new(0, BlockHash([0u8; 32]));

        // With MIN_PEERS_FOR_SYNC_DECISION = 1, a single peer report is enough
        // to determine sync state. This enables 2-phone testnet sync.
        sm.update_peer_state(PeerId::random(), 100, BlockHash([1u8; 32]));
        assert!(matches!(sm.state(), SyncState::Behind { .. }));
    }

    #[test]
    fn test_sync_manager_synced() {
        let mut sm = SyncManager::new(100, BlockHash([1u8; 32]));

        // Single peer at same height should show as synced
        sm.update_peer_state(PeerId::random(), 100, BlockHash([1u8; 32]));
        assert!(sm.state().is_synced());
    }

    #[test]
    fn test_best_network_height_uses_median() {
        let mut sm = SyncManager::new(0, BlockHash([0u8; 32]));

        sm.update_peer_state(PeerId::random(), 100, BlockHash([1u8; 32]));
        sm.update_peer_state(PeerId::random(), 102, BlockHash([2u8; 32]));
        sm.update_peer_state(PeerId::random(), 999999, BlockHash([3u8; 32])); // outlier

        // Median of [100, 102, 999999] = 102
        assert_eq!(sm.best_network_height(), Some(102));
    }

    #[test]
    fn test_next_sync_request() {
        let mut sm = SyncManager::new(0, BlockHash([0u8; 32]));
        let peer = PeerId::random();

        sm.update_peer_state(peer, 100, BlockHash([1u8; 32]));
        sm.update_peer_state(PeerId::random(), 100, BlockHash([1u8; 32]));
        sm.update_peer_state(PeerId::random(), 100, BlockHash([1u8; 32]));

        let (_selected_peer, request) = sm.next_sync_request().unwrap();
        match request {
            SyncRequest::GetBlocks {
                from_height,
                to_height,
            } => {
                assert_eq!(from_height, 1);
                assert_eq!(to_height, MAX_BLOCKS_PER_REQUEST);
            }
            _ => panic!("Expected GetBlocks request"),
        }
    }

    #[test]
    fn test_handle_chain_tip_request() {
        let sm = SyncManager::new(42, BlockHash([7u8; 32]));
        let response = sm.handle_sync_request(&SyncRequest::GetChainTip, |_, _| None);

        match response {
            SyncResponse::ChainTip { height, hash } => {
                assert_eq!(height, 42);
                assert_eq!(hash, BlockHash([7u8; 32]));
            }
            _ => panic!("Expected ChainTip response"),
        }
    }

    #[test]
    fn test_sync_request_serialization() {
        let req = SyncRequest::GetBlocks {
            from_height: 10,
            to_height: 20,
        };
        let bytes = req.to_bytes().unwrap();
        let decoded = SyncRequest::from_bytes(&bytes).unwrap();

        match decoded {
            SyncRequest::GetBlocks {
                from_height,
                to_height,
            } => {
                assert_eq!(from_height, 10);
                assert_eq!(to_height, 20);
            }
            _ => panic!("Expected GetBlocks"),
        }
    }

    #[test]
    fn test_sync_response_serialization() {
        let resp = SyncResponse::ChainTip {
            height: 99,
            hash: BlockHash([0xAB; 32]),
        };
        let bytes = resp.to_bytes().unwrap();
        let decoded = SyncResponse::from_bytes(&bytes).unwrap();

        match decoded {
            SyncResponse::ChainTip { height, hash } => {
                assert_eq!(height, 99);
                assert_eq!(hash, BlockHash([0xAB; 32]));
            }
            _ => panic!("Expected ChainTip"),
        }
    }

    #[test]
    fn test_handle_blocks_contiguity_check() {
        let mut sm = SyncManager::new(0, BlockHash([0u8; 32]));
        let peer = PeerId::random();
        sm.pending_requests.insert(peer, (1, 3, Utc::now()));

        // Non-contiguous blocks should fail
        let blocks = vec![
            make_block_at_height(1),
            make_block_at_height(3), // gap!
        ];

        let result = sm.handle_blocks_response(&peer, blocks);
        assert!(result.is_err());
        assert_eq!(sm.consecutive_failures(), 1);
    }

    #[test]
    fn test_evict_stale_peers() {
        let mut sm = SyncManager::new(0, BlockHash([0u8; 32]));
        let fresh_peer = PeerId::random();
        let stale_peer = PeerId::random();

        // Fresh peer
        sm.update_peer_state(fresh_peer, 100, BlockHash([1u8; 32]));

        // Stale peer — manually backdate
        sm.peer_states.insert(
            stale_peer,
            PeerChainState {
                height: 50,
                tip_hash: BlockHash([2u8; 32]),
                last_updated: Utc::now() - chrono::Duration::seconds(PEER_STALE_TIMEOUT_SECS + 10),
            },
        );

        assert_eq!(sm.tracked_peer_count(), 2);
        let evicted = sm.evict_stale_peers();
        assert_eq!(evicted, 1);
        assert_eq!(sm.tracked_peer_count(), 1);
        assert!(sm.peer_states.contains_key(&fresh_peer));
        assert!(!sm.peer_states.contains_key(&stale_peer));
    }

    #[test]
    fn test_cancel_timed_out_requests() {
        let mut sm = SyncManager::new(0, BlockHash([0u8; 32]));
        let peer = PeerId::random();

        // Insert a request that's already timed out
        sm.pending_requests.insert(
            peer,
            (1, 50, Utc::now() - chrono::Duration::seconds(SYNC_REQUEST_TIMEOUT_SECS + 5)),
        );

        assert_eq!(sm.pending_request_count(), 1);
        let timed_out = sm.cancel_timed_out_requests();
        assert_eq!(timed_out.len(), 1);
        assert_eq!(sm.pending_request_count(), 0);
        assert_eq!(sm.consecutive_failures(), 1);
    }

    #[test]
    fn test_parallel_sync_requests() {
        let mut sm = SyncManager::new(0, BlockHash([0u8; 32]));

        // Add enough peers to support parallel requests
        let peers: Vec<PeerId> = (0..5).map(|_| PeerId::random()).collect();
        for peer in &peers {
            sm.update_peer_state(*peer, 200, BlockHash([1u8; 32]));
        }

        // Should be able to get multiple concurrent requests
        let req1 = sm.next_sync_request();
        assert!(req1.is_some());
        let req2 = sm.next_sync_request();
        assert!(req2.is_some());
        let req3 = sm.next_sync_request();
        assert!(req3.is_some());

        // Fourth should be blocked by MAX_CONCURRENT_SYNC_REQUESTS
        let req4 = sm.next_sync_request();
        assert!(req4.is_none());

        assert_eq!(sm.pending_request_count(), 3);
    }

    #[test]
    fn test_tick_maintenance() {
        let mut sm = SyncManager::new(0, BlockHash([0u8; 32]));
        let stale_peer = PeerId::random();

        sm.peer_states.insert(
            stale_peer,
            PeerChainState {
                height: 50,
                tip_hash: BlockHash([1u8; 32]),
                last_updated: Utc::now() - chrono::Duration::seconds(PEER_STALE_TIMEOUT_SECS + 10),
            },
        );

        let summary = sm.tick_maintenance();
        assert_eq!(summary.stale_peers_evicted, 1);
        assert_eq!(summary.timed_out_requests, 0);
    }

    #[test]
    fn test_successful_response_resets_failures() {
        let mut sm = SyncManager::new(0, BlockHash([0u8; 32]));
        let peer = PeerId::random();
        sm.consecutive_failures = 5;
        sm.pending_requests.insert(peer, (1, 1, Utc::now()));

        let blocks = vec![make_block_at_height(1)];
        sm.handle_blocks_response(&peer, blocks).unwrap();
        assert_eq!(sm.consecutive_failures(), 0);
    }

    fn make_block_at_height(height: u64) -> Block {
        Block {
            header: gratia_core::types::BlockHeader {
                height,
                timestamp: Utc::now(),
                parent_hash: BlockHash([0u8; 32]),
                transactions_root: [0u8; 32],
                state_root: [0u8; 32],
                attestations_root: [0u8; 32],
                producer: gratia_core::types::NodeId([0u8; 32]),
                vrf_proof: vec![],
                active_miners: 0,
                geographic_diversity: 0,
            },
            transactions: vec![],
            attestations: vec![],
            validator_signatures: vec![],
        }
    }
}
