//! # Bluetooth/Wi-Fi Direct Mesh Transport (Layer 0)
//!
//! Implements the mesh networking layer for offline and local transactions.
//! This layer operates below the libp2p consensus layer (Layer 1) and enables:
//!
//! - **Offline payments:** NFC-initiated, BLE-relayed transactions that reach
//!   the consensus network through bridge peers with internet connectivity.
//! - **Local peer discovery:** Bluetooth LE and Wi-Fi Direct scanning for
//!   nearby Gratia nodes.
//! - **Multi-hop relay:** Messages propagate through the mesh with TTL-based
//!   flood routing and deduplication.
//!
//! ## Architecture
//!
//! The mesh layer is transport-agnostic at the protocol level. Actual BLE and
//! Wi-Fi Direct I/O is handled by the native platform layer (Kotlin on Android,
//! Swift on iOS) which calls into this Rust core via UniFFI. This module
//! manages the mesh protocol: message formatting, relay decisions, peer
//! tracking, deduplication, and bridge detection.
//!
//! ## Offline Transaction Flow
//!
//! 1. User A taps phone to User B (NFC contact exchange)
//! 2. Transaction is created and signed locally
//! 3. Transaction is broadcast to the mesh network as a `MeshMessage`
//! 4. Any mesh peer with internet connectivity bridges it to Layer 1
//! 5. Transaction confirms when it reaches the consensus network

use std::collections::{HashMap, HashSet, VecDeque};
use std::fmt;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::error::NetworkError;

// ============================================================================
// Configuration
// ============================================================================

/// Configuration for the mesh transport layer.
///
/// WHY: Separate config from TransportConfig because mesh operates on a
/// fundamentally different layer (BLE/Wi-Fi Direct vs. libp2p/QUIC) with
/// different constraints and tuning parameters.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MeshConfig {
    /// Enable Bluetooth LE mesh.
    pub bluetooth_enabled: bool,

    /// Enable Wi-Fi Direct mesh.
    pub wifi_direct_enabled: bool,

    /// Maximum mesh peers to maintain.
    /// WHY: BLE connections are expensive on battery. 8 peers balances mesh
    /// connectivity against power consumption on low-end phones.
    pub max_mesh_peers: usize,

    /// Mesh message TTL (hop count).
    /// WHY: 5 hops covers a reasonable physical area (~50-100m per hop for BLE)
    /// without flooding the mesh with stale messages. Higher TTL wastes battery
    /// on relay traffic that will likely never reach a bridge peer.
    pub max_ttl: u8,

    /// Scan interval for peer discovery (seconds).
    /// WHY: BLE scanning is power-intensive. 30 seconds balances discovery
    /// speed against battery drain. Passive scanning (listening for beacons)
    /// uses less power than active scanning.
    pub scan_interval_secs: u64,

    /// Maximum message size for mesh relay (bytes).
    /// WHY: BLE MTU is typically 244 bytes (with DLE) but we fragment at the
    /// transport layer. 4KB covers a signed transaction (~250 bytes standard,
    /// ~2KB shielded) with room for mesh headers. Larger messages (blocks)
    /// are NOT relayed over mesh — only transactions and compact announcements.
    pub max_mesh_message_size: usize,

    /// Maximum age of a message before it's considered stale (seconds).
    /// WHY: Prevents relaying ancient messages that were stuck in a partition.
    /// 5 minutes is generous — most mesh messages should reach a bridge peer
    /// within seconds if one exists in range.
    pub max_message_age_secs: u64,

    /// Maximum number of seen message IDs to cache for deduplication.
    /// WHY: Each ID is 32 bytes. 10,000 IDs = 320KB — acceptable on mobile.
    /// Messages older than max_message_age_secs are purged, but this cap
    /// prevents unbounded growth if many unique messages arrive quickly.
    pub max_seen_cache_size: usize,

    /// Peer staleness timeout (seconds).
    /// WHY: If a mesh peer hasn't been seen in 2 minutes, it likely moved out
    /// of BLE range. Remove it so we don't waste relay attempts.
    pub peer_stale_timeout_secs: u64,
}

impl Default for MeshConfig {
    fn default() -> Self {
        MeshConfig {
            bluetooth_enabled: true,
            wifi_direct_enabled: false,
            max_mesh_peers: 8,
            // 5 hops — covers ~250-500m BLE mesh radius
            max_ttl: 5,
            // 30 seconds — balances discovery speed vs battery
            scan_interval_secs: 30,
            // 4 KB — fits a shielded transaction with mesh headers
            max_mesh_message_size: 4096,
            // 5 minutes — generous staleness window
            max_message_age_secs: 300,
            // 10,000 message IDs at 32 bytes each = 320 KB
            max_seen_cache_size: 10_000,
            // 2 minutes — peer likely moved out of range
            peer_stale_timeout_secs: 120,
        }
    }
}

impl MeshConfig {
    /// Validate the mesh configuration.
    pub fn validate(&self) -> Result<(), String> {
        if !self.bluetooth_enabled && !self.wifi_direct_enabled {
            return Err("At least one mesh transport must be enabled".to_string());
        }
        if self.max_mesh_peers == 0 {
            return Err("max_mesh_peers must be greater than 0".to_string());
        }
        if self.max_ttl == 0 {
            return Err("max_ttl must be greater than 0".to_string());
        }
        if self.max_mesh_message_size == 0 {
            return Err("max_mesh_message_size must be greater than 0".to_string());
        }
        Ok(())
    }
}

// ============================================================================
// Peer Types
// ============================================================================

/// A mesh peer identifier — 32-byte public key hash.
///
/// WHY: Separate from libp2p PeerId because mesh peers may not have a libp2p
/// identity (they might only be reachable via BLE). The mesh peer ID is derived
/// from the node's Ed25519 public key, same as NodeId.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct MeshPeerId(pub [u8; 32]);

impl fmt::Display for MeshPeerId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Show first 8 hex chars for readability
        for byte in &self.0[..4] {
            write!(f, "{:02x}", byte)?;
        }
        write!(f, "...")
    }
}

/// How a mesh peer is connected.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum MeshTransportType {
    /// Connected via Bluetooth Low Energy.
    BluetoothLE,
    /// Connected via Wi-Fi Direct.
    WifiDirect,
    /// Connected via both transports simultaneously.
    Both,
}

/// Information about a known mesh peer.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MeshPeerInfo {
    /// Peer identity.
    pub peer_id: MeshPeerId,

    /// How this peer is connected.
    pub transport: MeshTransportType,

    /// Signal strength (RSSI for BLE, signal level for Wi-Fi Direct).
    /// WHY: Used to prefer closer/stronger peers for relay. BLE RSSI typically
    /// ranges from -30 (very close) to -100 (far away). None if unavailable.
    pub signal_strength: Option<i8>,

    /// Last seen timestamp (Unix seconds).
    pub last_seen: u64,

    /// Hop count to this peer (1 = direct, 2+ = relayed).
    pub hop_count: u8,

    /// Whether this peer has internet connectivity (can bridge to Layer 1).
    /// WHY: Bridge peers are critical — they connect the offline mesh to the
    /// consensus network. Preferring bridge peers for relay ensures offline
    /// transactions reach confirmation faster.
    pub has_internet: bool,
}

// ============================================================================
// Message Types
// ============================================================================

