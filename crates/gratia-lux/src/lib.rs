//! # Lux — Decentralized Social Protocol
//!
//! Lux is a Twitter-style social protocol built on Gratia where every
//! participant is Proof-of-Life verified as a unique real human.
//!
//! ## Architecture
//!
//! - **On-chain:** Post anchors (hash + sig + timestamp), likes, reposts,
//!   moderation verdicts, ban records. Incorruptible.
//! - **Off-chain (DHT):** Post content, profiles, attachments.
//!   Tamper-detectable via on-chain hash anchors.
//!
//! ## Content Types
//!
//! The protocol is content-type agnostic from day one. V1 only renders
//! `text/plain`, but the infrastructure supports images, video, and
//! any future media type without protocol changes.

pub mod types;
pub mod store;
pub mod feed;
pub mod fees;
pub mod moderation;

pub use types::*;
pub use store::LuxStore;
pub use feed::FeedManager;
pub use fees::FeeCalculator;
pub use moderation::JurySystem;
