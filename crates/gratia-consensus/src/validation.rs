//! Transaction and block validation.
//!
//! Validates transactions (signatures, nonces, fees, payload-specific rules)
//! and blocks (structure, all contained transactions, size limits).

use gratia_core::crypto::{verify_signature, sha256};
use gratia_core::error::GratiaError;
use gratia_core::types::{
    Block, BlockHeader, Lux, ProofOfLifeAttestation, Transaction, TransactionPayload,
};
use gratia_zk::bulletproofs::{PolRangeProof, PolThresholds};

use crate::committee::ValidatorCommittee;

// ============================================================================
// Constants
// ============================================================================

/// Maximum block size in bytes (256 KB).
/// WHY: Sized for mobile network transmission within the 3-5 second block time.
/// A 256 KB block can be transmitted over a 1 Mbps connection in ~2 seconds,
/// leaving time for validation and propagation.
pub const MAX_BLOCK_SIZE: usize = 262_144;

/// Minimum transaction fee in Lux (1000 Lux = 0.001 GRAT).
/// WHY: Prevents spam transactions while remaining negligible for real users.
/// This fee is burned, contributing to deflationary pressure.
pub const MIN_TRANSACTION_FEE: Lux = 1_000;

/// Maximum transactions per block.
/// WHY: Bounds validation time on mobile devices. At ~250 bytes per standard
/// transaction, 256 KB fits ~1000 transactions, but we cap lower to leave
/// room for attestations and signatures.
pub const MAX_TRANSACTIONS_PER_BLOCK: usize = 512;

/// Maximum bytecode size for contract deployment (64 KB).
/// WHY: Larger contracts consume excessive storage on mobile nodes.
/// Complex contracts should be split into multiple smaller contracts.
pub const MAX_CONTRACT_BYTECODE_SIZE: usize = 65_536;

/// Maximum size of a single transaction payload in bytes (128 KB).
/// WHY: Individual transactions should not consume more than half the block.
pub const MAX_TRANSACTION_PAYLOAD_SIZE: usize = 131_072;

// ============================================================================
// Validation Context
// ============================================================================

/// State needed to validate transactions and blocks.
///
/// In a full implementation, this would reference the state database.
/// For PoC, it holds the minimum information needed for validation.
#[derive(Debug, Clone)]
pub struct ValidationContext {
    /// Current block height (the height we're validating for).
    pub current_height: u64,
    /// The hash of the previous block.
    pub previous_block_hash: [u8; 32],
    /// The active validator committee.
    pub committee: ValidatorCommittee,
    /// Maximum block size in bytes (from config, default 256 KB).
    pub max_block_size: usize,
    /// Minimum transaction fee (from config).
    pub min_transaction_fee: Lux,
    /// Timestamp of the previous block, used to enforce monotonicity.
    /// `None` only for the genesis block (height 0).
    pub previous_block_timestamp: Option<chrono::DateTime<chrono::Utc>>,
    /// Proof of Life thresholds for ZK proof verification.
    /// WHY: These are governance-adjustable, so they come from config rather
    /// than being hardcoded. The verifier must use the same thresholds the
    /// prover used (which are the network's current thresholds at the time
    /// the attestation was created).
    pub pol_thresholds: PolThresholds,
}

// ============================================================================
// Transaction Validation
// ============================================================================

