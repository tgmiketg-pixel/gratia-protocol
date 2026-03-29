//! Gossipsub block and transaction propagation.
//!
//! Implements publish/subscribe messaging over libp2p Gossipsub for
//! efficient block, transaction, and attestation propagation across
//! the Gratia mobile network.

use std::collections::HashSet;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use gratia_core::types::{Block, NodeId, ProofOfLifeAttestation, Transaction, ValidatorSignature};

use crate::error::NetworkError;

// ============================================================================
// Topic Definitions
// ============================================================================

/// Gossipsub topic names used by the Gratia protocol.
/// These are string constants that identify the pub/sub channels.
pub const TOPIC_BLOCKS: &str = "gratia/blocks/1";
pub const TOPIC_TRANSACTIONS: &str = "gratia/transactions/1";
pub const TOPIC_ATTESTATIONS: &str = "gratia/attestations/1";
/// WHY: Separate topic for node announcements so committee-related traffic
/// doesn't mix with block/tx propagation and can be independently filtered.
pub const TOPIC_NODE_ANNOUNCE: &str = "gratia/nodes/1";
/// WHY: Separate topic for sync protocol messages (request/response). These are
/// point-to-point messages routed through gossipsub with embedded target peer IDs.
/// Nodes ignore messages not addressed to them. Acceptable for a small testnet;
/// a dedicated request/response protocol (e.g., libp2p request-response) should
/// replace this in Phase 3 for bandwidth efficiency.
pub const TOPIC_SYNC: &str = "gratia/sync/1";
/// WHY: Separate topic for Lux social posts so social traffic doesn't compete
/// with consensus-critical block/tx propagation and can be rate-limited independently.
pub const TOPIC_LUX_POSTS: &str = "gratia/lux/posts/1";
/// WHY: Dedicated topic for validator signatures on pending blocks. Separate from
/// TOPIC_BLOCKS because signatures arrive asynchronously after a block is proposed —
/// mixing them with block propagation would complicate deduplication and processing.
pub const TOPIC_VALIDATOR_SIGS: &str = "gratia/validator-sigs/1";

/// All topics the Gratia node subscribes to.
pub const ALL_TOPICS: &[&str] = &[TOPIC_BLOCKS, TOPIC_TRANSACTIONS, TOPIC_ATTESTATIONS, TOPIC_NODE_ANNOUNCE, TOPIC_SYNC, TOPIC_LUX_POSTS, TOPIC_VALIDATOR_SIGS];

// ============================================================================
// Message Types
// ============================================================================

/// A node's announcement of its eligibility for committee selection.
/// Broadcast when joining the network and when new peers connect.
///
/// WHY: Lives in gratia-network (not gratia-consensus) to avoid a circular
/// dependency. The FFI layer converts NodeAnnouncement -> EligibleNode when
/// rebuilding the committee.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeAnnouncement {
    /// The node's identity (Ed25519 public key hash).
    pub node_id: NodeId,
    /// VRF public key bytes (32 bytes compressed Ristretto).
    pub vrf_pubkey_bytes: [u8; 32],
    /// Composite Presence Score (40-100).
    pub presence_score: u8,
    /// Consecutive days of valid Proof of Life.
    pub pol_days: u64,
    /// Timestamp of this announcement.
    pub timestamp: DateTime<Utc>,
}

/// A validator's signature on a pending block, broadcast for BFT finality.
///
/// WHY: When a block producer creates a block, it signs it and broadcasts the
/// block + its own signature. Other committee members validate the block and
/// broadcast their signatures via this message. Once enough signatures accumulate
/// (meeting the finality threshold), the block is finalized.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ValidatorSignatureMessage {
    /// SHA-256 hash of the block header being signed.
    pub block_hash: [u8; 32],
    /// Height of the block being signed.
    pub height: u64,
    /// The validator's signature (includes validator NodeId and Ed25519 sig).
    pub signature: ValidatorSignature,
}

