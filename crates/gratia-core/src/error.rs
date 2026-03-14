//! Error types for the Gratia protocol.

use thiserror::Error;

#[derive(Error, Debug)]
pub enum GratiaError {
    // Proof of Life errors
    #[error("Proof of Life validation failed: {reason}")]
    ProofOfLifeInvalid { reason: String },

    #[error("Insufficient unlock events: {count} (minimum: {required})")]
    InsufficientUnlocks { count: u32, required: u32 },

    #[error("Unlock spread too narrow: {hours}h (minimum: {required}h)")]
    UnlockSpreadTooNarrow { hours: u32, required: u32 },

    #[error("No charge cycle event detected in 24-hour window")]
    NoChargeCycleEvent,

    #[error("Insufficient Bluetooth environment variation")]
    InsufficientBtVariation,

    #[error("Node is in onboarding period (day {current} of {required})")]
    OnboardingIncomplete { current: u32, required: u32 },

    // Mining errors
    #[error("Mining conditions not met: {reason}")]
    MiningConditionsNotMet { reason: String },

    #[error("Phone not plugged in")]
    NotPluggedIn,

    #[error("Battery below threshold: {percent}% (minimum: {required}%)")]
    BatteryTooLow { percent: u8, required: u8 },

    #[error("CPU temperature too high: {temp}°C (maximum: {max}°C)")]
    ThermalThrottle { temp: f32, max: f32 },

    // Staking errors
    #[error("Insufficient stake: {amount} Lux (minimum: {required} Lux)")]
    InsufficientStake { amount: u64, required: u64 },

    #[error("Unstaking cooldown active: {remaining_secs} seconds remaining")]
    UnstakeCooldownActive { remaining_secs: u64 },

    // Transaction errors
    #[error("Insufficient balance: {available} Lux (required: {required} Lux)")]
    InsufficientBalance { available: u64, required: u64 },

    #[error("Invalid transaction signature")]
    InvalidSignature,

    #[error("Transaction nonce mismatch: expected {expected}, got {got}")]
    NonceMismatch { expected: u64, got: u64 },

    #[error("Invalid ZK proof: {reason}")]
    InvalidZkProof { reason: String },

    // Consensus errors
    #[error("Block validation failed: {reason}")]
    BlockValidationFailed { reason: String },

    #[error("Invalid VRF proof")]
    InvalidVrfProof,

    #[error("Insufficient validator signatures: {count}/{required}")]
    InsufficientSignatures { count: usize, required: usize },

    // Governance errors
    #[error("Insufficient participation history: {days} days (minimum: {required} days)")]
    InsufficientHistory { days: u64, required: u64 },

    #[error("Quorum not met: {participation_bps} bps (required: {required_bps} bps)")]
    QuorumNotMet { participation_bps: u32, required_bps: u32 },

    #[error("Already voted on this proposal")]
    AlreadyVoted,

    // Wallet errors
    #[error("Wallet locked: biometric authentication required")]
    WalletLocked,

    #[error("Recovery claim pending on this wallet")]
    RecoveryClaimPending,

    #[error("Behavioral matching confidence below threshold: {score}% (required: {required}%)")]
    BehavioralMatchFailed { score: u32, required: u32 },

    // Network errors
    #[error("Peer not found: {node_id}")]
    PeerNotFound { node_id: String },

    #[error("Shard not available: {shard_id}")]
    ShardNotAvailable { shard_id: u16 },

    // Storage errors
    #[error("State database error: {0}")]
    StorageError(String),

    #[error("State database exceeds size limit")]
    StorageLimitExceeded,

    // Serialization
    #[error("Serialization error: {0}")]
    SerializationError(String),

    // Generic
    #[error("{0}")]
    Other(String),
}
