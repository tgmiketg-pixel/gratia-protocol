//! Network-specific error types for the Gratia networking layer.

use thiserror::Error;

#[derive(Error, Debug)]
pub enum NetworkError {
    #[error("Transport error: {0}")]
    Transport(String),

    #[error("Failed to dial peer: {0}")]
    DialFailure(String),

    #[error("Failed to listen on address: {0}")]
    ListenFailure(String),

    #[error("Gossipsub publish failed: {0}")]
    PublishError(String),

    #[error("Gossipsub subscription failed for topic '{topic}': {reason}")]
    SubscriptionError { topic: String, reason: String },

    #[error("Peer discovery failed: {0}")]
    DiscoveryError(String),

    #[error("Peer not found: {0}")]
    PeerNotFound(String),

    #[error("Sync error: {0}")]
    SyncError(String),

    #[error("Message too large: {size} bytes (max: {max} bytes)")]
    MessageTooLarge { size: usize, max: usize },

    #[error("Duplicate message: {0}")]
    DuplicateMessage(String),

    #[error("Invalid message: {0}")]
    InvalidMessage(String),

    #[error("Serialization error: {0}")]
    Serialization(String),

    #[error("Network not started")]
    NotStarted,

    #[error("Network already started")]
    AlreadyStarted,

    #[error("Connection limit reached: {current}/{max}")]
    ConnectionLimitReached { current: usize, max: usize },

    #[error("Bootstrap failed: no bootstrap peers reachable")]
    BootstrapFailed,

    #[error("Channel send error: {0}")]
    ChannelError(String),
}

impl From<bincode::Error> for NetworkError {
    fn from(e: bincode::Error) -> Self {
        NetworkError::Serialization(e.to_string())
    }
}