/// A gossip message wrapping the different types of data propagated
/// over the network. Serialized to bincode for compact mobile-friendly encoding.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum GossipMessage {
    /// A new block produced by a validator.
    NewBlock(Box<Block>),

    /// A new transaction submitted by a user.
    NewTransaction(Box<Transaction>),

    /// A Proof of Life attestation (contains only ZK proofs, no raw sensor data).
    NewAttestation(Box<ProofOfLifeAttestation>),

    /// A node announcing its eligibility for committee selection.
    NodeAnnouncement(Box<NodeAnnouncement>),

    /// A new Lux social post created by a user.
    NewLuxPost(Box<gratia_lux::LuxPost>),

    /// A validator's signature on a pending block for BFT finality.
    ValidatorSignatureMsg(Box<ValidatorSignatureMessage>),
}

impl GossipMessage {
    /// Determine which topic this message should be published to.
    pub fn topic(&self) -> &str {
        match self {
            GossipMessage::NewBlock(_) => TOPIC_BLOCKS,
            GossipMessage::NewTransaction(_) => TOPIC_TRANSACTIONS,
            GossipMessage::NewAttestation(_) => TOPIC_ATTESTATIONS,
            GossipMessage::NodeAnnouncement(_) => TOPIC_NODE_ANNOUNCE,
            GossipMessage::NewLuxPost(_) => TOPIC_LUX_POSTS,
            GossipMessage::ValidatorSignatureMsg(_) => TOPIC_VALIDATOR_SIGS,
        }
    }

    /// Serialize this message to bytes using bincode.
    pub fn to_bytes(&self) -> Result<Vec<u8>, NetworkError> {
        bincode::serialize(self).map_err(NetworkError::from)
    }

    /// Deserialize a message from bytes.
    pub fn from_bytes(data: &[u8]) -> Result<Self, NetworkError> {
        bincode::deserialize(data).map_err(NetworkError::from)
    }

    /// Get a unique identifier for this message, used for deduplication.
    pub fn message_id(&self) -> Vec<u8> {
        match self {
            GossipMessage::NewBlock(block) => {
                // WHY: Fallback to height-based ID if header serialization fails.
                // This is a deduplication key, so a degraded ID is acceptable.
                let mut id = b"block:".to_vec();
                match block.header.hash() {
                    Ok(hash) => id.extend_from_slice(&hash.0),
                    Err(_) => id.extend_from_slice(&block.header.height.to_be_bytes()),
                }
                id
            }
            GossipMessage::NewTransaction(tx) => {
                let mut id = b"tx:".to_vec();
                id.extend_from_slice(&tx.hash.0);
                id
            }
            GossipMessage::NewAttestation(att) => {
                // WHY: Use nullifier for deduplication — it is the same within
                // an epoch, so duplicate attestations from the same node in the
                // same epoch are correctly detected.
                let mut id = b"att:".to_vec();
                id.extend_from_slice(&att.nullifier);
                id
            }
            GossipMessage::NodeAnnouncement(ann) => {
                // WHY: Dedup by node_id — only the latest announcement from a
                // given node matters. Re-announcements (e.g., on reconnect) are
                // expected and will be filtered by the dedup cache.
                let mut id = b"node:".to_vec();
                id.extend_from_slice(&ann.node_id.0);
                id
            }
            GossipMessage::NewLuxPost(post) => {
                // WHY: Dedup by post hash — each post has a unique SHA-256 hash.
                let mut id = b"lux:".to_vec();
                id.extend_from_slice(post.hash.as_bytes());
                id
            }
            GossipMessage::ValidatorSignatureMsg(msg) => {
                // WHY: Dedup by block hash + validator node ID — each committee
                // member signs each block exactly once.
                let mut id = b"vsig:".to_vec();
                id.extend_from_slice(&msg.block_hash);
                id.extend_from_slice(&msg.signature.validator.0);
                id
            }
        }
    }
}

// ============================================================================
// Message Validation
// ============================================================================

