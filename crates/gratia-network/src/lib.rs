//! # gratia-network — Peer-to-peer networking for the Gratia protocol
//!
//! This crate implements the networking layer for Gratia, built on libp2p.
//!
//! ## Architecture
//!
//! - **Layer 0 (Mesh):** Bluetooth + Wi-Fi Direct via [`mesh::MeshTransport`].
//! - **Layer 1 (Consensus):** Cellular/Wi-Fi via libp2p — primary implementation.
//!
//! ## Components
//!
//! - [`transport`] — QUIC transport with connection management.
//! - [`mesh`] — Bluetooth/Wi-Fi Direct mesh transport (Layer 0).
//! - [`discovery`] — Kademlia DHT peer discovery.
//! - [`gossip`] — Gossipsub for block/transaction/attestation propagation.
//! - [`sync`] — Block synchronization protocol.
//! - [`reputation`] — Peer reputation tracking and rate limiting.
//!
//! ## Usage
//!
//! The [`NetworkManager`] is the main entry point. It coordinates all
//! networking subsystems and exposes a simple API to the consensus layer.

pub mod discovery;
pub mod error;
pub mod gossip;
pub mod mesh;
pub mod reputation;
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
use crate::gossip::{GossipHandler, GossipMessage, NodeAnnouncement, ValidatorSignatureMessage, ALL_TOPICS};
use crate::mesh::MeshTransport;
use crate::sync::{SyncManager, SyncPayload, SyncProtocolMessage, SyncRequest, SyncState, CHAIN_TIP_POLL_INTERVAL_SECS};
use crate::transport::{ConnectionManager, TransportConfig};

// ============================================================================
// Block Provider Trait
// ============================================================================

/// Trait for providing blocks to the sync protocol.
///
/// WHY: The network event loop doesn't own the state database.
/// The FFI/application layer wraps StateManager in this trait so
/// the sync handler can fetch blocks by height range when peers
/// request them.
pub trait BlockProvider: Send + Sync + 'static {
    /// Get blocks in the given height range (inclusive).
    /// Returns blocks that exist in the range, stopping at the first gap.
    fn get_blocks(&self, from_height: u64, to_height: u64) -> Vec<Block>;
}

/// No-op block provider used when state is not yet initialized.
pub struct NoBlockProvider;

impl BlockProvider for NoBlockProvider {
    fn get_blocks(&self, _from: u64, _to: u64) -> Vec<Block> {
        Vec::new()
    }
}

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

    /// A peer announced their node info for committee selection.
    NodeAnnounced(Box<NodeAnnouncement>),

    /// A Lux social post was received from the gossip network.
    LuxPostReceived(Box<gratia_lux::LuxPost>),

    /// A validator signature for a pending block was received.
    /// WHY: Committee members sign blocks they validate. When enough signatures
    /// accumulate (meeting the finality threshold), the block is finalized.
    ValidatorSignatureReceived(Box<ValidatorSignatureMessage>),
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

    /// Mesh transport layer (Layer 0: BLE + Wi-Fi Direct).
    /// WHY: Optional because mesh is only available on mobile devices with
    /// BLE/Wi-Fi Direct hardware. Bootstrap servers leave this as None.
    mesh_transport: Option<MeshTransport>,

    /// Whether the network event loop is running.
    running: bool,

    /// Channel sender for outbound messages (to the swarm event loop).
    /// Populated when `start()` is called.
    command_tx: Option<mpsc::Sender<NetworkCommand>>,

    /// Block provider for serving sync requests.
    /// WHY: Set after state is initialized (start_consensus), not at network start.
    block_provider: std::sync::Arc<dyn BlockProvider>,
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
    /// Announce this node's info to the network for committee selection.
    AnnounceNode(Box<NodeAnnouncement>),
    /// Publish a Lux social post to the gossip network.
    PublishLuxPost(Box<gratia_lux::LuxPost>),
    /// Publish a validator signature for a pending block to the gossip network.
    /// WHY: After validating a block from another producer, committee members
    /// broadcast their signature so the producer can collect enough for finality.
    PublishValidatorSignature(Box<ValidatorSignatureMessage>),
    /// Dial a specific peer address.
    DialPeer(String),
    /// Trigger a sync check — the SyncManager determines what to request.
    /// WHY: Called by the FFI layer when the app wants to force a sync (e.g.,
    /// on startup, after network reconnect, or when the user taps "refresh").
    RequestSync,
    /// Send a sync response back to a peer (internal, from event loop).
    SendSyncResponse {
        target_peer_bytes: Vec<u8>,
        response: crate::sync::SyncResponse,
    },
    /// Shut down the network.
    Shutdown,
}

