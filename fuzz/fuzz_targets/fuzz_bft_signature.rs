#![no_main]
//! Fuzz target: BFT co-signing via ConsensusEngine::add_block_signature().
//!
//! Feeds random ValidatorSignature data to the consensus engine's signature
//! accumulation path. Verifies it handles invalid signatures, non-committee
//! members, duplicate signers, and missing pending blocks gracefully.
//! Must not panic on any input.

use libfuzzer_sys::fuzz_target;

use gratia_consensus::ConsensusEngine;
use gratia_consensus::committee::EligibleNode;
use gratia_consensus::vrf::VrfPublicKey;
use gratia_core::types::{NodeId, ValidatorSignature};

fuzz_target!(|data: &[u8]| {
    // Need at least: 32 bytes signing_key + 32 bytes validator_node_id
    //   + variable-length signature bytes
    if data.len() < 64 {
        return;
    }

    let mut signing_key = [0u8; 32];
    signing_key.copy_from_slice(&data[0..32]);

    let node_id = NodeId(signing_key);

    // Create a consensus engine
    let mut engine = ConsensusEngine::new(node_id, &signing_key, 75);
    engine.trust_aware = false;

    // Path 1: add_block_signature with NO pending block.
    // Must return Err, never panic.
    let mut validator_bytes = [0u8; 32];
    validator_bytes.copy_from_slice(&data[32..64]);

    let sig_data = data[64..].to_vec();

    let sig = ValidatorSignature {
        validator: NodeId(validator_bytes),
        signature: sig_data.clone(),
    };
    let _ = engine.add_block_signature(sig);

    // Path 2: Set up a committee and pending block, then fuzz signatures.
    // Build some eligible nodes so we can initialize a committee.
    let mut nodes = Vec::new();
    for i in 0..25u8 {
        let mut id_bytes = [0u8; 32];
        id_bytes[0] = i;
        // Mix fuzz data into node construction if available
        if data.len() > 64 + (i as usize) {
            id_bytes[1] = data[64 + (i as usize)];
        }
        nodes.push(EligibleNode {
            node_id: NodeId(id_bytes),
            vrf_pubkey: VrfPublicKey { bytes: id_bytes },
            presence_score: 75,
            has_valid_pol: true,
            meets_minimum_stake: true,
            pol_days: 90,
            signing_pubkey: id_bytes.to_vec(),
            vrf_proof: Vec::new(),
        });
    }
    // Include our own node
    nodes.push(EligibleNode {
        node_id,
        vrf_pubkey: VrfPublicKey { bytes: signing_key },
        presence_score: 75,
        has_valid_pol: true,
        meets_minimum_stake: true,
        pol_days: 90,
        signing_pubkey: signing_key.to_vec(),
        vrf_proof: Vec::new(),
    });

    let seed = [0x42u8; 32];
    let _ = engine.initialize_committee(&nodes, &seed, 0, 0);

    // Force into Producing state and produce a block so we have a
    // pending block to accumulate signatures on.
    engine.force_producing_state();
    let _ = engine.produce_block(vec![], vec![], [0u8; 32]);

    // Now fuzz add_block_signature with various inputs.

    // Fuzz sig 1: completely random validator and signature
    let sig1 = ValidatorSignature {
        validator: NodeId(validator_bytes),
        signature: sig_data.clone(),
    };
    let _ = engine.add_block_signature(sig1);

    // Fuzz sig 2: empty signature
    let sig2 = ValidatorSignature {
        validator: NodeId(validator_bytes),
        signature: Vec::new(),
    };
    let _ = engine.add_block_signature(sig2);

    // Fuzz sig 3: signature from our own node_id (duplicate signer)
    let sig3 = ValidatorSignature {
        validator: node_id,
        signature: sig_data.clone(),
    };
    let _ = engine.add_block_signature(sig3);

    // Fuzz sig 4: extremely long signature
    if data.len() > 100 {
        let long_sig = data.to_vec();
        let sig4 = ValidatorSignature {
            validator: NodeId(validator_bytes),
            signature: long_sig,
        };
        let _ = engine.add_block_signature(sig4);
    }

    // Fuzz sig 5: iterate through fuzz data as multiple signatures
    let mut offset = 64;
    while offset + 32 < data.len() {
        let mut v_bytes = [0u8; 32];
        v_bytes.copy_from_slice(&data[offset..offset + 32]);
        offset += 32;

        let sig_len = if offset < data.len() {
            (data[offset] as usize).min(data.len() - offset - 1)
        } else {
            0
        };
        offset += 1;

        let sig_bytes = if sig_len > 0 && offset + sig_len <= data.len() {
            data[offset..offset + sig_len].to_vec()
        } else {
            Vec::new()
        };
        offset += sig_len;

        let sig = ValidatorSignature {
            validator: NodeId(v_bytes),
            signature: sig_bytes,
        };
        let _ = engine.add_block_signature(sig);
    }
});