/// Maximum gossip message size in bytes.
/// WHY: 256 KB max block size + overhead for serialization format.
/// Messages larger than this are rejected to prevent memory exhaustion
/// on resource-constrained mobile devices.
pub const MAX_MESSAGE_SIZE: usize = 300 * 1024; // 300 KB

/// Validates an incoming gossip message before processing.
/// Returns the deserialized message if valid.
pub fn validate_incoming_message(data: &[u8]) -> Result<GossipMessage, NetworkError> {
    // Size check — reject oversized messages before deserialization
    if data.len() > MAX_MESSAGE_SIZE {
        return Err(NetworkError::MessageTooLarge {
            size: data.len(),
            max: MAX_MESSAGE_SIZE,
        });
    }

    // Deserialize
    let msg = GossipMessage::from_bytes(data)?;

    // Basic structural validation (full consensus validation happens elsewhere)
    match &msg {
        GossipMessage::NewBlock(block) => {
            if block.transactions.len() > 10_000 {
                // WHY: Sanity check — a 256KB block physically cannot contain
                // more than ~10K minimal transactions.
                return Err(NetworkError::InvalidMessage(
                    "Block contains too many transactions".to_string(),
                ));
            }
        }
        GossipMessage::NewTransaction(tx) => {
            // WHY: Reject structurally invalid transactions at the gossip layer
            // before they ever reach the application. This is a first line of
            // defense; full Ed25519 verification happens in the FFI layer.
            if tx.signature.is_empty() {
                return Err(NetworkError::InvalidMessage(
                    "Transaction has empty signature".to_string(),
                ));
            }
            if tx.signature.len() != 64 {
                return Err(NetworkError::InvalidMessage(
                    format!("Invalid signature length: {} (expected 64)", tx.signature.len()),
                ));
            }
            if tx.sender_pubkey.len() != 32 {
                return Err(NetworkError::InvalidMessage(
                    format!("Invalid pubkey length: {} (expected 32)", tx.sender_pubkey.len()),
                ));
            }
        }
        GossipMessage::NewAttestation(att) => {
            if att.zk_proof.is_empty() {
                return Err(NetworkError::InvalidMessage(
                    "Attestation has empty ZK proof".to_string(),
                ));
            }
            if att.presence_score < 40 || att.presence_score > 100 {
                return Err(NetworkError::InvalidMessage(format!(
                    "Invalid presence score: {} (must be 40-100)",
                    att.presence_score
                )));
            }
        }
        GossipMessage::NodeAnnouncement(ann) => {
            if ann.presence_score < 40 || ann.presence_score > 100 {
                return Err(NetworkError::InvalidMessage(format!(
                    "Invalid announcement presence score: {} (must be 40-100)",
                    ann.presence_score
                )));
            }
        }
        GossipMessage::NewLuxPost(post) => {
            // WHY: Reject posts with empty content or missing signature at the
            // gossip layer before they reach the application.
            if post.content.is_empty() && post.attachments.is_empty() {
                return Err(NetworkError::InvalidMessage(
                    "Lux post has no content and no attachments".to_string(),
                ));
            }
            if post.signature.is_empty() {
                return Err(NetworkError::InvalidMessage(
                    "Lux post has empty signature".to_string(),
                ));
            }
            if post.hash.is_empty() {
                return Err(NetworkError::InvalidMessage(
                    "Lux post has empty hash".to_string(),
                ));
            }
        }
        GossipMessage::ValidatorSignatureMsg(msg) => {
            // WHY: Reject structurally invalid validator signatures at the gossip
            // layer. Full committee membership and cryptographic verification
            // happens in the consensus/FFI layer.
            if msg.signature.signature.len() != 64 {
                return Err(NetworkError::InvalidMessage(
                    format!(
                        "Invalid validator signature length: {} (expected 64)",
                        msg.signature.signature.len()
                    ),
                ));
            }
            if msg.block_hash == [0u8; 32] {
                return Err(NetworkError::InvalidMessage(
                    "Validator signature has zero block hash".to_string(),
                ));
            }
        }
    }

    Ok(msg)
}

