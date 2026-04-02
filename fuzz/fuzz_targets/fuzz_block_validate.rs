#![no_main]
//! Fuzz target: Block deserialization and gossip validation.
//!
//! Feeds random bytes through validate_incoming_message() which performs
//! size checks, deserialization, and structural validation. Must not
//! panic on any input.

use libfuzzer_sys::fuzz_target;

use gratia_network::gossip::validate_incoming_message;
use gratia_core::types::Block;

fuzz_target!(|data: &[u8]| {
    // validate_incoming_message performs size check, deserialization,
    // and structural validation (transaction count, producer signature, etc.)
    // It must handle all malformed inputs without panicking.
    let _ = validate_incoming_message(data);

    // Also try direct Block deserialization and hash computation
    if let Ok(block) = bincode::deserialize::<Block>(data) {
        // Try computing the block header hash — this serializes the header
        // internally and must not panic even on corrupt data.
        let _ = block.header.hash();

        // Try computing message_id on a wrapped GossipMessage
        let msg = gratia_network::gossip::GossipMessage::NewBlock(Box::new(block));
        let _ = msg.message_id();
    }
});
