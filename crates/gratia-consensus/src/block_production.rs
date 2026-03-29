//! Block production logic.
//!
//! Handles creating new blocks (assembling transactions, computing roots,
//! signing) and provides the BlockProducer that determines when this node
//! should produce a block.

use chrono::Utc;
use serde::{Deserialize, Serialize};

use gratia_core::crypto::{sha256, merkle_root, Keypair};
use gratia_core::error::GratiaError;
use gratia_core::types::{
    Block, BlockHash, BlockHeader, NodeId, ProofOfLifeAttestation,
    Transaction, ValidatorSignature,
};

use crate::committee::ValidatorCommittee;
use crate::validation::MAX_BLOCK_SIZE;
use crate::vrf::{self, VrfSecretKey};

// ============================================================================
// Types
// ============================================================================

/// A block producer that manages block creation for this node.
#[derive(Debug)]
pub struct BlockProducer {
    /// This node's identity.
    pub node_id: NodeId,
    /// The current slot number.
    current_slot: u64,
    /// Number of active mining nodes in the network.
    pub(crate) active_miners: u64,
    /// Geographic diversity metric.
    pub(crate) geographic_diversity: u16,
}

/// A block that has been produced but not yet finalized (awaiting committee signatures).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PendingBlock {
    /// The produced block.
    pub block: Block,
    /// Collected validator signatures.
    pub signatures: Vec<ValidatorSignature>,
    /// The slot in which this block was produced.
    pub slot: u64,
    /// The finality threshold for this block's committee (from graduated scaling).
    /// WHY: Committee size and finality threshold scale with network size.
    /// This stores the threshold at the time the block was produced.
    pub finality_threshold: usize,
}

impl PendingBlock {
    /// Check if enough signatures have been collected for finality.
    pub fn is_finalized(&self) -> bool {
        self.signatures.len() >= self.finality_threshold
    }

    /// Add a validator signature. Returns error if the signer already signed.
    pub fn add_signature(&mut self, sig: ValidatorSignature) -> Result<(), GratiaError> {
        if self.signatures.iter().any(|s| s.validator == sig.validator) {
            return Err(GratiaError::BlockValidationFailed {
                reason: format!("Duplicate signature from validator {}", sig.validator),
            });
        }
        self.signatures.push(sig);
        Ok(())
    }

    /// Finalize the block by copying collected signatures into the block.
    pub fn finalize(mut self) -> Result<Block, GratiaError> {
        if !self.is_finalized() {
            return Err(GratiaError::InsufficientSignatures {
                count: self.signatures.len(),
                required: self.finality_threshold,
            });
        }
        self.block.validator_signatures = self.signatures;
        Ok(self.block)
    }
}

// ============================================================================
// Block Producer
// ============================================================================

impl BlockProducer {
    /// Create a new block producer for this node.
    pub fn new(node_id: NodeId, active_miners: u64, geographic_diversity: u16) -> Self {
        BlockProducer {
            node_id,
            current_slot: 0,
            active_miners,
            geographic_diversity,
        }
    }

    /// Set the current slot.
    pub fn set_slot(&mut self, slot: u64) {
        self.current_slot = slot;
    }

    /// Update network statistics used in block headers.
    pub fn update_network_stats(&mut self, active_miners: u64, geographic_diversity: u16) {
        self.active_miners = active_miners;
        self.geographic_diversity = geographic_diversity;
    }

    /// Check if this node should produce a block in the current slot.
    pub fn should_produce_block(&self, committee: &ValidatorCommittee) -> bool {
        if let Some(producer) = committee.block_producer_for_slot(self.current_slot) {
            producer.node_id == self.node_id
        } else {
            false
        }
    }