// ============================================================================
// Deduplication Cache
// ============================================================================

/// Tracks recently seen message IDs to reject duplicates.
/// WHY: On a mobile gossip network, the same message arrives via multiple paths.
/// Deduplication avoids re-processing and re-propagating messages we already have.
pub struct DeduplicationCache {
    /// Set of message IDs we have seen recently.
    seen: HashSet<Vec<u8>>,

    /// Maximum number of entries before we start evicting.
    /// WHY: 10,000 entries at ~40 bytes each = ~400KB. Acceptable on mobile.
    /// Covers roughly 3-5 minutes of high-throughput network activity.
    max_entries: usize,
}

impl DeduplicationCache {
    pub fn new(max_entries: usize) -> Self {
        DeduplicationCache {
            seen: HashSet::new(),
            max_entries,
        }
    }

    /// Check if a message has been seen before. If not, marks it as seen.
    /// Returns true if this is a NEW (not previously seen) message.
    pub fn check_and_insert(&mut self, message_id: &[u8]) -> bool {
        if self.seen.contains(message_id) {
            return false;
        }

        // Evict all entries when cache is full.
        // WHY: Simple strategy — a proper LRU would be better but adds complexity.
        // Since gossip messages are ephemeral, clearing the whole cache is acceptable.
        // Messages re-received after eviction will simply be re-validated.
        if self.seen.len() >= self.max_entries {
            self.seen.clear();
        }

        self.seen.insert(message_id.to_vec());
        true
    }

    /// Check if a message ID has been seen (without inserting).
    pub fn contains(&self, message_id: &[u8]) -> bool {
        self.seen.contains(message_id)
    }

    /// Number of entries in the cache.
    pub fn len(&self) -> usize {
        self.seen.len()
    }

    /// Whether the cache is empty.
    pub fn is_empty(&self) -> bool {
        self.seen.is_empty()
    }

    /// Clear all entries.
    pub fn clear(&mut self) {
        self.seen.clear();
    }
}

// ============================================================================
// Gossip Handler
// ============================================================================

/// Manages gossipsub message handling for the Gratia node.
///
/// This struct coordinates message validation, deduplication, and dispatch.
/// The actual gossipsub behaviour is owned by the libp2p Swarm; this handler
/// provides the application-level logic.
pub struct GossipHandler {
    /// Deduplication cache for blocks.
    block_cache: DeduplicationCache,
    /// Deduplication cache for transactions.
    tx_cache: DeduplicationCache,
    /// Deduplication cache for attestations.
    attestation_cache: DeduplicationCache,
    /// Deduplication cache for node announcements.
    announce_cache: DeduplicationCache,
    /// Deduplication cache for Lux social posts.
    lux_cache: DeduplicationCache,
    /// Deduplication cache for validator signature messages.
    sig_cache: DeduplicationCache,
}

impl GossipHandler {
    pub fn new() -> Self {
        GossipHandler {
            // WHY: Block cache is smaller — blocks arrive every 3-5 seconds.
            // 1,000 entries covers ~1 hour of blocks.
            block_cache: DeduplicationCache::new(1_000),
            // WHY: Transaction cache is larger — high throughput target (131-218 TPS).
            // 10,000 entries covers ~50-75 seconds at max throughput.
            tx_cache: DeduplicationCache::new(10_000),
            // WHY: Attestation cache — one per node per day, but many nodes.
            // 5,000 entries is generous for early network.
            attestation_cache: DeduplicationCache::new(5_000),
            // WHY: Announce cache — one per node. 500 entries covers all peers
            // in early network. Re-announcements on reconnect are expected.
            announce_cache: DeduplicationCache::new(500),
            // WHY: Lux post cache — social posts arrive frequently but less than
            // transactions. 5,000 entries covers several minutes of activity.
            lux_cache: DeduplicationCache::new(5_000),
            // WHY: Validator sig cache — up to 21 committee members * recent blocks.
            // 2,000 entries covers ~95 blocks worth of full committee signatures.
            sig_cache: DeduplicationCache::new(2_000),
        }
    }

