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

use std::collections::hash_map::DefaultHasher;
use std::collections::HashSet;
use std::hash::{Hash, Hasher};
use std::time::Duration;

use libp2p::futures::StreamExt;
use libp2p::swarm::NetworkBehaviour;
use libp2p::{
    gossipsub, identify, mdns, swarm::SwarmEvent, Multiaddr, PeerId, Swarm, SwarmBuilder,
};
use tokio::sync::mpsc;

use gratia_core::types::{Block, BlockHash, NodeId, ProofOfLifeAttestation, Transaction};

use crate::discovery::PeerDiscovery;
use crate::error::NetworkError;
use crate::gossip::{GossipHandler, GossipMessage, ALL_TOPICS};
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
// Composed libp2p Behaviour
// ============================================================================

/// The composed libp2p NetworkBehaviour for a Gratia node.
///
/// WHY: libp2p's derive macro auto-generates the NetworkBehaviour implementation,
/// producing a combined `GratiaBehaviourEvent` enum that wraps events from
/// each sub-behaviour.
#[derive(NetworkBehaviour)]
struct GratiaBehaviour {
    /// Gossipsub for block/transaction/attestation propagation.
    gossipsub: gossipsub::Behaviour,
    /// Identify protocol — exchanges peer metadata on connect.
    identify: identify::Behaviour,
    /// mDNS for local network peer discovery (same Wi-Fi).
    /// WHY: Essential for the Phase 1 demo where phones need to find each other
    /// on the same local network without a bootstrap server.
    mdns: mdns::tokio::Behaviour,
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

