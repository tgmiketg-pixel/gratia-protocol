#![no_main]
//! Fuzz target: NodeAnnouncement validation in the gossip layer.
//!
//! Feeds random bytes through validate_incoming_message() specifically
//! targeting the NodeAnnouncement code path. Tests boundary conditions:
//! empty signatures, extreme timestamps, invalid pubkeys. Must not panic
//! on any input.

use libfuzzer_sys::fuzz_target;

use gratia_network::gossip::{
    validate_incoming_message, GossipMessage, NodeAnnouncement,
    node_announcement_signing_payload,
};
use gratia_core::types::NodeId;

fuzz_target!(|data: &[u8]| {
    // Path 1: Feed raw bytes through validate_incoming_message.
    // This exercises the full deserialization + structural validation
    // pipeline, including the NodeAnnouncement branch.
    let _ = validate_incoming_message(data);

    // Path 2: Direct NodeAnnouncement deserialization.
    // If deserialization succeeds, exercise the signing payload builder
    // and further validation logic.
    if let Ok(ann) = bincode::deserialize::<NodeAnnouncement>(data) {
        // Build the signing payload — must not panic on any field values.
        let _ = node_announcement_signing_payload(&ann);

        // Wrap as a GossipMessage and try validate_incoming_message on
        // the re-serialized form. This tests round-trip robustness.
        let msg = GossipMessage::NodeAnnouncement(Box::new(ann));
        if let Ok(bytes) = msg.to_bytes() {
            let _ = validate_incoming_message(&bytes);
        }
        let _ = msg.message_id();
    }

    // Path 3: Construct NodeAnnouncement from fuzz data with boundary values.
    // Exercises edge cases that random deserialization is unlikely to hit.
    if data.len() >= 128 {
        let mut node_id_bytes = [0u8; 32];
        node_id_bytes.copy_from_slice(&data[0..32]);

        let mut vrf_bytes = [0u8; 32];
        vrf_bytes.copy_from_slice(&data[32..64]);

        let mut pubkey_bytes = [0u8; 32];
        pubkey_bytes.copy_from_slice(&data[64..96]);

        // Use a byte from fuzz data for presence_score (may be out of 40-100 range)
        let presence_score = data[96];

        // Extract pol_days from fuzz data
        let mut pol_days_bytes = [0u8; 8];
        pol_days_bytes.copy_from_slice(&data[97..105]);
        let pol_days = u64::from_be_bytes(pol_days_bytes);

        // Extract timestamp offset from fuzz data (signed, for past/future)
        let mut ts_bytes = [0u8; 8];
        ts_bytes.copy_from_slice(&data[105..113]);
        let ts_offset = i64::from_be_bytes(ts_bytes);

        let timestamp = chrono::Utc::now()
            + chrono::Duration::seconds(ts_offset.clamp(-1_000_000, 1_000_000));

        // Signature: use remaining bytes (may be empty, wrong length, etc.)
        let sig = data[113..].to_vec();

        let ann = NodeAnnouncement {
            node_id: NodeId(node_id_bytes),
            vrf_pubkey_bytes: vrf_bytes,
            presence_score,
            pol_days,
            timestamp,
            ed25519_pubkey: pubkey_bytes,
            signature: sig,
            height: 0,
        };

        // Build signing payload — must not panic
        let _ = node_announcement_signing_payload(&ann);

        // Wrap and validate — must not panic
        let msg = GossipMessage::NodeAnnouncement(Box::new(ann));
        if let Ok(bytes) = msg.to_bytes() {
            let _ = validate_incoming_message(&bytes);
        }
    }
});
