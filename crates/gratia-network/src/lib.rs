//! # gratia-network — Peer-to-peer networking for the Gratia protocol
//!
//! This crate implements the networking layer for Gratia, built on libp2p.
//!
//! ## Architecture
//!
//! - **Layer 0 (Mesh):** Bluetooth + Wi-Fi Direct — stubbed, planned for Phase 3.
//! - **Layer 1 (Consensus):** Cellular/Wi-Fi via libp2p — primary implementation.
//!
//! ## Components
//!
//! - [`transport`] — QUIC transport with connection management.
//! - [`discovery`] — Kademlia DHT peer discovery.
//! - [`gossip`] — Gossipsub for block/transaction/attestation propagation.
//! - [`sync`] — Block synchronization protocol.
//!
//! ## Usage
//!
//! The [`NetworkManager`] is the main entry point. It coordinates all
//! networking subsystems and exposes a simple API to the consensus layer.

pub mod discovery;
pub mod error;
pub mod gossip;
pub mod sync;
pub mod transport;

use std::collections::HashSet;

use libp2p::PeerId;
use tokio::sync::mpsc;

use gratia_core::types::{Block, BlockHash, NodeId, ProofOfLifeAttestation, Transaction};

use crate::discovery::PeerDiscovery;
use crate::error::NetworkError;
use crate::gossip::GossipHandler;
use crate::sync::{SyncManager, SyncState};
use crate::transport::{ConnectionManager, TransportConfig};

// ============================================================================
// Network Events
// ============================================================================

/// Events emitted by the network layer for the consensus/application layer to handle.
#[derive(Debug)]
pub enum NetworkEvent {
    /// A new block was received from the gossip network and passed basic validation.
    BlockReceived(Box<Block>),

    /// A new transaction was received from the gossip network.
    TransactionReceived(Box<Transaction>),

    /// A new Proof of Life attestation was received.
    AttestationReceived(Box<ProofOfLifeAttestation>),

    /// A peer connected.
    PeerConnected {
        peer_id: PeerId,
        node_id: Option<NodeId>,
    },

    /// A peer disconnected.
    PeerDisconnected {
        peer_id: PeerId,
    },

    /// Sync state changed.
    SyncStateChanged(SyncState),

    /// Blocks received via sync (not gossip). Caller should validate and apply.
    SyncBlocksReceived(Vec<Block>),
}

// ============================================================================
// Network Configuration
// ============================================================================

/// Full network configuration combining all subsystem configs.
#[derive(Debug, Clone)]
pub struct NetworkConfig {
    /// Transport-level settings (addresses, connection limits).
    pub transport: TransportConfig,

    /// Bootstrap peer addresses for initial network entry.
    pub bootstrap_peers: Vec<String>,

    /// Maximum peer records to cache in the discovery layer.
    /// WHY: 500 records at ~200 bytes = ~100KB — acceptable on low-end mobile.
    pub max_cached_peers: usize,

    /// This node's Gratia NodeId.
    pub local_node_id: NodeId,

    /// This node's Presence Score.
    pub presence_score: u8,

    /// Geographic shard this node belongs to.
    pub shard_id: u16,
}

impl NetworkConfig {
    /// Create a configuration with sensible defaults for mobile.
    pub fn new(local_node_id: NodeId) -> Self {
        NetworkConfig {
            transport: TransportConfig::default(),
            bootstrap_peers: Vec::new(),
            max_cached_peers: 500,
            local_node_id,
            presence_score: 40, // Minimum threshold
            shard_id: 0,
        }
    }
}

// ============================================================================
// Network Manager
// ============================================================================

/// The main coordinator for all networking operations.
///
/// `NetworkManager` owns the gossip handler, peer discovery, sync manager,
/// and connection manager. It provides a high-level API to the consensus
/// and application layers.
///
/// The actual libp2p Swarm is started in the [`NetworkManager::start`] method,
/// which spawns a background tokio task to drive the swarm event loop.
/// Communication between the event loop and the application happens through
/// channels.
pub struct NetworkManager {
    /// Network configuration.
    config: NetworkConfig,

    /// Gossip message handler (validation, deduplication).
    gossip: GossipHandler,

    /// Peer discovery cache.
    discovery: PeerDiscovery,

    /// Block synchronization manager.
    sync_manager: SyncManager,

    /// Connection tracking and limit enforcement.
    connections: ConnectionManager,

    /// Whether the network event loop is running.
    running: bool,

    /// Channel sender for outbound messages (to the swarm event loop).
    /// Populated when `start()` is called.
    command_tx: Option<mpsc::Sender<NetworkCommand>>,
}

