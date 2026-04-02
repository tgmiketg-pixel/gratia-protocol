//! Network transport layer built on libp2p.
//!
//! Configures QUIC transport with Noise encryption for the Gratia P2P network.
//! In modern libp2p (0.54+), transport setup is handled through SwarmBuilder.

use std::collections::HashSet;
use std::time::Duration;

use libp2p::Multiaddr;
use serde::{Deserialize, Serialize};

use crate::mesh::MeshConfig;

/// Configuration for the network transport layer.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TransportConfig {
    /// Addresses this node should listen on.
    /// Default: ["/ip4/0.0.0.0/udp/0/quic-v1"]
    pub listen_addresses: Vec<String>,

    /// Bootstrap peer addresses for initial network entry.
    pub bootstrap_peers: Vec<String>,

    /// Maximum number of simultaneous peer connections.
    /// WHY: Mobile devices have limited memory and bandwidth. 50 peers balances
    /// network connectivity against resource constraints on $50 phones.
    pub max_peers: usize,

    /// Maximum number of inbound connections.
    /// WHY: Prevents resource exhaustion from excessive inbound connections.
    /// Set lower than max_peers to ensure we can always make outbound connections.
    pub max_inbound: usize,

    /// Maximum number of outbound connections.
    pub max_outbound: usize,

    /// Idle connection timeout.
    /// WHY: On mobile, stale connections waste battery. 5 minutes balances
    /// reconnection overhead against idle resource usage.
    pub idle_timeout_secs: u64,

    /// Interval between keep-alive pings (seconds).
    /// WHY: Mobile connections are unstable (Wi-Fi to cellular handoffs).
    /// Regular pings detect dead connections quickly.
    pub keepalive_interval_secs: u64,

    /// Skip QUIC transport entirely, using TCP only.
    /// WHY: Samsung budget phones without a SIM card (e.g., A06 Indian variant)
    /// have broken UDP routing — ICMP ping works but app-level UDP sockets fail.
    /// When this is true, the SwarmBuilder skips `with_quic_config()` so the node
    /// connects via TCP only, avoiding the 30-second QUIC timeout before fallback.
    pub tcp_only: bool,

    /// Mesh layer (Layer 0) configuration.
    /// WHY: Optional because mesh transport (BLE/Wi-Fi Direct) is only available
    /// on devices with the necessary hardware. Desktop archive nodes and bootstrap
    /// servers do not participate in the mesh layer.
    pub mesh: Option<MeshConfig>,
}

impl Default for TransportConfig {
    fn default() -> Self {
        TransportConfig {
            // WHY: QUIC over UDP — better than TCP for mobile networks because
            // QUIC handles connection migration when switching Wi-Fi <-> cellular.
            listen_addresses: vec!["/ip4/0.0.0.0/udp/0/quic-v1".to_string()],
            bootstrap_peers: Vec::new(),
            max_peers: 50,
            // WHY: Reserve headroom for outbound connections by capping inbound at 30.
            max_inbound: 30,
            max_outbound: 20,
            // WHY: 60 seconds idle timeout gives connections more time to survive
            // Samsung's aggressive network management (Doze, Adaptive Battery).
            // The 30s setting was causing connections to drop prematurely when
            // Samsung buffered UDP packets during brief power-save windows.
            // 60s is still short enough to detect genuinely dead peers quickly.
            idle_timeout_secs: 60,
            // WHY: 15 seconds keepalive — must be SHORTER than idle_timeout to
            // prevent the connection from being considered idle. Samsung's network
            // stack may close NAT mappings for UDP sockets that haven't sent
            // traffic in ~30s. 15s keepalive ensures regular traffic flows,
            // keeping the NAT pinhole open and the connection alive.
            keepalive_interval_secs: 15,
            tcp_only: false,
            // WHY: Mesh is None by default — enabled explicitly on mobile devices
            // that have BLE/Wi-Fi Direct hardware. Bootstrap servers and archive
            // nodes leave this as None.
            mesh: None,
        }
    }
}

impl TransportConfig {
    /// Create a transport config with mesh layer enabled (for mobile devices).
    pub fn with_mesh(mut self, mesh_config: MeshConfig) -> Self {
        self.mesh = Some(mesh_config);
        self
    }

    /// Parse listen addresses into libp2p Multiaddr values.
    /// Returns only the addresses that parse successfully, logging warnings for failures.
    pub fn parsed_listen_addresses(&self) -> Vec<Multiaddr> {
        self.listen_addresses
            .iter()
            .filter_map(|addr| {
                addr.parse::<Multiaddr>()
                    .map_err(|e| {
                        tracing::warn!("Invalid listen address '{}': {}", addr, e);
                        e
                    })
                    .ok()
            })
            .collect()
    }

    /// Parse bootstrap peer addresses into libp2p Multiaddr values.
    pub fn parsed_bootstrap_peers(&self) -> Vec<Multiaddr> {
        self.bootstrap_peers
            .iter()
            .filter_map(|addr| {
                addr.parse::<Multiaddr>()
                    .map_err(|e| {
                        tracing::warn!("Invalid bootstrap peer address '{}': {}", addr, e);
                        e
                    })
                    .ok()
            })
            .collect()
    }

    /// Validate the transport configuration.
    pub fn validate(&self) -> Result<(), String> {
        if self.listen_addresses.is_empty() {
            return Err("At least one listen address is required".to_string());
        }
        if self.max_peers == 0 {
            return Err("max_peers must be greater than 0".to_string());
        }
        if self.max_inbound + self.max_outbound > self.max_peers {
            return Err(format!(
                "max_inbound ({}) + max_outbound ({}) exceeds max_peers ({})",
                self.max_inbound, self.max_outbound, self.max_peers
            ));
        }
        // Validate mesh config if present
        if let Some(ref mesh) = self.mesh {
            mesh.validate()?;
        }
        Ok(())
    }
}