/// The type of message being relayed over the mesh.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum MeshMessageType {
    /// A signed transaction to relay to the consensus network.
    Transaction,
    /// A compact block header announcement (not the full block).
    /// WHY: Full blocks are too large for BLE relay. Headers let mesh peers
    /// know the chain is progressing even while offline.
    BlockHeader,
    /// Peer discovery beacon — announces presence and capabilities.
    PeerDiscovery,
    /// Offline payment (NFC-initiated, BLE-relayed).
    /// WHY: Separate from Transaction because offline payments include extra
    /// metadata (NFC handshake proof, local timestamp agreement) that the
    /// bridge peer needs to validate before forwarding to Layer 1.
    OfflinePayment,
    /// Mesh routing table update — shares known peer topology.
    RoutingUpdate,
}

/// A message relayed through the mesh network.
///
/// WHY: Fixed format optimized for BLE constraints. The ID is computed from
/// the payload + source + timestamp to prevent replay attacks while enabling
/// deduplication across relay hops.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MeshMessage {
    /// Unique message ID (SHA-256 hash of source + payload + timestamp).
    /// WHY: Content-addressed ID enables deduplication without maintaining
    /// sender-specific sequence numbers across disconnected mesh segments.
    pub id: [u8; 32],

    /// Message type.
    pub msg_type: MeshMessageType,

    /// Remaining TTL (decremented at each hop).
    pub ttl: u8,

    /// Source peer (originator, NOT the relay).
    /// WHY: Preserved across hops so the final recipient (or bridge peer)
    /// knows who originated the message. Relays do not change this field.
    pub source: MeshPeerId,

    /// Payload (serialized transaction, block header, etc.).
    pub payload: Vec<u8>,

    /// Timestamp (Unix seconds) when the message was created.
    /// WHY: Used with max_message_age_secs to drop stale messages.
    pub timestamp: u64,
}

impl MeshMessage {
    /// Compute the message ID from source, payload, and timestamp.
    ///
    /// WHY: Deterministic ID means every node computes the same ID for the
    /// same message, enabling deduplication without coordination.
    pub fn compute_id(source: &MeshPeerId, payload: &[u8], timestamp: u64) -> [u8; 32] {
        let mut hasher = Sha256::new();
        hasher.update(b"gratia-mesh-msg-v1:");
        hasher.update(source.0);
        hasher.update(payload);
        hasher.update(timestamp.to_be_bytes());
        let result = hasher.finalize();
        let mut id = [0u8; 32];
        id.copy_from_slice(&result);
        id
    }

    /// Create a new mesh message with a computed ID.
    pub fn new(
        msg_type: MeshMessageType,
        ttl: u8,
        source: MeshPeerId,
        payload: Vec<u8>,
        timestamp: u64,
    ) -> Self {
        let id = Self::compute_id(&source, &payload, timestamp);
        MeshMessage {
            id,
            msg_type,
            ttl,
            source,
            payload,
            timestamp,
        }
    }

    /// Serialize the message to bytes for transmission.
    pub fn to_bytes(&self) -> Result<Vec<u8>, NetworkError> {
        bincode::serialize(self).map_err(|e| NetworkError::Serialization(e.to_string()))
    }

    /// Deserialize a message from bytes.
    pub fn from_bytes(data: &[u8]) -> Result<Self, NetworkError> {
        bincode::deserialize(data).map_err(|e| NetworkError::Serialization(e.to_string()))
    }

    /// Verify the message ID matches the content.
    /// WHY: Prevents tampered messages from being relayed. A relay that
    /// modifies the payload without updating the ID will be caught.
    pub fn verify_id(&self) -> bool {
        let expected = Self::compute_id(&self.source, &self.payload, self.timestamp);
        self.id == expected
    }
}

// ============================================================================
// Mesh Action (receive result)
// ============================================================================

/// The action to take after receiving a mesh message.
#[derive(Debug, PartialEq, Eq)]
pub enum MeshAction {
    /// Message is new and valid — deliver to the application layer.
    Deliver(MeshMessageType),
    /// Message should be relayed to other peers (already queued internally).
    Relay,
    /// Message was already seen (duplicate).
    Duplicate,
    /// Message TTL has expired — do not relay.
    Expired,
    /// Message is too old (timestamp exceeds max_message_age_secs).
    Stale,
    /// Message ID does not match content (tampered).
    InvalidId,
}

// ============================================================================
// Mesh Transport
// ============================================================================

/// The mesh transport manager.
///
/// Manages the Layer 0 mesh protocol: peer tracking, message relay,
/// deduplication, and bridge detection. Actual BLE/Wi-Fi Direct I/O is
/// performed by the native platform layer and fed into this struct via
/// `receive_message()` and `add_peer()`.
pub struct MeshTransport {
    /// Mesh configuration.
    config: MeshConfig,

    /// Known mesh peers and their connection state.
    peers: HashMap<MeshPeerId, MeshPeerInfo>,

    /// Messages pending relay to other peers.
    /// WHY: VecDeque for FIFO ordering — oldest messages relayed first.
    relay_queue: VecDeque<MeshMessage>,

    /// Seen message IDs for deduplication.
    /// WHY: HashSet of 32-byte IDs. Messages are deduplicated by content hash,
    /// so the same transaction relayed through different paths is only processed
    /// once. This prevents relay storms in dense mesh topologies.
    seen_messages: HashSet<[u8; 32]>,

    /// Whether the mesh layer is actively scanning/advertising.
    active: bool,

    /// Our own mesh peer ID.
    local_peer_id: MeshPeerId,

    /// Whether we have internet connectivity (are we a bridge peer?).
    /// WHY: Bridge peers advertise internet availability so offline nodes
    /// can route transactions toward them for Layer 1 bridging.
    has_internet: bool,
}

impl MeshTransport {
    /// Create a new mesh transport with the given configuration.
    pub fn new(config: MeshConfig, local_peer_id: MeshPeerId) -> Self {
        MeshTransport {
            config,
            peers: HashMap::new(),
            relay_queue: VecDeque::new(),
            seen_messages: HashSet::new(),
            active: false,
            local_peer_id,
            has_internet: false,
        }
    }

    /// Start mesh scanning and advertisement.
    ///
    /// WHY: This only sets the internal state to active. The actual BLE/Wi-Fi
    /// Direct scanning is triggered by the native platform layer which polls
    /// `is_active()` and starts platform-specific scan APIs accordingly.
    pub fn start(&mut self) -> Result<(), NetworkError> {
        self.config
            .validate()
            .map_err(|e| NetworkError::Transport(e))?;

        self.active = true;
        tracing::info!(
            bluetooth = self.config.bluetooth_enabled,
            wifi_direct = self.config.wifi_direct_enabled,
            max_peers = self.config.max_mesh_peers,
            "Mesh transport started"
        );
        Ok(())
    }

    /// Stop mesh layer.
    pub fn stop(&mut self) {
        self.active = false;
        self.relay_queue.clear();
        tracing::info!("Mesh transport stopped");
    }

    /// Whether the mesh layer is active.
    pub fn is_active(&self) -> bool {
        self.active
    }

    /// Set whether this node has internet connectivity.
    pub fn set_internet_available(&mut self, available: bool) {
        self.has_internet = available;
    }

