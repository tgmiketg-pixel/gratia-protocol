//! Core types for the Gratia protocol.
//!
//! All fundamental data structures that flow through the protocol are defined here.
//! These types are serializable and designed to be compact for mobile network transmission.

use ed25519_dalek::VerifyingKey;
use serde::{Deserialize, Serialize};
use sha2::{Sha256, Digest};
use chrono::{DateTime, Utc};

use crate::error::GratiaError;

// ============================================================================
// Token Units
// ============================================================================

/// The smallest unit of GRAT. 1 GRAT = 1,000,000 Lux.
pub type Lux = u64;

/// Number of Lux per GRAT.
pub const LUX_PER_GRAT: Lux = 1_000_000;

/// Convert GRAT (as f64) to Lux.
pub fn grat_to_lux(grat: f64) -> Lux {
    (grat * LUX_PER_GRAT as f64) as Lux
}

/// Convert Lux to GRAT (as f64).
pub fn lux_to_grat(lux: Lux) -> f64 {
    lux as f64 / LUX_PER_GRAT as f64
}

// ============================================================================
// Identity
// ============================================================================

/// A 32-byte node identifier derived from the node's public key.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct NodeId(pub [u8; 32]);

impl NodeId {
    /// Derive a NodeId from an Ed25519 public key.
    pub fn from_public_key(key: &VerifyingKey) -> Self {
        let mut hasher = Sha256::new();
        hasher.update(key.as_bytes());
        let result = hasher.finalize();
        let mut id = [0u8; 32];
        id.copy_from_slice(&result);
        NodeId(id)
    }

    /// Display as hex string (first 8 chars for brevity).
    pub fn short_hex(&self) -> String {
        hex::encode(&self.0[..4])
    }
}

impl std::fmt::Display for NodeId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", hex::encode(&self.0))
    }
}

/// A wallet address, derived from the public key.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Address(pub [u8; 32]);

impl Address {
    /// Derive an address from an Ed25519 public key.
    pub fn from_public_key(key: &VerifyingKey) -> Self {
        let mut hasher = Sha256::new();
        hasher.update(b"gratia-address-v1:");
        hasher.update(key.as_bytes());
        let result = hasher.finalize();
        let mut addr = [0u8; 32];
        addr.copy_from_slice(&result);
        Address(addr)
    }

    /// Derive an address from raw Ed25519 public key bytes.
    ///
    /// WHY: When receiving a transaction via gossipsub, we only have the raw
    /// sender_pubkey bytes (Vec<u8>). This avoids requiring callers to construct
    /// a VerifyingKey just to derive an address.
    pub fn from_pubkey(pubkey_bytes: &[u8]) -> Self {
        let mut hasher = Sha256::new();
        hasher.update(b"gratia-address-v1:");
        hasher.update(pubkey_bytes);
        let result = hasher.finalize();
        let mut addr = [0u8; 32];
        addr.copy_from_slice(&result);
        Address(addr)
    }
}

impl std::fmt::Display for Address {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "grat:{}", hex::encode(&self.0))
    }
}

// ============================================================================
// Block Types
// ============================================================================

/// Hash of a block (SHA-256).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
pub struct BlockHash(pub [u8; 32]);

impl std::fmt::Display for BlockHash {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", hex::encode(&self.0))
    }
}

/// Hash of a transaction (SHA-256).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct TxHash(pub [u8; 32]);

impl std::fmt::Display for TxHash {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", hex::encode(&self.0))
    }
}

/// Block header — compact representation for propagation and validation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BlockHeader {
    /// Block height (0-indexed from genesis).
    pub height: u64,
    /// Timestamp of block production.
    pub timestamp: DateTime<Utc>,
    /// Hash of the previous block header.
    pub parent_hash: BlockHash,
    /// Merkle root of all transactions in this block.
    pub transactions_root: [u8; 32],
    /// Merkle root of the state trie after applying this block.
    pub state_root: [u8; 32],
    /// Merkle root of Proof of Life attestations included in this block.
    pub attestations_root: [u8; 32],
    /// NodeId of the block producer (selected by VRF).
    pub producer: NodeId,
    /// VRF proof that the producer was legitimately selected.
    pub vrf_proof: Vec<u8>,
    /// Number of active mining nodes at time of block production.
    pub active_miners: u64,
    /// Geographic distribution metric (number of distinct geographic shards represented).
    pub geographic_diversity: u16,
}

