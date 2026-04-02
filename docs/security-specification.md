# Gratia Protocol — Security Specification

**Version 1.0 | April 2, 2026**
**Status: Pre-audit draft**
**Classification: Confidential — for security auditors only**

---

## Table of Contents

1. [Threat Model](#1-threat-model)
2. [Consensus Security (Streamlet BFT)](#2-consensus-security-streamlet-bft)
3. [Sybil Resistance (Three-Pillar Model)](#3-sybil-resistance-three-pillar-model)
4. [Cryptographic Primitives](#4-cryptographic-primitives)
5. [Network Security](#5-network-security)
6. [Wallet Security](#6-wallet-security)
7. [Known Limitations](#7-known-limitations)
8. [Invariants](#8-invariants)
9. [Audit Scope](#9-audit-scope)

---

## 1. Threat Model

### 1.1 System Description

Gratia is a mobile-native layer-1 blockchain. All consensus participants are smartphones. There are no server-based validators. Block production, validation, and finalization occur on consumer ARM devices over cellular and Wi-Fi networks.

### 1.2 Attacker Capabilities

**Network-level attacker:**
- Can observe, delay, reorder, or drop messages between any two peers.
- Can inject arbitrary messages from Sybil peers.
- Can perform eclipse attacks by surrounding a victim node with attacker-controlled peers.
- Can perform DoS by flooding a node with messages.

**Device-level attacker:**
- Rooted phone: full control over sensor data reported to the Proof of Life engine. Can fabricate GPS, accelerometer, Bluetooth, and all other sensor readings.
- Emulator: can simulate an ARM device on commodity hardware. No physical sensors.
- Phone farm: multiple physical devices operated by one person, attempting to claim separate identities.

**Economic attacker:**
- Whale: large GRAT holder seeking to gain disproportionate consensus power.
- Briber: external party offering incentives for validators to behave dishonestly.
- Short seller: party with financial interest in disrupting the network.

### 1.3 Assets Under Protection

| Asset | Threat | Primary Defense |
|-------|--------|----------------|
| User funds (GRAT balances) | Theft, double-spend | Ed25519 signatures, BFT finality |
| Consensus integrity | Forged blocks, forks | Streamlet BFT with 2/3+ threshold |
| Identity uniqueness (one-phone-one-vote) | Sybil attacks, phone farms | Proof of Life + Staking + Energy |
| Privacy (sensor data) | Surveillance, deanonymization | On-device ZK proofs, unlinkable attestations |
| Key material | Extraction, side-channel | Secure enclave (design), AES-256-GCM file encryption (current) |
| Network availability | DoS, eclipse attacks | Peer reputation, rate limiting |

### 1.4 Trust Assumptions

1. **Honest majority.** The protocol assumes fewer than 1/3 of committee members are Byzantine. This is the standard BFT assumption. If 1/3 or more committee members collude, safety is lost.

2. **Hardware trust (partial).** The design intends to use TEE/secure enclave attestation to bind keys to physical hardware. The current implementation does NOT enforce TEE attestation — this is a planned feature. See [Section 7: Known Limitations](#7-known-limitations).

3. **Network eventual synchrony.** Streamlet BFT provides safety under asynchrony but requires eventual message delivery for liveness. If a node is permanently partitioned, it will not finalize new blocks.

4. **ARM hardware.** The energy expenditure pillar assumes real ARM computation is required. An attacker with access to sufficiently fast x86 hardware and a perfect ARM emulator could bypass this check. The protocol relies on a combination of all three pillars, not energy alone.

5. **Sensor data integrity (on honest devices).** The PoL engine trusts the OS-provided sensor APIs. A rooted device can fake all sensor readings. See [Section 7.4](#74-pol-spoofability-on-rooted-devices).

---

## 2. Consensus Security (Streamlet BFT)

### 2.1 Protocol Overview

Gratia uses a modified Streamlet BFT protocol (Chan & Shi, 2020). The implementation is in `crates/gratia-consensus/src/streamlet.rs`.

**Protocol phases:**
1. **Propose:** Each epoch, the designated leader (selected via VRF round-robin) proposes a block extending the longest notarized chain.
2. **Vote:** Committee members vote for the proposal if it extends a notarized chain they know about. Each member votes for at most one block per epoch.
3. **Notarize:** A block is notarized when it receives votes from 2/3+ of the committee.
4. **Finalize:** When three consecutive notarized blocks exist at heights h, h+1, h+2 — the chain up to height h+1 is finalized.

### 2.2 Safety Guarantee

**Claim:** No two conflicting blocks can both be finalized if fewer than 1/3 of committee members are Byzantine.

**Proof sketch:** Finalization requires three consecutive notarized blocks. Notarization requires 2/3+ votes. Two conflicting blocks at the same height would each need 2/3+ votes, but the intersection of two 2/3 sets in a committee of size n is at least n/3 — meaning at least n/3 validators double-voted (equivocated). If fewer than n/3 are Byzantine, this is impossible.

**Code enforcement:**

- Notarization threshold calculation: `streamlet.rs:ProposedBlock::add_vote()` (line 77):
  ```
  let threshold = (committee_size * 2 + 2) / 3; // Ceiling division for 2/3
  ```
  This computes ceil(2n/3). For committee_size=21, threshold=14. For committee_size=3, threshold=2.

- Duplicate vote rejection: `streamlet.rs:ProposedBlock::add_vote()` (line 71):
  ```
  if self.votes.iter().any(|v| v.signature.validator == vote.signature.validator) {
      return false;
  }
  ```

- Equivocation detection: `streamlet.rs:StreamletState::add_vote()` (lines 208-221). The `remote_votes` map tracks `(epoch, validator) -> block_hash`. If a vote arrives for the same epoch and validator but a different block hash, it is rejected and logged as Byzantine behavior.

- Self double-vote prevention: `streamlet.rs:StreamletState::should_vote()` (line 162) checks `voted_epochs` before allowing this node to vote.

- Finality check: `streamlet.rs:StreamletState::check_finality()` (lines 253-280). Scans from `finalized_height + 1` upward looking for three consecutive heights with notarized blocks.

### 2.3 Liveness Guarantee

**Claim:** The chain makes progress if more than 2/3 of committee members are honest and the network is eventually synchronous.

**Conditions for progress:**
- A valid leader must be selected for each slot (`committee.rs:block_producer_for_slot()`, line 279 — round-robin within VRF-selected committee).
- The leader must produce a block and broadcast it.
- Honest committee members must receive the block and vote.
- Votes must be received by enough nodes to reach notarization.

**Liveness risk: sync stalling.** If a node enters `Syncing` state and the sync protocol fails, the node could stall indefinitely. The engine logs a warning after 15 slots (~60 seconds) but does NOT auto-resume to avoid forking (`lib.rs:advance_slot()`, lines 266-278). Recovery depends on the FFI layer detecting peer loss and rebuilding the committee.

### 2.4 Committee Selection

Implementation: `crates/gratia-consensus/src/committee.rs`

**Graduated scaling.** Committee size scales with network size across 7 tiers:

| Network Size | Committee | Finality Threshold | Cooldown |
|-------------|-----------|-------------------|----------|
| 0-99 | 3 | 2 (67%) | 5 rounds |
| 100-499 | 5 | 4 (80%) | 3 rounds |
| 500-2,499 | 7 | 5 (71%) | 2 rounds |
| 2,500-9,999 | 11 | 8 (73%) | 1 round |
| 10,000-49,999 | 15 | 10 (67%) | 1 round |
| 50,000-99,999 | 19 | 13 (68%) | 1 round |
| 100,000+ | 21 | 14 (67%) | 1 round |

Defined in `committee.rs:SCALING_TIERS` (lines 73-125).

**Selection algorithm** (`committee.rs:select_committee_with_network_size()`, line 397):
1. Filter to committee-eligible nodes (30+ days PoL history for progressive trust). Falls back to basic eligibility if insufficient 30-day nodes exist.
2. Apply cooldown filtering via `CooldownTracker` to prevent back-to-back committee membership. Falls back to unfiltered if cooldown removes too many candidates.
3. For each eligible node, either verify a submitted VRF proof or compute a deterministic SHA-256 pseudo-VRF from `COMMITTEE_SELECTION_DOMAIN || epoch_seed || epoch_number || node_id`.
4. Weight each node's selection value by Presence Score via `vrf.rs:vrf_output_to_selection()` — integer division `raw_hash / score` ensures deterministic cross-platform ordering.
5. Sort by selection value ascending; take the top `committee_size` nodes.

**Epoch structure:**
- `SLOTS_PER_EPOCH = 900` (~1 hour at 4-second block time).
- Committee rotates at epoch boundaries.
- Block producer within an epoch: round-robin `(slot - start_slot) % committee_size` over the VRF-sorted committee (`committee.rs:block_producer_for_slot()`, line 286).

### 2.5 Fork Resolution

The engine detects forks in `lib.rs:process_incoming_block()` (line 532):
- A block at expected height with a different parent hash returns `ForkDetected`.
- A block more than 2 ahead returns `ForkDetected` for the caller to initiate reorg.
- A block exactly 2 ahead is accepted as a fast-forward (gap tolerance for pending block expiration).

**Equivocation detection in process_incoming_block:** The `seen_proposals` map tracks `(height, producer) -> block_hash` (line 592). If the same producer proposes different blocks at the same height, the block is rejected.

### 2.6 Block Validation on Receipt

When receiving a block from the network (`lib.rs:process_incoming_block()`, lines 647-700):
1. Signature count check: in multi-node mode, requires `finality_threshold` signatures. In bootstrap (threshold <= 1), requires at least 1.
2. Committee membership check: every signature must be from a current committee member.
3. Cryptographic signature verification: each validator's Ed25519 signature is verified against their registered signing pubkey via `block_production::verify_block_signature()`.
4. Producer verification: the block's producer must match the expected producer for that slot, with +-1 height tolerance for clock skew.

---

## 3. Sybil Resistance (Three-Pillar Model)

### 3.1 Proof of Life (Pillar 1)

Implementation: `crates/gratia-pol/src/lib.rs`

**What it proves:** A physical smartphone was used by a human throughout a 24-hour period, based on 8 behavioral parameters:

1. Minimum 10 unlock events spread across >= 6-hour window
2. Screen interaction sessions showing organic touch patterns
3. At least one orientation change
4. Accelerometer data showing human-consistent motion
5. At least one GPS fix with plausible location
6. Connection to Wi-Fi OR detection of Bluetooth peers
7. Varying Bluetooth peer environments at different times
8. At least one charge cycle event (plug/unplug)

**What it cannot prove:**
- That the device is not rooted (sensor data can be fabricated on rooted devices).
- That one human is not operating multiple phones (phone farms).
- That the sensor data is not generated by sophisticated automation scripts.

**ZK attestation:** Daily PoL data is proven via Bulletproofs range proofs (`crates/gratia-zk/src/bulletproofs.rs`). The proof demonstrates each numeric parameter meets its threshold without revealing actual values. Boolean parameters (orientation, motion, GPS, charge) are encoded as 0/1. The proof is generated in `gratia-pol/src/lib.rs:ProofOfLifeManager::finalize_day()` (line 124).

**Grace period:** 1 day grace for missed PoL. Two consecutive missed days pauses mining. Resumes immediately on the next valid day. Implemented in `lib.rs:finalize_day()` (lines 187-193).

**Progressive trust model:**
- Day 0: Unverified (mining allowed, maximum scrutiny)
- Day 7: Provisional
- Day 30: Established (committee-eligible, enforced in `committee.rs:EligibleNode::is_committee_eligible()`, line 194: `self.pol_days >= 30`)
- Day 90+: Trusted (governance-eligible)

### 3.2 Staking (Pillar 2)

Implementation: `crates/gratia-staking/src/`

**Model:** Capped staking with overflow pool.
- Minimum stake required to mine (governance-adjustable).
- Per-node stake cap (e.g., 1,000 GRAT).
- Stake above cap flows to Network Security Pool.
- Pool yield distributed to ALL active mining nodes proportionally.

**Slashing:** Implementation in `crates/gratia-staking/src/slashing.rs`.

Progressive penalty schedule:

| Offense | Penalty | Additional |
|---------|---------|------------|
| 1st (Warning) | 0% slash | 48-hour mining pause |
| 2nd within 90 days (Minor) | 10% effective stake | No pause |
| 3rd within 90 days (Major) | 50% effective stake | 30-day lockout |
| Proven fraud (Critical) | 100% burned permanently | Permanent ban |

Escalation logic: `slashing.rs:SlashingHistory::effective_severity_at()` (line 212). Uses a 90-day rolling window (`windowed_counts()`, line 177) — old offenses outside the window do not contribute to escalation.

Slash distribution for proven fraud: 70% burned (deflationary), 30% to reporting validators. Configured in `SlashingConfig::default()` (lines 139-142).

Slash calculation: `slashing.rs:calculate_slash_amount()` (line 254). Slashes effective stake first (reducing consensus power), then overflow. Uses u128 intermediate to prevent overflow in `total_stake * slash_bps / 10_000`.

### 3.3 Energy Expenditure (Pillar 3)

**Design intent:** Mining requires real ARM computation while the phone is plugged in and battery is at or above 80%. This is enforced by the mining mode controller on-device.

**Current implementation status:** The mining mode controller is implemented in the Android Kotlin layer (`MiningService.kt`). The energy expenditure proof (demonstrating real ARM work was performed) is a design-phase concept; no on-chain verifiable energy proof exists yet. This pillar currently relies on the difficulty of emulating ARM computation at scale combined with PoL's sensor requirements.

---

## 4. Cryptographic Primitives

### 4.1 Ed25519 Signatures

**Usage:**
- Transaction signing (all transfers, staking, governance votes)
- Block signing by producers and committee validators
- Node announcement signing (gossip layer authentication)
- VRF key derivation (Ed25519 key -> VRF secret key)

**Key lifecycle:**
1. Generation: `ed25519_dalek::SigningKey::generate(&mut OsRng)` in `crates/gratia-core/src/crypto.rs:Keypair::generate()` (line 28) and `crates/gratia-wallet/src/keystore.rs:SoftwareKeystore::generate_keypair()` (line 124).
2. Storage: AES-256-GCM encrypted file on disk (`FileKeystore`) or in-memory (`SoftwareKeystore`). The design calls for Android Keystore/StrongBox and iOS Secure Enclave on production devices.
3. Signing: Standard Ed25519 (`ed25519_dalek::Signer::sign()`). No additional domain separation in the Ed25519 layer itself — domain separation is at the message construction level.
4. Verification: `gratia_core::crypto::verify_signature()` (line 77) — uses `ed25519_dalek::Verifier::verify()` (not `verify_strict()`). Note: the gossip layer uses `verify_strict()` for node announcements (`gossip.rs:verify_ed25519()`, line 234).

**Signing domains (message construction):**
- Block signatures: signing over the block header hash (SHA-256 of serialized header).
- Transaction signatures: `payload_bytes || nonce (LE) || chain_id (LE) || fee (LE) || timestamp_millis (LE)` — includes chain_id for cross-chain replay protection (`validation.rs`, lines 91-95).
- Node announcements: `node_id || vrf_pubkey_bytes || presence_score || pol_days || timestamp` (`gossip.rs:node_announcement_signing_payload()`, line 214).

### 4.2 Bulletproofs (Zero-Knowledge Range Proofs)

Implementation: `crates/gratia-zk/src/bulletproofs.rs`

**Construction:**
- Library: `bulletproofs` crate (dalek-cryptography) with `merlin` Fiat-Shamir transcripts.
- Proof technique: To prove `value >= minimum`, prove `(value - minimum)` lies in range `[0, 2^n)`. If `value < minimum`, the subtraction underflows in u64 and cannot produce a valid range proof.
- Aggregation: Multiple parameters are proven in a single aggregated Bulletproof for efficiency.

**Two API layers:**
1. High-level (8 parameters, 16-bit range, transcript domain `gratia-proof-of-life-v1`).
2. Flexible (4 core numeric parameters, 32-bit range, transcript domain `gratia-pol-range-proof-v1`). Used in production.

**Replay prevention:** The `epoch_day` field is bound into the Merlin Fiat-Shamir transcript (line 179 of `bulletproofs.rs`), preventing a valid proof from one day being accepted on a different day.

**Proof size:** ~700-900 bytes for a 4-value aggregated Bulletproof.

### 4.3 ECVRF (Verifiable Random Function)

Implementation: `crates/gratia-consensus/src/vrf.rs`

**Construction:** Schnorr-like proof on Ristretto points (curve25519-dalek). This is a PoC implementation; the code comments note that a full RFC 9381 implementation would be used for mainnet (`vrf.rs`, line 10).

**Properties:**
- **Deterministic:** Same key + same input = same output (verified in test `test_deterministic_output`).
- **Pseudorandom:** Output indistinguishable from random without the secret key.
- **Verifiable:** Anyone with the public key can verify the proof.

**Nonce generation:** Deterministic from `SHA-512(gratia-vrf-nonce-v1: || secret_key || input)` (line 197). This prevents nonce reuse attacks that would leak the secret key.

**VRF input for block production:** `previous_block_hash || slot_number (big-endian)` (`vrf.rs:build_vrf_input()`, line 328).

**Selection weighting:** `vrf.rs:vrf_output_to_selection()` (line 310). Uses integer division `u64_from_vrf_output / presence_score` to ensure deterministic ordering across ARM and x86 platforms. The previous f64 version was replaced due to floating-point rounding differences between architectures.

**Scalar validation:** Verification uses `Scalar::from_canonical_bytes()` (lines 255, 262) to reject non-canonical scalar encodings, preventing malleability.

### 4.4 AES-256-GCM (Keystore Encryption)

Implementation: `crates/gratia-wallet/src/keystore.rs`

**Encryption:** `FileKeystore::encrypt_key_material()` (line 276).
- Algorithm: AES-256-GCM via the `ring` crate.
- Salt: 16 random bytes (OsRng).
- Nonce: 12 random bytes (OsRng).
- AAD: `b"gratia-wallet-v2"` (authenticated associated data).
- Ciphertext: 32 bytes plaintext + 16 bytes GCM auth tag = 48 bytes total.

**Key derivation:** `FileKeystore::derive_encryption_key()` (line 257).
- **Current method:** `SHA-256(gratia-keystore-v1 || salt)` — the domain string is hardcoded.
- **AUDIT NOTE:** This is explicitly flagged in the code (line 254) as needing replacement with a proper KDF (Argon2 or HKDF) and device-bound key. See [Section 7.2](#72-keystore-key-derivation).

**Decryption:** `FileKeystore::decrypt_key_material()` (line 319). Validates ciphertext length (48 bytes) and nonce length (12 bytes) before attempting decryption. GCM authentication tag verification detects any tampering.

**Legacy format migration:** Old XOR-encrypted keys (ciphertext length 32) are detected and auto-upgraded to AES-GCM on load (`load_key()`, line 207).

**Debug-only plaintext fallback:** Raw 32-byte key files are loaded only under `#[cfg(debug_assertions)]` (line 222). Release builds reject unencrypted key files.

### 4.5 SHA-256 and Domain Separation

All SHA-256 usage includes domain separation prefixes to prevent cross-protocol confusion. The following table lists every domain prefix string found in the codebase:

| Domain Prefix | File | Purpose |
|--------------|------|---------|
| `gratia-address-v1:` | `gratia-core/src/types.rs:83` | Wallet address derivation from Ed25519 pubkey |
| `gratia-blinded-id-v1:` | `gratia-core/src/crypto.rs:126` | Unlinkable PoL blinded identifier (daily) |
| `gratia-nullifier-v1:` | `gratia-core/src/crypto.rs:134` | PoL double-submission detection (per epoch) |
| `gratia-keystore-v1` | `gratia-wallet/src/keystore.rs:260` | File keystore encryption key derivation |
| `gratia-wallet-v2` | `gratia-wallet/src/keystore.rs:297,343` | AES-GCM authenticated associated data |
| `gratia-contract-v1:` | `gratia-vm/src/lib.rs:162,436` | Smart contract address derivation |
| `gratia-empty-state-v1` | `gratia-state/src/lib.rs:563` | Empty state root computation |
| `gratia-mesh-msg-v1:` | `gratia-network/src/mesh.rs:259` | Bluetooth mesh message hashing |
| `gratia-committee-select-v1` | `gratia-consensus/src/committee.rs:38` | Committee selection VRF input |
| `gratia-epoch-seed-v1:` | `gratia-consensus/src/committee.rs:562,582` | Epoch seed derivation |
| `gratia-randao-seed-v1:` | `gratia-consensus/src/committee.rs:613` | RANDAO-style seed mixing |
| `gratia-shard-committee-v1` | `gratia-consensus/src/sharded_consensus.rs:50` | Shard committee selection |
| `gratia-cross-shard-v1:` | `gratia-consensus/src/sharded_consensus.rs:271` | Cross-shard message hashing |
| `gratia-poll-v1:` | `gratia-governance/src/polling.rs:248` | On-chain poll ID derivation |
| `gratia-proposal-v1:` | `gratia-governance/src/proposals.rs:469` | Governance proposal ID derivation |
| `gratia-pedersen-blinding-v1:` | `gratia-zk/src/pedersen.rs:163` | Pedersen commitment blinding factor |

**Merlin transcript domain separators (Fiat-Shamir):**

| Domain Prefix | File | Purpose |
|--------------|------|---------|
| `gratia-proof-of-life-v1` | `gratia-zk/src/bulletproofs.rs:71` | PoL attestation Bulletproof transcript |
| `gratia-pol-range-proof-v1` | `gratia-zk/src/bulletproofs.rs:77` | Flexible PoL range proof transcript |
| `gratia-shielded-transfer-v1` | `gratia-zk/src/shielded_tx.rs:39` | Shielded transaction proof transcript |
| `gratia-groth16-proof-v1` | `gratia-zk/src/groth16.rs:56` | Groth16 ZK proof transcript |
| `gratia-groth16-generator-v1:` | `gratia-zk/src/groth16.rs:366` | Groth16 parameter generation |

**VRF domain separators (SHA-512):**

| Domain Prefix | File | Purpose |
|--------------|------|---------|
| `gratia-vrf-h2c-v1` | `gratia-consensus/src/vrf.rs:30` | VRF hash-to-point |
| `gratia-vrf-challenge-v1` | `gratia-consensus/src/vrf.rs:33` | VRF Schnorr challenge |
| `gratia-vrf-output-v1` | `gratia-consensus/src/vrf.rs:36` | VRF output derivation |
| `gratia-vrf-keygen-v1:` | `gratia-consensus/src/vrf.rs:94` | Ed25519 -> VRF key derivation |
| `gratia-vrf-nonce-v1:` | `gratia-consensus/src/vrf.rs:197` | VRF deterministic nonce |

---

## 5. Network Security

### 5.1 Gossipsub Message Authentication

Implementation: `crates/gratia-network/src/gossip.rs`

Messages are validated in layers:

**Layer 1 — Size check** (`gossip.rs:validate_incoming_message()`, line 264):
- Maximum message size: 300 KB (`MAX_MESSAGE_SIZE = 300 * 1024`).
- Messages exceeding this are rejected before deserialization.

**Layer 2 — Structural validation** (lines 276-396):
- **Blocks:** Must contain a signature from the claimed producer with valid length (64 bytes). If `producer_pubkey` is present, an Ed25519 pre-check is attempted (non-blocking — full verification in consensus layer).
- **Transactions:** Must have non-empty signature (exactly 64 bytes) and sender_pubkey (exactly 32 bytes).
- **Attestations:** Must have non-empty ZK proof. Presence score must be in range [40, 100].
- **Node announcements:** Must be signed. The `ed25519_pubkey` must hash to the claimed `node_id` (SHA-256 check). The Ed25519 signature over the canonical payload (`node_announcement_signing_payload()`) is verified cryptographically. Unsigned announcements are rejected.

**Layer 3 — Consensus validation** (in `crates/gratia-consensus/src/lib.rs` and `validation.rs`):
- Full BFT signature threshold verification.
- Committee membership checks.
- Producer slot assignment verification.
- Transaction fee and nonce validation.

### 5.2 Peer Reputation and Rate Limiting

Implementation: `crates/gratia-network/src/reputation.rs`

**Reputation scoring:**
- Starting score: 100 (range: -100 to 1000).
- Valid block relay: +5
- Valid transaction relay: +1
- Invalid block relay: -20 (4x reward — invalid blocks waste validation resources)
- Invalid transaction relay: -10 (10x reward — discourages tx spam)
- Spam/unrecognized message: -15

**Ban thresholds:**
- Score < -50: disconnect + 1-hour short ban.
- Score <= -100: 24-hour hard ban.

**NodeId-to-PeerId linking** (`reputation.rs:link_node_id()`, line 332): When a gossip message carries a NodeId, it is linked to the PeerId. If an attacker rotates PeerId but reuses the same NodeId, bad reputation carries over. Note: the current implementation links NodeId->PeerId but still keys primary reputation on PeerId. The code acknowledges this is a partial fix (line 214).

**Rate limiting** (sliding window, 60-second window):
- Max 10 blocks per peer per minute.
- Max 100 transactions per peer per minute.
- Max 50 generic messages per peer per minute.

**Stale peer eviction:** Auto-evicts peers inactive for >24 hours when the peer map exceeds 1000 entries (`reputation.rs:entry()`, line 233).

### 5.3 Kademlia DHT Security Considerations

The DHT is used for peer discovery. Known risks:

- **Sybil flooding:** An attacker can create many DHT node IDs positioned near a target to intercept lookups. Mitigation: peer reputation scoring discourages connections to misbehaving Sybils, but DHT-level Sybil resistance is not implemented.
- **Routing table poisoning:** Malicious nodes can return incorrect routing information. Mitigation: libp2p's Kademlia implementation includes basic protections, but the protocol does not add additional verification.

### 5.4 Eclipse Attack Mitigations

Current mitigations are limited:
- Peer reputation scoring penalizes peers that send invalid data.
- Rate limiting prevents any single peer from monopolizing bandwidth.
- Node announcements require Ed25519 signature verification, preventing identity spoofing.

**Not yet implemented:** Diverse peer selection across IP ranges, geographic diversity requirements for peer connections, or minimum peer count enforcement from distinct subnets.

### 5.5 DoS Resistance

- Per-peer rate limiting (Section 5.2).
- Message size caps (300 KB).
- Transaction fee requirement (minimum 1,000 Lux) prevents free transaction spam.
- Gossip-layer structural validation rejects malformed messages before expensive consensus processing.
- Proof of Life attestation deduplication via nullifiers (`gratia-core/src/crypto.rs:generate_nullifier()`, line 133) — same node, same epoch produces the same nullifier.

### 5.6 Gossipsub Topics

The protocol uses dedicated topics to isolate traffic:

| Topic | Purpose |
|-------|---------|
| `gratia/blocks/1` | Block propagation |
| `gratia/transactions/1` | Transaction propagation |
| `gratia/attestations/1` | PoL attestation propagation |
| `gratia/nodes/1` | Node announcements |
| `gratia/sync/1` | State sync (point-to-point via gossipsub) |
| `gratia/lux/posts/1` | Social protocol posts |
| `gratia/validator-sigs/1` | BFT validator signatures |

**Audit note:** The sync protocol currently uses gossipsub for point-to-point request/response messages (`TOPIC_SYNC`). Nodes embed target peer IDs and ignore messages not addressed to them. This is acknowledged in the code as acceptable for testnet but inefficient for production — a dedicated libp2p request-response protocol should replace it.

---

## 6. Wallet Security

### 6.1 Key Generation and Storage

**Software keystore** (`keystore.rs:SoftwareKeystore`): In-memory Ed25519 key. Keys are lost on app restart. Used for testing.

**File keystore** (`keystore.rs:FileKeystore`): Persists key to `{data_dir}/wallet_key.bin` as AES-256-GCM encrypted JSON. See Section 4.4 for encryption details.

**Secure enclave** (design, not yet enforced): The `Keystore` trait (`keystore.rs`, line 28) abstracts key storage. Production implementations should delegate to Android Keystore/StrongBox or iOS Secure Enclave. The trait's `export_secret_key()` method explicitly documents that hardware implementations MUST return an error — the private key never leaves the chip.

### 6.2 Transaction Signing Flow

1. Application constructs a `Transaction` with payload, nonce, chain_id, fee, timestamp.
2. Signing message is assembled: `payload_bytes || nonce (LE u64) || chain_id (LE u32) || fee (LE u64) || timestamp_millis (LE i64)`.
3. Ed25519 signature is computed via the keystore.
4. Transaction is broadcast via gossipsub.
5. Receiving nodes validate the signature in the gossip layer (structural check) and consensus layer (full verification against sender's public key).

**Cross-chain replay protection:** The `chain_id` is included in the signed message, preventing a transaction signed for one chain from being valid on another.

### 6.3 Block Signing Flow

1. Block producer assembles block header (height, parent hash, transaction root, state root, VRF proof).
2. Block header is hashed via SHA-256.
3. Producer signs the header hash with their Ed25519 key.
4. Block is broadcast with the producer's signature.
5. Committee members validate the block and broadcast their own signatures via `TOPIC_VALIDATOR_SIGS`.
6. Signatures are verified cryptographically in `lib.rs:add_block_signature()` (line 396):
   - Signer must be a committee member.
   - Ed25519 signature must verify against the signer's registered pubkey.
   - In multi-node mode, signatures from validators with empty pubkeys are rejected.

### 6.4 Shielded Transactions

Implementation: `crates/gratia-zk/src/shielded_tx.rs`

Optional per-transaction. Uses Bulletproofs + Pedersen commitments to hide the transaction amount. Transcript domain: `gratia-shielded-transfer-v1`. Proof generation targets 2-5 seconds on ARM. Designed to run during mining mode (plugged in, above 80% battery).

### 6.5 Recovery Mechanism

**Design:** Proof of Life behavioral matching over 7-14 day window on new device. Old wallet frozen during recovery. Original device owner can reject the claim instantly.

**Risks:**
- Behavioral patterns may be spoofable by someone who has observed the owner's phone usage patterns.
- No social recovery (by design — collusion vulnerability).
- Optional seed phrase available as a fallback (opt-in, not default).

---

## 7. Known Limitations

This section documents security gaps the audit team should be aware of. These are honest assessments of the current implementation state.

### 7.1 TEE Attestation (Planned, Not Enforced)

The design calls for Trusted Execution Environment attestation to bind keys to physical hardware (Android Keystore/StrongBox, iOS Secure Enclave). The `Keystore` trait abstraction exists, but the current implementation uses file-based storage with software encryption. No on-chain or network-level TEE attestation verification is performed.

**Impact:** Without TEE binding, an attacker who gains file system access can extract the encrypted key file. The encryption key is derived from a hardcoded domain string + random salt (see 7.2), not from hardware-bound material.

### 7.2 Keystore Key Derivation

`FileKeystore::derive_encryption_key()` uses `SHA-256(gratia-keystore-v1 || salt)`. The code explicitly flags this (`keystore.rs`, line 254):

> TODO(audit): Replace with a proper KDF (Argon2 or HKDF) and use a real device-bound key from Android Keystore / iOS Secure Enclave instead of a hardcoded domain string. The current approach provides authenticated encryption (AES-256-GCM) but not strong protection against a local attacker who can read the salt from the same file.

**Impact:** A local attacker with read access to the key file can derive the encryption key (salt is stored in the same JSON file) and decrypt the private key. The AES-256-GCM provides integrity (tamper detection) but not confidentiality against this attacker model.

### 7.3 Single Bootstrap Node

The network currently uses a single Vultr VPS as a bootstrap/relay node. This is a single point of failure for initial peer discovery.

**Impact:** If the bootstrap node is unavailable, new nodes cannot join the network. Existing connected nodes continue to communicate peer-to-peer.

### 7.4 PoL Spoofability on Rooted Devices

On a rooted Android device, all sensor APIs can be intercepted and faked. The Proof of Life engine processes sensor data from OS APIs — it has no way to verify the sensor data is genuine if the OS itself is compromised.

**Impact:** A rooted device can generate valid PoL attestations with entirely fabricated sensor data. The ZK proof is correct (the fabricated values do meet the thresholds), but the underlying claim is false.

**Mitigations (partial):**
- Behavioral anomaly detection (`gratia-pol/src/behavioral_anomaly.rs`) analyzes patterns over time to detect non-human regularity.
- TEE attestation (when implemented) would make sensor data fabrication significantly harder.
- The three-pillar model means spoofing PoL alone is insufficient — staking and energy requirements still apply.

### 7.5 VRF Implementation Is PoC

The VRF implementation (`vrf.rs`, line 10) explicitly states:

> This is a PoC implementation suitable for testnet. A full RFC 9381 implementation would be used for mainnet after security audit.

The current implementation uses a Schnorr-like proof on Ristretto points. While the construction is standard, it has not been formally verified against RFC 9381 and may have subtle differences in hash-to-point or proof encoding.

### 7.6 Sync Protocol Uses Gossipsub

The state synchronization protocol (`TOPIC_SYNC`) routes point-to-point messages through the broadcast gossipsub layer. Nodes filter messages by embedded target peer ID. This leaks sync state to all subscribers and wastes bandwidth.

**Impact:** Privacy reduction (observers see who is syncing), bandwidth waste on mobile networks, potential DoS amplification.

### 7.7 Force-Finalize in Bootstrap Mode

The `force_finalize_pending_block()` method (`lib.rs`, line 458) allows block finalization with fewer than `finality_threshold` signatures when the committee has at most 1 real member.

**Security gate:** The method counts real committee members (those with non-empty signing keys) and rejects force-finalize if more than 1 exist (`lib.rs`, lines 464-477). This is critical — without this gate, a single node could unilaterally finalize blocks in a multi-node network.

### 7.8 Signature Verification Bypass in Solo Mode

In `add_block_signature()` (`lib.rs`, lines 408-426), signatures from validators with empty signing pubkeys are accepted without cryptographic verification in solo/bootstrap mode (when only 1 real committee member exists). In multi-node mode, such signatures are rejected.

**Risk:** If the transition from solo to multi-node mode has a race condition, there could be a window where unverified signatures are accepted with multiple real nodes present.

### 7.9 No Timestamp Validation on Incoming Blocks

The `process_incoming_block()` function checks parent hash and producer but does not validate that the block's timestamp is monotonically increasing relative to the previous finalized block. The `last_finalized_timestamp` field exists in `ConsensusEngine` but is not checked during incoming block validation.

**Impact:** A malicious producer could set timestamps in the past or far in the future without rejection.

---

## 8. Invariants

The following security invariants are maintained by the code. Each is mapped to the enforcing code location.

### 8.1 Consensus Invariants

| # | Invariant | Enforced By |
|---|-----------|-------------|
| C1 | A block is never notarized with fewer than ceil(2n/3) unique votes. | `streamlet.rs:ProposedBlock::add_vote()`, line 77 |
| C2 | A block is never finalized without 3 consecutive notarized blocks. | `streamlet.rs:StreamletState::check_finality()`, line 253 |
| C3 | A validator never casts two votes for different blocks in the same epoch (self). | `streamlet.rs:StreamletState::should_vote()`, line 162; `voted_epochs` map |
| C4 | A remote validator's second vote for a different block in the same epoch is rejected. | `streamlet.rs:StreamletState::add_vote()`, lines 208-221; `remote_votes` map |
| C5 | A duplicate vote from the same validator for the same block is silently ignored. | `streamlet.rs:ProposedBlock::add_vote()`, line 71 |
| C6 | Incoming blocks with insufficient signatures are rejected. | `lib.rs:process_incoming_block()`, lines 660-671 |
| C7 | Every validator signature on an incoming block is verified cryptographically (multi-node mode). | `lib.rs:process_incoming_block()`, lines 686-693 |
| C8 | Block signatures from non-committee members are rejected. | `lib.rs:process_incoming_block()`, lines 678-685; `lib.rs:add_block_signature()`, lines 386-394 |
| C9 | A producer who proposes two different blocks at the same height is detected and the second block is rejected. | `lib.rs:process_incoming_block()`, lines 592-604 |
| C10 | Force-finalize is blocked when more than 1 real committee member exists. | `lib.rs:force_finalize_pending_block()`, lines 464-477 |
| C11 | `PendingBlock::finalize()` rejects if signature count < finality_threshold. | `block_production.rs:PendingBlock::finalize()`, lines 71-76 |
| C12 | `PendingBlock::force_finalize()` rejects if zero signatures. | `block_production.rs:PendingBlock::force_finalize()`, lines 93-97 |
| C13 | Duplicate signatures from the same validator on a pending block are rejected. | `block_production.rs:PendingBlock::add_signature()`, lines 61-63 |

### 8.2 Cryptographic Invariants

| # | Invariant | Enforced By |
|---|-----------|-------------|
| K1 | VRF scalars are validated as canonical before use. | `vrf.rs:verify_vrf_proof()`, lines 255, 262 |
| K2 | VRF output is independently recomputed from Gamma and compared to the claimed output. | `vrf.rs:verify_vrf_proof()`, lines 283-286 |
| K3 | VRF nonce is deterministic (prevents key leakage from nonce reuse). | `vrf.rs:generate_vrf_proof()`, lines 195-203 |
| K4 | Presence Score is clamped to [40, 100] in VRF selection weighting. | `vrf.rs:vrf_output_to_selection()`, line 313 |
| K5 | AES-GCM ciphertext length is validated (48 bytes) before decryption. | `keystore.rs:decrypt_key_material()`, line 323 |
| K6 | AES-GCM nonce length is validated (12 bytes) before decryption. | `keystore.rs:decrypt_key_material()`, line 331 |
| K7 | Plaintext key files are only loaded in debug builds. | `keystore.rs:load_key()`, line 222 (`#[cfg(debug_assertions)]`) |

### 8.3 Network Invariants

| # | Invariant | Enforced By |
|---|-----------|-------------|
| N1 | Messages larger than 300 KB are rejected before deserialization. | `gossip.rs:validate_incoming_message()`, line 264 |
| N2 | Unsigned node announcements are rejected. | `gossip.rs:validate_incoming_message()`, lines 377-380 |
| N3 | Node announcement pubkey must hash to claimed node_id. | `gossip.rs:validate_incoming_message()`, lines 383-388 |
| N4 | Node announcement signature is cryptographically verified (Ed25519 verify_strict). | `gossip.rs:validate_incoming_message()`, lines 390-395 |
| N5 | Transactions with empty or wrong-length signatures are rejected at gossip layer. | `gossip.rs:validate_incoming_message()`, lines 338-350 |
| N6 | Blocks must carry a signature from their claimed producer. | `gossip.rs:validate_incoming_message()`, lines 288-306 |
| N7 | Attestation presence score must be in range [40, 100]. | `gossip.rs:validate_incoming_message()`, lines 359-364 |
| N8 | Peers with reputation score < -50 are disconnected and banned. | `reputation.rs:PeerReputation::evaluate_ban()`, line 147 |
| N9 | Rate limits cap per-peer message throughput (10 blocks, 100 txs, 50 msgs per minute). | `reputation.rs` constants, lines 79-89 |

### 8.4 Proof of Life Invariants

| # | Invariant | Enforced By |
|---|-----------|-------------|
| P1 | Raw sensor data never leaves the device — only ZK proofs are broadcast. | `gratia-pol/src/lib.rs` (architectural: PoL manager generates proof locally, broadcasts proof only) |
| P2 | Committee eligibility requires 30+ days of consecutive PoL. | `committee.rs:EligibleNode::is_committee_eligible()`, line 196 |
| P3 | Two consecutive missed PoL days pauses mining eligibility. | `gratia-pol/src/lib.rs:finalize_day()`, lines 187-193 |
| P4 | PoL ZK proof epoch_day is bound into the Fiat-Shamir transcript (anti-replay). | `bulletproofs.rs:PolRangeProof::epoch_day`, line 182 |
| P5 | PoL nullifier is deterministic per (node_id, epoch), enabling duplicate detection. | `gratia-core/src/crypto.rs:generate_nullifier()`, line 133 |

### 8.5 Staking Invariants

| # | Invariant | Enforced By |
|---|-----------|-------------|
| S1 | Slashing applies to effective stake first, then overflow. | `slashing.rs:calculate_slash_amount()`, lines 272-275 |
| S2 | Slash amount calculation uses u128 intermediate to prevent overflow. | `slashing.rs:calculate_slash_amount()`, line 269 |
| S3 | Offense escalation uses a 90-day rolling window, not lifetime counts. | `slashing.rs:SlashingHistory::windowed_counts()`, line 177 |
| S4 | Critical severity (permanent ban) always results in 100% slash. | `slashing.rs:SlashingConfig::default()`, line 131: `critical_slash_bps: 10_000` |
| S5 | Mining pause duration for Critical uses `u64::MAX / 2` (not `u64::MAX`) to prevent timestamp overflow. | `slashing.rs:build_slashing_event()`, line 331 |

---

## 9. Audit Scope

### 9.1 Crates In Scope

All crates under `crates/` are in scope. Priority order for security review:

| Priority | Crate | Rationale |
|----------|-------|-----------|
| Critical | `gratia-consensus` | BFT consensus, block production, committee selection, VRF — correctness directly affects safety |
| Critical | `gratia-wallet` | Key management, transaction signing — directly protects user funds |
| Critical | `gratia-zk` | Zero-knowledge proofs — correctness determines privacy and attestation validity |
| High | `gratia-network` | Gossip validation, peer reputation — first line of defense against network attacks |
| High | `gratia-core` | Cryptographic primitives, types, hashing — foundational to all other crates |
| High | `gratia-staking` | Slashing logic — determines economic penalties |
| Medium | `gratia-pol` | Proof of Life — Sybil resistance layer |
| Medium | `gratia-governance` | Voting, proposals — correctness affects protocol evolution |
| Lower | `gratia-state` | Storage, Merkle trees — data integrity |
| Lower | `gratia-vm` | Smart contract VM — not yet production-critical |
| Lower | `gratia-ffi` | UniFFI bindings — thin bridge layer |

### 9.2 Security-Critical Files

The following files contain the highest concentration of security-critical logic:

| File | Lines | Critical Functions |
|------|-------|--------------------|
| `crates/gratia-consensus/src/streamlet.rs` | ~470 | `add_vote`, `check_finality`, equivocation detection |
| `crates/gratia-consensus/src/lib.rs` | ~700 | `process_incoming_block`, `add_block_signature`, `force_finalize_pending_block` |
| `crates/gratia-consensus/src/committee.rs` | ~620 | `select_committee_with_network_size`, cooldown logic, tier selection |
| `crates/gratia-consensus/src/vrf.rs` | ~520 | `generate_vrf_proof`, `verify_vrf_proof`, `vrf_output_to_selection` |
| `crates/gratia-consensus/src/block_production.rs` | ~100+ | `PendingBlock::finalize`, `force_finalize`, `add_signature` |
| `crates/gratia-consensus/src/validation.rs` | ~100+ | `validate_transaction`, signature format matching |
| `crates/gratia-wallet/src/keystore.rs` | ~400+ | `encrypt_key_material`, `decrypt_key_material`, `derive_encryption_key` |
| `crates/gratia-zk/src/bulletproofs.rs` | ~200+ | `generate_pol_proof`, `verify_pol_proof`, transcript construction |
| `crates/gratia-network/src/gossip.rs` | ~400+ | `validate_incoming_message`, `verify_ed25519`, announcement auth |
| `crates/gratia-network/src/reputation.rs` | ~350 | `PeerReputation::evaluate_ban`, rate limiting |
| `crates/gratia-staking/src/slashing.rs` | ~350 | `calculate_slash_amount`, `effective_severity_at`, `windowed_counts` |
| `crates/gratia-core/src/crypto.rs` | ~150 | `verify_signature`, `sha256_multi`, `generate_nullifier` |

### 9.3 Suggested Focus Areas

1. **Streamlet BFT correctness.** Verify that the finality rule (3 consecutive notarized blocks) is correctly implemented and that no edge case allows conflicting finalization. Pay special attention to the `prune_below()` logic and whether pruning can ever remove data needed for finality checks.

2. **Force-finalize gate.** Verify that `force_finalize_pending_block()` cannot be invoked in multi-node mode. Trace all call paths from the FFI layer to confirm the "real member count > 1" check cannot be bypassed.

3. **Signature verification completeness.** Verify that every code path accepting a block or vote cryptographically verifies the Ed25519 signature. Check for any path where an unverified signature could be accepted in multi-node mode.

4. **VRF determinism across platforms.** The switch from f64 to u64 for selection weighting was made to ensure cross-platform determinism. Verify that no f64 arithmetic remains in the committee selection path.

5. **Key derivation weakness.** Assess the risk of the current `SHA-256(domain || salt)` key derivation. Recommend specific replacement (Argon2id with device-bound key from Android Keystore).

6. **Gossip layer authentication gaps.** Verify that a malicious peer cannot bypass node announcement signature verification. Check whether a race condition between solo-mode and multi-node mode transitions could allow forged signatures.

7. **Bulletproofs range proof soundness.** Verify that the `(value - minimum)` subtraction correctly prevents proofs for values below the threshold. Check that the epoch_day binding into the transcript prevents cross-day replay.

8. **Equivocation detection completeness.** Verify that the `seen_proposals` map in `process_incoming_block()` and the `remote_votes` map in `StreamletState` together catch all forms of double-voting and double-proposing.

9. **Timestamp validation gap.** The missing timestamp monotonicity check on incoming blocks (Section 7.9) should be assessed for exploitability — can an attacker with a compromised producer slot benefit from backdating or future-dating block timestamps?

10. **Overflow arithmetic.** Review all Lux (u64) arithmetic in the staking and slashing modules for potential overflow or underflow, especially in `calculate_slash_amount()` and reward distribution.

---

*End of Security Specification*