    /// Process an incoming gossip message. Returns the deserialized message
    /// if it passes validation and deduplication, or an error/None if rejected.
    pub fn process_incoming(
        &mut self,
        topic: &str,
        data: &[u8],
    ) -> Result<Option<GossipMessage>, NetworkError> {
        let msg = validate_incoming_message(data)?;

        // Verify the message matches the topic it was received on
        if msg.topic() != topic {
            return Err(NetworkError::InvalidMessage(format!(
                "Message type does not match topic: expected '{}', received on '{}'",
                msg.topic(),
                topic
            )));
        }

        // Deduplication check
        let msg_id = msg.message_id();
        let is_new = match &msg {
            GossipMessage::NewBlock(_) => self.block_cache.check_and_insert(&msg_id),
            GossipMessage::NewTransaction(_) => self.tx_cache.check_and_insert(&msg_id),
            GossipMessage::NewAttestation(_) => self.attestation_cache.check_and_insert(&msg_id),
            GossipMessage::NodeAnnouncement(_) => self.announce_cache.check_and_insert(&msg_id),
            GossipMessage::NewLuxPost(_) => self.lux_cache.check_and_insert(&msg_id),
            GossipMessage::ValidatorSignatureMsg(_) => self.sig_cache.check_and_insert(&msg_id),
        };

        if is_new {
            Ok(Some(msg))
        } else {
            Ok(None) // Duplicate — silently drop
        }
    }

    /// Prepare a block for gossip publication.
    pub fn prepare_block(&self, block: Block) -> Result<(String, Vec<u8>), NetworkError> {
        let msg = GossipMessage::NewBlock(Box::new(block));
        let data = msg.to_bytes()?;
        if data.len() > MAX_MESSAGE_SIZE {
            return Err(NetworkError::MessageTooLarge {
                size: data.len(),
                max: MAX_MESSAGE_SIZE,
            });
        }
        Ok((TOPIC_BLOCKS.to_string(), data))
    }

    /// Prepare a transaction for gossip publication.
    pub fn prepare_transaction(&self, tx: Transaction) -> Result<(String, Vec<u8>), NetworkError> {
        let msg = GossipMessage::NewTransaction(Box::new(tx));
        let data = msg.to_bytes()?;
        if data.len() > MAX_MESSAGE_SIZE {
            return Err(NetworkError::MessageTooLarge {
                size: data.len(),
                max: MAX_MESSAGE_SIZE,
            });
        }
        Ok((TOPIC_TRANSACTIONS.to_string(), data))
    }

    /// Prepare a node announcement for gossip publication.
    pub fn prepare_node_announcement(
        &self,
        announcement: NodeAnnouncement,
    ) -> Result<(String, Vec<u8>), NetworkError> {
        let msg = GossipMessage::NodeAnnouncement(Box::new(announcement));
        let data = msg.to_bytes()?;
        if data.len() > MAX_MESSAGE_SIZE {
            return Err(NetworkError::MessageTooLarge {
                size: data.len(),
                max: MAX_MESSAGE_SIZE,
            });
        }
        Ok((TOPIC_NODE_ANNOUNCE.to_string(), data))
    }

    /// Prepare an attestation for gossip publication.
    pub fn prepare_attestation(
        &self,
        attestation: ProofOfLifeAttestation,
    ) -> Result<(String, Vec<u8>), NetworkError> {
        let msg = GossipMessage::NewAttestation(Box::new(attestation));
        let data = msg.to_bytes()?;
        if data.len() > MAX_MESSAGE_SIZE {
            return Err(NetworkError::MessageTooLarge {
                size: data.len(),
                max: MAX_MESSAGE_SIZE,
            });
        }
        Ok((TOPIC_ATTESTATIONS.to_string(), data))
    }