    /// Produce a new block.
    ///
    /// Assembles the given transactions and attestations into a block,
    /// computes Merkle roots, and signs it with the VRF proof for this slot.
    pub fn produce_block(
        &self,
        transactions: Vec<Transaction>,
        attestations: Vec<ProofOfLifeAttestation>,
        previous_hash: BlockHash,
        height: u64,
        state_root: [u8; 32],
        vrf_secret_key: &VrfSecretKey,
        committee: &ValidatorCommittee,
    ) -> Result<PendingBlock, GratiaError> {
        // Compute transaction Merkle root
        let tx_hashes: Vec<[u8; 32]> = transactions.iter().map(|tx| tx.hash.0).collect();
        let transactions_root = merkle_root(&tx_hashes);

        // Compute attestation Merkle root
        let attestation_hashes: Vec<[u8; 32]> = attestations
            .iter()
            .map(|a| {
                let bytes = bincode::serialize(a)
                    .expect("attestation serialization should not fail");
                sha256(&bytes)
            })
            .collect();
        let attestations_root = merkle_root(&attestation_hashes);

        // Generate VRF proof for this slot
        let vrf_input = vrf::build_vrf_input(&previous_hash.0, self.current_slot);
        let vrf_proof = vrf::generate_vrf_proof(vrf_secret_key, &vrf_input);

        let header = BlockHeader {
            height,
            timestamp: Utc::now(),
            parent_hash: previous_hash,
            transactions_root,
            state_root,
            attestations_root,
            producer: self.node_id,
            vrf_proof: vrf_proof.proof_bytes.clone(),
            active_miners: self.active_miners,
            geographic_diversity: self.geographic_diversity,
        };

        let block = Block {
            header,
            transactions,
            attestations,
            validator_signatures: Vec::new(), // Signatures collected separately
        };

        // Check block size before returning
        let block_bytes = bincode::serialize(&block)
            .map_err(|e| GratiaError::SerializationError(e.to_string()))?;
        if block_bytes.len() > MAX_BLOCK_SIZE {
            return Err(GratiaError::BlockValidationFailed {
                reason: format!(
                    "Produced block {} bytes exceeds maximum {} bytes. Reduce transactions.",
                    block_bytes.len(),
                    MAX_BLOCK_SIZE,
                ),
            });
        }

        Ok(PendingBlock {
            block,
            signatures: Vec::new(),
            slot: self.current_slot,
            finality_threshold: committee.finality_threshold,
        })
    }
}

/// Sign a block header as a validator, producing a ValidatorSignature.
pub fn sign_block(
    header: &BlockHeader,
    node_id: NodeId,
    keypair: &Keypair,
) -> Result<ValidatorSignature, GratiaError> {
    let header_hash = header.hash()?;
    let signature = keypair.sign(&header_hash.0);
    Ok(ValidatorSignature {
        validator: node_id,
        signature,
    })
}