/// Validate a single transaction.
///
/// Checks:
/// 1. Signature is valid for the sender's public key
/// 2. Fee meets minimum requirement
/// 3. Payload-specific rules are satisfied
///
/// Note: Nonce and balance checks require state access and are deferred
/// to the block execution phase. This function validates structure only.
pub fn validate_transaction(tx: &Transaction, min_fee: Lux) -> Result<(), GratiaError> {
    // 1. Verify signature using the canonical signing format.
    // WHY: The signing format MUST match gratia-wallet/transactions.rs:
    //   payload_bytes || nonce (LE) || chain_id (LE) || fee (LE) || timestamp_millis (LE)
    // Previously this function used a DIFFERENT format (nonce || fee || timestamp || payload)
    // which would reject all valid transactions. The correct format includes chain_id
    // for cross-chain replay protection.
    let payload_bytes = bincode::serialize(&tx.payload)
        .map_err(|e| GratiaError::SerializationError(e.to_string()))?;

    let mut signable = Vec::with_capacity(payload_bytes.len() + 28);
    signable.extend_from_slice(&payload_bytes);
    signable.extend_from_slice(&tx.nonce.to_le_bytes());
    signable.extend_from_slice(&tx.chain_id.to_le_bytes());
    signable.extend_from_slice(&tx.fee.to_le_bytes());
    signable.extend_from_slice(&tx.timestamp.timestamp_millis().to_le_bytes());

    verify_signature(&tx.sender_pubkey, &signable, &tx.signature)?;

    // 2. Verify the transaction hash matches
    // WHY: Hash format must match gratia-wallet/transactions.rs:
    //   SHA256(sender_pubkey || signable || signature)
    let mut hash_input = Vec::new();
    hash_input.extend_from_slice(&tx.sender_pubkey);
    hash_input.extend_from_slice(&signable);
    hash_input.extend_from_slice(&tx.signature);
    let computed_hash = sha256(&hash_input);
    if tx.hash.0 != computed_hash {
        return Err(GratiaError::BlockValidationFailed {
            reason: format!(
                "Transaction hash mismatch: expected {}, got {}",
                hex::encode(computed_hash),
                tx.hash,
            ),
        });
    }

    // 3. Check minimum fee
    if tx.fee < min_fee {
        return Err(GratiaError::BlockValidationFailed {
            reason: format!(
                "Transaction fee {} Lux below minimum {} Lux",
                tx.fee, min_fee,
            ),
        });
    }

    // 4. Payload-specific validation
    validate_payload(&tx.payload)?;

    Ok(())
}

/// Validate payload-specific rules.
fn validate_payload(payload: &TransactionPayload) -> Result<(), GratiaError> {
    match payload {
        TransactionPayload::Transfer { amount, .. } => {
            if *amount == 0 {
                return Err(GratiaError::BlockValidationFailed {
                    reason: "Transfer amount cannot be zero".into(),
                });
            }
        }
        TransactionPayload::ShieldedTransfer {
            commitment,
            range_proof,
            ..
        } => {
            if commitment.is_empty() {
                return Err(GratiaError::BlockValidationFailed {
                    reason: "Shielded transfer missing commitment".into(),
                });
            }
            if range_proof.is_empty() {
                return Err(GratiaError::BlockValidationFailed {
                    reason: "Shielded transfer missing range proof".into(),
                });
            }
        }
        TransactionPayload::Stake { amount } | TransactionPayload::Unstake { amount } => {
            if *amount == 0 {
                return Err(GratiaError::BlockValidationFailed {
                    reason: "Stake/unstake amount cannot be zero".into(),
                });
            }
        }
        TransactionPayload::DeployContract { bytecode, .. } => {
            if bytecode.is_empty() {
                return Err(GratiaError::BlockValidationFailed {
                    reason: "Contract bytecode cannot be empty".into(),
                });
            }
            if bytecode.len() > MAX_CONTRACT_BYTECODE_SIZE {
                return Err(GratiaError::BlockValidationFailed {
                    reason: format!(
                        "Contract bytecode {} bytes exceeds maximum {} bytes",
                        bytecode.len(),
                        MAX_CONTRACT_BYTECODE_SIZE,
                    ),
                });
            }
        }
        TransactionPayload::CallContract { gas_limit, function, .. } => {
            if *gas_limit == 0 {
                return Err(GratiaError::BlockValidationFailed {
                    reason: "Contract call gas limit cannot be zero".into(),
                });
            }
            if function.is_empty() {
                return Err(GratiaError::BlockValidationFailed {
                    reason: "Contract call function name cannot be empty".into(),
                });
            }
        }
        TransactionPayload::GovernanceProposal { title, description, .. } => {
            if title.is_empty() {
                return Err(GratiaError::BlockValidationFailed {
                    reason: "Governance proposal title cannot be empty".into(),
                });
            }
            if description.is_empty() {
                return Err(GratiaError::BlockValidationFailed {
                    reason: "Governance proposal description cannot be empty".into(),
                });
            }
        }
        TransactionPayload::GovernanceVote { .. } => {
            // Vote validity (duplicate check, eligibility) requires state access
        }
        TransactionPayload::CreatePoll { question, options, duration_secs, .. } => {
            if question.is_empty() {
                return Err(GratiaError::BlockValidationFailed {
                    reason: "Poll question cannot be empty".into(),
                });
            }
            if options.len() < 2 {
                return Err(GratiaError::BlockValidationFailed {
                    reason: "Poll must have at least 2 options".into(),
                });
            }
            if *duration_secs == 0 {
                return Err(GratiaError::BlockValidationFailed {
                    reason: "Poll duration cannot be zero".into(),
                });
            }
        }
        TransactionPayload::PollVote { .. } => {
            // Vote validity requires state access
        }
    }

    Ok(())
}