impl NetworkManager {
    /// Create a new NetworkManager with the given configuration.
    pub fn new(config: NetworkConfig) -> Self {
        let transport_config = config.transport.clone();
        let bootstrap = config.bootstrap_peers.clone();
        let max_cached = config.max_cached_peers;

        // WHY: Initialize mesh transport if mesh config is present in the
        // transport config. The mesh peer ID is derived from the node's identity
        // (same 32-byte key as NodeId).
        let mesh_transport = transport_config.mesh.as_ref().map(|mesh_config| {
            let mesh_peer_id = mesh::MeshPeerId(config.local_node_id.0);
            MeshTransport::new(mesh_config.clone(), mesh_peer_id)
        });

        NetworkManager {
            config,
            gossip: GossipHandler::new(),
            discovery: PeerDiscovery::new(bootstrap, max_cached),
            sync_manager: SyncManager::new(0, BlockHash([0u8; 32])),
            connections: ConnectionManager::new(transport_config),
            mesh_transport,
            running: false,
            command_tx: None,
            block_provider: std::sync::Arc::new(NoBlockProvider),
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

        // WHY: Dial bootstrap peers on startup so phones can discover each other
        // across the internet, not just via mDNS on the same Wi-Fi. The bootstrap
        // node relays gossipsub and Kademlia traffic. If unreachable, the phone
        // falls back to mDNS-only discovery (same-LAN).
        for addr_str in &self.config.bootstrap_peers {
            match addr_str.parse::<Multiaddr>() {
                Ok(addr) => {
                    tracing::info!(%addr, "Dialing bootstrap peer");
                    if let Err(e) = swarm.dial(addr) {
                        tracing::warn!("Failed to dial bootstrap peer: {}", e);
                    }
                }
                Err(e) => {
                    tracing::warn!("Invalid bootstrap peer address '{}': {}", addr_str, e);
                }
            }
        }

        // Spawn the event loop as a background task
        let node_id = self.config.local_node_id;
        let block_provider = self.block_provider.clone();
        tokio::spawn(run_swarm_event_loop(swarm, command_rx, event_tx, node_id, block_provider));

        tracing::info!(
            node_id = %self.config.local_node_id,
            "Network manager started"
        );

        Ok(event_rx)
    }

    /// Set the block provider for serving sync requests.
    /// WHY: Called after state is initialized (typically in start_consensus)
    /// so that the sync protocol can serve blocks to peers requesting them.
    pub fn set_block_provider(&mut self, provider: std::sync::Arc<dyn BlockProvider>) {
        self.block_provider = provider;
    }

    /// Stop the network layer gracefully.
    pub async fn stop(&mut self) -> Result<(), NetworkError> {
        if !self.running {
            return Err(NetworkError::NotStarted);
        }

        if let Some(tx) = &self.command_tx {
            let _ = tx.send(NetworkCommand::Shutdown).await;
        }

        // Stop mesh layer if it was running
        self.stop_mesh();

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

    /// Non-blocking broadcast of a block to the network.
    /// WHY: Used from sync contexts (slot timer under mutex) where we can't
    /// await. Uses try_send which fails immediately if the channel is full
    /// rather than blocking. A full channel means the swarm task is backed up —
    /// dropping one block broadcast is acceptable; it'll be synced via catch-up.
    pub fn try_broadcast_block_sync(&self, block: &Block) -> Result<(), NetworkError> {
        let tx = self.command_tx.as_ref().ok_or(NetworkError::NotStarted)?;

        let _ = self.gossip.prepare_block(block.clone())?;

        tx.try_send(NetworkCommand::PublishBlock(Box::new(block.clone())))
            .map_err(|e| NetworkError::ChannelError(e.to_string()))?;

        Ok(())
    }

    /// Non-blocking broadcast of a transaction to the network.
    /// WHY: Used from sync contexts where we can't await.
    pub fn try_broadcast_transaction_sync(&self, tx: &Transaction) -> Result<(), NetworkError> {
        let cmd_tx = self.command_tx.as_ref().ok_or(NetworkError::NotStarted)?;

        let _ = self.gossip.prepare_transaction(tx.clone())?;

        cmd_tx.try_send(NetworkCommand::PublishTransaction(Box::new(tx.clone())))
            .map_err(|e| NetworkError::ChannelError(e.to_string()))?;

        Ok(())
    }

    /// Non-blocking broadcast of a validator signature to the network.
    /// WHY: Used from sync contexts (slot timer under mutex) where we can't await.
    pub fn try_broadcast_validator_signature_sync(
        &self,
        msg: &ValidatorSignatureMessage,
    ) -> Result<(), NetworkError> {
        let cmd_tx = self.command_tx.as_ref().ok_or(NetworkError::NotStarted)?;

        let _ = self.gossip.prepare_validator_signature(msg.clone())?;

        cmd_tx
            .try_send(NetworkCommand::PublishValidatorSignature(Box::new(msg.clone())))
            .map_err(|e| NetworkError::ChannelError(e.to_string()))?;

        Ok(())
    }

    /// Non-blocking broadcast of a Lux post to the network.
    /// WHY: Used from sync contexts (FFI layer) where we can't await.
    pub fn try_broadcast_lux_post_sync(&self, post: &gratia_lux::LuxPost) -> Result<(), NetworkError> {
        let cmd_tx = self.command_tx.as_ref().ok_or(NetworkError::NotStarted)?;

        let _ = self.gossip.prepare_lux_post(post.clone())?;

        cmd_tx.try_send(NetworkCommand::PublishLuxPost(Box::new(post.clone())))
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

    /// Announce this node's eligibility to the network via gossipsub.
    pub async fn announce_node(&self, announcement: NodeAnnouncement) -> Result<(), NetworkError> {
        let tx = self.command_tx.as_ref().ok_or(NetworkError::NotStarted)?;

        tx.send(NetworkCommand::AnnounceNode(Box::new(announcement)))
            .await
            .map_err(|e| NetworkError::ChannelError(e.to_string()))?;

        Ok(())
    }

    /// Non-blocking announce of this node's eligibility to the network.
    /// WHY: Used from sync contexts (under mutex) where we can't await.
    /// Uses try_send which fails immediately if the channel is full.
    pub fn try_announce_node_sync(&self, announcement: &NodeAnnouncement) -> Result<(), NetworkError> {
        let tx = self.command_tx.as_ref().ok_or(NetworkError::NotStarted)?;

        tx.try_send(NetworkCommand::AnnounceNode(Box::new(announcement.clone())))
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

    /// Request a sync from peers. The SyncManager determines what blocks
    /// to request and from which peer.
    pub async fn request_sync(&self) -> Result<(), NetworkError> {
        let tx = self.command_tx.as_ref().ok_or(NetworkError::NotStarted)?;

        tx.send(NetworkCommand::RequestSync)
            .await
            .map_err(|e| NetworkError::ChannelError(e.to_string()))?;

        Ok(())
    }

    /// Non-blocking sync request. WHY: Used from sync contexts where we can't await.
    pub fn try_request_sync(&self) -> Result<(), NetworkError> {
        let tx = self.command_tx.as_ref().ok_or(NetworkError::NotStarted)?;

        tx.try_send(NetworkCommand::RequestSync)
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

    /// Get a reference to the mesh transport (if configured).
    pub fn mesh_transport(&self) -> Option<&MeshTransport> {
        self.mesh_transport.as_ref()
    }

    /// Get a mutable reference to the mesh transport (if configured).
    pub fn mesh_transport_mut(&mut self) -> Option<&mut MeshTransport> {
        self.mesh_transport.as_mut()
    }

    /// Start the mesh layer (Layer 0).
    /// WHY: Mesh is started separately from the main network because it depends
    /// on native platform BLE/Wi-Fi Direct APIs that initialize asynchronously.
    /// The native layer calls this after its BLE stack is ready.
    pub fn start_mesh(&mut self) -> Result<(), NetworkError> {
        if let Some(ref mut mesh) = self.mesh_transport {
            mesh.start()?;
            tracing::info!("Mesh transport layer started");
            Ok(())
        } else {
            Err(NetworkError::Transport(
                "Mesh transport not configured".to_string(),
            ))
        }
    }

    /// Stop the mesh layer.
    pub fn stop_mesh(&mut self) {
        if let Some(ref mut mesh) = self.mesh_transport {
            mesh.stop();
            tracing::info!("Mesh transport layer stopped");
        }
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
            gossip::GossipMessage::NodeAnnouncement(ann) => NetworkEvent::NodeAnnounced(ann),
            gossip::GossipMessage::NewLuxPost(post) => NetworkEvent::LuxPostReceived(post),
            gossip::GossipMessage::ValidatorSignatureMsg(sig) => NetworkEvent::ValidatorSignatureReceived(sig),
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
///
/// Also manages sync state: tracks peer chain tips from gossipped blocks,
/// periodically polls for chain tips, and handles sync request/response
/// messages routed through gossipsub.
async fn run_swarm_event_loop(
    mut swarm: Swarm<GratiaBehaviour>,
    mut command_rx: mpsc::Receiver<NetworkCommand>,
    event_tx: mpsc::Sender<NetworkEvent>,
    node_id: NodeId,
    block_provider: std::sync::Arc<dyn BlockProvider>,
) {
    // WHY: Separate gossip handler for the event loop — deduplication must
    // happen where messages first arrive (here), not in the application layer.
    let mut gossip_handler = GossipHandler::new();

    // WHY: The event loop owns its own SyncManager to track peer chain tips
    // and coordinate block fetching. This avoids sharing mutable state with
    // the NetworkManager across the channel boundary.
    let mut sync_manager = SyncManager::new(0, BlockHash([0u8; 32]));

    // WHY: Periodic chain tip poll ensures we detect when we're behind even
    // if we miss a gossipped block (e.g., brief disconnection).
    let mut chain_tip_interval = tokio::time::interval(
        Duration::from_secs(CHAIN_TIP_POLL_INTERVAL_SECS),
    );

    // WHY: Get our own PeerId bytes once for filtering incoming sync messages.
    let local_peer_id = *swarm.local_peer_id();
    let local_peer_bytes = local_peer_id.to_bytes();

    tracing::info!(%node_id, %local_peer_id, "Swarm event loop started");

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

                        // Sync messages are handled separately from regular gossip
                        if topic == gossip::TOPIC_SYNC {
                            handle_sync_message(
                                &message.data,
                                &local_peer_bytes,
                                &mut sync_manager,
                                &mut swarm,
                                &event_tx,
                                &local_peer_id,
                                block_provider.as_ref(),
                            ).await;
                            continue;
                        }

                        match gossip_handler.process_incoming(topic, &message.data) {
                            Ok(Some(msg)) => {
                                let net_event = match msg {
                                    GossipMessage::NewBlock(block) => {
                                        tracing::debug!(
                                            height = block.header.height,
                                            "Received block via gossip"
                                        );

                                        // WHY: When we receive a block via gossip, the
                                        // producing peer is at least at this height. Use
                                        // the block's height and hash to update our view
                                        // of the network's chain tip. The source PeerId
                                        // from gossipsub tells us who sent it.
                                        if let Some(source_peer) = message.source {
                                            if let Ok(block_hash) = block.header.hash() {
                                                sync_manager.update_peer_state(
                                                    source_peer,
                                                    block.header.height,
                                                    block_hash,
                                                );
                                            }
                                        }

                                        NetworkEvent::BlockReceived(block)
                                    }
                                    GossipMessage::NewTransaction(tx) => {
                                        NetworkEvent::TransactionReceived(tx)
                                    }
                                    GossipMessage::NewAttestation(att) => {
                                        NetworkEvent::AttestationReceived(att)
                                    }
                                    GossipMessage::NodeAnnouncement(ann) => {
                                        tracing::info!(
                                            node_id = ?ann.node_id,
                                            score = ann.presence_score,
                                            "Received node announcement via gossip"
                                        );
                                        NetworkEvent::NodeAnnounced(ann)
                                    }
                                    GossipMessage::NewLuxPost(post) => {
                                        tracing::debug!(
                                            hash = %post.hash,
                                            author = %post.author,
                                            "Received Lux post via gossip"
                                        );
                                        NetworkEvent::LuxPostReceived(post)
                                    }
                                    GossipMessage::ValidatorSignatureMsg(sig_msg) => {
                                        tracing::debug!(
                                            height = sig_msg.height,
                                            validator = ?sig_msg.signature.validator,
                                            "Received validator signature via gossip"
                                        );
                                        NetworkEvent::ValidatorSignatureReceived(sig_msg)
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
                            // WHY: Log to file since tracing doesn't reach Android logcat
                            let log_msg = format!("mDNS discovered peer: {} at {}", peer_id, addr);
                            let log_path = "/data/user/0/io.gratia.app.debug/files/gratia-rust.log";
                            if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(log_path) {
                                use std::io::Write;
                                let _ = writeln!(f, "[{}] {}", chrono::Utc::now().format("%H:%M:%S"), log_msg);
                            }
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
                        // WHY: Remove peer's chain state so stale data doesn't
                        // influence sync decisions after disconnect.
                        sync_manager.remove_peer(&peer_id);
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

            // ── Periodic chain tip poll ──────────────────────────────────
            _ = chain_tip_interval.tick() => {
                // WHY: Broadcast a GetChainTip request to all peers so we can
                // detect if we're behind. The empty target field means all peers
                // should respond.
                let msg = SyncProtocolMessage {
                    source: local_peer_bytes.clone(),
                    target: vec![], // broadcast
                    payload: SyncPayload::Request(SyncRequest::GetChainTip),
                };
                if let Ok(data) = msg.to_bytes() {
                    let topic = gossipsub::IdentTopic::new(gossip::TOPIC_SYNC);
                    if let Err(e) = swarm.behaviour_mut().gossipsub.publish(topic, data) {
                        tracing::debug!("Failed to publish chain tip poll: {}", e);
                    }
                }

                // WHY: After polling, check if SyncManager has pending work.
                // If we're behind, generate and send a block request.
                try_send_next_sync_request(
                    &mut sync_manager,
                    &local_peer_bytes,
                    &mut swarm,
                );

                // Notify the application of sync state changes
                let state = sync_manager.state();
                let _ = event_tx.send(NetworkEvent::SyncStateChanged(state)).await;
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
                    Some(NetworkCommand::AnnounceNode(ann)) => {
                        let msg = GossipMessage::NodeAnnouncement(ann);
                        if let Ok(data) = msg.to_bytes() {
                            let topic = gossipsub::IdentTopic::new(gossip::TOPIC_NODE_ANNOUNCE);
                            if let Err(e) = swarm.behaviour_mut().gossipsub.publish(topic, data) {
                                tracing::error!("Failed to publish node announcement: {}", e);
                            }
                        }
                    }
                    Some(NetworkCommand::PublishLuxPost(post)) => {
                        let msg = GossipMessage::NewLuxPost(post);
                        if let Ok(data) = msg.to_bytes() {
                            let topic = gossipsub::IdentTopic::new(gossip::TOPIC_LUX_POSTS);
                            if let Err(e) = swarm.behaviour_mut().gossipsub.publish(topic, data) {
                                tracing::error!("Failed to publish Lux post: {}", e);
                            }
                        }
                    }
                    Some(NetworkCommand::PublishValidatorSignature(sig_msg)) => {
                        let msg = GossipMessage::ValidatorSignatureMsg(sig_msg);
                        if let Ok(data) = msg.to_bytes() {
                            let topic = gossipsub::IdentTopic::new(gossip::TOPIC_VALIDATOR_SIGS);
                            if let Err(e) = swarm.behaviour_mut().gossipsub.publish(topic, data) {
                                tracing::error!("Failed to publish validator signature: {}", e);
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
                    Some(NetworkCommand::SyncRequest { peer, from_height, to_height }) => {
                        // WHY: Send a targeted sync request to a specific peer
                        // via the gossipsub sync topic.
                        let msg = SyncProtocolMessage {
                            source: local_peer_bytes.clone(),
                            target: peer.to_bytes(),
                            payload: SyncPayload::Request(SyncRequest::GetBlocks {
                                from_height,
                                to_height,
                            }),
                        };
                        if let Ok(data) = msg.to_bytes() {
                            let topic = gossipsub::IdentTopic::new(gossip::TOPIC_SYNC);
                            if let Err(e) = swarm.behaviour_mut().gossipsub.publish(topic, data) {
                                tracing::error!("Failed to publish sync request: {}", e);
                            } else {
                                tracing::debug!(
                                    %peer,
                                    from_height,
                                    to_height,
                                    "Sent sync request to peer"
                                );
                            }
                        }
                    }
                    Some(NetworkCommand::RequestSync) => {
                        // WHY: The application/FFI layer wants to trigger a sync.
                        // Ask the SyncManager what to request next.
                        try_send_next_sync_request(
                            &mut sync_manager,
                            &local_peer_bytes,
                            &mut swarm,
                        );
                    }
                    Some(NetworkCommand::SendSyncResponse { target_peer_bytes, response }) => {
                        let msg = SyncProtocolMessage {
                            source: local_peer_bytes.clone(),
                            target: target_peer_bytes,
                            payload: SyncPayload::Response(response),
                        };
                        if let Ok(data) = msg.to_bytes() {
                            let topic = gossipsub::IdentTopic::new(gossip::TOPIC_SYNC);
                            if let Err(e) = swarm.behaviour_mut().gossipsub.publish(topic, data) {
                                tracing::error!("Failed to publish sync response: {}", e);
                            }
                        }
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

/// Handle an incoming sync protocol message from gossipsub.
///
/// Deserializes the message, checks if it's addressed to us, and either:
/// - Handles a SyncRequest by responding with our local data
/// - Handles a SyncResponse by feeding blocks to the SyncManager
async fn handle_sync_message(
    data: &[u8],
    local_peer_bytes: &[u8],
    sync_manager: &mut SyncManager,
    swarm: &mut Swarm<GratiaBehaviour>,
    event_tx: &mpsc::Sender<NetworkEvent>,
    _local_peer_id: &PeerId,
    block_provider: &dyn BlockProvider,
) {
    let msg = match SyncProtocolMessage::from_bytes(data) {
        Ok(m) => m,
        Err(e) => {
            tracing::debug!("Failed to deserialize sync message: {}", e);
            return;
        }
    };

    // Ignore messages not addressed to us (unless broadcast)
    if !msg.is_for_peer(local_peer_bytes) {
        return;
    }

    // Don't process our own messages
    if msg.source == local_peer_bytes {
        return;
    }

    let source_peer = match PeerId::from_bytes(&msg.source) {
        Ok(p) => p,
        Err(e) => {
            tracing::debug!("Invalid source PeerId in sync message: {}", e);
            return;
        }
    };

    match msg.payload {
        SyncPayload::Request(request) => {
            tracing::debug!(
                %source_peer,
                ?request,
                "Received sync request"
            );

            // WHY: Respond to the requesting peer with our local data.
            // The block_provider is set by the FFI layer after state initialization,
            // giving the sync handler access to stored blocks.
            let response = sync_manager.handle_sync_request(&request, |from, to| {
                let blocks = block_provider.get_blocks(from, to);
                if blocks.is_empty() { None } else { Some(blocks) }
            });

            // Send the response back to the requesting peer
            let resp_msg = SyncProtocolMessage {
                source: local_peer_bytes.to_vec(),
                target: msg.source,
                payload: SyncPayload::Response(response),
            };
            if let Ok(resp_data) = resp_msg.to_bytes() {
                let topic = gossipsub::IdentTopic::new(gossip::TOPIC_SYNC);
                if let Err(e) = swarm.behaviour_mut().gossipsub.publish(topic, resp_data) {
                    tracing::debug!("Failed to send sync response: {}", e);
                }
            }
        }
        SyncPayload::Response(response) => {
            tracing::debug!(
                %source_peer,
                "Received sync response"
            );

            match response {
                crate::sync::SyncResponse::ChainTip { height, hash } => {
                    // WHY: Update the SyncManager's view of this peer's chain.
                    // This is how we discover we're behind after chain tip polls.
                    sync_manager.update_peer_state(source_peer, height, hash);

                    tracing::debug!(
                        %source_peer,
                        height,
                        "Updated peer chain tip"
                    );

                    // After learning about a new chain tip, check if we should sync
                    if let Some((target_peer, request)) = sync_manager.next_sync_request() {
                        let req_msg = SyncProtocolMessage {
                            source: local_peer_bytes.to_vec(),
                            target: target_peer.to_bytes(),
                            payload: SyncPayload::Request(request),
                        };
                        if let Ok(req_data) = req_msg.to_bytes() {
                            let topic = gossipsub::IdentTopic::new(gossip::TOPIC_SYNC);
                            if let Err(e) = swarm.behaviour_mut().gossipsub.publish(topic, req_data) {
                                tracing::debug!("Failed to send sync request after tip update: {}", e);
                            }
                        }
                    }

                    // Notify application of sync state change
                    let _ = event_tx.send(NetworkEvent::SyncStateChanged(
                        sync_manager.state()
                    )).await;
                }
                crate::sync::SyncResponse::Blocks(blocks) => {
                    // WHY: Feed received blocks to the SyncManager for validation,
                    // then forward to the application layer for consensus validation
                    // and application to the chain.
                    match sync_manager.handle_blocks_response(&source_peer, blocks) {
                        Ok(validated_blocks) => {
                            if !validated_blocks.is_empty() {
                                tracing::info!(
                                    count = validated_blocks.len(),
                                    from = validated_blocks.first().map(|b| b.header.height),
                                    to = validated_blocks.last().map(|b| b.header.height),
                                    "Sync received blocks"
                                );

                                // Update local state based on the last block received
                                if let Some(last) = validated_blocks.last() {
                                    if let Ok(hash) = last.header.hash() {
                                        sync_manager.update_local_state(
                                            last.header.height,
                                            hash,
                                        );
                                    }
                                }

                                let _ = event_tx.send(
                                    NetworkEvent::SyncBlocksReceived(validated_blocks)
                                ).await;

                                // WHY: After receiving a batch, check if there are
                                // more blocks to fetch (we might be many batches behind).
                                if let Some((next_peer, next_request)) = sync_manager.next_sync_request() {
                                    let req_msg = SyncProtocolMessage {
                                        source: local_peer_bytes.to_vec(),
                                        target: next_peer.to_bytes(),
                                        payload: SyncPayload::Request(next_request),
                                    };
                                    if let Ok(req_data) = req_msg.to_bytes() {
                                        let topic = gossipsub::IdentTopic::new(gossip::TOPIC_SYNC);
                                        let _ = swarm.behaviour_mut().gossipsub.publish(topic, req_data);
                                    }
                                }
                            }
                        }
                        Err(e) => {
                            tracing::warn!(
                                %source_peer,
                                "Sync blocks response validation failed: {}",
                                e
                            );
                        }
                    }

                    let _ = event_tx.send(NetworkEvent::SyncStateChanged(
                        sync_manager.state()
                    )).await;
                }
                crate::sync::SyncResponse::Headers(headers) => {
                    tracing::debug!(
                        %source_peer,
                        count = headers.len(),
                        "Received sync headers (not yet used for sync decisions)"
                    );
                }
                crate::sync::SyncResponse::Error(err) => {
                    tracing::debug!(
                        %source_peer,
                        "Sync error from peer: {}",
                        err
                    );
                }
            }
        }
    }
}

/// Try to generate and send the next sync request from the SyncManager.
///
/// WHY: Extracted into a helper because this is called from multiple places:
/// periodic chain tip poll, RequestSync command, and after receiving blocks.
fn try_send_next_sync_request(
    sync_manager: &mut SyncManager,
    local_peer_bytes: &[u8],
    swarm: &mut Swarm<GratiaBehaviour>,
) {
    if let Some((peer, request)) = sync_manager.next_sync_request() {
        let msg = SyncProtocolMessage {
            source: local_peer_bytes.to_vec(),
            target: peer.to_bytes(),
            payload: SyncPayload::Request(request),
        };
        if let Ok(data) = msg.to_bytes() {
            let topic = gossipsub::IdentTopic::new(gossip::TOPIC_SYNC);
            if let Err(e) = swarm.behaviour_mut().gossipsub.publish(topic, data) {
                tracing::debug!("Failed to send sync request: {}", e);
            } else {
                tracing::debug!(%peer, "Sent sync request");
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

    #[tokio::test]
    async fn test_request_sync_requires_running() {
        let config = NetworkConfig::new(test_node_id());
        let nm = NetworkManager::new(config);

        let result = nm.request_sync().await;
        assert!(matches!(result, Err(NetworkError::NotStarted)));
    }

    #[tokio::test]
    async fn test_try_request_sync_requires_running() {
        let config = NetworkConfig::new(test_node_id());
        let nm = NetworkManager::new(config);

        let result = nm.try_request_sync();
        assert!(matches!(result, Err(NetworkError::NotStarted)));
    }

    #[tokio::test]
    async fn test_request_sync_sends_command() {
        let config = NetworkConfig::new(test_node_id());
        let mut nm = NetworkManager::new(config);

        let _event_rx = nm.start().await.unwrap();

        // Should succeed — sends RequestSync command to event loop
        let result = nm.request_sync().await;
        assert!(result.is_ok());

        nm.stop().await.unwrap();
    }

    #[test]
    fn test_sync_manager_accessible() {
        let config = NetworkConfig::new(test_node_id());
        let mut nm = NetworkManager::new(config);

        // Sync manager should be initialized with unknown state
        assert_eq!(nm.sync_state(), SyncState::Unknown);
        assert_eq!(nm.sync_manager().local_height(), 0);

        // Should be able to update via mutable reference
        let peer = PeerId::random();
        nm.sync_manager_mut().update_peer_state(peer, 100, BlockHash([1u8; 32]));
        assert!(matches!(nm.sync_state(), SyncState::Behind { .. }));
    }

    #[test]
    fn test_sync_protocol_message_serialization() {
        use crate::sync::{SyncProtocolMessage, SyncPayload, SyncRequest};

        let peer = PeerId::random();
        let msg = SyncProtocolMessage {
            source: peer.to_bytes(),
            target: vec![], // broadcast
            payload: SyncPayload::Request(SyncRequest::GetChainTip),
        };

        let bytes = msg.to_bytes().unwrap();
        let decoded = SyncProtocolMessage::from_bytes(&bytes).unwrap();

        assert_eq!(decoded.source, peer.to_bytes());
        assert!(decoded.target.is_empty());
        assert!(matches!(
            decoded.payload,
            SyncPayload::Request(SyncRequest::GetChainTip)
        ));
    }

    #[test]
    fn test_sync_protocol_message_peer_filtering() {
        use crate::sync::{SyncProtocolMessage, SyncPayload, SyncRequest};

        let source = PeerId::random();
        let target = PeerId::random();
        let other = PeerId::random();

        // Targeted message
        let msg = SyncProtocolMessage {
            source: source.to_bytes(),
            target: target.to_bytes(),
            payload: SyncPayload::Request(SyncRequest::GetChainTip),
        };

        assert!(msg.is_for_peer(&target.to_bytes()));
        assert!(!msg.is_for_peer(&other.to_bytes()));

        // Broadcast message (empty target)
        let broadcast_msg = SyncProtocolMessage {
            source: source.to_bytes(),
            target: vec![],
            payload: SyncPayload::Request(SyncRequest::GetChainTip),
        };

        assert!(broadcast_msg.is_for_peer(&target.to_bytes()));
        assert!(broadcast_msg.is_for_peer(&other.to_bytes()));
    }

    #[test]
    fn test_sync_protocol_message_blocks_response() {
        use crate::sync::{SyncProtocolMessage, SyncPayload, SyncResponse};

        let source = PeerId::random();
        let target = PeerId::random();

        let msg = SyncProtocolMessage {
            source: source.to_bytes(),
            target: target.to_bytes(),
            payload: SyncPayload::Response(SyncResponse::ChainTip {
                height: 42,
                hash: BlockHash([7u8; 32]),
            }),
        };

        let bytes = msg.to_bytes().unwrap();
        let decoded = SyncProtocolMessage::from_bytes(&bytes).unwrap();

        match decoded.payload {
            SyncPayload::Response(SyncResponse::ChainTip { height, hash }) => {
                assert_eq!(height, 42);
                assert_eq!(hash, BlockHash([7u8; 32]));
            }
            _ => panic!("Expected ChainTip response"),
        }
    }

    #[test]
    fn test_peer_disconnect_removes_sync_state() {
        let config = NetworkConfig::new(test_node_id());
        let mut nm = NetworkManager::new(config);

        let peer = PeerId::random();
        nm.on_peer_connected(peer, true);
        nm.sync_manager_mut().update_peer_state(peer, 100, BlockHash([1u8; 32]));
        assert_eq!(nm.sync_manager().tracked_peer_count(), 1);

        nm.on_peer_disconnected(&peer, true);
        assert_eq!(nm.sync_manager().tracked_peer_count(), 0);
    }

    #[test]
    fn test_sync_topic_in_all_topics() {
        // Verify TOPIC_SYNC is included in ALL_TOPICS so nodes subscribe to it
        assert!(gossip::ALL_TOPICS.contains(&gossip::TOPIC_SYNC));
    }

    #[test]
    fn test_sync_manager_initialized_at_genesis() {
        let config = NetworkConfig::new(test_node_id());
        let nm = NetworkManager::new(config);

        // SyncManager should start at height 0 with zero hash
        assert_eq!(nm.sync_manager().local_height(), 0);
        assert_eq!(nm.sync_state(), SyncState::Unknown);
        assert_eq!(nm.sync_manager().tracked_peer_count(), 0);
        assert_eq!(nm.sync_manager().pending_request_count(), 0);
    }
}
