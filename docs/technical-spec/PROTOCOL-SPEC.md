# Gratia Protocol Specification

**Version:** 0.1.0-draft
**Date:** 2026-04-02
**Status:** Pre-audit draft
**Audience:** Security auditors, protocol researchers

---

## Table of Contents

1. [Overview](#1-overview)
2. [Consensus Model](#2-consensus-model)
3. [Block Production](#3-block-production)
4. [BFT Finality](#4-bft-finality)
5. [Peer Discovery](#5-peer-discovery)
6. [NodeAnnouncement Protocol](#6-nodeannouncement-protocol)
7. [Committee Management](#7-committee-management)
8. [Security Properties](#8-security-properties)
9. [Mining Rewards](#9-mining-rewards)
10. [Protocol Gates](#10-protocol-gates)
11. [Cryptographic Primitives](#11-cryptographic-primitives)
12. [Wire Formats and Constants](#12-wire-formats-and-constants)

---

## 1. Overview

Gratia is a mobile-native layer-1 blockchain. Consensus runs exclusively on smartphones. The protocol combines three security pillars:

1. **Proof of Life (PoL)** -- daily behavioral attestation proving a real human operates the device.
2. **Staking** -- minimum stake required to mine; capped per node with overflow to a network security pool.
3. **Energy Expenditure** -- mining requires real ARM computation while plugged in, blocking emulators/VMs.

All three pillars must be satisfied simultaneously for a node to participate in consensus.

**Key parameters at a glance:**

| Parameter | Value | Source |
|-----------|-------|--------|
| Target block time | 4 seconds | `consensus/lib.rs:TARGET_BLOCK_TIME_SECS` |
| Max block size | 256 KB (262,144 bytes) | `consensus/validation.rs:MAX_BLOCK_SIZE` |
| Max committee size | 21 validators | `consensus/committee.rs:MAX_COMMITTEE_SIZE` |
| Full finality threshold | 14/21 (67%) | `consensus/committee.rs:FINALITY_THRESHOLD` |
| Slots per epoch | 900 (~1 hour) | `consensus/committee.rs:SLOTS_PER_EPOCH` |
| Max transactions per block | 512 | `consensus/validation.rs:MAX_TRANSACTIONS_PER_BLOCK` |
| Minimum transaction fee | 1,000 Lux (0.001 GRAT) | `consensus/validation.rs:MIN_TRANSACTION_FEE` |
| Presence Score range | 40--100 | `consensus/committee.rs:EligibleNode::is_eligible()` |
| Max gossip message size | 300 KB (307,200 bytes) | `network/gossip.rs:MAX_MESSAGE_SIZE` |

---

## 2. Consensus Model

### 2.1 High-Level Design

Gratia uses a **Streamlet BFT** consensus protocol with **VRF-based block producer selection**. The protocol operates in epochs of 900 slots (approximately 1 hour at 4-second slot times). Each epoch has a fixed validator committee selected via VRF.

Reference: "Streamlet: Textbook Streamlined Blockchains" by Chan & Shi, 2020.

### 2.2 Streamlet BFT Protocol

The Streamlet protocol operates in three phases per block:

1. **Propose:** Each slot, the designated leader (selected by VRF round-robin within the committee) proposes a block extending the longest notarized chain.
2. **Vote:** Committee members vote for the proposal if it extends a notarized chain they know about. Each member votes for **at most one block per epoch** (enforced via `voted_epochs` map in `StreamletState`).
3. **Notarize:** A block is notarized when it receives **2/3+ committee votes**. The notarization threshold is computed as `(committee_size * 2 + 2) / 3` (ceiling division).
4. **Finalize:** When **3 consecutive notarized blocks** exist at heights h, h+1, h+2, the chain up to height **h+1** (the middle block) is finalized.

Implementation: `crates/gratia-consensus/src/streamlet.rs`

### 2.3 Equivocation Detection

Two forms of equivocation are detected:

1. **Double-voting within an epoch:** `StreamletState::remote_votes` tracks `(epoch, validator) -> block_hash`. A second vote from the same validator for a different block in the same epoch is rejected and logged as Byzantine behavior.

2. **Double-proposal at the same height:** `ConsensusEngine::seen_proposals` tracks `(height, producer) -> block_hash`. A second block from the same producer at the same height with a different hash is rejected with an error. Entries are pruned when the height is more than 100 blocks behind the current tip.

### 2.4 Committee Tiers (Graduated Scaling)

Committee size scales with network size across 7 tiers. All committee sizes are odd to prevent voting ties. Finality threshold stays near 67% at every tier.

| Tier | Min Network Size | Committee Size | Finality Threshold | Min Selection Pool | Cooldown Rounds |
|------|-----------------|----------------|--------------------|--------------------|-----------------|
| 0 | 0 | 3 | 2 | 10 | 5 |
| 1 | 100 | 5 | 4 | 50 | 3 |
| 2 | 500 | 7 | 5 | 100 | 2 |
| 3 | 2,500 | 11 | 8 | 500 | 1 |
| 4 | 10,000 | 15 | 10 | 2,000 | 1 |
| 5 | 50,000 | 19 | 13 | 10,000 | 1 |
| 6 | 100,000 | 21 | 14 | 20,000 | 1 |

Source: `consensus/committee.rs:SCALING_TIERS`

Tier selection: the highest tier where `network_size >= tier.min_network_size` is used. Network size is the count of eligible nodes (those passing `is_eligible()`: valid PoL + minimum stake + presence score >= 40).

---

## 3. Block Production

### 3.1 Slot Timing

- **Target block time:** 4 seconds (`TARGET_BLOCK_TIME_SECS`).
- **Slot advancement:** `ConsensusEngine::advance_slot()` is called by an external timer every ~4 seconds.
- **Producer determination:** Uses `next_height = current_height + 1` as the slot index, NOT the local slot counter. This ensures all nodes agree on who produces, since all nodes share `current_height` (from the finalized chain) while local slot counters drift due to independent timers.

### 3.2 VRF-Based Producer Selection

Within an epoch, block production rotates among committee members using a round-robin derived from VRF ordering:

```
producer_index = (slot - epoch.start_slot) % committee.members.len()
```

The committee is already VRF-selected and sorted by `selection_value` (lowest first = highest priority), so round-robin within the epoch is fair. A per-slot VRF selection is planned for mainnet.

Source: `ValidatorCommittee::block_producer_for_slot()` in `committee.rs`

### 3.3 VRF Selection Value Computation

Each eligible node's selection priority is computed as:

```
vrf_input = COMMITTEE_SELECTION_DOMAIN || epoch_seed || epoch_number (big-endian)
```

For nodes with a real VRF proof:
- The proof is verified against the node's VRF public key and the selection input.
- The verified VRF output is used.

For nodes without a VRF proof (PoC fallback):
```
output = SHA-256(vrf_input || node_id)
```

Selection value:
```
raw = u64::from_le_bytes(vrf_output[0..8])
selection_value = raw / clamp(presence_score, 40, 100)
```

Lower selection value = higher priority. Higher presence score produces a smaller (better) selection value. Integer division ensures determinism across ARM and x86.

Source: `vrf::vrf_output_to_selection()` in `vrf.rs`

### 3.4 RANDAO Epoch Seed Derivation

The epoch seed is derived from the last finalized block:

```
epoch_seed = SHA-256("gratia-epoch-seed-v1:" || last_finalized_hash || current_height)
```

For epoch rotation specifically:
```
new_seed = SHA-256("gratia-epoch-seed-v1:" || last_block_hash || new_epoch_number)
```

This makes committee selection unpredictable until the seed block is finalized. An attacker would need to control block production to manipulate the seed.

Source: `ConsensusEngine::compute_epoch_seed()`, `committee::rotate_committee()`

### 3.5 Block Structure

A produced block contains:

- **BlockHeader:**
  - `height: u64`
  - `timestamp: DateTime<Utc>`
  - `parent_hash: BlockHash` (SHA-256, 32 bytes)
  - `transactions_root: [u8; 32]` (Merkle root of transaction hashes)
  - `state_root: [u8; 32]`
  - `attestations_root: [u8; 32]` (Merkle root of serialized attestation hashes)
  - `producer: NodeId` (32 bytes)
  - `vrf_proof: Vec<u8>` (96 bytes, see Section 11)
  - `active_miners: u64`
  - `geographic_diversity: u16`
  - `producer_pubkey: Vec<u8>` (32 bytes Ed25519)
- **Transactions:** `Vec<Transaction>`
- **Attestations:** `Vec<ProofOfLifeAttestation>`
- **Validator Signatures:** `Vec<ValidatorSignature>`

### 3.6 VRF Proof in Block Header

Each block includes a VRF proof from the producer for the current slot:

```
vrf_input = previous_block_hash || slot_number (big-endian)
```

The proof is 96 bytes: 32 (Gamma compressed Ristretto) + 32 (challenge scalar) + 32 (response scalar).

Source: `vrf::build_vrf_input()`, `vrf::VRF_PROOF_SIZE`

### 3.7 Block Size Enforcement

After assembly, the block is serialized with bincode. If the serialized size exceeds `MAX_BLOCK_SIZE` (262,144 bytes), production fails. The producer must reduce the transaction count.

### 3.8 Fresh Network Stagger

On a fresh network with only 1 node, the node enters "solo mode" (see Section 7.1). Block production begins immediately after committee initialization without waiting for external peers.

---

## 4. BFT Finality

### 4.1 Two-Layer Finality

Gratia implements finality at two levels:

1. **Signature-threshold finality (operational):** A `PendingBlock` tracks collected `ValidatorSignature` entries. The block is considered finalized when `signatures.len() >= finality_threshold` (from the graduated scaling table). This is the primary finality mechanism used in the current implementation.

2. **Streamlet finality (formal BFT):** Three consecutive notarized blocks at heights h, h+1, h+2 finalize the chain up to h+1. This provides the theoretical BFT safety guarantee.

### 4.2 Block Proposal Flow

1. **Producer creates `PendingBlock`:** Block is assembled with transactions, attestations, Merkle roots, and VRF proof. `signatures` starts empty. `finality_threshold` is set from the current committee tier.

2. **Producer self-signs:** The producer signs the block header hash with their Ed25519 key, producing a `ValidatorSignature`. This is added to the pending block.

3. **Broadcast:** The block (with producer signature) is broadcast via gossipsub on `TOPIC_BLOCKS`. The producer's signature is broadcast separately on `TOPIC_VALIDATOR_SIGS`.

4. **Committee co-signing:** Other committee members validate the block and, if valid, sign the block header hash and broadcast their `ValidatorSignatureMessage` on `TOPIC_VALIDATOR_SIGS`.

5. **Signature collection:** The producer collects incoming signatures via `ConsensusEngine::add_block_signature()`. Each signature is:
   - Checked for committee membership.
   - Cryptographically verified (Ed25519 over block header hash) using the signing pubkey stored in the committee.
   - Checked for duplicates (same validator cannot sign twice).

6. **Finalization:** When `signatures.len() >= finality_threshold`, the block is finalized via `finalize_pending_block()`. The chain tip advances.

### 4.3 Signature Verification Security

`add_block_signature()` enforces the following:

- The signer's `NodeId` must be in the current committee.
- The Ed25519 signature is verified against the committee member's stored `signing_pubkey` using `verify_block_signature()`.
- If the signing pubkey is empty (synthetic node) and the committee has >1 real member, the signature is **rejected**. Empty-pubkey acceptance is only permitted in solo/bootstrap mode (<=1 real member).

### 4.4 Force Finalization (Bootstrap Only)

`force_finalize_pending_block()` allows finalizing with fewer signatures than the threshold. It is gated:

- `PendingBlock::force_finalize()` requires at least 1 signature AND `finality_threshold <= 1`. If `finality_threshold > 1` and signatures are insufficient, it returns an error.
- `ConsensusEngine::force_finalize_pending_block()` additionally counts real committee members (non-empty signing keys). If >1 real member exists, force finalization is blocked.

This ensures force finalization is only possible when the network has a single real node.

### 4.5 Incoming Block Processing

`process_incoming_block()` handles blocks received from peers:

1. **Height check:**
   - At or below current height: **skipped**.
   - Exactly 1 ahead (expected): normal processing.
   - Exactly 2 ahead: **fast-forward** (peer finalized a block we missed). Accepted without parent hash check.
   - More than 2 ahead: **ForkDetected** (triggers sync/reorg).

2. **Parent hash check:** For non-fast-forward blocks, `block.parent_hash` must equal `last_finalized_hash`. Mismatch returns `ForkDetected`.

3. **Equivocation check:** Detects if the same producer proposed a different block at the same height.

4. **Producer validation:** The block's producer must match the expected producer for the slot (via `block_producer_for_slot()`). A tolerance of +1 height is allowed for clock skew. Skipped for fast-forwards.

5. **Minimum signature check:** The block must contain at least 1 signature (the producer's) with valid 64-byte Ed25519 format.

### 4.6 Gossipsub Topics

| Topic | Content | Purpose |
|-------|---------|---------|
| `gratia/blocks/1` | `NewBlock` | Block proposal propagation |
| `gratia/transactions/1` | `NewTransaction` | Transaction propagation |
| `gratia/attestations/1` | `NewAttestation` | PoL attestation propagation |
| `gratia/nodes/1` | `NodeAnnouncement` | Committee eligibility announcements |
| `gratia/sync/1` | Sync messages | Point-to-point sync (testnet; libp2p request-response planned for Phase 3) |
| `gratia/lux/posts/1` | `NewLuxPost` | Social layer posts |
| `gratia/validator-sigs/1` | `ValidatorSignatureMsg` | BFT co-signatures |

### 4.7 Message Deduplication

Each `GossipMessage` variant produces a deterministic `message_id()`:

- **Block:** `"block:" || header_hash` (fallback: height bytes)
- **Transaction:** `"tx:" || tx_hash`
- **Attestation:** `"att:" || nullifier`
- **NodeAnnouncement:** `"node:" || node_id || timestamp` (timestamp included so re-announcements are not filtered)
- **LuxPost:** `"lux:" || post_hash`
- **ValidatorSignature:** `"vsig:" || block_hash || validator_node_id`

A `DeduplicationCache` (HashSet-based) tracks recently seen IDs.

---

## 5. Peer Discovery

### 5.1 Discovery Layers

Gratia uses three peer discovery mechanisms:

1. **mDNS (local network):** For discovering peers on the same Wi-Fi or LAN. Used in testnet and local development.

2. **Kademlia DHT (internet):** `PeerDiscovery` maintains a local cache of `PeerRecord` entries, each containing:
   - `node_id: NodeId`
   - `peer_id_bytes: Vec<u8>` (libp2p peer ID)
   - `addresses: Vec<String>` (multiaddresses)
   - `presence_score: u8`
   - `shard_id: u16`
   - `is_mining: bool`
   - `last_seen: DateTime<Utc>`

   Stale threshold: **600 seconds (10 minutes)**. Records older than this are pruned.
   Max cached peers: configurable (default sizing targets ~100 KB memory at ~200 bytes per record).

3. **Gossipsub NodeAnnouncements:** Nodes broadcast `NodeAnnouncement` messages on `gratia/nodes/1` when joining and periodically (~32 seconds). These contain committee eligibility data and are cryptographically signed (see Section 6).

### 5.2 Bootstrap Relay

Bootstrap peer addresses are configured at startup. The `PeerDiscovery` instance stores these and uses them as initial entry points to the Kademlia DHT.

---

## 6. NodeAnnouncement Protocol

### 6.1 Message Fields

```rust
struct NodeAnnouncement {
    node_id: NodeId,               // [u8; 32] -- SHA-256(domain_prefix || ed25519_pubkey)
    vrf_pubkey_bytes: [u8; 32],    // Compressed Ristretto point
    presence_score: u8,            // 40--100
    pol_days: u64,                 // Consecutive days of valid PoL
    timestamp: DateTime<Utc>,      // When this announcement was created
    ed25519_pubkey: [u8; 32],      // Raw Ed25519 public key
    signature: Vec<u8>,            // Ed25519 signature (64 bytes)
}
```

### 6.2 Signing

The signature covers a canonical byte payload constructed by `node_announcement_signing_payload()`:

```
payload = node_id (32) || vrf_pubkey_bytes (32) || presence_score (1) || pol_days (8, big-endian) || timestamp (8, big-endian i64 unix seconds)
```

Total payload: 81 bytes.

The signature is produced with the node's Ed25519 signing key. Both the signing side (FFI/application) and the verification side (gossip validation) use the same `node_announcement_signing_payload()` function as the single source of truth.

### 6.3 Validation (Gossip Layer)

When a `NodeAnnouncement` is received via gossipsub, `validate_incoming_message()` performs the following checks in order:

1. **Presence score range:** `40 <= presence_score <= 100`. Reject otherwise.

2. **Signature presence:** `signature` must be non-empty. Unsigned announcements are rejected.

3. **Pubkey-to-NodeId derivation:**
   ```
   derived_id = SHA-256("gratia-node-id-v1:" || ed25519_pubkey)
   ```
   Must equal the claimed `node_id`. This prevents impersonation -- a node cannot claim another node's ID without possessing their Ed25519 private key.

   Source: `gossip::node_id_from_pubkey()`

4. **Ed25519 signature verification:** The signature is verified against `ed25519_pubkey` over the canonical payload. Uses `ed25519_dalek::VerifyingKey::verify_strict()` (rejects malleable signatures).

5. **Timestamp freshness:** The announcement's age (`now - timestamp`) must be:
   - At most **300 seconds** (5 minutes) in the past. Rejects stale/replayed announcements.
   - At most **60 seconds** in the future. Rejects clock-manipulated announcements.

### 6.4 Deduplication

NodeAnnouncement dedup IDs include the timestamp (`"node:" || node_id || timestamp`). This ensures periodic re-announcements (e.g., every 32 seconds) pass through the dedup cache, which is necessary for committee rebuilds after network reconnection.

---

## 7. Committee Management

### 7.1 Solo Mode (Bootstrap)

When the network has only 1 real node:

1. The committee is initialized with the single real node plus **2 synthetic placeholder members** (to fill the tier-0 committee size of 3).
2. Synthetic members have **empty `signing_pubkey`** -- they cannot produce valid signatures.
3. The `finality_threshold` is overridden to **1** (instead of the tier's threshold of 2), because only 1 real signer exists.
4. Blocks are finalized via `force_finalize()`, which requires at least 1 signature and `finality_threshold == 1`.

Source: `select_committee_with_network_size()` -- the `real_signers <= 1` check.

### 7.2 Multi-Node Transition

When a second real node broadcasts a `NodeAnnouncement`:

1. The FFI layer converts the announcement to an `EligibleNode`.
2. The committee is rebuilt via `select_committee()` with the updated eligible node set.
3. If 2+ real signers exist, `finality_threshold` uses the tier's actual value (e.g., 2 for tier-0 with 3 members).
4. `force_finalize()` is now blocked -- blocks must reach the real BFT threshold.
5. Signatures from validators with empty pubkeys are rejected in multi-node mode.

### 7.3 Epoch Seed Caching

The epoch seed is deterministic from chain state:
```
seed = SHA-256("gratia-epoch-seed-v1:" || last_finalized_hash || current_height)
```

All nodes with the same finalized chain tip compute the same seed, producing the same committee. No communication is required for committee agreement -- it is derived from shared state.

### 7.4 Committee Rotation

Rotation occurs when `current_slot >= committee.epoch.end_slot` (checked in `advance_slot()`):

- New seed: `SHA-256("gratia-epoch-seed-v1:" || last_block_hash || new_epoch_number)`
- New epoch number: `previous_epoch + 1`
- New start slot: `previous_epoch.end_slot`
- New end slot: `new_start_slot + SLOTS_PER_EPOCH` (900)

Source: `committee::rotate_committee()`, `committee::should_rotate()`

### 7.5 Cooldown Enforcement

The `CooldownTracker` maintains a ring buffer of recent committee member sets (up to 20 entries). Before committee selection, nodes that served in the most recent `cooldown_rounds` committees are filtered out.

If cooldown filtering removes too many candidates (below `tier.committee_size`), the cooldown is bypassed and the unfiltered eligible set is used.

### 7.6 Rate-Limited Rebuilds

Committee rebuilds are triggered by:
- New `NodeAnnouncement` from a previously unknown peer.
- Epoch rotation (slot >= end_slot).
- Peer departure detection (BFT expiration in FFI layer).

The FFI layer controls rebuild frequency. Re-announcements every ~32 seconds can trigger rebuilds, but the committee only changes if the eligible node set changes.

### 7.7 Eligibility Requirements

**Basic eligibility** (`is_eligible()`):
- `has_valid_pol == true`
- `meets_minimum_stake == true`
- `presence_score >= 40`

**Committee eligibility** (`is_committee_eligible()`):
- All basic eligibility requirements, PLUS
- `pol_days >= 30` (Established trust tier)

If fewer than `tier.committee_size` nodes meet committee eligibility, the selection falls back to basic eligibility. This is expected during the first month of the network.

---

## 8. Security Properties

### 8.1 Guarantees

**BFT Safety (with <1/3 faulty validators):**
Streamlet guarantees that no two conflicting blocks can both be finalized, provided fewer than 1/3 of the committee is Byzantine. Two conflicting notarizations at the same height would require >1/3 of validators to double-vote, which is detected and rejected by equivocation detection.

**Deterministic Committee Agreement:**
All honest nodes with the same finalized chain tip derive the same epoch seed, compute the same VRF outputs, and select the same committee. No committee election protocol is needed.

**Equivocation Evidence:**
Double-voting and double-proposing are detected and logged. Evidence is retained for 100 heights (proposals) or until epoch completion (votes).

**Signature Authenticity:**
- Block signatures: Ed25519 verified against committee member's stored pubkey.
- NodeAnnouncements: Ed25519 verified at the gossip layer before propagation.
- ValidatorSignatureMessages: Ed25519 verified at the gossip layer (pubkey -> NodeId derivation + signature check).
- All verification uses `verify_strict()` which rejects malleable signatures.

**Replay Protection:**
- NodeAnnouncements: 300-second staleness window + 60-second future rejection.
- Blocks: height must be exactly `current_height + 1` (or +2 for fast-forward).
- Transactions: nonce-based (not covered in this spec; see transaction validation).

**Anti-Impersonation:**
NodeId is derived as `SHA-256("gratia-node-id-v1:" || ed25519_pubkey)`. An attacker cannot claim a victim's NodeId without possessing the victim's Ed25519 private key, because:
1. The pubkey is included in the announcement.
2. The derivation is checked: `SHA-256(domain || pubkey) == claimed_node_id`.
3. The signature over the announcement is verified with the pubkey.

### 8.2 Non-Guarantees

**No Offline Payment Resistance:**
All transactions require BFT consensus confirmation. NFC, Bluetooth, and Wi-Fi Direct are transport layers only. A transaction is not final until included in a block with sufficient committee signatures.

**No Sybil Resistance Without PoL:**
Committee selection alone does not prevent Sybil attacks. Sybil resistance depends on the Proof of Life system (not covered in this spec) ensuring one-human-one-node. If PoL is compromised, an attacker could register many nodes and dominate the committee.

**No Safety During Network Partition:**
If the network partitions such that neither partition has 2/3 of the committee, neither partition can notarize blocks. The chain stalls until connectivity is restored. This is inherent to BFT -- safety is preserved at the cost of liveness.

**No Finality in Solo Mode:**
Solo-mode blocks are force-finalized with a single signature. They provide no BFT guarantees. If a second node joins with a conflicting chain, a reorg may occur. Solo mode is a bootstrap mechanism, not a security mechanism.

**No Clock Synchronization Guarantee:**
Slot timing relies on each phone's local clock. Clocks may drift. The protocol mitigates this by using `current_height + 1` for producer selection (shared state) rather than local slot counters, and by allowing +1 height tolerance for producer validation. The 300-second freshness window on NodeAnnouncements tolerates moderate clock skew.

### 8.3 Attack Surface Summary

| Attack | Mitigation | Residual Risk |
|--------|-----------|---------------|
| Committee takeover via fake NodeAnnouncements | Ed25519 signature + pubkey-to-NodeId derivation | None if Ed25519 is secure |
| Forged block signatures | Ed25519 verification in `add_block_signature()` | None if Ed25519 is secure |
| Forged validator co-signatures | Gossip-layer Ed25519 verification of `ValidatorSignatureMsg` | None if Ed25519 is secure |
| Stale announcement replay | 300s freshness window | Attacker with <5 min delay can replay; limited impact (same data) |
| Double-block proposal | `seen_proposals` equivocation detection | Evidence retained for 100 heights only |
| Double-voting | `remote_votes` equivocation detection in StreamletState | Evidence retained until epoch ends |
| Force-finalize bypass in multi-node | Real-member count check in `force_finalize_pending_block()` | None (hard error if >1 real member) |
| Empty-pubkey signature bypass | Rejected when >1 real committee member | Accepted in solo mode (by design) |
| Oversized message DoS | 300 KB max message size check before deserialization | Within budget for mobile devices |
| Transaction spam | Structural validation at gossip layer (signature length, pubkey length) | Full validation deferred to consensus layer |
| Committee manipulation via seed grinding | Seed derived from finalized block hash (unpredictable) | Attacker controlling block production can grind |

---

## 9. Mining Rewards

### 9.1 Emission Schedule

| Parameter | Value | Source |
|-----------|-------|--------|
| Total mining supply | 8,500,000,000 GRAT | `core/emission.rs:TOTAL_MINING_SUPPLY_GRAT` |
| Year 1 emission | 2,125,000,000 GRAT | `core/emission.rs:YEAR_1_EMISSION_GRAT` |
| Annual retention | 75% (25% reduction) | `core/emission.rs:ANNUAL_RETENTION_BPS = 7500` |
| Emission block time | 4 seconds | `core/emission.rs:BLOCK_TIME_SECS` |
| Blocks per day | 21,600 | `core/emission.rs:BLOCKS_PER_DAY` |
| Blocks per year | 7,884,000 | `core/emission.rs:BLOCKS_PER_YEAR` |

**Note:** The emission block time now matches the consensus engine's target block time (4 seconds, `consensus/lib.rs:TARGET_BLOCK_TIME_SECS`). The daily and yearly emission totals are unchanged; the per-block reward is 1/3 of the previous value since blocks are produced 3x more frequently.

Year N emission: `Year_1 * 0.75^(N-1)`

Computed iteratively:
```
emission = YEAR_1_EMISSION_GRAT
for _ in 1..year:
    emission = emission * 7500 / 10000
```

### 9.2 Per-Block Reward

```
daily_budget_grat = annual_emission_grat(year) / 365
block_reward_lux = (daily_budget_grat * 1_000_000) / BLOCKS_PER_DAY
```

Year 1, height 0: `block_reward_lux = 808,599,583 Lux` (~808.6 GRAT)

The Lux-first computation avoids integer division truncation that would zero out rewards at high years (when `daily_grat < BLOCKS_PER_DAY`).

### 9.3 Reward Distribution

- **Flat rate:** All active miners receive the same base reward per block.
- **Geographic equity:** Underserved regions receive up to 1.5x bonus (`geographic_bonus_max_bps = 15000`).
- **Per-miner reward:** `block_reward_lux / active_miners` (0 miners returns full reward as bootstrap edge case).
- **Network Security Pool allocation:** A portion of each block's reward goes to the overflow staking pool, distributed proportionally to all active miners.

### 9.4 Reward Crediting

Rewards are credited **only on BFT-finalized blocks**. Solo-mode blocks (force-finalized) do NOT distribute mining rewards to the network. This prevents a single node from generating unbounded rewards without peer validation.

---

## 10. Protocol Gates

### 10.1 Mining Activation

Mining mode activates only when ALL of the following are true:

1. **Battery >= 80%:** The phone must be at or above 80% charge.
2. **Plugged in:** The phone must be connected to any power source.
3. **Valid Proof of Life:** A valid PoL attestation for the current rolling 24-hour window must exist (all 8 parameters met).
4. **Minimum stake (future):** A governance-adjustable minimum GRAT stake must be in place. Not yet enforced in PoC.

The phone charges to 80% FIRST, then mining activates. User battery needs always take priority.

### 10.2 Proof of Life Parameters

All 8 must be met within a rolling 24-hour window:

1. Minimum 10 unlock events spread across at least a 6-hour window.
2. Organic screen interaction events at multiple points throughout the day (timing/frequency only, never content).
3. At least 1 orientation change.
4. Accelerometer data showing human-consistent motion during at least part of the day.
5. At least 1 GPS fix confirming plausible geographic location.
6. Connection to at least 1 Wi-Fi network OR detection of Bluetooth peers.
7. Varying Bluetooth peer environments (different device sets at different times).
8. At least 1 charge cycle event (plug-in or unplug).

### 10.3 PoL Grace Period

- **1-day grace period:** Missing PoL for 1 day does not pause mining.
- **2 consecutive missed days:** Mining is paused.
- **Resumption:** Immediate on next valid PoL day.

### 10.4 Committee Eligibility Gate

Committee membership requires:
- All mining prerequisites (Section 10.1).
- `presence_score >= 40` (binary pass/fail for consensus threshold).
- **30+ days consecutive PoL** for committee eligibility (Established trust tier). Falls back to basic eligibility if insufficient 30-day nodes exist.
- **90+ days PoL** for governance proposal submission (Trusted tier).

### 10.5 Progressive Trust

| Days | Tier | Capabilities |
|------|------|-------------|
| 0 | Unverified | Mining (max scrutiny) |
| 7 | Provisional | Mining (reduced scrutiny) |
| 30 | Established | Mining + committee eligibility |
| 90+ | Trusted | Mining + committee + governance proposals |

Mining rewards are flat at every trust level. Only trust-gated capabilities change.

---

## 11. Cryptographic Primitives

### 11.1 Identity Keys

- **Algorithm:** Ed25519
- **Key generation:** Secure enclave (Android Keystore/StrongBox, iOS Secure Enclave)
- **NodeId derivation:** `SHA-256("gratia-node-id-v1:" || ed25519_pubkey_bytes)` -- 32 bytes

### 11.2 VRF Keys

- **Algorithm:** Schnorr-like proof on Ristretto points (curve25519-dalek)
- **Key derivation:** From Ed25519 signing key: `Scalar::from_bytes_mod_order_wide(SHA-512("gratia-vrf-keygen-v1:" || signing_key_bytes))`
- **Public key:** `scalar * RISTRETTO_BASEPOINT_POINT`, stored as 32-byte compressed Ristretto
- **Proof size:** 96 bytes (32 Gamma + 32 challenge + 32 response)
- **Domain separators:**
  - Hash-to-point: `"gratia-vrf-h2c-v1"`
  - Challenge: `"gratia-vrf-challenge-v1"`
  - Output: `"gratia-vrf-output-v1"`
  - Committee selection: `"gratia-committee-select-v1"`

**Note:** This is a PoC VRF implementation. A full RFC 9381 (ECVRF) implementation is planned for mainnet.

### 11.3 Block Signing

- **Algorithm:** Ed25519
- **Input:** `SHA-256(bincode(BlockHeader))` -- the header hash
- **Output:** 64-byte Ed25519 signature
- **Verification:** `ed25519_dalek::VerifyingKey::verify_strict()` (or `gratia_core::crypto::verify_signature()`)

### 11.4 Hashing

- **Block header hash:** SHA-256 over bincode-serialized `BlockHeader`
- **Merkle root:** SHA-256-based Merkle tree over leaf hashes
- **Epoch seed:** SHA-256 with domain separation
- **NodeId:** SHA-256 with domain separation
- **General:** `ring` crate for SHA-256/SHA-512 with ARM hardware acceleration (ARMv8 Cryptography Extensions)

### 11.5 Zero-Knowledge Proofs

- **PoL attestations:** Bulletproofs (no trusted setup)
- **Shielded transactions (optional):** Bulletproofs + Pedersen commitments
- **Complex ZK (smart contracts):** Groth16 via `bellman`

### 11.6 Serialization

- **Wire format:** bincode (compact binary, used for gossip messages, blocks, transactions)
- **Storage format:** bincode (RocksDB values)
- **Signature payloads:** Custom canonical byte encoding (e.g., `node_announcement_signing_payload()`) to avoid serialization format ambiguity

---

## 12. Wire Formats and Constants

### 12.1 Gossip Message Envelope

All gossip messages are `bincode`-serialized `GossipMessage` enum variants. Maximum size: 300 KB (`MAX_MESSAGE_SIZE`).

### 12.2 ValidatorSignatureMessage

```rust
struct ValidatorSignatureMessage {
    block_hash: [u8; 32],          // SHA-256 of block header
    height: u64,                   // Block height
    signature: ValidatorSignature, // { validator: NodeId, signature: Vec<u8> (64 bytes) }
    validator_pubkey: [u8; 32],    // Ed25519 pubkey for verification
}
```

Gossip-layer validation:
1. `signature.signature` must be exactly 64 bytes.
2. `block_hash` must not be all zeros.
3. `validator_pubkey` must not be all zeros.
4. `SHA-256("gratia-node-id-v1:" || validator_pubkey) == signature.validator` (NodeId derivation check).
5. Ed25519 signature over `block_hash` verified with `validator_pubkey`.

### 12.3 Transaction Validation (Gossip Layer)

Structural checks before consensus-layer processing:
1. `signature` must be non-empty and exactly 64 bytes.
2. `sender_pubkey` must be exactly 32 bytes.

### 12.4 Attestation Validation (Gossip Layer)

1. `zk_proof` must be non-empty.
2. `presence_score` must be in range 40--100.

### 12.5 Block Validation (Gossip Layer)

1. Transaction count must be <= 10,000 (sanity bound).
2. For blocks at height > 0: must contain at least 1 signature from the claimed `producer`, with valid 64-byte signature length.
3. If `producer_pubkey` is 32 bytes: optional Ed25519 pre-check of first signature (logged, not rejected -- definitive check is in consensus layer).

### 12.6 Consensus-Layer Block Validation

1. Height must be `current_height + 1` (or +2 for fast-forward).
2. Parent hash must match `last_finalized_hash` (skipped for fast-forward).
3. Producer must match expected committee member for the slot (+/-1 height tolerance).
4. At least 1 valid signature required.
5. Serialized block size must not exceed 262,144 bytes.

### 12.7 Syncing Behavior

- The engine enters `Syncing` state during fork resolution.
- No blocks are produced while syncing.
- After 15 slots (~60 seconds) with no sync progress, a warning is logged but the node remains in Syncing.
- Recovery from stuck sync occurs via BFT expiration detection in the FFI layer, which triggers committee rebuild to solo mode and sets state back to Active.

---

## Appendix A: Source File Index

| File | Description |
|------|-------------|
| `crates/gratia-consensus/src/lib.rs` | ConsensusEngine: state machine, slot advancement, block processing, finalization |
| `crates/gratia-consensus/src/committee.rs` | Committee selection, graduated scaling tiers, cooldown tracking, rotation |
| `crates/gratia-consensus/src/block_production.rs` | BlockProducer, PendingBlock, block signing and verification |
| `crates/gratia-consensus/src/vrf.rs` | ECVRF key generation, proof generation/verification, selection weighting |
| `crates/gratia-consensus/src/validation.rs` | Transaction and block validation rules, size constants |
| `crates/gratia-consensus/src/streamlet.rs` | Streamlet BFT state machine: proposals, votes, notarization, finality |
| `crates/gratia-network/src/gossip.rs` | Gossipsub topics, message types, gossip-layer validation, deduplication |
| `crates/gratia-network/src/discovery.rs` | Kademlia DHT peer discovery, PeerRecord management |
| `crates/gratia-core/src/emission.rs` | Emission schedule: annual budget, per-block reward, year calculation |
| `crates/gratia-core/src/config.rs` | RewardsConfig defaults (50 GRAT/block, 25% reduction, 1.5x geo bonus) |
| `crates/gratia-staking/src/rewards.rs` | EmissionSchedule (configurable), BlockRewardDistribution, geographic equity |