impl BlockHeader {
    /// Compute the hash of this block header.
    ///
    /// Returns `Err` if the header cannot be serialized (e.g., corrupt data).
    /// Previously this used `.expect()` which would panic and crash the node.
    pub fn hash(&self) -> Result<BlockHash, GratiaError> {
        let encoded = bincode::serialize(self)
            .map_err(|e| GratiaError::SerializationError(
                format!("block header serialization failed: {}", e),
            ))?;
        let mut hasher = Sha256::new();
        hasher.update(&encoded);
        let result = hasher.finalize();
        let mut hash = [0u8; 32];
        hash.copy_from_slice(&result);
        Ok(BlockHash(hash))
    }
}

/// A complete block including header, transactions, and validator signatures.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Block {
    pub header: BlockHeader,
    pub transactions: Vec<Transaction>,
    pub attestations: Vec<ProofOfLifeAttestation>,
    /// Signatures from the 21-member validator committee.
    /// Finality requires 14/21 (67%).
    pub validator_signatures: Vec<ValidatorSignature>,
}

/// A validator's signature on a block.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ValidatorSignature {
    pub validator: NodeId,
    pub signature: Vec<u8>,
}

// ============================================================================
// Transaction Types
// ============================================================================

/// A transaction on the Gratia network.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Transaction {
    /// Unique transaction hash.
    pub hash: TxHash,
    /// Transaction payload.
    pub payload: TransactionPayload,
    /// Sender's public key.
    pub sender_pubkey: Vec<u8>,
    /// Ed25519 signature over the payload.
    pub signature: Vec<u8>,
    /// Transaction nonce (prevents replay).
    pub nonce: u64,
    /// Fee in Lux.
    pub fee: Lux,
    /// Timestamp.
    pub timestamp: DateTime<Utc>,
}

/// The payload of a transaction — what it actually does.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum TransactionPayload {
    /// Standard transparent transfer (sender, receiver, amount visible on chain).
    Transfer {
        to: Address,
        amount: Lux,
    },
    /// Shielded transfer using Bulletproofs + Pedersen commitments.
    /// Amount and parties are hidden; ZK proof validates correctness.
    ShieldedTransfer {
        /// Pedersen commitment to the transfer amount.
        commitment: Vec<u8>,
        /// Bulletproof range proof (proves amount is positive and within bounds).
        range_proof: Vec<u8>,
        /// Encrypted recipient data (only decryptable by recipient).
        encrypted_recipient: Vec<u8>,
    },
    /// Stake GRAT for mining eligibility.
    Stake {
        amount: Lux,
    },
    /// Unstake GRAT (subject to cooldown period).
    Unstake {
        amount: Lux,
    },
    /// Deploy a smart contract.
    DeployContract {
        /// WASM bytecode of the contract.
        bytecode: Vec<u8>,
        /// Constructor arguments.
        init_args: Vec<u8>,
    },
    /// Call a smart contract function.
    CallContract {
        /// Address of the deployed contract.
        contract: Address,
        /// Function name.
        function: String,
        /// Encoded arguments.
        args: Vec<u8>,
        /// Gas limit in Lux.
        gas_limit: Lux,
    },
    /// Submit a governance proposal.
    GovernanceProposal {
        title: String,
        description: String,
        /// Encoded proposal payload (parameter change, code upgrade, etc.).
        proposal_data: Vec<u8>,
    },
    /// Cast a governance vote. One phone, one vote.
    GovernanceVote {
        proposal_id: [u8; 32],
        vote: Vote,
    },
    /// Create an on-chain poll.
    CreatePoll {
        question: String,
        options: Vec<String>,
        /// Duration in seconds.
        duration_secs: u64,
        /// Optional geographic filter (only nodes in specific regions can vote).
        geographic_filter: Option<GeographicFilter>,
    },
    /// Cast a vote in an on-chain poll.
    PollVote {
        poll_id: [u8; 32],
        /// Index of selected option.
        option_index: u32,
    },
}

/// Governance vote options.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Vote {
    Yes,
    No,
    Abstain,
}

/// Geographic filter for polls.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GeographicFilter {
    /// Center latitude.
    pub lat: f64,
    /// Center longitude.
    pub lon: f64,
    /// Radius in kilometers.
    pub radius_km: f64,
}

// ============================================================================
// Proof of Life Types
// ============================================================================

