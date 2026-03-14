//! Peer discovery via Kademlia DHT.
//!
//! Handles finding and registering peers on the Gratia network using
//! libp2p's Kademlia distributed hash table implementation.

use std::collections::HashMap;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use gratia_core::types::NodeId;

/// Record of a known peer on the network, stored in the discovery layer.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PeerRecord {
    /// Gratia-level node identity (derived from Ed25519 public key).
    pub node_id: NodeId,
    /// libp2p peer ID (derived from the libp2p identity keypair).
    pub peer_id_bytes: Vec<u8>,
    /// Multiaddresses where this peer can be reached.
    pub addresses: Vec<String>,
    /// Composite Presence Score (40-100). Only affects block production
    /// selection probability, NOT mining rewards.
    pub presence_score: u8,
    /// When this peer record was last updated.
    pub last_seen: DateTime<Utc>,
    /// Geographic shard this peer belongs to.
    pub shard_id: u16,
    /// Whether this peer is currently mining.
    pub is_mining: bool,
}

impl PeerRecord {
    /// Check if this peer record is stale (not seen recently).
    /// A record is considered stale after 10 minutes without updates.
    /// WHY: 10 minutes — mobile peers frequently go offline (subway, airplane mode).
    /// Aggressive pruning would cause churn; too lenient wastes routing table slots.
    pub fn is_stale(&self, now: DateTime<Utc>) -> bool {
        const STALE_THRESHOLD_SECS: i64 = 600; // 10 minutes
        (now - self.last_seen).num_seconds() > STALE_THRESHOLD_SECS
    }
}

/// Manages peer discovery through Kademlia DHT.
///
/// This struct maintains a local cache of known peers and coordinates with
/// the libp2p Kademlia behaviour (managed in the network swarm) to find
/// and announce peers.
pub struct PeerDiscovery {
    /// Known peers indexed by their libp2p PeerId (as bytes for serializability).
    known_peers: HashMap<Vec<u8>, PeerRecord>,

    /// Bootstrap peer addresses — initial entry points to the network.
    bootstrap_addresses: Vec<String>,

    /// This node's own record for announcements.
    local_record: Option<PeerRecord>,

    /// Maximum number of peer records to keep in the local cache.
    /// WHY: Mobile devices have limited memory. 500 peer records at ~200 bytes
    /// each is ~100KB — acceptable even on low-end devices.
    max_cached_peers: usize,
}

impl PeerDiscovery {
    /// Create a new PeerDiscovery instance.
    pub fn new(bootstrap_addresses: Vec<String>, max_cached_peers: usize) -> Self {
        PeerDiscovery {
            known_peers: HashMap::new(),
            bootstrap_addresses,
            local_record: None,
            max_cached_peers,
        }
    }

    /// Set the local node's peer record for network announcements.
    pub fn set_local_record(&mut self, record: PeerRecord) {
        self.local_record = Some(record);
    }

    /// Get the bootstrap addresses.
    pub fn bootstrap_addresses(&self) -> &[String] {
        &self.bootstrap_addresses
    }

    /// Add or update a peer record in the local cache.
    /// If the cache is full, the stalest peer is evicted.
    pub fn upsert_peer(&mut self, peer_id_bytes: Vec<u8>, record: PeerRecord) {
        if self.known_peers.len() >= self.max_cached_peers
            && !self.known_peers.contains_key(&peer_id_bytes)
        {
            self.evict_stalest_peer();
        }
        self.known_peers.insert(peer_id_bytes, record);
    }

    /// Remove and return the stalest peer record from the cache.
    fn evict_stalest_peer(&mut self) {
        let stalest = self
            .known_peers
            .iter()
            .min_by_key(|(_, record)| record.last_seen)
            .map(|(key, _)| key.clone());

        if let Some(key) = stalest {
            self.known_peers.remove(&key);
        }
    }

    /// Remove a peer from the cache.
    pub fn remove_peer(&mut self, peer_id_bytes: &[u8]) {
        self.known_peers.remove(peer_id_bytes);
    }

    /// Look up a peer by its libp2p PeerId bytes.
    pub fn get_peer(&self, peer_id_bytes: &[u8]) -> Option<&PeerRecord> {
        self.known_peers.get(peer_id_bytes)
    }

    /// Look up a peer by its Gratia NodeId.
    pub fn get_peer_by_node_id(&self, node_id: &NodeId) -> Option<&PeerRecord> {
        self.known_peers
            .values()
            .find(|record| record.node_id == *node_id)
    }

    /// Get all known peers.
    pub fn all_peers(&self) -> impl Iterator<Item = &PeerRecord> {
        self.known_peers.values()
    }

    /// Get peers that are currently mining (potential block producers/validators).
    pub fn mining_peers(&self) -> impl Iterator<Item = &PeerRecord> {
        self.known_peers.values().filter(|p| p.is_mining)
    }

    /// Get peers in a specific geographic shard.
    pub fn peers_in_shard(&self, shard_id: u16) -> impl Iterator<Item = &PeerRecord> {
        self.known_peers
            .values()
            .filter(move |p| p.shard_id == shard_id)
    }

