//! Gratia Core - Foundational types and traits for the Gratia protocol.
//!
//! This crate defines the core data structures, traits, and configuration
//! that all other Gratia crates depend on. It contains no business logic —
//! only type definitions, serialization, and shared constants.

pub mod types;
pub mod config;
pub mod emission;
pub mod error;
pub mod crypto;

pub use types::*;
pub use config::Config;
pub use error::GratiaError;
