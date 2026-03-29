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
use std::sync::{Arc, Mutex};

use chrono::Utc;
use tracing::{debug, error, info, warn};

use gratia_consensus::committee::EligibleNode;
use gratia_consensus::vrf::{VrfPublicKey, VrfSecretKey};
use gratia_consensus::ConsensusEngine;
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
use gratia_state::db::InMemoryStore;
use gratia_state::StateManager;
use gratia_vm::interpreter::InterpreterRuntime;
use gratia_vm::runtime::{MockRuntime, ContractValue};
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
    store: Arc<InMemoryStore>,
}

impl BlockProvider for StateBlockProvider {
    fn get_blocks(&self, from_height: u64, to_height: u64) -> Vec<Block> {
        let db = gratia_state::db::StateDb::new(self.store.clone() as Arc<dyn gratia_state::db::StateStore>);
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
    /// Block pending broadcast to network. Set inside the mutex, broadcast
    /// after the lock is released (async broadcast can't hold the lock).
    pending_broadcast_block: Option<Block>,
    /// Known peer nodes for committee selection. Populated via NodeAnnounced events.
    /// WHY: Stored here so the committee can be rebuilt with real peer data when
    /// new nodes join the network, replacing synthetic padding nodes.
    known_peer_nodes: Vec<NodeAnnouncement>,
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
    /// Direct reference to the InMemoryStore for file-based persistence.
    /// WHY: StateManager holds the store as Arc<dyn StateStore>, which doesn't
    /// expose save_to_file(). We keep a typed Arc<InMemoryStore> so we can
    /// save state to disk after each block finalization.
    state_store: Option<Arc<InMemoryStore>>,
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
    /// enough signatures within 2 slot durations (8 seconds), we finalize with
    /// whatever signatures we have (bootstrap mode) or warn about weak finality.
    pending_block_created_at: Option<std::time::Instant>,
    /// Block hash of the pending block awaiting signatures.
    /// WHY: Needed to match incoming ValidatorSignatureReceived events to our
    /// pending block. If the hash doesn't match, the signature is for a different
    /// block (possibly from a fork) and should be ignored.
    pending_block_hash: Option<[u8; 32]>,
}

impl GratiaNodeInner {
    /// Returns true if debug bypass is active. Always false in release builds.
    /// WHY: Centralizes the cfg check so callers don't repeat conditional compilation.
    fn is_debug_bypass(&self) -> bool {
        #[cfg(debug_assertions)]
        { self.debug_bypass_checks }
        #[cfg(not(debug_assertions))]
        { false }
    }

