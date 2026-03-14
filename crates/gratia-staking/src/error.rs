//! Error types for the staking subsystem.

use gratia_core::types::NodeId;
use thiserror::Error;

#[derive(Error, Debug)]
pub enum StakingError {
    #[error("insufficient stake: {available} Lux available, {required} Lux required")]
    InsufficientStake { available: u64, required: u64 },

    #[error("insufficient overflow: {available} Lux available, {requested} Lux requested")]
    InsufficientOverflow { available: u64, requested: u64 },

    #[error("invalid amount: {reason}")]
    InvalidAmount { reason: String },

    #[error("node not found: {node_id}")]
    NodeNotFound { node_id: NodeId },

    #[error("unstaking cooldown active: {remaining_secs}s remaining")]
    CooldownActive { remaining_secs: u64 },

    #[error("node is banned from staking")]
    NodeBanned { node_id: NodeId },

    #[error("node already has an active stake")]
    AlreadyStaked { node_id: NodeId },

    #[error("stake below minimum: {amount} Lux staked, {minimum} Lux required")]
    BelowMinimumStake { amount: u64, minimum: u64 },
}