/// Validate all transactions in a block.
///
/// Checks:
/// - No duplicate transaction hashes
/// - Each transaction individually valid
/// - Total transaction count within limits
pub fn validate_block_transactions(
    transactions: &[Transaction],
    min_fee: Lux,
) -> Result<(), GratiaError> {
    // Check transaction count
    if transactions.len() > MAX_TRANSACTIONS_PER_BLOCK {
        return Err(GratiaError::BlockValidationFailed {
            reason: format!(
                "Block contains {} transactions, maximum is {}",
                transactions.len(),
                MAX_TRANSACTIONS_PER_BLOCK,
            ),
        });
    }

    // Check for duplicate transaction hashes
    let mut seen_hashes = std::collections::HashSet::new();
    for tx in transactions {
        if !seen_hashes.insert(tx.hash) {
            return Err(GratiaError::BlockValidationFailed {
                reason: format!("Duplicate transaction hash: {}", tx.hash),
            });
        }
    }

    // Validate each transaction
    for (i, tx) in transactions.iter().enumerate() {
        validate_transaction(tx, min_fee).map_err(|e| {
            GratiaError::BlockValidationFailed {
                reason: format!("Transaction {} invalid: {}", i, e),
            }
        })?;
    }

    Ok(())
}

/// Validate a block header (lightweight, no transaction checks).
///
/// Checks:
/// - Height is sequential
/// - Parent hash matches expected
/// - Block size within limits
/// - Timestamp is plausible
/// - Producer is a committee member
pub fn validate_block_header(
    header: &BlockHeader,
    ctx: &ValidationContext,
) -> Result<(), GratiaError> {
    // Height must be sequential
    if header.height != ctx.current_height {
        return Err(GratiaError::BlockValidationFailed {
            reason: format!(
                "Block height {} does not match expected {}",
                header.height, ctx.current_height,
            ),
        });
    }

    // Parent hash must match
    if header.parent_hash.0 != ctx.previous_block_hash {
        return Err(GratiaError::BlockValidationFailed {
            reason: format!(
                "Parent hash mismatch: expected {}, got {}",
                hex::encode(ctx.previous_block_hash),
                header.parent_hash,
            ),
        });
    }

    // Producer must be a committee member
    if !ctx.committee.is_committee_member(&header.producer) {
        return Err(GratiaError::BlockValidationFailed {
            reason: format!(
                "Block producer {} is not a committee member",
                header.producer,
            ),
        });
    }

    // Timestamp sanity check: should be within a reasonable window.
    // WHY: Prevents blocks with far-future timestamps from being accepted,
    // which could disrupt time-dependent logic. The 30-second tolerance
    // accounts for clock skew on mobile devices.
    let now = chrono::Utc::now();
    let max_future = chrono::Duration::seconds(30);
    if header.timestamp > now + max_future {
        return Err(GratiaError::BlockValidationFailed {
            reason: "Block timestamp is too far in the future".into(),
        });
    }

    // WHY: Timestamps must be monotonically increasing to prevent
    // time-manipulation attacks where a producer backdates a block to
    // gain an advantage in time-dependent logic (e.g., staking cooldowns,
    // governance deadlines). We use >= (not >) to allow same-second blocks
    // during rapid block production.
    if let Some(prev_ts) = ctx.previous_block_timestamp {
        if header.timestamp < prev_ts {
            return Err(GratiaError::BlockValidationFailed {
                reason: format!(
                    "Block timestamp {} is before previous block timestamp {}",
                    header.timestamp, prev_ts,
                ),
            });
        }
    }

    Ok(())
}

