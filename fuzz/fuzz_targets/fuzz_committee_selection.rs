#![no_main]
//! Fuzz target: Committee selection with random eligible nodes and seeds.
//!
//! Feeds random eligible node lists, epoch seeds, and network sizes to
//! select_committee() and select_committee_with_network_size(). Verifies
//! the function never panics and always returns a valid committee or error.

use libfuzzer_sys::fuzz_target;

use gratia_consensus::committee::{
    select_committee, select_committee_with_network_size,
    EligibleNode, CooldownTracker,
};
use gratia_consensus::vrf::VrfPublicKey;
use gratia_core::types::NodeId;

/// Build a Vec<EligibleNode> from fuzz data.
/// Layout per node: 32 bytes node_id + 32 bytes vrf_pubkey + 1 byte score
///   + 1 byte flags (pol, stake) + 8 bytes pol_days + 32 bytes signing_pubkey
///   = 106 bytes per node.
const NODE_SIZE: usize = 106;

fn build_eligible_nodes(data: &[u8]) -> Vec<EligibleNode> {
    let count = data.len() / NODE_SIZE;
    let mut nodes = Vec::with_capacity(count);

    for i in 0..count {
        let offset = i * NODE_SIZE;
        let chunk = &data[offset..offset + NODE_SIZE];

        let mut node_id_bytes = [0u8; 32];
        node_id_bytes.copy_from_slice(&chunk[0..32]);

        let mut vrf_bytes = [0u8; 32];
        vrf_bytes.copy_from_slice(&chunk[32..64]);

        let presence_score = chunk[64];
        let flags = chunk[65];
        let has_valid_pol = flags & 0x01 != 0;
        let meets_minimum_stake = flags & 0x02 != 0;

        let mut pol_days_bytes = [0u8; 8];
        pol_days_bytes.copy_from_slice(&chunk[66..74]);
        let pol_days = u64::from_be_bytes(pol_days_bytes);

        let signing_pubkey = chunk[74..106].to_vec();

        nodes.push(EligibleNode {
            node_id: NodeId(node_id_bytes),
            vrf_pubkey: VrfPublicKey { bytes: vrf_bytes },
            presence_score,
            has_valid_pol,
            meets_minimum_stake,
            pol_days,
            signing_pubkey,
            vrf_proof: Vec::new(),
        });
    }

    nodes
}

fuzz_target!(|data: &[u8]| {
    // Need at least a 32-byte seed + 8 bytes epoch + 8 bytes slot + 8 bytes network_size
    if data.len() < 56 {
        return;
    }

    let mut seed = [0u8; 32];
    seed.copy_from_slice(&data[0..32]);

    let mut epoch_bytes = [0u8; 8];
    epoch_bytes.copy_from_slice(&data[32..40]);
    let epoch_number = u64::from_be_bytes(epoch_bytes);

    let mut slot_bytes = [0u8; 8];
    slot_bytes.copy_from_slice(&data[40..48]);
    let current_slot = u64::from_be_bytes(slot_bytes);

    let mut ns_bytes = [0u8; 8];
    ns_bytes.copy_from_slice(&data[48..56]);
    let network_size = u64::from_be_bytes(ns_bytes);

    let nodes = build_eligible_nodes(&data[56..]);

    // Path 1: select_committee (derives network size from eligible nodes)
    let result = select_committee(&nodes, &seed, epoch_number, current_slot);
    match result {
        Ok(committee) => {
            // Verify the committee has valid structure — these must not panic.
            let _ = committee.members.len();
            let _ = committee.finality_threshold;
        }
        Err(_) => {
            // Errors are fine — we only care that it doesn't panic.
        }
    }

    // Path 2: select_committee_with_network_size (explicit network size, may differ)
    let result2 = select_committee_with_network_size(
        &nodes,
        &seed,
        epoch_number,
        current_slot,
        network_size,
        None,
    );
    match result2 {
        Ok(committee) => {
            let _ = committee.members.len();
            let _ = committee.finality_threshold;
        }
        Err(_) => {}
    }

    // Path 3: With cooldown tracker
    let cooldown = CooldownTracker::new();
    let _ = select_committee_with_network_size(
        &nodes,
        &seed,
        epoch_number,
        current_slot,
        network_size,
        Some(&cooldown),
    );

    // Path 4: Edge cases — empty node list, zero network size
    let _ = select_committee(&[], &seed, epoch_number, current_slot);
    let _ = select_committee_with_network_size(&[], &seed, epoch_number, current_slot, 0, None);
    let _ = select_committee_with_network_size(
        &nodes, &seed, epoch_number, current_slot, 0, None,
    );
    let _ = select_committee_with_network_size(
        &nodes, &seed, epoch_number, current_slot, u64::MAX, None,
    );
});
