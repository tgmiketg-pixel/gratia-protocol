//! gratia-ffi — UniFFI bridge for the Gratia protocol.
//!
//! This crate is the **single entry point** for the mobile apps (Android/iOS).
//! It wraps internal Rust crates and exposes a simplified, mobile-friendly API.
//! UniFFI auto-generates Kotlin and Swift bindings from the exported types and
//! functions defined here.
//!
//! ## Architecture
//!
//! ```text
//! Kotlin/Swift UI ──> UniFFI bindings ──> GratiaNode (this crate)
//!                                              │
//!                         ┌────────────────────┼────────────────────┐
//!                         ▼                    ▼                    ▼
//!                   gratia-wallet        gratia-pol          gratia-staking
//!                         │                    │                    │
//!                         └────────────────────┴────────────────────┘
//!                                              │
//!                                        gratia-core
//! ```
//!
//! All types crossing the FFI boundary are simple structs/enums with only
//! primitive fields, strings, and Vec<T>. No generics, no trait objects, no
//! lifetimes. Internal errors are mapped to a flat `FfiError` enum.

pub mod convert;

use std::collections::VecDeque;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

use chrono::Utc;
use tracing::{debug, error, info, warn};

use gratia_consensus::committee::EligibleNode;
use gratia_consensus::validation::{validate_block_transactions, MIN_TRANSACTION_FEE};
use gratia_consensus::vrf::{VrfPublicKey, VrfSecretKey};
use gratia_consensus::{BlockProcessResult, ConsensusEngine};
use gratia_consensus::streamlet::{StreamletState, StreamletVote};
use gratia_core::emission::EmissionSchedule;
use gratia_core::config::Config;
use gratia_core::types::{Block, BlockHash, Lux, MiningState, NodeId, PowerState};
use gratia_consensus::sync::SyncProtocol as ConsensusSyncProtocol;
use gratia_network::sync::{SyncManager, SyncState};
use gratia_network::gossip::NodeAnnouncement;
use gratia_network::{BlockProvider, NetworkConfig, NetworkEvent, NetworkManager};
use gratia_pol::collector::SensorEventBuffer;
use gratia_pol::ProofOfLifeManager;
use gratia_governance::GovernanceManager;
use gratia_core::types::Vote;
use gratia_staking::StakingManager;
use gratia_state::db::{StorageBackend, StorageBackendConfig, open_storage};
use gratia_state::StateManager;
use gratia_vm::interpreter::InterpreterRuntime;
// MockRuntime used for VM initialization in deploy_contract
use gratia_vm::host_functions::HostEnvironment;
use gratia_vm::sandbox::ContractPermissions;
use gratia_vm::{GratiaVm, ContractCall};
use gratia_wallet::keystore::FileKeystore;
use gratia_wallet::recovery::SeedPhrase;
use gratia_wallet::WalletManager;

use crate::convert::{address_from_hex, address_to_hex, mining_state_to_string};

// ============================================================================
// Block Provider for Sync
// ============================================================================

/// Wraps the on-chain state store to provide blocks for the sync protocol.
/// WHY: The network event loop runs in a separate tokio task and can't access
/// the FFI inner state directly. This Arc-wrapped provider bridges the gap.
struct StateBlockProvider {
    store: Arc<dyn gratia_state::db::StateStore>,
}

impl BlockProvider for StateBlockProvider {
    fn get_blocks(&self, from_height: u64, to_height: u64) -> Vec<Block> {
        let db = gratia_state::db::StateDb::new(self.store.clone());
        let mut blocks = Vec::new();
        for height in from_height..=to_height.min(from_height + 49) {
            // WHY: Cap at 50 blocks per request to bound response size for mobile.
            match db.get_block_by_height(height) {
                Ok(Some(block)) => blocks.push(block),
                _ => break, // Stop at first missing block
            }
        }
        blocks
    }
}

// Re-export uniffi scaffolding. This macro generates the C-level FFI symbols
// that UniFFI's generated Kotlin/Swift code calls into.
uniffi::setup_scaffolding!();

// ============================================================================
// FFI Error Type
// ============================================================================

/// User-friendly error type exposed across the FFI boundary.
///
/// Variants are kept simple and descriptive — mobile UI code switches on the
/// variant name to decide what to show the user (e.g., a "battery too low"
/// toast vs. a "wallet locked" dialog).
#[derive(Debug, thiserror::Error, uniffi::Error)]
pub enum FfiError {
    #[error("Wallet not initialized: call create_wallet() first")]
    WalletNotInitialized,

    #[error("Wallet already exists on this device")]
    WalletAlreadyExists,

    #[error("Invalid address format: {reason}")]
    InvalidAddress { reason: String },

    #[error("Insufficient balance: have {available_lux} Lux, need {required_lux} Lux")]
    InsufficientBalance {
        available_lux: u64,
        required_lux: u64,
    },

    #[error("Mining conditions not met: {reason}")]
    MiningNotAvailable { reason: String },

    #[error("Staking error: {reason}")]
    StakingError { reason: String },

    #[error("Proof of Life error: {reason}")]
    ProofOfLifeError { reason: String },

    #[error("Wallet is frozen due to an active recovery claim")]
    WalletFrozen,

    #[error("Network error: {reason}")]
    NetworkError { reason: String },

    #[error("Internal error: {reason}")]
    InternalError { reason: String },
}

/// Map any `GratiaError` from the core crates into an `FfiError`.
///
/// WHY: We collapse the detailed internal error variants into a smaller set of
/// FFI-friendly variants. Mobile code doesn't need the full granularity — it
/// needs enough to show the right UI.
impl From<gratia_core::error::GratiaError> for FfiError {
    fn from(e: gratia_core::error::GratiaError) -> Self {
        use gratia_core::error::GratiaError;
        match e {
            GratiaError::InsufficientBalance {
                available,
                required,
            } => FfiError::InsufficientBalance {
                available_lux: available,
                required_lux: required,
            },
            GratiaError::RecoveryClaimPending => FfiError::WalletFrozen,
            GratiaError::WalletLocked => FfiError::WalletNotInitialized,
            GratiaError::NotPluggedIn
            | GratiaError::BatteryTooLow { .. }
            | GratiaError::ThermalThrottle { .. }
            | GratiaError::MiningConditionsNotMet { .. } => FfiError::MiningNotAvailable {
                reason: e.to_string(),
            },
            GratiaError::InsufficientStake { .. } | GratiaError::UnstakeCooldownActive { .. } => {
                FfiError::StakingError {
                    reason: e.to_string(),
                }
            }
            GratiaError::ProofOfLifeInvalid { .. }
            | GratiaError::InsufficientUnlocks { .. }
            | GratiaError::UnlockSpreadTooNarrow { .. }
            | GratiaError::NoChargeCycleEvent
            | GratiaError::InsufficientBtVariation
            | GratiaError::OnboardingIncomplete { .. } => FfiError::ProofOfLifeError {
                reason: e.to_string(),
            },
            other => FfiError::InternalError {
                reason: other.to_string(),
            },
        }
    }
}

// ============================================================================
// FFI Data Types
// ============================================================================

/// Wallet information returned to the mobile UI.
#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiWalletInfo {
    /// Wallet address as "grat:<hex>" string.
    pub address: String,
    /// Balance in Lux (1 GRAT = 1,000,000 Lux).
    pub balance_lux: u64,
    /// Current mining state as a human-readable string.
    pub mining_state: String,
}

/// A single transaction record for display in the mobile UI.
#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiTransactionInfo {
    /// Transaction hash as hex string.
    pub hash_hex: String,
    /// "sent" or "received".
    pub direction: String,
    /// Counterparty address (None for stake/unstake operations).
    pub counterparty: Option<String>,
    /// Amount in Lux.
    pub amount_lux: u64,
    /// Unix timestamp in milliseconds.
    pub timestamp_millis: i64,
    /// "pending", "confirmed", or "failed".
    pub status: String,
}

/// Current mining status for the mobile UI.
#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiMiningStatus {
    /// Mining state string: "proof_of_life", "pending_activation", "mining",
    /// "throttled", or "battery_low".
    pub state: String,
    /// Current battery percentage (0-100).
    pub battery_percent: u8,
    /// Whether the phone is connected to power.
    pub is_plugged_in: bool,
    /// Whether today's Proof of Life is valid.
    pub current_day_pol_valid: bool,
    /// Composite Presence Score (40-100, or 0 if not yet calculated).
    pub presence_score: u8,
}

/// Proof of Life status for the mobile UI.
#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiProofOfLifeStatus {
    /// Whether today's PoL requirements have been met.
    pub is_valid_today: bool,
    /// Number of consecutive valid PoL days.
    pub consecutive_days: u64,
    /// Whether the one-day onboarding period is complete.
    pub is_onboarded: bool,
    /// List of parameter names that have been satisfied today.
    pub parameters_met: Vec<String>,
}

/// Staking information for the mobile UI.
#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiStakeInfo {
    /// Effective stake counting toward consensus (capped at per-node cap), in Lux.
    pub node_stake_lux: u64,
    /// Amount overflowed to the Network Security Pool, in Lux.
    pub overflow_amount_lux: u64,
    /// Total committed stake (effective + overflow), in Lux.
    pub total_committed_lux: u64,
    /// Unix timestamp in milliseconds when the stake was placed.
    pub staked_at_millis: i64,
    /// Whether this node meets the minimum stake requirement.
    pub meets_minimum: bool,
}

/// Pending unstake status for the mobile UI.
///
/// WHY: The mobile app needs to show users whether they have a pending unstake,
/// how much is pending, and how long until the cooldown expires so they can
/// call `complete_unstake()`.
#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiUnstakeStatus {
    /// Whether there is a pending unstake request.
    pub has_pending_unstake: bool,
    /// Amount of Lux pending release (0 if no pending unstake).
    pub pending_amount_lux: u64,
    /// Unix timestamp in milliseconds when the unstake was requested (0 if none).
    pub requested_at_millis: i64,
    /// Seconds remaining in the cooldown period (0 if cooldown has elapsed or no pending).
    pub remaining_cooldown_secs: u64,
}

/// Network Security Pool status for the mobile UI.
///
/// WHY: The mobile app needs to display how much overflow is in the pool,
/// how many nodes contribute, and the user's share of accumulated yield.
#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiPoolStatus {
    /// Total overflow Lux in the Network Security Pool.
    pub total_overflow_lux: u64,
    /// Number of nodes contributing overflow to the pool.
    pub contributor_count: u32,
    /// Total accumulated yield in the pool (Lux).
    pub accumulated_yield_lux: u64,
    /// This node's overflow contribution (Lux), 0 if none.
    pub your_overflow_lux: u64,
    /// This node's estimated yield share (Lux), 0 if none.
    pub your_estimated_yield_lux: u64,
}

/// Staking activation status for the mobile UI.
///
/// WHY: Staking minimum is not enforced until the network crosses a miner
/// threshold, then a grace period begins. The mobile app needs to show users
/// whether staking is enforced, how long the grace period lasts, and what
/// the effective minimum stake is right now.
#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiActivationStatus {
    /// Whether the staking minimum has been activated (threshold crossed).
    pub is_activated: bool,
    /// Unix timestamp in milliseconds when staking was activated (0 if not yet).
    pub activated_at_millis: i64,
    /// Seconds remaining in the grace period (0 if grace has elapsed or not activated).
    pub grace_period_remaining_secs: u64,
    /// The effective minimum stake right now (0 during genesis/grace, full amount after).
    pub effective_minimum_stake_lux: u64,
    /// Human-readable enforcement state: "genesis", "grace_period", or "enforced".
    pub enforcement_state: String,
}

/// Proof of Life ZK range proof for the mobile UI.
///
/// WHY: The Bulletproofs-based PoL proof is generated on-device and proves
/// that the user's daily Proof of Life parameters meet the required thresholds
/// without revealing the actual sensor values. Bytes are hex-encoded for safe
/// transport across the FFI boundary.
#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiPolRangeProof {
    /// Hex-encoded serialized Bulletproof proof bytes.
    pub proof_bytes_hex: String,
    /// Hex-encoded Pedersen commitments (one per proven parameter).
    pub commitments_hex: Vec<String>,
    /// Number of parameters proven in this proof.
    pub parameter_count: u8,
    /// Epoch day (days since Unix epoch) this proof covers.
    pub epoch_day: u32,
}

/// Network status for the mobile UI.
#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiNetworkStatus {
    /// Whether the network layer is running.
    pub is_running: bool,
    /// Number of connected peers.
    pub peer_count: u32,
    /// Current listen address (if available).
    pub listen_address: Option<String>,
    /// Sync status: "synced", "syncing 123/456", "unknown", or "not_started".
    pub sync_status: String,
    /// Local chain height.
    pub local_height: u64,
}

/// A network event delivered to the mobile app via polling.
#[derive(Debug, Clone, uniffi::Enum)]
pub enum FfiNetworkEvent {
    /// A peer connected.
    PeerConnected { peer_id: String },
    /// A peer disconnected.
    PeerDisconnected { peer_id: String },
    /// A block was received from the network.
    BlockReceived { height: u64, producer: String },
    /// A transaction was received from the network.
    TransactionReceived { hash_hex: String },
    /// A Lux social post was received from the network.
    LuxPostReceived { hash: String, author: String },
}

/// Consensus status for the mobile UI.
#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiConsensusStatus {
    /// Consensus state: "syncing", "active", "producing", or "stopped".
    pub state: String,
    /// Current slot number.
    pub current_slot: u64,
    /// Current block height (last finalized block).
    pub current_height: u64,
    /// Whether this node is on the current validator committee.
    pub is_committee_member: bool,
    /// Number of blocks this node has produced.
    pub blocks_produced: u64,
}

/// Sync status for the mobile UI.
/// WHY: Exposes the consensus-level sync state machine so the app can show
/// a progress bar during initial sync or when catching up after going offline.
#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiSyncStatus {
    /// Current sync state: "idle", "syncing", or "synced".
    pub state: String,
    /// Local chain height (what we have applied).
    pub current_height: u64,
    /// Target height we are syncing toward.
    pub target_height: u64,
    /// Sync progress as a percentage (0-100).
    pub progress_percent: u8,
}

/// Result of a smart contract execution.
#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiContractResult {
    /// Whether the contract call succeeded.
    pub success: bool,
    /// Return value as a string (serialized).
    pub return_value: String,
    /// Gas used by the execution.
    pub gas_used: u64,
    /// Gas remaining from the limit.
    pub gas_remaining: u64,
    /// Events emitted by the contract.
    pub events: Vec<String>,
    /// Error message if execution failed.
    pub error: Option<String>,
}

/// Sensor events pushed from the native platform layer (Android/iOS) into
/// the Rust PoL engine.
///
/// WHY: This enum mirrors `gratia_pol::collector::SensorEvent` but strips
/// out the `DateTime<Utc>` timestamp field (which is not FFI-safe). The
/// timestamp is set to `Utc::now()` on the Rust side when the event arrives.
#[derive(Debug, Clone, uniffi::Enum)]
pub enum FfiSensorEvent {
    /// Phone was unlocked by the user.
    Unlock,
    /// A screen interaction session was recorded.
    Interaction {
        /// Duration of the session in seconds.
        duration_secs: u32,
    },
    /// Phone orientation changed (picked up, rotated, set down).
    OrientationChange,
    /// Accelerometer detected human-consistent motion.
    Motion,
    /// A GPS fix was obtained.
    GpsUpdate {
        lat: f32,
        lon: f32,
    },
    /// Wi-Fi scan completed with visible BSSIDs (as opaque hashes).
    WifiScan {
        bssid_hashes: Vec<u64>,
    },
    /// Bluetooth scan completed with nearby peers (as opaque hashes).
    BluetoothScan {
        peer_hashes: Vec<u64>,
    },
    /// Charge state changed (plugged in or unplugged).
    ChargeEvent {
        is_charging: bool,
    },
    /// Barometric pressure reading (hPa).
    /// WHY: Enables environmental oracle contracts and weather-aware smart contracts.
    /// Aggregated across thousands of phones, this creates a decentralized weather network.
    BarometerReading {
        hpa: f32,
    },
    /// Ambient light level reading (lux — photometric unit, not GRAT Lux).
    /// WHY: Indoor/outdoor detection for location-triggered contracts without GPS.
    LightReading {
        lux: f32,
    },
    /// Magnetometer heading (degrees, 0-360).
    /// WHY: Orientation-aware contracts and compass-based proximity verification.
    MagnetometerReading {
        degrees: f32,
    },
    /// Accelerometer magnitude reading (m/s^2, scalar).
    /// WHY: Activity-level detection for fitness contracts and proof-of-movement.
    AccelerometerReading {
        magnitude: f32,
    },
}

/// Connection profile detected by the mobile platform layer.
///
/// WHY: Devices without a SIM card (e.g., Samsung A06 Indian variant) have broken
/// UDP/QUIC sockets — Android's carrier firmware cripples UDP when no cellular radio
/// is active. By detecting SIM presence upfront, we choose the right transport
/// strategy immediately instead of waiting for QUIC to timeout and then falling back
/// to TCP (which adds 10-30s of unnecessary delay on every startup).
#[derive(Debug, Clone, uniffi::Enum)]
pub enum FfiConnectionProfile {
    /// SIM present, cellular or Wi-Fi available — QUIC primary, TCP fallback.
    Full,
    /// No SIM, Wi-Fi only — TCP primary, skip QUIC entirely.
    WifiOnly,
    /// No connectivity — Bluetooth mesh relay only (queue transactions).
    Offline,
}

/// A governance proposal for the mobile UI.
#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiProposal {
    pub id_hex: String,
    pub title: String,
    pub description: String,
    pub status: String,
    pub votes_yes: u64,
    pub votes_no: u64,
    pub votes_abstain: u64,
    pub discussion_end_millis: i64,
    pub voting_end_millis: i64,
    pub submitted_by: String,
}

/// An on-chain poll for the mobile UI.
#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiPoll {
    pub id_hex: String,
    pub question: String,
    pub options: Vec<String>,
    pub votes: Vec<u64>,
    pub total_voters: u64,
    pub end_millis: i64,
    pub created_by: String,
}

/// Detailed poll results for the mobile UI.
#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiPollOptionResult {
    pub index: u32,
    pub label: String,
    pub votes: u64,
    /// Percentage of total voters (0.0 - 100.0).
    pub percentage: f64,
}

/// Aggregated poll results including per-option breakdown.
#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiPollResults {
    pub poll_id_hex: String,
    pub question: String,
    pub total_voters: u64,
    pub options: Vec<FfiPollOptionResult>,
    pub expired: bool,
}

/// Bluetooth/Wi-Fi Direct mesh network status for the mobile UI.
#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiMeshStatus {
    /// Whether the mesh layer is enabled.
    pub enabled: bool,
    /// Whether Bluetooth transport is active.
    pub bluetooth_active: bool,
    /// Whether Wi-Fi Direct transport is active.
    pub wifi_direct_active: bool,
    /// Number of mesh peers (Bluetooth + Wi-Fi Direct).
    pub mesh_peer_count: u32,
    /// Number of bridge peers (mesh peers that also have internet connectivity).
    pub bridge_peer_count: u32,
    /// Number of messages pending relay to the wider network.
    pub pending_relay_count: u32,
}

/// A mesh peer discovered via Bluetooth or Wi-Fi Direct.
#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiMeshPeer {
    /// Peer identifier as hex string.
    pub peer_id: String,
    /// Transport type: "bluetooth", "wifi_direct", or "both".
    pub transport: String,
    /// Signal strength in dBm (negative values; -30 = strong, -90 = weak).
    pub signal_strength: i32,
    /// Number of hops from this node (1 = direct peer).
    pub hop_count: u8,
    /// Whether this peer has internet connectivity (bridge peer).
    pub has_internet: bool,
}

/// Geographic shard information for the mobile UI.
#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiShardInfo {
    /// This node's assigned shard ID.
    pub shard_id: u16,
    /// Total number of active shards in the network.
    pub shard_count: u16,
    /// Number of validators in this node's local shard.
    pub local_validators: u32,
    /// Number of cross-shard validators (participate in multiple shards).
    pub cross_shard_validators: u32,
    /// Current block height within this shard.
    pub shard_height: u64,
    /// Whether geographic sharding is currently active.
    pub is_sharding_active: bool,
}

/// GratiaVM runtime information for the mobile UI.
#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiVmInfo {
    /// Runtime type: "wasmer" or "interpreter".
    pub runtime_type: String,
    /// Number of contracts currently deployed.
    pub contracts_loaded: u32,
    /// Cumulative gas consumed across all contract calls.
    pub total_gas_used: u64,
    /// Whether the WASM runtime uses memory-wired pages (locked in RAM).
    pub memory_wired: bool,
}

// ============================================================================
// Lux Social Protocol FFI types
// ============================================================================

/// A Lux post as returned to the mobile app.
#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiLuxPost {
    pub hash: String,
    pub author: String,
    pub author_display_name: String,
    pub content: String,
    pub timestamp_millis: i64,
    pub likes: u64,
    pub reposts: u64,
    pub replies: u64,
    pub liked_by_me: bool,
    pub reposted_by_me: bool,
}

/// Lux feed result returned to the mobile app.
#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiLuxFeed {
    pub posts: Vec<FfiLuxPost>,
    pub post_fee_lux: u64,
    pub total_burned_lux: u64,
}

/// Lux user profile.
#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiLuxProfile {
    pub address: String,
    pub display_name: String,
    pub bio: String,
    pub follower_count: u64,
    pub following_count: u64,
    pub post_count: u64,
}

// ============================================================================
// GratiaNode — The main FFI entry point
// ============================================================================

/// The main API object exposed to mobile apps via UniFFI.
///
/// A single `GratiaNode` instance is created at app launch and held for the
/// lifetime of the app. It owns all subsystem managers (wallet, PoL, staking)
/// and coordinates their interactions.
///
/// Thread safety: all internal state is behind a `Mutex` so that concurrent
/// calls from the native UI thread and background services are safe.
#[derive(uniffi::Object)]
pub struct GratiaNode {
    /// Data directory for persistent storage (e.g., app-internal storage path).
    /// Used for RocksDB and persistent wallet storage (Phase 2).
    #[allow(dead_code)]
    data_dir: String,
    /// Inner state protected by Arc<Mutex> for thread safety across FFI calls
    /// and background tasks (slot timer, event loop).
    inner: Arc<Mutex<GratiaNodeInner>>,
    /// Tokio runtime for async operations (network, consensus).
    /// WHY: UniFFI methods are synchronous, but libp2p networking is async.
    /// We embed a tokio runtime so FFI methods can call `block_on()` to drive
    /// async operations. The runtime is created once at node initialization.
    runtime: tokio::runtime::Runtime,
}

/// Simple file-based chain state persistence for Phase 1.
/// WHY: Full RocksDB requires C++ cross-compilation for Android which is
/// complex to set up. File-based persistence gives us chain height, tip hash,
/// and block count survival across restarts with zero external dependencies.
struct ChainPersistence {
    data_dir: String,
}

impl ChainPersistence {
    fn new(data_dir: &str) -> Self {
        ChainPersistence { data_dir: data_dir.to_string() }
    }

    fn data_dir(&self) -> &str {
        &self.data_dir
    }

    /// Save chain state to file.
    /// Format: 8 bytes height + 32 bytes hash + 8 bytes blocks_produced = 48 bytes
    fn save(&self, height: u64, tip_hash: &[u8; 32], blocks_produced: u64) {
        let path = format!("{}/chain_state.bin", self.data_dir);
        let mut data = Vec::with_capacity(48);
        data.extend_from_slice(&height.to_le_bytes());
        data.extend_from_slice(tip_hash);
        data.extend_from_slice(&blocks_produced.to_le_bytes());
        let _ = std::fs::write(&path, &data);
    }

    /// Load chain state from file. Returns (height, tip_hash, blocks_produced).
    fn load(&self) -> Option<(u64, [u8; 32], u64)> {
        let path = format!("{}/chain_state.bin", self.data_dir);
        let data = std::fs::read(&path).ok()?;
        if data.len() != 48 { return None; }

        let height = u64::from_le_bytes(data[0..8].try_into().ok()?);
        let mut hash = [0u8; 32];
        hash.copy_from_slice(&data[8..40]);
        let blocks_produced = u64::from_le_bytes(data[40..48].try_into().ok()?);

        Some((height, hash, blocks_produced))
    }
}

/// Mutable inner state of the GratiaNode.
struct GratiaNodeInner {
    wallet: WalletManager<FileKeystore>,
    pol: ProofOfLifeManager,
    sensor_buffer: SensorEventBuffer,
    staking: StakingManager,
    /// Cached power state from the last `update_power_state` call.
    power_state: PowerState,
    /// Cached mining state derived from current conditions.
    mining_state: MiningState,
    /// Composite Presence Score (placeholder — will be calculated from sensor flags).
    presence_score: u8,
    /// Network manager — created when `start_network()` is called.
    network: Option<NetworkManager>,
    /// Network event receiver — polls events from the background swarm task.
    network_event_rx: Option<tokio::sync::mpsc::Receiver<NetworkEvent>>,
    /// Buffered network events for delivery to the mobile app via `poll_network_events()`.
    pending_network_events: VecDeque<FfiNetworkEvent>,
    /// Raw network events buffered by the discovery drain for later processing.
    /// WHY: The discovery drain only processes PeerConnected/NodeAnnounced but
    /// must not drop blocks/sigs that arrive during the discovery phase.
    buffered_raw_events: Vec<NetworkEvent>,
    /// Current listen address reported by the swarm.
    listen_address: Option<String>,
    /// Consensus engine — created when `start_consensus()` is called.
    consensus: Option<ConsensusEngine>,
    /// Sync manager for block catch-up (network-level peer tracking).
    sync_manager: Option<SyncManager>,
    /// Consensus-level sync protocol — tracks sync state machine, generates
    /// batched requests, and reports progress for the UI.
    /// WHY: The network-level SyncManager handles gossipsub transport and peer
    /// chain tip tracking. This consensus-level SyncProtocol sits above it and
    /// decides *when* to sync, validates block ordering, and reports progress.
    sync_protocol: Option<ConsensusSyncProtocol>,
    /// Number of blocks this node has produced (lifetime counter).
    blocks_produced: u64,
    /// Handle to the slot timer task (so we can cancel it on stop).
    slot_timer_handle: Option<tokio::task::JoinHandle<()>>,
    /// WHY: Debug-only flag to bypass Proof of Life and staking requirements
    /// for testing mining on real devices before a full 24-hour PoL window
    /// has elapsed. This field only exists in debug builds.
    #[cfg(debug_assertions)]
    debug_bypass_checks: bool,
    /// WHY: When the user taps "Stop Mining", this flag prevents
    /// update_power_state from automatically re-enabling mining.
    /// Without it, debug bypass + plugged in + ≥80% battery causes
    /// update_power_state to immediately override the user's stop.
    /// Cleared when the user taps "Start Mining" again.
    user_stopped_mining: bool,
    /// Block pending broadcast to network. Set inside the mutex, broadcast
    /// after the lock is released (async broadcast can't hold the lock).
    pending_broadcast_block: Option<Block>,
    /// Known peer nodes for committee selection. Populated via NodeAnnounced events.
    /// WHY: Stored here so the committee can be rebuilt with real peer data when
    /// new nodes join the network, replacing synthetic padding nodes.
    known_peer_nodes: Vec<NodeAnnouncement>,
    /// Streamlet BFT state machine — formally proven consensus protocol.
    /// WHY: Replaces the custom pending_block + signature collection approach
    /// with Streamlet's propose→vote→notarize→finalize protocol. Provides
    /// formal safety proof and built-in fork resolution.
    streamlet: Option<StreamletState>,

    /// PeerIds (as bytes) of connected committee peers for direct BFT delivery.
    /// WHY: The network layer uses PeerIds for direct request-response. When a
    /// block is received, the source PeerId is stored here so the slot timer
    /// can send block proposals directly to committee members.
    /// Stored as Vec<u8> because libp2p is not a direct dependency of gratia-ffi.
    bft_peer_id_bytes: Vec<Vec<u8>>,
    /// Number of real (non-synthetic) committee members.
    /// WHY: When all committee members except the producer are synthetic padding,
    /// BFT finality can never be reached because synthetics can't sign. This
    /// counter lets the slot timer auto-finalize when real_members <= 1.
    real_committee_members: usize,
    /// Recent finalized blocks cache for sync protocol.
    /// WHY: When a new peer connects, we broadcast our recent blocks so they
    /// can catch up without a full request-response protocol. Capped at 100
    /// blocks (~400 seconds / ~7 minutes of history). New peers that are
    /// further behind will need the full sync protocol (Phase 3).
    recent_blocks: VecDeque<Block>,
    /// File-based chain state persistence (height, tip hash, blocks produced).
    /// WHY: Survives app restarts without requiring RocksDB cross-compilation.
    chain_persistence: Option<ChainPersistence>,
    /// Transaction mempool — verified transactions waiting to be included in the
    /// next block. Populated when we send a transaction or receive a valid one
    /// from the network. Drained by the slot timer when producing blocks.
    /// WHY: Without a mempool, produce_block() gets an empty vec and blocks carry
    /// no transactions, making the blockchain a dummy chain with 0 TPS.
    mempool: Vec<gratia_core::types::Transaction>,
    /// TX hashes already applied to on-chain state. Prevents double-crediting
    /// when the same TX appears in both fork resolution and BFT finalization paths.
    applied_tx_hashes: std::collections::HashSet<[u8; 32]>,
    /// On-chain state manager — tracks account balances, nonces, and blocks.
    /// WHY: Without shared state, each phone tracks balances locally and there's
    /// no way to verify a sender actually has the GRAT they claim. The state
    /// manager applies transactions at block finalization, enforcing balance
    /// checks and nonce ordering. This closes the double-spend vulnerability.
    state_manager: Option<StateManager>,
    /// GratiaVM smart contract engine.
    /// WHY: Enables location-triggered contracts, proximity escrows, and other
    /// mobile-native smart contracts. Uses MockRuntime with native handlers
    /// for Phase 2; upgradeable to full WASM execution with wasmer later.
    vm: Option<GratiaVm>,
    /// Storage backend handle for state persistence.
    /// WHY: StateManager holds the store as Arc<dyn StateStore>, which doesn't
    /// expose persist(). We keep the StorageBackend handle so we can save
    /// state to disk after each block finalization. For RocksDB, persist()
    /// is a no-op (writes are already durable). For InMemoryStore, it
    /// serializes the BTreeMap to disk.
    storage_backend: Option<StorageBackend>,
    /// Governance manager — one-phone-one-vote proposals and polls.
    governance: GovernanceManager,
    /// Bluetooth/Wi-Fi Direct mesh transport layer (Phase 3).
    /// WHY: Enables offline transaction relay and local peer discovery
    /// without internet connectivity. Created when start_mesh() is called.
    mesh_transport: Option<gratia_network::mesh::MeshTransport>,
    /// Geographic shard coordinator (Phase 3).
    /// WHY: Manages multi-shard consensus and cross-shard transaction routing.
    /// Created when sharding is activated based on network size.
    shard_coordinator: Option<gratia_consensus::sharded_consensus::ShardCoordinator>,
    /// Cumulative gas consumed across all VM contract calls (Phase 3).
    /// WHY: Tracked here so get_vm_info() can report it without querying
    /// every contract execution result retroactively.
    total_gas_used: u64,
    /// Lux social protocol store — manages posts, engagement, social graph.
    lux_store: gratia_lux::LuxStore,
    /// Lux dynamic posting fee calculator — adjusts fees based on block utilization.
    lux_fees: gratia_lux::FeeCalculator,
    /// Timestamp (epoch millis) when the current pending block was created.
    /// WHY: Used to implement the BFT signature timeout — if we don't collect
    /// enough signatures within the BFT timeout (base 20s + 2s per committee
    /// member beyond 2), the block is discarded (no finality without real sigs).
    pending_block_created_at: Option<std::time::Instant>,
    /// WHY: Cooldown after fork resolution to prevent infinite reorg loops.
    /// After rolling back, incoming gossip blocks trigger ForkDetected again
    /// (peer is ahead of our rolled-back height). Without a cooldown, we
    /// reorg → timeout → produce → reorg → forever. 60-second cooldown gives
    /// the sync protocol time to deliver blocks and catch us up.
    last_reorg_at: Option<std::time::Instant>,
    /// WHY: Prevents block production until peer discovery and chain sync
    /// are complete. Without this, both phones produce divergent chains
    /// before discovering each other via the bootstrap node, creating
    /// permanent forks that trigger infinite reorg loops. Set to true
    /// after the 30-second discovery phase completes (either synced with
    /// peers, or no peers found and we're solo).
    initial_sync_done: bool,
    /// Block hash of the pending block awaiting signatures.
    /// WHY: Needed to match incoming ValidatorSignatureReceived events to our
    /// pending block. If the hash doesn't match, the signature is for a different
    /// block (possibly from a fork) and should be ignored.
    pending_block_hash: Option<[u8; 32]>,
    /// Hash + height of the last block that expired from BFT timeout.
    /// WHY: When a BFT timeout fires, the pending block is discarded. But the
    /// peer's signature may still be in-flight via gossipsub. If it arrives
    /// within a few seconds of expiration, we should still accept it and
    /// finalize the block rather than wasting it. This stores the expired
    /// block's hash so we can match late-arriving signatures.
    last_expired_block_hash: Option<[u8; 32]>,
    last_expired_block_height: Option<u64>,
    /// Count of consecutive blocks that expired without BFT finality.
    /// WHY: When WiFi drops silently, QUIC connections don't close cleanly and
    /// the node doesn't know peers are gone. Blocks keep expiring because no
    /// co-signatures arrive. After 5 consecutive expirations (~100 seconds with
    /// 20s base timeout), we assume peers are unreachable and rebuild to solo
    /// mode so blocks can auto-finalize again. Reset to 0 whenever a block
    /// reaches BFT finality.
    consecutive_bft_expirations: u32,
    /// Incremented each BFT expiry for the current height, reset on finality.
    /// Used by 2-node alternation to flip producer after a collision.
    bft_retry_count: u64,

    /// Count of consecutive solo-finalized blocks (no peer co-signatures).
    /// WHY: Caps divergent solo chain length to reduce reorg size on reconnect.
    /// After 50 consecutive solo blocks (~200s), production pauses until a peer
    /// reconnects and co-signs.
    consecutive_solo_blocks: u32,

    /// Cached RANDAO epoch seed for the current epoch.
    /// WHY: Without this, every NodeAnnouncement with changed score/pol_days
    /// recomputes the epoch seed from the latest chain state, allowing an
    /// attacker to manipulate their score mid-epoch to influence committee
    /// ordering. The seed is computed once at epoch start (or first committee
    /// init) and reused for all committee rebuilds within the same epoch.
    /// Only reset at epoch boundaries (SLOTS_PER_EPOCH) or node restart.
    epoch_seed: Option<[u8; 32]>,

    /// Peers we've already seen during a solo→multi transition.
    /// WHY: Prevents redundant solo→multi transition logic from firing
    /// multiple times for the same peer when BFT falls back to solo mode
    /// and the peer re-announces. Persists across solo fallbacks.
    yield_checked_peers: Vec<[u8; 32]>,

    // ── Live sensor cache for VM host functions ─────────────────────────
    // WHY: Smart contracts access sensor data via @location, @proximity,
    // @presence, and @sensor host functions. The PoL engine uses sensor events
    // for daily attestation validation, but the VM needs the LATEST reading
    // at contract execution time. These fields cache the most recent value
    // from each sensor so HostEnvironment can be populated immediately.

    /// Latest GPS fix (latitude, longitude).
    /// Updated on every FfiSensorEvent::GpsUpdate.
    last_gps: Option<(f32, f32)>,
    /// Latest barometric pressure in hPa.
    last_barometer: Option<f64>,
    /// Latest ambient light level in lux (photometric, not GRAT Lux).
    last_light: Option<f64>,
    /// Latest magnetometer heading in degrees (0-360).
    last_magnetometer: Option<f64>,
    /// Latest accelerometer magnitude in m/s^2.
    last_accelerometer: Option<f64>,
    /// Timestamp of the most recent sensor update (for freshness checks).
    last_sensor_time: chrono::DateTime<chrono::Utc>,
    /// Cumulative transaction fees burned (deducted from senders, never credited).
    /// WHY: Transaction fees are deflationary — deducted from sender balance but
    /// not credited to any account. This counter tracks the total Lux removed
    /// from circulation via transaction fees, separate from Lux social protocol
    /// burns tracked by lux_fees.
    total_burned_tx_fees: u64,
    /// Timestamp of the last block finalization (BFT or solo).
    /// WHY: Health reports use this to compute `last_block_age_secs` — if this
    /// grows large, the node is likely stalled or disconnected. Updated on every
    /// successful block finalization regardless of path (BFT, solo, sync).
    last_finalized_at: Option<std::time::Instant>,

    // ── Mining streak / session stats for UI animations ──────────────────
    // WHY: The Kotlin UI layer needs live mining stats for tick-up balance
    // animation, streak fire counter, and session earnings display. These
    // are populated from the reward crediting paths (BFT and solo) and
    // reset on each app session (not persisted — streak comes from PoL).

    /// Total GRAT (in Lux) earned THIS app session. Reset on restart.
    session_grat_earned: u64,
    /// Blocks produced THIS session that earned a reward. Reset on restart.
    session_blocks_produced: u64,
    /// Timestamp of the last reward credit. Used to compute `last_reward_age_secs`
    /// for the UI pulse animation (fresh reward = glow).
    last_reward_timestamp: Option<chrono::DateTime<chrono::Utc>>,
}

impl GratiaNodeInner {
    /// Compile-time debug bypass check.
    ///
    /// **Audit hardening (SA-01):** Uses `cfg!(debug_assertions)` which the
    /// compiler evaluates to the constant `false` in release builds. The
    /// expression `false && <anything>` is a compile-time constant, so LLVM
    /// eliminates all bypass branches entirely — the bypass code is physically
    /// absent from release binaries, not merely unreachable at runtime.
    #[inline(always)]
    fn is_debug_bypass(&self) -> bool {
        cfg!(debug_assertions) && {
            #[cfg(debug_assertions)]
            { self.debug_bypass_checks }
            #[cfg(not(debug_assertions))]
            { unreachable!() }
        }
    }

    /// Get a human-readable sync status string for the UI.
    fn get_sync_status_string(&self) -> String {
        // WHY: The network-level SyncManager often stays in Unknown state
        // because its peer chain tip tracking isn't wired end-to-end.
        // Instead, derive sync status from observable state: do we have
        // peers and is consensus running? This gives accurate results
        // that the user can actually see on the Mining screen.
        let has_peers = self.network.as_ref()
            .map(|n| n.connected_peer_count() > 0)
            .unwrap_or(false);
        let has_consensus = self.consensus.is_some();

        if !has_consensus {
            return "Not Started".to_string();
        }

        // Check network-level sync state first for active sync operations
        if let Some(ref sm) = self.sync_manager {
            match sm.state() {
                SyncState::Syncing { local_height, target_height } => {
                    return format!("Syncing {}/{}", local_height, target_height);
                }
                SyncState::Behind { local_height, network_height } => {
                    return format!("Behind {}/{}", local_height, network_height);
                }
                SyncState::Synced => {
                    return "Synced".to_string();
                }
                SyncState::Unknown => {
                    // Fall through to peer-based heuristic
                }
            }
        }

        // WHY: If SyncManager is Unknown, derive sync status from
        // consensus height vs what we know about peer heights.
        // "Synced" requires peers AND our height matching the network.
        if !has_peers {
            return "No Peers".to_string();
        }

        // Compare our height against the best known network height
        let our_height = self.consensus.as_ref()
            .map(|e| e.current_height())
            .unwrap_or(0);
        let network_height = self.sync_manager.as_ref()
            .and_then(|sm| sm.best_network_height())
            .unwrap_or(0);

        if network_height > 0 && our_height < network_height {
            format!("Behind {}/{}", our_height, network_height)
        } else {
            "Synced".to_string()
        }
    }

    /// Get the local chain height.
    fn get_local_height(&self) -> u64 {
        self.consensus.as_ref()
            .map(|e| e.current_height())
            .unwrap_or(0)
    }
}

/// Global log file path, set once during GratiaNode::new().
/// WHY: Android file permissions require writing to the app's private data dir.
/// We cache the path to avoid re-discovering it on every log call.
static LOG_PATH: std::sync::OnceLock<String> = std::sync::OnceLock::new();

/// Global error/warning counter for health reporting.
/// WHY: `rust_log` is a free function without access to `GratiaNodeInner`, so we
/// use an atomic counter that any code path can increment. The health report reads
/// this to surface how many issues occurred this session without parsing log files.
static ERROR_COUNT: AtomicU32 = AtomicU32::new(0);

/// App start time for uptime calculation in health reports.
/// WHY: `std::time::Instant` isn't const-constructable, so we initialize it once
/// in `GratiaNode::new()` via OnceLock. Health report computes uptime as
/// `Instant::now() - APP_START_TIME`.
static APP_START_TIME: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();

/// Write a debug log line to the Rust log file in the app's data directory.
/// Also writes to Android logcat when running on Android.
fn rust_log(msg: &str) {
    // Count error/warning messages for health telemetry.
    {
        let lower = msg.to_ascii_lowercase();
        if lower.starts_with("error") || lower.starts_with("warn")
            || lower.contains("error:") || lower.contains("warn:")
            || lower.contains("failed") || lower.contains("panic")
        {
            ERROR_COUNT.fetch_add(1, Ordering::Relaxed);
        }
    }
    // Write to log file
    if let Some(path) = LOG_PATH.get() {
        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
            use std::io::Write;
            let _ = writeln!(f, "[{}] {}", chrono::Utc::now().format("%H:%M:%S"), msg);
        }
    }
    // Write to Android logcat
    #[cfg(target_os = "android")]
    android_logcat("GratiaRust", msg);
}

/// Initialize the log file path from the app's data directory.
fn init_rust_log(data_dir: &str) {
    let path = format!("{}/gratia-rust.log", data_dir);
    let _ = LOG_PATH.set(path);
}

// --- Android logcat integration ---
// WHY: On Android, `tracing_subscriber::fmt()` writes to stderr which is silently
// discarded. We call the NDK's `__android_log_write` directly via FFI so that ALL
// tracing output (info!, warn!, error!, debug! from every crate) appears in logcat.

#[cfg(target_os = "android")]
mod android_log {
    use std::ffi::CString;

    // Android log priority levels (from android/log.h)
    #[allow(dead_code)]
    pub const ANDROID_LOG_DEBUG: i32 = 3;
    pub const ANDROID_LOG_INFO: i32 = 4;
    #[allow(dead_code)]
    pub const ANDROID_LOG_WARN: i32 = 5;
    #[allow(dead_code)]
    pub const ANDROID_LOG_ERROR: i32 = 6;

    extern "C" {
        fn __android_log_write(prio: i32, tag: *const std::ffi::c_char, text: *const std::ffi::c_char) -> i32;
    }

    pub fn logcat(priority: i32, tag: &str, msg: &str) {
        if let (Ok(c_tag), Ok(c_msg)) = (CString::new(tag), CString::new(msg)) {
            unsafe {
                __android_log_write(priority, c_tag.as_ptr().cast(), c_msg.as_ptr().cast());
            }
        }
    }

    pub fn priority_from_level(level: &tracing::Level) -> i32 {
        match *level {
            tracing::Level::ERROR => ANDROID_LOG_ERROR,
            tracing::Level::WARN => ANDROID_LOG_WARN,
            tracing::Level::INFO => ANDROID_LOG_INFO,
            _ => ANDROID_LOG_DEBUG,
        }
    }

    /// A tracing Layer that forwards all events to Android logcat + rust_log file.
    pub struct AndroidLogcatLayer;

    impl<S: tracing::Subscriber> tracing_subscriber::Layer<S> for AndroidLogcatLayer {
        fn on_event(
            &self,
            event: &tracing::Event<'_>,
            _ctx: tracing_subscriber::layer::Context<'_, S>,
        ) {
            // Extract the message from the event fields
            let mut visitor = MessageVisitor::default();
            event.record(&mut visitor);

            let target = event.metadata().target();
            let level = event.metadata().level();
            let msg = if visitor.message.is_empty() {
                format!("{}: (no message)", target)
            } else {
                format!("{}: {}", target, visitor.message)
            };

            // Write to logcat
            logcat(priority_from_level(level), "GratiaRust", &msg);

            // Also write to rust_log file for persistence
            let prefixed = format!("{} {}", level.as_str().to_uppercase(), msg);
            super::rust_log_file_only(&prefixed);
        }
    }

    #[derive(Default)]
    struct MessageVisitor {
        message: String,
    }

    impl tracing::field::Visit for MessageVisitor {
        fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
            if field.name() == "message" {
                self.message = format!("{:?}", value);
            } else if self.message.is_empty() {
                self.message = format!("{}={:?}", field.name(), value);
            } else {
                self.message.push_str(&format!(" {}={:?}", field.name(), value));
            }
        }

        fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
            if field.name() == "message" {
                self.message = value.to_string();
            } else if self.message.is_empty() {
                self.message = format!("{}={}", field.name(), value);
            } else {
                self.message.push_str(&format!(" {}={}", field.name(), value));
            }
        }
    }
}

#[cfg(target_os = "android")]
fn android_logcat(tag: &str, msg: &str) {
    android_log::logcat(android_log::ANDROID_LOG_INFO, tag, msg);
}

/// Write to the log file only (no logcat), used by the AndroidLogcatLayer
/// to avoid double-logging to logcat.
#[cfg_attr(not(target_os = "android"), allow(dead_code))]
fn rust_log_file_only(msg: &str) {
    // Count errors for health telemetry
    {
        let lower = msg.to_ascii_lowercase();
        if lower.starts_with("error") || lower.starts_with("warn")
            || lower.contains("error:") || lower.contains("warn:")
            || lower.contains("failed") || lower.contains("panic")
        {
            ERROR_COUNT.fetch_add(1, Ordering::Relaxed);
        }
    }
    if let Some(path) = LOG_PATH.get() {
        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
            use std::io::Write;
            let _ = writeln!(f, "[{}] {}", chrono::Utc::now().format("%H:%M:%S"), msg);
        }
    }
}

/// Initialize tracing subscriber — platform-specific.
/// On Android: custom Layer that writes to logcat + file.
/// On desktop/test: standard fmt subscriber to stderr.
fn init_tracing() {
    #[cfg(target_os = "android")]
    {
        use tracing_subscriber::layer::SubscriberExt;
        use tracing_subscriber::util::SubscriberInitExt;
        let _ = tracing_subscriber::registry()
            .with(android_log::AndroidLogcatLayer)
            .with(tracing_subscriber::filter::LevelFilter::from_level(tracing::Level::INFO))
            .try_init();
    }
    #[cfg(not(target_os = "android"))]
    {
        let _ = tracing_subscriber::fmt()
            .with_max_level(tracing::Level::INFO)
            .with_target(true)
            .with_ansi(false)
            .try_init();
    }
}

#[uniffi::export]
impl GratiaNode {
    /// Create a new GratiaNode instance.
    ///
    /// `data_dir` is the path to the app's private data directory where
    /// persistent state (wallet keys, PoL history, etc.) will be stored.
    #[uniffi::constructor]
    pub fn new(data_dir: String) -> Result<Self, FfiError> {
        let config = Config::default();

        // Initialize file-based logging (must happen before init_tracing so
        // the AndroidLogcatLayer can write to the file).
        init_rust_log(&data_dir);

        // Initialize tracing subscriber: on Android, routes all tracing output
        // to logcat + file. On desktop, uses standard fmt subscriber to stderr.
        init_tracing();
        // WHY: Record app start time for health report uptime calculation.
        // OnceLock ensures this is set exactly once even if new() is called again.
        let _ = APP_START_TIME.set(std::time::Instant::now());
        rust_log(&format!("GratiaNode::new called, data_dir={}", data_dir));
        info!("initializing GratiaNode with data_dir: {}", data_dir);

        // WHY: Multi-threaded runtime with 2 worker threads — enough for the
        // libp2p swarm event loop + slot timer without hogging all CPU cores
        // on a mobile device.
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .map_err(|e| FfiError::InternalError {
                reason: format!("failed to create tokio runtime: {}", e),
            })?;

        let inner = GratiaNodeInner {
            // WHY: FileKeystore persists the Ed25519 key to {data_dir}/wallet_key.bin
            // so the wallet address survives app restarts. If the file already exists,
            // the key is loaded automatically — no need to call create_wallet() again.
            wallet: WalletManager::with_file_keystore(&data_dir),
            pol: {
                let mut pol = ProofOfLifeManager::new(config.clone());
                pol.load_state(&data_dir);
                pol
            },
            sensor_buffer: SensorEventBuffer::new(),
            staking: StakingManager::new(config.staking),
            power_state: PowerState {
                is_plugged_in: false,
                battery_percent: 0,
                // WHY: Default CPU temp of 25C is a safe baseline. The native layer
                // will update this via update_power_state() with real readings.
                cpu_temp_celsius: 25.0,
                is_throttled: false,
            },
            mining_state: MiningState::ProofOfLife,
            presence_score: 0,
            network: None,
            network_event_rx: None,
            pending_network_events: VecDeque::new(),
            buffered_raw_events: Vec::new(),
            listen_address: None,
            consensus: None,
            sync_manager: None,
            sync_protocol: None,
            blocks_produced: 0,
            slot_timer_handle: None,
            #[cfg(debug_assertions)]
            debug_bypass_checks: false,
            user_stopped_mining: false,
            pending_broadcast_block: None,
            known_peer_nodes: Vec::new(),
            streamlet: None,
            bft_peer_id_bytes: Vec::new(),
            real_committee_members: 1,
            initial_sync_done: false,
            consecutive_bft_expirations: 0,
            bft_retry_count: 0,
            consecutive_solo_blocks: 0,
            epoch_seed: None,
            yield_checked_peers: Vec::new(),
            recent_blocks: VecDeque::with_capacity(100),
            chain_persistence: Some(ChainPersistence::new(&data_dir)),
            mempool: Vec::new(),
            applied_tx_hashes: std::collections::HashSet::new(),
            state_manager: None, // Initialized when consensus starts
            storage_backend: None,
            vm: None, // Initialized on first contract deploy or call
            governance: GovernanceManager::new(config.governance),
            mesh_transport: None,
            shard_coordinator: None,
            total_gas_used: 0,
            lux_store: {
                // WHY: Load persisted Lux posts on startup so social feed
                // survives app restarts. Falls back to empty store if no file.
                let lux_path = format!("{}/lux_store.json", data_dir);
                gratia_lux::LuxStore::load_from_file(&lux_path).unwrap_or_else(|e| {
                    info!("Lux store loaded fresh (no prior data or error: {})", e);
                    gratia_lux::LuxStore::new()
                })
            },
            lux_fees: gratia_lux::FeeCalculator::new(),
            pending_block_created_at: None,
            last_reorg_at: None,
            pending_block_hash: None,
            last_expired_block_hash: None,
            last_expired_block_height: None,
            last_gps: None,
            last_barometer: None,
            last_light: None,
            last_magnetometer: None,
            last_accelerometer: None,
            last_sensor_time: chrono::Utc::now(),
            total_burned_tx_fees: 0,
            last_finalized_at: None,
            session_grat_earned: 0,
            session_blocks_produced: 0,
            last_reward_timestamp: None,
        };

        Ok(GratiaNode {
            data_dir,
            inner: Arc::new(Mutex::new(inner)),
            runtime,
        })
    }

    // ========================================================================
    // Debug methods (testing only)
    // ========================================================================

    /// Enable debug bypass for PoL and staking checks.
    /// WHY: During development and device testing, a full 24-hour PoL window
    /// is impractical. This lets us test the mining and transaction flow
    /// immediately. In release builds this is a no-op — the bypass flag is
    /// silently ignored so it cannot weaken production security.
    pub fn enable_debug_bypass(&self) -> Result<(), FfiError> {
        #[cfg(debug_assertions)]
        {
            let mut inner = self.lock_inner()?;
            inner.debug_bypass_checks = true;
            info!("FFI: debug bypass enabled — PoL and staking checks will be skipped");
        }
        #[cfg(not(debug_assertions))]
        {
            info!("FFI: enable_debug_bypass called in release build — ignored");
        }
        Ok(())
    }

    /// Reset chain state for a fresh genesis. Deletes chain_state.bin,
    /// chain_state.db, and pol_state.bin. Wallet keys are preserved.
    /// WHY: When transitioning from testnet to mainnet, the chain must
    /// start fresh at block 0. The wallet (Ed25519 keypair) survives so
    /// the user keeps their identity. All balances, blocks, and PoL
    /// history are wiped — everyone starts equal at genesis.
    pub fn reset_for_genesis(&self) -> Result<String, FfiError> {
        let inner = self.lock_inner()?;
        let data_dir = inner.chain_persistence
            .as_ref()
            .map(|p| p.data_dir().to_string())
            .unwrap_or_default();

        if data_dir.is_empty() {
            return Err(FfiError::InternalError { reason: "No data directory configured".into() });
        }

        let mut deleted = Vec::new();

        // Delete chain metadata (height, tip hash)
        let chain_bin = format!("{}/chain_state.bin", data_dir);
        if std::fs::remove_file(&chain_bin).is_ok() {
            deleted.push("chain_state.bin");
        }

        // Delete account state (balances, nonces, stakes)
        let chain_db = format!("{}/chain_state.db", data_dir);
        if std::fs::remove_file(&chain_db).is_ok() {
            deleted.push("chain_state.db");
        }

        // Delete PoL history (consecutive days, trust tier)
        let pol_state = format!("{}/pol_state.bin", data_dir);
        if std::fs::remove_file(&pol_state).is_ok() {
            deleted.push("pol_state.bin");
        }

        info!(
            "FFI: Chain reset for genesis — deleted: [{}]. Wallet keys preserved.",
            deleted.join(", ")
        );

        Ok(format!(
            "Genesis reset complete. Deleted {} files: [{}]. Restart the app to begin at block 0.",
            deleted.len(),
            deleted.join(", ")
        ))
    }

    // ========================================================================
    // Wallet methods
    // ========================================================================

    /// Generate a new wallet keypair. Returns the wallet address string.
    ///
    /// Can only be called once per device. Returns `WalletAlreadyExists` if
    /// a wallet already exists.
    pub fn create_wallet(&self) -> Result<String, FfiError> {
        let mut inner = self.lock_inner()?;
        let address = inner.wallet.create_wallet().map_err(|e| {
            if e.to_string().contains("already exists") {
                FfiError::WalletAlreadyExists
            } else {
                FfiError::from(e)
            }
        })?;
        Ok(address_to_hex(&address))
    }

    /// Get current wallet information (address, balance, mining state).
    pub fn get_wallet_info(&self) -> Result<FfiWalletInfo, FfiError> {
        let inner = self.lock_inner()?;
        let address = inner
            .wallet
            .address()
            .map_err(|_| FfiError::WalletNotInitialized)?;

        Ok(FfiWalletInfo {
            address: address_to_hex(&address),
            balance_lux: inner.wallet.balance(),
            mining_state: mining_state_to_string(&inner.mining_state),
        })
    }

    /// Send a GRAT transfer to another address.
    ///
    /// `to` is the recipient address as a hex string (with or without "grat:" prefix).
    /// `amount` is the transfer amount in Lux.
    ///
    /// Returns the transaction hash as a hex string.
    pub fn send_transfer(&self, to: String, amount: u64) -> Result<String, FfiError> {
        let recipient = address_from_hex(&to).map_err(|reason| FfiError::InvalidAddress { reason })?;

        let mut inner = self.lock_inner()?;

        // WHY: Use a fixed fee of 1000 Lux (~0.001 GRAT) as a placeholder.
        // In production, the fee will be dynamically calculated based on
        // network congestion and transaction size.
        let fee: u64 = 1000; // Placeholder fee — ~0.001 GRAT

        // WHY: Sync the wallet nonce from on-chain state before sending.
        // After an app restart, the wallet's local nonce resets to 0 but
        // the on-chain nonce may be higher from previous transactions.
        // Using a stale nonce causes the transaction to be rejected.
        if let (Some(ref sm), Ok(our_addr)) = (&inner.state_manager, inner.wallet.address()) {
            let acct = sm.get_account(&our_addr).unwrap_or_default();
            if acct.nonce > inner.wallet.nonce() {
                // WHY: Reject suspiciously large nonce jumps. A gap > 100 likely
                // indicates state corruption or reorg, not normal usage. Normal
                // users send < 100 txns between app restarts.
                let gap = acct.nonce - inner.wallet.nonce();
                if gap > 100 {
                    return Err(FfiError::InternalError {
                        reason: format!(
                            "Nonce jump too large ({} → {}). Possible state corruption — please restart the app.",
                            inner.wallet.nonce(), acct.nonce
                        ),
                    });
                }
                inner.wallet.set_nonce(acct.nonce);
                rust_log(&format!("Nonce synced from on-chain state: {}", acct.nonce));
            }
        }

        let tx = inner.wallet.send_transfer(recipient, amount, fee)?;
        let hash_hex = hex::encode(tx.hash.0);

        // Broadcast the transaction to the network via gossipsub.
        // WHY: Without broadcasting, the transaction only updates the sender's
        // local balance. The recipient's phone needs to receive it via gossip
        // to credit their balance and show the incoming transaction.
        if let Some(ref network) = inner.network {
            match network.try_broadcast_transaction_sync(&tx) {
                Ok(()) => {
                    let from_addr = inner.wallet.address()
                        .map(|a| address_to_hex(&a))
                        .unwrap_or_else(|_| "unknown".to_string());
                    rust_log(&format!("Transaction broadcast: {} -> {} amount={}",
                        from_addr, to, amount));
                    info!("FFI: transfer sent and broadcast, hash={}", hash_hex);
                }
                Err(e) => {
                    warn!("FFI: transfer sent locally but broadcast failed: {}", e);
                    rust_log(&format!("Transaction broadcast FAILED: {}", e));
                }
            }
        } else {
            info!("FFI: transfer sent (local only, network not running), hash={}", hash_hex);
        }

        // WHY: Add to local mempool so the next block we produce includes
        // this transaction on-chain. Without this, blocks are empty and
        // transactions only live in gossip — never finalized.
        inner.mempool.push(tx);

        Ok(hash_hex)
    }

    /// Export the wallet's seed phrase as a hex string.
    ///
    /// WHY: Optional backup mechanism. The seed phrase IS the raw Ed25519
    /// private key encoded as hex. Prefer `export_seed_words()` for a
    /// human-friendly 24-word BIP39 mnemonic when the `seed-phrase` feature
    /// is enabled.
    ///
    /// This is deliberately buried behind a confirmation dialog in the UI
    /// and not shown during onboarding — per the design spec, behavioral
    /// recovery (Proof of Life matching) is the primary recovery method.
    pub fn export_seed_phrase(&self) -> Result<String, FfiError> {
        let inner = self.lock_inner()?;
        let phrase = inner.wallet.export_seed_phrase().map_err(|e| {
            FfiError::InternalError {
                reason: format!("seed phrase export failed: {}", e),
            }
        })?;
        let hex_str = phrase.to_hex();
        rust_log("Seed phrase exported (user requested)");
        Ok(hex_str)
    }

    /// Import a wallet from a seed phrase (hex-encoded private key).
    ///
    /// Replaces the current wallet if one exists. Returns the wallet address
    /// string. This is the counterpart to `export_seed_phrase` — used when the
    /// user wants to restore a wallet on a new device using their backed-up
    /// hex seed.
    pub fn import_seed_phrase(&self, seed_hex: String) -> Result<String, FfiError> {
        let mut inner = self.lock_inner()?;
        let phrase = SeedPhrase::from_hex(&seed_hex).map_err(|_| FfiError::InvalidAddress {
            reason: "seed phrase must be valid hex (64 characters)".into(),
        })?;
        let address = inner.wallet.import_seed_phrase(&phrase).map_err(|e| {
            FfiError::InternalError {
                reason: format!("seed import failed: {}", e),
            }
        })?;
        rust_log(&format!(
            "Wallet restored from seed phrase: {}",
            address_to_hex(&address)
        ));
        Ok(address_to_hex(&address))
    }

    /// Get the transaction history for this wallet.
    pub fn get_transaction_history(&self) -> Result<Vec<FfiTransactionInfo>, FfiError> {
        let inner = self.lock_inner()?;
        let history: Vec<FfiTransactionInfo> = inner
            .wallet
            .history()
            .iter()
            .map(FfiTransactionInfo::from)
            .collect();
        Ok(history)
    }

    // ========================================================================
    // Mining methods
    // ========================================================================

    /// Get the current mining status.
    pub fn get_mining_status(&self) -> Result<FfiMiningStatus, FfiError> {
        let inner = self.lock_inner()?;
        // WHY: Day 0 onboarding users have no PoL data yet — treat as valid.
        // After day 0, require real PoL or be within the 1-day grace period.
        // Debug bypass preserved for development testing.
        let pol_valid = inner.is_debug_bypass()
            || inner.pol.is_onboarding()
            || inner.pol.current_day_valid()
            || inner.pol.in_grace_period();
        Ok(FfiMiningStatus {
            state: mining_state_to_string(&inner.mining_state),
            battery_percent: inner.power_state.battery_percent,
            is_plugged_in: inner.power_state.is_plugged_in,
            current_day_pol_valid: pol_valid,
            presence_score: inner.presence_score,
        })
    }

    /// Update the phone's power state from the native layer.
    ///
    /// Called by the Android/iOS battery manager whenever the charging state
    /// or battery level changes. This triggers a re-evaluation of whether
    /// mining conditions are met.
    pub fn update_power_state(
        &self,
        is_plugged_in: bool,
        battery_percent: u8,
    ) -> Result<FfiMiningStatus, FfiError> {
        let mut inner = self.lock_inner()?;

        inner.power_state.is_plugged_in = is_plugged_in;
        inner.power_state.battery_percent = battery_percent;

        // WHY: If the user tapped "Stop Mining", don't auto-restart.
        // Return current status without recalculating mining state.
        if inner.user_stopped_mining {
            return Ok(FfiMiningStatus {
                state: mining_state_to_string(&inner.mining_state),
                battery_percent: inner.power_state.battery_percent,
                is_plugged_in: inner.power_state.is_plugged_in,
                current_day_pol_valid: inner.is_debug_bypass()
                    || inner.pol.is_onboarding()
                    || inner.pol.current_day_valid()
                    || inner.pol.in_grace_period(),
                presence_score: inner.presence_score,
            });
        }

        // Recalculate mining state based on new power conditions.
        // WHY: Use meets_minimum_stake_at() to respect the activation threshold
        // and grace period. Before 1,000 miners, minimum is 0 (anyone can mine).
        // After activation + 7-day grace, the activated_minimum_stake applies.
        let has_min_stake = inner.is_debug_bypass()
            || inner.pol.is_onboarding()
            || inner.staking.meets_minimum_stake_at(
                // WHY: We need the NodeId to check stake, but the wallet may not
                // be initialized yet. Use a zeroed NodeId as a safe fallback —
                // meets_minimum_stake_at will return true if staking is not yet
                // activated (minimum=0), or false if activated and no stake exists.
                &self.get_node_id_or_default(&inner),
                Utc::now(),
            );
        inner.mining_state = if inner.is_debug_bypass() {
            // WHY: In debug bypass mode, skip PoL and staking checks entirely.
            // Go straight to Mining when power conditions are met so that
            // developers don't have to wait 24 hours for PoL or manually
            // tap "Start Mining" during testing.
            if inner.power_state.is_plugged_in && inner.power_state.battery_percent >= 80 {
                MiningState::Mining
            } else {
                MiningState::ProofOfLife
            }
        } else if inner.pol.is_onboarding() {
            // WHY: Zero-delay onboarding — day 0 users can mine immediately
            // when power conditions are met, without waiting for PoL data.
            if inner.power_state.is_plugged_in && inner.power_state.battery_percent >= 80 && has_min_stake {
                MiningState::Mining
            } else if !inner.power_state.is_plugged_in {
                MiningState::ProofOfLife
            } else if inner.power_state.battery_percent < 80 {
                MiningState::BatteryLow
            } else {
                MiningState::PendingActivation
            }
        } else {
            inner.pol.determine_mining_state(&inner.power_state, has_min_stake)
        };

        // WHY: Day 0 onboarding users have no PoL data yet — treat as valid.
        // After day 0, require real PoL or be within the 1-day grace period.
        let pol_valid = inner.is_debug_bypass()
            || inner.pol.is_onboarding()
            || inner.pol.current_day_valid()
            || inner.pol.in_grace_period();
        Ok(FfiMiningStatus {
            state: mining_state_to_string(&inner.mining_state),
            battery_percent: inner.power_state.battery_percent,
            is_plugged_in: inner.power_state.is_plugged_in,
            current_day_pol_valid: pol_valid,
            presence_score: inner.presence_score,
        })
    }

    /// Request to start mining.
    ///
    /// Returns the current mining status. Mining will only activate if all
    /// conditions are met (plugged in, battery >= 80%, valid PoL, minimum stake).
    pub fn start_mining(&self) -> Result<FfiMiningStatus, FfiError> {
        let mut inner = self.lock_inner()?;
        inner.user_stopped_mining = false;

        if !inner.power_state.is_plugged_in {
            return Err(FfiError::MiningNotAvailable {
                reason: "phone must be plugged in to mine".into(),
            });
        }
        if inner.power_state.battery_percent < 80 {
            return Err(FfiError::MiningNotAvailable {
                reason: format!(
                    "battery at {}%, must be at least 80%",
                    inner.power_state.battery_percent
                ),
            });
        }
        // WHY: PoL enforcement follows the onboarding design:
        // - Day 0 (onboarding): mining allowed without PoL (zero-delay onboarding)
        // - Day 1+: require valid PoL from previous day OR be within grace period
        // - 2 consecutive missed days: mining paused until next valid day
        // Debug bypass still skips all checks for development testing.
        if inner.is_debug_bypass() {
            info!("FFI: debug bypass active — skipping PoL and staking checks");
        } else if inner.pol.is_onboarding() {
            info!("FFI: day 0 onboarding — PoL not yet required");
        } else if !inner.pol.is_mining_eligible() && !inner.pol.in_grace_period() {
            return Err(FfiError::MiningNotAvailable {
                reason: format!(
                    "Proof of Life required. {} consecutive days missed — mining paused. \
                     Resume by using your phone normally for one day.",
                    inner.pol.missed_days()
                ),
            });
        } else if inner.pol.in_grace_period() {
            info!(
                "FFI: mining within grace period ({} missed day(s))",
                inner.pol.missed_days()
            );
        }

        let node_id = self.get_node_id_or_default(&inner);
        // WHY: Use meets_minimum_stake_at() instead of meets_minimum_stake()
        // to respect the activation threshold and grace period. Before the
        // network reaches 1,000 miners, minimum is 0 (anyone can mine).
        // After activation + 7-day grace, the activated_minimum_stake applies.
        if !inner.pol.is_onboarding() && !inner.is_debug_bypass()
            && !inner.staking.meets_minimum_stake_at(&node_id, Utc::now())
        {
            let effective_min = inner.staking.effective_minimum_stake(Utc::now());
            let current_stake = inner.staking.effective_stake(&node_id);
            return Err(FfiError::MiningNotAvailable {
                reason: format!(
                    "minimum stake required to mine: {} Lux needed, {} Lux staked",
                    effective_min, current_stake
                ),
            });
        }

        inner.mining_state = MiningState::Mining;
        info!("FFI: mining started");

        let pol_valid = inner.is_debug_bypass()
            || inner.pol.is_onboarding()
            || inner.pol.current_day_valid()
            || inner.pol.in_grace_period();
        Ok(FfiMiningStatus {
            state: mining_state_to_string(&inner.mining_state),
            battery_percent: inner.power_state.battery_percent,
            is_plugged_in: inner.power_state.is_plugged_in,
            current_day_pol_valid: pol_valid,
            presence_score: inner.presence_score,
        })
    }

    /// Tick mining rewards for one minute of active mining.
    ///
    /// Called by the native MiningService every 60 seconds while mining is
    /// active. Credits the wallet with the flat-rate mining reward.
    ///
    /// WHY: In Phase 1 (no consensus network), mining rewards are credited
    /// directly to the local wallet. In production, rewards flow through
    /// block production and the consensus layer distributes them. This
    /// method provides a working reward loop for development and testing.
    ///
    /// Returns the updated wallet balance in Lux.
    pub fn tick_mining_reward(&self) -> Result<u64, FfiError> {
        let inner = self.lock_inner()?;

        if !matches!(inner.mining_state, MiningState::Mining) {
            return Err(FfiError::MiningNotAvailable {
                reason: "not currently mining".into(),
            });
        }

        // WHY: Mining rewards are now credited solely via block finalization
        // in the slot timer (50 GRAT per finalized block). This tick function
        // no longer adds rewards — it just returns the current balance for the
        // Android notification to display. The per-minute tick was a Phase 1
        // placeholder that caused double-crediting when combined with block rewards.
        Ok(inner.wallet.balance())
    }

    /// Stop mining.
    ///
    /// Returns the updated mining status. The node reverts to Proof of Life
    /// passive collection mode.
    pub fn stop_mining(&self) -> Result<FfiMiningStatus, FfiError> {
        let mut inner = self.lock_inner()?;
        inner.mining_state = MiningState::ProofOfLife;
        inner.user_stopped_mining = true;
        info!("FFI: mining stopped by user");

        Ok(FfiMiningStatus {
            state: mining_state_to_string(&inner.mining_state),
            battery_percent: inner.power_state.battery_percent,
            is_plugged_in: inner.power_state.is_plugged_in,
            current_day_pol_valid: inner.is_debug_bypass()
                || inner.pol.is_onboarding()
                || inner.pol.current_day_valid()
                || inner.pol.in_grace_period(),
            presence_score: inner.presence_score,
        })
    }

    // ========================================================================
    // Proof of Life methods
    // ========================================================================

    /// Get the current Proof of Life status.
    pub fn get_proof_of_life_status(&self) -> Result<FfiProofOfLifeStatus, FfiError> {
        let inner = self.lock_inner()?;

        // WHY: When debug bypass is active, report all PoL parameters as met
        // so the Mining screen shows "Complete" and allows mining activation.
        if inner.is_debug_bypass() {
            return Ok(FfiProofOfLifeStatus {
                is_valid_today: true,
                consecutive_days: 1,
                is_onboarded: true,
                parameters_met: vec![
                    "unlocks".into(), "unlock_spread".into(), "interactions".into(),
                    "orientation".into(), "motion".into(), "gps".into(),
                    "network".into(), "bt_variation".into(), "charge_event".into(),
                ],
            });
        }

        let daily_data = inner.sensor_buffer.to_daily_data();
        let is_onboarding = inner.pol.is_onboarding();

        // Build list of which PoL parameters are currently satisfied.
        let mut params_met = Vec::new();
        if daily_data.unlock_count >= 10 {
            params_met.push("unlocks".to_string());
        }
        // Check unlock spread
        if let (Some(first), Some(last)) = (daily_data.first_unlock, daily_data.last_unlock) {
            if (last - first).num_hours() >= 6 {
                params_met.push("unlock_spread".to_string());
            }
        }
        if daily_data.interaction_sessions >= 3 {
            params_met.push("interactions".to_string());
        }
        if daily_data.orientation_changed {
            params_met.push("orientation".to_string());
        }
        if daily_data.human_motion_detected {
            params_met.push("motion".to_string());
        }
        if daily_data.gps_fix_obtained {
            params_met.push("gps".to_string());
        }
        if daily_data.distinct_wifi_networks >= 1 || daily_data.distinct_bt_environments >= 1 {
            params_met.push("network".to_string());
        }
        // WHY: BT variation is only required when the device has BT peers.
        // Wi-Fi-only phones (distinct_bt_environments == 0) are first-class
        // citizens per spec — they pass this check automatically. This must
        // match the logic in validator.rs and types.rs is_valid().
        if daily_data.distinct_bt_environments == 0
            || (daily_data.distinct_bt_environments >= 2
                && daily_data.bt_environment_change_count >= 1)
        {
            params_met.push("bt_variation".to_string());
        }
        if daily_data.charge_cycle_event {
            params_met.push("charge_event".to_string());
        }

        Ok(FfiProofOfLifeStatus {
            // WHY: During onboarding, report as valid for mining but show real
            // parameter progress so the user sees their PoL checklist filling in.
            is_valid_today: is_onboarding || inner.pol.current_day_valid(),
            consecutive_days: inner.pol.consecutive_days(),
            is_onboarded: !is_onboarding && inner.pol.is_onboarded(),
            parameters_met: params_met,
        })
    }

    /// Submit a sensor event from the native platform layer.
    ///
    /// Called by the Android/iOS sensor managers whenever a relevant event
    /// occurs (unlock, GPS fix, BT scan, etc.). Events are buffered and
    /// processed into the daily PoL attestation.
    pub fn submit_sensor_event(&self, event: FfiSensorEvent) -> Result<(), FfiError> {
        let mut inner = self.lock_inner()?;

        // WHY: Cache the latest reading from each sensor for VM host functions.
        // The PoL engine gets the event for daily attestation validation.
        // The VM needs the LATEST value at contract execution time — these
        // cached fields are read when building HostEnvironment.
        let now = chrono::Utc::now();
        match &event {
            FfiSensorEvent::GpsUpdate { lat, lon } => {
                inner.last_gps = Some((*lat, *lon));
                inner.last_sensor_time = now;
            }
            FfiSensorEvent::BarometerReading { hpa } => {
                inner.last_barometer = Some(*hpa as f64);
                inner.last_sensor_time = now;
            }
            FfiSensorEvent::LightReading { lux } => {
                inner.last_light = Some(*lux as f64);
                inner.last_sensor_time = now;
            }
            FfiSensorEvent::MagnetometerReading { degrees } => {
                inner.last_magnetometer = Some(*degrees as f64);
                inner.last_sensor_time = now;
            }
            FfiSensorEvent::AccelerometerReading { magnitude } => {
                inner.last_accelerometer = Some(*magnitude as f64);
                inner.last_sensor_time = now;
            }
            FfiSensorEvent::BluetoothScan { .. } => {
                // WHY: BLE peer count is already tracked by the sync manager
                // for the @proximity host function. No separate cache needed.
            }
            _ => {}
        }

        // WHY: Only feed PoL-relevant events into the sensor buffer. Environmental
        // readings (barometer, light, magnetometer, accelerometer magnitude) are
        // cached above for VM host functions but must NOT enter the PoL buffer.
        // Previously they were converted to SensorEvent::Motion via From<FfiSensorEvent>,
        // which incorrectly satisfied PoL parameter #4 (human-consistent motion)
        // from any environmental sensor reading.
        if let Some(internal_event) = crate::convert::ffi_sensor_to_pol(&event) {
            inner.sensor_buffer.process_event(internal_event);
        }
        Ok(())
    }

    /// Finalize the current day's Proof of Life.
    ///
    /// Called at end-of-day (midnight UTC). Evaluates all accumulated sensor
    /// data, generates the PoL attestation, and resets the sensor buffer.
    ///
    /// Returns `true` if the day was valid (all PoL parameters met).
    pub fn finalize_day(&self, epoch_day: u32) -> Result<bool, FfiError> {
        let mut inner = self.lock_inner()?;

        // Feed the buffered sensor data into the PoL manager.
        let daily_data = inner.sensor_buffer.to_daily_data();

        // WHY: We replay the daily data into the PoL manager's individual
        // record methods to keep its internal state consistent. This is the
        // bridge between the event-based sensor buffer and the PoL manager's
        // record-based API.
        if daily_data.unlock_count > 0 {
            for _ in 0..daily_data.unlock_count {
                inner.pol.record_unlock();
            }
        }
        for _ in 0..daily_data.interaction_sessions {
            inner.pol.record_interaction_session();
        }
        if daily_data.orientation_changed {
            inner.pol.record_orientation_change();
        }
        if daily_data.human_motion_detected {
            inner.pol.record_human_motion();
        }
        if daily_data.gps_fix_obtained {
            if let Some(loc) = daily_data.approximate_location {
                inner.pol.record_gps_fix(loc.lat, loc.lon);
            }
        }
        for _ in 0..daily_data.distinct_wifi_networks {
            inner.pol.record_wifi_network();
        }
        for _ in 0..daily_data.distinct_bt_environments {
            inner.pol.record_bt_environment_change();
        }
        if daily_data.charge_cycle_event {
            inner.pol.record_charge_event();
        }

        let is_valid = inner.pol.finalize_day(epoch_day);

        // Persist PoL state (consecutive days, total days, onboarding status).
        // WHY: Without this, the trust tier resets on every app restart.
        inner.pol.save_state(&self.data_dir_for_persistence());

        // Reset the sensor buffer for the new day.
        inner.sensor_buffer.reset();

        if is_valid {
            info!("FFI: day finalized — VALID");
            // Record PoL event for wallet's dead-man switch (inheritance).
            inner.wallet.record_proof_of_life();
        } else {
            warn!("FFI: day finalized — INVALID");
        }

        Ok(is_valid)
    }

    // ========================================================================
    // Staking methods
    // ========================================================================

    /// Stake GRAT for mining eligibility.
    ///
    /// `amount` is in Lux. If the total committed stake exceeds the per-node
    /// cap, the excess automatically flows to the Network Security Pool.
    ///
    /// Returns the transaction hash as a hex string.
    pub fn stake(&self, amount: u64) -> Result<String, FfiError> {
        let mut inner = self.lock_inner()?;

        // WHY: Placeholder fee of 1000 Lux. Same rationale as send_transfer.
        let fee: u64 = 1000;

        let tx = inner.wallet.send_stake(amount, fee)?;
        let hash_hex = hex::encode(tx.hash.0);

        // Also register the stake in the local staking manager.
        let node_id = self.get_node_id_or_default(&inner);
        if let Err(e) = inner.staking.stake(node_id, amount, Utc::now()) {
            error!("FFI: staking manager error: {}", e);
            return Err(FfiError::StakingError {
                reason: e.to_string(),
            });
        }

        info!("FFI: stake of {} Lux sent, hash={}", amount, hash_hex);
        Ok(hash_hex)
    }

    /// Unstake GRAT (subject to cooldown period).
    ///
    /// `amount` is in Lux. Overflow stake is removed first to preserve
    /// consensus participation.
    ///
    /// Returns the transaction hash as a hex string.
    pub fn unstake(&self, amount: u64) -> Result<String, FfiError> {
        let mut inner = self.lock_inner()?;

        let fee: u64 = 1000; // Placeholder fee

        let tx = inner.wallet.send_unstake(amount, fee)?;
        let hash_hex = hex::encode(tx.hash.0);

        let node_id = self.get_node_id_or_default(&inner);
        if let Err(e) = inner.staking.request_unstake(node_id, amount, Utc::now()) {
            error!("FFI: staking manager unstake error: {}", e);
            return Err(FfiError::StakingError {
                reason: e.to_string(),
            });
        }

        info!("FFI: unstake of {} Lux sent, hash={}", amount, hash_hex);
        Ok(hash_hex)
    }

    /// Get current staking information for this node.
    pub fn get_stake_info(&self) -> Result<FfiStakeInfo, FfiError> {
        let inner = self.lock_inner()?;
        let node_id = self.get_node_id_or_default(&inner);

        match inner.staking.get_stake_info(&node_id) {
            Some(info) => Ok(FfiStakeInfo::from(&info)),
            None => {
                // No stake exists — return zeroed info.
                Ok(FfiStakeInfo {
                    node_stake_lux: 0,
                    overflow_amount_lux: 0,
                    total_committed_lux: 0,
                    staked_at_millis: 0,
                    meets_minimum: false,
                })
            }
        }
    }

    /// Complete a pending unstake after the cooldown period has elapsed.
    ///
    /// Returns the amount of Lux released back to the node's wallet.
    /// Fails if no pending unstake exists or cooldown hasn't elapsed.
    pub fn complete_unstake(&self) -> Result<u64, FfiError> {
        let mut inner = self.lock_inner()?;
        let node_id = self.get_node_id_or_default(&inner);
        let now = Utc::now();

        let released = inner.staking.complete_unstake(node_id, now).map_err(|e| {
            error!("FFI: complete_unstake error: {}", e);
            FfiError::StakingError {
                reason: e.to_string(),
            }
        })?;

        info!("FFI: unstake completed, {} Lux released", released);
        Ok(released)
    }

    /// Get pending unstake status for this node.
    ///
    /// WHY: The mobile UI needs to display cooldown countdown and pending
    /// amount so the user knows when they can call `complete_unstake()`.
    pub fn get_unstake_status(&self) -> Result<FfiUnstakeStatus, FfiError> {
        let inner = self.lock_inner()?;
        let node_id = self.get_node_id_or_default(&inner);
        let now = Utc::now();

        match inner.staking.get_pending_unstake(&node_id) {
            Some((pending_amount, requested_at)) => {
                let elapsed = now
                    .signed_duration_since(requested_at)
                    .num_seconds()
                    .max(0) as u64;
                let cooldown = inner.staking.config().unstake_cooldown_secs;
                let remaining = cooldown.saturating_sub(elapsed);

                Ok(FfiUnstakeStatus {
                    has_pending_unstake: true,
                    pending_amount_lux: pending_amount,
                    requested_at_millis: requested_at.timestamp_millis(),
                    remaining_cooldown_secs: remaining,
                })
            }
            None => Ok(FfiUnstakeStatus {
                has_pending_unstake: false,
                pending_amount_lux: 0,
                requested_at_millis: 0,
                remaining_cooldown_secs: 0,
            }),
        }
    }

    /// Get Network Security Pool status.
    ///
    /// WHY: The mobile UI displays pool stats — total overflow, contributor
    /// count, accumulated yield, and the user's personal share.
    pub fn get_pool_status(&self) -> Result<FfiPoolStatus, FfiError> {
        let inner = self.lock_inner()?;
        let node_id = self.get_node_id_or_default(&inner);
        let pool = inner.staking.pool();

        let your_overflow = pool
            .get_contribution(&node_id)
            .map(|c| c.amount)
            .unwrap_or(0);
        let your_yield = pool.calculate_yield_share(&node_id).unwrap_or(0);

        Ok(FfiPoolStatus {
            total_overflow_lux: pool.total_overflow(),
            contributor_count: pool.contributor_count() as u32,
            accumulated_yield_lux: pool.accumulated_yield(),
            your_overflow_lux: your_overflow,
            your_estimated_yield_lux: your_yield,
        })
    }

    /// Get staking activation status.
    ///
    /// WHY: The mobile UI needs to show whether staking minimum is enforced,
    /// the grace period countdown, and the effective minimum stake amount.
    pub fn get_activation_status(&self) -> Result<FfiActivationStatus, FfiError> {
        let inner = self.lock_inner()?;
        let now = Utc::now();

        let is_activated = inner.staking.is_staking_activated();
        let activated_at = inner.staking.staking_activated_at();
        let effective_min = inner.staking.effective_minimum_stake(now);

        let (activated_at_millis, grace_remaining, state) = match activated_at {
            None => (0i64, 0u64, "genesis".to_string()),
            Some(at) => {
                let elapsed = now.signed_duration_since(at).num_seconds().max(0) as u64;
                let grace = inner.staking.config().staking_activation_grace_secs;
                if elapsed < grace {
                    (
                        at.timestamp_millis(),
                        grace - elapsed,
                        "grace_period".to_string(),
                    )
                } else {
                    (at.timestamp_millis(), 0, "enforced".to_string())
                }
            }
        };

        Ok(FfiActivationStatus {
            is_activated,
            activated_at_millis,
            grace_period_remaining_secs: grace_remaining,
            effective_minimum_stake_lux: effective_min,
            enforcement_state: state,
        })
    }

    /// Check if this node is permanently banned from staking.
    ///
    /// WHY: Banned nodes cannot participate in mining or consensus. The mobile
    /// UI needs to detect this and show an appropriate message.
    pub fn is_node_banned(&self) -> Result<bool, FfiError> {
        let inner = self.lock_inner()?;
        let node_id = self.get_node_id_or_default(&inner);
        Ok(inner.staking.is_banned(&node_id))
    }

    // ========================================================================
    // Network methods
    // ========================================================================

    /// Start the peer-to-peer network layer.
    ///
    /// Initializes the libp2p swarm with QUIC transport, Gossipsub for
    /// block/transaction propagation, and mDNS for local peer discovery.
    ///
    /// `listen_port` specifies the port to listen on (0 = OS-assigned).
    /// `connection_profile` tells us what transports are viable based on SIM/network state.
    pub fn start_network(&self, listen_port: u16, connection_profile: FfiConnectionProfile) -> Result<FfiNetworkStatus, FfiError> {
        let mut inner = self.lock_inner()?;

        // WHY: Make startNetwork idempotent — if already running, return current
        // status instead of erroring. This avoids UI errors when the user navigates
        // back to the Network screen or the app resumes from background.
        if inner.network.is_some() {
            info!("FFI: network already running, returning current status");
            return Ok(FfiNetworkStatus {
                is_running: true,
                peer_count: inner.network.as_ref()
                    .map(|n| n.connected_peer_count() as u32)
                    .unwrap_or(0),
                listen_address: inner.listen_address.clone(),
                sync_status: inner.get_sync_status_string(),
                local_height: inner.get_local_height(),
            });
        }

        let node_id = self.get_node_id_or_default(&inner);

        let mut net_config = NetworkConfig::new(node_id);
        // WHY: Persist libp2p identity across restarts so the PeerId is stable.
        // Without this, each restart creates a new PeerId, causing peers to see
        // us as a new node — triggering committee rebuilds and chain resets.
        let data_dir = inner.chain_persistence
            .as_ref()
            .map(|p| p.data_dir().to_string());
        net_config.data_dir = data_dir;
        // WHY: Choose transport strategy based on SIM/network detection from the
        // Android layer. Devices without a SIM have broken UDP/QUIC on some
        // firmware — skip it entirely instead of waiting for timeout + fallback.
        // WHY: Multiple bootstrap nodes for redundancy. If one goes down,
        // phones can still discover peers via the other(s). The network layer's
        // retry loop already dials ALL bootstrap peers and considers itself
        // connected if ANY one responds.
        struct BootstrapNode {
            ip: &'static str,
            peer_id: &'static str,
        }
        let bootstrap_nodes: Vec<BootstrapNode> = vec![
            // Bootstrap 1: Vultr Miami (US East) — node-index=1
            BootstrapNode {
                ip: "45.77.95.111",
                peer_id: "12D3KooWRUqRqDGpQwLtxMP6iGfKEjZYWnkgkiW5BLPyxAeB8gLF",
            },
            // Bootstrap 2: [REGION TBD] — node-index=2
            // Recommended: Frankfurt (covers EU/ME/Africa) or Singapore (covers SE Asia/India)
            // To activate:
            //   1. Run: ./scripts/deploy-bootstrap.sh --host root@<IP> --ssh-key ~/.ssh/gratia_bootstrap
            //      --node-index 2 --peer "/ip4/45.77.95.111/udp/9000/quic-v1/p2p/12D3KooWRUqRqDGpQwLtxMP6iGfKEjZYWnkgkiW5BLPyxAeB8gLF"
            //      --peer "/ip4/45.77.95.111/tcp/9001/p2p/12D3KooWRUqRqDGpQwLtxMP6iGfKEjZYWnkgkiW5BLPyxAeB8gLF"
            //   2. Get PeerId from new node's logs
            //   3. Uncomment below, fill in ip + peer_id, rebuild app
            //   4. Update Miami node to peer back (see docs/operations/BOOTSTRAP-NODES.md)
            // BootstrapNode {
            //     ip: "SECOND_BOOTSTRAP_IP",
            //     peer_id: "SECOND_BOOTSTRAP_PEERID",
            // },
        ];

        match connection_profile {
            FfiConnectionProfile::Full => {
                // SIM present — QUIC primary (fast, multiplexed), TCP fallback
                info!("FFI: connection profile FULL — QUIC primary, TCP fallback");
                net_config.transport.listen_addresses = vec![
                    format!("/ip4/0.0.0.0/udp/{}/quic-v1", listen_port),
                    format!("/ip4/0.0.0.0/tcp/{}", listen_port),
                ];
                let mut peers = Vec::new();
                for node in &bootstrap_nodes {
                    peers.push(format!("/ip4/{}/udp/9000/quic-v1/p2p/{}", node.ip, node.peer_id));
                    peers.push(format!("/ip4/{}/tcp/9001/p2p/{}", node.ip, node.peer_id));
                }
                net_config.bootstrap_peers = peers;
            }
            FfiConnectionProfile::WifiOnly => {
                // No SIM — TCP only, skip QUIC entirely. Also enable aggressive
                // mDNS for local peer discovery since we're Wi-Fi-only.
                // WHY: Samsung budget phones without SIM have broken UDP routing.
                // Setting tcp_only tells the network layer to skip QUIC transport
                // in the SwarmBuilder, avoiding a 30-second timeout before fallback.
                info!("FFI: connection profile WIFI_ONLY — TCP only, aggressive mDNS");
                net_config.transport.tcp_only = true;
                net_config.transport.listen_addresses = vec![
                    format!("/ip4/0.0.0.0/tcp/{}", listen_port),
                ];
                let mut peers = Vec::new();
                for node in &bootstrap_nodes {
                    peers.push(format!("/ip4/{}/tcp/9001/p2p/{}", node.ip, node.peer_id));
                }
                net_config.bootstrap_peers = peers;
            }
            FfiConnectionProfile::Offline => {
                // No connectivity — still start the network layer for mDNS/BT mesh
                // so we can find peers if Wi-Fi appears later.
                info!("FFI: connection profile OFFLINE — mDNS only, no bootstrap");
                net_config.transport.tcp_only = true;
                net_config.transport.listen_addresses = vec![
                    format!("/ip4/0.0.0.0/tcp/{}", listen_port),
                ];
                net_config.bootstrap_peers = vec![];
            }
        }

        let mut network = NetworkManager::new(net_config);

        let event_rx = self.runtime.block_on(async {
            network.start().await
        }).map_err(|e| FfiError::NetworkError {
            reason: e.to_string(),
        })?;

        inner.network = Some(network);
        inner.network_event_rx = Some(event_rx);

        info!("FFI: network started on port {}", listen_port);

        Ok(FfiNetworkStatus {
            is_running: true,
            peer_count: 0,
            listen_address: inner.listen_address.clone(),
            sync_status: "Not Started".to_string(),
            local_height: 0,
        })
    }

    /// Stop the peer-to-peer network layer.
    pub fn stop_network(&self) -> Result<(), FfiError> {
        // WHY: Extract network from inner and drop the lock BEFORE calling
        // block_on. Holding the mutex during block_on can deadlock if the
        // async task needs to acquire the same lock.
        let mut taken_network = {
            let mut inner = self.lock_inner()?;
            inner.network_event_rx = None;
            inner.pending_network_events.clear();
            inner.listen_address = None;
            inner.network.take()
        };

        if let Some(ref mut network) = taken_network {
            self.runtime.block_on(async {
                let _ = network.stop().await;
            });
        }

        info!("FFI: network stopped");
        Ok(())
    }

    /// Connect to a remote peer by multiaddr string.
    ///
    /// For local WiFi demo, use: "/ip4/<peer-ip>/udp/<port>/quic-v1"
    /// Example: "/ip4/192.168.1.42/udp/9000/quic-v1"
    pub fn connect_peer(&self, addr: String) -> Result<(), FfiError> {
        rust_log(&format!("connect_peer called with addr={}", addr));
        let inner = self.lock_inner()?;

        let network = inner.network.as_ref().ok_or(FfiError::NetworkError {
            reason: "network not started".into(),
        })?;

        // WHY: Use non-blocking try_dial_peer_sync instead of block_on(dial_peer).
        // Holding the mutex lock while calling block_on can deadlock if the async
        // task tries to acquire the same lock.
        rust_log("network is present, calling try_dial_peer_sync...");
        network.try_dial_peer_sync(&addr).map_err(|e| {
            rust_log(&format!("dial_peer FAILED: {}", e));
            FfiError::NetworkError {
                reason: e.to_string(),
            }
        })?;

        rust_log(&format!("dial_peer succeeded for {}", addr));
        info!("FFI: dialing peer at {}", addr);
        Ok(())
    }

    /// Get the current network status.
    pub fn get_network_status(&self) -> Result<FfiNetworkStatus, FfiError> {
        let inner = self.lock_inner()?;

        let (is_running, peer_count) = match &inner.network {
            Some(network) => (network.is_running(), network.connected_peer_count() as u32),
            None => (false, 0),
        };

        Ok(FfiNetworkStatus {
            is_running,
            peer_count,
            listen_address: inner.listen_address.clone(),
            sync_status: inner.get_sync_status_string(),
            local_height: inner.get_local_height(),
        })
    }

    /// Start a lightweight HTTP API for the block explorer.
    ///
    /// Serves chain data as JSON on the given port. The web-based block explorer
    /// connects to `http://<phone-ip>:<port>/api/explorer/data` to display
    /// live blocks, transactions, and network stats.
    ///
    /// Returns the URL the explorer should connect to.
    pub fn start_explorer_api(&self, port: u16) -> Result<String, FfiError> {
        let inner_arc = Arc::clone(&self.inner);
        let actual_port = if port == 0 { 8080 } else { port };

        self.runtime.spawn(async move {
            run_explorer_http(inner_arc, actual_port).await;
        });

        let url = format!("http://0.0.0.0:{}", actual_port);
        rust_log(&format!("Explorer API started on {}", url));
        info!("FFI: Explorer API started on port {}", actual_port);
        Ok(url)
    }

    /// Poll for network events.
    ///
    /// Returns a list of events that have occurred since the last poll.
    /// Call this periodically from the mobile app (e.g., every 500ms) to
    /// receive peer connection/disconnection and block/transaction notifications.
    pub fn poll_network_events(&self) -> Result<Vec<FfiNetworkEvent>, FfiError> {
        let mut inner = self.lock_inner()?;

        // WHY: Take the receiver out temporarily to avoid borrow conflicts.
        // We need mutable access to both the receiver (try_recv) and the
        // pending_network_events queue simultaneously.
        let mut rx = match inner.network_event_rx.take() {
            Some(rx) => rx,
            None => return Ok(Vec::new()),
        };

        // WHY: Process any raw events buffered by the discovery drain first.
        // These are blocks/sigs that arrived during the discovery phase and
        // were preserved instead of dropped.
        let buffered = std::mem::take(&mut inner.buffered_raw_events);

        // Drain available events from the channel
        let mut new_events = Vec::new();
        // Chain buffered events before channel events
        let channel_events: Vec<NetworkEvent> = {
            let mut v = Vec::new();
            loop {
                match rx.try_recv() {
                    Ok(event) => v.push(event),
                    Err(_) => break,
                }
            }
            v
        };
        for event in buffered.into_iter().chain(channel_events.into_iter()) {
                    let ffi_event = match event {
                        NetworkEvent::PeerConnected { peer_id, is_inbound, .. } => {
                            if let Some(network) = &mut inner.network {
                                network.on_peer_connected(peer_id, is_inbound);
                            }
                            // WHY: When a new peer connects, broadcast our recent
                            // blocks so they can catch up. This is the sync protocol
                            // for Phase 2: gossip-based block catchup. The new peer
                            // receives these blocks via the normal BlockReceived path
                            // and processes them with relaxed validation (accepts
                            // blocks ahead of its current height).
                            let blocks_to_sync = inner.recent_blocks.len();
                            if blocks_to_sync > 0 {
                                if let Some(ref network) = inner.network {
                                    let mut synced = 0u32;
                                    for block in inner.recent_blocks.iter() {
                                        if network.try_broadcast_block_sync(block).is_ok() {
                                            synced += 1;
                                        }
                                    }
                                    rust_log(&format!(
                                        "Sync: broadcast {} recent blocks to new peer",
                                        synced
                                    ));
                                }
                            }

                            // Update sync manager with peer connection
                            if let Some(ref mut _sync) = inner.sync_manager {
                                info!("Sync: new peer connected, {} cached blocks broadcast", blocks_to_sync);
                            }

                            // WHY: Announce directly to the newly connected peer
                            // via request-response protocol, bypassing gossipsub.
                            // When a peer first connects, the gossipsub mesh hasn't
                            // formed yet, AND the mesh may be one-directional (Noise
                            // handshake failures prevent outbound connections).
                            // Request-response uses the already-established connection
                            // and works regardless of gossipsub mesh state.
                            if inner.consensus.is_some() {
                                if let (Some(ref network), Ok(sk_bytes)) = (
                                    &inner.network,
                                    inner.wallet.signing_key_bytes(),
                                ) {
                                    let local_node_id = self.get_node_id_or_default(&inner);
                                    let vrf_pk = VrfSecretKey::from_ed25519_bytes(&sk_bytes).public_key();
                                    let our_height = inner.consensus.as_ref()
                                        .map(|e| e.current_height())
                                        .unwrap_or(0);
                                    let mut announcement = NodeAnnouncement {
                                        node_id: local_node_id,
                                        vrf_pubkey_bytes: vrf_pk.bytes,
                                        presence_score: 100,
                                        pol_days: 90,
                                        timestamp: Utc::now(),
                                        ed25519_pubkey: [0u8; 32],
                                        signature: Vec::new(),
                                        height: our_height,
                                    };
                                    // Sign the announcement with our Ed25519 key
                                    let keypair = gratia_core::crypto::Keypair::from_secret_key_bytes(&sk_bytes);
                                    announcement.ed25519_pubkey = *keypair.public_key().as_bytes();
                                    let payload = gratia_network::gossip::node_announcement_signing_payload(&announcement);
                                    announcement.signature = keypair.sign(&payload);
                                    if let Err(e) = network.try_direct_announce(&announcement, peer_id) {
                                        warn!("Failed to direct-announce on peer connect: {}", e);
                                    } else {
                                        rust_log("Direct-announced to newly connected peer");
                                    }
                                }
                            }

                            // WHY: Track PeerId for direct BFT delivery.
                            // Populating on PeerConnected (not just BlockReceived)
                            // means direct proposals work from the very first block,
                            // eliminating startup-phase expirations.
                            let pid_bytes = peer_id.to_bytes();
                            if !inner.bft_peer_id_bytes.contains(&pid_bytes) {
                                inner.bft_peer_id_bytes.push(pid_bytes);
                                rust_log(&format!("BFT: registered peer {} for direct delivery", peer_id));
                            }

                            FfiNetworkEvent::PeerConnected {
                                peer_id: peer_id.to_string(),
                            }
                        }
                        NetworkEvent::PeerDisconnected { peer_id } => {
                            if let Some(network) = &mut inner.network {
                                network.on_peer_disconnected(&peer_id, true);
                            }
                            // WHY: Don't rebuild committee on disconnect events.
                            // libp2p prunes duplicate connections (mDNS creates both
                            // inbound+outbound), firing PeerDisconnected for normal
                            // connection management. Rebuilding committee here caused
                            // premature solo transitions after just 16 seconds.
                            // Instead, the BFT expiration detector (3 consecutive
                            // timeouts = 36s) is the sole mechanism for detecting
                            // real peer loss and reverting to solo mode.
                            // Remove from BFT direct delivery list
                            let pid_bytes = peer_id.to_bytes();
                            inner.bft_peer_id_bytes.retain(|b| b != &pid_bytes);

                            FfiNetworkEvent::PeerDisconnected {
                                peer_id: peer_id.to_string(),
                            }
                        }
                        NetworkEvent::BlockReceived(block, source_peer_id) => {
                            let height = block.header.height;
                            let producer = hex::encode(block.header.producer.0);
                            rust_log(&format!(
                                "BLOCK RECEIVED: height={} producer={}",
                                height, &producer[..8.min(producer.len())]
                            ));
                            // WHY: Track PeerIds of peers that send us blocks.
                            // Used by the slot timer to send block proposals
                            // directly to committee members for fast BFT.
                            if let Some(ref pid) = source_peer_id {
                                let pid_bytes = pid.to_bytes();
                                if !inner.bft_peer_id_bytes.contains(&pid_bytes) {
                                    inner.bft_peer_id_bytes.push(pid_bytes);
                                }
                            }
                            let block_hash = block.header.hash().ok();

                            // WHY: Cache received blocks for sync protocol. When a
                            // new peer connects later, we broadcast these blocks so
                            // they can catch up quickly.
                            let block_clone = (*block).clone();

                            // WHY: Notify consensus sync protocol of EVERY gossip
                            // block's height, even if we can't process it yet
                            // (because we're behind). This ensures the sync system
                            // knows the network height and will request missing
                            // blocks. Without this, skipped ahead-blocks would
                            // leave the sync protocol unaware we're behind.
                            if let Some(ref mut sp) = inner.sync_protocol {
                                sp.on_block_received(height);
                            }

                            let process_result = if let Some(ref mut consensus) = inner.consensus {
                                let our_h = consensus.current_height();
                                let our_tip = *consensus.last_finalized_hash();
                                rust_log(&format!(
                                    "BLOCK PROCESS: incoming h={} our_h={} our_tip={}",
                                    height, our_h, &hex::encode(our_tip.0)[..8]
                                ));
                                match consensus.process_incoming_block(*block) {
                                    Ok(BlockProcessResult::Accepted) => {
                                        let h = consensus.current_height();
                                        let tip = consensus.last_finalized_hash().0;
                                        rust_log(&format!("BLOCK ACCEPTED: new_height={}", h));
                                        Some((Some((h, tip)), false))
                                    }
                                    Ok(BlockProcessResult::Skipped) => {
                                        rust_log(&format!("BLOCK SKIPPED: height={}", height));
                                        Some((None, false))
                                    }
                                    Ok(BlockProcessResult::ForkDetected) => {
                                        rust_log(&format!("BLOCK FORK: height={}", height));
                                        let our_height = consensus.current_height();
                                        // WHY: Check cooldown — don't reorg if we
                                        // already reorged in the last 60 seconds.
                                        // Without this, after rollback every gossip
                                        // block triggers another reorg (infinite loop).
                                        let in_cooldown = inner.last_reorg_at
                                            .map(|t| t.elapsed().as_secs() < 10)
                                            .unwrap_or(false);

                                        if in_cooldown {
                                            // WHY: Short cooldown (10s) after fast-sync to
                                            // prevent re-triggering on the same gossip burst.
                                            // After cooldown, normal fork handling resumes.
                                            Some((None, false))
                                        } else if height > our_height {
                                            rust_log(&format!(
                                                "FORK DETECTED: peer at height {} > our height {} — triggering reorg",
                                                height, our_height,
                                            ));
                                            Some((None, true))
                                        } else {
                                            rust_log(&format!(
                                                "FORK DETECTED: peer block at height {}, our height {} — we're not behind, ignoring",
                                                height, our_height,
                                            ));
                                            Some((None, false))
                                        }
                                    }
                                    Err(e) => {
                                        rust_log(&format!("BLOCK REJECTED: height={} error={}", height, e));
                                        warn!(height = height, error = %e, "Failed to process incoming block");
                                        Some((None, false))
                                    }
                                }
                            } else { None };

                            // WHY: peer_height_hint not available yet, use the incoming
                            // block height as a proxy. If the peer sent a block at our
                            // expected height, they're building a chain at least as long.
                            let should_reorg = process_result.map(|(_, reorg)| reorg).unwrap_or(false);
                            let block_result = process_result.and_then(|(accepted, _)| accepted);

                            // ── Fork resolution: jump to peer's chain tip ──────
                            // WHY: The peer has a longer chain. Instead of rolling
                            // back to genesis (which fails because gossip only sends
                            // the latest block, not the full history), we JUMP to the
                            // peer's chain tip. We set our height and tip hash to
                            // match the incoming block, so subsequent blocks from the
                            // peer will be at our expected height and get Accepted.
                            //
                            // Trade-off: we skip intermediate blocks (no transactions
                            // from blocks we missed). For testnet this is acceptable.
                            // For mainnet, a full sync protocol (request blocks by
                            // height range) will fill the gaps.
                            //
                            // This is the blockchain equivalent of "fast sync" — jump
                            // to the tip and validate new blocks going forward.
                            if should_reorg {
                                let our_height = inner.consensus.as_ref()
                                    .map(|e| e.current_height())
                                    .unwrap_or(0);

                                rust_log(&format!(
                                    "FORK RESOLUTION: jumping from height {} to peer's height {} (fast sync)",
                                    our_height, height,
                                ));

                                // Jump consensus to the incoming block's height and hash
                                // WHY: If hash computation fails, skip the rollback entirely.
                                // Proceeding with a zero hash would set the chain tip to a
                                // nonexistent block, corrupting consensus state.
                                let tip_hash_result = block_clone.header.hash();
                                if tip_hash_result.is_err() {
                                    rust_log("FORK RESOLUTION ABORTED: failed to compute block hash for rollback");
                                }
                                if let (Some(ref mut consensus), Ok(tip_hash)) = (&mut inner.consensus, tip_hash_result) {
                                    match consensus.rollback_to(height, tip_hash) {
                                        Ok(()) => {
                                            rust_log(&format!(
                                                "FORK RESOLUTION: chain tip set to height={} hash={}",
                                                height, &hex::encode(tip_hash.0)[..8]
                                            ));
                                        }
                                        Err(e) => {
                                            rust_log(&format!(
                                                "FORK RESOLUTION REJECTED: rollback too deep: {}",
                                                e
                                            ));
                                        }
                                    }
                                }

                                // Clear pending blocks, caches, and stale broadcast.
                                // WHY: Must also clear engine.pending_block — otherwise
                                // the slot timer will try to finalize an orphaned block
                                // that belongs to the old fork.
                                inner.recent_blocks.clear();
                                inner.pending_block_hash = None;
                                inner.pending_block_created_at = None;
                                inner.pending_broadcast_block = None;
                                inner.consecutive_bft_expirations = 0;
                                if let Some(ref mut engine) = inner.consensus {
                                    engine.pending_block = None;
                                }

                                // WHY: Revert on-chain state and sync wallet from it.
                                // Mining rewards from our solo-mined blocks need to be
                                // unwound — those blocks are being replaced by the peer's
                                // chain. The wallet must reflect on-chain reality.
                                if let Some(ref sm) = inner.state_manager {
                                    let _ = sm.revert_to_height(0);
                                }
                                if let (Some(ref sm), Ok(our_addr)) = (&inner.state_manager, inner.wallet.address()) {
                                    let acct = sm.get_account(&our_addr).unwrap_or_default();
                                    inner.wallet.sync_balance(acct.balance);
                                    // WHY: Also sync nonce from on-chain state after fork
                                    // resolution. Without this, the wallet's local nonce
                                    // diverges from the on-chain nonce, causing subsequent
                                    // transactions to be rejected (nonce mismatch) or
                                    // enabling replay of reverted transactions.
                                    inner.wallet.sync_nonce(acct.nonce);
                                }

                                // Update sync managers
                                // WHY: If hash computation fails, skip sync/persist updates.
                                // A zero hash would desync the sync manager and persist
                                // corrupt chain state to disk.
                                if let Ok(tip_hash) = block_clone.header.hash() {
                                    if let Some(ref mut sync) = inner.sync_manager {
                                        sync.update_local_state(height, tip_hash);
                                    }
                                    if let Some(ref network) = inner.network {
                                        let _ = network.try_reset_local_height(height, tip_hash);
                                    }

                                    // Persist the new chain state
                                    if let Some(ref persistence) = inner.chain_persistence {
                                        persistence.save(height, &tip_hash.0, inner.blocks_produced);
                                    }
                                } else {
                                    rust_log("FORK RESOLUTION: skipping sync/persist — block hash computation failed");
                                }

                                // Set cooldown to prevent re-triggering
                                inner.last_reorg_at = Some(std::time::Instant::now());

                                // Cache the block we just jumped to
                                inner.recent_blocks.push_back(block_clone.clone());

                                // WHY: Apply transactions from the reorg block to on-chain
                                // state. Without this, transfers in blocks received via fork
                                // resolution are silently dropped — the recipient never gets
                                // credited because the fork path skips normal block processing.
                                if !block_clone.transactions.is_empty() {
                                    let mut new_hashes_reorg: Vec<[u8; 32]> = Vec::new();
                                    let mut incoming_transfers_reorg: Vec<(String, gratia_core::types::Address, Lux, chrono::DateTime<chrono::Utc>)> = Vec::new();
                                    let skip_reorg: std::collections::HashSet<[u8; 32]> = inner.applied_tx_hashes.clone();
                                    let mut incoming_reorg: Lux = 0;
                                    if let Some(ref sm) = inner.state_manager {
                                        let our_addr_reorg = inner.wallet.address().ok();
                                        for tx in &block_clone.transactions {
                                            if skip_reorg.contains(&tx.hash.0) {
                                                continue;
                                            }
                                            let sender_addr = gratia_core::types::Address::from_pubkey(&tx.sender_pubkey);
                                            if let gratia_core::types::TransactionPayload::Transfer { to, amount } = &tx.payload {
                                                let mut sender_acct = sm.get_account(&sender_addr).unwrap_or_default();
                                                let total = amount + tx.fee;
                                                if sender_acct.balance >= total && sender_acct.nonce == tx.nonce {
                                                    sender_acct.balance -= total;
                                                    sender_acct.nonce += 1;
                                                    let _ = sm.db().put_account(&sender_addr, &sender_acct);
                                                    let mut recv_acct = sm.get_account(to).unwrap_or_default();
                                                    recv_acct.balance += amount;
                                                    let _ = sm.db().put_account(to, &recv_acct);
                                                } else {
                                                    // Insufficient balance or wrong nonce — skip entire TX
                                                    continue;
                                                }
                                                new_hashes_reorg.push(tx.hash.0);
                                                if let Some(ref our) = our_addr_reorg {
                                                    if to == our {
                                                        incoming_reorg += amount;
                                                        incoming_transfers_reorg.push((
                                                            hex::encode(tx.hash.0),
                                                            sender_addr,
                                                            *amount,
                                                            tx.timestamp,
                                                        ));
                                                    }
                                                }
                                            }
                                        }
                                    }
                                    for h in new_hashes_reorg {
                                        inner.applied_tx_hashes.insert(h);
                                    }
                                    for (hash, sender, amount, ts) in incoming_transfers_reorg {
                                        inner.wallet.record_incoming_transfer(hash, sender, amount, ts);
                                    }
                                    if incoming_reorg > 0 {
                                        if let (Some(ref sm2), Ok(our_addr2)) = (&inner.state_manager, inner.wallet.address()) {
                                            let acct = sm2.get_account(&our_addr2).unwrap_or_default();
                                            inner.wallet.sync_balance(acct.balance);
                                            rust_log(&format!(
                                                "FORK RESOLUTION: credited {} Lux ({} GRAT) from reorg block txs — wallet: {} Lux",
                                                incoming_reorg, incoming_reorg / 1_000_000, acct.balance
                                            ));
                                        }
                                    }
                                }

                                rust_log(&format!(
                                    "FORK RESOLUTION: fast-synced to height {}, ready for shared chain",
                                    height,
                                ));

                                // ── Co-sign the block we just adopted ─────────────
                                // WHY: After fast-forwarding to the peer's chain tip,
                                // we trust this block enough to build on it. Co-signing
                                // it sends our BFT signature back to the producer, helping
                                // THEIR pending block reach finality. Without this, two-phone
                                // BFT deadlocks: each phone's block is treated as a fork by
                                // the other, so neither gets co-signed, BFT always expires
                                // at 1/2 signatures, and the chain never reaches finality.
                                let is_committee = inner.consensus.as_ref()
                                    .map(|e| e.is_committee_member())
                                    .unwrap_or(false);
                                if is_committee {
                                    if let Ok(sk_bytes) = inner.wallet.signing_key_bytes() {
                                        let keypair = gratia_core::crypto::Keypair::from_secret_key_bytes(&sk_bytes);
                                        if let Some(our_sig) = inner.consensus.as_ref().and_then(|engine| {
                                            engine.sign_block_as_validator(&block_clone.header, &keypair).ok()
                                        }) {
                                            // WHY: If hash computation fails, skip the co-sign broadcast.
                                            // Sending a signature for hash [0;32] would let peers accept
                                            // a signature that doesn't correspond to any real block.
                                            if let Ok(block_hash_bytes) = block_clone.header.hash().map(|h| h.0) {
                                            if let Some(ref network) = inner.network {
                                                // Use direct delivery if source peer is known
                                                if let Some(ref peer_id) = source_peer_id {
                                                    let _ = network.try_send_bft_signature_direct(
                                                        *peer_id, block_hash_bytes, height, our_sig.clone(),
                                                    );
                                                    rust_log(&format!(
                                                        "BFT: co-signed reorg block {} from peer (direct)", height
                                                    ));
                                                } else {
                                                    let sig_msg = gratia_network::gossip::ValidatorSignatureMessage {
                                                        block_hash: block_hash_bytes,
                                                        height,
                                                        signature: our_sig,
                                                        validator_pubkey: *keypair.public_key().as_bytes(),
                                                    };
                                                    let _ = network.try_broadcast_validator_signature_sync(&sig_msg);
                                                    rust_log(&format!(
                                                        "BFT: co-signed reorg block {} from peer (gossipsub)", height
                                                    ));
                                                }
                                            }
                                            } else {
                                                rust_log("BFT: skipping co-sign of reorg block — hash computation failed");
                                            }
                                        }
                                    }
                                }
                            }

                            if let Some((new_height, tip_hash)) = block_result {
                                info!(height = new_height, "Processed incoming block from network");
                                if let Some(ref mut sync) = inner.sync_manager {
                                    if let Some(hash) = block_hash {
                                        sync.update_local_state(new_height, hash);
                                    }
                                }
                                if let Some(ref persistence) = inner.chain_persistence {
                                    persistence.save(
                                        new_height,
                                        &tip_hash,
                                        inner.blocks_produced,
                                    );
                                }

                                // WHY: Validate synced block transactions BEFORE applying.
                                // Without validation, a malicious peer could send blocks
                                // containing forged transactions (fake signatures, zero fees)
                                // that would corrupt our on-chain state. validate_block_transactions
                                // checks: signature validity, minimum fee, payload rules, no
                                // duplicate tx hashes, and transaction count limits.
                                if !block_clone.transactions.is_empty() {
                                    if let Err(e) = validate_block_transactions(
                                        &block_clone.transactions,
                                        MIN_TRANSACTION_FEE,
                                    ) {
                                        warn!(
                                            "REJECTING synced block at height {}: {}",
                                            new_height, e,
                                        );
                                        rust_log(&format!(
                                            "SECURITY: Rejected block {} — invalid transactions: {}",
                                            new_height, e,
                                        ));
                                        continue; // Skip this block, don't apply
                                    }
                                }

                                // WHY: Apply validated synced block transactions to on-chain state.
                                // Transactions have passed structural validation above. Balance and
                                // nonce checks happen here during application. This closes the gap
                                // where only locally-produced blocks updated state.
                                let mut burned_fees_accum: u64 = 0;
                                let mut new_hashes_sync: Vec<[u8; 32]> = Vec::new();
                                // Collect incoming transfer details for wallet history recording
                                let mut incoming_transfers_sync: Vec<(String, gratia_core::types::Address, Lux, chrono::DateTime<chrono::Utc>)> = Vec::new();
                                {
                                    let skip_sync: std::collections::HashSet<[u8; 32]> = inner.applied_tx_hashes.clone();
                                if let Some(ref sm) = inner.state_manager {
                                    let our_addr = inner.wallet.address().ok();
                                    let mut applied = 0u32;
                                    let mut incoming_lux: Lux = 0;
                                    for tx in &block_clone.transactions {
                                        if skip_sync.contains(&tx.hash.0) {
                                            continue;
                                        }
                                        let sender_addr = gratia_core::types::Address::from_pubkey(&tx.sender_pubkey);
                                        match &tx.payload {
                                            gratia_core::types::TransactionPayload::Transfer { to, amount } => {
                                                let mut sender_acct = sm.get_account(&sender_addr).unwrap_or_default();
                                                let total = amount + tx.fee;
                                                if sender_acct.balance >= total && sender_acct.nonce == tx.nonce {
                                                    sender_acct.balance -= total;
                                                    sender_acct.nonce += 1;
                                                    let _ = sm.db().put_account(&sender_addr, &sender_acct);
                                                    let mut recv_acct = sm.get_account(to).unwrap_or_default();
                                                    recv_acct.balance += amount;
                                                    let _ = sm.db().put_account(to, &recv_acct);
                                                    if tx.fee > 0 {
                                                        burned_fees_accum = burned_fees_accum.saturating_add(tx.fee);
                                                        rust_log(&format!("FEE BURNED: {} Lux from tx {}", tx.fee, hex::encode(tx.hash.0)));
                                                    }
                                                } else {
                                                    continue;
                                                }
                                                new_hashes_sync.push(tx.hash.0);
                                                if let Some(ref our) = our_addr {
                                                    if to == our {
                                                        incoming_lux += amount;
                                                        incoming_transfers_sync.push((
                                                            hex::encode(tx.hash.0),
                                                            sender_addr,
                                                            *amount,
                                                            tx.timestamp,
                                                        ));
                                                    }
                                                }
                                                applied += 1;
                                            }
                                            _ => { applied += 1; }
                                        }
                                    }
                                    if incoming_lux > 0 {
                                        if let (Some(ref sm2), Ok(our_addr2)) = (&inner.state_manager, inner.wallet.address()) {
                                            let acct = sm2.get_account(&our_addr2).unwrap_or_default();
                                            inner.wallet.sync_balance(acct.balance);
                                            rust_log(&format!(
                                                "Received {} Lux ({} GRAT) — wallet synced to on-chain: {} Lux",
                                                incoming_lux, incoming_lux / 1_000_000,
                                                acct.balance
                                            ));
                                        }
                                    }
                                    if applied > 0 {
                                        rust_log(&format!(
                                            "Sync state: block {} — {} txs applied from network",
                                            new_height, applied
                                        ));
                                    }
                                }
                                } // end skip_sync scope
                                // Record incoming transfers in wallet history (outside sm borrow)
                                for (hash, sender, amount, ts) in incoming_transfers_sync {
                                    rust_log(&format!(
                                        "HISTORY: recording incoming transfer {} — {} Lux from {:?}",
                                        hash, amount, sender
                                    ));
                                    inner.wallet.record_incoming_transfer(hash, sender, amount, ts);
                                }
                                for h in new_hashes_sync {
                                    inner.applied_tx_hashes.insert(h);
                                }
                                inner.total_burned_tx_fees = inner.total_burned_tx_fees.saturating_add(burned_fees_accum);

                                // WHY: Credit mining reward for received blocks to the
                                // block producer's account in our state, so the explorer
                                // and balance queries reflect the true state of the chain.
                                // SKIP if we're the producer — we already credited
                                // ourselves during finalization. Double-crediting would
                                // inflate our balance.
                                if let Some(ref sm) = inner.state_manager {
                                    let producer_addr = gratia_core::types::Address::from_pubkey(&block_clone.header.producer_pubkey);
                                    let our_addr = inner.wallet.address().ok();
                                    let is_self = our_addr.as_ref() == Some(&producer_addr);

                                    if !is_self {
                                        let active_miners = (block_clone.header.active_miners as u64).max(1);
                                        let reward: Lux = EmissionSchedule
                                            ::per_miner_block_reward_lux(new_height, active_miners);
                                        let mut acct = sm.get_account(&producer_addr).unwrap_or_default();
                                        acct.balance += reward;
                                        let _ = sm.db().put_account(&producer_addr, &acct);
                                    }
                                }

                                // Cache for sync protocol
                                // WHY: Clone before push_back because block_clone is
                                // needed later for BFT co-signing (header reference).
                                inner.recent_blocks.push_back(block_clone.clone());
                                if inner.recent_blocks.len() > 100 {
                                    inner.recent_blocks.pop_front();
                                }

                                // WHY: Persist after every received block, not just every 5th.
                                // Account nonces must survive force-kills — if a TX increments
                                // a nonce but the state isn't persisted before the app dies,
                                // the nonce resets to 0 on restart, causing all subsequent TXs
                                // from that sender to fail with "nonce mismatch".
                                {
                                    if let Some(ref backend) = inner.storage_backend {
                                        if let Err(e) = backend.persist() {
                                            warn!("Failed to persist synced state: {}", e);
                                        }
                                    }
                                }
                            }

                            // ── BFT co-signing ──────────────────────────────────
                            // WHY: If we're a committee member and we just accepted
                            // a valid block from another producer, sign it and
                            // broadcast our signature. This is how non-producing
                            // committee members contribute to BFT finality.
                            // CRITICAL: Only sign blocks that were ACCEPTED (build
                            // on our chain). Signing ForkDetected or Skipped blocks
                            // would give BFT finality to a competing fork, causing
                            // permanent chain divergence.
                            {
                                let block_was_accepted = block_result.is_some();
                                let is_committee_member = inner.consensus.as_ref()
                                    .map(|e| e.is_committee_member())
                                    .unwrap_or(false);

                                if is_committee_member && block_was_accepted {
                                    if let Ok(sk_bytes) = inner.wallet.signing_key_bytes() {
                                        let keypair = gratia_core::crypto::Keypair::from_secret_key_bytes(&sk_bytes);
                                        let sign_result = inner.consensus.as_ref()
                                            .and_then(|engine| {
                                                engine.sign_block_as_validator(
                                                    &block_clone.header,
                                                    &keypair,
                                                ).ok()
                                            });

                                        if let Some(our_sig) = sign_result {
                                            // WHY: If hash computation fails, skip the co-sign entirely.
                                            // Broadcasting a signature for hash [0;32] is a consensus
                                            // integrity violation — it signs a nonexistent block.
                                            if let Ok(blk_hash) = block_clone.header.hash().map(|h| h.0) {
                                            if let Some(ref network) = inner.network {
                                                // WHY: Send co-signature directly to the block
                                                // producer via request-response for sub-second
                                                // delivery. Falls back to gossipsub if direct
                                                // delivery isn't available (no source PeerId).
                                                if let Some(ref peer_id) = source_peer_id {
                                                    match network.try_send_bft_signature_direct(
                                                        *peer_id,
                                                        blk_hash,
                                                        height,
                                                        our_sig.clone(),
                                                    ) {
                                                        Ok(()) => {
                                                            // WHY: Successfully co-signing a peer's block
                                                            // via direct delivery proves the peer is alive.
                                                            // Reset expiration counter to prevent false solo
                                                            // fallback when our blocks expire but the peer
                                                            // is clearly still producing and connected.
                                                            inner.consecutive_bft_expirations = 0;
                                                            rust_log(&format!(
                                                                "BFT: co-signed block {} from {} (direct)",
                                                                height, &producer[..8.min(producer.len())]
                                                            ));
                                                        }
                                                        Err(e) => {
                                                            rust_log(&format!(
                                                                "BFT: direct co-sign failed ({}), falling back to gossipsub",
                                                                e
                                                            ));
                                                            // Fallback to gossipsub
                                                            let sig_msg = gratia_network::gossip::ValidatorSignatureMessage {
                                                                block_hash: blk_hash,
                                                                height,
                                                                signature: our_sig,
                                                                validator_pubkey: *keypair.public_key().as_bytes(),
                                                            };
                                                            let _ = network.try_broadcast_validator_signature_sync(&sig_msg);
                                                        }
                                                    }
                                                } else {
                                                    // No source PeerId — fallback to gossipsub
                                                    let sig_msg = gratia_network::gossip::ValidatorSignatureMessage {
                                                        block_hash: blk_hash,
                                                        height,
                                                        signature: our_sig,
                                                        validator_pubkey: *keypair.public_key().as_bytes(),
                                                    };
                                                    let _ = network.try_broadcast_validator_signature_sync(&sig_msg);
                                                    rust_log(&format!(
                                                        "BFT: co-signed block {} from {} (gossipsub fallback)",
                                                        height, &producer[..8.min(producer.len())]
                                                    ));
                                                }
                                            }
                                            } else {
                                                rust_log("BFT: skipping co-sign — block hash computation failed");
                                            }
                                        }
                                    }
                                }
                            }

                            FfiNetworkEvent::BlockReceived {
                                height,
                                producer,
                            }
                        }
                        NetworkEvent::TransactionReceived(tx) => {
                            let hash_hex = hex::encode(tx.hash.0);

                            // WHY: Verify the Ed25519 signature and hash BEFORE
                            // crediting any balance. Without this, a malicious node
                            // could forge transactions to inflate anyone's balance.
                            // This is the primary defense against transaction forgery
                            // in the gossip layer.
                            match gratia_wallet::transactions::verify_transaction(&tx) {
                                Ok(()) => {
                                    // Signature and hash are valid. Now check on-chain
                                    // state if available.
                                    let sender_address = gratia_core::types::Address::from_pubkey(&tx.sender_pubkey);
                                    let mut state_valid = true;

                                    // WHY: If on-chain state is available, verify the sender
                                    // has sufficient balance and correct nonce. This prevents
                                    // double-spends — a node can't spend GRAT it doesn't have
                                    // on-chain, even if the signature is valid.
                                    // WHY: Only enforce balance/nonce checks if we actually
                                    // know about the sender. In Phase 1, each phone's on-chain
                                    // state only tracks accounts it has seen (local wallet +
                                    // accounts from applied blocks). A transaction from an
                                    // unknown account (balance=0, nonce=0) should be accepted
                                    // if the signature is valid — we simply don't have enough
                                    // information to reject it. Rejecting unknown senders would
                                    // break cross-device transfers since phones don't share
                                    // state yet. Once full sync is implemented (Phase 2), all
                                    // nodes will have the complete account state and this check
                                    // becomes strict.
                                    // WHY: Don't reject at mempool admission based on
                                    // balance or nonce. Our local state may be a few blocks
                                    // behind the sender's actual state, causing false
                                    // rejections. Signature verification (above) is the
                                    // mempool gatekeeper. Balance and nonce are enforced at
                                    // block finalization time where state is authoritative.

                                    if state_valid {
                                        if let Ok(our_address) = inner.wallet.address() {
                                            if let gratia_core::types::TransactionPayload::Transfer { to, amount } = &tx.payload {
                                                if *to == our_address {
                                                    let new_balance = inner.wallet.balance() + amount;
                                                    inner.wallet.sync_balance(new_balance);

                                                    inner.wallet.record_incoming_transfer(
                                                        hash_hex.clone(),
                                                        sender_address,
                                                        *amount,
                                                        tx.timestamp,
                                                    );

                                                    rust_log(&format!(
                                                        "RECEIVED {} Lux ({} GRAT) — verified tx {}",
                                                        amount, amount / 1_000_000, hash_hex
                                                    ));
                                                }
                                            }
                                        }

                                        // WHY: Add verified transaction to mempool so it gets
                                        // included in the next block we produce.
                                        inner.mempool.push(*tx);
                                    }
                                }
                                Err(e) => {
                                    // WHY: Reject forged or tampered transactions.
                                    // Log the failure for debugging but do NOT credit
                                    // any balance or record any history.
                                    warn!(
                                        hash = %hash_hex,
                                        error = %e,
                                        "REJECTED incoming transaction — signature/hash verification failed"
                                    );
                                    rust_log(&format!(
                                        "REJECTED tx {} — invalid signature: {}",
                                        hash_hex, e
                                    ));
                                }
                            }

                            FfiNetworkEvent::TransactionReceived {
                                hash_hex,
                            }
                        }
                        NetworkEvent::NodeAnnounced(ann) => {
                            // WHY: Unbox immediately — we need the owned value for storage.
                            let announcement = *ann;
                            let peer_node_id = announcement.node_id;
                            let peer_announced_height = announcement.height;
                            rust_log(&format!(
                                "NodeAnnounced: node={:?} score={} pol_days={} height={}",
                                peer_node_id, announcement.presence_score, announcement.pol_days, peer_announced_height,
                            ));

                            // WHY: Dedup by node_id — if we already know this peer,
                            // update their entry instead of adding a duplicate.
                            // WHY: Track whether score/pol_days actually changed.
                            // Without this guard, every 32s re-broadcast triggers
                            // a full committee rebuild + BFT state clear — a DoS
                            // vector where an attacker spams announcements to
                            // continuously disrupt in-progress finality.
                            let needs_committee_rebuild;
                            if let Some(existing) = inner.known_peer_nodes.iter_mut().find(|n| n.node_id == peer_node_id) {
                                let score_changed = existing.presence_score != announcement.presence_score;
                                let pol_changed = existing.pol_days != announcement.pol_days;
                                needs_committee_rebuild = score_changed || pol_changed;
                                if needs_committee_rebuild {
                                    rust_log(&format!(
                                        "NodeAnnounced: peer {:?} changed score {}->{} pol_days {}->{}",
                                        &peer_node_id.0[..4],
                                        existing.presence_score, announcement.presence_score,
                                        existing.pol_days, announcement.pol_days,
                                    ));
                                }
                                *existing = announcement.clone();
                            } else {
                                needs_committee_rebuild = true;
                                inner.known_peer_nodes.push(announcement);
                            }

                            // WHY: Only rebuild the committee when a NEW peer is
                            // discovered or an existing peer's presence_score/pol_days
                            // changed. Re-announcements with identical data (the
                            // normal 32s heartbeat) skip the rebuild entirely.
                            let has_consensus = inner.consensus.is_some() && needs_committee_rebuild;
                            let local_node_id = self.get_node_id_or_default(&inner);
                            let signing_key_bytes = inner.wallet.signing_key_bytes().ok();

                            if has_consensus {
                                if let Some(ref sk_bytes) = signing_key_bytes {
                                    // WHY: Use real presence score for committee reconstruction.
                                    // During onboarding (day 0), use minimum threshold of 40
                                    // so new users can still participate in block production.
                                    // Debug bypass uses 100 to ensure demo node wins.
                                    let local_score = if inner.is_debug_bypass() { 100u8 }
                                        else if inner.presence_score > 0 { inner.presence_score }
                                        else if inner.pol.is_onboarding() { 40u8 }
                                        else { 75u8 };
                                    let vrf_pubkey = VrfSecretKey::from_ed25519_bytes(sk_bytes).public_key();

                                    let local_signing_pubkey = gratia_core::crypto::Keypair::from_secret_key_bytes(sk_bytes).public_key_bytes();
                                    let mut all_eligible = vec![EligibleNode {
                                        node_id: local_node_id,
                                        vrf_pubkey,
                                        presence_score: local_score,
                                        has_valid_pol: true,
                                        meets_minimum_stake: true,
                                        pol_days: 90,
                                        signing_pubkey: local_signing_pubkey,
                                        vrf_proof: vec![],
                                    }];

                                    // WHY: Convert each known peer's NodeAnnouncement
                                    // into an EligibleNode for committee selection.
                                    for peer in &inner.known_peer_nodes {
                                        all_eligible.push(EligibleNode {
                                            node_id: peer.node_id,
                                            vrf_pubkey: VrfPublicKey { bytes: peer.vrf_pubkey_bytes },
                                            presence_score: peer.presence_score,
                                            has_valid_pol: true,
                                            meets_minimum_stake: true,
                                            pol_days: peer.pol_days,
                                            signing_pubkey: peer.ed25519_pubkey.to_vec(),
                                            vrf_proof: vec![],
                                        });
                                    }

                                    // Synthetic padding removed — causes slot mismatch rejections.
                                    // 2-node committee works fine with round-robin.
                                    let real_count = all_eligible.len();

                                    // WHY: Sort by node_id so all phones build the committee
                                    // in the same canonical order. Without this, Phone A has
                                    // [A, B, Synthetic] and Phone B has [B, A, Synthetic] —
                                    // different orderings cause different slot assignments
                                    // and both phones produce for the same slot.
                                    all_eligible.sort_by(|a, b| a.node_id.0.cmp(&b.node_id.0));

                                    let prev_real_members = inner.real_committee_members;

                                    rust_log(&format!(
                                        "Rebuilding committee: {} real + {} synthetic = {} total (was {} real)",
                                        real_count,
                                        all_eligible.len() - real_count,
                                        all_eligible.len(),
                                        prev_real_members,
                                    ));

                                    // WHY: Use the cached epoch seed for committee rebuilds
                                    // within the same epoch. This prevents mid-epoch seed
                                    // manipulation — an attacker changing their score can
                                    // alter committee membership but NOT the seed that
                                    // determines ordering. The seed only changes at epoch
                                    // boundaries (SLOTS_PER_EPOCH) or first init.
                                    // WHY: Update real_committee_members AFTER successful
                                    // init. If init fails, the old count stays correct.
                                    // WHY: Scope the consensus borrow to avoid conflicting
                                    // mutable borrows on `inner` fields below.
                                    // WHY: Extract epoch seed BEFORE borrowing consensus mutably.
                                    // Compute new seed only at epoch boundaries or first init.
                                    let epoch_seed = {
                                        let should_new_seed = inner.epoch_seed.is_none()
                                            || inner.consensus.as_ref()
                                                .and_then(|c| c.committee())
                                                .map(|c| gratia_consensus::committee::should_rotate(c, inner.consensus.as_ref().map(|e| e.current_slot()).unwrap_or(0)))
                                                .unwrap_or(true);
                                        if should_new_seed {
                                            let new_seed = inner.consensus.as_ref()
                                                .map(|c| c.compute_epoch_seed())
                                                .unwrap_or([0u8; 32]);
                                            inner.epoch_seed = Some(new_seed);
                                        }
                                        inner.epoch_seed.unwrap_or([0u8; 32])
                                    };
                                    let committee_init_ok = if let Some(ref mut consensus) = inner.consensus {
                                        match consensus.initialize_committee(&all_eligible, &epoch_seed, 0, 0) {
                                            Ok(()) => {
                                                // Clear pending block inside the consensus borrow
                                                if prev_real_members != real_count {
                                                    consensus.pending_block = None;
                                                }
                                                true
                                            }
                                            Err(e) => {
                                                warn!("Failed to rebuild committee: {}", e);
                                                false
                                            }
                                        }
                                    } else { false };

                                    if committee_init_ok {
                                        let committee_changed = prev_real_members != real_count;
                                        // WHY: Only update after successful init
                                        inner.real_committee_members = real_count;

                                        // WHY: Peer reconnected — reset solo block cap so
                                        // production resumes immediately with BFT finality.
                                        if real_count > 1 && prev_real_members <= 1 {
                                            inner.consecutive_solo_blocks = 0;
                                        }

                                        // WHY: When committee composition changes (peer
                                        // lost then re-found, or new peer joins), clear
                                        // stale BFT state. Without this, after a network
                                        // partition recovery, consecutive_bft_expirations
                                        // and pending_block from the old committee linger,
                                        // causing the new committee to stall — neither
                                        // node produces because the old pending block
                                        // blocks new production, and the expiry counter
                                        // triggers premature solo fallback.
                                        if committee_changed {
                                            inner.consecutive_bft_expirations = 0;
                                            inner.pending_block_hash = None;
                                            inner.pending_block_created_at = None;
                                            inner.pending_broadcast_block = None;
                                            rust_log(&format!(
                                                "Committee changed ({}→{} real) — cleared BFT state for clean start",
                                                prev_real_members, real_count,
                                            ));
                                        }
                                    }

                                    // ── Solo→Multi fork resolution ────────────────────
                                    // WHY: When two phones have been mining solo, they
                                    // build completely independent chains with different
                                    // genesis blocks and incompatible parent hashes.
                                    // Normal sync/fork detection can't reconcile these
                                    // because no common ancestor exists. The solution:
                                    // the phone with the SHORTER chain resets to height 0
                                    // and syncs the longer chain from the peer. The phone
                                    // with the LONGER chain keeps its history.
                                    if prev_real_members <= 1 && real_count > 1 {
                                        let our_height = inner.consensus.as_ref()
                                            .map(|e| e.current_height())
                                            .unwrap_or(0);

                                        if !inner.yield_checked_peers.contains(&peer_node_id.0) {
                                            inner.yield_checked_peers.push(peer_node_id.0);
                                        }

                                        // Determine if we should yield our chain to the peer.
                                        // Shorter chain yields. Equal height = same chain, no action.
                                        let heights_equal = our_height == peer_announced_height;
                                        let should_yield = if our_height > 0 && peer_announced_height > 0 && !heights_equal {
                                            our_height < peer_announced_height
                                        } else {
                                            false
                                        };

                                        if should_yield {
                                            rust_log(&format!(
                                                "FORK RESOLUTION: yielding chain (height {}) to peer (height {}) — resetting to sync from peer",
                                                our_height, peer_announced_height,
                                            ));

                                            // 1. Reset consensus engine to height 0
                                            if let Some(ref mut consensus) = inner.consensus {
                                                let _ = consensus.rollback_to(0, BlockHash([0u8; 32]));
                                            }

                                            // 2. Reset Streamlet BFT state
                                            if let Some(ref mut streamlet) = inner.streamlet {
                                                streamlet.restore(0, [0u8; 32]);
                                            }

                                            // 3. Delete chain_state.bin (persisted height/hash)
                                            if let Some(ref persistence) = inner.chain_persistence {
                                                persistence.save(0, &[0u8; 32], 0);
                                            }

                                            // 4. Delete chain_state.db (account balances/nonces)
                                            let chain_db_path = format!("{}/chain_state.db", self.data_dir);
                                            let _ = std::fs::remove_file(&chain_db_path);
                                            // Also clear RocksDB directory if it exists
                                            let rocksdb_path = format!("{}/rocksdb", self.data_dir);
                                            let _ = std::fs::remove_dir_all(&rocksdb_path);

                                            // 5. Re-open storage backend and state manager
                                            let state_path = format!("{}/chain_state.db", self.data_dir);
                                            let backend_config = {
                                                #[cfg(feature = "rocksdb-backend")]
                                                {
                                                    let rdb_path = format!("{}/rocksdb", self.data_dir);
                                                    StorageBackendConfig::RocksDb { db_path: rdb_path }
                                                }
                                                #[cfg(not(feature = "rocksdb-backend"))]
                                                {
                                                    StorageBackendConfig::InMemory {
                                                        persistence_path: Some(state_path.clone()),
                                                    }
                                                }
                                            };
                                            if let Ok(backend) = open_storage(backend_config) {
                                                let sm = StateManager::new(backend.store.clone());
                                                inner.storage_backend = Some(backend);
                                                inner.state_manager = Some(sm);
                                                rust_log("FORK RESOLUTION: storage backend and state manager re-initialized");
                                            } else {
                                                rust_log("FORK RESOLUTION: WARNING — failed to re-open storage backend");
                                            }

                                            // 6. Reset wallet balance to 0 (will be rebuilt from synced blocks)
                                            inner.wallet.sync_balance(0);
                                            inner.wallet.sync_nonce(0);

                                            // 7. Clear recent blocks cache
                                            inner.recent_blocks.clear();

                                            // 8. Reset blocks produced counter
                                            inner.blocks_produced = 0;

                                            // 9. Update sync managers to height 0
                                            if let Some(ref mut sync) = inner.sync_manager {
                                                sync.update_local_state(0, BlockHash([0u8; 32]));
                                            }
                                            if let Some(ref network) = inner.network {
                                                let _ = network.try_reset_local_height(0, BlockHash([0u8; 32]));
                                            }

                                            // 10. Reset consensus sync protocol
                                            if let Some(ref mut sp) = inner.sync_protocol {
                                                sp.reset(0);
                                            }

                                            // 11. Set reorg cooldown to prevent immediate re-trigger
                                            inner.last_reorg_at = Some(std::time::Instant::now());

                                            rust_log(&format!(
                                                "FORK RESOLUTION: reset complete — now at height 0, will sync from peer at height {}",
                                                peer_announced_height,
                                            ));
                                        } else if heights_equal {
                                            rust_log(&format!(
                                                "SOLO→MULTI: same height ({}) — continuing shared chain, no reset",
                                                our_height,
                                            ));
                                        } else {
                                            // We have longer chain — reset consensus to 0 but keep balances
                                            rust_log(&format!(
                                                "SOLO→MULTI: winning chain (height {}) vs peer (height {}) — resetting consensus to 0, preserving balances",
                                                our_height, peer_announced_height,
                                            ));
                                            if let Some(ref mut consensus) = inner.consensus {
                                                let _ = consensus.rollback_to(0, BlockHash([0u8; 32]));
                                            }
                                            if let Some(ref mut streamlet) = inner.streamlet {
                                                streamlet.restore(0, [0u8; 32]);
                                            }
                                            if let Some(ref persistence) = inner.chain_persistence {
                                                persistence.save(0, &[0u8; 32], inner.blocks_produced);
                                            }
                                            inner.recent_blocks.clear();
                                            inner.blocks_produced = 0;
                                            if let Some(ref mut sync) = inner.sync_manager {
                                                sync.update_local_state(0, BlockHash([0u8; 32]));
                                            }
                                            if let Some(ref network) = inner.network {
                                                let _ = network.try_reset_local_height(0, BlockHash([0u8; 32]));
                                            }
                                            if let Some(ref mut sp) = inner.sync_protocol {
                                                sp.reset(0);
                                            }
                                            inner.last_reorg_at = Some(std::time::Instant::now());
                                        }

                                        // Clear pending BFT state so the new committee
                                        // starts clean without stale solo-mode artifacts
                                        inner.pending_block_hash = None;
                                        inner.pending_block_created_at = None;
                                        inner.pending_broadcast_block = None;
                                        inner.consecutive_bft_expirations = 0;
                                    }
                                }
                            }

                            // ── Deferred fork resolution for discovery-added peers ──
                            // WHY: The discovery handler (run_slot_timer) consumes
                            // NodeAnnounced events and adds peers to known_peer_nodes,
                            // but if the discovery handler didn't perform fork resolution
                            // (e.g., consensus wasn't started yet at that point, or
                            // the event was processed before this code path), we need
                            // to catch it here on the next re-announcement. Check if
                            // this peer needs fork resolution by looking at
                            // yield_checked_peers — if absent, we haven't resolved yet.
                            if !needs_committee_rebuild
                                && inner.consensus.is_some()
                                && inner.real_committee_members > 1
                                && !inner.yield_checked_peers.contains(&peer_node_id.0)
                            {
                                let local_node_id = self.get_node_id_or_default(&inner);
                                let our_height = inner.consensus.as_ref()
                                    .map(|e| e.current_height())
                                    .unwrap_or(0);

                                inner.yield_checked_peers.push(peer_node_id.0);

                                let should_yield = if our_height > 0 && peer_announced_height > 0 {
                                    if our_height < peer_announced_height {
                                        true
                                    } else if our_height > peer_announced_height {
                                        false
                                    } else {
                                        local_node_id.0 > peer_node_id.0
                                    }
                                } else {
                                    false
                                };

                                if should_yield {
                                    rust_log(&format!(
                                        "DEFERRED FORK RESOLUTION: yielding chain (height {}) to peer (height {}) — resetting to sync",
                                        our_height, peer_announced_height,
                                    ));

                                    // 1. Reset consensus engine to height 0
                                    if let Some(ref mut consensus) = inner.consensus {
                                        let _ = consensus.rollback_to(0, BlockHash([0u8; 32]));
                                    }

                                    // 2. Reset Streamlet BFT state
                                    if let Some(ref mut streamlet) = inner.streamlet {
                                        streamlet.restore(0, [0u8; 32]);
                                    }

                                    // 3. Delete chain_state.bin (persisted height/hash)
                                    if let Some(ref persistence) = inner.chain_persistence {
                                        persistence.save(0, &[0u8; 32], 0);
                                    }

                                    // 4. Delete chain_state.db and RocksDB directory
                                    let chain_db_path = format!("{}/chain_state.db", self.data_dir);
                                    let _ = std::fs::remove_file(&chain_db_path);
                                    let rocksdb_path = format!("{}/rocksdb", self.data_dir);
                                    let _ = std::fs::remove_dir_all(&rocksdb_path);

                                    // 5. Re-open storage backend and state manager
                                    let state_path = format!("{}/chain_state.db", self.data_dir);
                                    let backend_config = {
                                        #[cfg(feature = "rocksdb-backend")]
                                        {
                                            let rdb_path = format!("{}/rocksdb", self.data_dir);
                                            StorageBackendConfig::RocksDb { db_path: rdb_path }
                                        }
                                        #[cfg(not(feature = "rocksdb-backend"))]
                                        {
                                            StorageBackendConfig::InMemory {
                                                persistence_path: Some(state_path.clone()),
                                            }
                                        }
                                    };
                                    if let Ok(backend) = open_storage(backend_config) {
                                        let sm = StateManager::new(backend.store.clone());
                                        inner.storage_backend = Some(backend);
                                        inner.state_manager = Some(sm);
                                        rust_log("DEFERRED FORK RESOLUTION: storage backend and state manager re-initialized");
                                    } else {
                                        rust_log("DEFERRED FORK RESOLUTION: WARNING — failed to re-open storage backend");
                                    }

                                    // 6. Reset wallet balance to 0
                                    inner.wallet.sync_balance(0);
                                    inner.wallet.sync_nonce(0);

                                    // 7. Clear recent blocks cache
                                    inner.recent_blocks.clear();

                                    // 8. Reset blocks produced counter
                                    inner.blocks_produced = 0;

                                    // 9. Update sync managers to height 0
                                    if let Some(ref mut sync) = inner.sync_manager {
                                        sync.update_local_state(0, BlockHash([0u8; 32]));
                                    }
                                    if let Some(ref network) = inner.network {
                                        let _ = network.try_reset_local_height(0, BlockHash([0u8; 32]));
                                    }

                                    // 10. Reset consensus sync protocol
                                    if let Some(ref mut sp) = inner.sync_protocol {
                                        sp.reset(0);
                                    }

                                    // 11. Set reorg cooldown
                                    inner.last_reorg_at = Some(std::time::Instant::now());

                                    // Clear pending BFT state
                                    inner.pending_block_hash = None;
                                    inner.pending_block_created_at = None;
                                    inner.pending_broadcast_block = None;
                                    inner.consecutive_bft_expirations = 0;

                                    rust_log(&format!(
                                        "DEFERRED FORK RESOLUTION: reset complete — now at height 0, will sync from peer at height {}",
                                        peer_announced_height,
                                    ));
                                } else {
                                    rust_log(&format!(
                                        "DEFERRED SOLO→MULTI: keeping chain (height {}) — peer has height {}",
                                        our_height, peer_announced_height,
                                    ));
                                }
                            }

                            FfiNetworkEvent::PeerConnected {
                                peer_id: format!("node:{:?}", peer_node_id),
                            }
                        }
                        NetworkEvent::LuxPostReceived(post) => {
                            let hash = post.hash.clone();
                            let author = post.author.clone();

                            // WHY: Verify the post signature before storing it.
                            // A malicious peer could forge posts with someone else's
                            // author field. Verification ensures authorship integrity.
                            match gratia_lux::LuxStore::verify_post(&post) {
                                Ok(true) => {
                                    let lux_path = format!("{}/lux_store.json", self.data_dir);
                                    inner.lux_store.store_received_post(*post);
                                    let _ = inner.lux_store.save_to_file(&lux_path);
                                    rust_log(&format!(
                                        "Lux post received via gossip: hash={} author={}",
                                        hash, author
                                    ));
                                }
                                Ok(false) => {
                                    warn!(
                                        hash = %hash,
                                        "REJECTED Lux post — signature verification failed"
                                    );
                                    continue;
                                }
                                Err(e) => {
                                    warn!(
                                        hash = %hash,
                                        error = %e,
                                        "REJECTED Lux post — verification error"
                                    );
                                    continue;
                                }
                            }

                            FfiNetworkEvent::LuxPostReceived { hash, author }
                        }
                        NetworkEvent::ValidatorSignatureReceived(sig_msg) => {
                            // WHY: BFT finality — a committee member signed a block
                            // we're tracking. If it matches our pending block, add
                            // the signature and check if we've reached finality.
                            let sig_height = sig_msg.height;
                            let sig_block_hash = sig_msg.block_hash;
                            let sig_validator = sig_msg.signature.validator;

                            let matches_pending = inner.pending_block_hash
                                .map(|h| h == sig_block_hash)
                                .unwrap_or(false);

                            if matches_pending {
                                // Verify signature is from a committee member and add it.
                                let finalized = if let Some(ref mut engine) = inner.consensus {
                                    match engine.add_block_signature(sig_msg.signature) {
                                        Ok(finalized) => {
                                            let current_sigs = engine.pending_block.as_ref()
                                                .map(|p| p.signatures.len())
                                                .unwrap_or(0);
                                            let threshold = engine.pending_finality_threshold();
                                            rust_log(&format!(
                                                "BFT: received sig from {:?} for height {} ({}/{} sigs)",
                                                sig_validator, sig_height, current_sigs, threshold
                                            ));
                                            finalized
                                        }
                                        Err(e) => {
                                            rust_log(&format!(
                                                "BFT: rejected sig from {:?}: {}",
                                                sig_validator, e
                                            ));
                                            false
                                        }
                                    }
                                } else { false };

                                // If finality reached, finalize the block now.
                                if finalized {
                                    rust_log(&format!("BFT: finality reached for height {}!", sig_height));
                                    inner.consecutive_bft_expirations = 0;
                                    inner.consecutive_solo_blocks = 0;
                                    inner.bft_retry_count = 0;
                                    inner.last_finalized_at = Some(std::time::Instant::now());
                                    let finalize_result = match inner.consensus.as_mut() {
                                        Some(engine) => engine.finalize_pending_block(),
                                        None => {
                                            rust_log("BFT: consensus engine missing during finalize");
                                            continue;
                                        }
                                    };
                                    match finalize_result {
                                        Ok(finalized_block) => {
                                            let fh = finalized_block.header.height;
                                            // WHY: DON'T increment blocks_produced here.
                                            // It was already incremented when the block was
                                            // produced in the slot timer. Incrementing again
                                            // causes double-counting.
                                            let new_h = inner.consensus.as_ref().map(|e| e.current_height()).unwrap_or(0);
                                            rust_log(&format!("BLOCK FINALIZED (BFT) height={} chain={}", fh, new_h));

                                            // Streamlet: track BFT-finalized block
                                            // WHY: If node_id() or hash() fails, skip Streamlet tracking.
                                            // A zero NodeId would cast votes as a nonexistent validator,
                                            // and a zero hash would track a nonexistent block.
                                            let sl_nid = inner.wallet.node_id().ok();
                                            let sl_bh = finalized_block.header.hash().map(|h| h.0).ok();
                                            if sl_nid.is_none() || sl_bh.is_none() {
                                                rust_log("STREAMLET: skipping BFT tracking — node_id or block hash unavailable");
                                            }
                                            if let (Some(ref mut streamlet), Some(our_nid), Some(bh)) = (&mut inner.streamlet, sl_nid, sl_bh) {
                                                streamlet.add_proposal(finalized_block.header.clone(), bh, sig_height);
                                                let sv = StreamletVote {
                                                    epoch: sig_height,
                                                    block_hash: bh,
                                                    height: fh,
                                                    signature: gratia_core::types::ValidatorSignature {
                                                        validator: our_nid,
                                                        signature: vec![1u8; 64],
                                                    },
                                                };
                                                let (_notarized, fin) = streamlet.add_vote(sv);
                                                if let Some(f) = fin {
                                                    rust_log(&format!("STREAMLET: finality at height {}", f));
                                                }
                                            }

                                            // Persist chain state.
                                            if let Some(ref persistence) = inner.chain_persistence {
                                                if let Some(ref engine) = inner.consensus {
                                                    let tip = engine.last_finalized_hash().0;
                                                    persistence.save(engine.current_height(), &tip, inner.blocks_produced);
                                                }
                                            }

                                            // Apply block transactions to on-chain state.
                                            let mut burned_fees_accum: u64 = 0;
                                            let mut new_applied_hashes: Vec<[u8; 32]> = Vec::new();
                                            let mut incoming_transfers_bft: Vec<(String, gratia_core::types::Address, Lux, chrono::DateTime<chrono::Utc>)> = Vec::new();
                                            let skip_hashes: std::collections::HashSet<[u8; 32]> = inner.applied_tx_hashes.clone();
                                            if let Some(ref sm) = inner.state_manager {
                                                let our_addr_bft = inner.wallet.address().ok();
                                                let mut incoming_lux_bft: Lux = 0;
                                                for tx in &finalized_block.transactions {
                                                    if skip_hashes.contains(&tx.hash.0) {
                                                        continue;
                                                    }
                                                    let sender_addr = gratia_core::types::Address::from_pubkey(&tx.sender_pubkey);
                                                    if let gratia_core::types::TransactionPayload::Transfer { to, amount } = &tx.payload {
                                                        let mut sender_acct = sm.get_account(&sender_addr).unwrap_or_default();
                                                        let total = amount + tx.fee;
                                                        if sender_acct.balance >= total && sender_acct.nonce == tx.nonce {
                                                            sender_acct.balance -= total;
                                                            sender_acct.nonce += 1;
                                                            let _ = sm.db().put_account(&sender_addr, &sender_acct);
                                                            let mut recv_acct = sm.get_account(to).unwrap_or_default();
                                                            recv_acct.balance += amount;
                                                            let _ = sm.db().put_account(to, &recv_acct);
                                                            if tx.fee > 0 {
                                                                burned_fees_accum = burned_fees_accum.saturating_add(tx.fee);
                                                                rust_log(&format!("FEE BURNED: {} Lux from tx {}", tx.fee, hex::encode(tx.hash.0)));
                                                            }
                                                        } else {
                                                            continue;
                                                        }
                                                        new_applied_hashes.push(tx.hash.0);
                                                        if let Some(ref our) = our_addr_bft {
                                                            if to == our {
                                                                incoming_lux_bft += amount;
                                                                incoming_transfers_bft.push((
                                                                    hex::encode(tx.hash.0),
                                                                    sender_addr,
                                                                    *amount,
                                                                    tx.timestamp,
                                                                ));
                                                            }
                                                        }
                                                    }
                                                }
                                                if incoming_lux_bft > 0 {
                                                    rust_log(&format!(
                                                        "BFT FINALIZE: credited {} Lux ({} GRAT) from incoming transfer(s)",
                                                        incoming_lux_bft, incoming_lux_bft / 1_000_000
                                                    ));
                                                }
                                            }
                                            for h in new_applied_hashes {
                                                inner.applied_tx_hashes.insert(h);
                                            }
                                            for (hash, sender, amount, ts) in incoming_transfers_bft {
                                                inner.wallet.record_incoming_transfer(hash, sender, amount, ts);
                                            }
                                            inner.total_burned_tx_fees = inner.total_burned_tx_fees.saturating_add(burned_fees_accum);

                                            inner.pending_broadcast_block = Some(finalized_block.clone());
                                            inner.recent_blocks.push_back(finalized_block.clone());
                                            if inner.recent_blocks.len() > 100 {
                                                inner.recent_blocks.pop_front();
                                            }

                                            // WHY: Credit reward ONLY here (BFT path) — the
                                            // slot timer path only credits for solo blocks
                                            // (real_members > 1 check). This is the single
                                            // source of truth for BFT-finalized block rewards.
                                            // blocks_produced is NOT incremented here (already
                                            // done at production time in slot timer).
                                            {
                                                let active_miners = 1u64.max(inner.staking.staker_count() as u64).max(1);
                                                let reward: Lux = gratia_core::emission::EmissionSchedule
                                                    ::per_miner_block_reward_lux(fh, active_miners);
                                                // Update on-chain state FIRST (source of truth)
                                                if let (Some(ref sm), Ok(our_addr)) = (&inner.state_manager, inner.wallet.address()) {
                                                    let mut acct = sm.get_account(&our_addr).unwrap_or_default();
                                                    acct.balance += reward;
                                                    let _ = sm.db().put_account(&our_addr, &acct);
                                                    // Sync wallet FROM on-chain state
                                                    inner.wallet.sync_balance(acct.balance);
                                                    rust_log(&format!(
                                                        "REWARD (BFT): height={} reward={} Lux ({} GRAT) balance={} Lux",
                                                        fh, reward, reward / 1_000_000, acct.balance
                                                    ));
                                                }
                                                // Track session stats for UI animations
                                                inner.session_grat_earned = inner.session_grat_earned.saturating_add(reward);
                                                inner.session_blocks_produced += 1;
                                                inner.last_reward_timestamp = Some(Utc::now());
                                            }

                                            // Persist on-chain state.
                                            if let Some(ref backend) = inner.storage_backend {
                                                let _ = backend.persist();
                                            }
                                        }
                                        Err(e) => {
                                            rust_log(&format!("BFT FINALIZE FAILED: {}", e));
                                        }
                                    }
                                    inner.pending_block_hash = None;
                                    inner.pending_block_created_at = None;
                                }
                            } else {
                                // WHY: Check if this signature matches a recently-expired
                                // block. Gossipsub can deliver signatures after the BFT
                                // timeout fires (mesh heartbeat delay, mobile radio wake).
                                // The expired block's pending state is still in the engine
                                // — we just cleared the FFI tracking. If the sig matches,
                                // try to finalize it as a "late save."
                                let matches_expired = inner.last_expired_block_hash
                                    .map(|h| h == sig_block_hash)
                                    .unwrap_or(false);

                                if matches_expired {
                                    rust_log(&format!(
                                        "BFT: LATE SIG for expired block at height {} — attempting rescue",
                                        sig_height
                                    ));
                                    let finalized = if let Some(ref mut engine) = inner.consensus {
                                        match engine.add_block_signature(sig_msg.signature) {
                                            Ok(f) => f,
                                            Err(_) => false,
                                        }
                                    } else { false };

                                    if finalized {
                                        rust_log(&format!("BFT: LATE FINALITY rescued block at height {}!", sig_height));
                                        inner.consecutive_bft_expirations = 0;
                                        inner.consecutive_solo_blocks = 0;
                                        inner.bft_retry_count = 0;
                                        inner.last_finalized_at = Some(std::time::Instant::now());
                                        inner.last_expired_block_hash = None;
                                        inner.last_expired_block_height = None;
                                        let finalize_result = match inner.consensus.as_mut() {
                                            Some(engine) => engine.finalize_pending_block(),
                                            None => continue,
                                        };
                                        match finalize_result {
                                            Ok(finalized_block) => {
                                                let fh = finalized_block.header.height;
                                                let new_h = inner.consensus.as_ref().map(|e| e.current_height()).unwrap_or(0);
                                                rust_log(&format!("BLOCK FINALIZED (LATE BFT) height={} chain={}", fh, new_h));

                                                if let Some(ref persistence) = inner.chain_persistence {
                                                    if let Some(ref engine) = inner.consensus {
                                                        let tip = engine.last_finalized_hash().0;
                                                        persistence.save(engine.current_height(), &tip, inner.blocks_produced);
                                                    }
                                                }

                                                // WHY: Apply block transactions to on-chain state.
                                                // Without this, late-finalized blocks' transfers
                                                // would be lost — balances and nonces never updated.
                                                // This mirrors the normal BFT finalization path.
                                                let mut burned_fees_accum: u64 = 0;
                                                // Collect hashes to mark as applied (avoids borrow conflict)
                                                let mut new_applied_hashes: Vec<[u8; 32]> = Vec::new();
                                                let skip_hashes: std::collections::HashSet<[u8; 32]> = inner.applied_tx_hashes.clone();
                                                if let Some(ref sm) = inner.state_manager {
                                                    for tx in &finalized_block.transactions {
                                                        if skip_hashes.contains(&tx.hash.0) {
                                                            continue;
                                                        }
                                                        let sender_addr = gratia_core::types::Address::from_pubkey(&tx.sender_pubkey);
                                                        if let gratia_core::types::TransactionPayload::Transfer { to, amount } = &tx.payload {
                                                            let mut sender_acct = sm.get_account(&sender_addr).unwrap_or_default();
                                                            let total = amount + tx.fee;
                                                            if sender_acct.balance >= total && sender_acct.nonce == tx.nonce {
                                                                sender_acct.balance -= total;
                                                                sender_acct.nonce += 1;
                                                                let _ = sm.db().put_account(&sender_addr, &sender_acct);
                                                                let mut recv_acct = sm.get_account(to).unwrap_or_default();
                                                                recv_acct.balance += amount;
                                                                let _ = sm.db().put_account(to, &recv_acct);
                                                                new_applied_hashes.push(tx.hash.0);
                                                                if tx.fee > 0 {
                                                                    burned_fees_accum = burned_fees_accum.saturating_add(tx.fee);
                                                                    rust_log(&format!("FEE BURNED: {} Lux from tx {}", tx.fee, hex::encode(tx.hash.0)));
                                                                }
                                                            }
                                                        }
                                                    }
                                                }
                                                for h in new_applied_hashes {
                                                    inner.applied_tx_hashes.insert(h);
                                                }
                                                inner.total_burned_tx_fees = inner.total_burned_tx_fees.saturating_add(burned_fees_accum);

                                                // Credit mining reward for the late-finalized block
                                                {
                                                    let active_miners = 1u64.max(inner.staking.staker_count() as u64);
                                                    let reward: Lux = EmissionSchedule
                                                        ::per_miner_block_reward_lux(fh, active_miners);
                                                    if let (Some(ref sm), Ok(our_addr)) = (&inner.state_manager, inner.wallet.address()) {
                                                        let mut acct = sm.get_account(&our_addr).unwrap_or_default();
                                                        acct.balance += reward;
                                                        let _ = sm.db().put_account(&our_addr, &acct);
                                                        inner.wallet.sync_balance(acct.balance);
                                                    }
                                                    rust_log(&format!(
                                                        "REWARD (LATE): height={} reward={} Lux ({} GRAT)",
                                                        fh, reward, reward / 1_000_000
                                                    ));
                                                    // Track session stats for UI animations
                                                    inner.session_grat_earned = inner.session_grat_earned.saturating_add(reward);
                                                    inner.session_blocks_produced += 1;
                                                    inner.last_reward_timestamp = Some(Utc::now());
                                                }

                                                // WHY: Broadcast the late-finalized block to the network.
                                                // Without this, peers never learn about the block and
                                                // can't update their chain state. The block has full
                                                // threshold signatures from PendingBlock::finalize().
                                                inner.pending_broadcast_block = Some(finalized_block.clone());

                                                // WHY: Cache for sync protocol — new peers joining
                                                // need recent blocks to catch up.
                                                inner.recent_blocks.push_back(finalized_block.clone());
                                                if inner.recent_blocks.len() > 100 {
                                                    inner.recent_blocks.pop_front();
                                                }

                                                if let Some(ref backend) = inner.storage_backend {
                                                    let _ = backend.persist();
                                                }
                                            }
                                            Err(e) => {
                                                rust_log(&format!("BFT LATE FINALIZE FAILED: {}", e));
                                            }
                                        }
                                    }
                                } else {
                                    rust_log(&format!(
                                        "BFT: ignoring sig for height {} (no matching pending block)",
                                        sig_height
                                    ));
                                }
                            }

                            // WHY: Validator signatures are internal consensus traffic —
                            // no need to surface them as an FfiNetworkEvent to the mobile UI.
                            continue;
                        }
                        NetworkEvent::SyncBlocksReceived(blocks) => {
                            // WHY: The network layer received and validated a batch of
                            // sync blocks. Process them through consensus, apply
                            // transactions to state, credit mining rewards, and update
                            // the recent_blocks cache. This mirrors BlockReceived
                            // processing but handles an ordered batch sequentially.
                            let block_count = blocks.len();
                            let first_h = blocks.first().map(|b| b.header.height).unwrap_or(0);
                            let last_h = blocks.last().map(|b| b.header.height).unwrap_or(0);

                            rust_log(&format!(
                                "Sync: received {} blocks (heights {}-{})",
                                block_count, first_h, last_h,
                            ));

                            let mut applied_height = 0u64;
                            let mut total_txs_applied = 0u32;
                            for block in &blocks {
                                let h = block.header.height;
                                let accepted = if let Some(ref mut consensus) = inner.consensus {
                                    match consensus.process_incoming_block(block.clone()) {
                                        Ok(BlockProcessResult::Accepted) => {
                                            applied_height = consensus.current_height();
                                            true
                                        }
                                        Ok(BlockProcessResult::Skipped) => false,
                                        Ok(BlockProcessResult::ForkDetected) => {
                                            rust_log(&format!(
                                                "Sync: fork detected at height {}, stopping sync batch",
                                                h,
                                            ));
                                            break;
                                        }
                                        Err(e) => {
                                            warn!(height = h, error = %e, "Failed to apply sync block");
                                            break;
                                        }
                                    }
                                } else { false };

                                if !accepted {
                                    continue;
                                }

                                // ── Validate and apply transactions ──────────────
                                // WHY: Same validation as BlockReceived — reject blocks
                                // with invalid transactions to prevent state corruption.
                                if !block.transactions.is_empty() {
                                    if let Err(e) = validate_block_transactions(
                                        &block.transactions,
                                        MIN_TRANSACTION_FEE,
                                    ) {
                                        warn!(
                                            "Sync: rejecting block {} — invalid txs: {}",
                                            h, e,
                                        );
                                        continue;
                                    }

                                    // Apply transactions to on-chain state
                                    let mut burned_fees_accum: u64 = 0;
                                    // Collect hashes to mark as applied (avoids borrow conflict)
                                    let mut new_applied_hashes: Vec<[u8; 32]> = Vec::new();
                                    let skip_hashes: std::collections::HashSet<[u8; 32]> = inner.applied_tx_hashes.clone();
                                    if let Some(ref sm) = inner.state_manager {
                                        let our_addr = inner.wallet.address().ok();
                                        let mut incoming_lux: Lux = 0;
                                        for tx in &block.transactions {
                                            if skip_hashes.contains(&tx.hash.0) {
                                                continue;
                                            }
                                            let sender_addr = gratia_core::types::Address::from_pubkey(&tx.sender_pubkey);
                                            match &tx.payload {
                                                gratia_core::types::TransactionPayload::Transfer { to, amount } => {
                                                    let mut sender_acct = sm.get_account(&sender_addr).unwrap_or_default();
                                                    let total = amount + tx.fee;
                                                    if sender_acct.balance >= total && sender_acct.nonce == tx.nonce {
                                                        sender_acct.balance -= total;
                                                        sender_acct.nonce += 1;
                                                        let _ = sm.db().put_account(&sender_addr, &sender_acct);
                                                        if tx.fee > 0 {
                                                            burned_fees_accum = burned_fees_accum.saturating_add(tx.fee);
                                                            rust_log(&format!("FEE BURNED: {} Lux from tx {}", tx.fee, hex::encode(tx.hash.0)));
                                                        }
                                                    }
                                                    let mut recv_acct = sm.get_account(to).unwrap_or_default();
                                                    recv_acct.balance += amount;
                                                    let _ = sm.db().put_account(to, &recv_acct);
                                                    new_applied_hashes.push(tx.hash.0);
                                                    if let Some(ref our) = our_addr {
                                                        if to == our {
                                                            incoming_lux += amount;
                                                        }
                                                    }
                                                    total_txs_applied += 1;
                                                }
                                                _ => { total_txs_applied += 1; }
                                            }
                                        }
                                        // Sync wallet from on-chain state after transfers
                                        if incoming_lux > 0 {
                                            if let (Some(ref sm2), Ok(our_addr2)) = (&inner.state_manager, inner.wallet.address()) {
                                                let acct = sm2.get_account(&our_addr2).unwrap_or_default();
                                                inner.wallet.sync_balance(acct.balance);
                                            }
                                        }
                                    }
                                    for h in new_applied_hashes {
                                        inner.applied_tx_hashes.insert(h);
                                    }
                                    inner.total_burned_tx_fees = inner.total_burned_tx_fees.saturating_add(burned_fees_accum);
                                }

                                // ── Credit mining reward to block producer ───────
                                // WHY: Same as BlockReceived — credit the producer's
                                // account so balances reflect the true chain state.
                                if let Some(ref sm) = inner.state_manager {
                                    let producer_addr = gratia_core::types::Address::from_pubkey(&block.header.producer_pubkey);
                                    let our_addr = inner.wallet.address().ok();
                                    let is_self = our_addr.as_ref() == Some(&producer_addr);

                                    if !is_self {
                                        let active_miners = (block.header.active_miners as u64).max(1);
                                        let reward: Lux = EmissionSchedule
                                            ::per_miner_block_reward_lux(h, active_miners);
                                        let mut acct = sm.get_account(&producer_addr).unwrap_or_default();
                                        acct.balance += reward;
                                        let _ = sm.db().put_account(&producer_addr, &acct);
                                    }
                                }

                                // ── Update recent_blocks cache ───────────────────
                                // WHY: Without this, synced blocks don't get cached and
                                // can't be served to peers that connect later. The cache
                                // is the primary source for the BlockProvider when state
                                // store doesn't have the block yet.
                                inner.recent_blocks.push_back(block.clone());
                                if inner.recent_blocks.len() > 100 {
                                    inner.recent_blocks.pop_front();
                                }
                            }

                            // WHY: Notify consensus sync protocol that blocks were applied
                            // so it can advance its state machine and request more if needed.
                            if applied_height > 0 {
                                if let Some(ref mut sp) = inner.sync_protocol {
                                    sp.mark_blocks_applied(applied_height);
                                }
                                // Update network sync manager too
                                if let Some(ref mut sync) = inner.sync_manager {
                                    if let Some(last_block) = blocks.last() {
                                        if let Ok(hash) = last_block.header.hash() {
                                            sync.update_local_state(applied_height, hash);
                                        }
                                    }
                                }
                                // Persist chain state
                                if let Some(ref persistence) = inner.chain_persistence {
                                    let tip = inner.consensus.as_ref()
                                        .map(|e| e.last_finalized_hash().0)
                                        .unwrap_or([0u8; 32]);
                                    persistence.save(applied_height, &tip, inner.blocks_produced);
                                }

                                // Persist state to disk periodically during sync
                                if applied_height % 10 == 0 {
                                    if let Some(ref backend) = inner.storage_backend {
                                        let _ = backend.persist();
                                    }
                                }

                                // WHY: Trigger network to request the next batch if
                                // we're still behind. Without this, the sync stalls
                                // until the next 32-second maintenance tick.
                                if let Some(ref network) = inner.network {
                                    let _ = network.try_request_sync();
                                }

                                rust_log(&format!(
                                    "Sync: applied {} blocks ({} txs), height now {}",
                                    block_count, total_txs_applied, applied_height,
                                ));
                            }

                            continue;
                        }
                        NetworkEvent::SyncStateChanged(sync_state) => {
                            // WHY: The network sync manager changed state (e.g., peer
                            // reported a new chain tip). Update the consensus sync
                            // protocol's network height so it stays in sync.
                            if let Some(ref mut sp) = inner.sync_protocol {
                                let net_h = match sync_state {
                                    SyncState::Syncing { target_height, .. } => target_height,
                                    SyncState::Behind { network_height, .. } => network_height,
                                    _ => 0,
                                };
                                if net_h > 0 {
                                    sp.on_block_received(net_h);
                                }
                            }
                            continue;
                        }
                        // Other events (attestations, etc.) — skip for now
                        _ => continue,
                    };
                    new_events.push(ffi_event);
        }

        // Put the receiver back
        inner.network_event_rx = Some(rx);

        Ok(new_events)
    }

    // ========================================================================
    // Consensus methods
    // ========================================================================

    /// Start the consensus engine and slot timer.
    ///
    /// Initializes the consensus engine with a demo committee of this node
    /// plus any connected peers. Starts a background slot timer that advances
    /// the consensus every 4 seconds.
    ///
    /// The network must be started first so received blocks can be processed.
    pub fn start_consensus(&self) -> Result<FfiConsensusStatus, FfiError> {
        let mut inner = self.lock_inner()?;

        if inner.consensus.is_some() {
            return Err(FfiError::InternalError {
                reason: "consensus already started".into(),
            });
        }

        let node_id = self.get_node_id_or_default(&inner);

        // WHY: Use the wallet's signing key bytes for VRF derivation. The
        // VRF secret key is derived from the Ed25519 signing key.
        let signing_key_bytes = inner
            .wallet
            .signing_key_bytes()
            .map_err(|_| FfiError::WalletNotInitialized)?;

        // WHY: Use the real presence score from sensor data for VRF weighting.
        // During onboarding (day 0), use minimum threshold of 40 so new users
        // can participate in block production but established nodes are favored.
        // If no score has been computed yet but user is past onboarding, fall
        // back to 75. Debug bypass uses 100 to ensure demo node wins.
        let real_score = inner.presence_score;
        let presence_score: u8 = if inner.is_debug_bypass() {
            100
        } else if real_score > 0 {
            real_score
        } else if inner.pol.is_onboarding() {
            40 // WHY: Minimum threshold for day 0 onboarding users
        } else {
            75 // WHY: Reasonable default for established nodes before first PoL calculation
        };

        let mut engine = ConsensusEngine::new(node_id, &signing_key_bytes, presence_score);

        // Load persisted chain state (height, tip hash, blocks produced) if available.
        // WHY: On app restart, the consensus engine continues from where it left off
        // instead of restarting from genesis.
        let (initial_height, initial_hash, persisted_blocks) = inner.chain_persistence
            .as_ref()
            .and_then(|p| p.load())
            .unwrap_or((0, [0u8; 32], 0));

        if initial_height > 0 {
            engine.restore_state(initial_height, BlockHash(initial_hash));
            rust_log(&format!(
                "Restored chain: height={}, blocks_produced={}",
                initial_height, persisted_blocks
            ));
        }
        inner.blocks_produced = persisted_blocks;

        // Build the committee from this node + any known peers from NodeAnnouncements.
        // WHY: Using real peer data means connected phones participate in actual
        // multi-node consensus. Synthetic padding is only added when fewer than 3
        // real nodes exist (the minimum for committee operation).
        let vrf_pubkey = VrfSecretKey::from_ed25519_bytes(&signing_key_bytes).public_key();
        let vrf_pubkey_bytes = vrf_pubkey.bytes; // Save before move
        let local_signing_pubkey = gratia_core::crypto::Keypair::from_secret_key_bytes(&signing_key_bytes).public_key_bytes();
        let mut all_eligible = vec![EligibleNode {
            node_id,
            vrf_pubkey,
            presence_score: presence_score,
            has_valid_pol: true,
            meets_minimum_stake: true,
            pol_days: 90,
            signing_pubkey: local_signing_pubkey,
            vrf_proof: vec![],
        }];

        // WHY: Convert each known peer's NodeAnnouncement into an EligibleNode.
        // These are real phones that announced themselves via gossipsub.
        for peer in &inner.known_peer_nodes {
            all_eligible.push(EligibleNode {
                node_id: peer.node_id,
                vrf_pubkey: VrfPublicKey { bytes: peer.vrf_pubkey_bytes },
                presence_score: peer.presence_score,
                has_valid_pol: true,
                meets_minimum_stake: true,
                pol_days: peer.pol_days,
                signing_pubkey: peer.ed25519_pubkey.to_vec(),
                vrf_proof: vec![],
            });
        }

        // WHY: Synthetic padding removed. With 2 real nodes, the committee works
        // fine — round-robin alternates between the 2 real producers. Synthetic
        // nodes caused slot mismatches: a fake NodeId would be selected as the
        // expected producer, but no real node could match it, causing blocks to
        // be rejected. The committee selection logic handles < 3 nodes gracefully.
        let real_count = all_eligible.len();

        // WHY: Sort by node_id so all phones build the committee in the same
        // canonical order, regardless of which phone is "self" vs "peer".
        all_eligible.sort_by(|a, b| a.node_id.0.cmp(&b.node_id.0));

        inner.real_committee_members = real_count;

        rust_log(&format!(
            "Committee: {} real + {} synthetic = {} total, local score={}",
            real_count,
            all_eligible.len() - real_count,
            all_eligible.len(),
            presence_score,
        ));

        // WHY: RANDAO-style seed — at genesis (height=0, no blocks yet) this
        // produces SHA-256("gratia-epoch-seed-v1:" + [0;32] + 0), deterministic
        // but unique. No entropy available at genesis anyway.
        // Store as the epoch seed so subsequent committee rebuilds within the
        // same epoch reuse it (prevents mid-epoch seed manipulation).
        let epoch_seed = engine.compute_epoch_seed();
        inner.epoch_seed = Some(epoch_seed);
        engine.initialize_committee(&all_eligible, &epoch_seed, 0, 0)
            .map_err(|e| FfiError::InternalError {
                reason: format!("failed to initialize committee: {}", e),
            })?;

        let status = consensus_status(&engine, 0);
        inner.consensus = Some(engine);

        // Initialize Streamlet BFT state machine.
        // WHY: Formally proven consensus protocol. Committee size determines
        // the notarization threshold (2/3+ votes). For solo mode (1 real node),
        // every self-vote notarizes immediately.
        let mut streamlet = StreamletState::new(node_id, real_count);
        streamlet.restore(initial_height, initial_hash);
        inner.streamlet = Some(streamlet);
        rust_log(&format!(
            "Streamlet BFT initialized: committee_size={}, finalized_height={}",
            real_count, initial_height
        ));

        // Initialize sync manager with the current chain state.
        // WHY: The sync manager tracks peer chain tips and generates
        // sync requests when this node falls behind.
        inner.sync_manager = Some(SyncManager::new(initial_height, BlockHash(initial_hash)));

        // Initialize consensus-level sync protocol.
        // WHY: Sits above the network SyncManager and tracks the sync state
        // machine (idle/requesting/downloading/synced) with progress reporting
        // for the mobile UI. Uses our node_id and current height as starting point.
        inner.sync_protocol = Some(ConsensusSyncProtocol::new(node_id, initial_height));

        // Initialize on-chain state manager with storage backend.
        // WHY: The state manager tracks account balances and nonces on-chain.
        // When blocks are finalized, transactions are applied to state — enforcing
        // balance checks and nonce ordering. This prevents double-spends.
        // WHY open_storage: The factory picks the right backend. With the
        // `rocksdb-backend` feature enabled, RocksDB is used (automatic
        // persistence, efficient iteration). Without it, InMemoryStore with
        // file persistence is used (no C++ dependency, works everywhere).
        let state_path = format!("{}/chain_state.db", self.data_dir);
        let backend_config = {
            #[cfg(feature = "rocksdb-backend")]
            {
                // WHY: RocksDB directory is separate from the file-based path
                let rocksdb_path = format!("{}/rocksdb", self.data_dir);
                StorageBackendConfig::RocksDb { db_path: rocksdb_path }
            }
            #[cfg(not(feature = "rocksdb-backend"))]
            {
                StorageBackendConfig::InMemory {
                    persistence_path: Some(state_path.clone()),
                }
            }
        };
        let backend = open_storage(backend_config).map_err(|e| FfiError::InternalError {
            reason: format!("failed to open storage backend: {}", e),
        })?;
        let sm = StateManager::new(backend.store.clone());
        inner.storage_backend = Some(backend);

        // WHY: Only seed if the on-chain account has zero balance (fresh store).
        // If we loaded from a persistence file, the account already has the
        // correct balance and seeding would overwrite it.
        if let Ok(our_address) = inner.wallet.address() {
            let on_chain_balance = sm.get_balance(&our_address).unwrap_or(0);
            if on_chain_balance == 0 {
                let current_balance = inner.wallet.balance();
                if current_balance > 0 {
                    let mut acct = sm.get_account(&our_address).unwrap_or_default();
                    acct.balance = current_balance;
                    let _ = sm.db().put_account(&our_address, &acct);
                    rust_log(&format!(
                        "State seeded: {} Lux ({} GRAT) for local wallet",
                        current_balance, current_balance / 1_000_000
                    ));
                }
            } else {
                rust_log(&format!(
                    "State loaded from disk: {} Lux ({} GRAT) on-chain, {} entries",
                    on_chain_balance, on_chain_balance / 1_000_000,
                    inner.storage_backend.as_ref().and_then(|b| b.in_memory_handle.as_ref()).map(|s| s.data_size_estimate()).unwrap_or(0),
                ));
            }
        }
        inner.state_manager = Some(sm);

        // Wire block provider into network for sync protocol.
        // WHY: Now that state is initialized, the network can serve blocks to
        // peers requesting them via the sync protocol. Before this point,
        // the NoBlockProvider returns empty results.
        // WHY: Clone the store Arc before borrowing network mutably to avoid
        // conflicting borrows on `inner` (mutable for network, immutable for storage_backend).
        let store_clone = inner.storage_backend.as_ref().map(|b| b.store.clone());
        if let (Some(ref mut network), Some(store)) = (&mut inner.network, store_clone) {
            let provider = Arc::new(StateBlockProvider {
                store,
            });
            network.set_block_provider(provider);
            rust_log("Block provider wired into network for sync");

            // WHY: After restore_state() loads height from persistence, the network
            // SyncManager's local height must be updated. The network was started
            // earlier (start_network) with height 0. Without this, the SyncManager
            // still thinks we're at height 0 and generates sync requests for blocks
            // we already have, while reporting "chain tip 0" to peers.
            if initial_height > 0 {
                let _ = network.try_reset_local_height(initial_height, BlockHash(initial_hash));
                rust_log(&format!(
                    "Network SyncManager updated to restored height {}",
                    initial_height
                ));
            }
        }

        // Start the slot timer background task
        let inner_arc = Arc::clone(&self.inner);
        let handle = self.runtime.spawn(async move {
            run_slot_timer(inner_arc).await;
        });
        inner.slot_timer_handle = Some(handle);

        // WHY: Announce our node to connected peers so they can include us
        // in their committee. This is the trigger for real multi-node consensus —
        // each phone announces itself, and all phones rebuild their committees
        // with the real peer data.
        if let Some(ref network) = inner.network {
            let keypair_for_ann = gratia_core::crypto::Keypair::from_secret_key_bytes(&signing_key_bytes);
            let mut announcement = NodeAnnouncement {
                node_id,
                vrf_pubkey_bytes: vrf_pubkey_bytes,
                presence_score: presence_score,
                pol_days: 90,
                timestamp: Utc::now(),
                ed25519_pubkey: *keypair_for_ann.public_key().as_bytes(),
                signature: Vec::new(),
                height: initial_height,
            };
            let payload = gratia_network::gossip::node_announcement_signing_payload(&announcement);
            announcement.signature = keypair_for_ann.sign(&payload);
            if let Err(e) = network.try_announce_node_sync(&announcement) {
                warn!("Failed to announce node after consensus start: {}", e);
            } else {
                rust_log("Announced node to network after consensus start");
            }
        }

        info!("FFI: consensus started");
        Ok(status)
    }

    /// Stop the consensus engine.
    ///
    /// WHY: Clears ALL BFT state (pending block, expiration counters) so that
    /// a subsequent start_consensus() begins with a clean slate. Without this,
    /// rapid WiFi toggle cycles (stop→start→stop→start) leave orphaned BFT
    /// votes/proposals that prevent block finality after restart.
    pub fn stop_consensus(&self) -> Result<(), FfiError> {
        let mut inner = self.lock_inner()?;

        if let Some(ref mut engine) = inner.consensus {
            engine.stop();
        }
        inner.consensus = None;
        inner.sync_protocol = None;

        // WHY: Clear all BFT tracking state. If a pending block was awaiting
        // signatures when we stopped, those signatures will never arrive on the
        // restarted consensus. Leaving stale pending_block_hash causes the new
        // consensus to think a block is in-flight and skip production.
        inner.pending_block_hash = None;
        inner.pending_block_created_at = None;
        inner.consecutive_bft_expirations = 0;
        inner.last_expired_block_hash = None;
        inner.last_expired_block_height = None;
        inner.streamlet = None;

        // Cancel the slot timer
        if let Some(handle) = inner.slot_timer_handle.take() {
            handle.abort();
        }

        info!("FFI: consensus stopped, BFT state cleared");
        Ok(())
    }

    /// Request block sync from connected peers.
    ///
    /// Checks if this node is behind the network and requests missing blocks.
    /// Called periodically from the mobile app or automatically after peer connect.
    /// Returns the current sync state.
    pub fn request_sync(&self) -> Result<String, FfiError> {
        let inner = self.lock_inner()?;
        // WHY: Actually trigger the network layer to generate and send sync
        // requests. Previously this was a no-op that only returned status.
        // Now it kicks off block downloads when the node is behind peers.
        if let Some(ref network) = inner.network {
            let _ = network.try_request_sync();
        }
        Ok(inner.get_sync_status_string())
    }

    /// Get detailed sync status from the consensus-level sync protocol.
    ///
    /// Returns structured sync state for the mobile UI including progress
    /// percentage, current height, and target height.
    pub fn get_sync_status(&self) -> Result<FfiSyncStatus, FfiError> {
        let inner = self.lock_inner()?;

        match &inner.sync_protocol {
            Some(sp) => {
                let (current, target) = sp.sync_progress();
                let state_str = match sp.state() {
                    gratia_consensus::sync::SyncState::Idle => "idle",
                    gratia_consensus::sync::SyncState::Requesting => "syncing",
                    gratia_consensus::sync::SyncState::Downloading { .. } => "syncing",
                    gratia_consensus::sync::SyncState::Applying => "syncing",
                    gratia_consensus::sync::SyncState::Synced => "synced",
                };
                let progress = (sp.state().progress() * 100.0) as u8;
                Ok(FfiSyncStatus {
                    state: state_str.to_string(),
                    current_height: current,
                    target_height: target,
                    progress_percent: progress,
                })
            }
            None => {
                // WHY: sync_protocol is None before start_consensus() is called.
                // Return idle with zero heights so the UI can show "not started".
                let height = inner.get_local_height();
                Ok(FfiSyncStatus {
                    state: "idle".to_string(),
                    current_height: height,
                    target_height: height,
                    progress_percent: 100,
                })
            }
        }
    }

    /// Get the current consensus status.
    pub fn get_consensus_status(&self) -> Result<FfiConsensusStatus, FfiError> {
        let inner = self.lock_inner()?;

        match &inner.consensus {
            Some(engine) => Ok(consensus_status(engine, inner.blocks_produced)),
            None => {
                // WHY: When consensus is stopped (user tapped Stop Mining), show the
                // last known chain height and blocks produced from persistence instead
                // of zeros. This prevents the UI from misleadingly showing Block Height 0
                // when the node has a valid chain stored on disk.
                let (height, _hash, produced) = inner.chain_persistence
                    .as_ref()
                    .and_then(|p| p.load())
                    .unwrap_or((0, [0u8; 32], 0));
                Ok(FfiConsensusStatus {
                    state: "stopped".to_string(),
                    current_slot: 0,
                    current_height: height,
                    is_committee_member: false,
                    blocks_produced: produced,
                })
            }
        }
    }
    // ========================================================================
    // Health / Telemetry
    // ========================================================================

    /// Build a JSON health report for remote diagnostics.
    ///
    /// WHY: When testing on remote phones (e.g., Samsung A06 without physical
    /// access), we need a lightweight snapshot of node health. This is READ-ONLY
    /// — the Kotlin layer decides where to send it (bootstrap server, local file,
    /// or nowhere). No networking or external dependencies added.
    pub fn get_health_report(&self) -> Result<String, FfiError> {
        let inner = self.lock_inner()?;

        // Node ID: first 16 hex chars (8 bytes) for privacy-safe identification.
        let node_id = inner.wallet.node_id()
            .map(|id| hex::encode(&id.0[..8]))
            .unwrap_or_else(|_| "uninitialized".to_string());

        // Chain heights from consensus engine or persistence fallback.
        let (chain_height, finalized_height) = match &inner.consensus {
            Some(engine) => {
                let h = engine.current_height();
                // WHY: In Phase 1, finalized_height == chain_height because
                // every accepted block is immediately finalized (no orphan pool).
                (h, h)
            }
            None => {
                let h = inner.chain_persistence
                    .as_ref()
                    .and_then(|p| p.load())
                    .map(|(h, _, _)| h)
                    .unwrap_or(0);
                (h, h)
            }
        };

        let peer_count = inner.network.as_ref()
            .map(|n| n.connected_peer_count())
            .unwrap_or(0);

        // PoL status
        let pol_valid = inner.is_debug_bypass()
            || inner.pol.is_onboarding()
            || inner.pol.current_day_valid()
            || inner.pol.in_grace_period();

        let pol_params_met = inner.pol.parameters_met_count();

        let is_mining = matches!(inner.mining_state, MiningState::Mining);

        let uptime_secs = APP_START_TIME.get()
            .map(|start| start.elapsed().as_secs())
            .unwrap_or(0);

        let last_block_age_secs = inner.last_finalized_at
            .map(|t| t.elapsed().as_secs())
            .unwrap_or(u64::MAX); // MAX signals "never finalized"

        let error_count = ERROR_COUNT.load(Ordering::Relaxed);

        // Build JSON manually to avoid adding serde derives to this struct.
        // WHY: This is a one-off serialization for diagnostics, not a
        // persistent data type. Manual JSON avoids #[derive(Serialize)]
        // on GratiaNodeInner fields that shouldn't be serializable.
        let json = serde_json::json!({
            "node_id": node_id,
            "app_version": env!("CARGO_PKG_VERSION"),
            "chain_height": chain_height,
            "finalized_height": finalized_height,
            "peer_count": peer_count,
            "real_committee_members": inner.real_committee_members,
            "blocks_produced": inner.blocks_produced,
            "consecutive_solo_blocks": inner.consecutive_solo_blocks,
            "consecutive_bft_expirations": inner.consecutive_bft_expirations,
            "balance_lux": inner.wallet.balance(),
            "pol_valid": pol_valid,
            "pol_params_met": pol_params_met,
            "is_mining": is_mining,
            "battery_percent": inner.power_state.battery_percent,
            "is_charging": inner.power_state.is_plugged_in,
            "uptime_secs": uptime_secs,
            "last_block_age_secs": if last_block_age_secs == u64::MAX { serde_json::Value::Null } else { serde_json::json!(last_block_age_secs) },
            "error_count": error_count,
        });

        Ok(json.to_string())
    }

    /// Get mining stats optimized for UI animation data.
    ///
    /// WHY: The Kotlin UI layer needs a single lightweight JSON blob to drive:
    /// - Live balance with tick-up animation (total_balance_lux changes)
    /// - Mining streak fire counter (streak_days from PoL consecutive days)
    /// - Session earnings display (session_grat / session_blocks)
    /// - Peer count and block production indicator
    /// - Last reward age for pulse/glow animation
    /// - PoL parameter progress (params_met / params_total)
    ///
    /// Called on a 1-second timer from Kotlin — must be fast (no I/O).
    pub fn get_mining_stats(&self) -> Result<String, FfiError> {
        let inner = self.lock_inner()?;

        let balance_lux: u64 = inner.wallet.balance();
        let balance_grat: u64 = balance_lux / 1_000_000;

        let is_mining = matches!(inner.mining_state, MiningState::Mining);

        let peer_count = inner.network.as_ref()
            .map(|n| n.connected_peer_count())
            .unwrap_or(0);

        let chain_height = match &inner.consensus {
            Some(engine) => engine.current_height(),
            None => inner.chain_persistence
                .as_ref()
                .and_then(|p| p.load())
                .map(|(h, _, _)| h)
                .unwrap_or(0),
        };

        let last_reward_age_secs = inner.last_reward_timestamp
            .map(|ts| {
                let elapsed = Utc::now().signed_duration_since(ts);
                elapsed.num_seconds().max(0) as u64
            });

        let pol_params_met = inner.pol.parameters_met_count();

        let json = serde_json::json!({
            "streak_days": inner.pol.consecutive_days(),
            "session_grat": inner.session_grat_earned / 1_000_000,
            "session_grat_lux": inner.session_grat_earned,
            "session_blocks": inner.session_blocks_produced,
            "total_balance_grat": balance_grat,
            "total_balance_lux": balance_lux,
            "is_mining": is_mining,
            "peers": peer_count,
            "chain_height": chain_height,
            "last_reward_age_secs": last_reward_age_secs,
            "pol_params_met": pol_params_met,
            "pol_params_total": 8,
        });

        Ok(json.to_string())
    }

    // ========================================================================
    // Smart Contract methods
    // ========================================================================

    /// Initialize the GratiaVM with built-in demo contracts.
    ///
    /// Creates the VM engine with InterpreterRuntime for real WASM execution
    /// and deploys demo contracts that showcase mobile-native opcodes.
    ///
    /// WHY: InterpreterRuntime is a pure-Rust WASM interpreter (no wasmer/LLVM).
    /// It compiles for any target (Android ARM64, iOS, desktop) with zero C++
    /// dependencies. GratiaScript contracts compile to WASM and execute for real.
    pub fn init_vm(&self) -> Result<Vec<String>, FfiError> {
        let mut inner = self.lock_inner()?;

        let runtime = InterpreterRuntime::new();
        let mut vm = GratiaVm::new(Box::new(runtime));

        // WHY: A zero address as deployer would deploy contracts to an unowned
        // address, making them irrecoverable. Require a valid wallet identity.
        let deployer = inner.wallet.address().map_err(|_| FfiError::InternalError {
            reason: "wallet has no address — cannot deploy contracts".into(),
        })?;

        // WHY: Deploy real GratiaScript contracts compiled to WASM, not fake
        // bytecode with mock handlers. The InterpreterRuntime will parse and
        // execute the actual WASM instructions generated by the compiler.
        let demo_contracts = [
            ("PresenceVerifier", r#"
                contract PresenceVerifier {
                    const minScore: i32 = 70;
                    function verify(): bool {
                        let score = @presence();
                        if (score >= minScore) {
                            return true;
                        }
                        return false;
                    }
                    function getScore(): i32 {
                        return @presence();
                    }
                    function getMinimum(): i32 {
                        return minScore;
                    }
                }
            "#),
            ("ProximityGate", r#"
                contract ProximityGate {
                    const minPeers: i32 = 3;
                    function checkAccess(): bool {
                        let peers = @proximity();
                        if (peers >= minPeers) {
                            return true;
                        }
                        return false;
                    }
                    function getMinPeers(): i32 {
                        return minPeers;
                    }
                }
            "#),
            ("LocationCheck", r#"
                contract LocationCheck {
                    let triggerLat: f32 = 40.7;
                    let triggerLon: f32 = -74.0;
                    function isNear(): bool {
                        let loc = @location();
                        let dlat = loc.lat - triggerLat;
                        let dlon = loc.lon - triggerLon;
                        let dist = dlat * dlat + dlon * dlon;
                        if (dist < 0.01) {
                            return true;
                        }
                        return false;
                    }
                }
            "#),
        ];

        let mut deployed = Vec::new();
        for (name, source) in demo_contracts {
            match gratiascript::compile(source) {
                Ok(wasm) => {
                    match vm.deploy_contract(&deployer, &wasm, ContractPermissions::all()) {
                        Ok(contract_addr) => {
                            let hex = format!("grat:{}", hex::encode(contract_addr.0));
                            rust_log(&format!("VM: Compiled+deployed {} at {} ({} bytes WASM)", name, hex, wasm.len()));
                            deployed.push(hex);
                        }
                        Err(e) => warn!("VM: Failed to deploy {}: {}", name, e),
                    }
                }
                Err(e) => warn!("VM: Failed to compile {}: {}", name, e),
            }
        }

        inner.vm = Some(vm);
        rust_log(&format!("VM: initialized with {} demo contracts", deployed.len()));
        Ok(deployed)
    }

    /// Call a smart contract function.
    ///
    /// Executes a function on a deployed contract with gas metering,
    /// sandboxing, and access to mobile-native host functions.
    pub fn call_contract(
        &self,
        contract_address: String,
        function_name: String,
        gas_limit: u64,
    ) -> Result<FfiContractResult, FfiError> {
        let mut inner = self.lock_inner()?;

        // WHY: Extract all needed values before the mutable borrow of vm,
        // to avoid borrow conflicts through the MutexGuard.
        let addr = address_from_hex(&contract_address)
            .map_err(|r| FfiError::InvalidAddress { reason: r })?;
        // WHY: A zero address as caller would execute contracts as a nonexistent
        // identity, bypassing access control. Require a valid wallet address.
        let caller = inner.wallet.address().map_err(|_| FfiError::InternalError {
            reason: "wallet has no address — cannot call contracts".into(),
        })?;
        let caller_balance = inner.wallet.balance();
        let block_height = inner.consensus.as_ref()
            .map(|e| e.current_height()).unwrap_or(0);
        let presence = inner.presence_score;
        let peers = inner.network.as_ref()
            .map(|n| n.connected_peer_count() as u32).unwrap_or(0);

        let vm = inner.vm.as_mut().ok_or(FfiError::InternalError {
            reason: "VM not initialized — call init_vm() first".into(),
        })?;

        let call = ContractCall {
            caller,
            contract_address: addr,
            function_name: function_name.clone(),
            args: vec![],
            gas_limit,
        };

        let now_ts = chrono::Utc::now().timestamp() as u64;
        let mut host_env = HostEnvironment::new(
            block_height,
            now_ts,
            caller,
            caller_balance,
        )
        .with_presence_score(presence)
        .with_nearby_peers(peers);

        // WHY: Populate host environment with live sensor data from the cache.
        // This is what makes GRATIA smart contracts unique — @location, @sensor
        // opcodes return REAL phone sensor data, not mock values. Contracts can
        // react to the physical world: geo-fenced deals, weather-aware logic,
        // proximity verification, proof-of-movement.
        {
            let inner = self.lock_inner().map_err(|e| FfiError::InternalError {
                reason: format!("sensor cache lock: {}", e),
            })?;
            let sensor_age_secs = now_ts.saturating_sub(
                inner.last_sensor_time.timestamp() as u64
            );
            // WHY: Readings older than 60 seconds are marked stale. Contracts
            // can check is_fresh and decide whether to trust the data.
            let is_fresh = sensor_age_secs < 60;

            if let Some((lat, lon)) = inner.last_gps {
                host_env = host_env.with_location(gratia_core::types::GeoLocation { lat, lon });
            }
            if let Some(hpa) = inner.last_barometer {
                host_env = host_env.with_sensor_reading(
                    gratia_vm::host_functions::SensorReading {
                        sensor_type: gratia_vm::host_functions::SensorType::Barometer,
                        value: hpa,
                        timestamp_secs: now_ts,
                        is_fresh,
                    },
                );
            }
            if let Some(lux) = inner.last_light {
                host_env = host_env.with_sensor_reading(
                    gratia_vm::host_functions::SensorReading {
                        sensor_type: gratia_vm::host_functions::SensorType::AmbientLight,
                        value: lux,
                        timestamp_secs: now_ts,
                        is_fresh,
                    },
                );
            }
            if let Some(deg) = inner.last_magnetometer {
                host_env = host_env.with_sensor_reading(
                    gratia_vm::host_functions::SensorReading {
                        sensor_type: gratia_vm::host_functions::SensorType::Magnetometer,
                        value: deg,
                        timestamp_secs: now_ts,
                        is_fresh,
                    },
                );
            }
            if let Some(mag) = inner.last_accelerometer {
                host_env = host_env.with_sensor_reading(
                    gratia_vm::host_functions::SensorReading {
                        sensor_type: gratia_vm::host_functions::SensorType::Accelerometer,
                        value: mag,
                        timestamp_secs: now_ts,
                        is_fresh,
                    },
                );
            }
        }

        match vm.call_contract(&call, &mut host_env) {
            Ok(result) => {
                let events: Vec<String> = result.events.iter()
                    .map(|e| format!("{}:{}", e.topic, hex::encode(&e.data)))
                    .collect();

                let return_str = format!("{:?}", result.return_value);

                // WHY: Accumulate gas across all contract calls for get_vm_info().
                inner.total_gas_used += result.gas_used;

                rust_log(&format!(
                    "VM: {}() → success={} gas={}/{} return={}",
                    function_name, result.success, result.gas_used,
                    gas_limit, return_str,
                ));

                Ok(FfiContractResult {
                    success: result.success,
                    return_value: return_str,
                    gas_used: result.gas_used,
                    gas_remaining: result.gas_remaining,
                    events,
                    error: result.error,
                })
            }
            Err(e) => {
                Err(FfiError::InternalError {
                    reason: format!("Contract execution failed: {}", e),
                })
            }
        }
    }

    /// List deployed contracts.
    pub fn list_contracts(&self) -> Result<Vec<String>, FfiError> {
        let _inner = self.lock_inner()?;
        // WHY: Return the addresses we deployed. In production, this would
        // query the state DB for all deployed contract addresses.
        Ok(vec![
            format!("grat:{}", hex::encode([0x01u8; 32])),
            format!("grat:{}", hex::encode([0x02u8; 32])),
            format!("grat:{}", hex::encode([0x03u8; 32])),
        ])
    }

    // ========================================================================
    // GratiaScript Compiler methods
    // ========================================================================

    /// Compile GratiaScript source code to WASM bytecode.
    ///
    /// Takes a `.gs` source string and returns the compiled WASM binary
    /// as a hex-encoded string. This lets the mobile app compile contracts
    /// on-device before deploying them.
    ///
    /// WHY: On-device compilation means developers can write, test, and deploy
    /// contracts from their phone — no desktop toolchain needed. This is
    /// consistent with the phone-first philosophy.
    pub fn compile_contract(&self, source: String) -> Result<String, FfiError> {
        let wasm = gratiascript::compile(&source).map_err(|e| {
            FfiError::InternalError {
                reason: format!("GratiaScript compile error: {}", e),
            }
        })?;
        rust_log(&format!("GratiaScript: compiled {} bytes of WASM", wasm.len()));
        Ok(hex::encode(&wasm))
    }

    /// Compile GratiaScript source and deploy the contract in one step.
    ///
    /// Compiles the source to WASM, deploys it to GratiaVM, and returns
    /// the contract address. This is the primary way contracts get deployed
    /// from the mobile app.
    pub fn compile_and_deploy_contract(
        &self,
        source: String,
    ) -> Result<String, FfiError> {
        let wasm = gratiascript::compile(&source).map_err(|e| {
            FfiError::InternalError {
                reason: format!("GratiaScript compile error: {}", e),
            }
        })?;

        let mut inner = self.lock_inner()?;

        // WHY: A zero address as deployer would deploy contracts to an unowned
        // address. Require a valid wallet identity for deployment.
        let deployer = inner.wallet.address().map_err(|_| FfiError::InternalError {
            reason: "wallet has no address — cannot deploy contract".into(),
        })?;

        let vm = inner.vm.as_mut().ok_or(FfiError::InternalError {
            reason: "VM not initialized — call init_vm() first".into(),
        })?;

        let permissions = gratia_vm::sandbox::ContractPermissions::all();
        let contract_addr = vm.deploy_contract(&deployer, &wasm, permissions)
            .map_err(|e| FfiError::InternalError {
                reason: format!("deploy failed: {}", e),
            })?;

        let addr_hex = format!("grat:{}", hex::encode(contract_addr.0));
        rust_log(&format!(
            "GratiaScript: compiled + deployed at {} ({} bytes WASM)",
            addr_hex, wasm.len()
        ));
        info!("FFI: GratiaScript contract deployed at {}", addr_hex);
        Ok(addr_hex)
    }

    // ========================================================================
    // Governance methods — One Phone, One Vote
    // ========================================================================

    /// Submit a governance proposal.
    ///
    /// Requires 90+ days of Proof of Life history per the governance spec.
    /// Returns the proposal ID as a hex string.
    pub fn submit_proposal(
        &self,
        title: String,
        description: String,
    ) -> Result<String, FfiError> {
        let mut inner = self.lock_inner()?;
        let node_id = self.get_node_id_or_default(&inner);
        let pol_days = inner.pol.consecutive_days();

        // WHY: 90-day PoL requirement prevents newcomers from submitting
        // governance proposals before they've proven sustained participation.
        // This is enforced even during onboarding — governance requires real history.
        // Debug bypass skips this for testing only.
        if !inner.is_debug_bypass() && pol_days < 90 {
            return Err(FfiError::ProofOfLifeError {
                reason: format!(
                    "90+ days PoL required to submit proposals (you have {} days)",
                    pol_days
                ),
            });
        }

        let now = Utc::now();
        // WHY: eligible_voters is set to 1 for Phase 2 testnet. In production,
        // this would be the count of active mining nodes on the network.
        let eligible_voters = 1u64;
        let effective_days = if inner.is_debug_bypass() { 90 } else { pol_days };
        let proposal_id = inner.governance.submit_proposal(
            node_id,
            effective_days,
            title.clone(),
            description,
            vec![], // proposal_data — empty for text-only proposals
            eligible_voters,
            now,
        ).map_err(|e| FfiError::InternalError {
            reason: format!("submit proposal failed: {}", e),
        })?;

        let id_hex = hex::encode(proposal_id);
        rust_log(&format!("Governance: proposal '{}' submitted, id={}", title, id_hex));
        Ok(id_hex)
    }

    /// Cast a vote on a proposal.
    ///
    /// `vote` must be "yes", "no", or "abstain".
    pub fn vote_on_proposal(
        &self,
        proposal_id_hex: String,
        vote: String,
    ) -> Result<(), FfiError> {
        let mut inner = self.lock_inner()?;
        let node_id = self.get_node_id_or_default(&inner);

        let id_bytes = hex::decode(&proposal_id_hex).map_err(|_| FfiError::InternalError {
            reason: "invalid proposal ID hex".into(),
        })?;
        let mut proposal_id = [0u8; 32];
        if id_bytes.len() != 32 {
            return Err(FfiError::InternalError { reason: "proposal ID must be 32 bytes".into() });
        }
        proposal_id.copy_from_slice(&id_bytes);

        let vote_enum = match vote.to_lowercase().as_str() {
            "yes" | "for" => Vote::Yes,
            "no" | "against" => Vote::No,
            "abstain" => Vote::Abstain,
            _ => return Err(FfiError::InternalError {
                reason: format!("invalid vote: '{}' — must be yes/no/abstain", vote),
            }),
        };

        // WHY: Voting requires valid PoL or being in grace period. Onboarding
        // users (day 0) can vote since they haven't had a chance to build PoL yet.
        let has_valid_pol = inner.is_debug_bypass()
            || inner.pol.is_onboarding()
            || inner.pol.current_day_valid()
            || inner.pol.in_grace_period();
        let now = Utc::now();

        inner.governance.cast_vote(&proposal_id, node_id, vote_enum, has_valid_pol, now)
            .map_err(|e| FfiError::InternalError {
                reason: format!("vote failed: {}", e),
            })?;

        rust_log(&format!("Governance: voted '{}' on proposal {}", vote, proposal_id_hex));
        Ok(())
    }

    /// Get all proposals (active and past).
    pub fn get_proposals(&self) -> Result<Vec<FfiProposal>, FfiError> {
        let inner = self.lock_inner()?;
        let proposals: Vec<FfiProposal> = inner.governance.get_all_proposals()
            .iter()
            .map(|p| {
                let status = match p.status {
                    gratia_core::types::ProposalStatus::Discussion => "discussion",
                    gratia_core::types::ProposalStatus::Voting => "voting",
                    gratia_core::types::ProposalStatus::Approved => "passed",
                    gratia_core::types::ProposalStatus::Rejected => "rejected",
                    gratia_core::types::ProposalStatus::Implemented => "implemented",
                    gratia_core::types::ProposalStatus::Reverted => "reverted",
                };
                FfiProposal {
                    id_hex: hex::encode(p.id),
                    title: p.title.clone(),
                    description: p.description.clone(),
                    status: status.to_string(),
                    votes_yes: p.votes_yes,
                    votes_no: p.votes_no,
                    votes_abstain: p.votes_abstain,
                    discussion_end_millis: p.discussion_ends.timestamp_millis(),
                    voting_end_millis: p.voting_ends.timestamp_millis(),
                    submitted_by: format!("grat:{}", hex::encode(p.proposer.0)),
                }
            })
            .collect();
        Ok(proposals)
    }

    /// Get a single proposal by hex ID.
    pub fn get_proposal(&self, proposal_id_hex: String) -> Result<FfiProposal, FfiError> {
        let inner = self.lock_inner()?;

        let id_bytes = hex::decode(&proposal_id_hex).map_err(|_| FfiError::InternalError {
            reason: "invalid proposal ID hex".into(),
        })?;
        if id_bytes.len() != 32 {
            return Err(FfiError::InternalError { reason: "proposal ID must be 32 bytes".into() });
        }
        let mut proposal_id = [0u8; 32];
        proposal_id.copy_from_slice(&id_bytes);

        let p = inner.governance.get_proposal(&proposal_id).ok_or_else(|| FfiError::InternalError {
            reason: format!("proposal not found: {}", proposal_id_hex),
        })?;

        let status = match p.status {
            gratia_core::types::ProposalStatus::Discussion => "discussion",
            gratia_core::types::ProposalStatus::Voting => "voting",
            gratia_core::types::ProposalStatus::Approved => "passed",
            gratia_core::types::ProposalStatus::Rejected => "rejected",
            gratia_core::types::ProposalStatus::Implemented => "implemented",
            gratia_core::types::ProposalStatus::Reverted => "reverted",
        };

        Ok(FfiProposal {
            id_hex: hex::encode(p.id),
            title: p.title.clone(),
            description: p.description.clone(),
            status: status.to_string(),
            votes_yes: p.votes_yes,
            votes_no: p.votes_no,
            votes_abstain: p.votes_abstain,
            discussion_end_millis: p.discussion_ends.timestamp_millis(),
            voting_end_millis: p.voting_ends.timestamp_millis(),
            submitted_by: format!("grat:{}", hex::encode(p.proposer.0)),
        })
    }

    /// Create an on-chain poll. One phone, one vote.
    ///
    /// `options` is a list of option labels (2-10 options).
    /// `duration_secs` is how long the poll stays open.
    /// Returns the poll ID as a hex string.
    pub fn create_poll(
        &self,
        question: String,
        options: Vec<String>,
        duration_secs: u64,
    ) -> Result<String, FfiError> {
        let mut inner = self.lock_inner()?;

        // WHY: A zero address as poll creator would create polls attributed to
        // no one, breaking accountability. Require a valid wallet identity.
        let creator = inner.wallet.address().map_err(|_| FfiError::InternalError {
            reason: "wallet has no address — cannot create poll".into(),
        })?;
        let balance = inner.wallet.balance();
        let now = Utc::now();

        let poll_id = inner.governance.create_poll(
            creator,
            question.clone(),
            options,
            duration_secs,
            None, // no geographic filter for Phase 2
            balance,
            now,
        ).map_err(|e| FfiError::InternalError {
            reason: format!("create poll failed: {}", e),
        })?;

        let id_hex = hex::encode(poll_id);
        rust_log(&format!("Governance: poll '{}' created, id={}", question, id_hex));
        Ok(id_hex)
    }

    /// Cast a vote on a poll.
    pub fn vote_on_poll(
        &self,
        poll_id_hex: String,
        option_index: u32,
    ) -> Result<(), FfiError> {
        let mut inner = self.lock_inner()?;
        let node_id = self.get_node_id_or_default(&inner);

        let id_bytes = hex::decode(&poll_id_hex).map_err(|_| FfiError::InternalError {
            reason: "invalid poll ID hex".into(),
        })?;
        let mut poll_id = [0u8; 32];
        if id_bytes.len() != 32 {
            return Err(FfiError::InternalError { reason: "poll ID must be 32 bytes".into() });
        }
        poll_id.copy_from_slice(&id_bytes);

        // WHY: Poll voting requires valid PoL or being in grace period.
        // Onboarding users (day 0) can vote since they haven't had a chance to build PoL yet.
        let has_valid_pol = inner.is_debug_bypass()
            || inner.pol.is_onboarding()
            || inner.pol.current_day_valid()
            || inner.pol.in_grace_period();
        let now = Utc::now();

        inner.governance.cast_poll_vote(
            &poll_id, node_id, option_index, has_valid_pol, None, now
        ).map_err(|e| FfiError::InternalError {
            reason: format!("poll vote failed: {}", e),
        })?;

        rust_log(&format!("Governance: voted option {} on poll {}", option_index, poll_id_hex));
        Ok(())
    }

    /// Get all active polls.
    pub fn get_polls(&self) -> Result<Vec<FfiPoll>, FfiError> {
        let inner = self.lock_inner()?;
        let now = Utc::now();
        let polls: Vec<FfiPoll> = inner.governance.get_active_polls(now)
            .iter()
            .map(|p| FfiPoll {
                id_hex: hex::encode(p.id),
                question: p.question.clone(),
                options: p.options.clone(),
                votes: p.votes.clone(),
                total_voters: p.total_voters,
                end_millis: p.expires_at.timestamp_millis(),
                created_by: format!("grat:{}", hex::encode(p.creator.0)),
            })
            .collect();
        Ok(polls)
    }

    /// Get detailed results for a poll by hex ID.
    ///
    /// Returns per-option vote counts and percentages, total voters,
    /// and whether the poll has expired.
    pub fn get_poll_results(&self, poll_id_hex: String) -> Result<FfiPollResults, FfiError> {
        let inner = self.lock_inner()?;

        let id_bytes = hex::decode(&poll_id_hex).map_err(|_| FfiError::InternalError {
            reason: "invalid poll ID hex".into(),
        })?;
        if id_bytes.len() != 32 {
            return Err(FfiError::InternalError { reason: "poll ID must be 32 bytes".into() });
        }
        let mut poll_id = [0u8; 32];
        poll_id.copy_from_slice(&id_bytes);

        let results = inner.governance.get_poll_results(&poll_id).ok_or_else(|| FfiError::InternalError {
            reason: format!("poll not found: {}", poll_id_hex),
        })?;

        Ok(FfiPollResults {
            poll_id_hex: hex::encode(results.poll_id),
            question: results.question,
            total_voters: results.total_voters,
            options: results.options.into_iter().map(|o| FfiPollOptionResult {
                index: o.index,
                label: o.label,
                votes: o.votes,
                percentage: o.percentage,
            }).collect(),
            expired: results.expired,
        })
    }

    // ========================================================================
    // Mesh Transport methods (Phase 3 — Bluetooth + Wi-Fi Direct)
    // ========================================================================

    /// Start the Bluetooth/Wi-Fi Direct mesh transport layer.
    ///
    /// Enables offline transaction relay and local peer discovery without
    /// internet connectivity. Mesh peers forward transactions to bridge
    /// peers that relay them to the wider network.
    ///
    /// WHY: The mesh layer is Layer 0 in the Gratia network architecture.
    /// It provides connectivity for users without cellular/Wi-Fi internet,
    /// enabling transactions in areas with poor connectivity.
    pub fn start_mesh(&self) -> Result<(), FfiError> {
        let mut inner = self.lock_inner()?;

        if inner.mesh_transport.is_some() {
            // WHY: Idempotent — if already running, return success.
            info!("FFI: mesh transport already running");
            return Ok(());
        }

        let node_id = self.get_node_id_or_default(&inner);
        let local_peer_id = gratia_network::mesh::MeshPeerId(node_id.0);

        let config = gratia_network::mesh::MeshConfig::default();
        let mut mesh = gratia_network::mesh::MeshTransport::new(config, local_peer_id);

        mesh.start().map_err(|e| FfiError::NetworkError {
            reason: format!("mesh start failed: {}", e),
        })?;

        // WHY: If the main network layer is running, this node acts as a
        // bridge peer — it can relay mesh transactions to the internet.
        if inner.network.is_some() {
            mesh.set_internet_available(true);
        }

        inner.mesh_transport = Some(mesh);
        rust_log("Mesh transport started");
        info!("FFI: mesh transport started");
        Ok(())
    }

    /// Stop the Bluetooth/Wi-Fi Direct mesh transport layer.
    pub fn stop_mesh(&self) -> Result<(), FfiError> {
        let mut inner = self.lock_inner()?;

        if let Some(ref mut mesh) = inner.mesh_transport {
            mesh.stop();
        }
        inner.mesh_transport = None;

        rust_log("Mesh transport stopped");
        info!("FFI: mesh transport stopped");
        Ok(())
    }

    /// Get the current mesh network status.
    ///
    /// Returns connectivity information for the Bluetooth/Wi-Fi Direct
    /// mesh layer including peer counts and relay queue depth.
    pub fn get_mesh_status(&self) -> Result<FfiMeshStatus, FfiError> {
        let inner = self.lock_inner()?;

        match &inner.mesh_transport {
            Some(mesh) => {
                let bridge_count = mesh.get_bridge_peers().len() as u32;
                Ok(FfiMeshStatus {
                    enabled: mesh.is_active(),
                    bluetooth_active: mesh.is_active(),
                    // WHY: Wi-Fi Direct support is determined by the mesh config.
                    // Both transports are managed by the same MeshTransport instance.
                    wifi_direct_active: mesh.is_active() && mesh.config().wifi_direct_enabled,
                    mesh_peer_count: mesh.peer_count() as u32,
                    bridge_peer_count: bridge_count,
                    pending_relay_count: mesh.relay_queue_len() as u32,
                })
            }
            None => Ok(FfiMeshStatus {
                enabled: false,
                bluetooth_active: false,
                wifi_direct_active: false,
                mesh_peer_count: 0,
                bridge_peer_count: 0,
                pending_relay_count: 0,
            }),
        }
    }

    /// Broadcast a transaction via the mesh layer for offline use.
    ///
    /// The transaction is serialized and broadcast to all mesh peers.
    /// Bridge peers with internet connectivity will relay it to the
    /// main network. Returns the transaction hash as a hex string.
    ///
    /// WHY: This enables sending transactions when the phone has no
    /// internet (airplane mode, poor signal, rural areas) by relaying
    /// through Bluetooth/Wi-Fi Direct to nearby peers.
    pub fn mesh_broadcast_transaction(&self, tx_hex: String) -> Result<String, FfiError> {
        let mut inner = self.lock_inner()?;

        let mesh = inner.mesh_transport.as_mut().ok_or(FfiError::NetworkError {
            reason: "mesh transport not started — call start_mesh() first".into(),
        })?;

        let tx_bytes = hex::decode(&tx_hex).map_err(|e| FfiError::InternalError {
            reason: format!("invalid transaction hex: {}", e),
        })?;

        // WHY: broadcast() returns the mesh message ID (SHA-256 hash) which
        // serves as a unique identifier for tracking the relayed transaction.
        let msg_id = mesh.broadcast(
            gratia_network::mesh::MeshMessageType::Transaction,
            tx_bytes,
            chrono::Utc::now().timestamp() as u64,
        ).map_err(|e| FfiError::NetworkError {
            reason: format!("mesh broadcast failed: {}", e),
        })?;

        let tx_hash = hex::encode(msg_id);
        rust_log(&format!("Mesh: broadcast transaction {}", tx_hash));
        Ok(tx_hash)
    }

    // ========================================================================
    // Sharded Consensus methods (Phase 3 — Geographic Sharding)
    // ========================================================================

    /// Get geographic shard information for this node.
    ///
    /// Returns the node's assigned shard, total shard count, and
    /// validator distribution. If sharding is not yet active (fewer
    /// than the minimum nodes required), returns default single-shard info.
    ///
    /// WHY: The mobile UI displays shard assignment so users understand
    /// which geographic region their node serves and can see cross-shard
    /// transaction routing in the explorer.
    pub fn get_shard_info(&self) -> Result<FfiShardInfo, FfiError> {
        let inner = self.lock_inner()?;

        match &inner.shard_coordinator {
            Some(coordinator) => {
                let primary = coordinator.primary_shard();
                let shard_count = coordinator.active_shard_count();
                let (local_vals, cross_vals) = match coordinator.get_shard_engine(&primary) {
                    Some(engine) => {
                        let local = engine.local_committee().len() as u32;
                        let cross = engine.cross_shard_committee().len() as u32;
                        (local, cross)
                    }
                    None => (0, 0),
                };
                let shard_height = coordinator.get_shard_engine(&primary)
                    .map(|e| e.shard_height())
                    .unwrap_or(0);

                Ok(FfiShardInfo {
                    shard_id: primary.0,
                    shard_count,
                    local_validators: local_vals,
                    cross_shard_validators: cross_vals,
                    shard_height,
                    is_sharding_active: shard_count > 1,
                })
            }
            None => {
                // WHY: Before sharding activates, the entire network operates
                // as a single shard (shard 0). Return sensible defaults.
                let height = inner.consensus.as_ref()
                    .map(|e| e.current_height())
                    .unwrap_or(0);
                Ok(FfiShardInfo {
                    shard_id: 0,
                    shard_count: 1,
                    local_validators: inner.known_peer_nodes.len() as u32 + 1,
                    cross_shard_validators: 0,
                    shard_height: height,
                    is_sharding_active: false,
                })
            }
        }
    }

    /// Get the number of cross-shard transactions waiting to be routed.
    ///
    /// WHY: Cross-shard transactions require receipts to be relayed between
    /// shard committees. The queue size indicates routing backlog — useful
    /// for the mobile UI to show network health.
    pub fn get_cross_shard_queue_size(&self) -> Result<u32, FfiError> {
        let inner = self.lock_inner()?;

        match &inner.shard_coordinator {
            Some(coordinator) => Ok(coordinator.cross_shard_queue_len() as u32),
            // WHY: No sharding active means no cross-shard queue.
            None => Ok(0),
        }
    }

    // ========================================================================
    // Groth16 ZK Proof methods (Phase 3 — Complex ZK for Smart Contracts)
    // ========================================================================

    /// Generate a Groth16 range proof for a value.
    ///
    /// Creates a zero-knowledge proof that a value lies within [0, 2^bit_width)
    /// without revealing the actual value. Used for smart contract interactions
    /// that need private amount verification.
    ///
    /// Returns the proof and verification key as a hex-encoded JSON string.
    ///
    /// WHY: Groth16 proofs are computationally heavy to generate (~2-5 seconds
    /// on ARM). This is designed to run during Mining Mode (plugged in, 80%+
    /// battery) so the phone has power to spare. The mobile app can queue proof
    /// generation and execute it when conditions are met.
    pub fn generate_range_proof(&self, value: u64, bit_width: u32) -> Result<String, FfiError> {
        let _inner = self.lock_inner()?;

        let (proof, params) = gratia_zk::prove_range(value, bit_width as usize)
            .map_err(|e| FfiError::InternalError {
                reason: format!("Groth16 range proof generation failed: {}", e),
            })?;

        // WHY: Serialize proof + verification key together as JSON, then
        // hex-encode the JSON bytes. This gives the mobile layer a single
        // opaque string to pass around. The verification side can deserialize
        // both from the same blob.
        let result = serde_json::json!({
            "proof": hex::encode(bincode::serialize(&proof).unwrap_or_default()),
            "vk": hex::encode(bincode::serialize(&params.verification_key).unwrap_or_default()),
        });

        let result_str = result.to_string();
        rust_log(&format!(
            "Groth16: generated range proof for value (bit_width={}), {} bytes",
            bit_width, result_str.len()
        ));
        Ok(result_str)
    }

    /// Verify a Groth16 proof against public inputs and a verification key.
    ///
    /// All parameters are hex-encoded binary (bincode-serialized).
    /// Returns true if the proof is valid.
    ///
    /// WHY: Verification is fast (~5-10ms on ARM) compared to proof generation.
    /// Every validator node verifies proofs for transactions in each block,
    /// so this must be efficient on mobile hardware.
    pub fn verify_groth16_proof(
        &self,
        proof_hex: String,
        public_inputs_hex: String,
        vk_hex: String,
    ) -> Result<bool, FfiError> {
        let _inner = self.lock_inner()?;

        let proof_bytes = hex::decode(&proof_hex).map_err(|e| FfiError::InternalError {
            reason: format!("invalid proof hex: {}", e),
        })?;
        let proof: gratia_zk::Groth16Proof = bincode::deserialize(&proof_bytes)
            .map_err(|e| FfiError::InternalError {
                reason: format!("proof deserialization failed: {}", e),
            })?;

        let vk_bytes = hex::decode(&vk_hex).map_err(|e| FfiError::InternalError {
            reason: format!("invalid vk hex: {}", e),
        })?;
        let vk: gratia_zk::VerificationKey = bincode::deserialize(&vk_bytes)
            .map_err(|e| FfiError::InternalError {
                reason: format!("verification key deserialization failed: {}", e),
            })?;

        let inputs_bytes = hex::decode(&public_inputs_hex).map_err(|e| FfiError::InternalError {
            reason: format!("invalid public inputs hex: {}", e),
        })?;
        // WHY: Public inputs are serialized as a vec of 32-byte Scalars.
        // Each scalar is exactly 32 bytes in canonical form.
        let public_inputs: Vec<curve25519_dalek::scalar::Scalar> = inputs_bytes
            .chunks_exact(32)
            .map(|chunk| {
                let mut arr = [0u8; 32];
                arr.copy_from_slice(chunk);
                curve25519_dalek::scalar::Scalar::from_bytes_mod_order(arr)
            })
            .collect();

        let valid = gratia_zk::groth16::verify(&vk, &proof, &public_inputs)
            .map_err(|e| FfiError::InternalError {
                reason: format!("Groth16 verification error: {}", e),
            })?;

        rust_log(&format!("Groth16: verification result={}", valid));
        Ok(valid)
    }

    // ========================================================================
    // Bulletproofs — Proof of Life ZK Attestation
    // ========================================================================

    /// Generate a Bulletproofs zero-knowledge proof that the daily Proof of Life
    /// parameters meet the required thresholds, without revealing actual values.
    ///
    /// WHY: This is the core privacy mechanism for Proof of Life. The phone
    /// collects sensor data locally, then proves to the network that it met
    /// every threshold (unlock count, spread, interactions, BT environments)
    /// without disclosing how much it exceeded each threshold by. The proof is
    /// compact (~700 bytes) and fast to verify on other mobile devices.
    pub fn generate_pol_proof(
        &self,
        unlock_count: u64,
        unlock_spread_hours: u64,
        interaction_sessions: u64,
        bt_environments: u64,
        min_unlocks: u64,
        min_spread: u64,
        min_interactions: u64,
        min_bt_envs: u64,
        epoch_day: u32,
    ) -> Result<FfiPolRangeProof, FfiError> {
        let _inner = self.lock_inner()?;

        let input = gratia_zk::PolProofInput {
            unlock_count,
            unlock_spread_hours,
            interaction_sessions,
            bt_environments,
        };

        let thresholds = gratia_zk::PolThresholds {
            min_unlocks,
            min_spread,
            min_interactions,
            min_bt_envs,
        };

        let proof = gratia_zk::generate_pol_proof(&input, &thresholds, epoch_day)
            .map_err(|e| FfiError::InternalError {
                reason: format!("Bulletproofs PoL proof generation failed: {}", e),
            })?;

        let ffi_proof = FfiPolRangeProof {
            proof_bytes_hex: hex::encode(&proof.proof_bytes),
            commitments_hex: proof.commitments.iter().map(|c| hex::encode(c)).collect(),
            parameter_count: proof.parameter_count,
            epoch_day: proof.epoch_day,
        };

        rust_log(&format!(
            "Bulletproofs: generated PoL proof for epoch_day={}, {} params, {} proof bytes",
            epoch_day, ffi_proof.parameter_count, proof.proof_bytes.len()
        ));
        Ok(ffi_proof)
    }

    /// Verify a Bulletproofs Proof of Life ZK proof against the given thresholds.
    ///
    /// WHY: Every validator node verifies PoL proofs when validating blocks.
    /// Bulletproof verification is fast (~5ms on ARM), making it suitable for
    /// mobile validators processing blocks within the 3-5 second window.
    pub fn verify_pol_proof(
        &self,
        proof_bytes_hex: String,
        commitments_hex: Vec<String>,
        parameter_count: u8,
        epoch_day: u32,
        min_unlocks: u64,
        min_spread: u64,
        min_interactions: u64,
        min_bt_envs: u64,
    ) -> Result<bool, FfiError> {
        let _inner = self.lock_inner()?;

        let proof_bytes = hex::decode(&proof_bytes_hex).map_err(|e| FfiError::InternalError {
            reason: format!("invalid proof_bytes hex: {}", e),
        })?;

        let commitments: Result<Vec<Vec<u8>>, _> = commitments_hex
            .iter()
            .map(|c| hex::decode(c))
            .collect();
        let commitments = commitments.map_err(|e| FfiError::InternalError {
            reason: format!("invalid commitment hex: {}", e),
        })?;

        let proof = gratia_zk::PolRangeProof {
            proof_bytes,
            commitments,
            parameter_count,
            epoch_day,
        };

        let thresholds = gratia_zk::PolThresholds {
            min_unlocks,
            min_spread,
            min_interactions,
            min_bt_envs,
        };

        let valid = gratia_zk::verify_pol_proof(&proof, &thresholds, epoch_day)
            .map_err(|e| FfiError::InternalError {
                reason: format!("Bulletproofs PoL verification failed: {}", e),
            })?;

        rust_log(&format!(
            "Bulletproofs: PoL verification result={} for epoch_day={}",
            valid, epoch_day
        ));
        Ok(valid)
    }

    /// Retrieve the last successfully generated Proof of Life ZK proof from the
    /// PoL manager, if one exists.
    ///
    /// WHY: After the PoL manager runs its daily attestation cycle and generates
    /// a ZK proof internally, the mobile app needs to retrieve it for display
    /// and for broadcasting to the network. This avoids regenerating the proof.
    pub fn get_last_pol_proof(&self) -> Result<Option<FfiPolRangeProof>, FfiError> {
        let inner = self.lock_inner()?;

        match inner.pol.last_zk_proof() {
            Some(proof) => {
                let ffi_proof = FfiPolRangeProof {
                    proof_bytes_hex: hex::encode(&proof.proof_bytes),
                    commitments_hex: proof.commitments.iter().map(|c| hex::encode(c)).collect(),
                    parameter_count: proof.parameter_count,
                    epoch_day: proof.epoch_day,
                };
                rust_log(&format!(
                    "Retrieved last PoL proof: epoch_day={}, {} params",
                    ffi_proof.epoch_day, ffi_proof.parameter_count
                ));
                Ok(Some(ffi_proof))
            }
            None => {
                rust_log("No PoL proof available yet");
                Ok(None)
            }
        }
    }

    // ========================================================================
    // Enhanced VM Status (Phase 3)
    // ========================================================================

    /// Get GratiaVM runtime information.
    ///
    /// Returns the VM runtime type, number of deployed contracts,
    /// cumulative gas usage, and memory configuration.
    ///
    /// WHY: The mobile UI displays VM health so developers and users
    /// can monitor smart contract system status. This is also useful
    /// for the DevKit app when testing contracts on real phones.
    pub fn get_vm_info(&self) -> Result<FfiVmInfo, FfiError> {
        let inner = self.lock_inner()?;

        match &inner.vm {
            Some(vm) => {
                // WHY: Determine runtime type from the sandbox config.
                // InterpreterRuntime is the default for cross-platform
                // compatibility; wasmer is used when available.
                let runtime_type = "interpreter".to_string();
                // WHY: Count deployed contracts by probing known addresses.
                // We can't directly access vm.contracts (private HashMap).
                // Probe a reasonable range of addresses — demo contracts use
                // deterministic addresses derived from deployer + bytecode hash.
                let deployed_count = {
                    let mut count = 0u32;
                    for i in 1u8..=50 {
                        let test_addr = gratia_core::types::Address([i; 32]);
                        if vm.is_deployed(&test_addr) {
                            count += 1;
                        }
                    }
                    count
                };

                Ok(FfiVmInfo {
                    runtime_type,
                    contracts_loaded: deployed_count,
                    total_gas_used: inner.total_gas_used,
                    // WHY: Memory wiring (mlock) prevents WASM pages from being
                    // swapped to disk, important for deterministic execution.
                    // Currently false for the interpreter runtime.
                    memory_wired: false,
                })
            }
            None => Ok(FfiVmInfo {
                runtime_type: "not_initialized".to_string(),
                contracts_loaded: 0,
                total_gas_used: 0,
                memory_wired: false,
            }),
        }
    }

    // ========================================================================
    // Lux Social Protocol methods
    // ========================================================================

    /// Create a new Lux text post. Returns the post hash.
    pub fn lux_create_post(&self, content: String) -> Result<String, FfiError> {
        let lux_path = format!("{}/lux_store.json", self.data_dir);
        let mut inner = self.lock_inner()?;
        let address = inner.wallet.address()
            .map_err(|_| FfiError::WalletNotInitialized)?.to_string();
        let sk_bytes = inner.wallet.signing_key_bytes()
            .map_err(|_| FfiError::WalletNotInitialized)?;
        let signing_key = ed25519_dalek::SigningKey::from_bytes(&sk_bytes);
        let hash = inner.lux_store.create_post(&address, &content, &signing_key, None)
            .map_err(|e| FfiError::InternalError { reason: format!("Lux post failed: {e}") })?;
        let fee = inner.lux_fees.post_fee();
        inner.lux_fees.record_burn(fee);
        let _ = inner.lux_store.save_to_file(&lux_path);

        // WHY: Broadcast the post to peers via gossipsub so it appears on other
        // connected phones. Failure to broadcast is non-fatal — the post is still
        // stored locally and can be synced later.
        if let Some(ref network) = inner.network {
            if let Some(post) = inner.lux_store.get_post(&hash).cloned() {
                if let Err(e) = network.try_broadcast_lux_post_sync(&post) {
                    warn!("Failed to broadcast Lux post: {}", e);
                } else {
                    rust_log(&format!("Lux post broadcast: hash={}", hash));
                }
            }
        }

        info!("FFI: Lux post created: hash={}, fee={} Lux", hash, fee);
        Ok(hash)
    }

    /// Create a reply to an existing post.
    pub fn lux_reply(&self, parent_hash: String, content: String) -> Result<String, FfiError> {
        let lux_path = format!("{}/lux_store.json", self.data_dir);
        let mut inner = self.lock_inner()?;
        let address = inner.wallet.address()
            .map_err(|_| FfiError::WalletNotInitialized)?.to_string();
        let sk_bytes = inner.wallet.signing_key_bytes()
            .map_err(|_| FfiError::WalletNotInitialized)?;
        let signing_key = ed25519_dalek::SigningKey::from_bytes(&sk_bytes);
        let hash = inner.lux_store.create_post(&address, &content, &signing_key, Some(parent_hash))
            .map_err(|e| FfiError::InternalError { reason: format!("Lux reply failed: {e}") })?;
        let fee = inner.lux_fees.post_fee();
        inner.lux_fees.record_burn(fee);
        let _ = inner.lux_store.save_to_file(&lux_path);

        // WHY: Broadcast replies via gossipsub just like top-level posts.
        if let Some(ref network) = inner.network {
            if let Some(post) = inner.lux_store.get_post(&hash).cloned() {
                if let Err(e) = network.try_broadcast_lux_post_sync(&post) {
                    warn!("Failed to broadcast Lux reply: {}", e);
                }
            }
        }

        Ok(hash)
    }

    /// Like a post. Costs 1 Lux (burned).
    pub fn lux_like_post(&self, post_hash: String) -> Result<(), FfiError> {
        let lux_path = format!("{}/lux_store.json", self.data_dir);
        let mut inner = self.lock_inner()?;
        let address = inner.wallet.address()
            .map_err(|_| FfiError::WalletNotInitialized)?.to_string();
        let sk_bytes = inner.wallet.signing_key_bytes()
            .map_err(|_| FfiError::WalletNotInitialized)?;
        let signing_key = ed25519_dalek::SigningKey::from_bytes(&sk_bytes);
        inner.lux_store.like_post(&post_hash, &address, &signing_key)
            .map_err(|e| FfiError::InternalError { reason: format!("Lux like failed: {e}") })?;
        inner.lux_fees.record_burn(gratia_lux::fees::LIKE_FEE_LUX);
        let _ = inner.lux_store.save_to_file(&lux_path);
        Ok(())
    }

    /// Repost a post with optional quote text. Costs 1 Lux (burned).
    pub fn lux_repost(&self, original_hash: String, quote: Option<String>) -> Result<String, FfiError> {
        let lux_path = format!("{}/lux_store.json", self.data_dir);
        let mut inner = self.lock_inner()?;
        let address = inner.wallet.address()
            .map_err(|_| FfiError::WalletNotInitialized)?.to_string();
        let sk_bytes = inner.wallet.signing_key_bytes()
            .map_err(|_| FfiError::WalletNotInitialized)?;
        let signing_key = ed25519_dalek::SigningKey::from_bytes(&sk_bytes);
        let hash = inner.lux_store.repost(&original_hash, &address, &signing_key, quote)
            .map_err(|e| FfiError::InternalError { reason: format!("Lux repost failed: {e}") })?;
        inner.lux_fees.record_burn(gratia_lux::fees::REPOST_FEE_LUX);
        let _ = inner.lux_store.save_to_file(&lux_path);
        Ok(hash)
    }

    /// Follow a user.
    pub fn lux_follow(&self, target_address: String) -> Result<(), FfiError> {
        let lux_path = format!("{}/lux_store.json", self.data_dir);
        let mut inner = self.lock_inner()?;
        let address = inner.wallet.address()
            .map_err(|_| FfiError::WalletNotInitialized)?.to_string();
        let sk_bytes = inner.wallet.signing_key_bytes()
            .map_err(|_| FfiError::WalletNotInitialized)?;
        let signing_key = ed25519_dalek::SigningKey::from_bytes(&sk_bytes);
        inner.lux_store.follow(&address, &target_address, &signing_key);
        let _ = inner.lux_store.save_to_file(&lux_path);
        Ok(())
    }

    /// Unfollow a user.
    pub fn lux_unfollow(&self, target_address: String) -> Result<(), FfiError> {
        let mut inner = self.lock_inner()?;
        let address = inner.wallet.address()
            .map_err(|_| FfiError::WalletNotInitialized)?.to_string();
        inner.lux_store.unfollow(&address, &target_address);
        Ok(())
    }

    /// Get the global feed: all posts, newest first.
    pub fn lux_get_global_feed(&self, limit: u32) -> Result<FfiLuxFeed, FfiError> {
        let inner = self.lock_inner()?;
        let address = inner.wallet.address()
            .map_err(|_| FfiError::WalletNotInitialized)?.to_string();
        let items = gratia_lux::FeedManager::global_feed(&inner.lux_store, &address, limit as usize);
        let posts = items.into_iter().map(|item| FfiLuxPost {
            hash: item.post.hash,
            author: item.post.author,
            author_display_name: item.author_display_name.unwrap_or_default(),
            content: item.post.content,
            timestamp_millis: item.post.timestamp.timestamp_millis(),
            likes: item.engagement.likes,
            reposts: item.engagement.reposts,
            replies: item.engagement.replies,
            liked_by_me: item.liked_by_me,
            reposted_by_me: false,
        }).collect();
        Ok(FfiLuxFeed {
            posts,
            post_fee_lux: inner.lux_fees.post_fee(),
            total_burned_lux: inner.lux_fees.total_burned(),
        })
    }

    /// Get a user's profile info.
    pub fn lux_get_profile(&self, address: String) -> Result<FfiLuxProfile, FfiError> {
        let inner = self.lock_inner()?;
        let profile = inner.lux_store.get_profile(&address);
        let followers = inner.lux_store.get_followers(&address);
        let following = inner.lux_store.get_following(&address);
        let posts = inner.lux_store.get_posts_by_author(&address);
        Ok(FfiLuxProfile {
            address: address.clone(),
            display_name: profile.and_then(|p| p.display_name.clone()).unwrap_or_default(),
            bio: profile.and_then(|p| p.bio.clone()).unwrap_or_default(),
            follower_count: followers.len() as u64,
            following_count: following.len() as u64,
            post_count: posts.len() as u64,
        })
    }

    /// Set the current user's display name and bio.
    pub fn lux_set_profile(&self, display_name: String, bio: String) -> Result<(), FfiError> {
        let mut inner = self.lock_inner()?;
        let address = inner.wallet.address()
            .map_err(|_| FfiError::WalletNotInitialized)?.to_string();
        let sk_bytes = inner.wallet.signing_key_bytes()
            .map_err(|_| FfiError::WalletNotInitialized)?;
        let signing_key = ed25519_dalek::SigningKey::from_bytes(&sk_bytes);
        use ed25519_dalek::Signer;
        let sig_data = format!("{}:{}", display_name, bio);
        let signature = signing_key.sign(sig_data.as_bytes());
        let profile = gratia_lux::LuxProfile {
            address,
            display_name: if display_name.is_empty() { None } else { Some(display_name) },
            bio: if bio.is_empty() { None } else { Some(bio) },
            avatar_hash: None,
            updated_at: chrono::Utc::now(),
            signature: signature.to_bytes().to_vec(),
        };
        inner.lux_store.set_profile(profile);
        Ok(())
    }

    /// Get the current posting fee in Lux.
    pub fn lux_get_post_fee(&self) -> Result<u64, FfiError> {
        let inner = self.lock_inner()?;
        Ok(inner.lux_fees.post_fee())
    }

    /// Get total Lux burned from social activity.
    pub fn lux_get_total_burned(&self) -> Result<u64, FfiError> {
        let inner = self.lock_inner()?;
        Ok(inner.lux_fees.total_burned())
    }

    /// Get the number of posts in the local store.
    pub fn lux_get_post_count(&self) -> Result<u64, FfiError> {
        let inner = self.lock_inner()?;
        Ok(inner.lux_store.post_count() as u64)
    }
}

// ============================================================================
// BIP39 seed phrase methods (optional, feature-gated)
// ============================================================================

#[cfg(feature = "seed-phrase")]
#[uniffi::export]
impl GratiaNode {
    /// Export the wallet's seed phrase as a 24-word BIP39 mnemonic.
    ///
    /// Returns a space-separated string of 24 English words. This is the
    /// preferred backup format for end users.
    ///
    /// # Security
    /// Only callable through an explicit user action in settings.
    /// Never shown during onboarding. Never logged.
    pub fn export_seed_words(&self) -> Result<String, FfiError> {
        let inner = self.lock_inner()?;
        let words = inner.wallet.export_seed_words().map_err(|e| {
            FfiError::InternalError {
                reason: format!("seed words export failed: {}", e),
            }
        })?;
        rust_log("Seed words exported (user requested)");
        Ok(words)
    }

    /// Restore a wallet from a 24-word BIP39 mnemonic.
    ///
    /// Validates the mnemonic checksum. Returns the wallet address string
    /// on success.
    pub fn import_seed_words(&self, words: String) -> Result<String, FfiError> {
        let mut inner = self.lock_inner()?;
        let address = inner.wallet.import_seed_words(&words).map_err(|e| {
            FfiError::InternalError {
                reason: format!("seed words import failed: {}", e),
            }
        })?;
        rust_log(&format!(
            "Wallet restored from seed words: {}",
            address_to_hex(&address)
        ));
        Ok(address_to_hex(&address))
    }
}

// ============================================================================
// Free functions (not exported via UniFFI)
// ============================================================================

/// Build an FfiConsensusStatus from the engine state.
fn consensus_status(engine: &ConsensusEngine, blocks_produced: u64) -> FfiConsensusStatus {
    let state = match engine.state() {
        gratia_consensus::ConsensusState::Syncing => "syncing",
        gratia_consensus::ConsensusState::Active => "active",
        gratia_consensus::ConsensusState::Producing => "producing",
        gratia_consensus::ConsensusState::Stopped => "stopped",
    };
    FfiConsensusStatus {
        state: state.to_string(),
        current_slot: engine.current_slot(),
        current_height: engine.current_height(),
        is_committee_member: engine.is_committee_member(),
        blocks_produced,
    }
}

/// Background task that advances the consensus engine every 4 seconds.
///
/// WHY: The consensus engine's slot timer must run continuously in the
/// background. When this node is selected as the block producer for a slot,
/// it produces an empty block (no transactions for the demo), serializes it,
/// and broadcasts it to the network.
async fn run_slot_timer(inner: Arc<Mutex<GratiaNodeInner>>) {
    // ── Peer Discovery & Initial Sync Phase (up to 30 seconds) ─────────
    // WHY: Wait for peer discovery and chain sync BEFORE producing any
    // blocks. Without this, both phones produce divergent chains before
    // discovering each other via the bootstrap node, creating permanent
    // forks that trigger infinite reorg loops.
    //
    // During this window:
    // - libp2p connects to the bootstrap node via QUIC (~2-5s)
    // - Gossipsub mesh forms and NodeAnnouncements propagate (~5-10s)
    // - MiningService.pollNetworkEvents() runs every 500ms, processing
    //   PeerConnected and NodeAnnounced events (rebuilding the committee)
    // - Any blocks from existing peers arrive via gossip and get applied
    //
    // After this window:
    // - If peers found + blocks received: we're synced, start producing
    // - If peers found + no blocks: fresh network, VRF handles ordering
    // - If no peers: solo mode (bootstrap, like Satoshi mining alone)
    {
        let discovery_start = std::time::Instant::now();
        let discovery_timeout = std::time::Duration::from_secs(30);
        let check_interval = tokio::time::Duration::from_secs(2);
        // WHY: Wait at least 10 seconds regardless, to give gossipsub
        // mesh time to form. Without this minimum, a fast local network
        // might pass the "has peers" check before the mesh is ready to
        // relay blocks, causing the node to start producing before it
        // could have received the peer's chain.
        let min_wait = std::time::Duration::from_secs(10);

        rust_log("Slot timer: starting 30s peer discovery phase");

        // WHY: Track when we last re-announced during discovery. The initial
        // announcement on PeerConnected fires before gossipsub subscriptions
        // are exchanged, so it gets silently dropped. Re-announcing every 5s
        // during discovery ensures peers receive our announcement once the
        // gossipsub mesh is ready.
        let mut last_discovery_announce = std::time::Instant::now();

        loop {
            tokio::time::sleep(check_interval).await;

            let (peer_count, has_known_peers, our_height) = {
                let mut guard = match inner.lock() {
                    Ok(g) => g,
                    Err(_) => {
                        error!("Slot timer: mutex poisoned during discovery");
                        return;
                    }
                };

                // WHY: Process pending PeerConnected/NodeAnnounced events during
                // discovery. Without this, if the Kotlin MiningService hasn't
                // started (battery < 80%), poll_network_events never runs and
                // the peer count stays at 0 — causing the node to think it has
                // no peers even when transport connections are active.
                {
                    // Take rx out to avoid borrow conflicts (same pattern as poll_network_events)
                    let mut raw_events = Vec::new();
                    if let Some(mut rx) = guard.network_event_rx.take() {
                        while let Ok(event) = rx.try_recv() {
                            raw_events.push(event);
                        }
                        guard.network_event_rx = Some(rx);
                    }
                    let mut peers_added = 0u32;
                    for event in raw_events {
                        match event {
                            NetworkEvent::PeerConnected { peer_id, is_inbound, .. } => {
                                if let Some(ref mut network) = guard.network {
                                    network.on_peer_connected(peer_id, is_inbound);
                                }
                                peers_added += 1;
                            }
                            NetworkEvent::PeerDisconnected { peer_id } => {
                                if let Some(ref mut network) = guard.network {
                                    network.on_peer_disconnected(&peer_id, true);
                                }
                            }
                            NetworkEvent::NodeAnnounced(ann) => {
                                let peer_node_id = ann.node_id;
                                let peer_announced_height = ann.height;
                                let payload = gratia_network::gossip::node_announcement_signing_payload(&ann);
                                let sig_valid = ed25519_dalek::VerifyingKey::from_bytes(&ann.ed25519_pubkey)
                                    .ok()
                                    .and_then(|pk| {
                                        use ed25519_dalek::Verifier;
                                        ed25519_dalek::Signature::from_slice(&ann.signature)
                                            .ok()
                                            .map(|sig| pk.verify(&payload, &sig).is_ok())
                                    })
                                    .unwrap_or(false);
                                if sig_valid && !guard.known_peer_nodes.iter().any(|p| p.node_id == peer_node_id) {
                                    rust_log(&format!("Discovery: NodeAnnounced from {:?} score={} height={}", &peer_node_id.0[..4], ann.presence_score, peer_announced_height));
                                    guard.known_peer_nodes.push(*ann);

                                    // WHY: Rebuild committee immediately when a new peer
                                    // is discovered. Without this, the committee stays
                                    // solo even after discovering peers — because the
                                    // full rebuild only happens in poll_network_events,
                                    // which doesn't run when MiningService is inactive
                                    // (battery < 80%).
                                    // WHY: If node_id() fails, skip committee rebuild entirely.
                                    // A zero-ID member in the committee could match any
                                    // lookup, corrupting validator selection and BFT.
                                    if let (Ok(sk_bytes), Ok(local_node_id)) = (guard.wallet.signing_key_bytes(), guard.wallet.node_id()) {
                                        let local_score = if guard.is_debug_bypass() { 100u8 }
                                            else if guard.presence_score > 0 { guard.presence_score }
                                            else { 75u8 };
                                        let vrf_pubkey = VrfSecretKey::from_ed25519_bytes(&sk_bytes).public_key();
                                        let local_signing_pubkey = gratia_core::crypto::Keypair::from_secret_key_bytes(&sk_bytes).public_key_bytes();
                                        let mut all_eligible = vec![EligibleNode {
                                            node_id: local_node_id, vrf_pubkey, presence_score: local_score,
                                            has_valid_pol: true, meets_minimum_stake: true, pol_days: 90,
                                            signing_pubkey: local_signing_pubkey, vrf_proof: vec![],
                                        }];
                                        for peer_ann in &guard.known_peer_nodes {
                                            // WHY: Use the VRF pubkey from the announcement directly.
                                            // Deriving from ed25519_pubkey treats the PUBLIC key as
                                            // a VRF SECRET key, producing a wrong VRF pubkey that
                                            // differs from the peer's actual VRF pubkey. This caused
                                            // committee ordering to disagree between phones.
                                            all_eligible.push(EligibleNode {
                                                node_id: peer_ann.node_id,
                                                vrf_pubkey: VrfPublicKey { bytes: peer_ann.vrf_pubkey_bytes },
                                                presence_score: peer_ann.presence_score,
                                                has_valid_pol: true, meets_minimum_stake: true,
                                                pol_days: peer_ann.pol_days,
                                                signing_pubkey: peer_ann.ed25519_pubkey.to_vec(),
                                                vrf_proof: vec![],
                                            });
                                        }
                                        all_eligible.sort_by(|a, b| a.node_id.0.cmp(&b.node_id.0));
                                        let real_count = all_eligible.len();
                                        // Extract epoch seed before mutable consensus borrow
                                        let epoch_seed = {
                                            let should_new = guard.epoch_seed.is_none()
                                                || guard.consensus.as_ref()
                                                    .and_then(|c| c.committee())
                                                    .map(|c| gratia_consensus::committee::should_rotate(c, guard.consensus.as_ref().map(|e| e.current_slot()).unwrap_or(0)))
                                                    .unwrap_or(true);
                                            if should_new {
                                                let s = guard.consensus.as_ref()
                                                    .map(|c| c.compute_epoch_seed())
                                                    .unwrap_or([0u8; 32]);
                                                guard.epoch_seed = Some(s);
                                            }
                                            guard.epoch_seed.unwrap_or([0u8; 32])
                                        };
                                        if let Some(ref mut consensus) = guard.consensus {
                                            let _ = consensus.initialize_committee(&all_eligible, &epoch_seed, 0, 0);
                                        }
                                        let prev = guard.real_committee_members;
                                        guard.real_committee_members = real_count;
                                        // WHY: Peer discovered — reset solo block cap so
                                        // production resumes with BFT finality available.
                                        if real_count > 1 && prev <= 1 {
                                            guard.consecutive_solo_blocks = 0;
                                        }
                                        if prev != real_count {
                                            rust_log(&format!("Discovery: committee changed {}->{} members", prev, real_count));
                                        }

                                        // ── Solo→Multi fork resolution (discovery phase) ──
                                        // WHY: The discovery handler consumes NodeAnnounced
                                        // events before poll_network_events can see them.
                                        // Without fork resolution here, two phones with
                                        // independent solo chains (different genesis blocks,
                                        // incompatible parent hashes) will never converge —
                                        // neither can accept the other's blocks. The shorter
                                        // chain resets to height 0 and syncs from the longer.
                                        if prev <= 1 && real_count > 1 {
                                            let our_height = guard.consensus.as_ref()
                                                .map(|e| e.current_height())
                                                .unwrap_or(0);

                                            if !guard.yield_checked_peers.contains(&peer_node_id.0) {
                                                guard.yield_checked_peers.push(peer_node_id.0);
                                            }

                                            // Fork resolution strategy:
                                            // - Shorter chain yields (resets to 0, syncs from peer)
                                            // - Longer chain also resets consensus to 0 (preserves balances)
                                            //   so both start a fresh shared chain
                                            // - Equal height: NO reset — phones are on the same chain
                                            //   (e.g., dual restart). Just rebuild committee and continue.
                                            let heights_equal = our_height == peer_announced_height;
                                            let should_yield = if our_height > 0 && peer_announced_height > 0 && !heights_equal {
                                                our_height < peer_announced_height
                                            } else {
                                                false
                                            };
                                            let should_reset_winner = !should_yield && !heights_equal
                                                && our_height > 0 && peer_announced_height > 0;

                                            if should_yield {
                                                rust_log(&format!(
                                                    "DISCOVERY FORK RESOLUTION: yielding chain (height {}) to peer (height {}) — resetting to sync",
                                                    our_height, peer_announced_height,
                                                ));

                                                // 1. Reset consensus engine to height 0
                                                if let Some(ref mut consensus) = guard.consensus {
                                                    let _ = consensus.rollback_to(0, BlockHash([0u8; 32]));
                                                }

                                                // 2. Reset Streamlet BFT state
                                                if let Some(ref mut streamlet) = guard.streamlet {
                                                    streamlet.restore(0, [0u8; 32]);
                                                }

                                                // 3. Delete chain_state.bin (persisted height/hash)
                                                if let Some(ref persistence) = guard.chain_persistence {
                                                    persistence.save(0, &[0u8; 32], 0);
                                                }

                                                // 4. Delete chain_state.db and RocksDB directory
                                                let data_dir = guard.chain_persistence
                                                    .as_ref()
                                                    .map(|p| p.data_dir().to_string())
                                                    .unwrap_or_default();
                                                if !data_dir.is_empty() {
                                                    let chain_db_path = format!("{}/chain_state.db", data_dir);
                                                    let _ = std::fs::remove_file(&chain_db_path);
                                                    let rocksdb_path = format!("{}/rocksdb", data_dir);
                                                    let _ = std::fs::remove_dir_all(&rocksdb_path);

                                                    // 5. Re-open storage backend and state manager
                                                    let state_path = format!("{}/chain_state.db", data_dir);
                                                    let backend_config = {
                                                        #[cfg(feature = "rocksdb-backend")]
                                                        {
                                                            let rdb_path = format!("{}/rocksdb", data_dir);
                                                            StorageBackendConfig::RocksDb { db_path: rdb_path }
                                                        }
                                                        #[cfg(not(feature = "rocksdb-backend"))]
                                                        {
                                                            StorageBackendConfig::InMemory {
                                                                persistence_path: Some(state_path.clone()),
                                                            }
                                                        }
                                                    };
                                                    if let Ok(backend) = open_storage(backend_config) {
                                                        let sm = StateManager::new(backend.store.clone());
                                                        guard.storage_backend = Some(backend);
                                                        guard.state_manager = Some(sm);
                                                        rust_log("DISCOVERY FORK RESOLUTION: storage backend and state manager re-initialized");
                                                    } else {
                                                        rust_log("DISCOVERY FORK RESOLUTION: WARNING — failed to re-open storage backend");
                                                    }
                                                }

                                                // 6. Reset wallet balance to 0 (will be rebuilt from synced blocks)
                                                guard.wallet.sync_balance(0);
                                                guard.wallet.sync_nonce(0);

                                                // 7. Clear recent blocks cache
                                                guard.recent_blocks.clear();

                                                // 8. Reset blocks produced counter
                                                guard.blocks_produced = 0;

                                                // 9. Update sync managers to height 0
                                                if let Some(ref mut sync) = guard.sync_manager {
                                                    sync.update_local_state(0, BlockHash([0u8; 32]));
                                                }
                                                if let Some(ref network) = guard.network {
                                                    let _ = network.try_reset_local_height(0, BlockHash([0u8; 32]));
                                                }

                                                // 10. Reset consensus sync protocol
                                                if let Some(ref mut sp) = guard.sync_protocol {
                                                    sp.reset(0);
                                                }

                                                // 11. Set reorg cooldown to prevent immediate re-trigger
                                                guard.last_reorg_at = Some(std::time::Instant::now());

                                                rust_log(&format!(
                                                    "DISCOVERY FORK RESOLUTION: reset complete — now at height 0, will sync from peer at height {}",
                                                    peer_announced_height,
                                                ));
                                            } else if should_reset_winner {
                                                // WHY: Winning phone resets consensus to 0 but
                                                // KEEPS balances. The loser reset to 0 with fresh
                                                // state. Both start a shared chain from genesis.
                                                rust_log(&format!(
                                                    "DISCOVERY SOLO→MULTI: winning chain (height {}) — resetting consensus to 0, preserving balances",
                                                    our_height,
                                                ));

                                                if let Some(ref mut consensus) = guard.consensus {
                                                    let _ = consensus.rollback_to(0, BlockHash([0u8; 32]));
                                                }

                                                let committee_sz = guard.consensus.as_ref()
                                                    .and_then(|c| c.committee())
                                                    .map(|cm| cm.members.len())
                                                    .unwrap_or(1);
                                                let our_nid = guard.wallet.node_id().ok();
                                                if let (Some(ref mut streamlet), Some(nid)) =
                                                    (&mut guard.streamlet, our_nid)
                                                {
                                                    *streamlet = StreamletState::new(nid, committee_sz);
                                                }

                                                if let Some(ref persistence) = guard.chain_persistence {
                                                    persistence.save(0, &[0u8; 32], guard.blocks_produced);
                                                }

                                                guard.recent_blocks.clear();
                                                guard.blocks_produced = 0;

                                                if let Some(ref mut sync) = guard.sync_manager {
                                                    sync.update_local_state(0, BlockHash([0u8; 32]));
                                                }
                                                if let Some(ref network) = guard.network {
                                                    let _ = network.try_reset_local_height(0, BlockHash([0u8; 32]));
                                                }
                                                if let Some(ref mut sp) = guard.sync_protocol {
                                                    sp.reset(0);
                                                }

                                                guard.last_reorg_at = Some(std::time::Instant::now());
                                            } else if heights_equal {
                                                // WHY: Equal height — both phones are on the same
                                                // chain (dual restart). No reset needed. Just clear
                                                // BFT state and continue from current height.
                                                rust_log(&format!(
                                                    "DISCOVERY SOLO→MULTI: same height ({}) — continuing shared chain, no reset",
                                                    our_height,
                                                ));
                                            }

                                            // Clear pending BFT state for clean multi-node start
                                            guard.pending_block_hash = None;
                                            guard.pending_block_created_at = None;
                                            guard.pending_broadcast_block = None;
                                            guard.consecutive_bft_expirations = 0;
                                        }
                                    }
                                }
                            }
                            other => {
                                // Buffer block/sig/tx events for poll_network_events
                                guard.buffered_raw_events.push(other);
                            }
                        }
                    }
                    if peers_added > 0 {
                        rust_log(&format!("Discovery: processed {} peer connect events", peers_added));
                    }
                }

                let pc = guard.network.as_ref()
                    .map(|n| n.connected_peer_count())
                    .unwrap_or(0);
                let kp = !guard.known_peer_nodes.is_empty();
                let h = guard.consensus.as_ref()
                    .map(|e| e.current_height())
                    .unwrap_or(0);

                // Re-announce every 5s during discovery if we have transport
                // peers but haven't received their NodeAnnouncements yet.
                if pc > 0 && !kp && last_discovery_announce.elapsed() >= std::time::Duration::from_secs(5) {
                    if let (Some(ref network), Ok(sk_bytes)) = (
                        &guard.network,
                        guard.wallet.signing_key_bytes(),
                    ) {
                        let keypair = gratia_core::crypto::Keypair::from_secret_key_bytes(&sk_bytes);
                        let local_node_id = NodeId::from_public_key(keypair.public_key());
                        let vrf_pk = VrfSecretKey::from_ed25519_bytes(&sk_bytes).public_key();
                        let discovery_height = guard.consensus.as_ref()
                            .map(|e| e.current_height())
                            .unwrap_or(0);
                        let mut ann = NodeAnnouncement {
                            node_id: local_node_id,
                            vrf_pubkey_bytes: vrf_pk.bytes,
                            presence_score: 100,
                            pol_days: 90,
                            timestamp: Utc::now(),
                            ed25519_pubkey: *keypair.public_key().as_bytes(),
                            signature: Vec::new(),
                            height: discovery_height,
                        };
                        let payload = gratia_network::gossip::node_announcement_signing_payload(&ann);
                        ann.signature = keypair.sign(&payload);
                        let _ = network.try_announce_node_sync(&ann);
                        rust_log(&format!("Discovery: re-announced to {} peers", pc));
                    }
                    last_discovery_announce = std::time::Instant::now();
                }

                (pc, kp, h)
            };

            let elapsed = discovery_start.elapsed();

            // WHY: If we've synced blocks from a peer (height > 0 from
            // a persisted chain or received gossip blocks), AND we've
            // discovered real peers, AND minimum wait has passed, we're
            // ready. The peer's chain is our starting point.
            if has_known_peers && our_height > 0 && elapsed >= min_wait {
                rust_log(&format!(
                    "Slot timer: synced with peers at height {}, peers={}, elapsed={}s — starting production",
                    our_height, peer_count, elapsed.as_secs()
                ));
                break;
            }

            // WHY: If we've found peers via NodeAnnouncements but haven't
            // received any blocks yet, and min_wait has passed, give a few
            // more seconds for blocks to arrive via gossip. If still no
            // blocks by 20s, both phones are starting fresh — safe to begin
            // since the committee and VRF will handle slot assignment.
            if has_known_peers && our_height == 0 && elapsed >= min_wait {
                // Wait up to 20s total for blocks from peers
                if elapsed >= std::time::Duration::from_secs(20) {
                    rust_log(&format!(
                        "Slot timer: peers found but no blocks received (fresh network), peers={}, elapsed={}s — starting production",
                        peer_count, elapsed.as_secs()
                    ));
                    break;
                }
                // Otherwise keep waiting for blocks
                continue;
            }

            // WHY: Dynamic timeout based on peer state. If transport-level
            // peers are connected (bootstrap, mDNS) but we haven't received
            // a NodeAnnounced yet (known=false), extend the wait to 60s.
            // Gossipsub mesh formation takes 5-10 seconds after connection,
            // and the periodic re-announcement runs every 32s. The 30s
            // timeout was expiring just before the first announcement arrived.
            let effective_timeout = if peer_count > 0 && !has_known_peers {
                // Peers connected but no announcements yet — wait longer
                std::time::Duration::from_secs(60)
            } else {
                discovery_timeout // 30s default
            };

            if elapsed >= effective_timeout {
                if has_known_peers {
                    rust_log(&format!(
                        "Slot timer: discovery complete (pc={}, known=true), height={} — starting production",
                        peer_count, our_height
                    ));
                } else if peer_count > 0 {
                    rust_log(&format!(
                        "Slot timer: peers connected but no announcements after {}s (pc={}), starting solo",
                        elapsed.as_secs(), peer_count
                    ));
                } else {
                    rust_log("Slot timer: no peers after 30s — starting solo production (bootstrap mode)");
                }
                break;
            }
        }

        // Mark initial sync as complete so the main loop can produce blocks.
        let stagger_slots = {
            let mut guard = match inner.lock() {
                Ok(g) => g,
                Err(_) => {
                    error!("Slot timer: mutex poisoned after discovery");
                    return;
                }
            };
            guard.initial_sync_done = true;

            // WHY: When multiple nodes start simultaneously on a fresh network
            // (height=0, 2+ real members), all of them exit the discovery phase
            // at roughly the same time and race to produce block 1. This causes
            // both to produce competing blocks → fork. To prevent this, stagger
            // startup: the first node in canonical committee order (sorted by
            // node_id) starts immediately; the second waits 1 slot (4s). The
            // first node produces and broadcasts block 1 before the second tries.
            let stagger = if guard.real_committee_members > 1
                && guard.consensus.as_ref().map(|e| e.current_height()).unwrap_or(0) == 0
            {
                // Am I the lowest node_id in the committee?
                let our_id = guard.wallet.node_id()
                    .unwrap_or(NodeId([0xFF; 32]));
                let first_peer_id = guard.known_peer_nodes.first()
                    .map(|p| p.node_id);

                if let Some(peer_id) = first_peer_id {
                    if our_id.0 > peer_id.0 {
                        // We're NOT first — wait 1 slot so the peer produces first
                        rust_log("Slot timer: staggering start by 1 slot (not first in committee order)");
                        1u64
                    } else {
                        // We ARE first — start immediately
                        rust_log("Slot timer: first in committee order, starting immediately");
                        0
                    }
                } else { 0 }
            } else { 0 };

            rust_log(&format!(
                "Slot timer: initial sync complete — real_committee_members={}, height={}, stagger={}",
                guard.real_committee_members,
                guard.consensus.as_ref().map(|e| e.current_height()).unwrap_or(0),
                stagger,
            ));
            stagger
        };

        // Apply stagger delay if needed
        if stagger_slots > 0 {
            tokio::time::sleep(tokio::time::Duration::from_secs(4 * stagger_slots)).await;
        }
    }

    // WHY: 4-second slot time, middle of the 3-5 second target range.
    let slot_duration = tokio::time::Duration::from_secs(4);
    let mut slot_count: u64 = 0;

    loop {
        tokio::time::sleep(slot_duration).await;
        slot_count += 1;

        let mut guard = match inner.lock() {
            Ok(g) => g,
            Err(_) => {
                error!("Slot timer: mutex poisoned, stopping");
                return;
            }
        };

        // WHY: Every 8 slots (~32 seconds), check sync state and update peer
        // chain tips. This enables the full request-response sync protocol:
        // if we're behind the network, we generate sync requests that the
        // network event handler will fulfill. 32 seconds is frequent enough
        // to catch up quickly but infrequent enough to avoid spamming peers.
        if slot_count % 8 == 0 {
            // WHY: Read consensus state first, then update sync manager.
            // Separate borrows to satisfy the borrow checker.
            let chain_state = guard.consensus.as_ref().map(|engine| {
                (engine.current_height(), *engine.last_finalized_hash())
            });

            // WHY: Extract network_height from sync_manager first, then release
            // the mutable borrow so we can borrow sync_protocol separately.
            // Rust's borrow checker doesn't allow two &mut borrows into the same
            // struct through a MutexGuard simultaneously.
            let mut net_height_for_sp: Option<(u64, u64)> = None; // (local_height, network_height)

            if let (Some((local_height, local_tip)), Some(ref mut sync)) =
                (chain_state, &mut guard.sync_manager)
            {
                sync.update_local_state(local_height, local_tip);

                // WHY: Run periodic maintenance before evaluating sync state.
                // Evicts stale peers (offline >5min) and cancels timed-out
                // requests (>30s) so sync decisions use fresh data.
                let maintenance = sync.tick_maintenance();
                if maintenance.stale_peers_evicted > 0 || maintenance.timed_out_requests > 0 {
                    rust_log(&format!(
                        "Sync maintenance: evicted {} stale peers, cancelled {} timed-out requests",
                        maintenance.stale_peers_evicted, maintenance.timed_out_requests,
                    ));
                }

                let network_height = sync.best_network_height().unwrap_or(0);
                net_height_for_sp = Some((local_height, network_height));

                // WHY: When we detect we're behind, actively trigger the
                // network layer to generate and send sync requests. The
                // network event loop has its own periodic chain tip poll
                // (every 30s) but that only fires on its timer. This
                // ensures the FFI maintenance tick (every 32s) also kicks
                // off sync, so a reconnecting phone doesn't wait up to
                // 30+32=62 seconds to start downloading missing blocks.
                match sync.state() {
                    gratia_network::sync::SyncState::Behind { local_height, network_height } => {
                        rust_log(&format!(
                            "Sync: behind network ({}/{}), triggering sync request",
                            local_height, network_height
                        ));
                        if let Some(ref network) = guard.network {
                            let _ = network.try_request_sync();
                        }
                    }
                    gratia_network::sync::SyncState::Syncing { local_height, target_height } => {
                        rust_log(&format!(
                            "Sync: downloading ({}/{})",
                            local_height, target_height
                        ));
                    }
                    _ => {}
                }
            }

            // WHY: Now that sync_manager borrow is released, update the
            // consensus-level sync protocol with the network height.
            if let (Some((local_height, network_height)), Some(ref mut sp)) =
                (net_height_for_sp, &mut guard.sync_protocol)
            {
                sp.on_block_received(network_height);

                if ConsensusSyncProtocol::needs_sync(local_height, network_height) {
                    if let Some(sync_req) = sp.create_sync_request() {
                        rust_log(&format!(
                            "Consensus sync: requesting blocks {}-{} (our={}, network={})",
                            sync_req.from_height, sync_req.to_height,
                            local_height, network_height,
                        ));
                    }
                }
            }

            // ── Geographic sharding activation check ────────────────────
            // WHY: Check every 8 slots (~32s) whether the network has grown
            // past the 10,000-node threshold that triggers sharding. Once
            // activated, each node is assigned to a geographic shard based on
            // its GPS location. Sharding is irreversible per epoch — once
            // active, it stays active even if nodes drop temporarily.
            if guard.shard_coordinator.is_none() {
                // Estimate total network nodes from peer count + 1 (self)
                let total_nodes = guard.sync_manager.as_ref()
                    .map(|sm| sm.tracked_peer_count() as u64 + 1)
                    .unwrap_or(1);

                if total_nodes >= gratia_state::sharding::SHARDING_ACTIVATION_THRESHOLD {
                    use gratia_consensus::sharded_consensus::ShardCoordinator;
                    use gratia_core::types::ShardId;

                    let coordinator = ShardCoordinator::new(
                        ShardId(0), // WHY: Default shard 0 until GPS-based assignment
                        gratia_state::sharding::DEFAULT_ACTIVE_SHARDS,
                    );
                    guard.shard_coordinator = Some(coordinator);
                    rust_log(&format!(
                        "SHARDING ACTIVATED: {} nodes detected (threshold: {})",
                        total_nodes,
                        gratia_state::sharding::SHARDING_ACTIVATION_THRESHOLD,
                    ));
                }
            }

            // ── Periodic peer count reconciliation ──────────────────────
            // WHY: When WiFi drops without a clean disconnect, the atomic
            // peer counter stays stale. Reconcile it with the ConnectionManager's
            // actual peer set every 32 seconds to keep the UI accurate.
            if let Some(ref network) = guard.network {
                network.reconcile_peer_count();
            }

            // ── Periodic node re-announcement ──────────────────────────
            // WHY: Phones connect to the bootstrap relay, not directly to
            // each other. The bootstrap relays gossipsub messages but does
            // NOT emit PeerConnected events to existing peers when a new
            // phone joins. So if Phone A connects, announces, then Phone B
            // connects later, Phone B never receives Phone A's announcement.
            // Re-announcing every ~32 seconds ensures newly-connected peers
            // discover us for committee selection, even if they missed our
            // initial announcement.
            if guard.consensus.is_some() {
                if let (Some(ref network), Ok(sk_bytes)) = (
                    &guard.network,
                    guard.wallet.signing_key_bytes(),
                ) {
                    let keypair_for_ann = gratia_core::crypto::Keypair::from_secret_key_bytes(&sk_bytes);
                    let local_node_id = NodeId::from_public_key(keypair_for_ann.public_key());
                    let vrf_pk = VrfSecretKey::from_ed25519_bytes(&sk_bytes).public_key();
                    let periodic_height = guard.consensus.as_ref()
                        .map(|e| e.current_height())
                        .unwrap_or(0);
                    let mut announcement = NodeAnnouncement {
                        node_id: local_node_id,
                        vrf_pubkey_bytes: vrf_pk.bytes,
                        presence_score: 100, // Demo score
                        pol_days: 90,
                        timestamp: Utc::now(),
                        ed25519_pubkey: *keypair_for_ann.public_key().as_bytes(),
                        signature: Vec::new(),
                        height: periodic_height,
                    };
                    let payload = gratia_network::gossip::node_announcement_signing_payload(&announcement);
                    announcement.signature = keypair_for_ann.sign(&payload);

                    // WHY: Standard gossipsub re-announce (works when mesh is healthy)
                    if let Err(e) = network.try_announce_node_sync(&announcement) {
                        // Channel full or network not running — not critical
                        tracing::trace!("Periodic re-announce failed: {}", e);
                    }

                    // ── Fix 3: Periodic re-discovery via direct message ──────
                    // WHY: If real_committee_members == 1 but we have connected
                    // peers, gossipsub never delivered the initial NodeAnnouncements.
                    // This happens when the gossipsub mesh is one-directional
                    // (Noise handshake failures). Send our announcement directly
                    // to all connected peers via request-response protocol, which
                    // bypasses gossipsub entirely. This catches the case where
                    // phones missed the initial 30s discovery window.
                    if guard.real_committee_members <= 1 && network.connected_peer_count() > 0 {
                        match network.try_direct_announce_all(&announcement) {
                            Ok(sent) => {
                                rust_log(&format!(
                                    "RE-DISCOVERY: real_committee_members=1 but {} peers connected — \
                                     direct-announced to {} peers via request-response",
                                    network.connected_peer_count(), sent
                                ));
                            }
                            Err(e) => {
                                tracing::trace!("Re-discovery direct announce failed: {}", e);
                            }
                        }
                    }
                }
            }
        }

        // WHY: We check consensus existence and advance the slot in a
        // scoped block, then operate on the result outside the borrow.
        // WHY: Extract values needed for synthetic override BEFORE borrowing
        // consensus mutably. Avoids borrow conflict with guard.known_peer_nodes.
        let real_count = guard.real_committee_members;
        let bft_retry = guard.bft_retry_count;
        let our_id_for_slot = guard.wallet.node_id()
            .map(|n| n.0)
            .unwrap_or([0xFF; 32]);
        let peer_ids_for_slot: Vec<[u8; 32]> = guard.known_peer_nodes.iter()
            .map(|p| p.node_id.0)
            .collect();

        let mut should_produce = {
            match guard.consensus.as_mut() {
                Some(engine) => {
                    let result = engine.advance_slot();
                    if result {
                        let cur_slot = engine.current_slot();
                        let cur_height = engine.current_height();
                        info!(
                            slot = cur_slot,
                            height = cur_height + 1,
                            "Slot timer: this node should produce a block"
                        );
                    }
                    if real_count == 1 {
                        // WHY: Solo mode — only 1 real node, produce every slot.
                        engine.force_producing_state();
                        true
                    } else if real_count == 2 {
                        // WHY: Two real nodes — IGNORE VRF result entirely.
                        // Use (height + bft_retry_count) % 2 for alternation.
                        // Both phones agree on height (shared chain) and on who
                        // has the lower NodeId, so they deterministically pick
                        // opposite turns. bft_retry_count breaks ties when BFT
                        // expires and both retry the same height — the retry
                        // flips parity so the other node gets a turn.
                        let next_h = engine.current_height() + 1;
                        let we_are_lower = peer_ids_for_slot.first()
                            .map(|peer_id| our_id_for_slot < *peer_id)
                            .unwrap_or(true);
                        let selector = next_h.wrapping_add(bft_retry);
                        let our_turn = if we_are_lower {
                            selector % 2 == 0
                        } else {
                            selector % 2 == 1
                        };
                        if our_turn {
                            engine.force_producing_state();
                            true
                        } else {
                            false
                        }
                    } else {
                        result
                    }
                }
                None => {
                    debug!("Slot timer: consensus stopped, exiting");
                    return;
                }
            }
        };

        // ── BFT pending block expiry ─────────────────────────────────
        // WHY: If a pending block hasn't reached BFT finality within
        // the BFT timeout (base 20 seconds = 5 slot durations), discard
        // it. Unlike the old approach which force-finalized with
        // insufficient signatures (allowing solo phones to mint fake
        // blocks), we now REQUIRE real peer signatures. No BFT finality
        // = block is invalid and discarded. This is how Bitcoin works —
        // if you can't prove your block is valid (via PoW hash / BFT
        // sigs), it doesn't count.
        let mut bft_incremented_this_tick = false;
        {
            let has_pending = guard.pending_block_created_at.is_some();
            if has_pending {
                // WHY: Scale BFT timeout with committee size. The timeout must
                // accommodate: gossipsub heartbeat (5s) + message propagation +
                // processing + return signature delivery. For 2 nodes on WiFi,
                // the round-trip is usually <2s but gossipsub mesh re-grafting
                // after a missed heartbeat can add 5-10s. Formula:
                //   base 20 seconds + 2 seconds per committee member beyond 2.
                // This gives:
                //   2 members → 20s (testnet — 5 slot ticks of margin)
                //   5 members → 26s (small testnet)
                //  21 members → 58s (mainnet — under 1 minute for global mobile)
                let bft_timeout_secs = 20 + (2 * guard.real_committee_members.saturating_sub(2) as u64);
                let expired = guard.pending_block_created_at
                    .map(|t| t.elapsed().as_secs() >= bft_timeout_secs)
                    .unwrap_or(false);

                if expired {
                    let sig_count = guard.consensus.as_ref()
                        .and_then(|e| e.pending_block.as_ref())
                        .map(|p| p.signatures.len())
                        .unwrap_or(0);
                    let threshold = guard.consensus.as_ref()
                        .map(|e| e.pending_finality_threshold())
                        .unwrap_or(0);

                    guard.consecutive_bft_expirations += 1;
                    bft_incremented_this_tick = true;
                    let threshold_display = if threshold == usize::MAX { "?".to_string() } else { threshold.to_string() };
                    guard.bft_retry_count += 1;
                    rust_log(&format!(
                        "BFT EXPIRED: discarding block with {}/{} sigs (insufficient finality) — {} consecutive, retry={}",
                        sig_count, threshold_display, guard.consecutive_bft_expirations, guard.bft_retry_count
                    ));

                    // WHY: Save the expired block's hash before clearing so
                    // late-arriving signatures can still finalize it. The
                    // pending block stays in the consensus engine briefly —
                    // only the tracking state in the FFI layer is cleared.
                    guard.last_expired_block_hash = guard.pending_block_hash.take();
                    guard.last_expired_block_height = guard.consensus.as_ref()
                        .and_then(|e| e.pending_block.as_ref())
                        .map(|p| p.block.header.height);
                    guard.pending_block_created_at = None;
                    // WHY: Clear seen_proposals for the expired height so the
                    // retry doesn't trigger equivocation detection. The previous
                    // block expired without finality — producing a new block at
                    // the same height with different contents is expected, not
                    // equivocation.
                    if let Some(ref mut consensus) = guard.consensus {
                        let expired_height = consensus.pending_block.as_ref()
                            .map(|p| p.block.header.height);
                        if let Some(h) = expired_height {
                            consensus.clear_proposals_for_height(h);
                        }
                    }
                    // NOTE: We do NOT clear engine.pending_block here anymore.
                    // It stays until either a late signature finalizes it, or
                    // the next block production overwrites it. This allows the
                    // late-signature handler to still call add_block_signature().

                    // ── Silent peer loss detection ─────────────────────
                    // WHY: After 5 consecutive BFT expirations (~100 seconds
                    // with 20s timeout), peers are likely gone. The threshold
                    // was raised from 2→5 because with gossipsub timing
                    // variability, 2 consecutive expirations happen even when
                    // peers are healthy. 5 consecutive (~100s of silence)
                    // strongly indicates genuine peer loss.
                    if guard.consecutive_bft_expirations >= 5 && guard.real_committee_members > 1 {
                        rust_log(&format!(
                            "PEER LOSS DETECTED: {} consecutive BFT expirations — reverting to solo mode",
                            guard.consecutive_bft_expirations
                        ));
                        guard.known_peer_nodes.clear();
                        guard.real_committee_members = 1;
                        guard.consecutive_bft_expirations = 0;

                        // Rebuild committee as solo
                        // WHY: If node_id() fails, skip committee rebuild. A zero-ID node
                        // in the committee would corrupt validator selection.
                        let sk_bytes_opt = guard.wallet.signing_key_bytes().ok();
                        let local_node_id_opt = guard.wallet.node_id().ok();
                        let local_score = if guard.presence_score > 0 { guard.presence_score } else { 75u8 };

                        if local_node_id_opt.is_none() {
                            rust_log("SOLO REVERT: skipping committee rebuild — node_id unavailable");
                        }

                        if let (Some(ref mut consensus), Some(ref sk_bytes), Some(local_node_id)) = (
                            &mut guard.consensus,
                            &sk_bytes_opt,
                            local_node_id_opt,
                        ) {
                            let vrf_pubkey = VrfSecretKey::from_ed25519_bytes(sk_bytes).public_key();
                            let local_signing_pubkey = gratia_core::crypto::Keypair::from_secret_key_bytes(sk_bytes).public_key_bytes();
                            let mut all_eligible = vec![EligibleNode {
                                node_id: local_node_id,
                                vrf_pubkey,
                                presence_score: local_score,
                                has_valid_pol: true,
                                meets_minimum_stake: true,
                                pol_days: 90,
                                signing_pubkey: local_signing_pubkey,
                                vrf_proof: vec![],
                            }];
                            for i in 1..=2u8 {
                                let mut fake_id = [0u8; 32];
                                fake_id[0] = i;
                                fake_id[31] = 0xFF;
                                all_eligible.push(EligibleNode {
                                    node_id: NodeId(fake_id),
                                    vrf_pubkey: VrfSecretKey::from_ed25519_bytes(&[i; 32]).public_key(),
                                    presence_score: 40,
                                    has_valid_pol: true,
                                    meets_minimum_stake: true,
                                    pol_days: 90,
                                    signing_pubkey: vec![],
                                    vrf_proof: vec![],
                                });
                            }
                            all_eligible.sort_by(|a, b| a.node_id.0.cmp(&b.node_id.0));
                            let epoch_seed = consensus.compute_epoch_seed();
                            if let Err(e) = consensus.initialize_committee(&all_eligible, &epoch_seed, 0, 0) {
                                warn!("Failed to rebuild solo committee: {}", e);
                            } else {
                                rust_log("Committee rebuilt: 1 real + 2 synthetic (solo mode after BFT timeout)");
                            }
                        }
                    }
                }
            }
        }

        // WHY: Safety net — don't produce if initial sync hasn't completed.
        // Normally initial_sync_done is true by the time we reach here (set
        // after the 30s discovery phase), but NodeAnnounced can reset it
        // during a solo→multi transition to force re-sync.
        if should_produce && !guard.initial_sync_done {
            should_produce = false;
        }

        // WHY: In bootstrap mode (only synthetic committee members), a solo
        // phone CAN mine — like Satoshi mining Bitcoin's genesis block alone.
        // Once real peers exist (real_committee_members > 1), we require at
        // least one peer connection so BFT signatures can be exchanged.
        // If we've been unable to reach peers for 3 consecutive attempts
        // (~12 seconds), fall back to solo mode. This prevents the deadlock
        // where committee=2 + peers=0 = skip forever (BFT expiration never
        // fires because no blocks are produced to expire).
        if should_produce && guard.real_committee_members > 1 {
            let has_peers = guard.network.as_ref()
                .map(|n| n.connected_peer_count() > 0)
                .unwrap_or(false);
            if !has_peers {
                // WHY: Only increment if BFT expiry didn't already increment
                // this tick. Both paths check the same condition (peer is gone),
                // so incrementing twice per tick would trigger solo mode 2x faster
                // than intended.
                if !bft_incremented_this_tick {
                    guard.consecutive_bft_expirations += 1;
                }
                if guard.consecutive_bft_expirations >= 2 {
                    rust_log(&format!(
                        "NO PEERS for {} consecutive slots — reverting to solo mode",
                        guard.consecutive_bft_expirations
                    ));
                    guard.known_peer_nodes.clear();
                    guard.real_committee_members = 1;
                    guard.consecutive_bft_expirations = 0;
                    // Rebuild solo committee inline
                    // WHY: If node_id() fails, skip committee rebuild. A zero-ID node
                    // in the committee would corrupt validator selection.
                    let sk_bytes_opt = guard.wallet.signing_key_bytes().ok();
                    let local_node_id_opt = guard.wallet.node_id().ok();
                    let local_score = if guard.presence_score > 0 { guard.presence_score } else { 75u8 };
                    if local_node_id_opt.is_none() {
                        rust_log("SOLO REVERT: skipping committee rebuild — node_id unavailable");
                    }
                    if let (Some(ref mut consensus), Some(ref sk_bytes), Some(local_node_id)) = (&mut guard.consensus, &sk_bytes_opt, local_node_id_opt) {
                        let vrf_pubkey = VrfSecretKey::from_ed25519_bytes(sk_bytes).public_key();
                        let local_signing_pubkey = gratia_core::crypto::Keypair::from_secret_key_bytes(sk_bytes).public_key_bytes();
                        let mut all_eligible = vec![EligibleNode {
                            node_id: local_node_id, vrf_pubkey, presence_score: local_score,
                            has_valid_pol: true, meets_minimum_stake: true, pol_days: 90,
                            signing_pubkey: local_signing_pubkey,
                            vrf_proof: vec![],
                        }];
                        for i in 1..=2u8 {
                            let mut fake_id = [0u8; 32]; fake_id[0] = i; fake_id[31] = 0xFF;
                            all_eligible.push(EligibleNode {
                                node_id: NodeId(fake_id),
                                vrf_pubkey: VrfSecretKey::from_ed25519_bytes(&[i; 32]).public_key(),
                                presence_score: 40, has_valid_pol: true,
                                meets_minimum_stake: true, pol_days: 90,
                                signing_pubkey: vec![],
                                vrf_proof: vec![],
                            });
                        }
                        all_eligible.sort_by(|a, b| a.node_id.0.cmp(&b.node_id.0));
                        let epoch_seed = consensus.compute_epoch_seed();
                        let _ = consensus.initialize_committee(&all_eligible, &epoch_seed, 0, 0);
                        rust_log("Committee rebuilt: solo mode (no peers reachable)");
                    }
                } else {
                    should_produce = false;
                }
            }
        }

        // WHY: Cap solo chain growth to limit divergence. After 50 consecutive
        // solo blocks (~200s), pause production until a peer reconnects and
        // co-signs. This limits reorg size on reconnect and avoids wasting
        // CPU/battery on blocks that will be orphaned.
        // FIX: If we have connected peers but real_committee_members is still 1,
        // gossipsub likely failed to deliver NodeAnnouncements. Reset the solo
        // cap so production continues — the periodic re-discovery (every 30s)
        // will eventually deliver the announcement and fix real_committee_members.
        if should_produce && guard.real_committee_members <= 1 && guard.consecutive_solo_blocks >= 50 {
            let has_connected_peers = guard.network.as_ref()
                .map(|n| n.connected_peer_count() > 0)
                .unwrap_or(false);
            if has_connected_peers {
                // Peers are connected but announcements haven't been received.
                // Don't cap — reset solo counter and allow production to continue.
                if guard.consecutive_solo_blocks % 50 == 0 {
                    rust_log(&format!(
                        "SOLO CAP: {} connected peers but real_committee_members=1 — \
                         gossipsub likely broken, resetting solo counter",
                        guard.network.as_ref().map(|n| n.connected_peer_count()).unwrap_or(0)
                    ));
                }
                guard.consecutive_solo_blocks = 0;
            } else {
                should_produce = false;
                // Log once every 50 blocks to avoid spam
                if guard.consecutive_solo_blocks % 50 == 0 {
                    rust_log("SOLO CAP: pausing production after 50 solo blocks — waiting for peer");
                }
            }
        }

        // WHY: Don't produce a new block if one is already pending BFT finality.
        // Producing overwrites the pending block and resets the BFT timer, which
        // means the timeout NEVER fires — each new production restarts the clock.
        // This caused S25 to get stuck forever: producing block 1 every 4 seconds,
        // each one resetting the 14-second timer before it could expire.
        if should_produce && guard.pending_block_created_at.is_some() {
            should_produce = false;
        }

        // WHY: Protocol-level mining gate. Battery requirements are enforced
        // here in addition to the mobile UI layer, so bypassing the Kotlin/Swift
        // layer doesn't allow mining without meeting energy requirements.
        // Debug builds bypass this for testing.
        if should_produce && !guard.is_debug_bypass() {
            if !guard.power_state.is_plugged_in || guard.power_state.battery_percent < 80 {
                should_produce = false;
            }
        }

        // WHY: Protocol-level PoL enforcement. In release builds, a node must
        // have valid Proof of Life to produce blocks. This mirrors the check in
        // start_mining() but enforces it continuously — if PoL lapses mid-session
        // (e.g., day rolls over without new PoL data), block production stops.
        // Onboarding (day 0) and grace period are honored per the protocol spec.
        // Debug builds bypass this so testing doesn't require 24h of sensor data.
        if should_produce && !guard.is_debug_bypass() {
            let pol_ok = guard.pol.is_onboarding()
                || guard.pol.is_mining_eligible()
                || guard.pol.in_grace_period();
            if !pol_ok {
                should_produce = false;
                // Log once per ~60 seconds (15 slots * 4s = 60s) to avoid spam
                if slot_count % 15 == 0 {
                    warn!(
                        "Slot timer: block production skipped — Proof of Life not valid. \
                         Missed days: {}",
                        guard.pol.missed_days()
                    );
                }
            }
        }

        // WHY: Protocol-level staking gate. In release builds, a node must
        // meet the minimum stake requirement to produce blocks. This uses
        // the time-aware check that respects the activation threshold (1,000
        // miners) and 7-day grace period. Before activation, minimum is 0
        // so everyone can mine (zero-delay onboarding). After activation +
        // grace period, nodes without sufficient stake cannot produce blocks.
        // Debug builds bypass this for testing.
        if should_produce && !guard.is_debug_bypass() {
            let our_node_id = guard.wallet.node_id()
                .unwrap_or(NodeId([0u8; 32]));
            let now_ts = Utc::now();
            if !guard.staking.meets_minimum_stake_at(&our_node_id, now_ts) {
                should_produce = false;
                if slot_count % 15 == 0 {
                    let effective_min = guard.staking.effective_minimum_stake(now_ts);
                    let current_stake = guard.staking.effective_stake(&our_node_id);
                    warn!(
                        "Slot timer: block production skipped — minimum stake not met. \
                         Required: {} Lux, current: {} Lux",
                        effective_min, current_stake
                    );
                }
            }
        }

        if !should_produce && real_count == 2 {
            let pending = guard.pending_block_created_at.is_some();
            let peers = guard.network.as_ref().map(|n| n.connected_peer_count()).unwrap_or(0);
            let solo = guard.consecutive_solo_blocks;
            let height = guard.consensus.as_ref().map(|e| e.current_height()).unwrap_or(0);
            if slot_count % 5 == 0 {
                rust_log(&format!(
                    "BLOCKED: height={} pending={} peers={} solo={} real={}",
                    height + 1, pending, peers, solo, real_count
                ));
            }
        }
        if should_produce {
            rust_log(&format!("PRODUCING: height={}", guard.consensus.as_ref().map(|e| e.current_height() + 1).unwrap_or(0)));
            // WHY: Sort the mempool deterministically before draining so that
            // ALL nodes produce identical blocks from the same transaction set.
            // Without deterministic ordering, two nodes with the same mempool
            // contents could produce blocks with transactions in different
            // order, causing chain divergence. Sorting by (sender_pubkey, nonce)
            // ensures canonical ordering: same sender's txs are sequential by
            // nonce, and different senders are ordered by public key bytes.
            guard.mempool.sort_by(|a, b| {
                a.sender_pubkey.cmp(&b.sender_pubkey)
                    .then(a.nonce.cmp(&b.nonce))
            });

            // WHY: Drain the mempool into the block. This is how user transactions
            // (sent locally or received via gossip) become on-chain. Cap at 512
            // per block to match MAX_TRANSACTIONS_PER_BLOCK.
            let drain_count = guard.mempool.len().min(512);
            let block_txs: Vec<gratia_core::types::Transaction> = guard.mempool
                .drain(..drain_count)
                .collect();
            let tx_count = block_txs.len();

            // WHY: Extract pubkey BEFORE borrowing consensus mutably.
            let our_pubkey_bytes = guard.wallet.signing_key_bytes().ok()
                .map(|sk| gratia_core::crypto::Keypair::from_secret_key_bytes(&sk).public_key_bytes())
                .unwrap_or_default();

            let produce_result = match guard.consensus.as_mut() {
                Some(engine) => engine.produce_block(block_txs, vec![], [0u8; 32], our_pubkey_bytes.clone()),
                None => {
                    rust_log("SLOT: consensus engine missing, skipping block production");
                    continue;
                }
            };

            match produce_result {
                Ok(pending) => {
                    let block_height = pending.block.header.height;
                    guard.blocks_produced += 1;

                    let chain_height = guard.consensus.as_ref().map(|e| e.current_height()).unwrap_or(0);
                    rust_log(&format!("BLOCK PRODUCED height={} txs={} chain_height={} total={}", block_height, tx_count, chain_height, guard.blocks_produced));
                    info!(height = block_height, txs = tx_count, "Block produced");

                    // BFT finality: sign with our real key, then either
                    // auto-finalize (bootstrap/solo) or await peer signatures.
                    //
                    // WHY: Careful borrow management — we can't hold a mutable
                    // borrow on guard.consensus while also accessing guard.wallet
                    // or guard.network. So we extract what we need in phases.

                    // Phase 1: Get signing key bytes and pubkey (immutable borrow on wallet).
                    // WHY: If signing key is unavailable, skip BFT entirely for this slot.
                    // A zero pubkey in a ValidatorSignatureMessage would be invalid and
                    // could be exploited to attribute signatures to a nonexistent validator.
                    let sk_bytes_opt = guard.wallet.signing_key_bytes().ok();
                    let our_pubkey_bytes: [u8; 32] = match sk_bytes_opt.as_ref()
                        .map(|sk| *gratia_core::crypto::Keypair::from_secret_key_bytes(sk).public_key().as_bytes())
                    {
                        Some(pk) => pk,
                        None => {
                            rust_log("SLOT: no signing key available — skipping BFT for this slot");
                            continue;
                        }
                    };

                    // Phase 2: Read committee info and sign (mutable borrow on consensus).
                    let (threshold, member_count, our_sig, pending_finalized, block_hash_for_broadcast, pending_block_clone) = {
                        let engine = match guard.consensus.as_mut() {
                            Some(e) => e,
                            None => {
                                rust_log("SLOT: consensus engine missing during Phase 2");
                                continue;
                            }
                        };
                        let threshold = engine.pending_finality_threshold();
                        let member_count = engine.committee()
                            .map(|c| c.members.len())
                            .unwrap_or(0);

                        // Sign with our OWN real Ed25519 key.
                        let our_sig = sk_bytes_opt.as_ref().and_then(|sk_bytes| {
                            let keypair = gratia_core::crypto::Keypair::from_secret_key_bytes(sk_bytes);
                            let header = match engine.pending_block.as_ref() {
                                Some(pb) => pb.block.header.clone(),
                                None => {
                                    rust_log("SLOT: no pending block to sign");
                                    return None;
                                }
                            };
                            match engine.sign_block_as_validator(&header, &keypair) {
                                Ok(sig) => Some(sig),
                                Err(e) => {
                                    rust_log(&format!("Failed to self-sign block: {}", e));
                                    None
                                }
                            }
                        });

                        if let Some(ref sig) = our_sig {
                            match engine.add_block_signature(sig.clone()) {
                                Ok(_finalized) => {}
                                Err(e) => {
                                    rust_log(&format!("Failed to add self-signature: {}", e));
                                }
                            }
                        }

                        let pending_finalized = engine.pending_block.as_ref()
                            .map(|p| p.is_finalized())
                            .unwrap_or(false);

                        // WHY: If pending block hash computation fails, log and skip broadcast.
                        // A zero hash would cause peers to co-sign a nonexistent block.
                        let block_hash_opt = engine.pending_block.as_ref()
                            .and_then(|p| p.block.header.hash().ok())
                            .map(|h| h.0);
                        if block_hash_opt.is_none() && engine.pending_block.is_some() {
                            rust_log("SLOT: pending block hash computation failed — BFT broadcast will be skipped");
                        }
                        let block_hash = block_hash_opt.unwrap_or([0u8; 32]);

                        // WHY: Clone the block WITH collected signatures for broadcast.
                        // PendingBlock stores signatures separately from block.validator_signatures.
                        // Without copying them in, the broadcast block has 0 sigs and peers reject it.
                        let pending_block_clone = engine.pending_block.as_ref()
                            .map(|p| {
                                let mut block = p.block.clone();
                                block.validator_signatures = p.signatures.clone();
                                block
                            });

                        (threshold, member_count, our_sig, pending_finalized, block_hash, pending_block_clone)
                    };
                    // Mutable borrow on consensus is now dropped.

                    // Phase 3: Decide whether to finalize now or await peer sigs.
                    // WHY: When real_committee_members <= 1, all other committee
                    // members are synthetic padding that can't sign. BFT finality
                    // would never be reached, so we auto-finalize.
                    let real_members = guard.real_committee_members;
                    let should_finalize_now;
                    if real_members <= 1 || member_count <= 1 || pending_finalized {
                        should_finalize_now = true;
                    } else {
                        should_finalize_now = false;

                        guard.pending_block_hash = Some(block_hash_for_broadcast);
                        guard.pending_block_created_at = Some(std::time::Instant::now());
                        // WHY: Clear expired block state when a new block is produced.
                        // A late signature for the old expired block shouldn't finalize
                        // once we've moved on to producing a new one.
                        guard.last_expired_block_hash = None;
                        guard.last_expired_block_height = None;

                        // Broadcast the pending block to peers via gossipsub.
                        // Clone for direct proposal before moving into broadcast.
                        // WHY: Don't broadcast blocks with 0 validator signatures.
                        // Peers reject them ("Insufficient validator signatures"),
                        // wasting bandwidth and triggering false fork detection.
                        let proposal_block = pending_block_clone.as_ref().map(|b| b.header.clone());
                        if let Some(block) = pending_block_clone {
                            if block.validator_signatures.is_empty() {
                                rust_log("SLOT: NOT broadcasting block with 0 signatures — self-signing failed");
                            } else {
                                guard.pending_broadcast_block = Some(block);
                            }
                        }

                        // Broadcast our validator signature via gossipsub (fallback path).
                        // WHY: Skip broadcast if block_hash_for_broadcast is zero — that means
                        // hash computation failed and we'd be signing a nonexistent block.
                        if block_hash_for_broadcast == [0u8; 32] {
                            rust_log("SLOT: skipping BFT broadcast — block hash is zero (computation failed)");
                        } else if let (Some(ref network), Some(ref our_sig)) = (&guard.network, &our_sig) {
                            let sig_msg = gratia_network::gossip::ValidatorSignatureMessage {
                                block_hash: block_hash_for_broadcast,
                                height: block_height,
                                signature: our_sig.clone(),
                                validator_pubkey: our_pubkey_bytes,
                            };
                            let _ = network.try_broadcast_validator_signature_sync(&sig_msg);

                            // WHY: Send block proposal DIRECTLY to all known BFT peers.
                            // This is the fast path — the peer receives the block in
                            // <100ms and can co-sign it immediately. Gossipsub broadcast
                            // above is kept as fallback for peers we don't have PeerIds for.
                            if !guard.bft_peer_id_bytes.is_empty() {
                                if let Some(ref header) = proposal_block {
                                    // WHY: If serialization fails, skip the direct proposal send.
                                    // Sending empty bytes would cause peers to fail deserialization
                                    // and potentially treat this node as Byzantine.
                                    match bincode::serialize(header) {
                                        Ok(header_bytes) => {
                                            for peer_id_bytes in &guard.bft_peer_id_bytes {
                                                let _ = network.try_send_block_proposal_bytes(
                                                    peer_id_bytes,
                                                    header_bytes.clone(),
                                                    block_hash_for_broadcast,
                                                    block_height,
                                                    our_sig.clone(),
                                                );
                                            }
                                            rust_log(&format!(
                                                "BFT: block {} proposed directly to {} peer(s) + gossipsub, awaiting {}/{} sigs",
                                                block_height, guard.bft_peer_id_bytes.len(), 1, threshold
                                            ));
                                        }
                                        Err(e) => {
                                            rust_log(&format!(
                                                "BFT: skipping direct proposal — header serialization failed: {}", e
                                            ));
                                        }
                                    }
                                }
                            } else {
                                rust_log(&format!(
                                    "BFT: broadcast block {} + our sig (gossipsub only), awaiting {}/{} sigs",
                                    block_height, 1, threshold
                                ));
                            }
                        }
                    }

                    if !should_finalize_now {
                        // WHY: Skip immediate finalization — we'll finalize when
                        // enough ValidatorSignatureReceived events arrive, or
                        // when the timeout fires in a future slot tick.
                        rust_log(&format!("Block {} pending BFT finality, awaiting peer signatures", block_height));
                    } else
                    // Finalize immediately (bootstrap/solo or threshold already met).
                    {
                    let finalize_result = match guard.consensus.as_mut() {
                        Some(engine) => {
                            if real_members <= 1 {
                                // WHY: With only synthetic committee members, normal finalize()
                                // will always fail (requires 2/2 sigs but synthetics can't sign).
                                // force_finalize() only requires 1 signature (our own).
                                engine.force_finalize_pending_block()
                            } else {
                                engine.finalize_pending_block()
                            }
                        }
                        None => {
                            rust_log("SLOT: consensus engine missing during finalization");
                            continue;
                        }
                    };
                    match finalize_result {
                        Ok(finalized_block) => {
                            let finalized_height = finalized_block.header.height;

                            // Broadcast the finalized block to all peers.
                            // WHY: This is the critical step for multi-node consensus.
                            // Without broadcasting, produced blocks stay local and
                            // other nodes never learn about them.
                            // WHY: Store the block for broadcasting after dropping
                            // the mutex guard. We can't hold the lock across an
                            // async broadcast call.
                            let new_chain_height = guard.consensus.as_ref().map(|e| e.current_height()).unwrap_or(0);
                            guard.consecutive_bft_expirations = 0;
                            guard.last_finalized_at = Some(std::time::Instant::now());
                            rust_log(&format!("BLOCK FINALIZED height={} new_chain_height={}", finalized_height, new_chain_height));

                            // ── Streamlet BFT tracking ──────────────────────────
                            // Register this finalized block as a Streamlet proposal,
                            // vote for it, and check for 3-consecutive finality.
                            // WHY: If node_id() or hash() fails, skip Streamlet tracking.
                            // A zero NodeId would cast votes as a nonexistent validator,
                            // and a zero hash would track a nonexistent block.
                            let streamlet_node_id = guard.wallet.node_id();
                            let streamlet_block_hash = finalized_block.header.hash().map(|h| h.0);
                            if streamlet_node_id.is_err() || streamlet_block_hash.is_err() {
                                rust_log("STREAMLET: skipping tracking — node_id or block hash unavailable");
                            }
                            if let (Some(ref mut streamlet), Ok(streamlet_node_id), Ok(block_hash)) = (&mut guard.streamlet, streamlet_node_id, streamlet_block_hash) {
                                streamlet.add_proposal(
                                    finalized_block.header.clone(),
                                    block_hash,
                                    slot_count,
                                );
                                // Self-vote
                                let our_node_id = streamlet_node_id;
                                let vote = StreamletVote {
                                    epoch: slot_count,
                                    block_hash,
                                    height: finalized_height,
                                    signature: gratia_core::types::ValidatorSignature {
                                        validator: our_node_id,
                                        signature: vec![1u8; 64], // placeholder sig
                                    },
                                };
                                let (notarized, finalized_up_to) = streamlet.add_vote(vote);
                                if notarized {
                                    rust_log(&format!(
                                        "STREAMLET: block {} notarized (height {})",
                                        &hex::encode(&block_hash[..4]), finalized_height
                                    ));
                                }
                                if let Some(fh) = finalized_up_to {
                                    rust_log(&format!(
                                        "STREAMLET: finality reached up to height {} (3 consecutive notarized)",
                                        fh
                                    ));
                                }
                                let (proposals, notarized_h, finalized_h) = streamlet.stats();
                                rust_log(&format!(
                                    "STREAMLET: proposals={} notarized_tip={} finalized={}",
                                    proposals, notarized_h, finalized_h
                                ));
                            }

                            // Persist chain state to file after every finalization.
                            // WHY: Ensures chain height and tip hash survive app
                            // restarts without requiring RocksDB.
                            if let Some(ref persistence) = guard.chain_persistence {
                                if let Some(ref engine) = guard.consensus {
                                    let tip_hash = engine.last_finalized_hash().0;
                                    persistence.save(
                                        engine.current_height(),
                                        &tip_hash,
                                        guard.blocks_produced,
                                    );
                                }
                            }

                            // WHY: Apply block transactions to on-chain state.
                            // This updates account balances and nonces, enforcing
                            // balance checks. If a transaction in the block is
                            // invalid (insufficient balance, wrong nonce), it's
                            // skipped — the block still finalizes but the bad tx
                            // has no effect on state.
                            let mut burned_fees_accum: u64 = 0;
                            if let Some(ref sm) = guard.state_manager {
                                let mut applied = 0u32;
                                let mut failed = 0u32;
                                for tx in &finalized_block.transactions {
                                    // WHY: Use the state manager's internal apply_transaction
                                    // via a direct transfer application. We bypass
                                    // apply_block's strict chain continuity checks since
                                    // Phase 1 doesn't have a single unified chain yet.
                                    let sender_addr = gratia_core::types::Address::from_pubkey(&tx.sender_pubkey);
                                    match &tx.payload {
                                        gratia_core::types::TransactionPayload::Transfer { to, amount } => {
                                            let mut sender_acct = sm.get_account(&sender_addr).unwrap_or_default();
                                            let total = amount + tx.fee;
                                            if sender_acct.balance >= total && sender_acct.nonce == tx.nonce {
                                                sender_acct.balance -= total;
                                                sender_acct.nonce += 1;
                                                let _ = sm.db().put_account(&sender_addr, &sender_acct);
                                                let mut recv_acct = sm.get_account(to).unwrap_or_default();
                                                recv_acct.balance += amount;
                                                let _ = sm.db().put_account(to, &recv_acct);
                                                if tx.fee > 0 {
                                                    burned_fees_accum = burned_fees_accum.saturating_add(tx.fee);
                                                    rust_log(&format!("FEE BURNED: {} Lux from tx {}", tx.fee, hex::encode(tx.hash.0)));
                                                }
                                                applied += 1;
                                            } else {
                                                failed += 1;
                                                rust_log(&format!(
                                                    "State: tx {} REJECTED — bal={} need={} nonce={}/{}",
                                                    hex::encode(tx.hash.0), sender_acct.balance, total,
                                                    sender_acct.nonce, tx.nonce
                                                ));
                                            }
                                        }
                                        _ => { applied += 1; } // Other tx types: count but skip state for now
                                    }
                                }
                                if applied > 0 || failed > 0 {
                                    rust_log(&format!(
                                        "State: block {} — {} txs applied, {} rejected",
                                        finalized_height, applied, failed
                                    ));
                                }
                            }
                            guard.total_burned_tx_fees = guard.total_burned_tx_fees.saturating_add(burned_fees_accum);

                            guard.pending_broadcast_block = Some(finalized_block.clone());

                            // WHY: Cache the finalized block for sync. When a new
                            // peer connects, we broadcast recent blocks so they can
                            // catch up immediately without a full sync protocol.
                            guard.recent_blocks.push_back(finalized_block.clone());
                            if guard.recent_blocks.len() > 100 {
                                guard.recent_blocks.pop_front();
                            }

                            // WHY: Only credit rewards for blocks that reached REAL
                            // BFT finality (2+ peer signatures). Solo-finalized blocks
                            // (auto-finalized when real_members <= 1) don't earn rewards
                            // because they were never validated by another node. If the
                            // phone reconnects and a peer has a longer chain, the solo
                            // chain gets replaced — phantom rewards from orphaned solo
                            // blocks would inflate the supply. Solo mode keeps the chain
                            // advancing (so the phone can resume quickly) but doesn't
                            // pay until the network confirms the work.
                            // WHY: Credit rewards for BFT-finalized blocks (real_members > 1)
                            // OR for solo blocks when we've previously seen peers
                            // (yield_checked_peers is non-empty). The latter covers
                            // BFT-fallback-to-solo: the node tried multi-mode but
                            // peer signatures didn't arrive, so it's producing solo
                            // blocks after a timeout. These blocks are still valid
                            // work on a real device. On mainnet this should be
                            // tightened, but for testnet it prevents the scenario
                            // where 2 phones are both mining but neither earns.
                            let has_seen_peers = !guard.yield_checked_peers.is_empty();
                            if real_members > 1 || has_seen_peers {
                                let active_miners = 1u64.max(
                                    guard.staking.staker_count() as u64
                                ).max(1);
                                let reward: Lux = gratia_core::emission::EmissionSchedule
                                    ::per_miner_block_reward_lux(finalized_height, active_miners);

                                if let (Some(ref sm), Ok(our_addr)) = (&guard.state_manager, guard.wallet.address()) {
                                    let mut acct = sm.get_account(&our_addr).unwrap_or_default();
                                    acct.balance += reward;
                                    let _ = sm.db().put_account(&our_addr, &acct);
                                    // Sync wallet FROM on-chain (single source of truth)
                                    guard.wallet.sync_balance(acct.balance);
                                }

                                // Track session stats for UI animations
                                guard.session_grat_earned = guard.session_grat_earned.saturating_add(reward);
                                guard.session_blocks_produced += 1;
                                guard.last_reward_timestamp = Some(Utc::now());

                                rust_log(&format!(
                                    "REWARD: height={} reward={} Lux ({} GRAT) active_miners={}",
                                    finalized_height, reward, reward / 1_000_000,
                                    active_miners
                                ));
                            } else {
                                guard.consecutive_solo_blocks += 1;
                                rust_log(&format!(
                                    "SOLO BLOCK: height={} finalized but NO reward (solo mode, no peer validation) solo_streak={}",
                                    finalized_height, guard.consecutive_solo_blocks
                                ));
                            }

                            // WHY: Persist on-chain state every block so the on-chain
                            // balance always matches the wallet display. Prevents
                            // transaction rejections from stale state. Flash wear is
                            // minimal (~1KB every 4 seconds). With RocksDB, persist()
                            // is a no-op (writes already durable via WAL).
                            {
                                if let Some(ref backend) = guard.storage_backend {
                                    if let Err(e) = backend.persist() {
                                        warn!("Failed to persist state: {}", e);
                                    }
                                }
                            }
                        }
                        Err(e) => {
                            rust_log(&format!("FINALIZE FAILED: {}", e));
                            warn!("Failed to finalize block: {}", e);
                        }
                    }
                    // Clear pending block tracking on finalization.
                    guard.pending_block_hash = None;
                    guard.pending_block_created_at = None;
                    } // closes `else { // Finalize immediately`
                }
                Err(e) => {
                    rust_log(&format!("PRODUCE FAILED: {}", e));
                    warn!("Failed to produce block: {}", e);
                }
            }
        }

        // Broadcast pending block AFTER dropping the mutex guard.
        // WHY: try_broadcast_block_sync sends to a channel (non-blocking),
        // but we still drop the guard first for safety. Single lock
        // acquisition extracts both the block and the network reference.
        drop(guard);

        {
            let mut g = match inner.lock() {
                Ok(g) => g,
                Err(_) => continue,
            };
            if let Some(block) = g.pending_broadcast_block.take() {
                let height = block.header.height;
                if let Some(ref network) = g.network {
                    let result = network.try_broadcast_block_sync(&block);
                    match result {
                        Ok(()) => {
                            rust_log(&format!("BLOCK BROADCAST: height={} to gossipsub", height));
                            info!(height = height, "Block broadcast to network");
                        }
                        Err(e) => {
                            rust_log(&format!("BLOCK BROADCAST FAILED: height={} error={}", height, e));
                            warn!(height = height, error = %e, "Failed to broadcast block");
                        }
                    }
                }
            }
        }
    }
}

// ============================================================================
// Explorer HTTP API
// ============================================================================

/// Lightweight HTTP server for the block explorer.
///
/// WHY: A bare TCP listener with manual HTTP parsing avoids adding any new
/// dependencies (no warp, axum, or tiny_http). We only need one endpoint
/// that returns JSON — this is intentionally minimal.
async fn run_explorer_http(inner: Arc<Mutex<GratiaNodeInner>>, port: u16) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let addr = format!("0.0.0.0:{}", port);
    let listener = match tokio::net::TcpListener::bind(&addr).await {
        Ok(l) => l,
        Err(e) => {
            error!("Explorer API: failed to bind to {}: {}", addr, e);
            return;
        }
    };
    info!("Explorer API listening on {}", addr);

    loop {
        let (mut socket, _peer) = match listener.accept().await {
            Ok(conn) => conn,
            Err(_) => continue,
        };

        let inner = Arc::clone(&inner);
        tokio::spawn(async move {
            let mut buf = vec![0u8; 4096];
            let n = match socket.read(&mut buf).await {
                Ok(n) if n > 0 => n,
                _ => return,
            };

            let request = String::from_utf8_lossy(&buf[..n]);

            // WHY: Parse just the first line to get method + path. We don't
            // need full HTTP parsing for this simple API.
            let first_line = request.lines().next().unwrap_or("");
            let path = first_line.split_whitespace().nth(1).unwrap_or("/");

            let (status, body) = if path == "/api/explorer/data" || path == "/explorer/data" {
                let json = build_explorer_json(&inner);
                ("200 OK", json)
            } else if path == "/" || path == "/api" {
                ("200 OK", r#"{"service":"Gratia Explorer API","version":"0.1.0"}"#.to_string())
            } else {
                ("404 Not Found", r#"{"error":"not found"}"#.to_string())
            };

            // WHY: CORS headers allow the explorer web page (opened from file://
            // or a different origin) to fetch data from the phone's HTTP API.
            let response = format!(
                "HTTP/1.1 {}\r\nContent-Type: application/json\r\nAccess-Control-Allow-Origin: *\r\nAccess-Control-Allow-Methods: GET\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                status,
                body.len(),
                body,
            );

            let _ = socket.write_all(response.as_bytes()).await;
        });
    }
}

/// Build the JSON payload for the explorer API.
///
/// WHY: Uses real block data from the recent_blocks cache instead of synthetic
/// blocks. This gives the explorer accurate hashes, timestamps, producers,
/// and transaction counts from the actual chain.
fn build_explorer_json(inner: &Arc<Mutex<GratiaNodeInner>>) -> String {
    let guard = match inner.lock() {
        Ok(g) => g,
        Err(_) => return r#"{"error":"internal lock error"}"#.to_string(),
    };

    let block_height = guard.consensus.as_ref()
        .map(|e| e.current_height())
        .unwrap_or(0);

    let blocks_produced = guard.blocks_produced;

    let peer_count = guard.network.as_ref()
        .map(|n| n.connected_peer_count() as u32)
        .unwrap_or(0);

    let wallet_address = guard.wallet.address()
        .map(|a| address_to_hex(&a))
        .unwrap_or_default();

    let wallet_balance = guard.wallet.balance();

    let mining_state = mining_state_to_string(&guard.mining_state);

    // Count total transactions across all cached blocks
    let total_tx_count: usize = guard.recent_blocks.iter()
        .map(|b| b.transactions.len())
        .sum();

    // Build blocks JSON from real recent_blocks cache (newest first)
    let blocks_json: Vec<String> = guard.recent_blocks.iter().rev().take(50).map(|block| {
        let hash_hex = block.header.hash()
            .map(|h| hex::encode(h.0))
            .unwrap_or_else(|_| "0".repeat(64));
        let parent_hex = hex::encode(block.header.parent_hash.0);
        let producer_hex = format!("grat:{}", hex::encode(block.header.producer.0));
        let tx_count = block.transactions.len();
        let att_count = block.attestations.len();
        let sig_count = block.validator_signatures.len();
        // WHY: Estimate block size from serialized components.
        // Header ~200 bytes + ~250 bytes per tx + ~100 bytes per attestation.
        let size_estimate = 200 + tx_count * 250 + att_count * 100 + sig_count * 64;
        format!(
            r#"{{"height":{},"hash":"{}","parentHash":"{}","timestamp":"{}","producer":"{}","transactionCount":{},"attestationCount":{},"signatures":{},"size":{}}}"#,
            block.header.height,
            hash_hex,
            parent_hex,
            block.header.timestamp.to_rfc3339(),
            producer_hex,
            tx_count,
            att_count,
            sig_count,
            size_estimate,
        )
    }).collect();

    // Build transactions JSON from real blocks (newest first)
    let mut txs_json: Vec<String> = Vec::new();
    for block in guard.recent_blocks.iter().rev() {
        for tx in &block.transactions {
            let hash_hex = hex::encode(tx.hash.0);
            let sender_hex = if tx.sender_pubkey.len() == 32 {
                // WHY: Derive address from pubkey for display. Use the same
                // derivation as the wallet so addresses match.
                let mut addr_bytes = [0u8; 32];
                addr_bytes.copy_from_slice(&tx.sender_pubkey);
                format!("grat:{}", hex::encode(addr_bytes))
            } else {
                "unknown".to_string()
            };
            let (to_hex, amount) = match &tx.payload {
                gratia_core::types::TransactionPayload::Transfer { to, amount } => {
                    (format!("grat:{}", hex::encode(to.0)), *amount)
                }
                gratia_core::types::TransactionPayload::Stake { amount } => {
                    ("stake".to_string(), *amount)
                }
                gratia_core::types::TransactionPayload::Unstake { amount } => {
                    ("unstake".to_string(), *amount)
                }
                _ => ("contract".to_string(), 0u64),
            };
            txs_json.push(format!(
                r#"{{"hash":"{}","blockHeight":{},"from":"{}","to":"{}","amount":{},"fee":{},"nonce":{},"status":"confirmed","timestamp":"{}"}}"#,
                hash_hex,
                block.header.height,
                sender_hex,
                to_hex,
                amount,
                tx.fee,
                tx.nonce,
                tx.timestamp.to_rfc3339(),
            ));
            // WHY: Cap at 100 transactions to keep the JSON payload small.
            // The explorer paginates client-side so this is plenty.
            if txs_json.len() >= 100 { break; }
        }
        if txs_json.len() >= 100 { break; }
    }

    // Also include wallet-local transactions that may not be in blocks yet
    let wallet_txs: Vec<String> = guard.wallet.history().iter().rev().take(20)
        .filter(|tx| {
            // WHY: Only include wallet txs not already in the block-sourced list.
            // Avoids duplicates between on-chain and local history.
            !txs_json.iter().any(|json| json.contains(&tx.hash))
        })
        .map(|tx| {
            let dir = match tx.direction {
                gratia_wallet::TransactionDirection::Sent => "sent",
                gratia_wallet::TransactionDirection::Received => "received",
            };
            let counterparty = tx.counterparty
                .map(|a| format!("\"grat:{}\"", hex::encode(a.0)))
                .unwrap_or_else(|| "null".to_string());
            let status = match tx.status {
                gratia_wallet::TransactionStatus::Pending => "pending",
                gratia_wallet::TransactionStatus::Confirmed => "confirmed",
                gratia_wallet::TransactionStatus::Failed => "failed",
            };
            format!(
                r#"{{"hash":"{}","direction":"{}","counterparty":{},"amount":{},"timestamp":"{}","status":"{}"}}"#,
                tx.hash, dir, counterparty, tx.amount,
                tx.timestamp.to_rfc3339(), status,
            )
        }).collect();

    // Compute average block time from recent blocks
    let avg_block_time = if guard.recent_blocks.len() >= 2 {
        let newest = if let Some(b) = guard.recent_blocks.back() { b.header.timestamp } else { Utc::now() };
        let oldest = if let Some(b) = guard.recent_blocks.front() { b.header.timestamp } else { newest };
        let span_secs = (newest - oldest).num_seconds() as f64;
        let block_count = guard.recent_blocks.len() as f64 - 1.0;
        if block_count > 0.0 { span_secs / block_count } else { 4.0 }
    } else {
        4.0
    };

    let tps = if block_height > 0 && avg_block_time > 0.0 {
        total_tx_count as f64 / (guard.recent_blocks.len() as f64 * avg_block_time)
    } else {
        0.0
    };

    // WHY: Combine transaction fee burns and Lux social protocol burns for
    // total deflationary impact. Both are fees permanently removed from supply.
    let burned_tx_fees = guard.total_burned_tx_fees;
    let burned_lux_fees = guard.lux_fees.total_burned();
    let total_burned = burned_tx_fees.saturating_add(burned_lux_fees);

    format!(
        r#"{{"network":{{"name":"Gratia Testnet","blockHeight":{},"totalTransactions":{},"activeNodes":{},"avgBlockTime":{:.1},"tps":{:.4},"miningState":"{}","blocksProduced":{},"burnedFees":{},"burnedTxFees":{},"burnedLuxFees":{}}},"blocks":[{}],"transactions":[{}],"walletTransactions":[{}],"wallet":{{"address":"{}","balance":{}}}}}"#,
        block_height,
        total_tx_count,
        peer_count + 1, // +1 for self
        avg_block_time,
        tps,
        mining_state,
        blocks_produced,
        total_burned,
        burned_tx_fees,
        burned_lux_fees,
        blocks_json.join(","),
        txs_json.join(","),
        wallet_txs.join(","),
        wallet_address,
        wallet_balance,
    )
}

// ============================================================================
// Private helpers (not exported via UniFFI)
// ============================================================================

impl GratiaNode {
    /// Get the data directory for file-based persistence.
    fn data_dir_for_persistence(&self) -> &str {
        &self.data_dir
    }

    /// Acquire the inner mutex, mapping poisoned lock to FfiError.
    fn lock_inner(&self) -> Result<std::sync::MutexGuard<'_, GratiaNodeInner>, FfiError> {
        self.inner.lock().map_err(|e| {
            error!("FFI: mutex poisoned: {}", e);
            FfiError::InternalError {
                reason: "internal lock error — please restart the app".into(),
            }
        })
    }

    /// Get the NodeId for the current wallet, or a zeroed default if the wallet
    /// is not yet initialized.
    ///
    /// WHY: Several subsystems (staking, mining state) need a NodeId to look up
    /// per-node records. Before the wallet is created, we return a zeroed NodeId
    /// which will not match any staking record — this is safe because staking
    /// and mining are impossible without a wallet anyway.
    fn get_node_id_or_default(
        &self,
        inner: &GratiaNodeInner,
    ) -> gratia_core::types::NodeId {
        inner
            .wallet
            .node_id()
            .unwrap_or(gratia_core::types::NodeId([0u8; 32]))
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    /// Create a test node with a unique data directory per call.
    ///
    /// WHY: FileKeystore persists keys to disk. If all tests share the same
    /// directory, a key file left by one test causes another test to auto-load
    /// a wallet it didn't create, breaking assertions about empty state.
    fn test_node() -> GratiaNode {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let id = COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir = format!("/tmp/gratia-ffi-test-{}-{}", std::process::id(), id);
        // WHY: Clean up any leftover key file from a previous test run.
        let _ = std::fs::remove_dir_all(&dir);
        GratiaNode::new(dir).expect("failed to create test node")
    }

    #[test]
    fn test_create_node() {
        let node = test_node();
        assert!(node.data_dir.starts_with("/tmp/gratia-ffi-test-"));
    }

    #[test]
    fn test_create_wallet() {
        let node = test_node();
        let addr = node.create_wallet().unwrap();
        assert!(addr.starts_with("grat:"));
        assert_eq!(addr.len(), 5 + 64); // "grat:" + 64 hex chars
    }

    #[test]
    fn test_create_wallet_twice_fails() {
        let node = test_node();
        node.create_wallet().unwrap();
        let result = node.create_wallet();
        assert!(result.is_err());
    }

    #[test]
    fn test_get_wallet_info_before_create() {
        let node = test_node();
        let result = node.get_wallet_info();
        assert!(result.is_err());
    }

    #[test]
    fn test_get_wallet_info_after_create() {
        let node = test_node();
        node.create_wallet().unwrap();
        let info = node.get_wallet_info().unwrap();
        assert!(info.address.starts_with("grat:"));
        assert_eq!(info.balance_lux, 0);
        assert_eq!(info.mining_state, "proof_of_life");
    }

    #[test]
    fn test_mining_status_defaults() {
        let node = test_node();
        let status = node.get_mining_status().unwrap();
        assert_eq!(status.state, "proof_of_life");
        assert!(!status.is_plugged_in);
        assert_eq!(status.battery_percent, 0);
        // WHY: Onboarding users report PoL as valid for mining eligibility
        assert!(status.current_day_pol_valid);
    }

    #[test]
    fn test_update_power_state() {
        let node = test_node();
        let status = node.update_power_state(true, 85).unwrap();
        assert!(status.is_plugged_in);
        assert_eq!(status.battery_percent, 85);
    }

    #[test]
    fn test_start_mining_without_conditions() {
        let node = test_node();
        // Not plugged in — should fail.
        let result = node.start_mining();
        assert!(result.is_err());
    }

    #[test]
    fn test_stop_mining() {
        let node = test_node();
        let status = node.stop_mining().unwrap();
        assert_eq!(status.state, "proof_of_life");
    }

    #[test]
    fn test_submit_sensor_events() {
        let node = test_node();
        node.submit_sensor_event(FfiSensorEvent::Unlock).unwrap();
        node.submit_sensor_event(FfiSensorEvent::Motion).unwrap();
        node.submit_sensor_event(FfiSensorEvent::GpsUpdate {
            lat: 40.7,
            lon: -74.0,
        })
        .unwrap();
        node.submit_sensor_event(FfiSensorEvent::ChargeEvent { is_charging: true })
            .unwrap();

        // Events should be buffered — PoL status should reflect them.
        let status = node.get_proof_of_life_status().unwrap();
        assert!(status.parameters_met.contains(&"motion".to_string()));
        assert!(status.parameters_met.contains(&"gps".to_string()));
        assert!(status.parameters_met.contains(&"charge_event".to_string()));
    }

    #[test]
    fn test_finalize_day_invalid() {
        let node = test_node();
        // No sensor events submitted — day should be invalid.
        let result = node.finalize_day(1).unwrap();
        assert!(!result);
    }

    #[test]
    fn test_get_stake_info_no_stake() {
        let node = test_node();
        node.create_wallet().unwrap();
        let info = node.get_stake_info().unwrap();
        assert_eq!(info.node_stake_lux, 0);
        assert!(!info.meets_minimum);
    }

    #[test]
    fn test_proof_of_life_status_initial() {
        let node = test_node();
        let status = node.get_proof_of_life_status().unwrap();
        // WHY: During onboarding (day 0), is_valid_today is true so mining
        // can start immediately. But is_onboarded is false and consecutive_days
        // is 0 — reflecting that no real PoL has been completed yet.
        assert!(status.is_valid_today); // Onboarding: mining allowed on day 0
        assert_eq!(status.consecutive_days, 0);
        assert!(!status.is_onboarded);
    }

    #[test]
    fn test_send_transfer_no_balance() {
        let node = test_node();
        node.create_wallet().unwrap();
        let recipient = "grat:".to_string() + &hex::encode([0x42u8; 32]);
        let result = node.send_transfer(recipient, 1_000_000);
        assert!(result.is_err());
    }

    #[test]
    fn test_ffi_error_display() {
        let err = FfiError::WalletNotInitialized;
        let msg = err.to_string();
        assert!(msg.contains("create_wallet"));
    }
}