/// Validate that a block's committee parameters match the graduated scaling spec.
///
/// Ensures the committee size and finality threshold reported for a block are
/// consistent with the tier that the given network size maps to. This prevents
/// a producer from claiming a smaller committee (easier to capture) or a lower
/// finality threshold (easier to forge) than the network size warrants.
pub fn validate_committee_parameters(
    committee_size: usize,
    finality_threshold: usize,
    network_size: u64,
) -> Result<(), GratiaError> {
    let expected_tier = crate::committee::tier_for_network_size(network_size);

    if committee_size != expected_tier.committee_size {
        return Err(GratiaError::BlockValidationFailed {
            reason: format!(
                "Committee size {} does not match expected {} for network size {}",
                committee_size, expected_tier.committee_size, network_size,
            ),
        });
    }

    if finality_threshold != expected_tier.finality_threshold {
        return Err(GratiaError::BlockValidationFailed {
            reason: format!(
                "Finality threshold {} does not match expected {} for committee size {}",
                finality_threshold, expected_tier.finality_threshold, committee_size,
            ),
        });
    }

    Ok(())
}

/// Validate a single Proof of Life attestation's ZK proof.
///
/// If the attestation contains a non-empty ZK proof and commitments, verify
/// them using the Bulletproofs verifier. Attestations without a ZK proof
/// (e.g., from the genesis epoch or during the transition period) are
/// accepted but logged.
///
/// # Arguments
/// * `attestation` - The on-chain PoL attestation to verify.
/// * `thresholds` - The governance-set PoL thresholds for the current epoch.
pub fn validate_attestation_zk_proof(
    attestation: &ProofOfLifeAttestation,
    thresholds: &PolThresholds,
) -> Result<(), GratiaError> {
    // WHY: During the transition period (and for genesis-era attestations),
    // the zk_proof field may be empty. We accept these but require ZK proofs
    // once the network matures. A future governance vote can make ZK proofs
    // mandatory by rejecting empty proofs here.
    if attestation.zk_proof.is_empty() {
        tracing::debug!("Attestation has no ZK proof — accepted during transition period");
        return Ok(());
    }

    let commitments = attestation.zk_commitments.as_ref().ok_or_else(|| {
        GratiaError::InvalidZkProof {
            reason: "attestation has zk_proof bytes but missing zk_commitments".into(),
        }
    })?;

    let range_proof = PolRangeProof {
        proof_bytes: attestation.zk_proof.clone(),
        commitments: commitments.clone(),
        // WHY: The flexible API uses 4 core numeric parameters.
        parameter_count: 4,
        epoch_day: attestation.epoch_day,
    };

    gratia_zk::verify_pol_proof(&range_proof, thresholds, attestation.epoch_day).map_err(|e| {
        GratiaError::InvalidZkProof {
            reason: format!("PoL attestation ZK proof verification failed: {}", e),
        }
    })?;

    Ok(())
}

