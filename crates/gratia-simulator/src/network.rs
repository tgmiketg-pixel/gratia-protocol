//! In-memory network simulation with configurable latency.
//!
//! Messages are routed between nodes via tokio mpsc channels.
//! Each message delivery is delayed by a random latency (50-200ms)
//! to simulate real network conditions.

use std::collections::HashMap;

use tokio::sync::mpsc;
use rand::Rng;

use gratia_core::types::{Block, BlockHash, NodeId, ValidatorSignature};

// ============================================================================
// Message types
// ============================================================================

/// Messages that flow between simulated nodes.
#[derive(Debug, Clone)]
pub enum NetworkMessage {
    /// A new block has been produced and needs committee signatures.
    BlockProposed {
        block: Block,
        producer: NodeId,
    },
    /// A committee member's co-signature on a proposed block.
    BlockSignature {
        block_hash: BlockHash,
        height: u64,
        signature: ValidatorSignature,
    },
    /// A block has been finalized (enough BFT signatures collected).
    BlockFinalized {
        block: Block,
    },
}

// ============================================================================
// Network router
// ============================================================================

/// Sender half for a node's inbox.
pub type NodeSender = mpsc::UnboundedSender<NetworkMessage>;

/// Receiver half for a node's inbox.
pub type NodeReceiver = mpsc::UnboundedReceiver<NetworkMessage>;

/// In-memory network that routes messages between nodes with simulated latency.
pub struct SimulatedNetwork {
    /// Per-node message senders, keyed by node index.
    senders: HashMap<usize, NodeSender>,
    /// Minimum latency in milliseconds.
    min_latency_ms: u64,
    /// Maximum latency in milliseconds.
    max_latency_ms: u64,
}

impl SimulatedNetwork {
    /// Create a new simulated network.
    pub fn new(min_latency_ms: u64, max_latency_ms: u64) -> Self {
        SimulatedNetwork {
            senders: HashMap::new(),
            min_latency_ms,
            max_latency_ms,
        }
    }

    /// Register a node and return its receiver channel.
    pub fn register_node(&mut self, index: usize) -> NodeReceiver {
        let (tx, rx) = mpsc::unbounded_channel();
        self.senders.insert(index, tx);
        rx
    }

    /// Remove a node from the network (disconnect).
    pub fn disconnect_node(&mut self, index: usize) {
        self.senders.remove(&index);
    }

    /// Reconnect a node (re-register with a new channel).
    pub fn reconnect_node(&mut self, index: usize) -> NodeReceiver {
        self.register_node(index)
    }

    /// Broadcast a message to all connected nodes except the sender.
    /// Each delivery is delayed by a random latency.
    pub fn broadcast(&self, from_index: usize, msg: NetworkMessage) {
        let min_ms = self.min_latency_ms;
        let max_ms = self.max_latency_ms;

        for (&idx, sender) in &self.senders {
            if idx == from_index {
                continue;
            }
            let msg_clone = msg.clone();
            let sender_clone = sender.clone();
            tokio::spawn(async move {
                let delay = rand::thread_rng().gen_range(min_ms..=max_ms);
                tokio::time::sleep(tokio::time::Duration::from_millis(delay)).await;
                let _ = sender_clone.send(msg_clone);
            });
        }
    }

    /// Send a message to a specific node with latency.
    pub fn send_to(&self, target_index: usize, msg: NetworkMessage) {
        if let Some(sender) = self.senders.get(&target_index) {
            let min_ms = self.min_latency_ms;
            let max_ms = self.max_latency_ms;
            let sender_clone = sender.clone();
            tokio::spawn(async move {
                let delay = rand::thread_rng().gen_range(min_ms..=max_ms);
                tokio::time::sleep(tokio::time::Duration::from_millis(delay)).await;
                let _ = sender_clone.send(msg);
            });
        }
    }

    /// Number of currently connected nodes.
    pub fn connected_count(&self) -> usize {
        self.senders.len()
    }
}