    /// Gossip message handler (validation, deduplication) — for outbound prep.
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
    /// Builds the libp2p Swarm with QUIC transport, Gossipsub, Identify,
    /// and mDNS behaviours, then spawns a background tokio task to drive
    /// the swarm event loop.
    ///
    /// Returns a receiver for network events that the consensus layer should poll.
    pub async fn start(&mut self) -> Result<mpsc::Receiver<NetworkEvent>, NetworkError> {
        if self.running {
            return Err(NetworkError::AlreadyStarted);
        }

        self.config
            .transport
            .validate()
            .map_err(|e| NetworkError::Transport(e))?;

        // Channel for network events (event loop -> consensus layer)
        // WHY: Buffer of 256 — handles burst of blocks/txs without backpressure
        let (event_tx, event_rx) = mpsc::channel::<NetworkEvent>(256);

        // Channel for commands (application -> event loop)
        // WHY: Buffer of 128 — application sends few commands per second
        let (command_tx, command_rx) = mpsc::channel::<NetworkCommand>(128);

        self.command_tx = Some(command_tx);

        // Build the libp2p Swarm
        let mut swarm = SwarmBuilder::with_new_identity()
            .with_tokio()
            .with_quic()
            .with_behaviour(|key| {
                // Gossipsub configuration tuned for mobile
                let gossipsub_config = gossipsub::ConfigBuilder::default()
                    // WHY: Custom message ID function — dedup by content hash
                    // rather than source+seqno, so the same message from
                    // different propagation paths is correctly deduplicated.
                    .message_id_fn(|msg| {
                        let mut hasher = DefaultHasher::new();
                        msg.data.hash(&mut hasher);
                        gossipsub::MessageId::from(hasher.finish().to_be_bytes().to_vec())
                    })
                    // WHY: 30 second heartbeat — longer than default (1s) to reduce
                    // battery drain from gossip protocol overhead on mobile.
                    .heartbeat_interval(Duration::from_secs(30))
                    // WHY: Mesh target of 4 peers (instead of default 6) — reduces
                    // bandwidth on mobile while maintaining reasonable propagation.
                    .mesh_n(4)
                    .mesh_n_low(2)
                    .mesh_n_high(8)
                    // WHY: 300 KB max transmit size — matches our MAX_MESSAGE_SIZE
                    // (256 KB block + serialization overhead).
                    .max_transmit_size(300 * 1024)
                    .build()
                    .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;

                let gossipsub = gossipsub::Behaviour::new(
                    gossipsub::MessageAuthenticity::Signed(key.clone()),
                    gossipsub_config,
                )
                .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;

                let identify = identify::Behaviour::new(identify::Config::new(
                    "/gratia/0.1.0".to_string(),
                    key.public(),
                ));

                let mdns = mdns::tokio::Behaviour::new(
                    mdns::Config::default(),
                    key.public().to_peer_id(),
                )?;

                Ok(GratiaBehaviour {
                    gossipsub,
                    identify,
                    mdns,
                })
            })
            .map_err(|e| NetworkError::Transport(e.to_string()))?
            .with_swarm_config(|cfg| {
                cfg.with_idle_connection_timeout(Duration::from_secs(
                    self.config.transport.idle_timeout_secs,
                ))
            })
            .build();

        // Subscribe to all gossip topics
        for topic_str in ALL_TOPICS {
            let topic = gossipsub::IdentTopic::new(*topic_str);
            swarm
                .behaviour_mut()
                .gossipsub
                .subscribe(&topic)
                .map_err(|e| NetworkError::SubscriptionError {
                    topic: topic_str.to_string(),
                    reason: e.to_string(),
                })?;
        }

        // Listen on configured addresses
        for addr in self.config.transport.parsed_listen_addresses() {
            swarm
                .listen_on(addr.clone())
                .map_err(|e| NetworkError::ListenFailure(e.to_string()))?;
            tracing::info!(%addr, "Listening");
        }

        self.running = true;

        // Spawn the event loop as a background task
        let node_id = self.config.local_node_id;
        tokio::spawn(run_swarm_event_loop(swarm, command_rx, event_tx, node_id));

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

    /// Dial a remote peer by multiaddr string.
    ///
    /// Used for manual peer connection (e.g., entering another phone's address).
    pub async fn dial_peer(&self, addr: &str) -> Result<(), NetworkError> {
        let tx = self.command_tx.as_ref().ok_or(NetworkError::NotStarted)?;

        // Validate address parses before sending
        addr.parse::<Multiaddr>()
            .map_err(|e| NetworkError::DialFailure(format!("invalid address '{}': {}", addr, e)))?;

        tx.send(NetworkCommand::DialPeer(addr.to_string()))
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
// Swarm Event Loop
// ============================================================================

/// The background task that drives the libp2p swarm.
///
/// This function runs in a tokio task spawned by `NetworkManager::start()`.
/// It processes swarm events (incoming messages, connections) and application
/// commands (publish, dial, shutdown) in a select loop.
async fn run_swarm_event_loop(
    mut swarm: Swarm<GratiaBehaviour>,
    mut command_rx: mpsc::Receiver<NetworkCommand>,
    event_tx: mpsc::Sender<NetworkEvent>,
    node_id: NodeId,
) {
    // WHY: Separate gossip handler for the event loop — deduplication must
    // happen where messages first arrive (here), not in the application layer.
    let mut gossip_handler = GossipHandler::new();

    tracing::info!(%node_id, "Swarm event loop started");

    loop {
        tokio::select! {
            // ── Swarm events ──────────────────────────────────────────────
            event = swarm.select_next_some() => {
                match event {
                    // Gossipsub message received
                    SwarmEvent::Behaviour(GratiaBehaviourEvent::Gossipsub(
                        gossipsub::Event::Message { message, .. }
                    )) => {
                        let topic = message.topic.as_str();
                        match gossip_handler.process_incoming(topic, &message.data) {
                            Ok(Some(msg)) => {
                                let net_event = match msg {
                                    GossipMessage::NewBlock(block) => {
                                        tracing::debug!(
                                            height = block.header.height,
                                            "Received block via gossip"
                                        );
                                        NetworkEvent::BlockReceived(block)
                                    }
                                    GossipMessage::NewTransaction(tx) => {
                                        NetworkEvent::TransactionReceived(tx)
                                    }
                                    GossipMessage::NewAttestation(att) => {
                                        NetworkEvent::AttestationReceived(att)
                                    }
                                };
                                if event_tx.send(net_event).await.is_err() {
                                    tracing::warn!("Event receiver dropped, shutting down");
                                    return;
                                }
                            }
                            Ok(None) => {
                                // Duplicate message — silently ignore
                            }
                            Err(e) => {
                                tracing::debug!("Gossip message rejected: {}", e);
                            }
                        }
                    }

                    // Gossipsub subscription event
                    SwarmEvent::Behaviour(GratiaBehaviourEvent::Gossipsub(
                        gossipsub::Event::Subscribed { peer_id, topic }
                    )) => {
                        tracing::debug!(%peer_id, %topic, "Peer subscribed to topic");
                    }

                    // mDNS discovered peers on local network
                    SwarmEvent::Behaviour(GratiaBehaviourEvent::Mdns(
                        mdns::Event::Discovered(peers)
                    )) => {
                        for (peer_id, addr) in peers {
                            tracing::info!(%peer_id, %addr, "mDNS discovered peer");
                            swarm.behaviour_mut().gossipsub.add_explicit_peer(&peer_id);
                            if let Err(e) = swarm.dial(addr.clone()) {
                                tracing::debug!(%peer_id, %addr, "Failed to dial mDNS peer: {}", e);
                            }
                        }
                    }

                    // mDNS peer expired
                    SwarmEvent::Behaviour(GratiaBehaviourEvent::Mdns(
                        mdns::Event::Expired(peers)
                    )) => {
                        for (peer_id, _addr) in peers {
                            tracing::debug!(%peer_id, "mDNS peer expired");
                            swarm.behaviour_mut().gossipsub.remove_explicit_peer(&peer_id);
                        }
                    }

                    // Identify — peer identified itself
                    SwarmEvent::Behaviour(GratiaBehaviourEvent::Identify(
                        identify::Event::Received { peer_id, info, .. }
                    )) => {
                        tracing::debug!(
                            %peer_id,
                            protocol = %info.protocol_version,
                            agent = %info.agent_version,
                            "Peer identified"
                        );
                    }

                    // New connection established
                    SwarmEvent::ConnectionEstablished {
                        peer_id,
                        endpoint,
                        connection_id: _,
                        num_established: _,
                        concurrent_dial_errors: _,
                        established_in: _,
                    } => {
                        let is_inbound = endpoint.is_listener();
                        tracing::info!(
                            %peer_id,
                            direction = if is_inbound { "inbound" } else { "outbound" },
                            "Connection established"
                        );
                        let _ = event_tx.send(NetworkEvent::PeerConnected {
                            peer_id,
                            node_id: None,
                        }).await;
                    }

                    // Connection closed
                    SwarmEvent::ConnectionClosed {
                        peer_id,
                        cause,
                        connection_id: _,
                        endpoint: _,
                        num_established: _,
                    } => {
                        tracing::info!(
                            %peer_id,
                            cause = ?cause,
                            "Connection closed"
                        );
                        let _ = event_tx.send(NetworkEvent::PeerDisconnected {
                            peer_id,
                        }).await;
                    }

                    // New listen address
                    SwarmEvent::NewListenAddr { address, .. } => {
                        tracing::info!(%address, "New listen address");
                    }

                    // Other swarm events — log at debug level
                    other => {
                        tracing::trace!("Swarm event: {:?}", other);
                    }
                }
            }

            // ── Application commands ──────────────────────────────────────
            cmd = command_rx.recv() => {
                match cmd {
                    Some(NetworkCommand::PublishBlock(block)) => {
                        let msg = GossipMessage::NewBlock(block);
                        if let Ok(data) = msg.to_bytes() {
                            let topic = gossipsub::IdentTopic::new(gossip::TOPIC_BLOCKS);
                            if let Err(e) = swarm.behaviour_mut().gossipsub.publish(topic, data) {
                                tracing::error!("Failed to publish block: {}", e);
                            }
                        }
                    }
                    Some(NetworkCommand::PublishTransaction(tx)) => {
                        let msg = GossipMessage::NewTransaction(tx);
                        if let Ok(data) = msg.to_bytes() {
                            let topic = gossipsub::IdentTopic::new(gossip::TOPIC_TRANSACTIONS);
                            if let Err(e) = swarm.behaviour_mut().gossipsub.publish(topic, data) {
                                tracing::error!("Failed to publish transaction: {}", e);
                            }
                        }
                    }
                    Some(NetworkCommand::PublishAttestation(att)) => {
                        let msg = GossipMessage::NewAttestation(att);
                        if let Ok(data) = msg.to_bytes() {
                            let topic = gossipsub::IdentTopic::new(gossip::TOPIC_ATTESTATIONS);
                            if let Err(e) = swarm.behaviour_mut().gossipsub.publish(topic, data) {
                                tracing::error!("Failed to publish attestation: {}", e);
                            }
                        }
                    }
                    Some(NetworkCommand::DialPeer(addr_str)) => {
                        match addr_str.parse::<Multiaddr>() {
                            Ok(addr) => {
                                tracing::info!(%addr, "Dialing peer");
                                if let Err(e) = swarm.dial(addr) {
                                    tracing::error!("Failed to dial: {}", e);
                                }
                            }
                            Err(e) => {
                                tracing::error!("Invalid multiaddr '{}': {}", addr_str, e);
                            }
                        }
                    }
                    Some(NetworkCommand::SyncRequest { .. }) => {
                        // TODO: Implement sync request/response protocol (Phase 2)
                        tracing::debug!("Sync request received but not yet implemented");
                    }
                    Some(NetworkCommand::Shutdown) | None => {
                        tracing::info!("Swarm event loop shutting down");
                        return;
                    }
                }
            }
        }
    }
}

// ============================================================================
// Tests
// ============================================================================

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

    #[tokio::test]
    async fn test_start_and_stop() {
        let config = NetworkConfig::new(test_node_id());
        let mut nm = NetworkManager::new(config);

        let _event_rx = nm.start().await.unwrap();
        assert!(nm.is_running());

        nm.stop().await.unwrap();
        assert!(!nm.is_running());
    }

    #[tokio::test]
    async fn test_start_twice_fails() {
        let config = NetworkConfig::new(test_node_id());
        let mut nm = NetworkManager::new(config);

        let _event_rx = nm.start().await.unwrap();
        let result = nm.start().await;
        assert!(matches!(result, Err(NetworkError::AlreadyStarted)));

        nm.stop().await.unwrap();
    }

    #[tokio::test]
    async fn test_dial_peer_validates_address() {
        let config = NetworkConfig::new(test_node_id());
        let mut nm = NetworkManager::new(config);

        let _event_rx = nm.start().await.unwrap();

        // Invalid address should fail
        let result = nm.dial_peer("not-a-valid-addr").await;
        assert!(matches!(result, Err(NetworkError::DialFailure(_))));

        nm.stop().await.unwrap();
    }
}