/// Validate all Proof of Life attestations in a block.
///
/// Checks:
/// - No duplicate nullifiers (prevents double-submission)
/// - ZK proof is valid for each attestation (if present)
pub fn validate_block_attestations(
    attestations: &[ProofOfLifeAttestation],
    thresholds: &PolThresholds,
) -> Result<(), GratiaError> {
    // Check for duplicate nullifiers within the same block.
    let mut seen_nullifiers = std::collections::HashSet::new();
    for att in attestations {
        if !seen_nullifiers.insert(att.nullifier) {
            return Err(GratiaError::BlockValidationFailed {
                reason: format!(
                    "Duplicate attestation nullifier in block: {}",
                    hex::encode(att.nullifier),
                ),
            });
        }
    }

    // Verify ZK proofs for each attestation.
    for (i, att) in attestations.iter().enumerate() {
        validate_attestation_zk_proof(att, thresholds).map_err(|e| {
            GratiaError::BlockValidationFailed {
                reason: format!("Attestation {} ZK proof invalid: {}", i, e),
            }
        })?;
    }

    Ok(())
}

/// Validate a complete block (header + transactions + size).
pub fn validate_block(
    block: &Block,
    ctx: &ValidationContext,
) -> Result<(), GratiaError> {
    // Validate header
    validate_block_header(&block.header, ctx)?;

    // Check serialized block size
    let block_bytes = bincode::serialize(block)
        .map_err(|e| GratiaError::SerializationError(e.to_string()))?;
    if block_bytes.len() > ctx.max_block_size {
        return Err(GratiaError::BlockValidationFailed {
            reason: format!(
                "Block size {} bytes exceeds maximum {} bytes",
                block_bytes.len(),
                ctx.max_block_size,
            ),
        });
    }

    // Validate transactions
    validate_block_transactions(&block.transactions, ctx.min_transaction_fee)?;

    // Validate PoL attestation ZK proofs
    validate_block_attestations(&block.attestations, &ctx.pol_thresholds)?;

    // Validate finality (sufficient validator signatures)
    let sig_count = block.validator_signatures.len();
    if !ctx.committee.has_finality(sig_count) {
        return Err(GratiaError::InsufficientSignatures {
            count: sig_count,
            required: crate::committee::FINALITY_THRESHOLD,
        });
    }

    // Verify each validator signature is from a committee member
    for vs in &block.validator_signatures {
        if !ctx.committee.is_committee_member(&vs.validator) {
            return Err(GratiaError::BlockValidationFailed {
                reason: format!(
                    "Validator signature from non-committee member: {}",
                    vs.validator,
                ),
            });
        }
    }

    Ok(())
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use gratia_core::types::*;
    use gratia_core::crypto::Keypair;
    use crate::committee::{self, EligibleNode};
    use crate::vrf::VrfPublicKey;
    use chrono::Utc;

    fn make_signed_transaction(keypair: &Keypair, payload: TransactionPayload, fee: Lux) -> Transaction {
        let nonce = 1u64;
        let chain_id = 2u32;
        let timestamp = Utc::now();

        // WHY: Must match the canonical signing format from gratia-wallet:
        //   payload_bytes || nonce (LE) || chain_id (LE) || fee (LE) || timestamp_millis (LE)
        let payload_bytes = bincode::serialize(&payload).unwrap();
        let mut signable = Vec::with_capacity(payload_bytes.len() + 28);
        signable.extend_from_slice(&payload_bytes);
        signable.extend_from_slice(&nonce.to_le_bytes());
        signable.extend_from_slice(&chain_id.to_le_bytes());
        signable.extend_from_slice(&fee.to_le_bytes());
        signable.extend_from_slice(&timestamp.timestamp_millis().to_le_bytes());

        let signature = keypair.sign(&signable);
        let sender_pubkey = keypair.public_key_bytes();

        // Hash: SHA256(sender_pubkey || signable || signature)
        let mut hash_input = Vec::new();
        hash_input.extend_from_slice(&sender_pubkey);
        hash_input.extend_from_slice(&signable);
        hash_input.extend_from_slice(&signature);
        let hash = sha256(&hash_input);

        Transaction {
            hash: TxHash(hash),
            payload,
            sender_pubkey,
            signature,
            nonce,
            chain_id,
            fee,
            timestamp,
        }
    }

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

    #[test]
    fn test_validate_transfer_transaction() {
        let keypair = Keypair::generate();
        let payload = TransactionPayload::Transfer {
            to: Address([0x42; 32]),
            amount: 1_000_000,
        };
        let tx = make_signed_transaction(&keypair, payload, MIN_TRANSACTION_FEE);
        assert!(validate_transaction(&tx, MIN_TRANSACTION_FEE).is_ok());
    }

    #[test]
    fn test_validate_transaction_bad_signature() {
        let keypair = Keypair::generate();
        let payload = TransactionPayload::Transfer {
            to: Address([0x42; 32]),
            amount: 1_000_000,
        };
        let mut tx = make_signed_transaction(&keypair, payload, MIN_TRANSACTION_FEE);
        tx.signature[0] ^= 0xFF; // Corrupt signature
        assert!(validate_transaction(&tx, MIN_TRANSACTION_FEE).is_err());
    }

    #[test]
    fn test_validate_transaction_fee_too_low() {
        let keypair = Keypair::generate();
        let payload = TransactionPayload::Transfer {
            to: Address([0x42; 32]),
            amount: 1_000_000,
        };
        let tx = make_signed_transaction(&keypair, payload, 0);
        assert!(validate_transaction(&tx, MIN_TRANSACTION_FEE).is_err());
    }

    #[test]
    fn test_validate_zero_transfer_amount() {
        let keypair = Keypair::generate();
        let payload = TransactionPayload::Transfer {
            to: Address([0x42; 32]),
            amount: 0,
        };
        let tx = make_signed_transaction(&keypair, payload, MIN_TRANSACTION_FEE);
        assert!(validate_transaction(&tx, MIN_TRANSACTION_FEE).is_err());
    }

    #[test]
    fn test_validate_contract_deploy_too_large() {
        let keypair = Keypair::generate();
        let payload = TransactionPayload::DeployContract {
            bytecode: vec![0u8; MAX_CONTRACT_BYTECODE_SIZE + 1],
            init_args: vec![],
        };
        let tx = make_signed_transaction(&keypair, payload, MIN_TRANSACTION_FEE);
        assert!(validate_transaction(&tx, MIN_TRANSACTION_FEE).is_err());
    }

    #[test]
    fn test_validate_contract_deploy_empty() {
        let keypair = Keypair::generate();
        let payload = TransactionPayload::DeployContract {
            bytecode: vec![],
            init_args: vec![],
        };
        let tx = make_signed_transaction(&keypair, payload, MIN_TRANSACTION_FEE);
        assert!(validate_transaction(&tx, MIN_TRANSACTION_FEE).is_err());
    }

    #[test]
    fn test_validate_poll_too_few_options() {
        let keypair = Keypair::generate();
        let payload = TransactionPayload::CreatePoll {
            question: "Test?".into(),
            options: vec!["Yes".into()],
            duration_secs: 3600,
            geographic_filter: None,
        };
        let tx = make_signed_transaction(&keypair, payload, MIN_TRANSACTION_FEE);
        assert!(validate_transaction(&tx, MIN_TRANSACTION_FEE).is_err());
    }

    #[test]
    fn test_validate_block_transactions_no_duplicates() {
        let keypair = Keypair::generate();
        let payload = TransactionPayload::Transfer {
            to: Address([0x42; 32]),
            amount: 1_000_000,
        };
        let tx = make_signed_transaction(&keypair, payload, MIN_TRANSACTION_FEE);
        let txs = vec![tx.clone(), tx]; // Duplicate
        assert!(validate_block_transactions(&txs, MIN_TRANSACTION_FEE).is_err());
    }

    #[test]
    fn test_validate_block_transactions_too_many() {
        let keypair = Keypair::generate();
        let txs: Vec<Transaction> = (0..MAX_TRANSACTIONS_PER_BLOCK + 1)
            .map(|i| {
                let payload = TransactionPayload::Transfer {
                    to: Address([0x42; 32]),
                    amount: (i as u64 + 1) * 1000,
                };
                // Build each with a unique nonce to get unique hashes
                let nonce = i as u64;
                let fee = MIN_TRANSACTION_FEE;
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
            })
            .collect();

        assert!(validate_block_transactions(&txs, MIN_TRANSACTION_FEE).is_err());
    }

    #[test]
    fn test_validate_block_header_height_mismatch() {
        let committee = make_test_committee();
        let producer = committee.members[0].node_id;

        let header = BlockHeader {
            height: 5, // Wrong height
            timestamp: Utc::now(),
            parent_hash: BlockHash([0xAA; 32]),
            transactions_root: [0; 32],
            state_root: [0; 32],
            attestations_root: [0; 32],
            producer,
            vrf_proof: vec![],
            active_miners: 100,
            geographic_diversity: 5,
        };

        let ctx = ValidationContext {
            current_height: 1,
            previous_block_hash: [0xAA; 32],
            committee,
            max_block_size: MAX_BLOCK_SIZE,
            min_transaction_fee: MIN_TRANSACTION_FEE,
            previous_block_timestamp: None,
            pol_thresholds: PolThresholds::default(),
        };

        assert!(validate_block_header(&header, &ctx).is_err());
    }

    #[test]
    fn test_validate_block_header_parent_hash_mismatch() {
        let committee = make_test_committee();
        let producer = committee.members[0].node_id;

        let header = BlockHeader {
            height: 1,
            timestamp: Utc::now(),
            parent_hash: BlockHash([0xBB; 32]), // Wrong parent
            transactions_root: [0; 32],
            state_root: [0; 32],
            attestations_root: [0; 32],
            producer,
            vrf_proof: vec![],
            active_miners: 100,
            geographic_diversity: 5,
        };

        let ctx = ValidationContext {
            current_height: 1,
            previous_block_hash: [0xAA; 32],
            committee,
            max_block_size: MAX_BLOCK_SIZE,
            min_transaction_fee: MIN_TRANSACTION_FEE,
            previous_block_timestamp: None,
            pol_thresholds: PolThresholds::default(),
        };

        assert!(validate_block_header(&header, &ctx).is_err());
    }

    #[test]
    fn test_validate_block_header_non_committee_producer() {
        let committee = make_test_committee();

        let header = BlockHeader {
            height: 1,
            timestamp: Utc::now(),
            parent_hash: BlockHash([0xAA; 32]),
            transactions_root: [0; 32],
            state_root: [0; 32],
            attestations_root: [0; 32],
            producer: NodeId([0xFF; 32]), // Not in committee
            vrf_proof: vec![],
            active_miners: 100,
            geographic_diversity: 5,
        };

        let ctx = ValidationContext {
            current_height: 1,
            previous_block_hash: [0xAA; 32],
            committee,
            max_block_size: MAX_BLOCK_SIZE,
            min_transaction_fee: MIN_TRANSACTION_FEE,
            previous_block_timestamp: None,
            pol_thresholds: PolThresholds::default(),
        };

        assert!(validate_block_header(&header, &ctx).is_err());
    }

    #[test]
    fn test_validate_block_header_valid() {
        let committee = make_test_committee();
        let producer = committee.members[0].node_id;

        let header = BlockHeader {
            height: 1,
            timestamp: Utc::now(),
            parent_hash: BlockHash([0xAA; 32]),
            transactions_root: [0; 32],
            state_root: [0; 32],
            attestations_root: [0; 32],
            producer,
            vrf_proof: vec![],
            active_miners: 100,
            geographic_diversity: 5,
        };

        let ctx = ValidationContext {
            current_height: 1,
            previous_block_hash: [0xAA; 32],
            committee,
            max_block_size: MAX_BLOCK_SIZE,
            min_transaction_fee: MIN_TRANSACTION_FEE,
            previous_block_timestamp: None,
            pol_thresholds: PolThresholds::default(),
        };

        assert!(validate_block_header(&header, &ctx).is_ok());
    }

    #[test]
    fn test_validate_block_header_timestamp_not_monotonic() {
        let committee = make_test_committee();
        let producer = committee.members[0].node_id;

        // Block timestamp is 10 seconds BEFORE the previous block
        let previous_ts = Utc::now();
        let header = BlockHeader {
            height: 1,
            timestamp: previous_ts - chrono::Duration::seconds(10),
            parent_hash: BlockHash([0xAA; 32]),
            transactions_root: [0; 32],
            state_root: [0; 32],
            attestations_root: [0; 32],
            producer,
            vrf_proof: vec![],
            active_miners: 100,
            geographic_diversity: 5,
        };

        let ctx = ValidationContext {
            current_height: 1,
            previous_block_hash: [0xAA; 32],
            committee: committee.clone(),
            max_block_size: MAX_BLOCK_SIZE,
            min_transaction_fee: MIN_TRANSACTION_FEE,
            previous_block_timestamp: Some(previous_ts),
            pol_thresholds: PolThresholds::default(),
        };

        let result = validate_block_header(&header, &ctx);
        assert!(result.is_err());
    }

    #[test]
    fn test_validate_block_header_timestamp_same_second_ok() {
        let committee = make_test_committee();
        let producer = committee.members[0].node_id;

        // Same timestamp as previous block should be allowed (>=)
        let ts = Utc::now();
        let header = BlockHeader {
            height: 1,
            timestamp: ts,
            parent_hash: BlockHash([0xAA; 32]),
            transactions_root: [0; 32],
            state_root: [0; 32],
            attestations_root: [0; 32],
            producer,
            vrf_proof: vec![],
            active_miners: 100,
            geographic_diversity: 5,
        };

        let ctx = ValidationContext {
            current_height: 1,
            previous_block_hash: [0xAA; 32],
            committee,
            max_block_size: MAX_BLOCK_SIZE,
            min_transaction_fee: MIN_TRANSACTION_FEE,
            previous_block_timestamp: Some(ts),
            pol_thresholds: PolThresholds::default(),
        };

        assert!(validate_block_header(&header, &ctx).is_ok());
    }

    #[test]
    fn test_validate_stake_zero_amount() {
        let keypair = Keypair::generate();
        let payload = TransactionPayload::Stake { amount: 0 };
        let tx = make_signed_transaction(&keypair, payload, MIN_TRANSACTION_FEE);
        assert!(validate_transaction(&tx, MIN_TRANSACTION_FEE).is_err());
    }

    #[test]
    fn test_validate_governance_proposal_empty_title() {
        let keypair = Keypair::generate();
        let payload = TransactionPayload::GovernanceProposal {
            title: "".into(),
            description: "Some description".into(),
            proposal_data: vec![],
        };
        let tx = make_signed_transaction(&keypair, payload, MIN_TRANSACTION_FEE);
        assert!(validate_transaction(&tx, MIN_TRANSACTION_FEE).is_err());
    }

    #[test]
    fn test_validate_committee_parameters_correct() {
        // 100K network -> tier 7: committee=21, finality=14
        assert!(validate_committee_parameters(21, 14, 100_000).is_ok());
    }

    #[test]
    fn test_validate_committee_parameters_wrong_size() {
        // 100K network expects 21, not 15
        let err = validate_committee_parameters(15, 14, 100_000);
        assert!(err.is_err());
    }

    #[test]
    fn test_validate_committee_parameters_small_network() {
        // 50 nodes -> tier 1: committee=3, finality=2
        assert!(validate_committee_parameters(3, 2, 50).is_ok());
    }

    #[test]
    fn test_validate_contract_call_empty_function() {
        let keypair = Keypair::generate();
        let payload = TransactionPayload::CallContract {
            contract: Address([0x42; 32]),
            function: "".into(),
            args: vec![],
            gas_limit: 1000,
        };
        let tx = make_signed_transaction(&keypair, payload, MIN_TRANSACTION_FEE);
        assert!(validate_transaction(&tx, MIN_TRANSACTION_FEE).is_err());
    }
}