/// A Proof of Life attestation included in blocks.
///
/// This is the ON-CHAIN form. It proves that SOME valid node produced
/// valid PoL without revealing WHICH node or WHEN (beyond the block it
/// appears in). This implements the "unlinkable attestations between days"
/// privacy guarantee from the whitepaper.
///
/// Privacy properties:
/// - `blinded_id`: Hash of (node_id + daily_secret), different each day
/// - `nullifier`: Hash of (node_id + epoch), prevents double-submission
///   within an epoch without linking across epochs
/// - No plaintext node_id or date
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProofOfLifeAttestation {
    /// Blinded identifier — hash of (node_id + daily_randomness).
    /// Different every day, unlinkable across days.
    /// WHY: Proves this attestation came from a unique node without
    /// revealing which node. Two attestations from different days
    /// produce different blinded_ids.
    pub blinded_id: [u8; 32],
    /// Nullifier — hash of (node_id + epoch_number).
    /// Same within an epoch, prevents double-submission.
    /// WHY: The network can detect if the same node submitted two
    /// attestations in the same epoch, but cannot link attestations
    /// across different epochs.
    pub nullifier: [u8; 32],
    /// ZK proof that all required PoL parameters were met.
    pub zk_proof: Vec<u8>,
    /// Composite Presence Score (40-100).
    /// WHY: The score is included because it affects VRF weighting
    /// but doesn't reveal identity. Many nodes share the same score.
    pub presence_score: u8,
    /// Which optional sensors contributed to the score.
    pub sensor_flags: SensorFlags,
    /// Signature over the attestation using a daily ephemeral key.
    /// WHY: Using an ephemeral key (derived from node key + daily seed)
    /// prevents linking via signature analysis.
    pub signature: Vec<u8>,
}

/// Local Proof of Life record — used on-device only, NEVER broadcast.
/// Contains the linkable identity information needed for local
/// eligibility checks and behavioral analysis.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LocalProofOfLifeRecord {
    /// The node that produced this record.
    pub node_id: NodeId,
    /// The day this record covers.
    pub date: chrono::NaiveDate,
    /// Whether PoL was valid this day.
    pub valid: bool,
    /// The blinded_id used in the on-chain attestation (for cross-reference).
    pub blinded_id: [u8; 32],
    /// The nullifier used (for cross-reference).
    pub nullifier: [u8; 32],
    /// Composite Presence Score.
    pub presence_score: u8,
}

/// Bitflags indicating which sensors contributed to the Presence Score.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct SensorFlags {
    /// Core (required)
    pub gps: bool,
    pub accelerometer: bool,
    pub wifi: bool,
    pub bluetooth: bool,
    /// Standard (optional, boosts score)
    pub gyroscope: bool,
    pub ambient_light: bool,
    pub cellular: bool,
    pub barometer: bool,
    pub magnetometer: bool,
    pub nfc: bool,
    pub secure_enclave: bool,
    pub biometric: bool,
    /// Enhanced (opt-in)
    pub camera_hash: bool,
    pub microphone_hash: bool,
}

impl SensorFlags {
    /// Calculate the Presence Score from available sensors.
    pub fn calculate_score(&self, participation_days: u64) -> u8 {
        let mut score: u8 = 40; // Base score for core four

        if self.gyroscope { score = score.saturating_add(5); }
        if self.ambient_light { score = score.saturating_add(3); }
        if self.bluetooth && self.wifi { score = score.saturating_add(5); } // Both, not just one
        if self.cellular { score = score.saturating_add(8); }
        if self.barometer { score = score.saturating_add(5); }
        if self.magnetometer { score = score.saturating_add(4); }
        if self.nfc { score = score.saturating_add(5); }
        if self.secure_enclave { score = score.saturating_add(8); }
        if self.biometric { score = score.saturating_add(5); }
        if self.camera_hash { score = score.saturating_add(4); }
        if self.microphone_hash { score = score.saturating_add(4); }

        // Participation history bonuses
        if participation_days >= 30 { score = score.saturating_add(2); }
        if participation_days >= 90 { score = score.saturating_add(2); }

        score.min(100) // Cap at 100
    }
}

// ============================================================================
// Proof of Life Parameters (what constitutes a valid day)
// ============================================================================