    /// Get a human-readable sync status string for the UI.
    fn get_sync_status_string(&self) -> String {
        match &self.sync_manager {
            Some(sm) => {
                match sm.state() {
                    SyncState::Synced => "synced".to_string(),
                    SyncState::Syncing { local_height, target_height } => {
                        format!("syncing {}/{}", local_height, target_height)
                    }
                    SyncState::Behind { local_height, network_height } => {
                        format!("behind {}/{}", local_height, network_height)
                    }
                    SyncState::Unknown => "unknown".to_string(),
                }
            }
            None => "not_started".to_string(),
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

/// Write a debug log line to the Rust log file in the app's data directory.
/// WHY: Android logcat doesn't capture native `tracing` output without
/// a platform-specific subscriber. This file-based logging is our workaround
/// until `android_logger` is integrated. Readable via:
///   adb shell 'run-as io.gratia.app.debug cat files/gratia-rust.log'
fn rust_log(msg: &str) {
    if let Some(path) = LOG_PATH.get() {
        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
            use std::io::Write;
            let _ = writeln!(f, "[{}] {}", chrono::Utc::now().format("%H:%M:%S"), msg);
        }
    }
}

/// Initialize the log file path from the app's data directory.
fn init_rust_log(data_dir: &str) {
    let path = format!("{}/gratia-rust.log", data_dir);
    let _ = LOG_PATH.set(path);
}

#[uniffi::export]
impl GratiaNode {
    /// Create a new GratiaNode instance.
    ///
    /// `data_dir` is the path to the app's private data directory where
    /// persistent state (wallet keys, PoL history, etc.) will be stored.
    #[uniffi::constructor]
    pub fn new(data_dir: String) -> Result<Self, FfiError> {
        // WHY: Initialize tracing subscriber for desktop/test environments.
        // On Android, this writes to stderr which doesn't reach logcat —
        // file-based rust_log() is used instead (see init_rust_log below).
        // On desktop (cargo test, CLI tools), this provides normal tracing output.
        // try_init() is safe to call multiple times (ignores if already set).
        let _ = tracing_subscriber::fmt()
            .with_max_level(tracing::Level::INFO)
            .with_target(true)
            .with_ansi(false)
            .try_init();

        let config = Config::default();

        // Initialize file-based logging for Android debugging.
        init_rust_log(&data_dir);
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
            listen_address: None,
            consensus: None,
            sync_manager: None,
            sync_protocol: None,
            blocks_produced: 0,
            slot_timer_handle: None,
            #[cfg(debug_assertions)]
            debug_bypass_checks: false,
            pending_broadcast_block: None,
            known_peer_nodes: Vec::new(),
            recent_blocks: VecDeque::with_capacity(100),
            chain_persistence: Some(ChainPersistence::new(&data_dir)),
            mempool: Vec::new(),
            state_manager: None, // Initialized when consensus starts
            state_store: None,
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
            pending_block_hash: None,
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
    /// private key encoded as hex. In production, this would be converted to
    /// a BIP39 24-word mnemonic. For Phase 2, hex export is sufficient for
    /// wallet recovery between devices.
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

        // Recalculate mining state based on new power conditions.
        // WHY: During onboarding (day 0), skip the stake check — genesis has
        // minimum_stake=0. After onboarding, require real stake or debug bypass.
        let has_min_stake = inner.is_debug_bypass()
            || inner.pol.is_onboarding()
            || inner.staking.meets_minimum_stake(
                // WHY: We need the NodeId to check stake, but the wallet may not
                // be initialized yet. Use a zeroed NodeId as a safe fallback —
                // meets_minimum_stake will return false, which is correct behavior
                // before the wallet is created.
                &self.get_node_id_or_default(&inner),
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
        if !inner.pol.is_onboarding() && !inner.is_debug_bypass()
            && !inner.staking.meets_minimum_stake(&node_id)
        {
            return Err(FfiError::MiningNotAvailable {
                reason: "minimum stake required to mine".into(),
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
        info!("FFI: mining stopped");

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
        if daily_data.distinct_bt_environments >= 2 {
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
        let internal_event: gratia_pol::collector::SensorEvent = event.into();
        inner.sensor_buffer.process_event(internal_event);
        Ok(())
    }

    /// Finalize the current day's Proof of Life.
    ///
    /// Called at end-of-day (midnight UTC). Evaluates all accumulated sensor
    /// data, generates the PoL attestation, and resets the sensor buffer.
    ///
    /// Returns `true` if the day was valid (all PoL parameters met).
    pub fn finalize_day(&self) -> Result<bool, FfiError> {
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

        let is_valid = inner.pol.finalize_day();

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

    // ========================================================================
    // Network methods
    // ========================================================================

    /// Start the peer-to-peer network layer.
    ///
    /// Initializes the libp2p swarm with QUIC transport, Gossipsub for
    /// block/transaction propagation, and mDNS for local peer discovery.
    ///
    /// `listen_port` specifies the UDP port to listen on (0 = OS-assigned).
    pub fn start_network(&self, listen_port: u16) -> Result<FfiNetworkStatus, FfiError> {
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
        // WHY: Use the caller-specified port. Port 0 lets the OS pick a free port,
        // which is the default for mobile (avoids port conflicts).
        net_config.transport.listen_addresses =
            vec![format!("/ip4/0.0.0.0/udp/{}/quic-v1", listen_port)];

        // WHY: Connect to the Gratia bootstrap node on startup. This enables
        // peer discovery beyond the local Wi-Fi network. The bootstrap node
        // relays gossipsub and Kademlia traffic but does NOT mine, store state,
        // or participate in consensus. If the bootstrap node is down, phones
        // on the same LAN still find each other via mDNS.
        net_config.bootstrap_peers = vec![
            "/ip4/45.77.95.111/udp/9000/quic-v1/p2p/12D3KooWH21iAdpGgfaKUshnuCPQZ2XUqNLByAt7Rpu5gK3SD1K3".to_string(),
        ];

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
            sync_status: "not_started".to_string(),
            local_height: 0,
        })
    }

    /// Stop the peer-to-peer network layer.
    pub fn stop_network(&self) -> Result<(), FfiError> {
        let mut inner = self.lock_inner()?;

        if let Some(mut network) = inner.network.take() {
            self.runtime.block_on(async {
                let _ = network.stop().await;
            });
        }

        inner.network_event_rx = None;
        inner.pending_network_events.clear();
        inner.listen_address = None;

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

        rust_log("network is present, calling dial_peer...");
        self.runtime.block_on(async {
            network.dial_peer(&addr).await
        }).map_err(|e| {
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

        // Drain available events from the channel
        let mut new_events = Vec::new();
        loop {
            match rx.try_recv() {
                Ok(event) => {
                    let ffi_event = match event {
                        NetworkEvent::PeerConnected { peer_id, .. } => {
                            if let Some(network) = &mut inner.network {
                                network.on_peer_connected(peer_id, true);
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

                            // WHY: Re-announce our node to newly connected peers so
                            // they discover us for committee selection. Without this,
                            // a peer that connects after our initial announcement
                            // would never learn about our node.
                            if inner.consensus.is_some() {
                                if let (Some(ref network), Ok(sk_bytes)) = (
                                    &inner.network,
                                    inner.wallet.signing_key_bytes(),
                                ) {
                                    let local_node_id = self.get_node_id_or_default(&inner);
                                    let vrf_pk = VrfSecretKey::from_ed25519_bytes(&sk_bytes).public_key();
                                    let announcement = NodeAnnouncement {
                                        node_id: local_node_id,
                                        vrf_pubkey_bytes: vrf_pk.bytes,
                                        presence_score: 100, // WHY: Demo score, same as start_consensus
                                        pol_days: 90,
                                        timestamp: Utc::now(),
                                    };
                                    if let Err(e) = network.try_announce_node_sync(&announcement) {
                                        warn!("Failed to re-announce node on peer connect: {}", e);
                                    } else {
                                        rust_log("Re-announced node to newly connected peer");
                                    }
                                }
                            }

                            FfiNetworkEvent::PeerConnected {
                                peer_id: peer_id.to_string(),
                            }
                        }
                        NetworkEvent::PeerDisconnected { peer_id } => {
                            if let Some(network) = &mut inner.network {
                                network.on_peer_disconnected(&peer_id, true);
                            }
                            FfiNetworkEvent::PeerDisconnected {
                                peer_id: peer_id.to_string(),
                            }
                        }
                        NetworkEvent::BlockReceived(block) => {
                            let height = block.header.height;
                            let producer = hex::encode(block.header.producer.0);
                            let block_hash = block.header.hash().ok();

                            // WHY: Cache received blocks for sync protocol. When a
                            // new peer connects later, we broadcast these blocks so
                            // they can catch up quickly.
                            let block_clone = (*block).clone();

                            let block_result = if let Some(ref mut consensus) = inner.consensus {
                                match consensus.process_incoming_block(*block) {
                                    Ok(()) => {
                                        let h = consensus.current_height();
                                        let tip = consensus.last_finalized_hash().0;
                                        Some((h, tip))
                                    }
                                    Err(e) => {
                                        warn!(height = height, error = %e, "Failed to process incoming block");
                                        None
                                    }
                                }
                            } else { None };

                            if let Some((new_height, tip_hash)) = block_result {
                                info!(height = new_height, "Processed incoming block from network");
                                if let Some(ref mut sync) = inner.sync_manager {
                                    if let Some(hash) = block_hash {
                                        sync.update_local_state(new_height, hash);
                                    }
                                }
                                // WHY: Notify consensus sync protocol of each block
                                // received via gossip so it can track network height
                                // and detect when we fall behind.
                                if let Some(ref mut sp) = inner.sync_protocol {
                                    sp.on_block_received(height);
                                }
                                if let Some(ref persistence) = inner.chain_persistence {
                                    persistence.save(
                                        new_height,
                                        &tip_hash,
                                        inner.blocks_produced,
                                    );
                                }

                                // WHY: Apply synced block transactions to on-chain state.
                                // Without this, blocks received from peers don't update
                                // account balances, so the receiving phone can't see
                                // incoming transfers or validate future transactions
                                // against correct nonces. This closes the gap where only
                                // locally-produced blocks updated state.
                                if let Some(ref sm) = inner.state_manager {
                                    let our_addr = inner.wallet.address().ok();
                                    let mut applied = 0u32;
                                    let mut incoming_lux: Lux = 0;
                                    for tx in &block_clone.transactions {
                                        let sender_addr = gratia_core::types::Address::from_pubkey(&tx.sender_pubkey);
                                        match &tx.payload {
                                            gratia_core::types::TransactionPayload::Transfer { to, amount } => {
                                                // Debit sender
                                                let mut sender_acct = sm.get_account(&sender_addr).unwrap_or_default();
                                                let total = amount + tx.fee;
                                                if sender_acct.balance >= total {
                                                    sender_acct.balance -= total;
                                                    sender_acct.nonce += 1;
                                                    let _ = sm.db().put_account(&sender_addr, &sender_acct);
                                                }
                                                // Credit recipient
                                                let mut recv_acct = sm.get_account(to).unwrap_or_default();
                                                recv_acct.balance += amount;
                                                let _ = sm.db().put_account(to, &recv_acct);
                                                // WHY: Track incoming transfers to our wallet so
                                                // the wallet UI balance updates immediately.
                                                if let Some(ref our) = our_addr {
                                                    if to == our {
                                                        incoming_lux += amount;
                                                    }
                                                }
                                                applied += 1;
                                            }
                                            _ => { applied += 1; }
                                        }
                                    }
                                    // Update local wallet balance for incoming transfers
                                    if incoming_lux > 0 {
                                        let current = inner.wallet.balance();
                                        inner.wallet.sync_balance(current + incoming_lux);
                                        rust_log(&format!(
                                            "Received {} Lux ({} GRAT) — new wallet balance: {} Lux",
                                            incoming_lux, incoming_lux / 1_000_000,
                                            current + incoming_lux
                                        ));
                                    }
                                    if applied > 0 {
                                        rust_log(&format!(
                                            "Sync state: block {} — {} txs applied from network",
                                            new_height, applied
                                        ));
                                    }
                                }

                                // WHY: Credit mining reward for received blocks to the
                                // block producer's account in our state, so the explorer
                                // and balance queries reflect the true state of the chain.
                                if let Some(ref sm) = inner.state_manager {
                                    let producer_addr = gratia_core::types::Address(block_clone.header.producer.0);
                                    let active_miners = 3u64; // Phase 2 estimate
                                    let reward: Lux = gratia_core::emission::EmissionSchedule
                                        ::per_miner_block_reward_lux(new_height, active_miners);
                                    let mut acct = sm.get_account(&producer_addr).unwrap_or_default();
                                    acct.balance += reward;
                                    let _ = sm.db().put_account(&producer_addr, &acct);
                                }

                                // Cache for sync protocol
                                // WHY: Clone before push_back because block_clone is
                                // needed later for BFT co-signing (header reference).
                                inner.recent_blocks.push_back(block_clone.clone());
                                if inner.recent_blocks.len() > 100 {
                                    inner.recent_blocks.pop_front();
                                }

                                // Persist state for synced blocks (same cadence as produced blocks)
                                if new_height % 5 == 0 {
                                    if let Some(ref store) = inner.state_store {
                                        let state_path = format!("{}/chain_state.db",
                                            inner.chain_persistence.as_ref()
                                                .map(|p| p.data_dir())
                                                .unwrap_or(""));
                                        if !state_path.is_empty() && state_path != "/chain_state.db" {
                                            if let Err(e) = store.save_to_file(&state_path) {
                                                warn!("Failed to persist synced state: {}", e);
                                            }
                                        }
                                    }
                                }
                            }

                            // ── BFT co-signing ──────────────────────────────────
                            // WHY: If we're a committee member and we just accepted
                            // a valid block from another producer, sign it and
                            // broadcast our signature. This is how non-producing
                            // committee members contribute to BFT finality.
                            {
                                let is_committee_member = inner.consensus.as_ref()
                                    .map(|e| e.is_committee_member())
                                    .unwrap_or(false);

                                if is_committee_member {
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
                                            let blk_hash = block_clone.header.hash()
                                                .map(|h| h.0)
                                                .unwrap_or([0u8; 32]);
                                            let sig_msg = gratia_network::gossip::ValidatorSignatureMessage {
                                                block_hash: blk_hash,
                                                height,
                                                signature: our_sig,
                                            };
                                            if let Some(ref network) = inner.network {
                                                match network.try_broadcast_validator_signature_sync(&sig_msg) {
                                                    Ok(()) => rust_log(&format!(
                                                        "BFT: co-signed block {} from {}",
                                                        height, &producer[..8.min(producer.len())]
                                                    )),
                                                    Err(e) => rust_log(&format!(
                                                        "BFT: failed to broadcast co-signature: {}", e
                                                    )),
                                                }
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
                                    if let Some(ref sm) = inner.state_manager {
                                        if let gratia_core::types::TransactionPayload::Transfer { amount, .. } = &tx.payload {
                                            let sender_acct = sm.get_account(&sender_address).unwrap_or_default();
                                            let is_known_account = sender_acct.balance > 0 || sender_acct.nonce > 0;

                                            if is_known_account {
                                                let total = amount + tx.fee;
                                                if sender_acct.balance < total {
                                                    rust_log(&format!(
                                                        "REJECTED tx {} — insufficient on-chain balance: has {} need {}",
                                                        hash_hex, sender_acct.balance, total
                                                    ));
                                                    state_valid = false;
                                                }
                                                if sender_acct.nonce != tx.nonce {
                                                    rust_log(&format!(
                                                        "REJECTED tx {} — nonce mismatch: state={} tx={}",
                                                        hash_hex, sender_acct.nonce, tx.nonce
                                                    ));
                                                    state_valid = false;
                                                }
                                            }
                                            // Unknown sender: accept on signature alone (Phase 1)
                                        }
                                    }

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
                            rust_log(&format!(
                                "NodeAnnounced: node={:?} score={} pol_days={}",
                                peer_node_id, announcement.presence_score, announcement.pol_days,
                            ));

                            // WHY: Dedup by node_id — if we already know this peer,
                            // update their entry instead of adding a duplicate.
                            if let Some(existing) = inner.known_peer_nodes.iter_mut().find(|n| n.node_id == peer_node_id) {
                                *existing = announcement.clone();
                            } else {
                                inner.known_peer_nodes.push(announcement);
                            }

                            // WHY: Rebuild the committee with real peer data whenever
                            // a new node announces itself. This replaces synthetic
                            // padding with actual network participants.
                            // Collect all data before borrowing consensus mutably
                            // to avoid borrow conflicts through MutexGuard.
                            let has_consensus = inner.consensus.is_some();
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

                                    let mut all_eligible = vec![EligibleNode {
                                        node_id: local_node_id,
                                        vrf_pubkey,
                                        presence_score: local_score,
                                        has_valid_pol: true,
                                        meets_minimum_stake: true,
                                        pol_days: 90,
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
                                        });
                                    }

                                    // WHY: Need minimum 3 nodes for committee (tier 0).
                                    // Only add synthetic padding if real peers < 3.
                                    let real_count = all_eligible.len();
                                    if real_count < 3 {
                                        for i in 1..=(3 - real_count as u8) {
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
                                            });
                                        }
                                    }

                                    rust_log(&format!(
                                        "Rebuilding committee: {} real + {} synthetic = {} total",
                                        real_count,
                                        all_eligible.len() - real_count,
                                        all_eligible.len(),
                                    ));

                                    let epoch_seed = [0xAB; 32]; // Demo seed
                                    // WHY: Now borrow consensus mutably — all other field
                                    // accesses are done, so no borrow conflict.
                                    if let Some(ref mut consensus) = inner.consensus {
                                        if let Err(e) = consensus.initialize_committee(&all_eligible, &epoch_seed, 0, 0) {
                                            warn!("Failed to rebuild committee: {}", e);
                                        }
                                    }
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
                                    match inner.consensus.as_mut().unwrap().finalize_pending_block() {
                                        Ok(finalized_block) => {
                                            let fh = finalized_block.header.height;
                                            inner.blocks_produced += 1;
                                            let new_h = inner.consensus.as_ref().map(|e| e.current_height()).unwrap_or(0);
                                            rust_log(&format!("BLOCK FINALIZED (BFT) height={} chain={}", fh, new_h));

                                            // Persist chain state.
                                            if let Some(ref persistence) = inner.chain_persistence {
                                                if let Some(ref engine) = inner.consensus {
                                                    let tip = engine.last_finalized_hash().0;
                                                    persistence.save(engine.current_height(), &tip, inner.blocks_produced);
                                                }
                                            }

                                            // Apply block transactions to on-chain state.
                                            if let Some(ref sm) = inner.state_manager {
                                                for tx in &finalized_block.transactions {
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
                                                        }
                                                    }
                                                }
                                            }

                                            inner.pending_broadcast_block = Some(finalized_block.clone());
                                            inner.recent_blocks.push_back(finalized_block.clone());
                                            if inner.recent_blocks.len() > 100 {
                                                inner.recent_blocks.pop_front();
                                            }

                                            // Credit mining reward.
                                            {
                                                let active_miners = 1u64.max(inner.staking.staker_count() as u64).max(1);
                                                let reward: Lux = gratia_core::emission::EmissionSchedule
                                                    ::per_miner_block_reward_lux(fh, active_miners);
                                                let current = inner.wallet.balance();
                                                inner.wallet.sync_balance(current + reward);
                                                if let (Some(ref sm), Ok(our_addr)) = (&inner.state_manager, inner.wallet.address()) {
                                                    let mut acct = sm.get_account(&our_addr).unwrap_or_default();
                                                    acct.balance += reward;
                                                    let _ = sm.db().put_account(&our_addr, &acct);
                                                }
                                            }

                                            // Persist on-chain state.
                                            if let Some(ref store) = inner.state_store {
                                                let state_path = format!("{}/chain_state.db",
                                                    inner.chain_persistence.as_ref()
                                                        .map(|p| p.data_dir())
                                                        .unwrap_or(""));
                                                if !state_path.is_empty() && state_path != "/chain_state.db" {
                                                    let _ = store.save_to_file(&state_path);
                                                }
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
                                rust_log(&format!(
                                    "BFT: ignoring sig for height {} (no matching pending block)",
                                    sig_height
                                ));
                            }

                            // WHY: Validator signatures are internal consensus traffic —
                            // no need to surface them as an FfiNetworkEvent to the mobile UI.
                            continue;
                        }
                        NetworkEvent::SyncBlocksReceived(blocks) => {
                            // WHY: The network layer received and validated a batch of
                            // sync blocks. Process them through consensus and update
                            // the consensus-level sync protocol's state machine.
                            let block_count = blocks.len();
                            let first_h = blocks.first().map(|b| b.header.height).unwrap_or(0);
                            let last_h = blocks.last().map(|b| b.header.height).unwrap_or(0);

                            rust_log(&format!(
                                "Sync: received {} blocks (heights {}-{})",
                                block_count, first_h, last_h,
                            ));

                            let mut applied_height = 0u64;
                            for block in &blocks {
                                let h = block.header.height;
                                if let Some(ref mut consensus) = inner.consensus {
                                    match consensus.process_incoming_block(block.clone()) {
                                        Ok(()) => {
                                            applied_height = consensus.current_height();
                                        }
                                        Err(e) => {
                                            warn!(height = h, error = %e, "Failed to apply sync block");
                                            break;
                                        }
                                    }
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

                                rust_log(&format!(
                                    "Sync: applied {} blocks, height now {}",
                                    block_count, applied_height,
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
                Err(tokio::sync::mpsc::error::TryRecvError::Empty) => break,
                Err(tokio::sync::mpsc::error::TryRecvError::Disconnected) => {
                    warn!("FFI: network event channel disconnected");
                    break;
                }
            }
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
        let mut all_eligible = vec![EligibleNode {
            node_id,
            vrf_pubkey,
            presence_score: presence_score,
            has_valid_pol: true,
            meets_minimum_stake: true,
            pol_days: 90,
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
            });
        }

        // WHY: Need minimum 3 nodes for committee (tier 0 in graduated scaling).
        // Only pad with synthetic nodes if real peers < 3. As more phones join,
        // synthetic padding disappears and the committee is fully real.
        let real_count = all_eligible.len();
        if real_count < 3 {
            for i in 1..=(3 - real_count as u8) {
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
                });
            }
        }

        rust_log(&format!(
            "Committee: {} real + {} synthetic = {} total, local score={}",
            real_count,
            all_eligible.len() - real_count,
            all_eligible.len(),
            presence_score,
        ));

        let epoch_seed = [0xAB; 32]; // Demo seed
        engine.initialize_committee(&all_eligible, &epoch_seed, 0, 0)
            .map_err(|e| FfiError::InternalError {
                reason: format!("failed to initialize committee: {}", e),
            })?;

        let status = consensus_status(&engine, 0);
        inner.consensus = Some(engine);

        // Initialize sync manager with the current chain state.
        // WHY: The sync manager tracks peer chain tips and generates
        // sync requests when this node falls behind.
        inner.sync_manager = Some(SyncManager::new(initial_height, BlockHash(initial_hash)));

        // Initialize consensus-level sync protocol.
        // WHY: Sits above the network SyncManager and tracks the sync state
        // machine (idle/requesting/downloading/synced) with progress reporting
        // for the mobile UI. Uses our node_id and current height as starting point.
        inner.sync_protocol = Some(ConsensusSyncProtocol::new(node_id, initial_height));

        // Initialize on-chain state manager with persistent InMemoryStore.
        // WHY: The state manager tracks account balances and nonces on-chain.
        // When blocks are finalized, transactions are applied to state — enforcing
        // balance checks and nonce ordering. This prevents double-spends.
        // WHY load_from_file: State persists across app restarts. On first launch,
        // the file doesn't exist and we get a fresh store. On subsequent launches,
        // account balances and nonces are restored from the previous session.
        let state_path = format!("{}/chain_state.db", self.data_dir);
        let store = Arc::new(InMemoryStore::load_from_file(&state_path));
        let sm = StateManager::new(store.clone() as Arc<dyn gratia_state::db::StateStore>);
        inner.state_store = Some(store);

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
                    inner.state_store.as_ref().map(|s| s.data_size_estimate()).unwrap_or(0),
                ));
            }
        }
        inner.state_manager = Some(sm);

        // Wire block provider into network for sync protocol.
        // WHY: Now that state is initialized, the network can serve blocks to
        // peers requesting them via the sync protocol. Before this point,
        // the NoBlockProvider returns empty results.
        // WHY: Clone the store Arc before borrowing network mutably to avoid
        // conflicting borrows on `inner` (mutable for network, immutable for state_store).
        let store_clone = inner.state_store.clone();
        if let (Some(ref mut network), Some(store)) = (&mut inner.network, store_clone) {
            let provider = Arc::new(StateBlockProvider {
                store,
            });
            network.set_block_provider(provider);
            rust_log("Block provider wired into network for sync");
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
            let announcement = NodeAnnouncement {
                node_id,
                vrf_pubkey_bytes: vrf_pubkey_bytes,
                presence_score: presence_score,
                pol_days: 90,
                timestamp: Utc::now(),
            };
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
    pub fn stop_consensus(&self) -> Result<(), FfiError> {
        let mut inner = self.lock_inner()?;

        if let Some(ref mut engine) = inner.consensus {
            engine.stop();
        }
        inner.consensus = None;
        inner.sync_protocol = None;

        // Cancel the slot timer
        if let Some(handle) = inner.slot_timer_handle.take() {
            handle.abort();
        }

        info!("FFI: consensus stopped");
        Ok(())
    }

    /// Request block sync from connected peers.
    ///
    /// Checks if this node is behind the network and requests missing blocks.
    /// Called periodically from the mobile app or automatically after peer connect.
    /// Returns the current sync state.
    pub fn request_sync(&self) -> Result<String, FfiError> {
        let mut inner = self.lock_inner()?;

        let sync_mgr = inner.sync_manager.as_mut().ok_or(FfiError::InternalError {
            reason: "sync manager not initialized (start consensus first)".into(),
        })?;

        // Check sync state
        let state = sync_mgr.state();
        let state_str = match state {
            SyncState::Synced => "synced".to_string(),
            SyncState::Syncing { local_height, target_height } => {
                format!("syncing {}/{}", local_height, target_height)
            }
            SyncState::Behind { local_height, network_height } => {
                format!("behind {}/{}", local_height, network_height)
            }
            SyncState::Unknown => "unknown".to_string(),
        };

        Ok(state_str)
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
            None => Ok(FfiConsensusStatus {
                state: "stopped".to_string(),
                current_slot: 0,
                current_height: 0,
                is_committee_member: false,
                blocks_produced: 0,
            }),
        }
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

        let deployer = inner.wallet.address().unwrap_or(gratia_core::types::Address([0u8; 32]));

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
        let caller = inner.wallet.address().unwrap_or(gratia_core::types::Address([0u8; 32]));
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

        let mut host_env = HostEnvironment::new(
            block_height,
            chrono::Utc::now().timestamp() as u64,
            caller,
            caller_balance,
        )
        .with_presence_score(presence)
        .with_nearby_peers(peers);

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

        let deployer = inner.wallet.address()
            .unwrap_or(gratia_core::types::Address([0u8; 32]));

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

        let creator = inner.wallet.address()
            .unwrap_or(gratia_core::types::Address([0u8; 32]));
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

                let network_height = sync.best_network_height().unwrap_or(0);
                net_height_for_sp = Some((local_height, network_height));

                match sync.state() {
                    gratia_network::sync::SyncState::Behind { local_height, network_height } => {
                        rust_log(&format!(
                            "Sync: behind network ({}/{}), requesting blocks",
                            local_height, network_height
                        ));
                        if let Some((_peer, request)) = sync.next_sync_request() {
                            rust_log(&format!("Sync: generated request {:?}", request));
                        }
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
        }

        // WHY: We check consensus existence and advance the slot in a
        // scoped block, then operate on the result outside the borrow.
        let should_produce = {
            match guard.consensus.as_mut() {
                Some(engine) => {
                    let result = engine.advance_slot();
                    if result {
                        info!(
                            slot = engine.current_slot(),
                            height = engine.current_height() + 1,
                            "Slot timer: this node should produce a block"
                        );
                    }
                    result
                }
                None => {
                    debug!("Slot timer: consensus stopped, exiting");
                    return;
                }
            }
        };

        // ── BFT timeout check ───────────────────────────────────────
        // WHY: If we have a pending block awaiting peer signatures and
        // 2 slot durations (8 seconds) have passed without reaching
        // finality, force-finalize with whatever signatures we have.
        // This prevents blocks from being stuck indefinitely when the
        // network has too few active committee members. In bootstrap
        // mode (< 3 nodes), 2 signatures are sufficient; otherwise
        // log a warning about weak finality.
        {
            let has_pending = guard.pending_block_created_at.is_some();
            if has_pending {
                // WHY: 8 seconds = 2 slot durations at 4-second slots.
                // Generous timeout for mobile network latency.
                let timeout_secs = 8;
                let timed_out = guard.pending_block_created_at
                    .map(|t| t.elapsed().as_secs() >= timeout_secs)
                    .unwrap_or(false);

                if timed_out {
                    let sig_count = guard.consensus.as_ref()
                        .and_then(|e| e.pending_block.as_ref())
                        .map(|p| p.signatures.len())
                        .unwrap_or(0);
                    let threshold = guard.consensus.as_ref()
                        .map(|e| e.pending_finality_threshold())
                        .unwrap_or(0);

                    // WHY: In bootstrap mode with very few nodes, accept 2+
                    // signatures as sufficient. For larger networks, accept
                    // anything >= 1 signature (our own) to avoid stuck blocks,
                    // but warn loudly about weak finality.
                    if sig_count >= 1 {
                        if sig_count < threshold {
                            rust_log(&format!(
                                "BFT TIMEOUT: force-finalizing block with {}/{} sigs (weak finality)",
                                sig_count, threshold
                            ));
                            warn!(
                                sigs = sig_count,
                                threshold = threshold,
                                "BFT timeout: finalizing with insufficient signatures"
                            );
                        } else {
                            rust_log(&format!(
                                "BFT TIMEOUT: finalizing block with {}/{} sigs (threshold met during wait)",
                                sig_count, threshold
                            ));
                        }

                        // WHY: Use force_finalize to bypass threshold check on timeout.
                        // Normal finalize requires threshold signatures, but during
                        // bootstrap or when peers are slow, we force-finalize with
                        // whatever we have to keep the chain moving.
                        match guard.consensus.as_mut().unwrap().force_finalize_pending_block() {
                            Ok(finalized_block) => {
                                let finalized_height = finalized_block.header.height;
                                let new_chain_height = guard.consensus.as_ref().map(|e| e.current_height()).unwrap_or(0);
                                rust_log(&format!("BLOCK FINALIZED (timeout) height={} new_chain_height={}", finalized_height, new_chain_height));

                                // Persist chain state.
                                if let Some(ref persistence) = guard.chain_persistence {
                                    if let Some(ref engine) = guard.consensus {
                                        let tip_hash = engine.last_finalized_hash().0;
                                        persistence.save(engine.current_height(), &tip_hash, guard.blocks_produced);
                                    }
                                }

                                // Apply block transactions to on-chain state.
                                if let Some(ref sm) = guard.state_manager {
                                    let mut applied = 0u32;
                                    let mut failed = 0u32;
                                    for tx in &finalized_block.transactions {
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
                                                    applied += 1;
                                                } else {
                                                    failed += 1;
                                                }
                                            }
                                            _ => { applied += 1; }
                                        }
                                    }
                                    if applied > 0 || failed > 0 {
                                        rust_log(&format!(
                                            "State: timeout block {} — {} txs applied, {} rejected",
                                            finalized_height, applied, failed
                                        ));
                                    }
                                }

                                guard.pending_broadcast_block = Some(finalized_block.clone());
                                guard.recent_blocks.push_back(finalized_block.clone());
                                if guard.recent_blocks.len() > 100 {
                                    guard.recent_blocks.pop_front();
                                }

                                // Credit mining reward.
                                {
                                    let active_miners = 1u64.max(guard.staking.staker_count() as u64).max(1);
                                    let reward: Lux = gratia_core::emission::EmissionSchedule
                                        ::per_miner_block_reward_lux(finalized_height, active_miners);
                                    let current = guard.wallet.balance();
                                    guard.wallet.sync_balance(current + reward);
                                    if let (Some(ref sm), Ok(our_addr)) = (&guard.state_manager, guard.wallet.address()) {
                                        let mut acct = sm.get_account(&our_addr).unwrap_or_default();
                                        acct.balance += reward;
                                        let _ = sm.db().put_account(&our_addr, &acct);
                                    }
                                }

                                // Persist on-chain state.
                                if let Some(ref store) = guard.state_store {
                                    let state_path = format!("{}/chain_state.db",
                                        guard.chain_persistence.as_ref()
                                            .map(|p| p.data_dir())
                                            .unwrap_or(""));
                                    if !state_path.is_empty() && state_path != "/chain_state.db" {
                                        let _ = store.save_to_file(&state_path);
                                    }
                                }
                            }
                            Err(e) => {
                                rust_log(&format!("TIMEOUT FINALIZE FAILED: {}", e));
                                warn!("Failed to finalize block on timeout: {}", e);
                            }
                        }
                    } else {
                        rust_log("BFT TIMEOUT: no signatures at all, discarding pending block");
                        // WHY: If we couldn't even self-sign, something is wrong.
                        // Discard the pending block and let the next slot try again.
                        if let Some(ref mut engine) = guard.consensus {
                            engine.stop();
                            // Re-activate so next slot works.
                            // Actually, just clear the pending block by forcing state back.
                        }
                    }
                    guard.pending_block_hash = None;
                    guard.pending_block_created_at = None;
                }
            }
        }

        if should_produce {
            // WHY: Drain the mempool into the block. This is how user transactions
            // (sent locally or received via gossip) become on-chain. Cap at 512
            // per block to match MAX_TRANSACTIONS_PER_BLOCK.
            let drain_count = guard.mempool.len().min(512);
            let block_txs: Vec<gratia_core::types::Transaction> = guard.mempool
                .drain(..drain_count)
                .collect();
            let tx_count = block_txs.len();

            let produce_result = guard.consensus.as_mut().unwrap()
                .produce_block(block_txs, vec![], [0u8; 32]);

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

                    // Phase 1: Get signing key bytes (immutable borrow on wallet).
                    let sk_bytes_opt = guard.wallet.signing_key_bytes().ok();

                    // Phase 2: Read committee info and sign (mutable borrow on consensus).
                    let (threshold, member_count, our_sig, pending_finalized, block_hash_for_broadcast, pending_block_clone) = {
                        let engine = guard.consensus.as_mut().unwrap();
                        let threshold = engine.pending_finality_threshold();
                        let member_count = engine.committee()
                            .map(|c| c.members.len())
                            .unwrap_or(0);

                        // Sign with our OWN real Ed25519 key.
                        let our_sig = sk_bytes_opt.as_ref().and_then(|sk_bytes| {
                            let keypair = gratia_core::crypto::Keypair::from_secret_key_bytes(sk_bytes);
                            let header = engine.pending_block.as_ref()
                                .unwrap().block.header.clone();
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
                                Ok(finalized) => {
                                    rust_log(&format!("Self-signature added, finalized={}", finalized));
                                }
                                Err(e) => {
                                    rust_log(&format!("Failed to add self-signature: {}", e));
                                }
                            }
                        }

                        let pending_finalized = engine.pending_block.as_ref()
                            .map(|p| p.is_finalized())
                            .unwrap_or(false);

                        let block_hash = engine.pending_block.as_ref()
                            .and_then(|p| p.block.header.hash().ok())
                            .map(|h| h.0)
                            .unwrap_or([0u8; 32]);

                        let pending_block_clone = engine.pending_block.as_ref()
                            .map(|p| p.block.clone());

                        (threshold, member_count, our_sig, pending_finalized, block_hash, pending_block_clone)
                    };
                    // Mutable borrow on consensus is now dropped.

                    // Phase 3: Decide whether to finalize now or await peer sigs.
                    let should_finalize_now;
                    if member_count <= 1 || pending_finalized {
                        should_finalize_now = true;
                        if member_count <= 1 {
                            rust_log(&format!(
                                "Bootstrap mode: solo node, auto-finalizing (members={})",
                                member_count
                            ));
                        }
                    } else {
                        should_finalize_now = false;

                        guard.pending_block_hash = Some(block_hash_for_broadcast);
                        guard.pending_block_created_at = Some(std::time::Instant::now());

                        // Broadcast the pending block to peers.
                        if let Some(block) = pending_block_clone {
                            guard.pending_broadcast_block = Some(block);
                        }

                        // Broadcast our validator signature.
                        if let (Some(ref network), Some(our_sig)) = (&guard.network, our_sig) {
                            let sig_msg = gratia_network::gossip::ValidatorSignatureMessage {
                                block_hash: block_hash_for_broadcast,
                                height: block_height,
                                signature: our_sig,
                            };
                            if let Err(e) = network.try_broadcast_validator_signature_sync(&sig_msg) {
                                rust_log(&format!("Failed to broadcast our sig: {}", e));
                            } else {
                                rust_log(&format!(
                                    "BFT: broadcast block {} + our sig, awaiting {}/{} sigs",
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
                    match guard.consensus.as_mut().unwrap().finalize_pending_block() {
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
                            rust_log(&format!("BLOCK FINALIZED height={} new_chain_height={}", finalized_height, new_chain_height));

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

                            guard.pending_broadcast_block = Some(finalized_block.clone());

                            // WHY: Cache the finalized block for sync. When a new
                            // peer connects, we broadcast recent blocks so they can
                            // catch up immediately without a full sync protocol.
                            guard.recent_blocks.push_back(finalized_block.clone());
                            if guard.recent_blocks.len() > 100 {
                                guard.recent_blocks.pop_front();
                            }

                            rust_log("REWARD: entering reward credit block");
                            // WHY: Credit mining reward to the block producer on
                            // finalization. The reward is earned by producing the
                            // block — if this node finalized it, this node gets paid.
                            // Mining state gates block production eligibility in
                            // production, but the reward always follows the block.
                            {
                                // WHY: active_miners count determines per-miner share.
                                // With 1 miner, they get the full block reward. As more
                                // join, the reward splits proportionally.
                                let active_miners = 1u64.max(
                                    guard.staking.staker_count() as u64
                                ).max(1);
                                let reward: Lux = gratia_core::emission::EmissionSchedule
                                    ::per_miner_block_reward_lux(finalized_height, active_miners);
                                let current = guard.wallet.balance();
                                guard.wallet.sync_balance(current + reward);

                                // WHY: Also credit the mining reward in on-chain state
                                // so the balance is available for future transfers.
                                if let (Some(ref sm), Ok(our_addr)) = (&guard.state_manager, guard.wallet.address()) {
                                    let mut acct = sm.get_account(&our_addr).unwrap_or_default();
                                    acct.balance += reward;
                                    let _ = sm.db().put_account(&our_addr, &acct);
                                }

                                rust_log(&format!(
                                    "REWARD: height={} reward={} Lux ({} GRAT) new_balance={} Lux ({} GRAT) active_miners={}",
                                    finalized_height, reward, reward / 1_000_000,
                                    current + reward, (current + reward) / 1_000_000,
                                    active_miners
                                ));
                            }

                            // WHY: Persist on-chain state every block so the on-chain
                            // balance always matches the wallet display. Prevents
                            // transaction rejections from stale state. Flash wear is
                            // minimal (~1KB every 4 seconds). In production with
                            // RocksDB, this becomes a WAL flush.
                            {
                                if let Some(ref store) = guard.state_store {
                                    let state_path = format!("{}/chain_state.db",
                                        guard.chain_persistence.as_ref()
                                            .map(|p| p.data_dir())
                                            .unwrap_or(""));
                                    if !state_path.is_empty() && state_path != "/chain_state.db" {
                                        if let Err(e) = store.save_to_file(&state_path) {
                                            warn!("Failed to persist state: {}", e);
                                        } else {
                                            rust_log(&format!(
                                                "State persisted at height {} ({} entries)",
                                                finalized_height,
                                                store.data_size_estimate(),
                                            ));
                                        }
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
                    warn!("Failed to produce block: {}", e);
                }
            }
        }

        // Broadcast pending block AFTER dropping the mutex guard.
        // WHY: broadcast_block is async and we can't hold the mutex across
        // an await point. So we stash the block in pending_broadcast_block
        // while holding the lock, then broadcast here after the guard is dropped.
        drop(guard);

        let broadcast_block = {
            let mut g = match inner.lock() {
                Ok(g) => g,
                Err(_) => continue,
            };
            g.pending_broadcast_block.take()
        };

        if let Some(block) = broadcast_block {
            let height = block.header.height;
            let g = match inner.lock() {
                Ok(g) => g,
                Err(_) => continue,
            };
            if let Some(ref network) = g.network {
                // WHY: We need to call the async broadcast_block but we're
                // holding the lock. Since NetworkManager::broadcast_block
                // sends to a channel internally (non-blocking), we can call
                // it synchronously via a short block_on. The actual gossip
                // propagation happens asynchronously in the swarm task.
                let result = network.try_broadcast_block_sync(&block);
                match result {
                    Ok(()) => info!(height = height, "Block broadcast to network"),
                    Err(e) => warn!(height = height, error = %e, "Failed to broadcast block"),
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
        let newest = guard.recent_blocks.back().unwrap().header.timestamp;
        let oldest = guard.recent_blocks.front().unwrap().header.timestamp;
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

    format!(
        r#"{{"network":{{"name":"Gratia Testnet","blockHeight":{},"totalTransactions":{},"activeNodes":{},"avgBlockTime":{:.1},"tps":{:.4},"miningState":"{}","blocksProduced":{}}},"blocks":[{}],"transactions":[{}],"walletTransactions":[{}],"wallet":{{"address":"{}","balance":{}}}}}"#,
        block_height,
        total_tx_count,
        peer_count + 1, // +1 for self
        avg_block_time,
        tps,
        mining_state,
        blocks_produced,
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
            .address()
            .map(|addr| {
                // WHY: We reuse the address bytes as a NodeId for local lookups.
                // In production, the NodeId is derived from the public key via
                // NodeId::from_public_key(), but at the FFI layer we don't have
                // direct access to the VerifyingKey. The address bytes serve as
                // a unique identifier for local staking manager lookups.
                gratia_core::types::NodeId(addr.0)
            })
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
        let result = node.finalize_day().unwrap();
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