    /// Whether this node can bridge mesh messages to Layer 1.
    pub fn is_bridge(&self) -> bool {
        self.has_internet
    }

    /// Get the local mesh peer ID.
    pub fn local_peer_id(&self) -> &MeshPeerId {
        &self.local_peer_id
    }

    /// Get the mesh configuration.
    pub fn config(&self) -> &MeshConfig {
        &self.config
    }

    // ── Message Operations ──────────────────────────────────────────────

    /// Broadcast a message to all mesh peers via relay.
    ///
    /// Creates a new MeshMessage with the configured TTL and queues it for
    /// relay. Returns the message ID.
    pub fn broadcast(
        &mut self,
        msg_type: MeshMessageType,
        payload: Vec<u8>,
        timestamp: u64,
    ) -> Result<[u8; 32], NetworkError> {
        if !self.active {
            return Err(NetworkError::NotStarted);
        }

        if payload.len() > self.config.max_mesh_message_size {
            return Err(NetworkError::MessageTooLarge {
                size: payload.len(),
                max: self.config.max_mesh_message_size,
            });
        }

        let msg = MeshMessage::new(
            msg_type,
            self.config.max_ttl,
            self.local_peer_id,
            payload,
            timestamp,
        );

        let id = msg.id;

        // Mark as seen so we don't process our own broadcast
        self.seen_messages.insert(id);
        self.enforce_seen_cache_limit();

        // Queue for relay to all connected peers
        self.relay_queue.push_back(msg);

        tracing::debug!(
            msg_id = hex::encode(&id[..4]),
            msg_type = ?msg_type,
            ttl = self.config.max_ttl,
            "Broadcast mesh message"
        );

        Ok(id)
    }

    /// Send a message to a specific peer (if reachable).
    ///
    /// WHY: Directed messages still use the relay queue but are tagged for
    /// a specific peer. In the current implementation, this is equivalent
    /// to broadcast (mesh is flood-based), but the API exists for future
    /// optimized routing.
    pub fn send_to(
        &mut self,
        _peer: &MeshPeerId,
        msg_type: MeshMessageType,
        payload: Vec<u8>,
        timestamp: u64,
    ) -> Result<(), NetworkError> {
        // WHY: For now, directed sends use the same broadcast path because
        // the mesh is flood-based. Future optimization: use routing table
        // to only relay toward the target peer.
        self.broadcast(msg_type, payload, timestamp)?;
        Ok(())
    }

    /// Process an incoming mesh message.
    ///
    /// Validates the message, checks for duplicates, decrements TTL,
    /// and either delivers to the application layer or queues for relay.
    pub fn receive_message(
        &mut self,
        msg: MeshMessage,
        current_time: u64,
    ) -> Result<MeshAction, NetworkError> {
        // Verify message ID integrity
        if !msg.verify_id() {
            tracing::debug!(
                msg_id = hex::encode(&msg.id[..4]),
                "Mesh message has invalid ID (tampered?)"
            );
            return Ok(MeshAction::InvalidId);
        }

        // Check for duplicate
        if self.seen_messages.contains(&msg.id) {
            return Ok(MeshAction::Duplicate);
        }

        // Check message age
        if current_time > msg.timestamp
            && (current_time - msg.timestamp) > self.config.max_message_age_secs
        {
            tracing::debug!(
                msg_id = hex::encode(&msg.id[..4]),
                age_secs = current_time - msg.timestamp,
                "Mesh message is stale"
            );
            return Ok(MeshAction::Stale);
        }

        // Check payload size
        if msg.payload.len() > self.config.max_mesh_message_size {
            return Err(NetworkError::MessageTooLarge {
                size: msg.payload.len(),
                max: self.config.max_mesh_message_size,
            });
        }

        // Mark as seen
        self.seen_messages.insert(msg.id);
        self.enforce_seen_cache_limit();

        // Check TTL
        if msg.ttl == 0 {
            tracing::debug!(
                msg_id = hex::encode(&msg.id[..4]),
                "Mesh message TTL expired"
            );
            return Ok(MeshAction::Expired);
        }

        let msg_type = msg.msg_type;

        // Queue for relay with decremented TTL
        let relay_msg = MeshMessage {
            ttl: msg.ttl - 1,
            ..msg
        };
        self.relay_queue.push_back(relay_msg);

        tracing::debug!(
            msg_id = hex::encode(&self.seen_messages.iter().last().unwrap_or(&[0u8; 32])[..4]),
            msg_type = ?msg_type,
            remaining_ttl = msg.ttl - 1,
            "Delivering mesh message"
        );

        Ok(MeshAction::Deliver(msg_type))
    }

    /// Drain the relay queue — returns messages paired with target peers.
    ///
    /// The native platform layer calls this periodically and transmits the
    /// returned messages over BLE/Wi-Fi Direct to the indicated peers.
    ///
    /// WHY: Returns (peer_id, message) pairs. Each message is sent to all
    /// connected peers except the source (to avoid echo). Bridge peers are
    /// prioritized by appearing first in the list.
    pub fn drain_relay_queue(&mut self) -> Vec<(MeshPeerId, MeshMessage)> {
        let mut outbound = Vec::new();

        while let Some(msg) = self.relay_queue.pop_front() {
            // Collect target peers: all peers except the message source
            // WHY: Don't relay back to the originator — wastes bandwidth
            let mut targets: Vec<&MeshPeerInfo> = self
                .peers
                .values()
                .filter(|p| p.peer_id != msg.source)
                .collect();

            // WHY: Sort bridge peers first — they can forward to Layer 1 fastest.
            // Among non-bridge peers, prefer stronger signal (closer peers).
            targets.sort_by(|a, b| {
                b.has_internet
                    .cmp(&a.has_internet)
                    .then_with(|| {
                        // Higher RSSI (less negative) = closer = prefer
                        let a_rssi = a.signal_strength.unwrap_or(i8::MIN);
                        let b_rssi = b.signal_strength.unwrap_or(i8::MIN);
                        b_rssi.cmp(&a_rssi)
                    })
            });

            for target in targets {
                outbound.push((target.peer_id, msg.clone()));
            }
        }

        outbound
    }

    // ── Peer Management ─────────────────────────────────────────────────

    /// Add or update a discovered mesh peer.
    pub fn add_peer(&mut self, peer: MeshPeerInfo) {
        // Don't track ourselves
        if peer.peer_id == self.local_peer_id {
            return;
        }

        // Enforce peer limit — only add if we have room or this peer is already known
        if self.peers.len() >= self.config.max_mesh_peers
            && !self.peers.contains_key(&peer.peer_id)
        {
            // WHY: If at capacity, evict the stalest non-bridge peer to make room.
            // Bridge peers are never evicted because they're critical for Layer 1 bridging.
            let stalest = self
                .peers
                .iter()
                .filter(|(_, p)| !p.has_internet)
                .min_by_key(|(_, p)| p.last_seen)
                .map(|(id, _)| *id);

            if let Some(evict_id) = stalest {
                tracing::debug!(
                    evicted = %evict_id,
                    new = %peer.peer_id,
                    "Evicting stale mesh peer to make room"
                );
                self.peers.remove(&evict_id);
            } else {
                // All peers are bridge peers — don't evict any
                tracing::debug!(
                    "Mesh peer limit reached and all peers are bridge peers, ignoring new peer"
                );
                return;
            }
        }

        tracing::debug!(
            peer_id = %peer.peer_id,
            transport = ?peer.transport,
            has_internet = peer.has_internet,
            hop_count = peer.hop_count,
            "Added/updated mesh peer"
        );
        self.peers.insert(peer.peer_id, peer);
    }