/// Raw sensor data collected on-device. NEVER leaves the phone.
/// This struct exists only in the app's local storage and is used
/// to generate the ZK attestation. It is defined here for type
/// consistency but is processed exclusively on-device.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DailyProofOfLifeData {
    /// Number of unlock events throughout the day.
    pub unlock_count: u32,
    /// Earliest unlock timestamp.
    pub first_unlock: Option<DateTime<Utc>>,
    /// Latest unlock timestamp.
    pub last_unlock: Option<DateTime<Utc>>,
    /// Number of screen interaction sessions.
    pub interaction_sessions: u32,
    /// Whether at least one orientation change was detected.
    pub orientation_changed: bool,
    /// Whether accelerometer showed human-consistent motion.
    pub human_motion_detected: bool,
    /// Whether a GPS fix was obtained.
    pub gps_fix_obtained: bool,
    /// Approximate location (used only for geographic shard assignment, never transmitted at precision).
    pub approximate_location: Option<GeoLocation>,
    /// Number of distinct Wi-Fi BSSIDs seen.
    pub distinct_wifi_networks: u32,
    /// Number of distinct Bluetooth peer sets observed.
    pub distinct_bt_environments: u32,
    /// Whether at least one charge cycle event (plug/unplug) occurred.
    pub charge_cycle_event: bool,
    /// Optional sensor readings for score enhancement.
    pub optional_sensors: OptionalSensorData,
}

impl DailyProofOfLifeData {
    /// Check if all required Proof of Life parameters are met.
    ///
    /// Uses thresholds from `ProofOfLifeConfig` so that governance can
    /// adjust requirements without code changes.
    pub fn is_valid(&self, config: &crate::config::ProofOfLifeConfig) -> bool {
        // 1. Minimum unlock events spread across the configured hour window
        let unlock_spread = match (self.first_unlock, self.last_unlock) {
            (Some(first), Some(last)) => {
                (last - first).num_hours() >= config.min_unlock_spread_hours as i64
            }
            _ => false,
        };

        // 2. Screen interactions at multiple points
        let interactions_ok = self.interaction_sessions >= config.min_interaction_sessions;

        // 3. At least one orientation change
        let orientation_ok = self.orientation_changed;

        // 4. Human-consistent accelerometer motion
        let motion_ok = self.human_motion_detected;

        // 5. GPS fix obtained
        let gps_ok = self.gps_fix_obtained;

        // 6. Wi-Fi OR Bluetooth connectivity
        let network_ok = self.distinct_wifi_networks >= 1 || self.distinct_bt_environments >= 1;

        // 7. Varying Bluetooth environments (only required if BT is used for connectivity)
        // WHY: Wi-Fi-only phones are first-class citizens per spec. BT variation
        // requirement only applies when the device actually has BT peers.
        let bt_variation_ok = self.distinct_bt_environments == 0
            || self.distinct_bt_environments >= config.min_distinct_bt_environments;

        // 8. Charge cycle event
        let charge_ok = self.charge_cycle_event;

        self.unlock_count >= config.min_daily_unlocks
            && unlock_spread
            && interactions_ok
            && orientation_ok
            && motion_ok
            && gps_ok
            && network_ok
            && bt_variation_ok
            && charge_ok
    }
}

/// Coarse geographic location (city-level precision for shard assignment).
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct GeoLocation {
    /// Latitude rounded to ~1km precision.
    pub lat: f32,
    /// Longitude rounded to ~1km precision.
    pub lon: f32,
}

/// Optional sensor data that enhances Presence Score but is not required.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct OptionalSensorData {
    pub gyroscope_active: bool,
    pub ambient_light_active: bool,
    pub cellular_connected: bool,
    pub cell_tower_count: Option<u32>,
    pub barometer_reading: Option<f32>,
    pub magnetometer_active: bool,
    pub nfc_available: bool,
    pub secure_enclave_attested: bool,
    pub biometric_confirmed: bool,
    pub camera_env_hash: Option<[u8; 32]>,
    pub microphone_env_hash: Option<[u8; 32]>,
}

// ============================================================================
// Mining Types
// ============================================================================

/// The current mining state of a node.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum MiningState {
    /// Phone is not plugged in, or battery below 80%.
    /// Proof of Life data is being collected passively.
    ProofOfLife,
    /// Phone is plugged in and battery is at or above 80%.
    /// Waiting for valid PoL attestation and minimum stake.
    PendingActivation,
    /// Actively mining — participating in consensus, earning rewards.
    Mining,
    /// Mining paused due to thermal throttling.
    Throttled,
    /// Mining paused due to battery dropping below 80%.
    BatteryLow,
}

