//! Gossipsub block and transaction propagation.
//!
//! Implements publish/subscribe messaging over libp2p Gossipsub for
//! efficient block, transaction, and attestation propagation across
//! the Gratia mobile network.

use std::collections::HashSet;

use serde::{Deserialize, Serialize};

use gratia_core::types::{Block, ProofOfLifeAttestation, Transaction};

use crate::error::NetworkError;

// ============================================================================
// Topic Definitions
// ============================================================================

/// Gossipsub topic names used by the Gratia protocol.
/// These are string constants that identify the pub/sub channels.
pub const TOPIC_BLOCKS: &str = "gratia/blocks/1";
pub const TOPIC_TRANSACTIONS: &str = "gratia/transactions/1";
pub const TOPIC_ATTESTATIONS: &str = "gratia/attestations/1";

/// All topics the Gratia node subscribes to.
pub const ALL_TOPICS: &[&str] = &[TOPIC_BLOCKS, TOPIC_TRANSACTIONS, TOPIC_ATTESTATIONS];

// ============================================================================
// Message Types
// ============================================================================

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
}

impl GossipMessage {
    /// Determine which topic this message should be published to.
    pub fn topic(&self) -> &str {
        match self {
            GossipMessage::NewBlock(_) => TOPIC_BLOCKS,
            GossipMessage::NewTransaction(_) => TOPIC_TRANSACTIONS,
            GossipMessage::NewAttestation(_) => TOPIC_ATTESTATIONS,
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
                let hash = block.header.hash();
                let mut id = b"block:".to_vec();
                id.extend_from_slice(&hash.0);
                id
            }
            GossipMessage::NewTransaction(tx) => {
                let mut id = b"tx:".to_vec();
                id.extend_from_slice(&tx.hash.0);
                id
            }
            GossipMessage::NewAttestation(att) => {
                let mut id = b"att:".to_vec();
                id.extend_from_slice(&att.node_id.0);
                id.extend_from_slice(att.date.to_string().as_bytes());
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
            if tx.signature.is_empty() {
                return Err(NetworkError::InvalidMessage(
                    "Transaction has empty signature".to_string(),
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
            fee: 1000,
            timestamp: Utc::now(),
        }
    }

    fn make_test_attestation() -> ProofOfLifeAttestation {
        ProofOfLifeAttestation {
            node_id: NodeId([5u8; 32]),
            date: Utc::now().date_naive(),
            zk_proof: vec![0u8; 128],
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