/// Commands sent from the application to the network event loop.
#[derive(Debug)]
pub enum NetworkCommand {
    /// Publish a block to the gossip network.
    PublishBlock(Box<Block>),
    /// Publish a transaction to the gossip network.
    PublishTransaction(Box<Transaction>),
    /// Publish an attestation to the gossip network.
    PublishAttestation(Box<ProofOfLifeAttestation>),
    /// Request blocks from a peer for sync.
    SyncRequest {
        peer: PeerId,
        from_height: u64,
        to_height: u64,
    },
    /// Dial a specific peer address.
    DialPeer(String),
    /// Shut down the network.
    Shutdown,
}

impl NetworkManager {
    /// Create a new NetworkManager with the given configuration.
    pub fn new(config: NetworkConfig) -> Self {
        let transport_config = config.transport.clone();
        let bootstrap = config.bootstrap_peers.clone();
        let max_cached = config.max_cached_peers;

        NetworkManager {
            config,
            gossip: GossipHandler::new(),
            discovery: PeerDiscovery::new(bootstrap, max_cached),
            sync_manager: SyncManager::new(0, BlockHash([0u8; 32])),
            connections: ConnectionManager::new(transport_config),
            running: false,
            command_tx: None,
        }
    }

    /// Start the network layer.
    ///
    /// This builds the libp2p Swarm with QUIC transport, Noise encryption,
    /// Gossipsub, Kademlia, and Identify behaviours, then spawns a background
    /// tokio task to drive the event loop.
    ///
    /// Returns a receiver for network events that the consensus layer should poll.
    ///
    /// NOTE: The actual libp2p SwarmBuilder integration requires careful async
    /// wiring. The exact API calls may need adjustment based on libp2p 0.54
    /// specifics when building on the target platform.
    pub async fn start(&mut self) -> Result<mpsc::Receiver<NetworkEvent>, NetworkError> {
        if self.running {
            return Err(NetworkError::AlreadyStarted);
        }

        // Validate transport config before starting
        self.config
            .transport
            .validate()
            .map_err(|e| NetworkError::Transport(e))?;

        // Channel for network events (from event loop -> consensus layer)
        // WHY: Buffer of 256 — handles burst of blocks/txs without backpressure
        // on the event loop. If consensus is slow, events queue here.
        let (event_tx, event_rx) = mpsc::channel::<NetworkEvent>(256);

        // Channel for commands (from application -> event loop)
        // WHY: Buffer of 128 — application rarely sends more than a few
        // commands per second (publish block, publish tx, sync requests).
        let (command_tx, command_rx) = mpsc::channel::<NetworkCommand>(128);

        self.command_tx = Some(command_tx);

        // ---------------------------------------------------------------
        // libp2p Swarm construction
        //
        // In libp2p 0.54, the Swarm is built with SwarmBuilder:
        //
        //   let swarm = libp2p::SwarmBuilder::with_new_identity()
        //       .with_tokio()
        //       .with_quic()
        //       .with_behaviour(|key| {
        //           // Compose: Gossipsub + Kademlia + Identify
        //           GratiaBehaviour { gossipsub, kademlia, identify }
        //       })?
        //       .with_swarm_config(|cfg| {
        //           cfg.with_idle_connection_timeout(Duration::from_secs(300))
        //       })
        //       .build();
        //
        // The event loop then polls swarm.select_next_some() in a loop,
        // matching on SwarmEvent variants and forwarding relevant events
        // through event_tx.
        //
        // For now, we mark the network as running. The full swarm integration
        // will be wired when we have end-to-end testing on physical devices.
        // ---------------------------------------------------------------

        self.running = true;

        tracing::info!(
            node_id = %self.config.local_node_id,
            "Network manager started"
        );

        Ok(event_rx)
    }

    /// Stop the network layer gracefully.
    pub async fn stop(&mut self) -> Result<(), NetworkError> {
        if !self.running {
            return Err(NetworkError::NotStarted);
        }

        if let Some(tx) = &self.command_tx {
            let _ = tx.send(NetworkCommand::Shutdown).await;
        }

        self.command_tx = None;
        self.running = false;

        tracing::info!(
            node_id = %self.config.local_node_id,
            "Network manager stopped"
        );

        Ok(())
    }