    /// Prepare a validator signature message for gossip publication.
    pub fn prepare_validator_signature(
        &self,
        msg: ValidatorSignatureMessage,
    ) -> Result<(String, Vec<u8>), NetworkError> {
        let gossip_msg = GossipMessage::ValidatorSignatureMsg(Box::new(msg));
        let data = gossip_msg.to_bytes()?;
        if data.len() > MAX_MESSAGE_SIZE {
            return Err(NetworkError::MessageTooLarge {
                size: data.len(),
                max: MAX_MESSAGE_SIZE,
            });
        }
        Ok((TOPIC_VALIDATOR_SIGS.to_string(), data))
    }

    /// Prepare a Lux social post for gossip publication.
    pub fn prepare_lux_post(
        &self,
        post: gratia_lux::LuxPost,
    ) -> Result<(String, Vec<u8>), NetworkError> {
        let msg = GossipMessage::NewLuxPost(Box::new(post));
        let data = msg.to_bytes()?;
        if data.len() > MAX_MESSAGE_SIZE {
            return Err(NetworkError::MessageTooLarge {
                size: data.len(),
                max: MAX_MESSAGE_SIZE,
            });
        }
        Ok((TOPIC_LUX_POSTS.to_string(), data))
    }
}

impl Default for GossipHandler {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use gratia_core::types::*;

    fn make_test_block() -> Block {
        Block {
            header: BlockHeader {
                height: 1,
                timestamp: Utc::now(),
                parent_hash: BlockHash([0u8; 32]),
                transactions_root: [0u8; 32],
                state_root: [0u8; 32],
                attestations_root: [0u8; 32],
                producer: NodeId([1u8; 32]),
                vrf_proof: vec![0u8; 64],
                active_miners: 100,
                geographic_diversity: 5,
            },
            transactions: vec![],
            attestations: vec![],
            validator_signatures: vec![],
        }
    }

    fn make_test_transaction() -> Transaction {
        Transaction {
            hash: TxHash([42u8; 32]),
            payload: TransactionPayload::Transfer {
                to: Address([0u8; 32]),
                amount: 1_000_000,
            },
            sender_pubkey: vec![1u8; 32],
            signature: vec![2u8; 64],
            nonce: 1,
            chain_id: 2, // WHY: Testnet chain ID for test data
            fee: 1000,
            timestamp: Utc::now(),
        }
    }

    fn make_test_attestation() -> ProofOfLifeAttestation {
        ProofOfLifeAttestation {
            blinded_id: [0xAA; 32],
            nullifier: [0xBB; 32],
            zk_proof: vec![0u8; 128],
            zk_commitments: None,
            presence_score: 65,
            sensor_flags: SensorFlags {
                gps: true,
                accelerometer: true,
                wifi: true,
                bluetooth: true,
                gyroscope: true,
                ambient_light: false,
                cellular: true,
                barometer: false,
                magnetometer: false,
                nfc: false,
                secure_enclave: false,
                biometric: false,
                camera_hash: false,
                microphone_hash: false,
            },
            signature: vec![3u8; 64],
        }
    }

    #[test]
    fn test_gossip_message_topics() {
        let block_msg = GossipMessage::NewBlock(Box::new(make_test_block()));
        assert_eq!(block_msg.topic(), TOPIC_BLOCKS);

        let tx_msg = GossipMessage::NewTransaction(Box::new(make_test_transaction()));
        assert_eq!(tx_msg.topic(), TOPIC_TRANSACTIONS);

        let att_msg = GossipMessage::NewAttestation(Box::new(make_test_attestation()));
        assert_eq!(att_msg.topic(), TOPIC_ATTESTATIONS);
    }

    #[test]
    fn test_message_serialization_roundtrip() {
        let block = make_test_block();
        let msg = GossipMessage::NewBlock(Box::new(block));

        let bytes = msg.to_bytes().unwrap();
        let decoded = GossipMessage::from_bytes(&bytes).unwrap();

        match decoded {
            GossipMessage::NewBlock(b) => {
                assert_eq!(b.header.height, 1);
                assert_eq!(b.header.active_miners, 100);
            }
            _ => panic!("Expected NewBlock"),
        }
    }

