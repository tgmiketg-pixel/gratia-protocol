//! Simulated node that wraps a ConsensusEngine with its own identity.

use sha2::{Sha256, Digest};

use gratia_core::crypto::Keypair;
use gratia_core::types::{NodeId, ValidatorSignature};
use gratia_consensus::ConsensusEngine;
use gratia_consensus::committee::EligibleNode;
use gratia_consensus::vrf::VrfSecretKey;

/// A simulated node with deterministic identity derived from its index.
pub struct SimulatedNode {
    /// Human-readable index (0..N).
    pub index: usize,
    /// The node's Ed25519 keypair.
    pub keypair: Keypair,
    /// The 32-byte signing key seed (needed for ConsensusEngine constructor).
    #[allow(dead_code)]
    pub signing_key_bytes: [u8; 32],
    /// The node's identity.
    pub node_id: NodeId,
    /// The node's VRF secret key.
    pub vrf_secret_key: VrfSecretKey,
    /// The consensus engine for this node.
    pub engine: ConsensusEngine,
    /// Total rewards earned (in Lux).
    pub rewards: u64,
    /// Whether this node is currently connected to the network.
    pub connected: bool,
    /// Blocks produced by this node.
    pub blocks_produced: u64,
}

/// Flat mining reward per block (in Lux). 10 GRAT per block.
const BLOCK_REWARD_LUX: u64 = 10_000_000;

impl SimulatedNode {
    /// Create a new simulated node with deterministic keys derived from `index`.
    pub fn new(index: usize) -> Self {
        let signing_key_bytes = deterministic_seed(index);
        let keypair = Keypair::from_secret_key_bytes(&signing_key_bytes);
        let node_id = keypair.node_id();
        let vrf_secret_key = VrfSecretKey::from_ed25519_bytes(&signing_key_bytes);

        let mut engine = ConsensusEngine::new(node_id, &signing_key_bytes, 60);
        engine.trust_aware = false;

        SimulatedNode {
            index,
            keypair,
            signing_key_bytes,
            node_id,
            vrf_secret_key,
            engine,
            rewards: 0,
            connected: true,
            blocks_produced: 0,
        }
    }

    /// Build an EligibleNode descriptor for committee selection.
    pub fn as_eligible_node(&self) -> EligibleNode {
        EligibleNode {
            node_id: self.node_id,
            vrf_pubkey: self.vrf_secret_key.public_key(),
            presence_score: 60,
            has_valid_pol: true,
            meets_minimum_stake: true,
            pol_days: 90, // Established — committee-eligible
            signing_pubkey: self.keypair.public_key_bytes(),
            vrf_proof: vec![],
        }
    }

    /// Sign a block header as a committee member, returning a ValidatorSignature.
    pub fn sign_block_header(
        &self,
        header: &gratia_core::types::BlockHeader,
    ) -> Result<ValidatorSignature, gratia_core::error::GratiaError> {
        gratia_consensus::block_production::sign_block(header, self.node_id, &self.keypair)
    }

    /// Award mining reward to this node.
    pub fn award_block_reward(&mut self) {
        self.rewards += BLOCK_REWARD_LUX;
        self.blocks_produced += 1;
    }
}

/// Derive a deterministic 32-byte seed from an index.
/// Uses domain-separated SHA-256 so keys are reproducible across runs.
fn deterministic_seed(index: usize) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(b"gratia-simulator-node-v1:");
    hasher.update(index.to_le_bytes());
    let result = hasher.finalize();
    let mut seed = [0u8; 32];
    seed.copy_from_slice(&result);
    seed
}