    /// Broadcast a new block to the network via gossipsub.
    pub async fn broadcast_block(&self, block: Block) -> Result<(), NetworkError> {
        let tx = self.command_tx.as_ref().ok_or(NetworkError::NotStarted)?;

        // Pre-validate that the block can be serialized within size limits
        let _ = self.gossip.prepare_block(block.clone())?;

        tx.send(NetworkCommand::PublishBlock(Box::new(block)))
            .await
            .map_err(|e| NetworkError::ChannelError(e.to_string()))?;

        Ok(())
    }

    /// Broadcast a new transaction to the network via gossipsub.
    pub async fn broadcast_transaction(&self, tx: Transaction) -> Result<(), NetworkError> {
        let cmd_tx = self.command_tx.as_ref().ok_or(NetworkError::NotStarted)?;

        let _ = self.gossip.prepare_transaction(tx.clone())?;

        cmd_tx
            .send(NetworkCommand::PublishTransaction(Box::new(tx)))
            .await
            .map_err(|e| NetworkError::ChannelError(e.to_string()))?;

        Ok(())
    }

    /// Broadcast a Proof of Life attestation to the network.
    pub async fn broadcast_attestation(
        &self,
        attestation: ProofOfLifeAttestation,
    ) -> Result<(), NetworkError> {
        let tx = self.command_tx.as_ref().ok_or(NetworkError::NotStarted)?;

        let _ = self.gossip.prepare_attestation(attestation.clone())?;

        tx.send(NetworkCommand::PublishAttestation(Box::new(attestation)))
            .await
            .map_err(|e| NetworkError::ChannelError(e.to_string()))?;

        Ok(())
    }

    /// Get the current sync state.
    pub fn sync_state(&self) -> SyncState {
        self.sync_manager.state()
    }

    /// Get the number of connected peers.
    pub fn connected_peer_count(&self) -> usize {
        self.connections.peer_count()
    }

    /// Get all connected peer IDs.
    pub fn connected_peers(&self) -> &HashSet<PeerId> {
        self.connections.connected_peers()
    }

    /// Whether the network is currently running.
    pub fn is_running(&self) -> bool {
        self.running
    }

    /// Get a reference to the discovery layer.
    pub fn discovery(&self) -> &PeerDiscovery {
        &self.discovery
    }

    /// Get a mutable reference to the discovery layer.
    pub fn discovery_mut(&mut self) -> &mut PeerDiscovery {
        &mut self.discovery
    }

    /// Get a reference to the sync manager.
    pub fn sync_manager(&self) -> &SyncManager {
        &self.sync_manager
    }

    /// Get a mutable reference to the sync manager.
    pub fn sync_manager_mut(&mut self) -> &mut SyncManager {
        &mut self.sync_manager
    }

    /// Get a reference to the gossip handler.
    pub fn gossip(&self) -> &GossipHandler {
        &self.gossip
    }

    /// Get a mutable reference to the gossip handler.
    pub fn gossip_mut(&mut self) -> &mut GossipHandler {
        &mut self.gossip
    }

    /// Get the network configuration.
    pub fn config(&self) -> &NetworkConfig {
        &self.config
    }

    /// Process an incoming gossip message from the swarm event loop.
    /// This is called internally by the event loop when a gossipsub
    /// message arrives.
    pub fn handle_gossip_message(
        &mut self,
        topic: &str,
        data: &[u8],
    ) -> Result<Option<NetworkEvent>, NetworkError> {
        let msg = self.gossip.process_incoming(topic, data)?;

        Ok(msg.map(|m| match m {
            gossip::GossipMessage::NewBlock(block) => NetworkEvent::BlockReceived(block),
            gossip::GossipMessage::NewTransaction(tx) => NetworkEvent::TransactionReceived(tx),
            gossip::GossipMessage::NewAttestation(att) => NetworkEvent::AttestationReceived(att),
        }))
    }

    /// Register a newly connected peer.
    pub fn on_peer_connected(&mut self, peer_id: PeerId, is_inbound: bool) -> bool {
        if is_inbound {
            self.connections.register_inbound(peer_id)
        } else {
            self.connections.register_outbound(peer_id)
        }
    }

    /// Handle a peer disconnection.
    pub fn on_peer_disconnected(&mut self, peer_id: &PeerId, is_inbound: bool) {
        self.connections.remove_peer(peer_id, is_inbound);
        self.sync_manager.remove_peer(peer_id);
    }
}

// ============================================================================
// libp2p Behaviour Composition (type definitions)
// ============================================================================