/// Information about the phone's current power state.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct PowerState {
    /// Whether the phone is connected to a power source.
    pub is_plugged_in: bool,
    /// Current battery level (0-100).
    pub battery_percent: u8,
    /// Current CPU temperature in Celsius.
    pub cpu_temp_celsius: f32,
    /// Whether the phone is in thermal throttle state.
    pub is_throttled: bool,
}

impl PowerState {
    /// Check if mining conditions are met.
    pub fn can_mine(&self) -> bool {
        self.is_plugged_in && self.battery_percent >= 80 && !self.is_throttled
    }
}

// ============================================================================
// Staking Types
// ============================================================================

/// A node's staking information.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StakeInfo {
    /// Amount staked by this node (up to the per-node cap).
    pub node_stake: Lux,
    /// Amount overflowed to the Network Security Pool.
    pub overflow_amount: Lux,
    /// Total amount committed (node_stake + overflow_amount).
    pub total_committed: Lux,
    /// Timestamp of initial stake.
    pub staked_at: DateTime<Utc>,
    /// Whether this node meets the minimum stake requirement.
    pub meets_minimum: bool,
}

// ============================================================================
// Governance Types
// ============================================================================

/// A governance proposal.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Proposal {
    /// Unique proposal ID.
    pub id: [u8; 32],
    /// Proposer's node ID (must have 90+ days PoL history).
    pub proposer: NodeId,
    pub title: String,
    pub description: String,
    /// Encoded proposal payload.
    pub proposal_data: Vec<u8>,
    /// When the proposal was submitted.
    pub submitted_at: DateTime<Utc>,
    /// End of discussion period (14 days after submission).
    pub discussion_ends: DateTime<Utc>,
    /// End of voting period (7 days after discussion ends).
    pub voting_ends: DateTime<Utc>,
    /// Implementation date if passed (30 days after voting ends).
    pub implementation_date: DateTime<Utc>,
    /// Current status.
    pub status: ProposalStatus,
    /// Vote tally.
    pub votes_yes: u64,
    pub votes_no: u64,
    pub votes_abstain: u64,
    /// Total eligible voters at time of vote.
    pub eligible_voters: u64,
}

impl Proposal {
    /// Check if quorum is met (20% of eligible voters participated).
    pub fn quorum_met(&self) -> bool {
        let total_votes = self.votes_yes + self.votes_no + self.votes_abstain;
        let quorum_threshold = self.eligible_voters / 5; // 20%
        total_votes >= quorum_threshold
    }

    /// Check if the proposal passed (51% of votes cast).
    pub fn passed(&self) -> bool {
        if !self.quorum_met() {
            return false;
        }
        let total_votes = self.votes_yes + self.votes_no; // Abstains don't count for/against
        // WHY: Using multiplication instead of division avoids integer truncation.
        // e.g., with 100 votes, `51 > 100/2` = `51 > 50` = true (correct),
        // but `50 > 100/2` = `50 > 50` = false (correct). With odd totals like 3,
        // `2*2 > 3` = true (correct 51% majority).
        self.votes_yes * 2 > total_votes
    }
}

/// Proposal lifecycle status.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ProposalStatus {
    /// In 14-day discussion period.
    Discussion,
    /// In 7-day voting period.
    Voting,
    /// Passed, in 30-day implementation delay.
    Approved,
    /// Rejected (did not reach 51% or quorum).
    Rejected,
    /// Implemented and active.
    Implemented,
    /// Emergency reversal.
    Reverted,
}

// ============================================================================
// Poll Types
// ============================================================================

/// An on-chain poll. One phone, one vote. All responses from verified humans.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Poll {
    pub id: [u8; 32],
    pub creator: Address,
    pub question: String,
    pub options: Vec<String>,
    pub created_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
    /// Vote counts per option index.
    pub votes: Vec<u64>,
    /// Total unique voters.
    pub total_voters: u64,
    /// Optional geographic restriction.
    pub geographic_filter: Option<GeographicFilter>,
    /// Cost paid to create this poll (burned).
    pub creation_fee: Lux,
}

// ============================================================================
// Network Types
// ============================================================================

/// Geographic shard identifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ShardId(pub u16);

/// Peer information for network discovery.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PeerInfo {
    pub node_id: NodeId,
    pub shard: ShardId,
    pub presence_score: u8,
    pub mining_state: MiningState,
    /// Approximate geographic region (not precise location).
    pub region: Option<String>,
}