    /// Remove a mesh peer by ID.
    pub fn remove_peer(&mut self, peer_id: &MeshPeerId) {
        if self.peers.remove(peer_id).is_some() {
            tracing::debug!(peer_id = %peer_id, "Removed mesh peer");
        }
    }

    /// Remove peers that haven't been seen within the staleness timeout.
    pub fn remove_stale_peers(&mut self, current_time: u64) {
        let timeout = self.config.peer_stale_timeout_secs;
        let stale_ids: Vec<MeshPeerId> = self
            .peers
            .iter()
            .filter(|(_, p)| {
                current_time > p.last_seen && (current_time - p.last_seen) > timeout
            })
            .map(|(id, _)| *id)
            .collect();

        for id in &stale_ids {
            tracing::debug!(peer_id = %id, "Removing stale mesh peer");
            self.peers.remove(id);
        }
    }

    /// Get all connected mesh peers.
    pub fn get_peers(&self) -> Vec<&MeshPeerInfo> {
        self.peers.values().collect()
    }

    /// Get the number of mesh peers.
    pub fn peer_count(&self) -> usize {
        self.peers.len()
    }

    /// Get peers that can bridge to Layer 1 (have internet connectivity).
    ///
    /// WHY: Bridge detection is critical for offline transaction flow. The
    /// native layer uses this to prioritize relay toward bridge peers so
    /// offline transactions reach Layer 1 faster.
    pub fn get_bridge_peers(&self) -> Vec<&MeshPeerInfo> {
        self.peers.values().filter(|p| p.has_internet).collect()
    }

    /// Check if a specific peer is known.
    pub fn has_peer(&self, peer_id: &MeshPeerId) -> bool {
        self.peers.contains_key(peer_id)
    }

    /// Get info about a specific peer.
    pub fn get_peer(&self, peer_id: &MeshPeerId) -> Option<&MeshPeerInfo> {
        self.peers.get(peer_id)
    }

    // ── Internal Helpers ────────────────────────────────────────────────

    /// Enforce the seen message cache size limit.
    ///
    /// WHY: When the cache is full, we clear half of it. This is a simple
    /// approach — a proper LRU would be better but adds complexity. Since
    /// messages older than max_message_age_secs are rejected anyway, clearing
    /// old entries is safe. The cost is a brief window where very recently
    /// cleared messages could be re-processed (harmless — they'll be deduped
    /// at the application layer).
    fn enforce_seen_cache_limit(&mut self) {
        if self.seen_messages.len() > self.config.max_seen_cache_size {
            // WHY: Clear half rather than one-at-a-time to amortize the cost.
            // HashSet doesn't support ordered eviction, so we just clear half
            // arbitrarily. This is acceptable because stale messages are also
            // rejected by timestamp check.
            let target = self.config.max_seen_cache_size / 2;
            let to_remove: Vec<[u8; 32]> = self
                .seen_messages
                .iter()
                .take(self.seen_messages.len() - target)
                .copied()
                .collect();
            for id in to_remove {
                self.seen_messages.remove(&id);
            }
            tracing::debug!(
                remaining = self.seen_messages.len(),
                "Pruned seen message cache"
            );
        }
    }

    /// Get the number of messages in the relay queue.
    pub fn relay_queue_len(&self) -> usize {
        self.relay_queue.len()
    }

    /// Get the number of seen message IDs cached.
    pub fn seen_cache_size(&self) -> usize {
        self.seen_messages.len()
    }

    /// Create a peer discovery beacon message.
    ///
    /// WHY: Beacons are broadcast periodically so nearby peers know about this
    /// node's existence and capabilities (especially bridge status).
    pub fn create_discovery_beacon(&self, timestamp: u64) -> Result<MeshMessage, NetworkError> {
        if !self.active {
            return Err(NetworkError::NotStarted);
        }

        // WHY: The beacon payload contains minimal info — just the peer's
        // internet status and transport capabilities. Keep it small because
        // beacons are sent frequently.
        let beacon_payload = DiscoveryBeacon {
            peer_id: self.local_peer_id,
            has_internet: self.has_internet,
            bluetooth_enabled: self.config.bluetooth_enabled,
            wifi_direct_enabled: self.config.wifi_direct_enabled,
            peer_count: self.peers.len() as u16,
        };

        let payload = bincode::serialize(&beacon_payload)
            .map_err(|e| NetworkError::Serialization(e.to_string()))?;

        Ok(MeshMessage::new(
            MeshMessageType::PeerDiscovery,
            // WHY: Discovery beacons use TTL of 2 — they only need to reach
            // immediate and one-hop neighbors. Higher TTL would flood the mesh
            // with beacon traffic.
            2,
            self.local_peer_id,
            payload,
            timestamp,
        ))
    }
}

// ============================================================================
// Discovery Beacon Payload
// ============================================================================

/// Payload for a peer discovery beacon.
///
/// WHY: Kept minimal (~70 bytes) because beacons are broadcast frequently
/// and must fit within BLE advertisement constraints.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiscoveryBeacon {
    /// The advertising peer's ID.
    pub peer_id: MeshPeerId,
    /// Whether this peer has internet (bridge capability).
    pub has_internet: bool,
    /// Whether Bluetooth LE is available.
    pub bluetooth_enabled: bool,
    /// Whether Wi-Fi Direct is available.
    pub wifi_direct_enabled: bool,
    /// Number of mesh peers this node is connected to.
    /// WHY: Helps new peers decide whether to connect — prefer peers with
    /// fewer connections for better mesh distribution.
    pub peer_count: u16,
}

// ============================================================================
// Offline Payment
// ============================================================================

/// An offline payment initiated via NFC and relayed via mesh.
///
/// WHY: Separate from a regular transaction because offline payments include
/// additional metadata needed for proper bridging: the NFC handshake ensures
/// both parties were physically present, and the local timestamp agreement
/// prevents replay attacks.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OfflinePayment {
    /// The signed transaction bytes (standard Gratia transaction format).
    pub transaction_bytes: Vec<u8>,

    /// NFC handshake proof — both devices signed a shared nonce during tap.
    /// WHY: Proves physical proximity at payment time. The bridge peer can
    /// verify this before forwarding to Layer 1, adding an anti-fraud layer.
    pub nfc_handshake_proof: Vec<u8>,

    /// Agreed timestamp between sender and receiver (Unix seconds).
    /// WHY: Both phones agree on a timestamp during NFC tap. This prevents
    /// a malicious party from creating backdated offline payments.
    pub agreed_timestamp: u64,

    /// Sender's mesh peer ID.
    pub sender: MeshPeerId,

    /// Receiver's mesh peer ID.
    pub receiver: MeshPeerId,
}

impl OfflinePayment {
    /// Serialize the offline payment for mesh transmission.
    pub fn to_bytes(&self) -> Result<Vec<u8>, NetworkError> {
        bincode::serialize(self).map_err(|e| NetworkError::Serialization(e.to_string()))
    }