    /// Remove stale peers from the cache.
    /// Returns the number of peers removed.
    pub fn prune_stale_peers(&mut self) -> usize {
        let now = Utc::now();
        let before = self.known_peers.len();
        self.known_peers.retain(|_, record| !record.is_stale(now));
        before - self.known_peers.len()
    }

    /// Number of known peers in the cache.
    pub fn peer_count(&self) -> usize {
        self.known_peers.len()
    }

    /// Get the local node's record.
    pub fn local_record(&self) -> Option<&PeerRecord> {
        self.local_record.as_ref()
    }

    /// Build a Kademlia key for this node's registration in the DHT.
    /// Uses the Gratia NodeId as the DHT key so peers can look up
    /// nodes by their protocol-level identity.
    pub fn dht_key_for_node(node_id: &NodeId) -> Vec<u8> {
        let mut key = b"gratia/node/".to_vec();
        key.extend_from_slice(&node_id.0);
        key
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_peer_record(node_bytes: u8, score: u8, shard: u16, mining: bool) -> PeerRecord {
        PeerRecord {
            node_id: NodeId([node_bytes; 32]),
            peer_id_bytes: vec![node_bytes],
            addresses: vec![format!("/ip4/192.168.1.{}/udp/4001/quic-v1", node_bytes)],
            presence_score: score,
            last_seen: Utc::now(),
            shard_id: shard,
            is_mining: mining,
        }
    }

    #[test]
    fn test_peer_record_staleness() {
        let mut record = make_peer_record(1, 50, 0, true);
        let now = Utc::now();
        assert!(!record.is_stale(now));

        // Simulate a record from 15 minutes ago
        record.last_seen = now - chrono::Duration::seconds(900);
        assert!(record.is_stale(now));
    }

    #[test]
    fn test_upsert_and_lookup() {
        let mut discovery = PeerDiscovery::new(vec![], 100);
        let record = make_peer_record(1, 60, 0, true);
        let peer_id = record.peer_id_bytes.clone();

        discovery.upsert_peer(peer_id.clone(), record);
        assert_eq!(discovery.peer_count(), 1);

        let found = discovery.get_peer(&peer_id).unwrap();
        assert_eq!(found.presence_score, 60);
    }

    #[test]
    fn test_lookup_by_node_id() {
        let mut discovery = PeerDiscovery::new(vec![], 100);
        let record = make_peer_record(42, 80, 3, true);
        let node_id = record.node_id;
        let peer_id = record.peer_id_bytes.clone();

        discovery.upsert_peer(peer_id, record);

        let found = discovery.get_peer_by_node_id(&node_id).unwrap();
        assert_eq!(found.shard_id, 3);
    }

    #[test]
    fn test_cache_eviction() {
        let mut discovery = PeerDiscovery::new(vec![], 3);

        // Fill cache to capacity
        for i in 0..3u8 {
            let record = make_peer_record(i, 50, 0, true);
            discovery.upsert_peer(vec![i], record);
        }
        assert_eq!(discovery.peer_count(), 3);

        // Adding one more should evict the stalest
        let record = make_peer_record(99, 50, 0, true);
        discovery.upsert_peer(vec![99], record);
        assert_eq!(discovery.peer_count(), 3);
    }

    #[test]
    fn test_mining_peers_filter() {
        let mut discovery = PeerDiscovery::new(vec![], 100);

        discovery.upsert_peer(vec![1], make_peer_record(1, 50, 0, true));
        discovery.upsert_peer(vec![2], make_peer_record(2, 50, 0, false));
        discovery.upsert_peer(vec![3], make_peer_record(3, 50, 0, true));

        let mining: Vec<_> = discovery.mining_peers().collect();
        assert_eq!(mining.len(), 2);
    }

    #[test]
    fn test_shard_filter() {
        let mut discovery = PeerDiscovery::new(vec![], 100);

        discovery.upsert_peer(vec![1], make_peer_record(1, 50, 0, true));
        discovery.upsert_peer(vec![2], make_peer_record(2, 50, 1, true));
        discovery.upsert_peer(vec![3], make_peer_record(3, 50, 0, true));

        let shard0: Vec<_> = discovery.peers_in_shard(0).collect();
        assert_eq!(shard0.len(), 2);
    }

    #[test]
    fn test_prune_stale() {
        let mut discovery = PeerDiscovery::new(vec![], 100);

        // Add a fresh peer
        discovery.upsert_peer(vec![1], make_peer_record(1, 50, 0, true));

        // Add a stale peer
        let mut stale_record = make_peer_record(2, 50, 0, true);
        stale_record.last_seen = Utc::now() - chrono::Duration::seconds(700);
        discovery.upsert_peer(vec![2], stale_record);

        let pruned = discovery.prune_stale_peers();
        assert_eq!(pruned, 1);
        assert_eq!(discovery.peer_count(), 1);
    }

    #[test]
    fn test_dht_key() {
        let node_id = NodeId([0xAB; 32]);
        let key = PeerDiscovery::dht_key_for_node(&node_id);
        assert!(key.starts_with(b"gratia/node/"));
        assert_eq!(key.len(), 12 + 32);
    }
}
