#![no_main]
//! Fuzz target: GossipMessage and related type deserialization.
//!
//! Feeds random bytes to bincode::deserialize for all gossip-layer message types.
//! Must not panic on any input — invalid data should return Err, never crash.

use libfuzzer_sys::fuzz_target;

use gratia_network::gossip::{GossipMessage, NodeAnnouncement, ValidatorSignatureMessage};
use gratia_core::types::{Block, Transaction};

fuzz_target!(|data: &[u8]| {
    // Try deserializing as each gossip-layer type.
    // None of these should ever panic — only return Ok or Err.

    let _ = GossipMessage::from_bytes(data);

    let _ = bincode::deserialize::<NodeAnnouncement>(data);

    let _ = bincode::deserialize::<ValidatorSignatureMessage>(data);

    let _ = bincode::deserialize::<Block>(data);

    let _ = bincode::deserialize::<Transaction>(data);

    // Also try the raw bincode path for GossipMessage directly
    let _ = bincode::deserialize::<GossipMessage>(data);
});
