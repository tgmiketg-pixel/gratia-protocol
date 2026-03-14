//! Error types for the governance subsystem.

use gratia_core::types::NodeId;
use thiserror::Error;

#[derive(Error, Debug)]
pub enum GovernanceError {
    #[error("insufficient PoL history: {days} days (minimum: {required} days)")]
    InsufficientHistory { days: u64, required: u64 },

    #[error("proposal not found: {id}")]
    ProposalNotFound { id: String },

    #[error("proposal is not in the expected phase: expected {expected}, got {actual}")]
    WrongPhase { expected: String, actual: String },

    #[error("node {node_id} has already voted on this proposal")]
    AlreadyVoted { node_id: NodeId },

    #[error("node does not have a valid Proof of Life for today")]
    NoValidProofOfLife,

    #[error("only the original proposer can cancel during discussion")]
    NotProposer,

    #[error("proposal cannot be cancelled outside the discussion phase")]
    CannotCancel,

    #[error("poll not found: {id}")]
    PollNotFound { id: String },

    #[error("poll has expired")]
    PollExpired,

    #[error("node {node_id} has already voted on this poll")]
    AlreadyVotedPoll { node_id: NodeId },

    #[error("invalid option index: {index} (poll has {count} options)")]
    InvalidOptionIndex { index: u32, count: usize },

    #[error("insufficient balance for poll creation fee: {available} Lux (required: {required} Lux)")]
    InsufficientBalance { available: u64, required: u64 },

    #[error("node is not on the validator committee")]
    NotOnCommittee,

    #[error("emergency proposal not found: {id}")]
    EmergencyProposalNotFound { id: String },

    #[error("emergency proposal has already been ratified or expired")]
    EmergencyAlreadyResolved,

    #[error("node location is outside the poll's geographic filter")]
    OutsideGeographicFilter,

    #[error("poll must have at least 2 options")]
    TooFewOptions,

    #[error("poll question must not be empty")]
    EmptyQuestion,
}