/// Verify a validator's signature on a block header.
pub fn verify_block_signature(
    header: &BlockHeader,
    sig: &ValidatorSignature,
    public_key_bytes: &[u8],
) -> Result<(), GratiaError> {
    let header_hash = header.hash()?;
    gratia_core::crypto::verify_signature(public_key_bytes, &header_hash.0, &sig.signature)
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use gratia_core::crypto::Keypair;
    use gratia_core::types::*;
    use crate::committee::{self, EligibleNode};
    use crate::vrf::{VrfPublicKey, VrfSecretKey};
    use rand::rngs::OsRng;

    fn make_test_committee() -> ValidatorCommittee {
        let nodes: Vec<EligibleNode> = (0..25)
            .map(|i| {
                let mut node_id = [0u8; 32];
                node_id[0] = i;
                EligibleNode {
                    node_id: NodeId(node_id),
                    vrf_pubkey: VrfPublicKey { bytes: [i; 32] },
                    presence_score: 60,
                    has_valid_pol: true,
                    meets_minimum_stake: true,
                    pol_days: 90,
                }
            })
            .collect();

        committee::select_committee(&nodes, &[0xAB; 32], 0, 0).unwrap()
    }

    fn make_test_transaction(keypair: &Keypair) -> Transaction {
        let payload = TransactionPayload::Transfer {
            to: Address([0x42; 32]),
            amount: 1_000_000,
        };
        let nonce = 1u64;
        let fee = 1_000u64;
        let timestamp = Utc::now();

        let payload_bytes = bincode::serialize(&payload).unwrap();
        let mut signing_message = Vec::new();
        signing_message.extend_from_slice(&nonce.to_le_bytes());
        signing_message.extend_from_slice(&fee.to_le_bytes());
        let ts_bytes = bincode::serialize(&timestamp).unwrap();
        signing_message.extend_from_slice(&ts_bytes);
        signing_message.extend_from_slice(&payload_bytes);

        let signature = keypair.sign(&signing_message);
        let hash = sha256(&signing_message);

        Transaction {
            hash: TxHash(hash),
            payload,
            sender_pubkey: keypair.public_key_bytes(),
            signature,
            nonce,
            chain_id: 2,
            fee,
            timestamp,
        }
    }

    #[test]
    fn test_produce_empty_block() {
        let committee = make_test_committee();
        let producer_id = committee.members[0].node_id;
        let vrf_sk = VrfSecretKey::generate(&mut OsRng);

        let mut bp = BlockProducer::new(producer_id, 100, 5);
        bp.set_slot(0);

        let result = bp.produce_block(
            vec![],
            vec![],
            BlockHash([0; 32]),
            1,
            [0; 32],
            &vrf_sk,
            &committee,
        );

        assert!(result.is_ok());
        let pending = result.unwrap();
        assert_eq!(pending.block.header.height, 1);
        assert_eq!(pending.block.header.producer, producer_id);
        assert_eq!(pending.block.transactions.len(), 0);
        assert!(!pending.is_finalized());
    }

    #[test]
    fn test_produce_block_with_transactions() {
        let committee = make_test_committee();
        let producer_id = committee.members[0].node_id;
        let keypair = Keypair::generate();
        let vrf_sk = VrfSecretKey::generate(&mut OsRng);

        let tx = make_test_transaction(&keypair);

        let mut bp = BlockProducer::new(producer_id, 100, 5);
        bp.set_slot(0);

        let result = bp.produce_block(
            vec![tx],
            vec![],
            BlockHash([0; 32]),
            1,
            [0; 32],
            &vrf_sk,
            &committee,
        );

        assert!(result.is_ok());
        let pending = result.unwrap();
        assert_eq!(pending.block.transactions.len(), 1);
    }

    #[test]
    fn test_should_produce_block() {
        let committee = make_test_committee();
        let producer_0 = committee.members[0].node_id;
        let producer_1 = committee.members[1].node_id;
        let non_producer = NodeId([0xFF; 32]);

        let mut bp0 = BlockProducer::new(producer_0, 100, 5);
        bp0.set_slot(0);
        assert!(bp0.should_produce_block(&committee));

        let mut bp1 = BlockProducer::new(producer_1, 100, 5);
        bp1.set_slot(1);
        assert!(bp1.should_produce_block(&committee));

        // Non-committee member should never produce
        let mut bp_non = BlockProducer::new(non_producer, 100, 5);
        bp_non.set_slot(0);
        assert!(!bp_non.should_produce_block(&committee));
    }

    #[test]
    fn test_pending_block_finalization() {
        let committee = make_test_committee();
        let producer_id = committee.members[0].node_id;
        let vrf_sk = VrfSecretKey::generate(&mut OsRng);

        let mut bp = BlockProducer::new(producer_id, 100, 5);
        bp.set_slot(0);

        let mut pending = bp.produce_block(
            vec![],
            vec![],
            BlockHash([0; 32]),
            1,
            [0; 32],
            &vrf_sk,
            &committee,
        ).unwrap();

        let threshold = pending.finality_threshold;

        // Not enough signatures yet
        assert!(!pending.is_finalized());
        assert!(pending.clone().finalize().is_err());

        // Add exactly `threshold` signatures (finality threshold from graduated scaling)
        for i in 0..threshold as u8 {
            let mut validator_id = [0u8; 32];
            validator_id[0] = i;
            let sig = ValidatorSignature {
                validator: NodeId(validator_id),
                signature: vec![0u8; 64], // Placeholder signature
            };
            pending.add_signature(sig).unwrap();
        }

        assert!(pending.is_finalized());
        let finalized = pending.finalize().unwrap();
        assert_eq!(finalized.validator_signatures.len(), threshold);
    }

    #[test]
    fn test_duplicate_signature_rejected() {
        let committee = make_test_committee();
        let producer_id = committee.members[0].node_id;
        let vrf_sk = VrfSecretKey::generate(&mut OsRng);

        let mut bp = BlockProducer::new(producer_id, 100, 5);
        bp.set_slot(0);

        let mut pending = bp.produce_block(
            vec![],
            vec![],
            BlockHash([0; 32]),
            1,
            [0; 32],
            &vrf_sk,
            &committee,
        ).unwrap();

        let sig = ValidatorSignature {
            validator: NodeId([0u8; 32]),
            signature: vec![0u8; 64],
        };

        assert!(pending.add_signature(sig.clone()).is_ok());
        assert!(pending.add_signature(sig).is_err()); // Duplicate
    }

    #[test]
    fn test_sign_and_verify_block() {
        let keypair = Keypair::generate();
        let node_id = keypair.node_id();

        let header = BlockHeader {
            height: 1,
            timestamp: Utc::now(),
            parent_hash: BlockHash([0; 32]),
            transactions_root: [0; 32],
            state_root: [0; 32],
            attestations_root: [0; 32],
            producer: node_id,
            vrf_proof: vec![],
            active_miners: 100,
            geographic_diversity: 5,
        };

        let sig = sign_block(&header, node_id, &keypair).unwrap();
        assert_eq!(sig.validator, node_id);

        // Verify the signature
        let result = verify_block_signature(&header, &sig, &keypair.public_key_bytes());
        assert!(result.is_ok());
    }

    #[test]
    fn test_verify_block_wrong_key() {
        let keypair1 = Keypair::generate();
        let keypair2 = Keypair::generate();
        let node_id = keypair1.node_id();

        let header = BlockHeader {
            height: 1,
            timestamp: Utc::now(),
            parent_hash: BlockHash([0; 32]),
            transactions_root: [0; 32],
            state_root: [0; 32],
            attestations_root: [0; 32],
            producer: node_id,
            vrf_proof: vec![],
            active_miners: 100,
            geographic_diversity: 5,
        };

        let sig = sign_block(&header, node_id, &keypair1).unwrap();

        // Verify with wrong key should fail
        let result = verify_block_signature(&header, &sig, &keypair2.public_key_bytes());
        assert!(result.is_err());
    }

    #[test]
    fn test_block_vrf_proof_included() {
        let committee = make_test_committee();
        let producer_id = committee.members[0].node_id;
        let vrf_sk = VrfSecretKey::generate(&mut OsRng);

        let mut bp = BlockProducer::new(producer_id, 100, 5);
        bp.set_slot(42);

        let pending = bp.produce_block(
            vec![],
            vec![],
            BlockHash([0xAA; 32]),
            1,
            [0; 32],
            &vrf_sk,
            &committee,
        ).unwrap();

        // VRF proof should be present in the header
        assert!(!pending.block.header.vrf_proof.is_empty());
        assert_eq!(pending.block.header.vrf_proof.len(), crate::vrf::VRF_PROOF_SIZE);
    }

    #[test]
    fn test_merkle_roots_computed() {
        let committee = make_test_committee();
        let producer_id = committee.members[0].node_id;
        let keypair = Keypair::generate();
        let vrf_sk = VrfSecretKey::generate(&mut OsRng);

        let tx = make_test_transaction(&keypair);
        let expected_tx_root = merkle_root(&[tx.hash.0]);

        let mut bp = BlockProducer::new(producer_id, 100, 5);
        bp.set_slot(0);

        let pending = bp.produce_block(
            vec![tx],
            vec![],
            BlockHash([0; 32]),
            1,
            [0xBB; 32],
            &vrf_sk,
            &committee,
        ).unwrap();

        assert_eq!(pending.block.header.transactions_root, expected_tx_root);
        assert_eq!(pending.block.header.state_root, [0xBB; 32]);
        // Empty attestations should give zero root
        assert_eq!(pending.block.header.attestations_root, [0; 32]);
    }

    #[test]
    fn test_update_network_stats() {
        let mut bp = BlockProducer::new(NodeId([0; 32]), 100, 5);
        assert_eq!(bp.active_miners, 100);
        assert_eq!(bp.geographic_diversity, 5);

        bp.update_network_stats(200, 10);
        assert_eq!(bp.active_miners, 200);
        assert_eq!(bp.geographic_diversity, 10);
    }
}