    /// Deserialize an offline payment from mesh data.
    pub fn from_bytes(data: &[u8]) -> Result<Self, NetworkError> {
        bincode::deserialize(data).map_err(|e| NetworkError::Serialization(e.to_string()))
    }
}

// ============================================================================
// Hex encoding helper (avoid adding `hex` dependency)
// ============================================================================

mod hex {
    /// Encode bytes as a hex string.
    pub fn encode(bytes: &[u8]) -> String {
        bytes.iter().map(|b| format!("{:02x}", b)).collect()
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn test_peer_id(byte: u8) -> MeshPeerId {
        MeshPeerId([byte; 32])
    }

    fn test_config() -> MeshConfig {
        MeshConfig {
            max_mesh_peers: 4,
            max_seen_cache_size: 100,
            ..Default::default()
        }
    }

    fn now() -> u64 {
        // Fixed timestamp for deterministic tests
        1_700_000_000
    }

    // ── MeshMessage Tests ────────────────────────────────────────────────

    #[test]
    fn test_message_creation_and_id() {
        let source = test_peer_id(0xAA);
        let payload = b"test transaction data".to_vec();
        let ts = now();

        let msg = MeshMessage::new(MeshMessageType::Transaction, 5, source, payload.clone(), ts);

        assert_eq!(msg.ttl, 5);
        assert_eq!(msg.source, source);
        assert_eq!(msg.payload, payload);
        assert_eq!(msg.timestamp, ts);
        assert!(msg.verify_id());
    }

    #[test]
    fn test_message_id_deterministic() {
        let source = test_peer_id(0xBB);
        let payload = b"same payload".to_vec();
        let ts = now();

        let msg1 = MeshMessage::new(MeshMessageType::Transaction, 5, source, payload.clone(), ts);
        let msg2 = MeshMessage::new(MeshMessageType::Transaction, 3, source, payload, ts);

        // WHY: TTL is NOT part of the ID — same content from same source at same
        // time produces the same ID regardless of TTL. This enables deduplication
        // even when the same message arrives via paths with different hop counts.
        assert_eq!(msg1.id, msg2.id);
    }

    #[test]
    fn test_message_id_differs_for_different_content() {
        let source = test_peer_id(0xCC);
        let ts = now();

        let msg1 = MeshMessage::new(
            MeshMessageType::Transaction,
            5,
            source,
            b"payload A".to_vec(),
            ts,
        );
        let msg2 = MeshMessage::new(
            MeshMessageType::Transaction,
            5,
            source,
            b"payload B".to_vec(),
            ts,
        );

        assert_ne!(msg1.id, msg2.id);
    }

    #[test]
    fn test_message_verify_id_detects_tampering() {
        let source = test_peer_id(0xDD);
        let mut msg =
            MeshMessage::new(MeshMessageType::Transaction, 5, source, b"original".to_vec(), now());

        // Tamper with the payload
        msg.payload = b"tampered".to_vec();
        assert!(!msg.verify_id());
    }

    #[test]
    fn test_message_serialization_roundtrip() {
        let source = test_peer_id(0xEE);
        let msg = MeshMessage::new(
            MeshMessageType::OfflinePayment,
            3,
            source,
            b"payment data".to_vec(),
            now(),
        );

        let bytes = msg.to_bytes().unwrap();
        let decoded = MeshMessage::from_bytes(&bytes).unwrap();

        assert_eq!(decoded.id, msg.id);
        assert_eq!(decoded.msg_type, MeshMessageType::OfflinePayment);
        assert_eq!(decoded.ttl, 3);
        assert_eq!(decoded.source, source);
        assert_eq!(decoded.payload, b"payment data");
        assert_eq!(decoded.timestamp, msg.timestamp);
    }

    // ── MeshTransport Tests ──────────────────────────────────────────────

    #[test]
    fn test_transport_start_stop() {
        let local = test_peer_id(0x01);
        let mut transport = MeshTransport::new(test_config(), local);

        assert!(!transport.is_active());
        transport.start().unwrap();
        assert!(transport.is_active());
        transport.stop();
        assert!(!transport.is_active());
    }

    #[test]
    fn test_broadcast_requires_active() {
        let local = test_peer_id(0x01);
        let mut transport = MeshTransport::new(test_config(), local);

        let result = transport.broadcast(MeshMessageType::Transaction, b"tx".to_vec(), now());
        assert!(matches!(result, Err(NetworkError::NotStarted)));
    }

    #[test]
    fn test_broadcast_creates_message() {
        let local = test_peer_id(0x01);
        let mut transport = MeshTransport::new(test_config(), local);
        transport.start().unwrap();

        let id = transport
            .broadcast(MeshMessageType::Transaction, b"tx data".to_vec(), now())
            .unwrap();

        // Message should be in relay queue
        assert_eq!(transport.relay_queue_len(), 1);
        // Message should be in seen cache (so we don't process our own broadcast)
        assert!(transport.seen_cache_size() > 0);

        // ID should be deterministic
        let expected_id = MeshMessage::compute_id(&local, b"tx data", now());
        assert_eq!(id, expected_id);
    }

    #[test]
    fn test_broadcast_rejects_oversized_message() {
        let local = test_peer_id(0x01);
        let config = MeshConfig {
            max_mesh_message_size: 10,
            ..test_config()
        };
        let mut transport = MeshTransport::new(config, local);
        transport.start().unwrap();

        let result = transport.broadcast(
            MeshMessageType::Transaction,
            vec![0u8; 100], // Way over the 10-byte limit
            now(),
        );
        assert!(matches!(result, Err(NetworkError::MessageTooLarge { .. })));
    }

    // ── Receive Message Tests ────────────────────────────────────────────

    #[test]
    fn test_receive_delivers_new_message() {
        let local = test_peer_id(0x01);
        let remote = test_peer_id(0x02);
        let mut transport = MeshTransport::new(test_config(), local);
        transport.start().unwrap();

        let msg = MeshMessage::new(
            MeshMessageType::Transaction,
            3,
            remote,
            b"tx data".to_vec(),
            now(),
        );

        let action = transport.receive_message(msg, now()).unwrap();
        assert_eq!(action, MeshAction::Deliver(MeshMessageType::Transaction));

        // Should be queued for relay with decremented TTL
        assert_eq!(transport.relay_queue_len(), 1);
    }

    #[test]
    fn test_receive_deduplicates() {
        let local = test_peer_id(0x01);
        let remote = test_peer_id(0x02);
        let mut transport = MeshTransport::new(test_config(), local);
        transport.start().unwrap();

        let msg = MeshMessage::new(
            MeshMessageType::Transaction,
            3,
            remote,
            b"tx data".to_vec(),
            now(),
        );

        // First receive: deliver
        let action = transport.receive_message(msg.clone(), now()).unwrap();
        assert_eq!(action, MeshAction::Deliver(MeshMessageType::Transaction));

        // Second receive: duplicate
        let action = transport.receive_message(msg, now()).unwrap();
        assert_eq!(action, MeshAction::Duplicate);
    }

    #[test]
    fn test_receive_ttl_expiry() {
        let local = test_peer_id(0x01);
        let remote = test_peer_id(0x02);
        let mut transport = MeshTransport::new(test_config(), local);
        transport.start().unwrap();

        // TTL = 0 means the message has been relayed max_ttl times already
        let msg = MeshMessage::new(
            MeshMessageType::Transaction,
            0,
            remote,
            b"tx data".to_vec(),
            now(),
        );

        let action = transport.receive_message(msg, now()).unwrap();
        assert_eq!(action, MeshAction::Expired);
    }

    #[test]
    fn test_receive_stale_message() {
        let local = test_peer_id(0x01);
        let remote = test_peer_id(0x02);
        let mut transport = MeshTransport::new(test_config(), local);
        transport.start().unwrap();

        // Message from 10 minutes ago (default max_message_age_secs = 300)
        let old_time = now() - 600;
        let msg = MeshMessage::new(
            MeshMessageType::Transaction,
            3,
            remote,
            b"old tx".to_vec(),
            old_time,
        );

        let action = transport.receive_message(msg, now()).unwrap();
        assert_eq!(action, MeshAction::Stale);
    }

    #[test]
    fn test_receive_invalid_id() {
        let local = test_peer_id(0x01);
        let remote = test_peer_id(0x02);
        let mut transport = MeshTransport::new(test_config(), local);
        transport.start().unwrap();

        let mut msg = MeshMessage::new(
            MeshMessageType::Transaction,
            3,
            remote,
            b"tx data".to_vec(),
            now(),
        );

        // Tamper with the payload
        msg.payload = b"tampered data".to_vec();

        let action = transport.receive_message(msg, now()).unwrap();
        assert_eq!(action, MeshAction::InvalidId);
    }

    #[test]
    fn test_receive_ttl_decremented_in_relay() {
        let local = test_peer_id(0x01);
        let remote = test_peer_id(0x02);
        let mut transport = MeshTransport::new(test_config(), local);
        transport.start().unwrap();

        // Add a peer so drain_relay_queue has someone to send to
        transport.add_peer(MeshPeerInfo {
            peer_id: test_peer_id(0x03),
            transport: MeshTransportType::BluetoothLE,
            signal_strength: Some(-50),
            last_seen: now(),
            hop_count: 1,
            has_internet: false,
        });

        let msg = MeshMessage::new(
            MeshMessageType::Transaction,
            4,
            remote,
            b"tx".to_vec(),
            now(),
        );

        transport.receive_message(msg, now()).unwrap();

        let relayed = transport.drain_relay_queue();
        assert_eq!(relayed.len(), 1);
        // TTL should be decremented from 4 to 3
        assert_eq!(relayed[0].1.ttl, 3);
    }

    // ── Peer Management Tests ────────────────────────────────────────────

    #[test]
    fn test_add_and_remove_peer() {
        let local = test_peer_id(0x01);
        let mut transport = MeshTransport::new(test_config(), local);

        let peer_info = MeshPeerInfo {
            peer_id: test_peer_id(0x02),
            transport: MeshTransportType::BluetoothLE,
            signal_strength: Some(-60),
            last_seen: now(),
            hop_count: 1,
            has_internet: false,
        };

        transport.add_peer(peer_info);
        assert_eq!(transport.peer_count(), 1);
        assert!(transport.has_peer(&test_peer_id(0x02)));

        transport.remove_peer(&test_peer_id(0x02));
        assert_eq!(transport.peer_count(), 0);
        assert!(!transport.has_peer(&test_peer_id(0x02)));
    }

    #[test]
    fn test_add_self_is_ignored() {
        let local = test_peer_id(0x01);
        let mut transport = MeshTransport::new(test_config(), local);

        transport.add_peer(MeshPeerInfo {
            peer_id: local,
            transport: MeshTransportType::BluetoothLE,
            signal_strength: Some(-30),
            last_seen: now(),
            hop_count: 0,
            has_internet: true,
        });

        // Should not add ourselves
        assert_eq!(transport.peer_count(), 0);
    }

    #[test]
    fn test_peer_limit_evicts_stalest() {
        let local = test_peer_id(0x01);
        let config = MeshConfig {
            max_mesh_peers: 2,
            ..test_config()
        };
        let mut transport = MeshTransport::new(config, local);

        // Add two peers (at limit)
        transport.add_peer(MeshPeerInfo {
            peer_id: test_peer_id(0x02),
            transport: MeshTransportType::BluetoothLE,
            signal_strength: None,
            last_seen: now() - 100, // Older
            hop_count: 1,
            has_internet: false,
        });
        transport.add_peer(MeshPeerInfo {
            peer_id: test_peer_id(0x03),
            transport: MeshTransportType::BluetoothLE,
            signal_strength: None,
            last_seen: now(), // Newer
            hop_count: 1,
            has_internet: false,
        });
        assert_eq!(transport.peer_count(), 2);

        // Add a third — should evict peer 0x02 (oldest)
        transport.add_peer(MeshPeerInfo {
            peer_id: test_peer_id(0x04),
            transport: MeshTransportType::WifiDirect,
            signal_strength: None,
            last_seen: now(),
            hop_count: 1,
            has_internet: false,
        });

        assert_eq!(transport.peer_count(), 2);
        assert!(!transport.has_peer(&test_peer_id(0x02))); // Evicted
        assert!(transport.has_peer(&test_peer_id(0x03)));
        assert!(transport.has_peer(&test_peer_id(0x04)));
    }

    #[test]
    fn test_bridge_peers_not_evicted() {
        let local = test_peer_id(0x01);
        let config = MeshConfig {
            max_mesh_peers: 2,
            ..test_config()
        };
        let mut transport = MeshTransport::new(config, local);

        // Fill with bridge peers
        transport.add_peer(MeshPeerInfo {
            peer_id: test_peer_id(0x02),
            transport: MeshTransportType::BluetoothLE,
            signal_strength: None,
            last_seen: now() - 100,
            hop_count: 1,
            has_internet: true, // Bridge
        });
        transport.add_peer(MeshPeerInfo {
            peer_id: test_peer_id(0x03),
            transport: MeshTransportType::BluetoothLE,
            signal_strength: None,
            last_seen: now(),
            hop_count: 1,
            has_internet: true, // Bridge
        });

        // Try to add a non-bridge peer — should fail (all existing are bridges)
        transport.add_peer(MeshPeerInfo {
            peer_id: test_peer_id(0x04),
            transport: MeshTransportType::WifiDirect,
            signal_strength: None,
            last_seen: now(),
            hop_count: 1,
            has_internet: false,
        });

        // All bridge peers preserved, new peer rejected
        assert_eq!(transport.peer_count(), 2);
        assert!(transport.has_peer(&test_peer_id(0x02)));
        assert!(transport.has_peer(&test_peer_id(0x03)));
        assert!(!transport.has_peer(&test_peer_id(0x04)));
    }

    #[test]
    fn test_remove_stale_peers() {
        let local = test_peer_id(0x01);
        let mut transport = MeshTransport::new(test_config(), local);

        transport.add_peer(MeshPeerInfo {
            peer_id: test_peer_id(0x02),
            transport: MeshTransportType::BluetoothLE,
            signal_strength: None,
            last_seen: now() - 200, // 200 seconds ago (> 120s timeout)
            hop_count: 1,
            has_internet: false,
        });
        transport.add_peer(MeshPeerInfo {
            peer_id: test_peer_id(0x03),
            transport: MeshTransportType::BluetoothLE,
            signal_strength: None,
            last_seen: now() - 10, // 10 seconds ago (fresh)
            hop_count: 1,
            has_internet: false,
        });

        transport.remove_stale_peers(now());

        assert_eq!(transport.peer_count(), 1);
        assert!(!transport.has_peer(&test_peer_id(0x02))); // Stale, removed
        assert!(transport.has_peer(&test_peer_id(0x03))); // Fresh, kept
    }

    #[test]
    fn test_get_bridge_peers() {
        let local = test_peer_id(0x01);
        let mut transport = MeshTransport::new(test_config(), local);

        transport.add_peer(MeshPeerInfo {
            peer_id: test_peer_id(0x02),
            transport: MeshTransportType::BluetoothLE,
            signal_strength: None,
            last_seen: now(),
            hop_count: 1,
            has_internet: false,
        });
        transport.add_peer(MeshPeerInfo {
            peer_id: test_peer_id(0x03),
            transport: MeshTransportType::WifiDirect,
            signal_strength: None,
            last_seen: now(),
            hop_count: 1,
            has_internet: true, // Bridge
        });
        transport.add_peer(MeshPeerInfo {
            peer_id: test_peer_id(0x04),
            transport: MeshTransportType::Both,
            signal_strength: None,
            last_seen: now(),
            hop_count: 2,
            has_internet: true, // Bridge
        });

        let bridges = transport.get_bridge_peers();
        assert_eq!(bridges.len(), 2);
        assert!(bridges.iter().all(|p| p.has_internet));
    }

    // ── Relay Queue Tests ────────────────────────────────────────────────

    #[test]
    fn test_drain_relay_queue_excludes_source() {
        let local = test_peer_id(0x01);
        let source = test_peer_id(0x02);
        let other = test_peer_id(0x03);
        let mut transport = MeshTransport::new(test_config(), local);
        transport.start().unwrap();

        // Add source and another peer
        transport.add_peer(MeshPeerInfo {
            peer_id: source,
            transport: MeshTransportType::BluetoothLE,
            signal_strength: None,
            last_seen: now(),
            hop_count: 1,
            has_internet: false,
        });
        transport.add_peer(MeshPeerInfo {
            peer_id: other,
            transport: MeshTransportType::BluetoothLE,
            signal_strength: None,
            last_seen: now(),
            hop_count: 1,
            has_internet: false,
        });

        // Receive a message from source
        let msg = MeshMessage::new(
            MeshMessageType::Transaction,
            3,
            source,
            b"tx".to_vec(),
            now(),
        );
        transport.receive_message(msg, now()).unwrap();

        let relayed = transport.drain_relay_queue();
        // Should only relay to `other`, not back to `source`
        assert_eq!(relayed.len(), 1);
        assert_eq!(relayed[0].0, other);
    }

    #[test]
    fn test_drain_relay_queue_prioritizes_bridge_peers() {
        let local = test_peer_id(0x01);
        let mut transport = MeshTransport::new(test_config(), local);
        transport.start().unwrap();

        // Add non-bridge peer first
        transport.add_peer(MeshPeerInfo {
            peer_id: test_peer_id(0x02),
            transport: MeshTransportType::BluetoothLE,
            signal_strength: Some(-70),
            last_seen: now(),
            hop_count: 1,
            has_internet: false,
        });
        // Add bridge peer second
        transport.add_peer(MeshPeerInfo {
            peer_id: test_peer_id(0x03),
            transport: MeshTransportType::WifiDirect,
            signal_strength: Some(-80),
            last_seen: now(),
            hop_count: 1,
            has_internet: true,
        });

        transport
            .broadcast(MeshMessageType::Transaction, b"tx".to_vec(), now())
            .unwrap();

        let relayed = transport.drain_relay_queue();
        assert_eq!(relayed.len(), 2);
        // Bridge peer should be first despite weaker signal
        assert_eq!(relayed[0].0, test_peer_id(0x03));
        assert_eq!(relayed[1].0, test_peer_id(0x02));
    }

    #[test]
    fn test_relay_queue_empty_after_drain() {
        let local = test_peer_id(0x01);
        let mut transport = MeshTransport::new(test_config(), local);
        transport.start().unwrap();

        transport
            .broadcast(MeshMessageType::Transaction, b"tx".to_vec(), now())
            .unwrap();
        assert_eq!(transport.relay_queue_len(), 1);

        let _ = transport.drain_relay_queue();
        assert_eq!(transport.relay_queue_len(), 0);
    }

    // ── Multi-Hop Relay Test ─────────────────────────────────────────────

    #[test]
    fn test_multi_hop_relay_a_to_b_to_c() {
        // Simulate: Node A creates a transaction. Node B receives and relays.
        // Node C (with internet) receives and would bridge to Layer 1.

        let node_a = test_peer_id(0xAA);
        let node_b = test_peer_id(0xBB);
        let node_c = test_peer_id(0xCC);

        // === Node A: Create and broadcast ===
        let mut transport_a = MeshTransport::new(test_config(), node_a);
        transport_a.start().unwrap();
        transport_a.add_peer(MeshPeerInfo {
            peer_id: node_b,
            transport: MeshTransportType::BluetoothLE,
            signal_strength: Some(-50),
            last_seen: now(),
            hop_count: 1,
            has_internet: false,
        });

        let msg_id = transport_a
            .broadcast(
                MeshMessageType::Transaction,
                b"offline tx payload".to_vec(),
                now(),
            )
            .unwrap();

        let outbound_a = transport_a.drain_relay_queue();
        assert_eq!(outbound_a.len(), 1);
        assert_eq!(outbound_a[0].0, node_b);
        let msg_for_b = outbound_a[0].1.clone();
        assert_eq!(msg_for_b.ttl, 5); // max_ttl = 5
        assert_eq!(msg_for_b.id, msg_id);

        // === Node B: Receive from A, relay to C ===
        let mut transport_b = MeshTransport::new(test_config(), node_b);
        transport_b.start().unwrap();
        transport_b.add_peer(MeshPeerInfo {
            peer_id: node_a,
            transport: MeshTransportType::BluetoothLE,
            signal_strength: Some(-50),
            last_seen: now(),
            hop_count: 1,
            has_internet: false,
        });
        transport_b.add_peer(MeshPeerInfo {
            peer_id: node_c,
            transport: MeshTransportType::BluetoothLE,
            signal_strength: Some(-60),
            last_seen: now(),
            hop_count: 1,
            has_internet: true, // Bridge peer!
        });

        let action = transport_b.receive_message(msg_for_b, now()).unwrap();
        assert_eq!(action, MeshAction::Deliver(MeshMessageType::Transaction));

        let outbound_b = transport_b.drain_relay_queue();
        // Should relay to C (bridge) but NOT back to A (source)
        assert_eq!(outbound_b.len(), 1);
        assert_eq!(outbound_b[0].0, node_c);
        let msg_for_c = outbound_b[0].1.clone();
        assert_eq!(msg_for_c.ttl, 4); // Decremented from 5 to 4

        // === Node C: Receive from B — this is the bridge peer ===
        let mut transport_c = MeshTransport::new(test_config(), node_c);
        transport_c.start().unwrap();
        transport_c.set_internet_available(true);
        transport_c.add_peer(MeshPeerInfo {
            peer_id: node_b,
            transport: MeshTransportType::BluetoothLE,
            signal_strength: Some(-60),
            last_seen: now(),
            hop_count: 1,
            has_internet: false,
        });

        let action = transport_c.receive_message(msg_for_c, now()).unwrap();
        assert_eq!(action, MeshAction::Deliver(MeshMessageType::Transaction));

        // Node C has internet — in production it would bridge this to Layer 1
        assert!(transport_c.is_bridge());

        // Verify the message source is still Node A (preserved through hops)
        // (The msg was already consumed, but we verified it delivered)
    }

    // ── Offline Payment Test ─────────────────────────────────────────────

    #[test]
    fn test_offline_payment_flow() {
        let sender = test_peer_id(0xAA);
        let receiver = test_peer_id(0xBB);
        let bridge = test_peer_id(0xCC);

        // Step 1: Create offline payment (NFC tap happened, both signed)
        let payment = OfflinePayment {
            transaction_bytes: b"signed-tx-bytes-here".to_vec(),
            nfc_handshake_proof: b"nfc-proof-data".to_vec(),
            agreed_timestamp: now(),
            sender,
            receiver,
        };

        let payment_bytes = payment.to_bytes().unwrap();

        // Step 2: Sender broadcasts via mesh
        let mut transport_sender = MeshTransport::new(test_config(), sender);
        transport_sender.start().unwrap();
        transport_sender.add_peer(MeshPeerInfo {
            peer_id: receiver,
            transport: MeshTransportType::BluetoothLE,
            signal_strength: Some(-30),
            last_seen: now(),
            hop_count: 1,
            has_internet: false,
        });

        let msg_id = transport_sender
            .broadcast(MeshMessageType::OfflinePayment, payment_bytes.clone(), now())
            .unwrap();
        assert_ne!(msg_id, [0u8; 32]);

        let outbound = transport_sender.drain_relay_queue();
        assert_eq!(outbound.len(), 1);

        // Step 3: Receiver gets the message and relays toward bridge
        let mut transport_receiver = MeshTransport::new(test_config(), receiver);
        transport_receiver.start().unwrap();
        transport_receiver.add_peer(MeshPeerInfo {
            peer_id: bridge,
            transport: MeshTransportType::WifiDirect,
            signal_strength: Some(-40),
            last_seen: now(),
            hop_count: 1,
            has_internet: true,
        });

        let action = transport_receiver
            .receive_message(outbound[0].1.clone(), now())
            .unwrap();
        assert_eq!(
            action,
            MeshAction::Deliver(MeshMessageType::OfflinePayment)
        );

        // Step 4: Bridge peer receives and would forward to Layer 1
        let outbound_recv = transport_receiver.drain_relay_queue();
        assert_eq!(outbound_recv.len(), 1);
        assert_eq!(outbound_recv[0].0, bridge);

        // Verify the payment can be deserialized from the relayed message
        let decoded_payment = OfflinePayment::from_bytes(&outbound_recv[0].1.payload).unwrap();
        assert_eq!(decoded_payment.sender, sender);
        assert_eq!(decoded_payment.receiver, receiver);
        assert_eq!(decoded_payment.transaction_bytes, b"signed-tx-bytes-here");
        assert_eq!(decoded_payment.nfc_handshake_proof, b"nfc-proof-data");
    }

    // ── Seen Cache Limit Test ────────────────────────────────────────────

    #[test]
    fn test_seen_cache_prunes_when_full() {
        let local = test_peer_id(0x01);
        let config = MeshConfig {
            max_seen_cache_size: 10,
            ..test_config()
        };
        let mut transport = MeshTransport::new(config, local);
        transport.start().unwrap();

        // Broadcast 12 messages to exceed cache of 10
        for i in 0u64..12 {
            let payload = i.to_be_bytes().to_vec();
            transport
                .broadcast(MeshMessageType::Transaction, payload, now() + i)
                .unwrap();
        }

        // Cache should have been pruned to ~5 (half of max 10)
        // It might be slightly more because we added entries after pruning
        assert!(transport.seen_cache_size() <= 10);
    }

    // ── Config Validation Tests ──────────────────────────────────────────

    #[test]
    fn test_config_validation() {
        let valid = MeshConfig::default();
        assert!(valid.validate().is_ok());

        let no_transport = MeshConfig {
            bluetooth_enabled: false,
            wifi_direct_enabled: false,
            ..Default::default()
        };
        assert!(no_transport.validate().is_err());

        let zero_peers = MeshConfig {
            max_mesh_peers: 0,
            ..Default::default()
        };
        assert!(zero_peers.validate().is_err());

        let zero_ttl = MeshConfig {
            max_ttl: 0,
            ..Default::default()
        };
        assert!(zero_ttl.validate().is_err());
    }

    // ── Discovery Beacon Test ────────────────────────────────────────────

    #[test]
    fn test_discovery_beacon_creation() {
        let local = test_peer_id(0x01);
        let mut transport = MeshTransport::new(test_config(), local);
        transport.start().unwrap();
        transport.set_internet_available(true);

        let beacon = transport.create_discovery_beacon(now()).unwrap();
        assert_eq!(beacon.msg_type, MeshMessageType::PeerDiscovery);
        assert_eq!(beacon.ttl, 2); // Beacons have low TTL
        assert_eq!(beacon.source, local);

        // Decode the beacon payload
        let decoded: DiscoveryBeacon = bincode::deserialize(&beacon.payload).unwrap();
        assert_eq!(decoded.peer_id, local);
        assert!(decoded.has_internet);
        assert!(decoded.bluetooth_enabled);
    }

    #[test]
    fn test_discovery_beacon_requires_active() {
        let local = test_peer_id(0x01);
        let transport = MeshTransport::new(test_config(), local);

        let result = transport.create_discovery_beacon(now());
        assert!(matches!(result, Err(NetworkError::NotStarted)));
    }

    // ── Internet / Bridge Tests ──────────────────────────────────────────

    #[test]
    fn test_set_internet_available() {
        let local = test_peer_id(0x01);
        let mut transport = MeshTransport::new(test_config(), local);

        assert!(!transport.is_bridge());
        transport.set_internet_available(true);
        assert!(transport.is_bridge());
        transport.set_internet_available(false);
        assert!(!transport.is_bridge());
    }

    // ── Peer Update Test ─────────────────────────────────────────────────

    #[test]
    fn test_add_peer_updates_existing() {
        let local = test_peer_id(0x01);
        let mut transport = MeshTransport::new(test_config(), local);

        let peer_id = test_peer_id(0x02);

        // Add initially without internet
        transport.add_peer(MeshPeerInfo {
            peer_id,
            transport: MeshTransportType::BluetoothLE,
            signal_strength: Some(-70),
            last_seen: now(),
            hop_count: 1,
            has_internet: false,
        });

        assert!(!transport.get_peer(&peer_id).unwrap().has_internet);

        // Update with internet
        transport.add_peer(MeshPeerInfo {
            peer_id,
            transport: MeshTransportType::Both,
            signal_strength: Some(-50),
            last_seen: now() + 10,
            hop_count: 1,
            has_internet: true,
        });

        // Should be updated, not duplicated
        assert_eq!(transport.peer_count(), 1);
        let info = transport.get_peer(&peer_id).unwrap();
        assert!(info.has_internet);
        assert_eq!(info.transport, MeshTransportType::Both);
        assert_eq!(info.signal_strength, Some(-50));
    }
}