/// Tracks active peer connections and enforces limits.
#[derive(Debug)]
pub struct ConnectionManager {
    config: TransportConfig,
    connected_peers: HashSet<libp2p::PeerId>,
    inbound_count: usize,
    outbound_count: usize,
}

impl ConnectionManager {
    pub fn new(config: TransportConfig) -> Self {
        ConnectionManager {
            config,
            connected_peers: HashSet::new(),
            inbound_count: 0,
            outbound_count: 0,
        }
    }

    /// Check if a new inbound connection can be accepted.
    pub fn can_accept_inbound(&self) -> bool {
        self.inbound_count < self.config.max_inbound
            && self.connected_peers.len() < self.config.max_peers
    }

    /// Check if a new outbound connection can be initiated.
    pub fn can_initiate_outbound(&self) -> bool {
        self.outbound_count < self.config.max_outbound
            && self.connected_peers.len() < self.config.max_peers
    }

    /// Register a new inbound connection.
    /// Returns false if the connection limit would be exceeded.
    pub fn register_inbound(&mut self, peer_id: libp2p::PeerId) -> bool {
        if !self.can_accept_inbound() {
            return false;
        }
        // WHY: Only count as new if the peer wasn't already connected.
        // HashSet::insert returns true if the value was inserted (new peer).
        // Without this check, duplicate PeerConnected events for the same
        // peer would inflate both inbound_count and live_peer_count.
        let is_new = self.connected_peers.insert(peer_id);
        if is_new {
            self.inbound_count += 1;
        }
        is_new
    }

    /// Register a new outbound connection.
    /// Returns false if the connection limit would be exceeded.
    pub fn register_outbound(&mut self, peer_id: libp2p::PeerId) -> bool {
        if !self.can_initiate_outbound() {
            return false;
        }
        let is_new = self.connected_peers.insert(peer_id);
        if is_new {
            self.outbound_count += 1;
        }
        is_new
    }

    /// Remove a disconnected peer.
    /// WHY: `is_inbound` hint from the connect event. If unknown, defaults to
    /// decrementing inbound (conservative — won't block outbound slots).
    pub fn remove_peer(&mut self, peer_id: &libp2p::PeerId, is_inbound: bool) {
        if self.connected_peers.remove(peer_id) {
            if is_inbound {
                self.inbound_count = self.inbound_count.saturating_sub(1);
            } else {
                self.outbound_count = self.outbound_count.saturating_sub(1);
            }
        }
    }

    /// Number of currently connected peers.
    pub fn peer_count(&self) -> usize {
        self.connected_peers.len()
    }

    /// Whether a specific peer is connected.
    pub fn is_connected(&self, peer_id: &libp2p::PeerId) -> bool {
        self.connected_peers.contains(peer_id)
    }

    /// Get all connected peer IDs.
    pub fn connected_peers(&self) -> &HashSet<libp2p::PeerId> {
        &self.connected_peers
    }

    /// Get the idle connection timeout as a Duration.
    pub fn idle_timeout(&self) -> Duration {
        Duration::from_secs(self.config.idle_timeout_secs)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_transport_config() {
        let config = TransportConfig::default();
        assert_eq!(config.max_peers, 50);
        assert_eq!(config.max_inbound, 30);
        assert_eq!(config.max_outbound, 20);
        assert!(config.validate().is_ok());
    }

    #[test]
    fn test_config_validation_empty_listen() {
        let config = TransportConfig {
            listen_addresses: vec![],
            ..Default::default()
        };
        assert!(config.validate().is_err());
    }

    #[test]
    fn test_config_validation_limits_exceeded() {
        let config = TransportConfig {
            max_peers: 10,
            max_inbound: 8,
            max_outbound: 8,
            ..Default::default()
        };
        assert!(config.validate().is_err());
    }

    #[test]
    fn test_parse_listen_addresses() {
        let config = TransportConfig::default();
        let addrs = config.parsed_listen_addresses();
        assert_eq!(addrs.len(), 1);
    }

    #[test]
    fn test_connection_manager_limits() {
        let config = TransportConfig {
            max_peers: 3,
            max_inbound: 2,
            max_outbound: 1,
            ..Default::default()
        };
        let mut cm = ConnectionManager::new(config);

        assert!(cm.can_accept_inbound());
        assert!(cm.can_initiate_outbound());

        // Fill outbound slot
        let peer1 = libp2p::PeerId::random();
        assert!(cm.register_outbound(peer1));
        assert!(!cm.can_initiate_outbound()); // outbound full

        // Fill inbound slots
        let peer2 = libp2p::PeerId::random();
        assert!(cm.register_inbound(peer2));
        let peer3 = libp2p::PeerId::random();
        assert!(cm.register_inbound(peer3));

        // Now at max_peers (3)
        assert!(!cm.can_accept_inbound());
        assert_eq!(cm.peer_count(), 3);

        // Remove one
        cm.remove_peer(&peer2, true);
        assert_eq!(cm.peer_count(), 2);
        assert!(cm.can_accept_inbound());
    }

    #[test]
    fn test_connection_manager_is_connected() {
        let config = TransportConfig::default();
        let mut cm = ConnectionManager::new(config);
        let peer = libp2p::PeerId::random();

        assert!(!cm.is_connected(&peer));
        cm.register_inbound(peer);
        assert!(cm.is_connected(&peer));
    }
}