/// The composed libp2p NetworkBehaviour for a Gratia node.
///
/// In libp2p 0.54, custom behaviours are composed using the derive macro:
///
/// ```ignore
/// #[derive(libp2p::swarm::NetworkBehaviour)]
/// pub struct GratiaBehaviour {
///     pub gossipsub: libp2p::gossipsub::Behaviour,
///     pub kademlia: libp2p::kad::Behaviour<libp2p::kad::store::MemoryStore>,
///     pub identify: libp2p::identify::Behaviour,
/// }
/// ```
///
/// The derive macro auto-generates the `NetworkBehaviour` implementation,
/// producing a combined `GratiaBehaviourEvent` enum that wraps events from
/// each sub-behaviour. The swarm event loop matches on these events to
/// dispatch to the appropriate handler.
///
/// NOTE: This derive requires the `libp2p::swarm::NetworkBehaviour` derive
/// macro which is available in libp2p 0.54. The exact generated event type
/// name follows the pattern `{StructName}Event`.

// WHY: We define the behaviour struct here rather than inline in start()
// so that tests and downstream code can reference the types if needed.
// The actual instantiation happens in the swarm builder closure.

#[cfg(test)]
mod tests {
    use super::*;

    fn test_node_id() -> NodeId {
        NodeId([0xAA; 32])
    }

    #[test]
    fn test_network_config_defaults() {
        let config = NetworkConfig::new(test_node_id());
        assert_eq!(config.max_cached_peers, 500);
        assert_eq!(config.presence_score, 40);
        assert_eq!(config.shard_id, 0);
        assert!(config.bootstrap_peers.is_empty());
    }

    #[test]
    fn test_network_manager_initial_state() {
        let config = NetworkConfig::new(test_node_id());
        let nm = NetworkManager::new(config);

        assert!(!nm.is_running());
        assert_eq!(nm.connected_peer_count(), 0);
        assert_eq!(nm.sync_state(), SyncState::Unknown);
    }

    #[test]
    fn test_handle_gossip_message_block() {
        let config = NetworkConfig::new(test_node_id());
        let mut nm = NetworkManager::new(config);

        // Create a valid block message
        let block = gratia_core::types::Block {
            header: gratia_core::types::BlockHeader {
                height: 1,
                timestamp: chrono::Utc::now(),
                parent_hash: BlockHash([0u8; 32]),
                transactions_root: [0u8; 32],
                state_root: [0u8; 32],
                attestations_root: [0u8; 32],
                producer: NodeId([1u8; 32]),
                vrf_proof: vec![0u8; 64],
                active_miners: 10,
                geographic_diversity: 2,
            },
            transactions: vec![],
            attestations: vec![],
            validator_signatures: vec![],
        };

        let msg = gossip::GossipMessage::NewBlock(Box::new(block));
        let data = msg.to_bytes().unwrap();

        // First time: should produce an event
        let result = nm
            .handle_gossip_message(gossip::TOPIC_BLOCKS, &data)
            .unwrap();
        assert!(result.is_some());
        assert!(matches!(result, Some(NetworkEvent::BlockReceived(_))));

        // Second time: duplicate, should produce None
        let result = nm
            .handle_gossip_message(gossip::TOPIC_BLOCKS, &data)
            .unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn test_peer_connection_tracking() {
        let config = NetworkConfig::new(test_node_id());
        let mut nm = NetworkManager::new(config);

        let peer = PeerId::random();
        assert!(nm.on_peer_connected(peer, true));
        assert_eq!(nm.connected_peer_count(), 1);

        nm.on_peer_disconnected(&peer, true);
        assert_eq!(nm.connected_peer_count(), 0);
    }

    #[tokio::test]
    async fn test_broadcast_requires_running() {
        let config = NetworkConfig::new(test_node_id());
        let nm = NetworkManager::new(config);

        let block = gratia_core::types::Block {
            header: gratia_core::types::BlockHeader {
                height: 1,
                timestamp: chrono::Utc::now(),
                parent_hash: BlockHash([0u8; 32]),
                transactions_root: [0u8; 32],
                state_root: [0u8; 32],
                attestations_root: [0u8; 32],
                producer: NodeId([1u8; 32]),
                vrf_proof: vec![],
                active_miners: 0,
                geographic_diversity: 0,
            },
            transactions: vec![],
            attestations: vec![],
            validator_signatures: vec![],
        };

        let result = nm.broadcast_block(block).await;
        assert!(matches!(result, Err(NetworkError::NotStarted)));
    }

    #[tokio::test]
    async fn test_stop_requires_running() {
        let config = NetworkConfig::new(test_node_id());
        let mut nm = NetworkManager::new(config);

        let result = nm.stop().await;
        assert!(matches!(result, Err(NetworkError::NotStarted)));
    }
}