    #[test]
    fn test_message_id_uniqueness() {
        let block1 = make_test_block();
        let mut block2 = make_test_block();
        block2.header.height = 2;

        let msg1 = GossipMessage::NewBlock(Box::new(block1));
        let msg2 = GossipMessage::NewBlock(Box::new(block2));

        // Different blocks should have different message IDs
        // (they have different heights -> different hashes)
        assert_ne!(msg1.message_id(), msg2.message_id());
    }

    #[test]
    fn test_deduplication_cache() {
        let mut cache = DeduplicationCache::new(100);

        let id = b"test_message_1".to_vec();
        assert!(cache.check_and_insert(&id)); // New
        assert!(!cache.check_and_insert(&id)); // Duplicate
        assert_eq!(cache.len(), 1);
    }

    #[test]
    fn test_deduplication_cache_eviction() {
        let mut cache = DeduplicationCache::new(3);

        assert!(cache.check_and_insert(b"a"));
        assert!(cache.check_and_insert(b"b"));
        assert!(cache.check_and_insert(b"c"));
        assert_eq!(cache.len(), 3);

        // This should trigger full eviction then insert
        assert!(cache.check_and_insert(b"d"));
        assert_eq!(cache.len(), 1);
    }

    #[test]
    fn test_validate_oversized_message() {
        let data = vec![0u8; MAX_MESSAGE_SIZE + 1];
        let result = validate_incoming_message(&data);
        assert!(matches!(result, Err(NetworkError::MessageTooLarge { .. })));
    }

    #[test]
    fn test_validate_invalid_presence_score() {
        let mut att = make_test_attestation();
        att.presence_score = 30; // Below minimum of 40
        let msg = GossipMessage::NewAttestation(Box::new(att));
        let bytes = msg.to_bytes().unwrap();

        let result = validate_incoming_message(&bytes);
        assert!(matches!(result, Err(NetworkError::InvalidMessage(_))));
    }

    #[test]
    fn test_gossip_handler_deduplicates() {
        let mut handler = GossipHandler::new();

        let block = make_test_block();
        let msg = GossipMessage::NewBlock(Box::new(block));
        let data = msg.to_bytes().unwrap();

        // First time: should return the message
        let result = handler.process_incoming(TOPIC_BLOCKS, &data).unwrap();
        assert!(result.is_some());

        // Second time: should return None (duplicate)
        let result = handler.process_incoming(TOPIC_BLOCKS, &data).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn test_gossip_handler_topic_mismatch() {
        let mut handler = GossipHandler::new();

        let block = make_test_block();
        let msg = GossipMessage::NewBlock(Box::new(block));
        let data = msg.to_bytes().unwrap();

        // Block message on transaction topic should fail
        let result = handler.process_incoming(TOPIC_TRANSACTIONS, &data);
        assert!(matches!(result, Err(NetworkError::InvalidMessage(_))));
    }

    #[test]
    fn test_prepare_block() {
        let handler = GossipHandler::new();
        let block = make_test_block();
        let (topic, data) = handler.prepare_block(block).unwrap();
        assert_eq!(topic, TOPIC_BLOCKS);
        assert!(!data.is_empty());
        assert!(data.len() < MAX_MESSAGE_SIZE);
    }

    #[test]
    fn test_prepare_transaction() {
        let handler = GossipHandler::new();
        let tx = make_test_transaction();
        let (topic, data) = handler.prepare_transaction(tx).unwrap();
        assert_eq!(topic, TOPIC_TRANSACTIONS);
        assert!(!data.is_empty());
    }

    #[test]
    fn test_prepare_attestation() {
        let handler = GossipHandler::new();
        let att = make_test_attestation();
        let (topic, data) = handler.prepare_attestation(att).unwrap();
        assert_eq!(topic, TOPIC_ATTESTATIONS);
        assert!(!data.is_empty());
    }
}
